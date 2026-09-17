//! `SQLite` reads and writes.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use axum::{
	Json,
	extract::{Path, Query, State},
	http::{HeaderMap, HeaderValue, header},
	response::IntoResponse,
};
use rusqlite::{
	Connection, OpenFlags, OptionalExtension, params, params_from_iter, types::ValueRef,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
	fs_ops::resolve_cwd_scoped_path,
	protocol::{
		SessionEvent,
		error::{ApiError, ApiResult, ErrorBody},
		requests::{ReadDbQuery, WriteDbOp, WriteDbRequest},
		responses::{
			ReadDbResponse, SqliteColumn, SqliteRow, SqliteTableInfo, SqliteTablesResponse,
			WriteDbResponse,
		},
	},
	session::Session,
	state::AppState,
};

const DEFAULT_LIMIT: u64 = 50;
const DEFAULT_OFFSET: u64 = 0;

#[utoipa::path(
	get,
	path = "/sessions/{id}/read.db",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "SQLite database path relative to the session cwd"),
		("table" = Option<String>, Query, description = "table name to inspect"),
		("key" = Option<String>, Query, description = "primary key or rowid for single-row lookup"),
		("q" = Option<String>, Query, description = "read-only SELECT query to execute"),
		("where" = Option<String>, Query, description = "filter expression for table reads"),
		("order" = Option<String>, Query, description = "ORDER BY expression for table reads"),
		("limit" = Option<u64>, Query, description = "maximum rows to return"),
		("offset" = Option<u64>, Query, description = "rows to skip before returning results"),
	),
	responses(
		(status = 200, body = Value),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path or row not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn read_db(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ReadDbQuery>,
) -> ApiResult<impl IntoResponse> {
	let session = get_session(&state, id)?;
	let cancel = session.cancellation_token.child_token();
	if cancel.is_cancelled() {
		return Err(ApiError::Cancelled);
	}
	let db_path = resolve_cwd_scoped_path(&session, &query.path).await?;
	let response = run_blocking_with_cancel(cancel, {
		let db_path = db_path.clone();
		move || read_db_blocking(&db_path, query)
	})
	.await?;
	let etag = state.etag_cache.compute(&db_path).map_err(ApiError::Io)?;
	let mut headers = HeaderMap::new();
	headers.insert(
		header::ETAG,
		HeaderValue::from_str(&format!("\"{etag}\"")).map_err(anyhow::Error::from)?,
	);
	Ok((headers, Json(response)))
}
#[utoipa::path(
	post,
	path = "/sessions/{id}/write.db",
	params(
		("id" = Uuid, Path),
		("If-Match" = Option<String>, Header, description = "Existing ETag required for conditional writes")
	),
	request_body = WriteDbRequest,
	responses(
		(status = 200, body = WriteDbResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "session not found"),
		(status = 412, body = ErrorBody, description = "etag mismatch"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn write_db(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	headers: HeaderMap,
	Json(body): Json<WriteDbRequest>,
) -> ApiResult<impl IntoResponse> {
	let session = get_session(&state, id)?;
	let cancel = session.cancellation_token.child_token();
	if cancel.is_cancelled() {
		return Err(ApiError::Cancelled);
	}
	let db_path = resolve_cwd_scoped_path(&session, &body.path).await?;
	let _guard = tokio::select! {
		guard = session.edit_lock.lock() => guard,
		() = cancel.cancelled() => return Err(ApiError::Cancelled),
	};
	if let Some(if_match) = if_match_header(&headers) {
		let current_etag = if tokio::fs::try_exists(&db_path)
			.await
			.map_err(ApiError::Io)?
		{
			Some(state.etag_cache.compute(&db_path).map_err(ApiError::Io)?)
		} else {
			None
		};
		if !any_tag_matches(&if_match, current_etag.as_deref()) {
			return Err(ApiError::EtagMismatch);
		}
	}
	let affected = run_blocking_with_cancel(cancel, {
		let db_path = db_path.clone();
		let body = body.clone();
		move || write_db_blocking(&db_path, body)
	})
	.await?;
	state.etag_cache.invalidate(&db_path);
	let etag = state.etag_cache.compute(&db_path).map_err(ApiError::Io)?;
	session.read_cache.invalidate(&db_path);
	let _ = session
		.events
		.send(SessionEvent::FileChanged { path: body.path, etag: Some(etag) });
	Ok(Json(WriteDbResponse { affected }))
}
fn get_session(state: &AppState, id: Uuid) -> ApiResult<Arc<Session>> {
	state
		.sessions
		.get(id)
		.ok_or_else(|| ApiError::NotFound(format!("session {id}")))
}

fn read_db_blocking(db_path: &PathBuf, query: ReadDbQuery) -> ApiResult<Value> {
	let connection = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
		.map_err(|error| ApiError::BadRequest(format!("failed to open database: {error}")))?;
	if let Some(sql) = query.q.as_deref() {
		validate_select_query(sql)?;
		let rows = select_rows_from_sql(&connection, sql)?;
		return to_json_value(rows);
	}

	match query.table.as_deref() {
		None => Ok(to_json_value(list_tables(&connection)?)?),
		Some(table) => {
			validate_identifier(table, "table")?;
			let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
			let offset = query.offset.unwrap_or(DEFAULT_OFFSET);
			let metadata = table_metadata(&connection, table)?;
			if metadata.columns.is_empty() {
				return Err(ApiError::NotFound(format!("table {table}")));
			}
			if let Some(key) = query.key.as_deref() {
				let row = read_row_by_key(&connection, table, &metadata, key)?;
				return to_json_value(ReadDbResponse {
					rows:         vec![row],
					columns:      metadata.columns,
					rowid_column: metadata.rowid_column,
				});
			}

			let rows = if query.where_clause.is_some() || query.order.is_some() {
				read_filtered_rows(
					&connection,
					table,
					query.where_clause.as_deref(),
					query.order.as_deref(),
					limit,
					offset,
				)?
			} else {
				read_table_rows(&connection, table, limit, offset)?
			};
			Ok(to_json_value(ReadDbResponse {
				rows,
				columns: metadata.columns,
				rowid_column: metadata.rowid_column,
			})?)
		},
	}
}

fn write_db_blocking(db_path: &PathBuf, body: WriteDbRequest) -> ApiResult<u64> {
	let connection = Connection::open_with_flags(
		db_path,
		OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
	)
	.map_err(|error| ApiError::BadRequest(format!("failed to open database: {error}")))?;
	match body.op {
		WriteDbOp::Insert => insert_row(&connection, &body),
		WriteDbOp::Update => update_row(&connection, &body),
		WriteDbOp::Delete => delete_row(&connection, &body),
		WriteDbOp::Exec => exec_sql(&connection, &body),
	}
}

async fn run_blocking_with_cancel<T, F>(
	cancel: tokio_util::sync::CancellationToken,
	work: F,
) -> ApiResult<T>
where
	T: Send + 'static,
	F: FnOnce() -> ApiResult<T> + Send + 'static,
{
	let task = tokio::task::spawn_blocking(work);
	tokio::select! {
		result = task => result.map_err(|error| ApiError::Internal(error.into()))?,
		() = cancel.cancelled() => Err(ApiError::Cancelled),
	}
}

fn if_match_header(headers: &HeaderMap) -> Option<String> {
	headers
		.get(header::IF_MATCH)
		.and_then(|value| value.to_str().ok())
		.map(ToOwned::to_owned)
}

fn any_tag_matches(raw: &str, etag: Option<&str>) -> bool {
	raw.split(',').any(|candidate| {
		let trimmed = candidate.trim();
		let strong = trimmed.strip_prefix("W/").unwrap_or(trimmed);
		strong == "*" || etag.is_some_and(|etag| strong.trim_matches('"') == etag)
	})
}

fn validate_select_query(sql: &str) -> ApiResult<()> {
	let trimmed = sql.trim();
	if trimmed.is_empty() {
		return Err(ApiError::BadRequest("q must not be empty".to_owned()));
	}
	let uppercase = trimmed.to_ascii_uppercase();
	if !uppercase.starts_with("SELECT") && !uppercase.starts_with("WITH") {
		return Err(ApiError::BadRequest("q must be a single SELECT statement".to_owned()));
	}
	if let Some(index) = trimmed.find(';') {
		let tail = &trimmed[index + 1..];
		if tail.chars().any(|ch| !ch.is_whitespace()) {
			return Err(ApiError::BadRequest("q must contain exactly one statement".to_owned()));
		}
	}
	Ok(())
}

fn to_json_value<T>(value: T) -> ApiResult<Value>
where
	T: serde::Serialize,
{
	serde_json::to_value(value).map_err(|error| ApiError::Internal(error.into()))
}

fn validate_identifier(value: &str, field: &str) -> ApiResult<()> {
	let mut chars = value.chars();
	let Some(first) = chars.next() else {
		return Err(ApiError::BadRequest(format!("{field} must not be empty")));
	};
	if !(first.is_ascii_alphabetic() || first == '_') {
		return Err(ApiError::BadRequest(format!("invalid {field}")));
	}
	if chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
		Ok(())
	} else {
		Err(ApiError::BadRequest(format!("invalid {field}")))
	}
}

fn validate_expression(expr: &str, field: &str) -> ApiResult<()> {
	let trimmed = expr.trim();
	if trimmed.is_empty() {
		return Err(ApiError::BadRequest(format!("{field} must not be empty")));
	}
	if trimmed.contains(';')
		|| trimmed.contains("--")
		|| trimmed.contains("/*")
		|| trimmed.contains("*/")
	{
		return Err(ApiError::BadRequest(format!("invalid {field}")));
	}
	let mut single_quoted = false;
	let mut double_quoted = false;
	for ch in trimmed.chars() {
		match ch {
			'\'' if !double_quoted => single_quoted = !single_quoted,
			'"' if !single_quoted => double_quoted = !double_quoted,
			_ if single_quoted || double_quoted => {
				if ch.is_control() {
					return Err(ApiError::BadRequest(format!("invalid {field}")));
				}
			},
			_ if ch.is_ascii_alphanumeric()
				|| matches!(
					ch,
					'_' | '.'
						| ',' | '(' | ')'
						| ' ' | '\t' | '\n'
						| '\r' | '=' | '<'
						| '>' | '!' | '+'
						| '-' | '*' | '/'
						| '%'
				) => {},
			_ => return Err(ApiError::BadRequest(format!("invalid {field}"))),
		}
	}
	if single_quoted || double_quoted {
		return Err(ApiError::BadRequest(format!("invalid {field}")));
	}
	Ok(())
}

fn list_tables(connection: &Connection) -> ApiResult<SqliteTablesResponse> {
	let mut statement = connection
		.prepare(
			"SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER \
			 BY name",
		)
		.map_err(|error| ApiError::BadRequest(format!("failed to inspect tables: {error}")))?;
	let table_names = statement
		.query_map([], |row| row.get::<_, String>(0))
		.map_err(|error| ApiError::BadRequest(format!("failed to inspect tables: {error}")))?
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| ApiError::BadRequest(format!("failed to inspect tables: {error}")))?;
	let tables = table_names
		.into_iter()
		.map(|name| {
			let metadata = table_metadata(connection, &name)?;
			let sql = format!("SELECT COUNT(*) FROM {}", quote_identifier(&name));
			let row_count_i64 = connection
				.query_row(&sql, [], |row| row.get::<_, i64>(0))
				.map_err(|error| {
					ApiError::BadRequest(format!("failed to count rows for {name}: {error}"))
				})?;
			let row_count = u64::try_from(row_count_i64)
				.map_err(|_| ApiError::BadRequest(format!("negative row count for {name}")))?;
			Ok(SqliteTableInfo { name, row_count, columns: metadata.columns })
		})
		.collect::<ApiResult<Vec<_>>>()?;
	Ok(SqliteTablesResponse { tables })
}

fn table_metadata(connection: &Connection, table: &str) -> ApiResult<TableMetadata> {
	let pragma = format!("PRAGMA table_info({})", quote_identifier(table));
	let mut statement = connection
		.prepare(&pragma)
		.map_err(|error| ApiError::BadRequest(format!("failed to inspect {table}: {error}")))?;
	let infos = statement
		.query_map([], |row| {
			Ok(ColumnInfo {
				name:      row.get(1)?,
				decl_type: row.get::<_, String>(2)?,
				pk_index:  row.get::<_, i64>(5)?,
			})
		})
		.map_err(|error| ApiError::BadRequest(format!("failed to inspect {table}: {error}")))?
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| ApiError::BadRequest(format!("failed to inspect {table}: {error}")))?;
	let primary_key_columns = infos
		.iter()
		.filter(|info| info.pk_index > 0)
		.map(|info| info.name.clone())
		.collect::<Vec<_>>();
	let rowid_column = match primary_key_columns.as_slice() {
		[column] => Some(column.clone()),
		[] => Some("rowid".to_owned()),
		_ => None,
	};
	let columns = infos
		.into_iter()
		.map(|info| SqliteColumn { name: info.name, r#type: info.decl_type })
		.collect();
	Ok(TableMetadata { columns, rowid_column })
}

fn read_row_by_key(
	connection: &Connection,
	table: &str,
	metadata: &TableMetadata,
	key: &str,
) -> ApiResult<SqliteRow> {
	let key_column = metadata.rowid_column.as_deref().unwrap_or("rowid");
	let sql = format!(
		"SELECT * FROM {} WHERE {} = ?1 LIMIT 1",
		quote_identifier(table),
		quote_identifier(key_column),
	);
	let mut statement = connection
		.prepare(&sql)
		.map_err(|error| ApiError::BadRequest(format!("failed to query {table}: {error}")))?;
	let column_names = statement
		.column_names()
		.into_iter()
		.map(ToOwned::to_owned)
		.collect::<Vec<_>>();
	statement
		.query_row([key], |row| row_to_json(row, &column_names))
		.optional()
		.map_err(|error| ApiError::BadRequest(format!("failed to query {table}: {error}")))?
		.ok_or_else(|| ApiError::NotFound(format!("row {key} not found in {table}")))
}

fn read_filtered_rows(
	connection: &Connection,
	table: &str,
	where_clause: Option<&str>,
	order: Option<&str>,
	limit: u64,
	offset: u64,
) -> ApiResult<Vec<SqliteRow>> {
	let mut sql = format!("SELECT * FROM {}", quote_identifier(table));
	if let Some(where_clause) = where_clause {
		validate_expression(where_clause, "where")?;
		sql.push_str(" WHERE ");
		sql.push_str(where_clause);
	}
	if let Some(order) = order {
		validate_expression(order, "order")?;
		sql.push_str(" ORDER BY ");
		sql.push_str(order);
	}
	sql.push_str(" LIMIT ?1 OFFSET ?2");
	query_rows(connection, &sql, params![u64_to_i64(limit)?, u64_to_i64(offset)?])
}

fn read_table_rows(
	connection: &Connection,
	table: &str,
	limit: u64,
	offset: u64,
) -> ApiResult<Vec<SqliteRow>> {
	let sql = format!("SELECT * FROM {} LIMIT ?1 OFFSET ?2", quote_identifier(table));
	query_rows(connection, &sql, params![u64_to_i64(limit)?, u64_to_i64(offset)?])
}

fn query_rows<P>(connection: &Connection, sql: &str, params: P) -> ApiResult<Vec<SqliteRow>>
where
	P: rusqlite::Params,
{
	let mut statement = connection
		.prepare(sql)
		.map_err(|error| ApiError::BadRequest(format!("failed to prepare query: {error}")))?;
	let column_names = statement
		.column_names()
		.into_iter()
		.map(ToOwned::to_owned)
		.collect::<Vec<_>>();
	let rows = statement
		.query_map(params, |row| row_to_json(row, &column_names))
		.map_err(|error| ApiError::BadRequest(format!("failed to execute query: {error}")))?
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| ApiError::BadRequest(format!("failed to execute query: {error}")))?;
	Ok(rows)
}

fn select_rows_from_sql(connection: &Connection, sql: &str) -> ApiResult<Vec<SqliteRow>> {
	let mut statement = connection
		.prepare(sql)
		.map_err(|error| ApiError::BadRequest(format!("failed to prepare query: {error}")))?;
	if !statement.readonly() {
		return Err(ApiError::BadRequest("q must be read-only".to_owned()));
	}
	let column_names = statement
		.column_names()
		.into_iter()
		.map(ToOwned::to_owned)
		.collect::<Vec<_>>();
	let rows = statement
		.query_map([], |row| row_to_json(row, &column_names))
		.map_err(|error| ApiError::BadRequest(format!("failed to execute query: {error}")))?
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| ApiError::BadRequest(format!("failed to execute query: {error}")))?;
	Ok(rows)
}

fn row_to_json(row: &rusqlite::Row<'_>, column_names: &[String]) -> rusqlite::Result<SqliteRow> {
	let mut object = BTreeMap::new();
	for (index, name) in column_names.iter().enumerate() {
		object.insert(name.clone(), value_ref_to_json(row.get_ref(index)?));
	}
	Ok(object)
}

fn value_ref_to_json(value: ValueRef<'_>) -> Value {
	match value {
		ValueRef::Null => Value::Null,
		ValueRef::Integer(value) => Value::from(value),
		ValueRef::Real(value) => Value::from(value),
		ValueRef::Text(value) => Value::String(String::from_utf8_lossy(value).into_owned()),
		ValueRef::Blob(value) => Value::Array(value.iter().copied().map(Value::from).collect()),
	}
}

fn insert_row(connection: &Connection, body: &WriteDbRequest) -> ApiResult<u64> {
	let table = required_table(body)?;
	let row = required_row(body)?;
	let entries = validated_row_entries(row)?;
	if entries.is_empty() {
		return Err(ApiError::BadRequest("row must not be empty".to_owned()));
	}
	let columns = entries
		.iter()
		.map(|(name, _)| quote_identifier(name))
		.collect::<Vec<_>>();
	let placeholders = (1..=entries.len())
		.map(|index| format!("?{index}"))
		.collect::<Vec<_>>();
	let values = entries
		.into_iter()
		.map(|(_, value)| json_to_sql_value(value))
		.collect::<ApiResult<Vec<_>>>()?;
	let sql = format!(
		"INSERT INTO {} ({}) VALUES ({})",
		quote_identifier(table),
		columns.join(", "),
		placeholders.join(", "),
	);
	let affected = connection
		.execute(&sql, params_from_iter(values.iter()))
		.map_err(|error| ApiError::BadRequest(format!("insert failed: {error}")))?;
	usize_to_u64(affected)
}

fn update_row(connection: &Connection, body: &WriteDbRequest) -> ApiResult<u64> {
	let table = required_table(body)?;
	let key = body
		.key
		.as_deref()
		.ok_or_else(|| ApiError::BadRequest("key is required for update".to_owned()))?;
	let row = required_row(body)?;
	let entries = validated_row_entries(row)?;
	if entries.is_empty() {
		return Err(ApiError::BadRequest("row must not be empty".to_owned()));
	}
	let metadata = table_metadata(connection, table)?;
	if metadata.columns.is_empty() {
		return Err(ApiError::NotFound(format!("table {table}")));
	}
	let key_column = metadata.rowid_column.as_deref().unwrap_or("rowid");
	let assignments = entries
		.iter()
		.enumerate()
		.map(|(index, (name, _))| format!("{} = ?{}", quote_identifier(name), index + 1))
		.collect::<Vec<_>>();
	let mut values = entries
		.into_iter()
		.map(|(_, value)| json_to_sql_value(value))
		.collect::<ApiResult<Vec<_>>>()?;
	values.push(rusqlite::types::Value::Text(key.to_owned()));
	let sql = format!(
		"UPDATE {} SET {} WHERE {} = ?{}",
		quote_identifier(table),
		assignments.join(", "),
		quote_identifier(key_column),
		values.len(),
	);
	let affected = connection
		.execute(&sql, params_from_iter(values.iter()))
		.map_err(|error| ApiError::BadRequest(format!("update failed: {error}")))?;
	usize_to_u64(affected)
}

fn delete_row(connection: &Connection, body: &WriteDbRequest) -> ApiResult<u64> {
	let table = required_table(body)?;
	let key = body
		.key
		.as_deref()
		.ok_or_else(|| ApiError::BadRequest("key is required for delete".to_owned()))?;
	let metadata = table_metadata(connection, table)?;
	if metadata.columns.is_empty() {
		return Err(ApiError::NotFound(format!("table {table}")));
	}
	let key_column = metadata.rowid_column.as_deref().unwrap_or("rowid");
	let sql = format!(
		"DELETE FROM {} WHERE {} = ?1",
		quote_identifier(table),
		quote_identifier(key_column),
	);
	let affected = connection
		.execute(&sql, [key])
		.map_err(|error| ApiError::BadRequest(format!("delete failed: {error}")))?;
	usize_to_u64(affected)
}

fn exec_sql(connection: &Connection, body: &WriteDbRequest) -> ApiResult<u64> {
	let sql = body
		.sql
		.as_deref()
		.ok_or_else(|| ApiError::BadRequest("sql is required for exec".to_owned()))?;
	let trimmed = sql.trim();
	if trimmed.is_empty() {
		return Err(ApiError::BadRequest("sql must not be empty".to_owned()));
	}
	if let Ok(affected) = connection.execute(trimmed, []) {
		return usize_to_u64(affected);
	}
	connection
		.execute_batch(trimmed)
		.map_err(|error| ApiError::BadRequest(format!("exec failed: {error}")))?;
	Ok(0)
}

fn required_table(body: &WriteDbRequest) -> ApiResult<&str> {
	let table = body
		.table
		.as_deref()
		.ok_or_else(|| ApiError::BadRequest("table is required".to_owned()))?;
	validate_identifier(table, "table")?;
	Ok(table)
}

fn required_row(body: &WriteDbRequest) -> ApiResult<&BTreeMap<String, Value>> {
	body
		.row
		.as_ref()
		.ok_or_else(|| ApiError::BadRequest("row is required".to_owned()))
}

fn validated_row_entries(row: &BTreeMap<String, Value>) -> ApiResult<Vec<(&str, &Value)>> {
	row.iter()
		.map(|(name, value)| {
			validate_identifier(name, "column")?;
			Ok((name.as_str(), value))
		})
		.collect()
}

fn json_to_sql_value(value: &Value) -> ApiResult<rusqlite::types::Value> {
	match value {
		Value::Null => Ok(rusqlite::types::Value::Null),
		Value::Bool(value) => Ok(rusqlite::types::Value::Integer(i64::from(*value))),
		Value::Number(value) => {
			if let Some(integer) = value.as_i64() {
				Ok(rusqlite::types::Value::Integer(integer))
			} else if let Some(float) = value.as_f64() {
				Ok(rusqlite::types::Value::Real(float))
			} else {
				Err(ApiError::BadRequest("unsupported numeric value".to_owned()))
			}
		},
		Value::String(value) => Ok(rusqlite::types::Value::Text(value.clone())),
		Value::Array(_) | Value::Object(_) => {
			Err(ApiError::BadRequest("row values must be scalar JSON values".to_owned()))
		},
	}
}

fn quote_identifier(identifier: &str) -> String {
	format!("\"{identifier}\"")
}

fn usize_to_u64(value: usize) -> ApiResult<u64> {
	u64::try_from(value)
		.map_err(|_| ApiError::Internal(anyhow::anyhow!("value does not fit into u64")))
}

fn u64_to_i64(value: u64) -> ApiResult<i64> {
	i64::try_from(value).map_err(|_| ApiError::BadRequest("value is too large".to_owned()))
}

#[derive(Debug)]
struct ColumnInfo {
	name:      String,
	decl_type: String,
	pk_index:  i64,
}

#[derive(Debug)]
struct TableMetadata {
	columns:      Vec<SqliteColumn>,
	rowid_column: Option<String>,
}

#[cfg(test)]
mod tests {
	use std::{sync::mpsc, time::Duration};

	use tokio::sync::oneshot;
	use tokio_util::sync::CancellationToken;

	use super::run_blocking_with_cancel;
	use crate::protocol::error::ApiError;

	#[tokio::test]
	async fn run_blocking_with_cancel_returns_worker_result() {
		let result = run_blocking_with_cancel(CancellationToken::new(), || Ok::<_, ApiError>(7))
			.await
			.expect("worker result");
		assert_eq!(result, 7);
	}

	#[tokio::test]
	async fn run_blocking_with_cancel_returns_cancelled_without_waiting_for_worker() {
		let cancel = CancellationToken::new();
		let (started_tx, started_rx) = oneshot::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let task = tokio::spawn(run_blocking_with_cancel(cancel.clone(), move || {
			let _ = started_tx.send(());
			let _ = release_rx.recv();
			Ok::<_, ApiError>(11)
		}));
		started_rx.await.expect("worker started");
		cancel.cancel();
		let result = tokio::time::timeout(Duration::from_secs(1), task)
			.await
			.expect("cancelled result before worker release")
			.expect("join result");
		assert!(matches!(result, Err(ApiError::Cancelled)));
		release_tx.send(()).expect("release worker");
	}
}
