use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use futures_util::TryStreamExt;
use rwp_server::{AppState, build_router, protocol::events::SessionEvent, session::Session};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;
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

fn insert_session(state: &AppState, cwd: &std::path::Path) -> Uuid {
	let session = Arc::new(Session::new(cwd.to_path_buf(), BTreeMap::new()));
	let id = session.id;
	state.sessions.insert(session);
	id
}

fn endpoint(addr: SocketAddr, session_id: Uuid, suffix: &str) -> String {
	format!("http://{addr}/sessions/{session_id}/{suffix}")
}

async fn read_event_line(response: reqwest::Response, timeout: Duration) -> Option<SessionEvent> {
	let stream = response.bytes_stream().map_err(std::io::Error::other);
	let reader = StreamReader::new(stream);
	let mut reader = BufReader::new(reader);
	let mut line = String::new();
	match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
		Ok(Ok(0)) => None,
		Ok(Ok(_)) => Some(serde_json::from_str(line.trim_end()).expect("valid session event JSON")),
		Ok(Err(error)) => panic!("stream read failed: {error}"),
		Err(_) => None,
	}
}

async fn open_events(
	client: &reqwest::Client,
	addr: SocketAddr,
	session_id: Uuid,
) -> reqwest::Response {
	let response = client
		.get(endpoint(addr, session_id, "events"))
		.send()
		.await
		.expect("events request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	response
}

#[tokio::test]
async fn watch_endpoint_emits_file_changed_for_external_writes() {
	let state = AppState::new();
	let tempdir = tempfile::tempdir().expect("tempdir");
	let session_id = insert_session(&state, tempdir.path());
	let addr = start_rwp_server(state).await;
	let client = reqwest::Client::new();

	let watch = client
		.put(endpoint(addr, session_id, "watch"))
		.json(&serde_json::json!({ "enabled": true }))
		.send()
		.await
		.expect("enable watch request");
	assert_eq!(watch.status(), reqwest::StatusCode::NO_CONTENT);

	let events = open_events(&client, addr, session_id).await;

	tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

	tokio::fs::write(tempdir.path().join("watched.txt"), "hello\n")
		.await
		.expect("write watched file");

	let event = read_event_line(events, Duration::from_secs(1))
		.await
		.expect("expected watcher event");
	match event {
		SessionEvent::FileChanged { path, etag } => {
			assert_eq!(path, "watched.txt");
			assert_eq!(etag, None);
		},
		other => panic!("unexpected event: {other:?}"),
	}
}

#[tokio::test]
async fn watch_endpoint_stops_after_disable() {
	let state = AppState::new();
	let tempdir = tempfile::tempdir().expect("tempdir");
	let session_id = insert_session(&state, tempdir.path());
	let addr = start_rwp_server(state).await;
	let client = reqwest::Client::new();

	let enable = client
		.put(endpoint(addr, session_id, "watch"))
		.json(&serde_json::json!({ "enabled": true }))
		.send()
		.await
		.expect("enable watch request");
	assert_eq!(enable.status(), reqwest::StatusCode::NO_CONTENT);

	let disable = client
		.put(endpoint(addr, session_id, "watch"))
		.json(&serde_json::json!({ "enabled": false }))
		.send()
		.await
		.expect("disable watch request");
	assert_eq!(disable.status(), reqwest::StatusCode::NO_CONTENT);

	let events = open_events(&client, addr, session_id).await;
	tokio::fs::write(tempdir.path().join("quiet.txt"), "still quiet\n")
		.await
		.expect("write quiet file");
	assert!(
		read_event_line(events, Duration::from_millis(300))
			.await
			.is_none()
	);
}
