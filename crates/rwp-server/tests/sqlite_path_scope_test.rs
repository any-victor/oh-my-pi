use std::{collections::BTreeMap, path::Path, sync::Arc};

use rwp_server::{AppState, build_router, session::Session};
use serde_json::{Value, json};
use tempfile::TempDir;

struct TestServer {
	addr:       std::net::SocketAddr,
	session_id: uuid::Uuid,
	tempdir:    TempDir,
}

fn create_fixture_db(path: &Path) {
	let connection = rusqlite::Connection::open(path).expect("open fixture db");
	connection
		.execute_batch(
			"CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL);INSERT INTO widgets \
			 (name) VALUES ('alpha');",
		)
		.expect("seed fixture db");
}

async fn start_server() -> TestServer {
	let tempdir = tempfile::tempdir().expect("tempdir");
	let workspace = tempdir.path().join("workspace");
	std::fs::create_dir(&workspace).expect("create workspace");
	create_fixture_db(&workspace.join("fixture.db"));
	create_fixture_db(&tempdir.path().join("secrets.db"));
	create_fixture_db(&tempdir.path().join("other.db"));

	let state = AppState::new();
	let session = Arc::new(Session::new(workspace, BTreeMap::new()));
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
async fn read_db_rejects_paths_outside_session_cwd() {
	let server = start_server().await;
	let client = reqwest::Client::new();
	let absolute_path = server.tempdir.path().join("other.db");

	let relative_escape = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", "../secrets.db")])
		.send()
		.await
		.expect("read.db escape request");
	assert_eq!(relative_escape.status(), reqwest::StatusCode::BAD_REQUEST);

	let absolute = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", absolute_path.to_str().expect("utf8 path"))])
		.send()
		.await
		.expect("read.db absolute request");
	assert_eq!(absolute.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn write_db_rejects_paths_outside_session_cwd() {
	let server = start_server().await;
	let client = reqwest::Client::new();
	let absolute_path = server.tempdir.path().join("other.db");

	let relative_escape = client
		.post(endpoint(&server, "write.db"))
		.json(&json!({
			"path": "../secrets.db",
			"op": "insert",
			"table": "widgets",
			"row": {"name": "beta"}
		}))
		.send()
		.await
		.expect("write.db escape request");
	assert_eq!(relative_escape.status(), reqwest::StatusCode::BAD_REQUEST);
	let relative_body: Value = relative_escape.json().await.expect("relative error body");
	assert_eq!(relative_body.get("code"), Some(&Value::String("bad-request".to_owned())));

	let absolute = client
		.post(endpoint(&server, "write.db"))
		.json(&json!({
			"path": absolute_path,
			"op": "insert",
			"table": "widgets",
			"row": {"name": "beta"}
		}))
		.send()
		.await
		.expect("write.db absolute request");
	assert_eq!(absolute.status(), reqwest::StatusCode::BAD_REQUEST);
	let absolute_body: Value = absolute.json().await.expect("absolute error body");
	assert_eq!(absolute_body.get("code"), Some(&Value::String("bad-request".to_owned())));
}
