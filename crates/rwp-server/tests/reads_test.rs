use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use axum::{Router, routing::get};
use rwp_server::{AppState, build_router, session::Session};
use tempfile::TempDir;
use uuid::Uuid;

async fn start_rwp_server(state: AppState) -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral rwp listener");
	let addr = listener.local_addr().expect("rwp local addr");
	let router = build_router(state, Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});
	addr
}

async fn start_text_server(body: &'static str) -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral text listener");
	let addr = listener.local_addr().expect("text local addr");
	let router = Router::new().route(
		"/fixture",
		get(move || async move { ([(axum::http::header::CONTENT_TYPE, "text/plain")], body) }),
	);
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});
	addr
}

fn insert_session(state: &AppState, cwd: &std::path::Path) -> Uuid {
	let session = Arc::new(Session::new(cwd.to_path_buf(), BTreeMap::new()));
	let id = session.id;
	state.sessions.insert(session);
	id
}

fn endpoint(addr: SocketAddr, session_id: Uuid, suffix: &str) -> String {
	format!("http://{addr}/sessions/{session_id}/{suffix}")
}

async fn setup() -> (AppState, TempDir, Uuid, SocketAddr, reqwest::Client) {
	let state = AppState::new();
	let tempdir = tempfile::tempdir().expect("tempdir");
	let session_id = insert_session(&state, tempdir.path());
	let addr = start_rwp_server(state.clone()).await;
	(state, tempdir, session_id, addr, reqwest::Client::new())
}

#[tokio::test]
async fn read_lines_supports_whole_ranges_inline_selector_and_etag() {
	let (_state, tempdir, session_id, addr, client) = setup().await;
	let path = tempdir.path().join("fixture.txt");
	let contents = (1..=12)
		.map(|line| format!("line-{line}"))
		.collect::<Vec<_>>()
		.join("\n");
	tokio::fs::write(&path, format!("{contents}\n"))
		.await
		.expect("write fixture");

	let whole = client
		.get(endpoint(addr, session_id, "read.lines"))
		.query(&[("path", "fixture.txt")])
		.send()
		.await
		.expect("whole read request");
	assert_eq!(whole.status(), reqwest::StatusCode::OK);
	assert_eq!(
		whole
			.headers()
			.get("x-total-lines")
			.and_then(|value| value.to_str().ok()),
		Some("12"),
	);
	let etag = whole
		.headers()
		.get(reqwest::header::ETAG)
		.and_then(|value| value.to_str().ok())
		.expect("etag header")
		.to_owned();
	assert_eq!(whole.text().await.expect("whole read body"), format!("{contents}\n"));

	let second = client
		.get(endpoint(addr, session_id, "read.lines"))
		.query(&[("path", "fixture.txt")])
		.send()
		.await
		.expect("second read request");
	assert_eq!(
		second
			.headers()
			.get(reqwest::header::ETAG)
			.and_then(|value| value.to_str().ok()),
		Some(etag.as_str()),
	);

	let range = client
		.get(endpoint(addr, session_id, "read.lines"))
		.query(&[("path", "fixture.txt"), ("range", "5-10")])
		.send()
		.await
		.expect("range read request");
	assert_eq!(range.status(), reqwest::StatusCode::OK);
	assert_eq!(
		range.text().await.expect("range body"),
		(5..=10)
			.map(|line| format!("line-{line}"))
			.collect::<Vec<_>>()
			.join("\n")
			+ "\n",
	);

	let inline = client
		.get(endpoint(addr, session_id, "read.lines"))
		.query(&[("path", "fixture.txt:3+5")])
		.send()
		.await
		.expect("inline selector request");
	assert_eq!(inline.status(), reqwest::StatusCode::OK);
	assert_eq!(
		inline.text().await.expect("inline selector body"),
		(3..=7)
			.map(|line| format!("line-{line}"))
			.collect::<Vec<_>>()
			.join("\n")
			+ "\n",
	);
}

#[tokio::test]
async fn read_lines_returns_404_for_missing_path_and_fetches_urls() {
	let (_state, _tempdir, session_id, addr, client) = setup().await;

	let missing = client
		.get(endpoint(addr, session_id, "read.lines"))
		.query(&[("path", "missing.txt")])
		.send()
		.await
		.expect("missing file request");
	assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

	let text_addr = start_text_server("alpha\nbeta\n").await;
	let remote_url = format!("http://{text_addr}/fixture");
	let remote = client
		.get(endpoint(addr, session_id, "read.lines"))
		.query(&[("path", remote_url.as_str())])
		.send()
		.await
		.expect("remote read request");
	assert_eq!(remote.status(), reqwest::StatusCode::OK);
	assert_eq!(remote.text().await.expect("remote body"), "alpha\nbeta\n");
}

#[tokio::test]
async fn read_blob_honors_byte_ranges() {
	let (_state, tempdir, session_id, addr, client) = setup().await;
	let path = tempdir.path().join("fixture.bin");
	tokio::fs::write(&path, b"abcdef")
		.await
		.expect("write binary fixture");

	let response = client
		.get(endpoint(addr, session_id, "read.blob"))
		.query(&[("path", "fixture.bin")])
		.header(reqwest::header::RANGE, "bytes=1-3")
		.send()
		.await
		.expect("blob request");
	assert_eq!(response.status(), reqwest::StatusCode::PARTIAL_CONTENT);
	assert_eq!(
		response
			.headers()
			.get(reqwest::header::CONTENT_RANGE)
			.and_then(|value| value.to_str().ok()),
		Some("bytes 1-3/6"),
	);
	assert_eq!(response.bytes().await.expect("blob bytes").as_ref(), b"bcd");
}

#[tokio::test]
async fn read_ast_summarizes_rust_functions() {
	let (_state, tempdir, session_id, addr, client) = setup().await;
	let path = tempdir.path().join("fixture.rs");
	let source = "fn alpha(x: i32) -> i32 {\n\tlet values = \
	              [\n\t\t1,\n\t\t2,\n\t\t3,\n\t\t4,\n\t];\n\tvalues.iter().sum::<i32>() + \
	              x\n}\n\nfn beta() {\n\tprintln!(\"hi\");\n}\n";
	tokio::fs::write(&path, source)
		.await
		.expect("write rust fixture");

	let response = client
		.get(endpoint(addr, session_id, "read.ast"))
		.query(&[("path", "fixture.rs")])
		.send()
		.await
		.expect("ast request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let body: serde_json::Value = response.json().await.expect("ast json body");
	assert_eq!(body.get("language").and_then(serde_json::Value::as_str), Some("rust"));
	assert_eq!(body.get("parsed").and_then(serde_json::Value::as_bool), Some(true));
	assert_eq!(body.get("elided").and_then(serde_json::Value::as_bool), Some(true));
	let segments = body
		.get("segments")
		.and_then(serde_json::Value::as_array)
		.expect("segments array");
	assert!(
		segments
			.iter()
			.any(|s| s.get("kind").and_then(serde_json::Value::as_str) == Some("elided"))
	);
	let kept: String = segments
		.iter()
		.filter_map(|s| s.get("text").and_then(serde_json::Value::as_str))
		.collect();
	assert!(kept.contains("fn alpha"), "kept text should include alpha signature: {kept:?}");
	assert!(kept.contains("fn beta"), "kept text should include beta signature: {kept:?}");
	assert!(kept.contains("fn alpha(x: i32) -> i32"), "alpha signature must survive: {kept:?}");
}
