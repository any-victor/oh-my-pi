use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use rwp_server::{AppState, build_router, protocol::responses::AstEditResult, session::Session};
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

#[tokio::test]
async fn edit_ast_dry_run_returns_diff_without_writing_file() {
	let (tempdir, state, session) = setup_session();
	tokio::fs::create_dir_all(tempdir.path().join("src"))
		.await
		.expect("create src dir");
	let fixture_path = tempdir.path().join("src/noext");
	let original = "fn alpha() {\n\tfoo(1);\n}\n";
	tokio::fs::write(&fixture_path, original)
		.await
		.expect("write fixture");

	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let response = client
		.post(url(addr, &format!("/sessions/{}/edit.ast", session.id)))
		.json(&serde_json::json!({
			"ops": [{ "pat": "foo($$$ARGS)", "out": "bar($$$ARGS)" }],
			"paths": ["src/noext"],
			"dry_run": true,
			"language": "rust"
		}))
		.send()
		.await
		.expect("edit.ast request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let result: AstEditResult = response.json().await.expect("edit.ast response");
	assert_eq!(result.changes.len(), 1);
	assert_eq!(result.changes[0].path, "src/noext");
	assert_eq!(result.changes[0].replacements, 1);
	assert!(result.changes[0].diff.contains("bar(1)"));
	assert_eq!(result.file_changes.len(), 1);
	assert_eq!(result.file_changes[0].path, "src/noext");
	assert_eq!(result.file_changes[0].replacements, 1);
	assert_eq!(result.file_changes[0].before_lines, vec!["fn alpha() {", "\tfoo(1);", "}"]);
	assert_eq!(result.file_changes[0].after_lines, vec!["fn alpha() {", "\tbar(1);", "}"]);
	assert_eq!(result.file_changes[0].hunks.len(), 1);
	assert_eq!(result.file_changes[0].hunks[0].before_start, 2);
	assert_eq!(result.file_changes[0].hunks[0].before_lines, vec!["\tfoo(1);"]);
	assert_eq!(result.file_changes[0].hunks[0].after_lines, vec!["\tbar(1);"]);
	assert_eq!(result.files_searched, 1);
	assert!(!result.limit_reached);
	assert!(result.parse_errors.is_empty());
	assert!(!result.written);
	assert!(!result.truncated);
	assert!(!result.exceeded_limit);

	let after = tokio::fs::read_to_string(&fixture_path)
		.await
		.expect("read unchanged file");
	assert_eq!(after, original);
}
