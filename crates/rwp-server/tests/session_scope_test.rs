use std::{collections::BTreeMap, net::SocketAddr, path::Path, sync::Arc};

use rwp_server::{AppState, build_router, session::Session};
use serde_json::{Value, json};
use tempfile::TempDir;
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

fn http_url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

fn path_to_string(path: &Path) -> String {
	path.to_str().expect("utf8 path").to_owned()
}

fn write_stub_script(dir: &TempDir) -> String {
	let path = dir.path().join("lsp_scope_stub.py");
	std::fs::write(&path, LSP_STUB).expect("write stub script");
	path_to_string(&path)
}

fn lsp_body(script_path: &str, scope: Option<&str>) -> Value {
	let mut body = json!({
		"kind": "lsp",
		"command": "python3",
		"args": [script_path],
		"env": {},
		"root_uri": "file:///workspace",
		"initialization_options": {"mode": "scope-test"}
	});
	if let Some(scope) = scope {
		body["scope"] = json!(scope);
	}
	body
}

async fn put_lsp(
	client: &reqwest::Client,
	addr: SocketAddr,
	name: &str,
	script_path: &str,
	session_id: Uuid,
	scope: Option<&str>,
) -> reqwest::Response {
	client
		.put(http_url(addr, &format!("/lsp/{name}")))
		.query(&[("session", session_id.to_string())])
		.json(&lsp_body(script_path, scope))
		.send()
		.await
		.expect("put lsp handle")
}

#[tokio::test]
async fn global_scope_survives_session_delete() {
	let state = AppState::new();
	let session_a = state.sessions.insert(Arc::new(Session::new(
		std::env::current_dir().expect("current dir"),
		BTreeMap::new(),
	)));
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");
	let script_path = write_stub_script(&tempdir);

	let put = put_lsp(&client, addr, "global-main", &script_path, session_a, None).await;
	assert_eq!(put.status(), reqwest::StatusCode::CREATED);

	let delete = client
		.delete(http_url(addr, &format!("/sessions/{session_a}")))
		.send()
		.await
		.expect("delete session a");
	assert_eq!(delete.status(), reqwest::StatusCode::NO_CONTENT);

	let get = client
		.get(http_url(addr, "/lsp/global-main"))
		.send()
		.await
		.expect("get global lsp");
	assert_eq!(get.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn session_scope_is_removed_with_owning_session() {
	let state = AppState::new();
	let session_a = state.sessions.insert(Arc::new(Session::new(
		std::env::current_dir().expect("current dir"),
		BTreeMap::new(),
	)));
	let _session_b = state.sessions.insert(Arc::new(Session::new(
		std::env::current_dir().expect("current dir"),
		BTreeMap::new(),
	)));
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");
	let script_path = write_stub_script(&tempdir);

	let put = put_lsp(&client, addr, "session-main", &script_path, session_a, Some("session")).await;
	assert_eq!(put.status(), reqwest::StatusCode::CREATED);

	let delete = client
		.delete(http_url(addr, &format!("/sessions/{session_a}")))
		.send()
		.await
		.expect("delete session a");
	assert_eq!(delete.status(), reqwest::StatusCode::NO_CONTENT);

	let get = client
		.get(http_url(addr, "/lsp/session-main"))
		.send()
		.await
		.expect("get removed lsp");
	assert_eq!(get.status(), reqwest::StatusCode::NOT_FOUND);
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
                'serverInfo': {'name': 'scope-stub'}
            }
        })
    elif method == 'initialized':
        continue
    elif method == 'shutdown':
        write_frame({'jsonrpc': '2.0', 'id': message['id'], 'result': None})
    elif method == 'exit':
        break
";
