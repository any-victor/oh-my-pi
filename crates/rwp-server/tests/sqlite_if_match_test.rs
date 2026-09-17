use std::{collections::BTreeMap, path::Path, sync::Arc};

use rwp_server::{AppState, build_router, session::Session};
use serde_json::json;
use tempfile::TempDir;

struct TestServer {
	addr:       std::net::SocketAddr,
	session_id: uuid::Uuid,
	_tempdir:   TempDir,
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
	create_fixture_db(&tempdir.path().join("fixture.db"));

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

	TestServer { addr, session_id, _tempdir: tempdir }
}

fn endpoint(server: &TestServer, suffix: &str) -> String {
	format!("http://{}/sessions/{}/{}", server.addr, server.session_id, suffix)
}

fn response_etag(response: &reqwest::Response) -> String {
	response
		.headers()
		.get(reqwest::header::ETAG)
		.and_then(|value| value.to_str().ok())
		.expect("etag header")
		.to_owned()
}

#[tokio::test]
async fn write_db_requires_current_if_match_etag() {
	let server = start_server().await;
	let client = reqwest::Client::new();

	let read = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("read.db request");
	assert_eq!(read.status(), reqwest::StatusCode::OK);
	let current_etag = response_etag(&read);

	let stale = client
		.post(endpoint(&server, "write.db"))
		.header(reqwest::header::IF_MATCH, "\"deadbeef\"")
		.json(&json!({
			"path": "fixture.db",
			"op": "insert",
			"table": "widgets",
			"row": {"name": "beta"}
		}))
		.send()
		.await
		.expect("stale write.db request");
	assert_eq!(stale.status(), reqwest::StatusCode::PRECONDITION_FAILED);

	let current = client
		.post(endpoint(&server, "write.db"))
		.header(reqwest::header::IF_MATCH, current_etag)
		.json(&json!({
			"path": "fixture.db",
			"op": "insert",
			"table": "widgets",
			"row": {"name": "gamma"}
		}))
		.send()
		.await
		.expect("current write.db request");
	assert_eq!(current.status(), reqwest::StatusCode::OK);
}
