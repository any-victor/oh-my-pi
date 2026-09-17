use std::{collections::BTreeMap, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use futures_util::TryStreamExt;
use rwp_server::{AppState, build_router, protocol::events::SessionEvent, session::Session};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;
use uuid::Uuid;

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

fn session_state(cwd: &Path) -> (AppState, Uuid) {
	let state = AppState::new();
	let session = Arc::new(Session::new(cwd.to_path_buf(), BTreeMap::new()));
	let id = state.sessions.insert(session);
	(state, id)
}

fn url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

async fn open_events(
	client: &reqwest::Client,
	addr: SocketAddr,
	session_id: Uuid,
) -> reqwest::Response {
	let response = client
		.get(url(addr, &format!("/sessions/{session_id}/events")))
		.send()
		.await
		.expect("open events stream");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	response
}

async fn read_event_line(response: reqwest::Response, timeout: Duration) -> Option<SessionEvent> {
	let stream = response.bytes_stream().map_err(std::io::Error::other);
	let reader = StreamReader::new(stream);
	let mut reader = BufReader::new(reader);
	let mut line = String::new();
	match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
		Ok(Ok(0)) => None,
		Ok(Ok(_)) => Some(serde_json::from_str(line.trim_end()).expect("valid session event JSON")),
		Ok(Err(error)) => panic!("stream read failed: {error}"),
		Err(_) => None,
	}
}

async fn expect_file_changed(response: reqwest::Response, expected_path: &str) {
	match read_event_line(response, Duration::from_secs(1)).await {
		Some(SessionEvent::FileChanged { path, etag: None }) if path == expected_path => {},
		other => panic!("unexpected event: {other:?}"),
	}
}

#[tokio::test]
async fn fs_delete_handles_success_missing_escape_and_directory() {
	let tempdir = tempfile::tempdir().expect("tempdir");
	std::fs::write(tempdir.path().join("delete-me.txt"), "delete me\n").expect("write file");
	std::fs::create_dir(tempdir.path().join("dir")).expect("create dir");

	let (state, id) = session_state(tempdir.path());
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let events = open_events(&client, addr, id).await;

	let delete_ok = client
		.delete(url(addr, &format!("/sessions/{id}/fs")))
		.query(&[("path", "delete-me.txt")])
		.send()
		.await
		.expect("delete file request");
	assert_eq!(delete_ok.status(), reqwest::StatusCode::NO_CONTENT);
	assert!(!tempdir.path().join("delete-me.txt").exists());
	expect_file_changed(events, "delete-me.txt").await;

	let delete_missing = client
		.delete(url(addr, &format!("/sessions/{id}/fs")))
		.query(&[("path", "missing.txt")])
		.send()
		.await
		.expect("delete missing request");
	assert_eq!(delete_missing.status(), reqwest::StatusCode::NOT_FOUND);

	let delete_escape = client
		.delete(url(addr, &format!("/sessions/{id}/fs")))
		.query(&[("path", "../escape.txt")])
		.send()
		.await
		.expect("delete escape request");
	assert_eq!(delete_escape.status(), reqwest::StatusCode::BAD_REQUEST);

	let delete_dir = client
		.delete(url(addr, &format!("/sessions/{id}/fs")))
		.query(&[("path", "dir")])
		.send()
		.await
		.expect("delete dir request");
	assert_eq!(delete_dir.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mkdir_handles_non_recursive_recursive_and_file_conflict() {
	let tempdir = tempfile::tempdir().expect("tempdir");
	std::fs::create_dir(tempdir.path().join("parent")).expect("create parent");
	std::fs::write(tempdir.path().join("existing-file"), "content").expect("write file");

	let (state, id) = session_state(tempdir.path());
	let addr = start_server(state).await;
	let client = reqwest::Client::new();

	let mkdir_plain = client
		.post(url(addr, &format!("/sessions/{id}/mkdir")))
		.query(&[("path", "parent/child"), ("recursive", "false")])
		.send()
		.await
		.expect("mkdir request");
	assert_eq!(mkdir_plain.status(), reqwest::StatusCode::NO_CONTENT);
	assert!(tempdir.path().join("parent/child").is_dir());

	let mkdir_again = client
		.post(url(addr, &format!("/sessions/{id}/mkdir")))
		.query(&[("path", "parent/child"), ("recursive", "false")])
		.send()
		.await
		.expect("mkdir idempotent request");
	assert_eq!(mkdir_again.status(), reqwest::StatusCode::OK);

	let mkdir_recursive = client
		.post(url(addr, &format!("/sessions/{id}/mkdir")))
		.query(&[("path", "deep/nested/tree"), ("recursive", "true")])
		.send()
		.await
		.expect("mkdir recursive request");
	assert_eq!(mkdir_recursive.status(), reqwest::StatusCode::NO_CONTENT);
	assert!(tempdir.path().join("deep/nested/tree").is_dir());

	let mkdir_conflict = client
		.post(url(addr, &format!("/sessions/{id}/mkdir")))
		.query(&[("path", "existing-file"), ("recursive", "false")])
		.send()
		.await
		.expect("mkdir conflict request");
	assert_eq!(mkdir_conflict.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn rename_handles_success_missing_source_conflict_and_overwrite() {
	let tempdir = tempfile::tempdir().expect("tempdir");
	std::fs::write(tempdir.path().join("from.txt"), "from\n").expect("write source file");
	std::fs::write(tempdir.path().join("src2.txt"), "replacement\n").expect("write second source");
	std::fs::write(tempdir.path().join("dest.txt"), "dest\n").expect("write dest file");

	let (state, id) = session_state(tempdir.path());
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let events = open_events(&client, addr, id).await;

	let rename_ok = client
		.post(url(addr, &format!("/sessions/{id}/rename")))
		.query(&[("from", "from.txt"), ("to", "moved.txt"), ("overwrite", "false")])
		.send()
		.await
		.expect("rename request");
	assert_eq!(rename_ok.status(), reqwest::StatusCode::NO_CONTENT);
	assert!(!tempdir.path().join("from.txt").exists());
	assert_eq!(
		std::fs::read_to_string(tempdir.path().join("moved.txt")).expect("read moved file"),
		"from\n"
	);
	expect_file_changed(events, "moved.txt").await;

	let rename_missing = client
		.post(url(addr, &format!("/sessions/{id}/rename")))
		.query(&[("from", "missing.txt"), ("to", "unused.txt"), ("overwrite", "false")])
		.send()
		.await
		.expect("rename missing request");
	assert_eq!(rename_missing.status(), reqwest::StatusCode::NOT_FOUND);

	let rename_conflict = client
		.post(url(addr, &format!("/sessions/{id}/rename")))
		.query(&[("from", "src2.txt"), ("to", "dest.txt"), ("overwrite", "false")])
		.send()
		.await
		.expect("rename conflict request");
	assert_eq!(rename_conflict.status(), reqwest::StatusCode::CONFLICT);
	assert!(tempdir.path().join("src2.txt").exists());
	assert_eq!(
		std::fs::read_to_string(tempdir.path().join("dest.txt")).expect("read original dest"),
		"dest\n"
	);

	let rename_overwrite = client
		.post(url(addr, &format!("/sessions/{id}/rename")))
		.query(&[("from", "src2.txt"), ("to", "dest.txt"), ("overwrite", "true")])
		.send()
		.await
		.expect("rename overwrite request");
	assert_eq!(rename_overwrite.status(), reqwest::StatusCode::NO_CONTENT);
	assert!(!tempdir.path().join("src2.txt").exists());
	assert_eq!(
		std::fs::read_to_string(tempdir.path().join("dest.txt")).expect("read overwritten dest"),
		"replacement\n"
	);
}
