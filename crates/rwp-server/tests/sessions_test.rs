use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use futures_util::TryStreamExt;
use rwp_server::{
	AppState, build_router,
	protocol::{
		events::SessionEvent, requests::CreateSessionRequest, responses::CreateSessionResponse,
	},
	session::Session,
};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;
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

fn url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

fn tempdir_path(tempdir: &TempDir) -> String {
	tempdir.path().to_string_lossy().into_owned()
}

async fn read_event_line(response: reqwest::Response) -> SessionEvent {
	let stream = response.bytes_stream().map_err(std::io::Error::other);
	let reader = StreamReader::new(stream);
	let mut reader = BufReader::new(reader);
	let mut line = String::new();
	tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
		.await
		.expect("timed out waiting for event")
		.expect("stream read succeeds");
	serde_json::from_str(line.trim_end()).expect("valid session event JSON")
}

#[tokio::test]
async fn create_delete_and_missing_session_ops() {
	let state = AppState::new();
	let addr = start_server(state.clone()).await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");

	let response = client
		.post(url(addr, "/sessions"))
		.json(&CreateSessionRequest { cwd: Some(tempdir_path(&tempdir)), env: BTreeMap::new() })
		.send()
		.await
		.expect("create session request");
	assert_eq!(response.status(), reqwest::StatusCode::CREATED);
	let body: CreateSessionResponse = response.json().await.expect("create session response");
	assert!(state.sessions.get(body.id).is_some(), "session inserted into registry");

	let delete_response = client
		.delete(url(addr, &format!("/sessions/{}", body.id)))
		.send()
		.await
		.expect("delete session request");
	assert_eq!(delete_response.status(), reqwest::StatusCode::NO_CONTENT);
	assert!(state.sessions.get(body.id).is_none(), "session removed from registry");

	for path in [
		format!("/sessions/{}/cwd", body.id),
		format!("/sessions/{}/env", body.id),
		format!("/sessions/{}/events", body.id),
	] {
		let response = if path.ends_with("/cwd") {
			client
				.put(url(addr, &path))
				.json(&serde_json::json!({ "cwd": tempdir_path(&tempdir) }))
				.send()
				.await
				.expect("set cwd after delete")
		} else if path.ends_with("/env") {
			client
				.patch(url(addr, &path))
				.json(&serde_json::json!({ "env": { "KEY": "value" } }))
				.send()
				.await
				.expect("patch env after delete")
		} else {
			client
				.get(url(addr, &path))
				.send()
				.await
				.expect("events after delete")
		};
		assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
	}
}

#[tokio::test]
async fn rejects_bad_cwd_and_resolves_relative_cwd() {
	let state = AppState::new();
	let addr = start_server(state.clone()).await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");
	let missing = tempdir.path().join("missing");

	let create_bad = client
		.post(url(addr, "/sessions"))
		.json(&serde_json::json!({ "cwd": missing }))
		.send()
		.await
		.expect("create with missing cwd");
	assert_eq!(create_bad.status(), reqwest::StatusCode::BAD_REQUEST);

	let create_response = client
		.post(url(addr, "/sessions"))
		.json(&CreateSessionRequest { cwd: Some(tempdir_path(&tempdir)), env: BTreeMap::new() })
		.send()
		.await
		.expect("create session request");
	assert_eq!(create_response.status(), reqwest::StatusCode::CREATED);
	let body: CreateSessionResponse = create_response.json().await.expect("create body");

	let relative_dir_name = format!("rwp-sessions-{}", Uuid::new_v4());
	let relative_dir = std::env::current_dir()
		.expect("cwd")
		.join(&relative_dir_name);
	tokio::fs::create_dir(&relative_dir)
		.await
		.expect("create relative dir");

	let set_cwd_response = client
		.put(url(addr, &format!("/sessions/{}/cwd", body.id)))
		.json(&serde_json::json!({ "cwd": relative_dir_name }))
		.send()
		.await
		.expect("set cwd request");
	assert_eq!(set_cwd_response.status(), reqwest::StatusCode::NO_CONTENT);
	let session = state
		.sessions
		.get(body.id)
		.expect("session remains present");
	assert_eq!(session.cwd(), relative_dir.canonicalize().expect("canonical cwd"));

	let set_bad = client
		.put(url(addr, &format!("/sessions/{}/cwd", body.id)))
		.json(&serde_json::json!({ "cwd": missing }))
		.send()
		.await
		.expect("set missing cwd");
	assert_eq!(set_bad.status(), reqwest::StatusCode::BAD_REQUEST);

	tokio::fs::remove_dir(&relative_dir)
		.await
		.expect("remove relative dir");
}

#[tokio::test]
async fn patch_env_sets_and_unsets_values() {
	let state = AppState::new();
	let addr = start_server(state.clone()).await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");

	let create_response = client
		.post(url(addr, "/sessions"))
		.json(&CreateSessionRequest {
			cwd: Some(tempdir_path(&tempdir)),
			env: BTreeMap::from([("BASE".to_owned(), "present".to_owned())]),
		})
		.send()
		.await
		.expect("create session request");
	let body: CreateSessionResponse = create_response.json().await.expect("create body");

	let patch_response = client
		.patch(url(addr, &format!("/sessions/{}/env", body.id)))
		.json(&serde_json::json!({
			"env": {
				"ADDED": "value",
				"BASE": null
			}
		}))
		.send()
		.await
		.expect("patch env request");
	assert_eq!(patch_response.status(), reqwest::StatusCode::NO_CONTENT);

	let session = state.sessions.get(body.id).expect("session present");
	assert_eq!(session.env_snapshot(), BTreeMap::from([("ADDED".to_owned(), "value".to_owned())]),);
}

#[tokio::test]
async fn events_stream_emits_heartbeat() {
	let state = AppState::new();
	let tempdir = TempDir::new().expect("tempdir");
	let session = Arc::new(Session::with_heartbeat_interval(
		tempdir.path().to_path_buf(),
		BTreeMap::new(),
		Duration::from_millis(50),
	));
	let id = state.sessions.insert(session);
	let addr = start_server(state).await;
	let client = reqwest::Client::new();

	let response = client
		.get(url(addr, &format!("/sessions/{id}/events")))
		.send()
		.await
		.expect("events request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	assert_eq!(
		response.headers().get(reqwest::header::CONTENT_TYPE),
		Some(&reqwest::header::HeaderValue::from_static("application/x-ndjson")),
	);

	let event = read_event_line(response).await;
	assert!(matches!(event, SessionEvent::Heartbeat));
}

#[tokio::test]
async fn events_stream_survives_client_disconnect() {
	let state = AppState::new();
	let tempdir = TempDir::new().expect("tempdir");
	let session = Arc::new(Session::with_heartbeat_interval(
		tempdir.path().to_path_buf(),
		BTreeMap::new(),
		Duration::from_millis(50),
	));
	let id = state.sessions.insert(session);
	let addr = start_server(state).await;
	let client = reqwest::Client::new();

	let response = client
		.get(url(addr, &format!("/sessions/{id}/events")))
		.send()
		.await
		.expect("events request");
	drop(response);
	tokio::time::sleep(Duration::from_millis(100)).await;

	let delete_response = client
		.delete(url(addr, &format!("/sessions/{id}")))
		.send()
		.await
		.expect("delete after disconnect");
	assert_eq!(delete_response.status(), reqwest::StatusCode::NO_CONTENT);
}
