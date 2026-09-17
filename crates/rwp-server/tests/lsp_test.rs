use std::{net::SocketAddr, path::Path};

use futures_util::{SinkExt, StreamExt};
use rwp_server::{AppState, build_router};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Duration, sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn start_server() -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(AppState::new(), Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});
	addr
}

fn http_url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

fn ws_url(addr: SocketAddr, path: &str) -> String {
	format!("ws://{addr}{path}")
}

fn write_stub_script(dir: &TempDir) -> String {
	let path = dir.path().join("lsp_stub.py");
	std::fs::write(&path, LSP_STUB).expect("write stub script");
	path_to_string(&path)
}

fn path_to_string(path: &Path) -> String {
	path.to_str().expect("utf8 path").to_owned()
}

fn put_body(script_path: &str, idle_timeout_ms: Option<u64>) -> Value {
	json!({
		"kind": "lsp",
		"command": "python3",
		"args": [script_path],
		"env": {},
		"root_uri": "file:///workspace",
		"initialization_options": {"mode": "test"},
		"idle_timeout_ms": idle_timeout_ms,
	})
}

async fn register_lsp(
	client: &reqwest::Client,
	addr: SocketAddr,
	name: &str,
	script_path: &str,
	idle_timeout_ms: Option<u64>,
) {
	let resp = client
		.put(http_url(addr, &format!("/lsp/{name}")))
		.json(&put_body(script_path, idle_timeout_ms))
		.send()
		.await
		.expect("put lsp handle");
	assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
}

async fn next_json_frame(
	stream: &mut tokio_tungstenite::WebSocketStream<
		tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
	>,
) -> Value {
	loop {
		let message = timeout(Duration::from_secs(2), stream.next())
			.await
			.expect("message before timeout")
			.expect("stream item")
			.expect("websocket message");
		if let Message::Text(text) = message {
			return serde_json::from_str(text.as_ref()).expect("json frame");
		}
	}
}

#[tokio::test]
async fn put_registers_and_get_reports_initialized() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");
	let script_path = write_stub_script(&tempdir);

	register_lsp(&client, addr, "main", &script_path, None).await;

	let resp = client
		.get(http_url(addr, "/lsp/main"))
		.send()
		.await
		.expect("get lsp handle");
	assert_eq!(resp.status(), reqwest::StatusCode::OK);
	let body: Value = resp.json().await.expect("json body");
	assert_eq!(body["name"], json!("main"));
	assert_eq!(body["initialized"], json!(true));
	assert_eq!(body["capabilities"]["textDocumentSync"], json!(1));
}

#[tokio::test]
async fn websocket_round_trip_broadcasts_to_all_clients() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");
	let script_path = write_stub_script(&tempdir);

	register_lsp(&client, addr, "main", &script_path, None).await;

	let (mut ws1, _) = connect_async(ws_url(addr, "/lsp/main"))
		.await
		.expect("ws1 connect");
	let (mut ws2, _) = connect_async(ws_url(addr, "/lsp/main"))
		.await
		.expect("ws2 connect");
	let did_open = json!({
		"jsonrpc": "2.0",
		"method": "textDocument/didOpen",
		"params": {
			"textDocument": {
				"uri": "file:///workspace/test.rs",
				"languageId": "rust",
				"version": 1,
				"text": "hello from ws"
			}
		}
	});
	ws1.send(Message::Text(did_open.to_string().into()))
		.await
		.expect("send didOpen");

	let frame1 = next_json_frame(&mut ws1).await;
	let frame2 = next_json_frame(&mut ws2).await;
	for frame in [&frame1, &frame2] {
		assert_eq!(frame["method"], json!("textDocument/publishDiagnostics"));
		assert_eq!(frame["params"]["uri"], json!("file:///workspace/test.rs"));
		assert_eq!(frame["params"]["diagnostics"][0]["message"], json!("hello from ws"));
	}
	let status: Value = client
		.get(http_url(addr, "/lsp/main"))
		.send()
		.await
		.expect("refresh lsp status")
		.json()
		.await
		.expect("decode lsp status");
	assert_eq!(status["project_loaded"], json!(true));
	assert_eq!(status["open_files"], json!(["file:///workspace/test.rs"]));
	assert_eq!(
		status["diagnostics"]["file:///workspace/test.rs"][0]["message"],
		json!("hello from ws")
	);
}

#[tokio::test]
async fn delete_kills_process_and_reaper_removes_idle_handle() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");
	let script_path = write_stub_script(&tempdir);

	register_lsp(&client, addr, "delete-me", &script_path, None).await;
	let delete_resp = client
		.delete(http_url(addr, "/lsp/delete-me"))
		.send()
		.await
		.expect("delete lsp handle");
	assert_eq!(delete_resp.status(), reqwest::StatusCode::NO_CONTENT);
	let get_resp = client
		.get(http_url(addr, "/lsp/delete-me"))
		.send()
		.await
		.expect("get after delete");
	assert_eq!(get_resp.status(), reqwest::StatusCode::NOT_FOUND);

	register_lsp(&client, addr, "idle", &script_path, Some(200)).await;
	sleep(Duration::from_millis(350)).await;
	let idle_resp = client
		.get(http_url(addr, "/lsp/idle"))
		.send()
		.await
		.expect("get after idle reaper");
	assert_eq!(idle_resp.status(), reqwest::StatusCode::NOT_FOUND);
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
                'serverInfo': {'name': 'stub-lsp'}
            }
        })
    elif method == 'initialized':
        continue
    elif method == 'shutdown':
        write_frame({'jsonrpc': '2.0', 'id': message['id'], 'result': None})
    elif method == 'exit':
        break
    elif method == 'textDocument/didOpen':
        text_document = message['params']['textDocument']
        write_frame({
            'jsonrpc': '2.0',
            'method': 'textDocument/publishDiagnostics',
            'params': {
                'uri': text_document['uri'],
                'diagnostics': [
                    {
                        'severity': 1,
                        'message': text_document.get('text', '')
                    }
                ]
            }
        })
";
