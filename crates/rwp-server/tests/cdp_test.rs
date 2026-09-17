use std::{net::SocketAddr, sync::LazyLock, time::Duration};

use futures_util::{SinkExt, StreamExt};
use rwp_server::{AppState, build_router, cdp_tunnel};
use serde::Deserialize;
use serde_json::json;
use tokio::{net::TcpListener, time::sleep};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

static CDP_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
	LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Debug, Deserialize)]
struct CdpMetadata {
	name:           String,
	kind:           String,
	ws_url:         String,
	ref_count:      u32,
	last_active_ms: u64,
	args:           Option<Vec<String>>,
	headless:       Option<bool>,
}

async fn start_server() -> SocketAddr {
	let listener = TcpListener::bind("127.0.0.1:0")
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

async fn spawn_echo_server() -> String {
	let listener = TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind echo server");
	let addr = listener.local_addr().expect("echo addr");
	tokio::spawn(async move {
		loop {
			let Ok((stream, _)) = listener.accept().await else {
				break;
			};
			tokio::spawn(async move {
				let socket = accept_async(stream).await.expect("accept websocket");
				let (mut sink, mut stream) = socket.split();
				while let Some(message) = stream.next().await {
					match message.expect("valid websocket message") {
						Message::Text(text) => {
							sink
								.send(Message::Text(text))
								.await
								.expect("send text echo");
						},
						Message::Binary(bytes) => {
							sink
								.send(Message::Binary(bytes))
								.await
								.expect("send binary echo");
						},
						Message::Ping(payload) => {
							sink.send(Message::Pong(payload)).await.expect("send pong");
						},
						Message::Pong(_) | Message::Frame(_) => {},
						Message::Close(frame) => {
							let _ = sink.send(Message::Close(frame)).await;
							break;
						},
					}
				}
			});
		}
	});
	format!("ws://{addr}")
}

#[tokio::test]
async fn attach_proxy_round_trips_websocket_frames() {
	let _guard = CDP_TEST_LOCK.lock().await;
	cdp_tunnel::set_idle_timeout_for_tests(Duration::from_mins(5));
	let upstream_url = spawn_echo_server().await;
	let addr = start_server().await;
	let client = reqwest::Client::new();

	let put = client
		.put(http_url(addr, "/cdp/echo"))
		.json(&json!({"kind": "cdp-attach", "cdp_url": upstream_url}))
		.send()
		.await
		.expect("put cdp attach");
	assert_eq!(put.status(), reqwest::StatusCode::CREATED);

	let metadata = client
		.get(http_url(addr, "/cdp/echo"))
		.send()
		.await
		.expect("get cdp metadata")
		.json::<CdpMetadata>()
		.await
		.expect("decode cdp metadata");
	assert_eq!(metadata.name, "echo");
	assert_eq!(metadata.kind, "attached");
	assert_eq!(metadata.ref_count, 0);
	assert!(metadata.last_active_ms < 5_000);
	assert!(metadata.ws_url.starts_with("ws://127.0.0.1:"));

	let (mut socket, _) = connect_async(ws_url(addr, "/cdp/echo"))
		.await
		.expect("connect to cdp proxy websocket");
	socket
		.send(Message::Text("ping".into()))
		.await
		.expect("send text frame through proxy");
	let reply = socket
		.next()
		.await
		.expect("proxy should yield reply")
		.expect("proxy reply should be valid websocket frame");
	assert_eq!(reply, Message::Text("ping".into()));
	socket.close(None).await.expect("close websocket client");

	let delete = client
		.delete(http_url(addr, "/cdp/echo"))
		.send()
		.await
		.expect("delete cdp handle");
	assert_eq!(delete.status(), reqwest::StatusCode::NO_CONTENT);

	let missing = client
		.get(http_url(addr, "/cdp/echo"))
		.send()
		.await
		.expect("get deleted cdp handle");
	assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn idle_reaper_removes_unattached_handle() {
	let _guard = CDP_TEST_LOCK.lock().await;
	cdp_tunnel::set_idle_timeout_for_tests(Duration::from_millis(100));
	let upstream_url = spawn_echo_server().await;
	let addr = start_server().await;
	let client = reqwest::Client::new();

	let put = client
		.put(http_url(addr, "/cdp/reap"))
		.json(&json!({"kind": "cdp-attach", "cdp_url": upstream_url}))
		.send()
		.await
		.expect("put cdp attach");
	assert_eq!(put.status(), reqwest::StatusCode::CREATED);

	for _ in 0..20 {
		let response = client
			.get(http_url(addr, "/cdp/reap"))
			.send()
			.await
			.expect("poll cdp handle");
		if response.status() == reqwest::StatusCode::NOT_FOUND {
			return;
		}
		sleep(Duration::from_millis(25)).await;
	}

	panic!("idle reaper did not remove cdp handle");
}

#[tokio::test]
#[ignore = "requires CHROMIUM_BIN to point at a Chromium-family binary that emits a DevTools banner"]
async fn spawn_path_can_register_local_chromium() {
	let _guard = CDP_TEST_LOCK.lock().await;
	cdp_tunnel::set_idle_timeout_for_tests(Duration::from_mins(5));
	let chromium = std::env::var("CHROMIUM_BIN").expect("CHROMIUM_BIN must point at Chromium");
	let addr = start_server().await;
	let client = reqwest::Client::new();

	let put = client
		.put(http_url(addr, "/cdp/browser"))
		.json(&json!({
			"kind": "cdp-spawn",
			"path": chromium,
			"args": ["about:blank"],
			"headless": true
		}))
		.send()
		.await
		.expect("put cdp spawn");
	assert_eq!(put.status(), reqwest::StatusCode::CREATED);

	let metadata = client
		.get(http_url(addr, "/cdp/browser"))
		.send()
		.await
		.expect("get cdp metadata")
		.json::<CdpMetadata>()
		.await
		.expect("decode cdp metadata");
	assert_eq!(metadata.kind, "spawned");
	assert_eq!(metadata.args, Some(vec!["about:blank".to_owned()]));
	assert_eq!(metadata.headless, Some(true));
	assert!(metadata.ws_url.starts_with("ws://127.0.0.1:"));
	let delete = client
		.delete(http_url(addr, "/cdp/browser"))
		.send()
		.await
		.expect("delete spawned cdp handle");
	assert_eq!(delete.status(), reqwest::StatusCode::NO_CONTENT);
}
