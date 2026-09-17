//! Axum router wiring + `OpenAPI` emission.

use std::collections::BTreeMap;

use axum::{
	Router,
	body::{self, Body},
	http::{
		Method, Request, Uri,
		header::{AUTHORIZATION, CONTENT_TYPE, ETAG, IF_MATCH},
	},
	middleware::{self, Next},
	response::Response,
	routing::{delete, get, patch, post, put},
};
use serde_json::{Value, json};
use tower_http::{
	cors::{AllowOrigin, Any, CorsLayer},
	trace::TraceLayer,
};
use tracing::{field::Empty, info_span};
use utoipa::OpenApi;
use uuid::Uuid;
#[path = "metrics.rs"]
mod metrics;

use crate::{
	auth,
	handlers::{bash, cdp, dap, edit, eval, fs, lsp, sessions, sqlite},
	protocol::{
		ErrorBody, SessionEvent,
		events::{LogLevel, LogRecord},
		requests, responses,
	},
	request_id,
	state::AppState,
};

#[derive(OpenApi)]
#[openapi(
	info(
		title = "Remote Workspace Protocol",
		version = env!("CARGO_PKG_VERSION"),
		description = "Thin remote workspace gateway: sessions, filesystem, edit primitives, bash, and tunnels.",
	),
	paths(
		sessions::create_session,
		sessions::delete_session,
		sessions::set_cwd,
		sessions::patch_env,
		sessions::put_watch,
		sessions::events,
		sessions::logs,
		fs::read_lines,
		fs::read_blob,
		fs::read_ast,
		fs::image_meta,
		fs::list_workspace,
		fs::write_lines,
		fs::write_blob,
		fs::delete_file,
		fs::mkdir,
		fs::rename_path,
		fs::stat,
		fs::exists,
		fs::archive_open,
		fs::archive_entries,
		fs::archive_read,
		fs::archive_snapshot_entries,
		fs::archive_snapshot_entry,
		fs::delete_archive_snapshot,
		fs::archive_write,
		fs::glob,
		fs::grep,
		fs::grep_ast,
		edit::edit_replace,
		edit::edit_patch,
		edit::edit_ast,
		sqlite::read_db,
		sqlite::write_db,
		bash::bash_exec,
		eval::get_eval,
		eval::put_eval,
		eval::delete_eval,
		eval::exec_eval,
		lsp::get_lsp,
		lsp::put_lsp,
		lsp::delete_lsp,
		dap::get_dap,
		dap::put_dap,
		dap::delete_dap,
		cdp::list_cdp,
		cdp::get_cdp,
		cdp::put_cdp,
		cdp::delete_cdp,
	),
	components(schemas(
		ErrorBody,
		SessionEvent,
		LogLevel,
		LogRecord,
		requests::CreateSessionRequest,
		requests::SetCwdRequest,
		requests::PatchEnvRequest,
		requests::EditReplaceRequest,
		requests::EditPatchRequest,
		requests::Hunk,
		requests::EditAstRequest,
		requests::AstOp,
		requests::BashExecRequest,
		bash::BashExecBody,
		requests::NamedHandleConfig,
		requests::DapTransport,
		requests::ReadDbQuery,
		requests::WriteDbOp,
		requests::WriteDbRequest,
		responses::CreateSessionResponse,
		responses::EditResult,
		responses::EditOp,
		responses::AstEditResult,
		responses::AstFileChange,
		responses::ReadAstResponse,
		responses::ReadAstSegment,
		responses::ImageMetadataResponse,
		responses::ListWorkspaceResponse,
		responses::WorkspaceEntry,
		responses::StatResponse,
		responses::ArchiveEntry,
		responses::ArchiveEntriesResponse,
		responses::ArchiveReadHeaders,
		responses::ArchiveWriteResponse,
		fs::ArchiveSnapshotOpenResponse,
		responses::BlobSizeResponse,
		responses::WriteDbResponse,
	)),
)]
pub struct ApiDoc;

/// Build the full router. Caller wires it into an axum server.
pub fn build_router(state: AppState, cors_origins: Vec<String>) -> Router {
	let auth_state = state.clone();
	let _metrics_handle = metrics::install_recorder();
	let protected = Router::new()
		// Sessions
		.route("/sessions", post(sessions::create_session))
		.route("/sessions/{id}", delete(sessions::delete_session))
		.route("/sessions/{id}/cwd", put(sessions::set_cwd))
		.route("/sessions/{id}/env", patch(sessions::patch_env))
		.route("/sessions/{id}/watch", put(sessions::put_watch))
		.route("/sessions/{id}/events", get(sessions::events))
		.route("/sessions/{id}/logs", get(sessions::logs))
		// FS
		.route("/sessions/{id}/read.lines", get(fs::read_lines))
		.route("/sessions/{id}/read.blob", get(fs::read_blob))
		.route("/sessions/{id}/read.ast", get(fs::read_ast))
		.route("/sessions/{id}/image_meta", get(fs::image_meta))
		.route("/sessions/{id}/list_workspace", get(fs::list_workspace))
		.route("/sessions/{id}/write.lines", put(fs::write_lines))
		.route("/sessions/{id}/write.blob", put(fs::write_blob))
		.route("/sessions/{id}/fs", delete(fs::delete_file))
		.route("/sessions/{id}/mkdir", post(fs::mkdir))
		.route("/sessions/{id}/rename", post(fs::rename_path))
		.route("/sessions/{id}/stat", get(fs::stat))
		.route("/sessions/{id}/exists", get(fs::exists))
		.route("/sessions/{id}/archive.open", post(fs::archive_open))
		.route("/sessions/{id}/archive.entries", get(fs::archive_entries))
		.route("/sessions/{id}/archive.read", get(fs::archive_read))
		.route("/archive/{snapshot_id}/entries", get(fs::archive_snapshot_entries))
		.route("/archive/{snapshot_id}/entry", get(fs::archive_snapshot_entry))
		.route("/archive/{snapshot_id}", delete(fs::delete_archive_snapshot))
		.route("/sessions/{id}/archive.write", put(fs::archive_write))
		.route("/sessions/{id}/archive.bulk_write", put(fs::archive_bulk_write))
		.route("/sessions/{id}/glob", get(fs::glob))
		.route("/sessions/{id}/grep", get(fs::grep))
		.route("/sessions/{id}/grep.ast", get(fs::grep_ast))
		// Edit
		.route("/sessions/{id}/edit.replace", post(edit::edit_replace))
		.route("/sessions/{id}/edit.patch", post(edit::edit_patch))
		.route("/sessions/{id}/edit.ast", post(edit::edit_ast))
		// Bash
		.route("/sessions/{id}/bash.exec", post(bash::bash_exec))
		// SQLite
		.route("/sessions/{id}/read.db", get(sqlite::read_db))
		.route("/sessions/{id}/write.db", post(sqlite::write_db))
		// Named handles
		.route(
			"/eval/{name}",
			get(eval::get_eval)
				.put(eval::put_eval)
				.delete(eval::delete_eval)
				.post(eval::exec_eval),
		)
		.route("/lsp/{name}", get(lsp::get_lsp).put(lsp::put_lsp).delete(lsp::delete_lsp))
		.route("/dap/{name}", get(dap::get_dap).put(dap::put_dap).delete(dap::delete_dap))
		.route("/cdp", get(cdp::list_cdp))
		.route("/cdp/{name}", get(cdp::get_cdp).put(cdp::put_cdp).delete(cdp::delete_cdp))
		// Schema
		.route("/openapi.json", get(openapi_json))
		.layer(middleware::from_fn_with_state(state.clone(), session_log_middleware))
		.layer(middleware::from_fn_with_state(auth_state, auth::require_bearer_auth));

	let router = Router::new()
		// `/metrics` is intentionally unauthenticated so Prometheus can scrape it
		// without carrying the bearer token used for interactive endpoints.
		.route("/metrics", get(metrics::scrape))
		.merge(protected)
		.layer(middleware::from_fn(metrics::middleware))
		.layer(middleware::from_fn(request_id::middleware))
		.layer(TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
			info_span!(
				"http_request",
				method = %request.method(),
				uri = %request.uri(),
				version = ?request.version(),
				request_id = Empty,
			)
		}))
		.with_state(state);

	match build_cors_layer(cors_origins) {
		Some(cors_layer) => router.layer(cors_layer),
		None => router,
	}
}

fn build_cors_layer(cors_origins: Vec<String>) -> Option<CorsLayer> {
	if cors_origins.is_empty() {
		return None;
	}

	let allow_origin: AllowOrigin = if cors_origins.iter().any(|origin| origin == "*") {
		Any.into()
	} else {
		cors_origins
			.into_iter()
			.map(|origin| http::HeaderValue::from_str(&origin).expect("clap validates CORS origins"))
			.collect::<Vec<_>>()
			.into()
	};

	Some(
		CorsLayer::new()
			.allow_origin(allow_origin)
			.allow_methods([
				Method::GET,
				Method::POST,
				Method::PUT,
				Method::PATCH,
				Method::DELETE,
				Method::OPTIONS,
			])
			.allow_headers([AUTHORIZATION, CONTENT_TYPE, IF_MATCH, request_id::X_REQUEST_ID_HEADER])
			.expose_headers([ETAG, request_id::X_REQUEST_ID_HEADER]),
	)
}

async fn openapi_json() -> axum::Json<utoipa::openapi::OpenApi> {
	axum::Json(ApiDoc::openapi())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionLogKind {
	Write,
	Edit,
	SqliteWrite,
}

#[derive(Debug, Clone)]
struct SessionLogTarget {
	session_id: Uuid,
	kind:       SessionLogKind,
	action:     String,
	path:       Option<String>,
}

async fn session_log_middleware(
	axum::extract::State(state): axum::extract::State<AppState>,
	request: Request<Body>,
	next: Next,
) -> Response {
	let Some(target) = classify_session_log_request(&request) else {
		return next.run(request).await;
	};
	let (request, path_from_body) = if target.path.is_some() {
		(request, None)
	} else if matches!(target.kind, SessionLogKind::Edit | SessionLogKind::SqliteWrite) {
		buffer_request_body_for_log(request).await
	} else {
		(request, None)
	};
	let response = next.run(request).await;
	if let Some(session) = state.sessions.get(target.session_id) {
		let status = response.status();
		let mut fields = BTreeMap::from([
			("action".to_owned(), Value::String(target.action.clone())),
			("status".to_owned(), json!(status.as_u16())),
		]);
		if let Some(path) = target.path.or(path_from_body) {
			fields.insert("path".to_owned(), Value::String(path));
		}
		let (level, message) = match (target.kind, status.is_success()) {
			(SessionLogKind::Write, true) => (LogLevel::Info, "write succeeded"),
			(SessionLogKind::Write, false) => (LogLevel::Error, "write failed"),
			(SessionLogKind::Edit, true) => (LogLevel::Info, "edit succeeded"),
			(SessionLogKind::Edit, false) => (LogLevel::Error, "edit failed"),
			(SessionLogKind::SqliteWrite, true) => (LogLevel::Info, "sqlite write succeeded"),
			(SessionLogKind::SqliteWrite, false) => (LogLevel::Error, "sqlite write failed"),
		};
		let source = match target.kind {
			SessionLogKind::Write => "write",
			SessionLogKind::Edit => "edit",
			SessionLogKind::SqliteWrite => "sqlite.write",
		};
		session.emit_log(level, source, message, fields);
	}
	response
}

fn classify_session_log_request(request: &Request<Body>) -> Option<SessionLogTarget> {
	let mut segments = request
		.uri()
		.path()
		.split('/')
		.filter(|segment| !segment.is_empty());
	if segments.next()? != "sessions" {
		return None;
	}
	let session_id = Uuid::parse_str(segments.next()?).ok()?;
	let action = segments.next()?;
	let kind = match (request.method(), action) {
		(&Method::PUT, "write.lines" | "write.blob") => SessionLogKind::Write,
		(&Method::POST, "edit.replace" | "edit.patch" | "edit.ast") => SessionLogKind::Edit,
		(&Method::POST, "write.db") => SessionLogKind::SqliteWrite,
		_ => return None,
	};
	Some(SessionLogTarget {
		session_id,
		kind,
		action: action.to_owned(),
		path: extract_query_field(request.uri(), "path"),
	})
}

async fn buffer_request_body_for_log(request: Request<Body>) -> (Request<Body>, Option<String>) {
	let (parts, body) = request.into_parts();
	let Ok(bytes) = body::to_bytes(body, 16 * 1024 * 1024).await else {
		return (Request::from_parts(parts, Body::empty()), None);
	};
	let path = serde_json::from_slice::<Value>(&bytes)
		.ok()
		.and_then(|body| {
			body
				.get("path")
				.and_then(Value::as_str)
				.map(ToOwned::to_owned)
		});
	(Request::from_parts(parts, Body::from(bytes)), path)
}

fn extract_query_field(uri: &Uri, key: &str) -> Option<String> {
	let query = uri.query()?;
	url::form_urlencoded::parse(query.as_bytes())
		.find_map(|(name, value)| (name == key).then(|| value.into_owned()))
}
