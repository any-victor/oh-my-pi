use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use rwp_server::{AppState, build_router};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::Barrier, time::timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

struct TestApp {
	addr:     SocketAddr,
	client:   Client,
	_tempdir: TempDir,
}

#[derive(Debug, Deserialize)]
struct CreateSessionResponse {
	id: Uuid,
}

#[derive(Debug, Deserialize)]
struct EvalStatus {
	name:      String,
	lang:      String,
	status:    String,
	ref_count: u32,
}

impl TestApp {
	async fn start() -> Self {
		let tempdir = TempDir::new().expect("tempdir");
		let listener = TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind ephemeral");
		let addr = listener.local_addr().expect("local addr");
		let router = build_router(AppState::new(), Vec::new());
		tokio::spawn(async move {
			let _ = axum::serve(listener, router).await;
		});
		Self { addr, client: Client::new(), _tempdir: tempdir }
	}

	fn http_url(&self, path: &str) -> String {
		format!("http://{}{}", self.addr, path)
	}

	fn ws_url(&self, path: &str) -> String {
		format!("ws://{}{}", self.addr, path)
	}

	async fn create_session(&self, cwd: &std::path::Path) -> Uuid {
		let response = self
			.client
			.post(self.http_url("/sessions"))
			.json(&json!({"cwd": cwd.to_str().expect("utf8 cwd"), "env": {}}))
			.send()
			.await
			.expect("create session");
		assert_eq!(response.status(), StatusCode::CREATED);
		response
			.json::<CreateSessionResponse>()
			.await
			.expect("create session json")
			.id
	}

	async fn write_lines(
		&self,
		session_id: Uuid,
		path: &str,
		body: &str,
		if_match: Option<&str>,
	) -> reqwest::Response {
		let mut request = self
			.client
			.put(self.http_url(&format!("/sessions/{session_id}/write.lines")))
			.query(&[("path", path)])
			.body(body.to_owned());
		if let Some(if_match) = if_match {
			request = request.header(reqwest::header::IF_MATCH, if_match);
		}
		request.send().await.expect("write.lines request")
	}

	async fn read_lines(&self, session_id: Uuid, path: &str) -> reqwest::Response {
		self
			.client
			.get(self.http_url(&format!("/sessions/{session_id}/read.lines")))
			.query(&[("path", path)])
			.send()
			.await
			.expect("read.lines request")
	}
}

#[tokio::test]
async fn edit_roundtrip() {
	let app = TestApp::start().await;
	let workspace = TempDir::new().expect("workspace");
	let session_id = app.create_session(workspace.path()).await;

	let write_response = app
		.write_lines(session_id, "note.txt", "alpha\nbeta\ngamma\n", None)
		.await;
	assert_eq!(write_response.status(), StatusCode::NO_CONTENT);
	let initial_etag = response_etag(&write_response);

	let read_response = app.read_lines(session_id, "note.txt").await;
	assert_eq!(read_response.status(), StatusCode::OK);
	assert_eq!(response_etag(&read_response), initial_etag);
	assert_eq!(read_response.text().await.expect("read body"), "alpha\nbeta\ngamma\n");

	let patch_response = app
		.client
		.post(app.http_url(&format!("/sessions/{session_id}/edit.patch")))
		.json(&json!({
			"path": "note.txt",
			"hunks": [
				{"start": 2, "deleted": 1, "inserted": ["beta-2"]}
			]
		}))
		.send()
		.await
		.expect("edit.patch request");
	assert_eq!(patch_response.status(), StatusCode::OK);
	let patch_body: Value = patch_response.json().await.expect("edit.patch json");
	assert_eq!(patch_body["op"], json!("update"));
	assert_eq!(patch_body["first_changed_line"], json!(2));
	assert!(
		patch_body["diff"]
			.as_str()
			.expect("diff")
			.contains("+beta-2")
	);

	let read_after = app.read_lines(session_id, "note.txt").await;
	assert_eq!(read_after.status(), StatusCode::OK);
	let changed_etag = response_etag(&read_after);
	assert_ne!(changed_etag, initial_etag);
	assert_eq!(read_after.text().await.expect("read updated body"), "alpha\nbeta-2\ngamma\n");
}

#[tokio::test]
async fn etag_cas_conflict() {
	let app = TestApp::start().await;
	let workspace = TempDir::new().expect("workspace");
	let session_id = app.create_session(workspace.path()).await;

	let seed = app
		.write_lines(session_id, "race.txt", "base\n", None)
		.await;
	assert_eq!(seed.status(), StatusCode::NO_CONTENT);

	let first_read = app.read_lines(session_id, "race.txt").await;
	let first_etag = response_etag(&first_read);
	assert_eq!(first_read.status(), StatusCode::OK);
	let second_read = app.read_lines(session_id, "race.txt").await;
	let second_etag = response_etag(&second_read);
	assert_eq!(second_read.status(), StatusCode::OK);
	assert_eq!(first_etag, second_etag);

	let winner = app
		.write_lines(session_id, "race.txt", "winner\n", Some(&first_etag))
		.await;
	assert_eq!(winner.status(), StatusCode::NO_CONTENT);
	let loser = app
		.write_lines(session_id, "race.txt", "loser\n", Some(&second_etag))
		.await;
	assert_eq!(loser.status(), StatusCode::PRECONDITION_FAILED);

	let final_read = app.read_lines(session_id, "race.txt").await;
	assert_eq!(final_read.status(), StatusCode::OK);
	assert_eq!(final_read.text().await.expect("final text"), "winner\n");
}

#[tokio::test]
async fn named_handle_race() {
	let app = TestApp::start().await;
	let barrier = Arc::new(Barrier::new(3));
	let url = app.http_url("/eval/main");
	let client = app.client.clone();
	let first = tokio::spawn({
		let barrier = Arc::clone(&barrier);
		let url = url.clone();
		let client = client.clone();
		async move {
			barrier.wait().await;
			client
				.put(url)
				.json(&json!({"kind": "eval", "lang": "javascript"}))
				.send()
				.await
				.expect("first put")
		}
	});
	let second = tokio::spawn({
		let barrier = Arc::clone(&barrier);
		let url = url.clone();
		let client = client.clone();
		async move {
			barrier.wait().await;
			client
				.put(url)
				.json(&json!({"kind": "eval", "lang": "javascript"}))
				.send()
				.await
				.expect("second put")
		}
	});
	barrier.wait().await;
	let (first, second) = tokio::join!(first, second);
	let statuses = [first.expect("first join").status(), second.expect("second join").status()];
	assert!(statuses.contains(&StatusCode::CREATED));
	assert!(statuses.contains(&StatusCode::OK));

	let get_response = app
		.client
		.get(app.http_url("/eval/main"))
		.send()
		.await
		.expect("get eval");
	assert_eq!(get_response.status(), StatusCode::OK);
	let status: EvalStatus = get_response.json().await.expect("eval status");
	assert_eq!(status.name, "main");
	assert_eq!(status.lang, "javascript");
	assert_eq!(status.status, "idle");
	assert_eq!(status.ref_count, 0);
}

#[tokio::test]
async fn session_events_observe_writes() {
	let app = TestApp::start().await;
	let workspace = TempDir::new().expect("workspace");
	let session_id = app.create_session(workspace.path()).await;

	let events_response = app
		.client
		.get(app.http_url(&format!("/sessions/{session_id}/events")))
		.send()
		.await
		.expect("events request");
	assert_eq!(events_response.status(), StatusCode::OK);
	let mut stream = events_response.bytes_stream();

	let write_response = app
		.write_lines(session_id, "events.txt", "payload\n", None)
		.await;
	assert_eq!(write_response.status(), StatusCode::NO_CONTENT);
	let expected_etag = response_etag(&write_response).trim_matches('"').to_owned();
	let event = next_ndjson_value(&mut stream).await;
	assert_eq!(event["type"], json!("file-changed"));
	assert_eq!(event["path"], json!("events.txt"));
	assert_eq!(event["etag"], json!(expected_etag));
}

#[tokio::test]
async fn edit_lsp_writethrough() {
	let app = TestApp::start().await;
	let workspace = TempDir::new().expect("workspace");
	let session_id = app.create_session(workspace.path()).await;
	let script_path = workspace.path().join("lsp_stub.py");
	std::fs::write(&script_path, LSP_STUB).expect("write lsp stub");

	let put_response = app
		.client
		.put(app.http_url("/lsp/rust"))
		.json(&json!({
			"kind": "lsp",
			"command": "python3",
			"args": [script_path.to_str().expect("utf8 script path")],
			"env": {},
			"root_uri": workspace.path().to_str().map(|path| format!("file://{path}")),
			"initialization_options": {"mode": "e2e"},
			"idle_timeout_ms": 10_000
		}))
		.send()
		.await
		.expect("put lsp handle");
	assert_eq!(put_response.status(), StatusCode::CREATED);

	let (mut websocket, _) = connect_async(app.ws_url("/lsp/rust"))
		.await
		.expect("connect websocket");

	let first_write = app
		.write_lines(session_id, "watched.rs", "fn first() {}\n", None)
		.await;
	assert_eq!(first_write.status(), StatusCode::NO_CONTENT);
	let first_diagnostics = next_ws_json(&mut websocket).await;
	assert_eq!(first_diagnostics["method"], json!("textDocument/publishDiagnostics"));
	assert_eq!(first_diagnostics["params"]["diagnostics"][0]["message"], json!("fn first() {}\n"));
	let first_etag = response_etag(&first_write);

	let second_write = app
		.write_lines(session_id, "watched.rs", "fn second() {}\n", Some(&first_etag))
		.await;
	assert_eq!(second_write.status(), StatusCode::NO_CONTENT);
	let second_diagnostics = next_ws_json(&mut websocket).await;
	assert_eq!(second_diagnostics["method"], json!("textDocument/publishDiagnostics"));
	assert_eq!(second_diagnostics["params"]["diagnostics"][0]["message"], json!("fn second() {}\n"));
}

fn response_etag(response: &reqwest::Response) -> String {
	response
		.headers()
		.get(reqwest::header::ETAG)
		.and_then(|value| value.to_str().ok())
		.expect("etag header")
		.to_owned()
}

async fn next_ndjson_value(
	stream: &mut (impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin),
) -> Value {
	let mut buffer = Vec::new();
	timeout(Duration::from_secs(5), async {
		loop {
			let chunk = stream
				.next()
				.await
				.expect("events stream ended")
				.expect("events chunk");
			buffer.extend_from_slice(&chunk);
			while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
				let line = String::from_utf8(buffer.drain(..=newline).collect()).expect("utf8 line");
				let trimmed = line.trim();
				if trimmed.is_empty() {
					continue;
				}
				let value: Value = serde_json::from_str(trimmed).expect("json event");
				if value["type"] != json!("heartbeat") {
					return value;
				}
			}
		}
	})
	.await
	.expect("timed out waiting for ndjson event")
}

async fn next_ws_json(
	websocket: &mut tokio_tungstenite::WebSocketStream<
		tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
	>,
) -> Value {
	timeout(Duration::from_secs(5), async {
		loop {
			let message = websocket
				.next()
				.await
				.expect("websocket closed")
				.expect("websocket frame");
			if let Message::Text(text) = message {
				return serde_json::from_str(text.as_ref()).expect("json websocket frame");
			}
		}
	})
	.await
	.expect("timed out waiting for websocket frame")
}

const LSP_STUB: &str = r"#!/usr/bin/env python3
import json
import sys


def read_frame():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line == b'\r\n':
            break
        name, value = line.decode('utf-8').split(':', 1)
        headers[name.strip().lower()] = value.strip()
    length = int(headers['content-length'])
    payload = sys.stdin.buffer.read(length)
    return json.loads(payload.decode('utf-8'))


def write_frame(message):
    body = json.dumps(message, separators=(',', ':')).encode('utf-8')
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode('utf-8'))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def publish(uri, text):
    write_frame({
        'jsonrpc': '2.0',
        'method': 'textDocument/publishDiagnostics',
        'params': {
            'uri': uri,
            'diagnostics': [
                {
                    'severity': 1,
                    'message': text,
                }
            ],
        },
    })


while True:
    message = read_frame()
    if message is None:
        break
    method = message.get('method')
    if method == 'initialize':
        write_frame({
            'jsonrpc': '2.0',
            'id': message['id'],
            'result': {
                'capabilities': {'textDocumentSync': 1},
                'serverInfo': {'name': 'stub-lsp'},
            },
        })
    elif method == 'initialized':
        continue
    elif method == 'shutdown':
        write_frame({'jsonrpc': '2.0', 'id': message['id'], 'result': None})
    elif method == 'exit':
        break
    elif method == 'textDocument/didOpen':
        text_document = message['params']['textDocument']
        publish(text_document['uri'], text_document['text'])
    elif method == 'textDocument/didChange':
        text_document = message['params']['textDocument']
        content_change = message['params']['contentChanges'][0]
        publish(text_document['uri'], content_change['text'])
";
