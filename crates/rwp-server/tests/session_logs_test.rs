use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use futures_util::TryStreamExt;
use reqwest::{Client, Method, StatusCode};
use rwp_server::{
	AppState, build_router,
	protocol::events::{LogLevel, LogRecord},
	session::Session,
};
use tempfile::TempDir;
use tokio::{
	io::{AsyncBufReadExt, BufReader},
	net::TcpListener,
};
use tokio_util::io::StreamReader;
use uuid::Uuid;

struct TestServer {
	addr:     SocketAddr,
	client:   Client,
	id:       Uuid,
	_tempdir: TempDir,
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
		Self { addr, client: Client::new(), id, _tempdir: tempdir }
	}

	fn url(&self, path: &str) -> String {
		format!("http://{}{}", self.addr, path)
	}

	async fn subscribe_logs(&self) -> reqwest::Response {
		self
			.client
			.get(self.url(&format!("/sessions/{}/logs", self.id)))
			.send()
			.await
			.expect("logs request")
	}

	async fn put_lines(&self, path: &str, body: &str) -> reqwest::Response {
		self
			.client
			.request(Method::PUT, self.url(&format!("/sessions/{}/write.lines", self.id)))
			.query(&[("path", path)])
			.body(body.to_owned())
			.send()
			.await
			.expect("write.lines request")
	}

	async fn edit_patch(&self, body: serde_json::Value) -> reqwest::Response {
		self
			.client
			.post(self.url(&format!("/sessions/{}/edit.patch", self.id)))
			.json(&body)
			.send()
			.await
			.expect("edit.patch request")
	}
}

async fn read_log_line(response: reqwest::Response) -> LogRecord {
	let stream = response.bytes_stream().map_err(std::io::Error::other);
	let reader = StreamReader::new(stream);
	let mut reader = BufReader::new(reader);
	let mut line = String::new();
	tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
		.await
		.expect("timed out waiting for log")
		.expect("stream read succeeds");
	serde_json::from_str(line.trim_end()).expect("valid log record JSON")
}

#[tokio::test]
async fn logs_stream_emits_write_record() {
	let server = TestServer::start().await;
	let logs_response = server.subscribe_logs().await;
	assert_eq!(logs_response.status(), StatusCode::OK);
	assert_eq!(
		logs_response.headers().get(reqwest::header::CONTENT_TYPE),
		Some(&reqwest::header::HeaderValue::from_static("application/x-ndjson")),
	);

	let write_response = server.put_lines("logs.txt", "payload\n").await;
	assert_eq!(write_response.status(), StatusCode::NO_CONTENT);

	let record = read_log_line(logs_response).await;
	assert!(record.ts_ms > 0);
	assert_eq!(record.level, LogLevel::Info);
	assert_eq!(record.source, "write");
	assert_eq!(record.message, "write succeeded");
	assert_eq!(
		record.fields.get("action"),
		Some(&serde_json::Value::String("write.lines".to_owned())),
	);
	assert_eq!(record.fields.get("path"), Some(&serde_json::Value::String("logs.txt".to_owned())),);
	assert_eq!(record.fields.get("status"), Some(&serde_json::json!(204)));
}

#[tokio::test]
async fn logs_stream_emits_error_record_for_bad_edit() {
	let server = TestServer::start().await;
	let logs_response = server.subscribe_logs().await;
	assert_eq!(logs_response.status(), StatusCode::OK);

	let edit_response = server
		.edit_patch(serde_json::json!({
			"path": "bad.txt",
			"hunks": [{ "start": 0, "deleted": 0, "inserted": [] }]
		}))
		.await;
	let edit_status = edit_response.status();
	assert!(matches!(edit_status.as_u16(), 400 | 404), "expected 400 or 404, got {edit_status}");

	let record = read_log_line(logs_response).await;
	assert!(record.ts_ms > 0);
	assert_eq!(record.level, LogLevel::Error);
	assert_eq!(record.source, "edit");
	assert_eq!(record.message, "edit failed");
	assert_eq!(
		record.fields.get("action"),
		Some(&serde_json::Value::String("edit.patch".to_owned())),
	);
	assert_eq!(record.fields.get("path"), Some(&serde_json::Value::String("bad.txt".to_owned())),);
	assert_eq!(record.fields.get("status"), Some(&serde_json::json!(edit_status.as_u16())));
}
