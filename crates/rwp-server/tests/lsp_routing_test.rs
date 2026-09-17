use std::{collections::BTreeMap, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use reqwest::{Client, Method, StatusCode};
use rwp_server::{AppState, build_router, session::Session};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, time::sleep};
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

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
}

fn write_stub_script(dir: &TempDir) -> String {
	let path = dir.path().join("lsp_stub.py");
	std::fs::write(&path, LSP_STUB).expect("write stub script");
	path_to_string(&path)
}

fn path_to_string(path: &Path) -> String {
	path.to_str().expect("utf8 path").to_owned()
}

fn put_body(script_path: &str, log_path: &str, sync_kind: u8) -> Value {
	json!({
		"kind": "lsp",
		"command": "python3",
		"args": [script_path],
		"env": {
			"RWP_LSP_LOG": log_path,
			"RWP_LSP_SYNC_KIND": sync_kind.to_string(),
		},
		"root_uri": "file:///workspace",
		"initialization_options": {"mode": "test"},
	})
}

async fn register_lsp(
	client: &Client,
	addr: SocketAddr,
	name: &str,
	script_path: &str,
	log_path: &str,
	sync_kind: u8,
) {
	let response = client
		.put(format!("http://{addr}/lsp/{name}"))
		.json(&put_body(script_path, log_path, sync_kind))
		.send()
		.await
		.expect("put lsp handle");
	assert_eq!(response.status(), StatusCode::CREATED);
}

fn etag_for(bytes: &[u8]) -> String {
	format!("\"{:016x}\"", xxh64(bytes, 0))
}

async fn wait_for_messages(log_path: &Path, expected: usize) -> Vec<Value> {
	for _ in 0..80 {
		let messages = read_messages(log_path).await;
		if messages.len() >= expected {
			return messages;
		}
		sleep(Duration::from_millis(25)).await;
	}
	read_messages(log_path).await
}

async fn read_messages(log_path: &Path) -> Vec<Value> {
	let Ok(body) = tokio::fs::read_to_string(log_path).await else {
		return Vec::new();
	};
	body
		.lines()
		.filter(|line| !line.is_empty())
		.map(|line| serde_json::from_str(line).expect("valid json log line"))
		.collect()
}

#[tokio::test]
async fn write_through_uses_incremental_did_change_with_utf16_positions() {
	let server = TestServer::start().await;
	let stub_dir = TempDir::new().expect("tempdir");
	let script_path = write_stub_script(&stub_dir);
	let log_path = stub_dir.path().join("incremental.log");

	register_lsp(&server.client, server.addr, "rust", &script_path, &path_to_string(&log_path), 2)
		.await;

	let original = "fn main() { let s = \"😀\"; }\n";
	let updated = "fn main() { let s = \"😀!\"; }\n";
	let create = server.put_lines("src/lib.rs", original, None).await;
	assert_eq!(create.status(), StatusCode::NO_CONTENT);
	let update = server
		.put_lines("src/lib.rs", updated, Some(&etag_for(original.as_bytes())))
		.await;
	assert_eq!(update.status(), StatusCode::NO_CONTENT);

	let messages = wait_for_messages(&log_path, 2).await;
	assert_eq!(messages.len(), 2);
	assert_eq!(messages[0]["method"], json!("textDocument/didOpen"));
	assert_eq!(messages[1]["method"], json!("textDocument/didChange"));
	let change = &messages[1]["params"]["contentChanges"][0];
	let expected_character =
		u32::try_from("fn main() { let s = \"😀".encode_utf16().count()).expect("u32 count");
	assert_eq!(change["range"]["start"]["line"], json!(0));
	assert_eq!(change["range"]["end"]["line"], json!(0));
	assert_eq!(change["range"]["start"]["character"], json!(expected_character));
	assert_eq!(change["range"]["end"]["character"], json!(expected_character));
	assert_eq!(change["text"], json!("!"));
}

#[tokio::test]
async fn write_through_routes_language_prefixed_handles_by_registration_order() {
	let server = TestServer::start().await;
	let stub_dir = TempDir::new().expect("tempdir");
	let script_path = write_stub_script(&stub_dir);
	let first_log = stub_dir.path().join("first.log");
	let second_log = stub_dir.path().join("second.log");

	register_lsp(
		&server.client,
		server.addr,
		"rust-first",
		&script_path,
		&path_to_string(&first_log),
		1,
	)
	.await;
	register_lsp(
		&server.client,
		server.addr,
		"rust-second",
		&script_path,
		&path_to_string(&second_log),
		1,
	)
	.await;

	let response = server.put_lines("src/lib.rs", "fn main() {}\n", None).await;
	assert_eq!(response.status(), StatusCode::NO_CONTENT);

	let first_messages = wait_for_messages(&first_log, 1).await;
	assert_eq!(first_messages.len(), 1);
	assert_eq!(first_messages[0]["method"], json!("textDocument/didOpen"));
	let uri = first_messages[0]["params"]["textDocument"]["uri"]
		.as_str()
		.expect("uri string");
	assert!(uri.ends_with("/src/lib.rs"));

	for _ in 0..20 {
		if !read_messages(&second_log).await.is_empty() {
			break;
		}
		sleep(Duration::from_millis(25)).await;
	}
	assert!(read_messages(&second_log).await.is_empty());
}

const LSP_STUB: &str = r"#!/usr/bin/env python3
import json
import os
import sys

LOG_PATH = os.environ['RWP_LSP_LOG']
SYNC_KIND = int(os.environ.get('RWP_LSP_SYNC_KIND', '1'))


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


def record(message):
    with open(LOG_PATH, 'a', encoding='utf-8') as fh:
        fh.write(json.dumps(message, separators=(',', ':')))
        fh.write('\n')


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
                'capabilities': {'textDocumentSync': SYNC_KIND},
                'serverInfo': {'name': 'stub-lsp'}
            }
        })
    elif method == 'initialized':
        continue
    elif method == 'shutdown':
        write_frame({'jsonrpc': '2.0', 'id': message['id'], 'result': None})
    elif method == 'exit':
        break
    else:
        record(message)
";
