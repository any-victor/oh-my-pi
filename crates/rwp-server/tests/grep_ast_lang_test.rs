use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use rwp_server::{AppState, build_router, session::Session};
use serde::Deserialize;
use tempfile::TempDir;

async fn start_server(state: AppState) -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(state, Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});
	addr
}

fn url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

fn setup_session() -> (TempDir, AppState, Arc<Session>) {
	let tempdir = tempfile::tempdir().expect("tempdir");
	let state = AppState::new();
	let session = Arc::new(Session::new(tempdir.path().to_path_buf(), BTreeMap::new()));
	state.sessions.insert(session.clone());
	(tempdir, state, session)
}

#[derive(Debug, Deserialize)]
struct GrepAstLine {
	path: String,
	text: String,
}

#[derive(Debug, Deserialize)]
struct GrepAstSummary {
	#[serde(rename = "parseErrors")]
	#[allow(dead_code, reason = "fields kept for wire-compat NDJSON parsing")]
	parse_errors:   Vec<serde_json::Value>,
	#[serde(rename = "filesSearched")]
	files_searched: usize,
	#[serde(rename = "limitReached")]
	limit_reached:  bool,
}

async fn grep_ast_lines(
	client: &reqwest::Client,
	addr: SocketAddr,
	session_id: uuid::Uuid,
	query: &[(&str, &str)],
) -> (Vec<GrepAstLine>, Option<GrepAstSummary>) {
	let response = client
		.get(url(addr, &format!("/sessions/{session_id}/grep.ast")))
		.query(query)
		.send()
		.await
		.expect("grep.ast request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let mut lines = Vec::new();
	let mut summary = None;
	for line in response.text().await.expect("grep.ast text").lines() {
		let value: serde_json::Value = serde_json::from_str(line).expect("valid ndjson line");
		if value.get("type").and_then(serde_json::Value::as_str) == Some("summary") {
			summary = Some(serde_json::from_value(value).expect("summary line"));
			continue;
		}
		lines.push(serde_json::from_value(value).expect("match line"));
	}
	(lines, summary)
}

#[tokio::test]
async fn grep_ast_prefers_explicit_language_over_file_extension() {
	let (tempdir, state, session) = setup_session();
	let rust_source = "fn main() {\n\tfoo(1);\n}\n";
	tokio::fs::write(tempdir.path().join("a.txt"), rust_source)
		.await
		.expect("write txt fixture");
	tokio::fs::write(tempdir.path().join("b.rs"), rust_source)
		.await
		.expect("write rust fixture");

	let addr = start_server(state).await;
	let client = reqwest::Client::new();

	let (override_lines, override_summary) = grep_ast_lines(&client, addr, session.id, &[
		("pattern", "foo($$$ARGS)"),
		("paths", "a.txt"),
		("language", "rust"),
	])
	.await;
	assert_eq!(override_lines.len(), 1);
	assert_eq!(override_lines[0].path, "a.txt");
	assert_eq!(override_lines[0].text, "foo(1)");
	assert_eq!(
		override_summary
			.as_ref()
			.map(|summary| summary.files_searched),
		Some(1)
	);

	let (inferred_txt_lines, _) =
		grep_ast_lines(&client, addr, session.id, &[("pattern", "foo($$$ARGS)"), ("paths", "a.txt")])
			.await;
	assert!(inferred_txt_lines.is_empty());

	let (inferred_rs_lines, _) =
		grep_ast_lines(&client, addr, session.id, &[("pattern", "foo($$$ARGS)"), ("paths", "b.rs")])
			.await;
	assert_eq!(inferred_rs_lines.len(), 1);
	assert_eq!(inferred_rs_lines[0].path, "b.rs");
	assert_eq!(inferred_rs_lines[0].text, "foo(1)");
}

#[tokio::test]
async fn grep_ast_reports_parse_errors_and_respects_strictness_query() {
	let (tempdir, state, session) = setup_session();
	tokio::fs::write(tempdir.path().join("broken.rs"), "fn main( {\n")
		.await
		.expect("write broken fixture");
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let (lines, summary) = grep_ast_lines(&client, addr, session.id, &[
		("pattern", "foo($$$ARGS)"),
		("paths", "broken.rs"),
		("language", "rust"),
		("strictness", "relaxed"),
	])
	.await;
	assert!(lines.is_empty());
	let summary = summary.expect("summary line");
	assert_eq!(summary.files_searched, 1);
	assert!(!summary.limit_reached);
}
