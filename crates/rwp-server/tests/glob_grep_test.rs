use std::{
	collections::BTreeMap, net::SocketAddr, path::Path, process::Command, sync::Arc, time::Duration,
};

use bytes::Bytes;
use futures_util::{StreamExt, pin_mut};
use rwp_server::{
	AppState, build_router,
	fs_ops::{HEARTBEAT_INTERVAL, heartbeat_stream},
	session::Session,
};
use serde::Deserialize;
use tempfile::TempDir;
use tokio::{sync::mpsc, task::yield_now, time::advance};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct GlobPathEntry {
	path:  String,
	mtime: i64,
	size:  u64,
}

#[derive(Debug, Deserialize)]
struct GlobResponse {
	paths:     Vec<GlobPathEntry>,
	truncated: bool,
}

#[derive(Debug, Deserialize)]
struct GrepRecord {
	path: String,
	line: u32,
	kind: String,
	text: String,
}
#[derive(Debug, Deserialize)]
#[allow(dead_code, reason = "fields kept for wire-compat NDJSON parsing")]
struct GrepSummaryRecord {
	#[serde(rename = "type")]
	type_:         String,
	#[serde(rename = "limitReached")]
	limit_reached: bool,
	truncated:     Option<bool>,
}

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

fn write_file(path: &Path, text: &str) {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).expect("create parent dirs");
	}
	std::fs::write(path, text).expect("write file");
}

async fn touch_after(path: &Path, text: &str) {
	write_file(path, text);
	tokio::time::sleep(Duration::from_millis(25)).await;
}

fn parse_ndjson(body: &str) -> Vec<GrepRecord> {
	body
		.lines()
		.filter_map(|line| {
			let value = serde_json::from_str::<serde_json::Value>(line).expect("valid ndjson row");
			if value.get("type") == Some(&serde_json::Value::String("summary".to_owned())) {
				return None;
			}
			Some(serde_json::from_value::<GrepRecord>(value).expect("valid grep row"))
		})
		.collect()
}
fn parse_summary(body: &str) -> GrepSummaryRecord {
	let line = body
		.lines()
		.find(|line| line.contains("\"type\":\"summary\""))
		.expect("summary record");
	serde_json::from_str(line).expect("valid grep summary")
}

#[cfg(unix)]
fn fifo(path: &Path) {
	let status = Command::new("mkfifo")
		.arg(path)
		.status()
		.expect("spawn mkfifo");
	assert!(status.success(), "mkfifo should succeed");
}

#[tokio::test]
async fn glob_sorts_filters_and_limits_results() {
	let tempdir = TempDir::new().expect("tempdir");
	write_file(&tempdir.path().join(".gitignore"), "ignored.txt\n");
	touch_after(&tempdir.path().join("alpha.rs"), "fn alpha() {}\n").await;
	touch_after(&tempdir.path().join("beta.txt"), "beta\n").await;
	touch_after(&tempdir.path().join("gamma.rs"), "fn gamma() {}\n").await;
	write_file(&tempdir.path().join("ignored.txt"), "ignore me\n");
	write_file(&tempdir.path().join(".hidden.rs"), "hidden\n");

	let (state, id) = session_state(tempdir.path());
	let addr = start_server(state).await;
	let client = reqwest::Client::new();

	let resp = client
		.get(url(addr, &format!("/sessions/{id}/glob")))
		.query(&[("patterns", "*.rs,*.txt")])
		.send()
		.await
		.expect("glob request");
	assert_eq!(resp.status().as_u16(), 200);
	let body: GlobResponse = resp.json().await.expect("glob json");
	assert!(!body.truncated);
	assert_eq!(
		body
			.paths
			.iter()
			.map(|entry| entry.path.as_str())
			.collect::<Vec<_>>(),
		vec!["gamma.rs", "beta.txt", "alpha.rs"]
	);
	assert!(
		body
			.paths
			.windows(2)
			.all(|window| window[0].mtime >= window[1].mtime)
	);
	assert!(body.paths.iter().all(|entry| entry.size > 0));

	let limited = client
		.get(url(addr, &format!("/sessions/{id}/glob")))
		.query(&[("patterns", "*.rs,*.txt"), ("limit", "2")])
		.send()
		.await
		.expect("limited glob request");
	assert_eq!(limited.status().as_u16(), 200);
	let limited_body: GlobResponse = limited.json().await.expect("limited glob json");
	assert!(limited_body.truncated);
	assert_eq!(
		limited_body
			.paths
			.iter()
			.map(|entry| entry.path.as_str())
			.collect::<Vec<_>>(),
		vec!["gamma.rs", "beta.txt"]
	);
}

#[tokio::test]
async fn grep_streams_ndjson_with_context_and_case_insensitive_matches() {
	let tempdir = TempDir::new().expect("tempdir");
	write_file(
		&tempdir.path().join("a.txt"),
		"before alpha\nneedle one\nbetween\nneedle two\nafter alpha\n",
	);
	write_file(&tempdir.path().join("b.txt"), "before beta\nNEEDLE three\nafter beta\n");

	let (state, id) = session_state(tempdir.path());
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let body = client
		.get(url(addr, &format!("/sessions/{id}/grep")))
		.query(&[("pattern", "needle"), ("paths", "a.txt,b.txt"), ("i", "true"), ("context", "1")])
		.send()
		.await
		.expect("grep request")
		.text()
		.await
		.expect("grep text");

	let records = parse_ndjson(&body);
	assert_eq!(records.len() + 1, body.lines().count());
	assert!(records.iter().any(|record| record.kind == "context"));
	assert!(
		records
			.iter()
			.filter(|record| record.kind == "match")
			.any(|record| record.path == "a.txt" && record.line == 2 && record.text == "needle one")
	);
	assert!(
		records
			.iter()
			.filter(|record| record.kind == "match")
			.any(|record| record.path == "a.txt" && record.line == 4 && record.text == "needle two")
	);
	assert!(
		records
			.iter()
			.filter(|record| record.kind == "match")
			.any(|record| record.path == "b.txt" && record.line == 2 && record.text == "NEEDLE three")
	);
	assert!(
		records
			.iter()
			.any(|record| record.path == "a.txt" && record.line == 1 && record.kind == "context")
	);
	assert!(
		records
			.iter()
			.any(|record| record.path == "b.txt" && record.line == 3 && record.kind == "context")
	);
	let summary = parse_summary(&body);
	assert_eq!(summary.type_, "summary");
	assert!(!summary.limit_reached);
}

#[tokio::test]
async fn grep_skip_omits_earlier_matches() {
	let tempdir = TempDir::new().expect("tempdir");
	write_file(&tempdir.path().join("matches.txt"), "needle first\nneedle second\nneedle third\n");

	let (state, id) = session_state(tempdir.path());
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let body = client
		.get(url(addr, &format!("/sessions/{id}/grep")))
		.query(&[
			("pattern", "needle"),
			("paths", "matches.txt"),
			("skip", "1"),
			("context", "0"),
			("max_matches", "2"),
		])
		.send()
		.await
		.expect("grep skip request")
		.text()
		.await
		.expect("grep skip text");

	let records = parse_ndjson(&body);
	assert_eq!(records.iter().map(|record| record.line).collect::<Vec<_>>(), vec![2, 3]);
	assert!(records.iter().all(|record| record.kind == "match"));
}

#[tokio::test(start_paused = true)]
async fn grep_heartbeat_stream_emits_heartbeat_while_stalled() {
	let (_tx, rx) = mpsc::channel::<Bytes>(1);
	let cancellation = tokio_util::sync::CancellationToken::new();
	let stream = heartbeat_stream(
		ReceiverStream::new(rx),
		HEARTBEAT_INTERVAL,
		cancellation,
		Bytes::from_static(b"{\"type\":\"heartbeat\"}\n"),
	);

	pin_mut!(stream);
	advance(Duration::from_secs(31)).await;
	yield_now().await;
	let chunk = stream
		.next()
		.await
		.expect("heartbeat item")
		.expect("heartbeat bytes");
	assert_eq!(chunk, Bytes::from_static(b"{\"type\":\"heartbeat\"}\n"));
}

#[cfg(unix)]
#[tokio::test]
async fn grep_stream_terminates_when_session_is_deleted() {
	let tempdir = TempDir::new().expect("tempdir");
	let pipe = tempdir.path().join("blocked.pipe");
	fifo(&pipe);
	let fifo_guard = std::fs::OpenOptions::new()
		.read(true)
		.write(true)
		.open(&pipe)
		.expect("open fifo guard");

	let (state, id) = session_state(tempdir.path());
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, &format!("/sessions/{id}/grep")))
		.query(&[("pattern", "needle"), ("paths", "blocked.pipe")])
		.send()
		.await
		.expect("grep request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);

	let request = tokio::spawn(async move { response.text().await.expect("grep response text") });
	let deleted = client
		.delete(url(addr, &format!("/sessions/{id}")))
		.send()
		.await
		.expect("delete session");
	assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
	drop(fifo_guard);
	tokio::time::timeout(Duration::from_secs(1), request)
		.await
		.expect("grep response should terminate promptly")
		.expect("grep request task panicked");
}
