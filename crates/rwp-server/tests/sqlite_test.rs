use std::{collections::BTreeMap, net::SocketAddr, path::Path, sync::Arc};

use rwp_server::{AppState, build_router, protocol::SessionEvent, session::Session};
use serde_json::json;
use tempfile::TempDir;

struct TestServer {
	addr:       SocketAddr,
	session_id: uuid::Uuid,
	session:    Arc<Session>,
	_tempdir:   TempDir,
}

async fn start_server() -> TestServer {
	let tempdir = tempfile::tempdir().expect("tempdir");
	let db_path = tempdir.path().join("fixture.db");
	create_fixture_db(&db_path);

	let state = AppState::new();
	let session = Arc::new(Session::new(tempdir.path().to_path_buf(), BTreeMap::new()));
	let session_id = state.sessions.insert(session.clone());

	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(state, Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});

	TestServer { addr, session_id, session, _tempdir: tempdir }
}

fn create_fixture_db(path: &Path) {
	let connection = rusqlite::Connection::open(path).expect("open fixture db");
	connection
		.execute_batch(
			"CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL, qty INTEGER NOT \
			 NULL);CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);INSERT INTO \
			 widgets (name, qty) VALUES ('alpha', 2), ('beta', 5);INSERT INTO notes (body) VALUES \
			 ('first note');",
		)
		.expect("seed fixture db");
}

fn db_url(server: &TestServer, suffix: &str) -> String {
	format!("http://{}/sessions/{}/{}", server.addr, server.session_id, suffix)
}

#[tokio::test]
async fn list_tables_returns_schema_and_counts() {
	let server = start_server().await;
	let client = reqwest::Client::new();

	let response = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("send read.db request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let body: serde_json::Value = response.json().await.expect("json body");
	let tables = body
		.get("tables")
		.and_then(serde_json::Value::as_array)
		.expect("tables array");
	assert_eq!(tables.len(), 2);
	assert_eq!(tables[0].get("name"), Some(&json!("notes")));
	assert_eq!(tables[0].get("row_count"), Some(&json!(1)));
	assert_eq!(tables[1].get("name"), Some(&json!("widgets")));
	assert_eq!(tables[1].get("row_count"), Some(&json!(2)));
	let widget_columns = tables[1]
		.get("columns")
		.and_then(serde_json::Value::as_array)
		.expect("widget columns");
	assert_eq!(widget_columns[0].get("name"), Some(&json!("id")));
	assert_eq!(widget_columns[0].get("type"), Some(&json!("INTEGER")));
}

#[tokio::test]
async fn table_reads_support_sampling_row_lookup_select_and_validation() {
	let server = start_server().await;
	let client = reqwest::Client::new();

	let sample = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db"), ("table", "widgets"), ("order", "id ASC"), ("limit", "1")])
		.send()
		.await
		.expect("send sample request");
	assert_eq!(sample.status(), reqwest::StatusCode::OK);
	let sample_body: serde_json::Value = sample.json().await.expect("sample json");
	assert_eq!(sample_body.get("rowid_column"), Some(&json!("id")));
	let rows = sample_body
		.get("rows")
		.and_then(serde_json::Value::as_array)
		.expect("rows array");
	assert_eq!(rows.len(), 1);
	assert_eq!(rows[0].get("name"), Some(&json!("alpha")));

	let by_key = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db"), ("table", "widgets"), ("key", "2")])
		.send()
		.await
		.expect("send key request");
	assert_eq!(by_key.status(), reqwest::StatusCode::OK);
	let by_key_body: serde_json::Value = by_key.json().await.expect("key json");
	assert_eq!(by_key_body.pointer("/rows/0/name"), Some(&json!("beta")));

	let select = client
		.get(db_url(&server, "read.db"))
		.query(&[
			("path", "fixture.db"),
			("q", "SELECT name, qty FROM widgets WHERE qty >= 2 ORDER BY id"),
		])
		.send()
		.await
		.expect("send select request");
	assert_eq!(select.status(), reqwest::StatusCode::OK);
	let select_body: serde_json::Value = select.json().await.expect("select json");
	assert_eq!(
		select_body,
		json!([
			{"name": "alpha", "qty": 2},
			{"name": "beta", "qty": 5}
		])
	);

	let filtered = client
		.get(db_url(&server, "read.db"))
		.query(&[
			("path", "fixture.db"),
			("table", "widgets"),
			("where", "qty >= 2"),
			("order", "qty DESC"),
			("limit", "1"),
			("offset", "0"),
		])
		.send()
		.await
		.expect("send filtered request");
	assert_eq!(filtered.status(), reqwest::StatusCode::OK);
	let filtered_body: serde_json::Value = filtered.json().await.expect("filtered json");
	assert_eq!(filtered_body.pointer("/rows/0/name"), Some(&json!("beta")));

	let rejected = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db"), ("q", "DELETE FROM widgets")])
		.send()
		.await
		.expect("send rejected request");
	assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
	let rejected_body: serde_json::Value = rejected.json().await.expect("rejected json");
	assert_eq!(rejected_body.get("code"), Some(&json!("bad-request")));

	let injected = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db"), ("table", "widgets; DROP TABLE notes")])
		.send()
		.await
		.expect("send injected request");
	assert_eq!(injected.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn write_db_supports_insert_update_delete_and_exec_with_events() {
	let server = start_server().await;
	let client = reqwest::Client::new();
	let mut events = server.session.events.subscribe();

	let insert = client
		.post(db_url(&server, "write.db"))
		.json(&json!({
			"path": "fixture.db",
			"op": "insert",
			"table": "widgets",
			"row": {"name": "gamma", "qty": 9}
		}))
		.send()
		.await
		.expect("send insert request");
	assert_eq!(insert.status(), reqwest::StatusCode::OK);
	assert_eq!(
		insert
			.json::<serde_json::Value>()
			.await
			.expect("insert json"),
		json!({"affected": 1})
	);
	let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
		.await
		.expect("event timeout")
		.expect("event recv");
	match event {
		SessionEvent::FileChanged { path, etag } => {
			assert_eq!(path, "fixture.db");
			assert!(etag.as_deref().is_some_and(|etag| !etag.is_empty()), "etag should not be empty");
		},
		other => panic!("unexpected event: {other:?}"),
	}

	let inserted_row = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db"), ("q", "SELECT qty FROM widgets WHERE name = 'gamma'")])
		.send()
		.await
		.expect("send inserted row request");
	assert_eq!(inserted_row.status(), reqwest::StatusCode::OK);
	assert_eq!(
		inserted_row
			.json::<serde_json::Value>()
			.await
			.expect("inserted row json"),
		json!([
			{"qty": 9}
		])
	);

	let update = client
		.post(db_url(&server, "write.db"))
		.json(&json!({
			"path": "fixture.db",
			"op": "update",
			"table": "widgets",
			"key": "1",
			"row": {"qty": 7}
		}))
		.send()
		.await
		.expect("send update request");
	assert_eq!(update.status(), reqwest::StatusCode::OK);
	assert_eq!(
		update
			.json::<serde_json::Value>()
			.await
			.expect("update json"),
		json!({"affected": 1})
	);

	let updated_row = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db"), ("table", "widgets"), ("key", "1")])
		.send()
		.await
		.expect("send updated row request");
	assert_eq!(updated_row.status(), reqwest::StatusCode::OK);
	assert_eq!(
		updated_row
			.json::<serde_json::Value>()
			.await
			.expect("updated row json")
			.pointer("/rows/0/qty"),
		Some(&json!(7))
	);

	let delete = client
		.post(db_url(&server, "write.db"))
		.json(&json!({
			"path": "fixture.db",
			"op": "delete",
			"table": "widgets",
			"key": "2"
		}))
		.send()
		.await
		.expect("send delete request");
	assert_eq!(delete.status(), reqwest::StatusCode::OK);
	assert_eq!(
		delete
			.json::<serde_json::Value>()
			.await
			.expect("delete json"),
		json!({"affected": 1})
	);

	let deleted_row = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db"), ("table", "widgets"), ("key", "2")])
		.send()
		.await
		.expect("send deleted row request");
	assert_eq!(deleted_row.status(), reqwest::StatusCode::NOT_FOUND);

	let exec = client
		.post(db_url(&server, "write.db"))
		.json(&json!({
			"path": "fixture.db",
			"op": "exec",
			"sql": "CREATE TABLE audit_log (id INTEGER PRIMARY KEY, body TEXT NOT NULL)"
		}))
		.send()
		.await
		.expect("send exec request");
	assert_eq!(exec.status(), reqwest::StatusCode::OK);
	assert_eq!(exec.json::<serde_json::Value>().await.expect("exec json"), json!({"affected": 0}));

	let tables = client
		.get(db_url(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("send tables request");
	assert_eq!(tables.status(), reqwest::StatusCode::OK);
	let tables_body: serde_json::Value = tables.json().await.expect("tables json");
	let tables_array = tables_body
		.get("tables")
		.and_then(serde_json::Value::as_array)
		.expect("tables array");
	assert!(
		tables_array.iter().any(|table| {
			table.get("name").and_then(serde_json::Value::as_str) == Some("audit_log")
		})
	);
}
