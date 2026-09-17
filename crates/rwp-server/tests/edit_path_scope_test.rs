use std::{collections::BTreeMap, sync::Arc};

use rwp_server::{AppState, build_router, session::Session};
use serde_json::{Value, json};
use tempfile::TempDir;

struct TestServer {
	addr:       std::net::SocketAddr,
	session_id: uuid::Uuid,
	tempdir:    TempDir,
}

async fn start_server() -> TestServer {
	let tempdir = tempfile::tempdir().expect("tempdir");
	std::fs::write(tempdir.path().join("fixture.txt"), "before\n").expect("write fixture");

	let state = AppState::new();
	let session = Arc::new(Session::new(tempdir.path().to_path_buf(), BTreeMap::new()));
	let session_id = state.sessions.insert(session);

	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(state, Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});

	TestServer { addr, session_id, tempdir }
}

fn endpoint(server: &TestServer, suffix: &str) -> String {
	format!("http://{}/sessions/{}/{}", server.addr, server.session_id, suffix)
}

#[tokio::test]
async fn edit_replace_rejects_escaping_relative_paths() {
	let server = start_server().await;
	let client = reqwest::Client::new();

	let response = client
		.post(endpoint(&server, "edit.replace"))
		.json(&json!({
			"path": "../escape.txt",
			"old": "before",
			"new": "after"
		}))
		.send()
		.await
		.expect("edit.replace request");

	assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
	let body: Value = response.json().await.expect("error body");
	assert_eq!(body.get("code"), Some(&Value::String("bad-request".to_owned())));
}

#[tokio::test]
async fn edit_patch_rejects_absolute_paths() {
	let server = start_server().await;
	let client = reqwest::Client::new();
	let absolute_path = server.tempdir.path().join("fixture.txt");

	let response = client
		.post(endpoint(&server, "edit.patch"))
		.json(&json!({
			"path": absolute_path,
			"hunks": [{"start": 1, "deleted": 1, "inserted": ["after"]}]
		}))
		.send()
		.await
		.expect("edit.patch request");

	assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
	let body: Value = response.json().await.expect("error body");
	assert_eq!(body.get("code"), Some(&Value::String("bad-request".to_owned())));
}
