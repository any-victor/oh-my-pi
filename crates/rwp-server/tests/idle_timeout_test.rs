use std::{
	fs,
	net::{SocketAddr, TcpListener as StdTcpListener},
	path::PathBuf,
	time::{Duration, Instant},
};

use futures_util::StreamExt;
use reqwest::StatusCode;
use rwp_server::{AppState, build_router};
use serde_json::json;
use tempfile::TempDir;
use tokio::{
	net::TcpListener,
	time::{sleep, timeout},
};
use tokio_tungstenite::accept_async;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_DELAY: Duration = Duration::from_millis(25);

async fn start_server() -> SocketAddr {
	let listener = TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral test listener");
	let addr = listener.local_addr().expect("read local addr");
	let router = build_router(AppState::new(), Vec::new());
	tokio::spawn(async move {
		let _serve_result = axum::serve(listener, router).await;
	});
	addr
}

fn http_url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

async fn wait_for_status(client: &reqwest::Client, url: &str, expected: StatusCode) {
	let deadline = Instant::now() + TEST_TIMEOUT;
	loop {
		let response = client.get(url).send().await.expect("send GET request");
		if response.status() == expected {
			return;
		}
		assert!(Instant::now() < deadline, "status did not become {expected}: {}", response.status());
		sleep(POLL_DELAY).await;
	}
}

fn write_dap_stdio_stub(dir: &TempDir) -> PathBuf {
	let script = dir.path().join("dap_stdio_stub.py");
	fs::write(
		&script,
		r#"#!/usr/bin/env python3
import json
import sys

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(write_through=True)

def read_frame():
    header = b""
    while not header.endswith(b"\r\n\r\n"):
        chunk = sys.stdin.buffer.read(1)
        if not chunk:
            return None
        header += chunk
    length = None
    for line in header.decode("ascii").split("\r\n"):
        if line.lower().startswith("content-length:"):
            length = int(line.split(":", 1)[1].strip())
            break
    if length is None:
        raise SystemExit(2)
    body = sys.stdin.buffer.read(length)
    if len(body) != length:
        return None
    return json.loads(body.decode("utf-8"))

while True:
    payload = read_frame()
    if payload is None:
        break
    if payload.get("command") == "disconnect":
        raise SystemExit(0)
"#,
	)
	.expect("write stdio stub");
	script
}

async fn spawn_cdp_echo_server() -> String {
	let listener = TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind cdp echo listener");
	let addr = listener.local_addr().expect("cdp echo addr");
	tokio::spawn(async move {
		loop {
			let Ok((stream, _peer_addr)) = listener.accept().await else {
				break;
			};
			tokio::spawn(async move {
				let socket = accept_async(stream).await.expect("accept websocket");
				let (_sink, mut stream) = socket.split();
				while let Some(message) = stream.next().await {
					if message.is_err() {
						break;
					}
				}
			});
		}
	});
	format!("ws://{addr}")
}

#[tokio::test]
async fn eval_handle_respects_idle_timeout_ms() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let url = http_url(addr, "/eval/idle-eval");

	let response = client
		.put(&url)
		.json(&json!({
			"kind": "eval",
			"lang": "python",
			"idle_timeout_ms": 200
		}))
		.send()
		.await
		.expect("create eval handle");
	assert_eq!(response.status(), StatusCode::CREATED);

	sleep(Duration::from_millis(400)).await;
	wait_for_status(&client, &url, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn dap_handle_respects_idle_timeout_ms() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let fixture_dir = TempDir::new().expect("create temp dir");
	let script = write_dap_stdio_stub(&fixture_dir);
	let url = http_url(addr, "/dap/idle-dap");

	let response = client
		.put(&url)
		.json(&json!({
			"kind": "dap",
			"command": "python3",
			"args": [script.to_string_lossy().into_owned()],
			"env": {},
			"transport": "stdio",
			"idle_timeout_ms": 200
		}))
		.send()
		.await
		.expect("create dap handle");
	assert_eq!(response.status(), StatusCode::CREATED);

	sleep(Duration::from_millis(400)).await;
	wait_for_status(&client, &url, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn cdp_handle_respects_idle_timeout_ms() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let upstream_url = spawn_cdp_echo_server().await;
	let url = http_url(addr, "/cdp/idle-cdp");

	let response = client
		.put(&url)
		.json(&json!({
			"kind": "cdp-attach",
			"cdp_url": upstream_url,
			"idle_timeout_ms": 200
		}))
		.send()
		.await
		.expect("create cdp handle");
	assert_eq!(response.status(), StatusCode::CREATED);

	sleep(Duration::from_millis(400)).await;
	wait_for_status(&client, &url, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn dap_tcp_retries_until_listener_is_available() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let reserve = StdTcpListener::bind("127.0.0.1:0").expect("reserve local port");
	let port = reserve.local_addr().expect("reserved addr").port();
	drop(reserve);

	tokio::spawn(async move {
		sleep(Duration::from_millis(100)).await;
		let listener = TcpListener::bind(("127.0.0.1", port))
			.await
			.expect("bind delayed dap listener");
		let (stream, _peer_addr) = listener.accept().await.expect("accept dap connection");
		let (_read_half, _write_half) = stream.into_split();
		sleep(Duration::from_millis(200)).await;
	});

	let url = http_url(addr, "/dap/retry-dap");
	let response = timeout(
		TEST_TIMEOUT,
		client
			.put(&url)
			.json(&json!({
				"kind": "dap",
				"command": "",
				"args": [],
				"env": {},
				"transport": "tcp",
				"host": "127.0.0.1",
				"port": port,
				"retry_ms": 50,
				"retry_attempts": 5,
				"idle_timeout_ms": 1_000
			}))
			.send(),
	)
	.await
	.expect("put dap retry request should finish")
	.expect("create tcp dap handle");
	assert_eq!(response.status(), StatusCode::CREATED);

	let deleted = client
		.delete(&url)
		.send()
		.await
		.expect("delete retried dap handle");
	assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}
