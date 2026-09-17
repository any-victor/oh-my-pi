use std::{collections::BTreeMap, path::Path, sync::atomic::Ordering, time::Duration};

use futures_util::TryStreamExt;
use rwp_server::{AppState, build_router, protocol::events::SessionEvent, session::Session};
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;
struct TestServer {
	addr:       std::net::SocketAddr,
	session_id: uuid::Uuid,
	state:      AppState,
	_tempdir:   TempDir,
}

async fn start_server() -> TestServer {
	let tempdir = tempfile::tempdir().expect("tempdir");
	let db_path = tempdir.path().join("fixture.db");
	create_fixture_db(&db_path);

	let state = AppState::new();
	let session = std::sync::Arc::new(Session::new(tempdir.path().to_path_buf(), BTreeMap::new()));
	let session_id = state.sessions.insert(session);

	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(state.clone(), Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});

	TestServer { addr, session_id, state, _tempdir: tempdir }
}

fn create_fixture_db(path: &Path) {
	let connection = rusqlite::Connection::open(path).expect("open fixture db");
	connection
		.execute_batch(
			"CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL, qty INTEGER NOT \
			 NULL);INSERT INTO widgets (name, qty) VALUES ('alpha', 2), ('beta', 5);",
		)
		.expect("seed fixture db");
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

async fn read_event_line(response: reqwest::Response) -> SessionEvent {
	let stream = response.bytes_stream().map_err(std::io::Error::other);
	let reader = StreamReader::new(stream);
	let mut reader = BufReader::new(reader);
	let mut line = String::new();
	tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
		.await
		.expect("timed out waiting for event")
		.expect("stream read succeeds");
	serde_json::from_str(line.trim_end()).expect("valid session event JSON")
}

#[tokio::test]
async fn repeated_sqlite_reads_reuse_cached_content_hash() {
	let server = start_server().await;
	let client = reqwest::Client::new();

	let first = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("first read");
	assert_eq!(first.status(), reqwest::StatusCode::OK);
	let first_etag = response_etag(&first);
	assert_eq!(first_etag, first_etag.to_ascii_lowercase());
	assert!(first.text().await.expect("first body").contains("widgets"));

	let second = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("second read");
	assert_eq!(second.status(), reqwest::StatusCode::OK);
	let second_etag = response_etag(&second);
	assert_eq!(second_etag, first_etag);
	assert_eq!(
		server
			.state
			.etag_cache
			.hashes_computed
			.load(Ordering::Relaxed),
		1,
		"content hash should be computed once for unchanged sqlite reads"
	);
}

#[tokio::test]
async fn sqlite_etag_changes_after_write_between_reads() {
	let server = start_server().await;
	let client = reqwest::Client::new();

	let first = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("first read");
	assert_eq!(first.status(), reqwest::StatusCode::OK);
	let first_etag = response_etag(&first);

	let write = client
		.post(endpoint(&server, "write.db"))
		.json(&json!({
			"path": "fixture.db",
			"op": "insert",
			"table": "widgets",
			"row": {"name": "gamma", "qty": 9}
		}))
		.send()
		.await
		.expect("write db");
	assert_eq!(write.status(), reqwest::StatusCode::OK);

	let second = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("second read");
	assert_eq!(second.status(), reqwest::StatusCode::OK);
	let second_etag = response_etag(&second);
	assert_ne!(second_etag, first_etag);
}

#[tokio::test]
async fn sqlite_write_event_etag_matches_followup_read_header() {
	let server = start_server().await;
	let client = reqwest::Client::new();

	let events = client
		.get(endpoint(&server, "events"))
		.send()
		.await
		.expect("open events stream");
	assert_eq!(events.status(), reqwest::StatusCode::OK);

	let write = client
		.post(endpoint(&server, "write.db"))
		.json(&json!({
			"path": "fixture.db",
			"op": "update",
			"table": "widgets",
			"key": "1",
			"row": {"qty": 7}
		}))
		.send()
		.await
		.expect("write db");
	assert_eq!(write.status(), reqwest::StatusCode::OK);

	let event = read_event_line(events).await;
	let event_etag = match event {
		SessionEvent::FileChanged { path, etag } => {
			assert_eq!(path, "fixture.db");
			etag.expect("write_db should emit an etag")
		},
		other => panic!("unexpected event: {other:?}"),
	};

	let read = client
		.get(endpoint(&server, "read.db"))
		.query(&[("path", "fixture.db")])
		.send()
		.await
		.expect("read db");
	assert_eq!(read.status(), reqwest::StatusCode::OK);
	assert_eq!(format!("\"{event_etag}\""), response_etag(&read));
}
