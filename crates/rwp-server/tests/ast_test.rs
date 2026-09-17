use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use rwp_server::{
	AppState, build_router,
	protocol::{
		events::SessionEvent,
		responses::{AstEditResult, ReadAstResponse},
	},
	session::Session,
};
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
	path:       String,
	line:       usize,
	column:     usize,
	end_line:   usize,
	end_column: usize,
	text:       String,
}

#[tokio::test]
async fn read_ast_returns_structural_summary_with_elisions() {
	let (tempdir, state, session) = setup_session();
	tokio::fs::create_dir_all(tempdir.path().join("src"))
		.await
		.expect("create src dir");
	tokio::fs::write(
		tempdir.path().join("src/lib.rs"),
		"pub fn alpha() {\n\tlet values = \
		 [\n\t\t1,\n\t\t2,\n\t\t3,\n\t\t4,\n\t\t5,\n\t];\n\tprintln!(\"{}\", values.len());\n}\n",
	)
	.await
	.expect("write rust fixture");

	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, &format!("/sessions/{}/read.ast", session.id)))
		.query(&[("path", "src/lib.rs")])
		.send()
		.await
		.expect("read.ast request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let body: ReadAstResponse = response.json().await.expect("read.ast response");
	assert_eq!(body.language.as_deref(), Some("rust"));
	assert!(body.parsed);
	assert!(body.elided);
	assert_eq!(body.total_lines, 10);
	assert!(body.segments.iter().any(|segment| segment.kind == "elided"));
	let kept_text = body
		.segments
		.iter()
		.filter_map(|segment| segment.text.as_deref())
		.collect::<String>();
	assert!(kept_text.contains("pub fn alpha() {"));
	assert!(!kept_text.contains("println!"));
}

#[tokio::test]
async fn read_ast_rejects_unsupported_languages() {
	let (tempdir, state, session) = setup_session();
	tokio::fs::write(tempdir.path().join("notes.txt"), "hello\n")
		.await
		.expect("write text fixture");

	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, &format!("/sessions/{}/read.ast", session.id)))
		.query(&[("path", "notes.txt")])
		.send()
		.await
		.expect("read.ast request");
	assert_eq!(response.status(), reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn grep_ast_finds_matches_in_two_files() {
	let (tempdir, state, session) = setup_session();
	tokio::fs::create_dir_all(tempdir.path().join("src/nested"))
		.await
		.expect("create src dir");
	tokio::fs::write(
		tempdir.path().join("src/nested/lib.rs"),
		"fn main() {\n\tfoo(1, 2);\n\tlet _x = 1;\n}\n",
	)
	.await
	.expect("write first rust fixture");
	tokio::fs::write(tempdir.path().join("src/other.rs"), "fn helper() {\n\tfoo(3);\n}\n")
		.await
		.expect("write second rust fixture");
	tokio::fs::write(tempdir.path().join("src/skip.txt"), "foo(4);\n")
		.await
		.expect("write skipped fixture");

	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, &format!("/sessions/{}/grep.ast", session.id)))
		.query(&[("pattern", "foo($$$ARGS)"), ("paths", "src/**/*.rs")])
		.send()
		.await
		.expect("grep.ast request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let body = response.text().await.expect("grep.ast text");
	let lines: Vec<GrepAstLine> = body
		.lines()
		.filter(|line| !line.contains("\"type\":\"summary\""))
		.map(|line| serde_json::from_str(line).expect("valid ndjson line"))
		.collect();
	assert_eq!(lines.len(), 2);
	assert_eq!(lines[0].path, "src/nested/lib.rs");
	assert_eq!(lines[0].line, 2);
	assert_eq!(lines[0].column, 2);
	assert_eq!(lines[0].text, "foo(1, 2)");
	assert_eq!(lines[0].end_line, 2);
	assert!(lines[0].end_column > lines[0].column);
	assert_eq!(lines[1].path, "src/other.rs");
	assert_eq!(lines[1].line, 2);
	assert_eq!(lines[1].text, "foo(3)");
}

#[tokio::test]
async fn edit_ast_rewrites_multiple_files_and_emits_events() {
	let (tempdir, state, session) = setup_session();
	tokio::fs::create_dir_all(tempdir.path().join("src/one"))
		.await
		.expect("create fixture dirs");
	tokio::fs::create_dir_all(tempdir.path().join("src/two"))
		.await
		.expect("create fixture dirs");
	tokio::fs::write(tempdir.path().join("src/one/a.rs"), "fn alpha() {\n\tfoo(1);\n\tfoo(2);\n}\n")
		.await
		.expect("write first rust fixture");
	tokio::fs::write(tempdir.path().join("src/two/b.rs"), "fn beta() {\n\tfoo(3);\n}\n")
		.await
		.expect("write second rust fixture");
	let mut events = session.events.subscribe();

	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let response = client
		.post(url(addr, &format!("/sessions/{}/edit.ast", session.id)))
		.json(&serde_json::json!({
			"ops": [{ "pat": "foo($$$ARGS)", "out": "bar($$$ARGS)" }],
			"paths": ["src/one/a.rs", "src/two/b.rs"]
		}))
		.send()
		.await
		.expect("edit.ast request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let result: AstEditResult = response.json().await.expect("edit.ast response");
	assert_eq!(result.changes.len(), 2);
	assert_eq!(result.changes[0].path, "src/one/a.rs");
	assert_eq!(result.changes[0].replacements, 2);
	assert!(!result.changes[0].diff.is_empty());
	assert_eq!(result.file_changes.len(), 2);
	assert_eq!(result.file_changes[0].path, "src/one/a.rs");
	assert_eq!(result.file_changes[0].replacements, 2);
	assert_eq!(result.file_changes[0].before_lines, vec![
		"fn alpha() {",
		"\tfoo(1);",
		"\tfoo(2);",
		"}"
	]);
	assert_eq!(result.file_changes[0].after_lines, vec![
		"fn alpha() {",
		"\tbar(1);",
		"\tbar(2);",
		"}"
	]);
	assert_eq!(result.file_changes[0].hunks.len(), 2);
	assert_eq!(result.file_changes[0].hunks[0].before_start, 2);
	assert_eq!(result.file_changes[0].hunks[0].before_lines, vec!["\tfoo(1);"]);
	assert_eq!(result.file_changes[0].hunks[0].after_lines, vec!["\tbar(1);"]);
	assert_eq!(result.changes[1].path, "src/two/b.rs");
	assert_eq!(result.changes[1].replacements, 1);
	assert!(!result.changes[1].diff.is_empty());
	let first = tokio::fs::read_to_string(tempdir.path().join("src/one/a.rs"))
		.await
		.expect("read first rewritten file");
	let second = tokio::fs::read_to_string(tempdir.path().join("src/two/b.rs"))
		.await
		.expect("read second rewritten file");
	assert!(first.contains("bar(1);"));
	assert!(first.contains("bar(2);"));
	assert!(second.contains("bar(3);"));

	let mut seen_paths = Vec::new();
	for _ in 0..2 {
		let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
			.await
			.expect("timed out waiting for file event")
			.expect("event stream open");
		if let SessionEvent::FileChanged { path, etag } = event {
			assert!(etag.as_deref().is_some_and(|etag| !etag.is_empty()));
			seen_paths.push(path);
		} else {
			panic!("expected FileChanged event");
		}
	}
	seen_paths.sort();
	assert_eq!(seen_paths, vec!["src/one/a.rs".to_owned(), "src/two/b.rs".to_owned()]);
}
