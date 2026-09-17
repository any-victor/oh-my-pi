use std::{
	fs,
	net::SocketAddr,
	path::{Path, PathBuf},
	time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use nix::{errno::Errno, sys::signal, unistd::Pid};
use reqwest::StatusCode;
use rwp_server::{AppState, build_router};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
	io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
	net::TcpListener,
	time::{Instant, sleep, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_DELAY: Duration = Duration::from_millis(25);

async fn start_server() -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
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

fn ws_url(addr: SocketAddr, path: &str) -> String {
	format!("ws://{addr}{path}")
}

fn write_stdio_stub(dir: &TempDir) -> PathBuf {
	let script = dir.path().join("dap_stdio_stub.py");
	fs::write(
		&script,
		r#"#!/usr/bin/env python3
import json
import os
import sys

PID_FILE = os.environ.get("DAP_PID_FILE")
if PID_FILE:
    with open(PID_FILE, "w", encoding="utf-8") as handle:
        handle.write(str(os.getpid()))

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

def write_frame(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    payload = read_frame()
    if payload is None:
        break
    if payload.get("command") == "disconnect":
        raise SystemExit(0)
    payload["echoed"] = True
    write_frame(payload)
"#,
	)
	.expect("write stdio stub");
	script
}

async fn wait_for_path(path: &Path) {
	wait_for(|| path.exists()).await;
}

async fn wait_for<F>(mut condition: F)
where
	F: FnMut() -> bool,
{
	let deadline = Instant::now() + TEST_TIMEOUT;
	loop {
		if condition() {
			return;
		}
		assert!(Instant::now() < deadline, "condition not satisfied before timeout");
		sleep(POLL_DELAY).await;
	}
}

fn read_pid(path: &Path) -> i32 {
	fs::read_to_string(path)
		.expect("read pid file")
		.trim()
		.parse()
		.expect("parse pid file")
}

fn process_exists(pid: i32) -> bool {
	match signal::kill(Pid::from_raw(pid), None) {
		Ok(()) => true,
		Err(Errno::ESRCH) => false,
		Err(error) => panic!("pid probe failed for {pid}: {error}"),
	}
}

async fn ws_round_trip(url: &str, request: Value) -> Value {
	let (mut socket, _response) = connect_async(url).await.expect("connect websocket");
	socket
		.send(Message::Text(request.to_string().into()))
		.await
		.expect("send dap request");
	let Some(message_result) = timeout(TEST_TIMEOUT, socket.next())
		.await
		.expect("wait for websocket response")
	else {
		panic!("websocket closed before dap response")
	};
	let message = message_result.expect("read websocket message");
	socket.close(None).await.expect("close websocket");
	match message {
		Message::Text(text) => serde_json::from_str(text.as_ref()).expect("parse websocket JSON"),
		other => panic!("unexpected websocket message: {other:?}"),
	}
}

#[tokio::test]
async fn dap_stdio_put_get_delete_round_trip() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let fixture_dir = TempDir::new().expect("make temp dir");
	let script = write_stdio_stub(&fixture_dir);
	let pid_file = fixture_dir.path().join("dap.pid");
	let script_arg = script.to_string_lossy().into_owned();
	let pid_file_arg = pid_file.to_string_lossy().into_owned();

	let create = client
		.put(http_url(addr, "/dap/stdio"))
		.json(&json!({
			"kind": "dap",
			"command": "python3",
			"args": [script_arg.clone()],
			"env": {
				"DAP_PID_FILE": pid_file_arg.clone(),
			},
			"transport": "stdio"
		}))
		.send()
		.await
		.expect("create stdio dap handle");
	assert_eq!(create.status(), StatusCode::CREATED);

	let idempotent = client
		.put(http_url(addr, "/dap/stdio"))
		.json(&json!({
			"kind": "dap",
			"command": "python3",
			"args": [script_arg],
			"env": {
				"DAP_PID_FILE": pid_file_arg,
			},
			"transport": "stdio"
		}))
		.send()
		.await
		.expect("repeat stdio dap put");
	assert_eq!(idempotent.status(), StatusCode::OK);

	wait_for_path(&pid_file).await;
	let response = ws_round_trip(
		&ws_url(addr, "/dap/stdio"),
		json!({
			"seq": 1,
			"type": "request",
			"command": "echo",
			"arguments": {"value": "ok"}
		}),
	)
	.await;
	assert_eq!(response["command"], "echo");
	assert_eq!(response["echoed"], true);
	assert_eq!(response["arguments"]["value"], "ok");

	let pid = read_pid(&pid_file);
	assert!(process_exists(pid), "spawned dap stub should be alive before delete");

	let deleted = client
		.delete(http_url(addr, "/dap/stdio"))
		.send()
		.await
		.expect("delete stdio dap handle");
	assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

	wait_for(|| !process_exists(pid)).await;
}

#[tokio::test]
async fn dap_tcp_put_get_delete_round_trip() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let listener = TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind dap tcp fixture listener");
	let port = listener.local_addr().expect("listener addr").port();
	let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel();

	tokio::spawn(async move {
		let (stream, _peer_addr) = listener.accept().await.expect("accept dap tcp fixture");
		let (reader, mut writer) = stream.into_split();
		let mut reader = BufReader::new(reader);
		let mut disconnect_tx = Some(disconnect_tx);
		loop {
			let Some(message) = read_frame(&mut reader).await.expect("read dap tcp frame") else {
				break;
			};
			let mut value: Value = serde_json::from_str(&message).expect("parse dap tcp frame");
			if value["command"] == "disconnect" {
				if let Some(sender) = disconnect_tx.take() {
					sender.send(value).expect("send disconnect payload");
				}
				break;
			}
			value["transport"] = Value::String("tcp".to_owned());
			write_frame(&mut writer, &serde_json::to_string(&value).expect("serialize tcp echo"))
				.await
				.expect("write dap tcp echo");
		}
	});

	let create = client
		.put(http_url(addr, "/dap/tcp"))
		.json(&json!({
			"kind": "dap",
			"command": "",
			"args": [],
			"env": {},
			"transport": "tcp",
			"host": "127.0.0.1",
			"port": port
		}))
		.send()
		.await
		.expect("create tcp dap handle");
	assert_eq!(create.status(), StatusCode::CREATED);

	let response = ws_round_trip(
		&ws_url(addr, "/dap/tcp"),
		json!({
			"seq": 7,
			"type": "request",
			"command": "launch",
			"arguments": {"program": "fixture"}
		}),
	)
	.await;
	assert_eq!(response["command"], "launch");
	assert_eq!(response["transport"], "tcp");
	assert_eq!(response["arguments"]["program"], "fixture");

	let deleted = client
		.delete(http_url(addr, "/dap/tcp"))
		.send()
		.await
		.expect("delete tcp dap handle");
	assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

	let disconnect = timeout(TEST_TIMEOUT, disconnect_rx)
		.await
		.expect("wait for tcp disconnect")
		.expect("receive disconnect payload");
	assert_eq!(disconnect["command"], "disconnect");
}

async fn read_frame<R>(reader: &mut R) -> std::io::Result<Option<String>>
where
	R: AsyncRead + Unpin,
{
	let mut header = Vec::new();
	let mut byte = [0_u8; 1];
	loop {
		match reader.read_exact(&mut byte).await {
			Ok(_read) => {
				header.push(byte[0]);
				if header.ends_with(b"\r\n\r\n") {
					break;
				}
			},
			Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof && header.is_empty() => {
				return Ok(None);
			},
			Err(error) => return Err(error),
		}
	}

	let content_length = header
		.split(|byte| *byte == b'\n')
		.find_map(|line| {
			let text = std::str::from_utf8(line).ok()?;
			let (name, value) = text.split_once(':')?;
			name
				.trim()
				.eq_ignore_ascii_case("Content-Length")
				.then_some(value.trim())
		})
		.expect("content length header")
		.parse::<usize>()
		.expect("numeric content length");
	let mut body = vec![0_u8; content_length];
	reader.read_exact(&mut body).await?;
	String::from_utf8(body)
		.map(Some)
		.map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

async fn write_frame<W>(writer: &mut W, message: &str) -> std::io::Result<()>
where
	W: AsyncWrite + Unpin,
{
	writer
		.write_all(format!("Content-Length: {}\r\n\r\n", message.len()).as_bytes())
		.await?;
	writer.write_all(message.as_bytes()).await?;
	writer.flush().await
}
