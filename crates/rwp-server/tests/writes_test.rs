use std::{
	collections::BTreeMap,
	io::{Read, Seek, SeekFrom},
	net::SocketAddr,
	sync::Arc,
};

use futures_util::StreamExt;
use reqwest::{Client, Method, StatusCode};
use rwp_server::{AppState, build_router, session::Session};
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpListener;
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

struct TestServer {
	addr:    SocketAddr,
	client:  Client,
	id:      Uuid,
	tempdir: TempDir,
}

impl TestServer {
	async fn start() -> Self {
		let tempdir = TempDir::new().expect("tempdir");
		let state = AppState::new();
		let session = Arc::new(Session::new(tempdir.path().to_path_buf(), BTreeMap::new()));
		let id = session.id;
		state.sessions.insert(session);
		let listener = TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind ephemeral");
		let addr = listener.local_addr().expect("local addr");
		let router = build_router(state, Vec::new());
		tokio::spawn(async move {
			let _ = axum::serve(listener, router).await;
		});
		Self { addr, client: Client::new(), id, tempdir }
	}

	fn url(&self, path: &str) -> String {
		format!("http://{}{}", self.addr, path)
	}

	async fn put_lines(&self, path: &str, body: &str, if_match: Option<&str>) -> reqwest::Response {
		let mut request = self
			.client
			.request(Method::PUT, self.url(&format!("/sessions/{}/write.lines", self.id)))
			.query(&[("path", path)])
			.body(body.to_owned());
		if let Some(value) = if_match {
			request = request.header(reqwest::header::IF_MATCH, value);
		}
		request.send().await.expect("write.lines request")
	}

	async fn put_blob(
		&self,
		path: &str,
		body: Vec<u8>,
		if_match: Option<&str>,
	) -> reqwest::Response {
		let mut request = self
			.client
			.request(Method::PUT, self.url(&format!("/sessions/{}/write.blob", self.id)))
			.query(&[("path", path)])
			.body(body);
		if let Some(value) = if_match {
			request = request.header(reqwest::header::IF_MATCH, value);
		}
		request.send().await.expect("write.blob request")
	}
}

fn etag_for(bytes: &[u8]) -> String {
	format!("\"{:016x}\"", xxh64(bytes, 0))
}

#[tokio::test]
async fn write_lines_creates_file_and_parent_dirs() {
	let server = TestServer::start().await;
	let response = server
		.put_lines("nested/path/file.txt", "hello\n", None)
		.await;
	assert_eq!(response.status(), StatusCode::NO_CONTENT);
	let etag = response
		.headers()
		.get(reqwest::header::ETAG)
		.and_then(|value| value.to_str().ok())
		.expect("etag header");
	assert_eq!(etag, etag_for(b"hello\n"));
	let bytes = tokio::fs::read(server.tempdir.path().join("nested/path/file.txt"))
		.await
		.expect("created file");
	assert_eq!(bytes, b"hello\n");
}

#[tokio::test]
async fn write_lines_updates_existing_file_with_matching_etag() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("update.txt");
	tokio::fs::write(&path, b"old\n").await.expect("seed file");
	let response = server
		.put_lines("update.txt", "new\n", Some(&etag_for(b"old\n")))
		.await;
	assert_eq!(response.status(), StatusCode::NO_CONTENT);
	let bytes = tokio::fs::read(&path).await.expect("updated file");
	assert_eq!(bytes, b"new\n");
	let etag = response
		.headers()
		.get(reqwest::header::ETAG)
		.and_then(|value| value.to_str().ok())
		.expect("etag header");
	assert_eq!(etag, etag_for(b"new\n"));
}

#[tokio::test]
async fn write_lines_rejects_wrong_etag() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("mismatch.txt");
	tokio::fs::write(&path, b"old\n").await.expect("seed file");
	let response = server
		.put_lines("mismatch.txt", "new\n", Some("\"wrong\""))
		.await;
	assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
	let body: Value = response.json().await.expect("error body");
	assert_eq!(body.get("code"), Some(&Value::String("etag-mismatch".to_owned())));
	let bytes = tokio::fs::read(&path).await.expect("unchanged file");
	assert_eq!(bytes, b"old\n");
}

#[tokio::test]
async fn write_lines_requires_if_match_for_existing_file() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("existing.txt");
	tokio::fs::write(&path, b"old\n").await.expect("seed file");
	let response = server.put_lines("existing.txt", "new\n", None).await;
	assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
	let body: Value = response.json().await.expect("error body");
	assert_eq!(body.get("code"), Some(&Value::String("etag-mismatch".to_owned())));
}

#[tokio::test]
async fn write_blob_is_atomic_for_existing_readers() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("atomic.bin");
	let old_bytes = b"old bytes".to_vec();
	let new_bytes = b"new bytes after rename".to_vec();
	tokio::fs::write(&path, &old_bytes)
		.await
		.expect("seed file");
	let mut old_handle = std::fs::File::open(&path).expect("open old handle");
	let response = server
		.put_blob("atomic.bin", new_bytes.clone(), Some(&etag_for(&old_bytes)))
		.await;
	assert_eq!(response.status(), StatusCode::NO_CONTENT);
	old_handle
		.seek(SeekFrom::Start(0))
		.expect("rewind old handle");
	let mut still_old = Vec::new();
	old_handle
		.read_to_end(&mut still_old)
		.expect("read old handle");
	assert_eq!(still_old, old_bytes);
	let current = tokio::fs::read(&path).await.expect("read new file");
	assert_eq!(current, new_bytes);
}

#[tokio::test]
async fn write_lines_preserves_utf16le_bom() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("utf16.txt");
	let original = utf16le_with_bom("alpha\n");
	tokio::fs::write(&path, &original)
		.await
		.expect("seed utf16 file");
	let response = server
		.put_lines("utf16.txt", "beta\n", Some(&etag_for(&original)))
		.await;
	assert_eq!(response.status(), StatusCode::NO_CONTENT);
	let bytes = tokio::fs::read(&path).await.expect("read utf16 file");
	assert!(bytes.starts_with(&[0xff, 0xfe]));
	assert_eq!(decode_utf16le(&bytes[2..]), "beta\n");
}

#[tokio::test]
async fn write_lines_preserves_crlf_when_input_is_consistent() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("crlf.txt");
	let original = b"alpha\r\nbeta\r\n".to_vec();
	tokio::fs::write(&path, &original)
		.await
		.expect("seed crlf file");
	let response = server
		.put_lines("crlf.txt", "gamma\ndelta\n", Some(&etag_for(&original)))
		.await;
	assert_eq!(response.status(), StatusCode::NO_CONTENT);
	let bytes = tokio::fs::read(&path).await.expect("read crlf file");
	assert_eq!(bytes, b"gamma\r\ndelta\r\n");
}

#[tokio::test]
async fn write_lines_emits_file_changed_event() {
	let server = TestServer::start().await;
	let events_response = server
		.client
		.get(server.url(&format!("/sessions/{}/events", server.id)))
		.send()
		.await
		.expect("events request");
	assert_eq!(events_response.status(), StatusCode::OK);
	let mut stream = events_response.bytes_stream();
	let write_response = server.put_lines("events.txt", "payload\n", None).await;
	assert_eq!(write_response.status(), StatusCode::NO_CONTENT);
	let expected_etag = write_response
		.headers()
		.get(reqwest::header::ETAG)
		.and_then(|value| value.to_str().ok())
		.expect("etag header")
		.trim_matches('"')
		.to_owned();
	let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
		let mut buffer = Vec::new();
		while let Some(chunk) = stream.next().await {
			let chunk = chunk.expect("event chunk");
			buffer.extend_from_slice(&chunk);
			while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
				let line = buffer.drain(..=newline).collect::<Vec<_>>();
				let text = String::from_utf8(line).expect("utf8 line");
				let trimmed = text.trim();
				if trimmed.is_empty() {
					continue;
				}
				let value: Value = serde_json::from_str(trimmed).expect("json event");
				if value.get("type") == Some(&Value::String("file-changed".to_owned())) {
					return value;
				}
			}
		}
		panic!("events stream ended before file-changed");
	})
	.await
	.expect("timed out waiting for event");
	assert_eq!(event.get("path"), Some(&Value::String("events.txt".to_owned())));
	assert_eq!(event.get("etag"), Some(&Value::String(expected_etag)));
}

fn utf16le_with_bom(text: &str) -> Vec<u8> {
	let mut bytes = vec![0xff, 0xfe];
	for unit in text.encode_utf16() {
		bytes.extend_from_slice(&unit.to_le_bytes());
	}
	bytes
}

fn decode_utf16le(bytes: &[u8]) -> String {
	let units = bytes
		.chunks_exact(2)
		.map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
		.collect::<Vec<_>>();
	String::from_utf16(&units).expect("valid utf16")
}
