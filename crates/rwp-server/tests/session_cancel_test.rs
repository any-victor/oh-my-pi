use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use rwp_server::{AppState, build_router, session::Session};
use serde_json::json;
use tokio::time::{sleep, timeout};

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

#[tokio::test]
async fn deleting_session_cancels_inflight_bash_exec() {
	let state = AppState::new();
	let session = Arc::new(Session::new(
		std::env::current_dir().expect("current dir"),
		std::env::vars().collect::<BTreeMap<_, _>>(),
	));
	let session_id = state.sessions.insert(session);
	let addr = start_server(state).await;
	let client = reqwest::Client::new();
	let exec_url = http_url(addr, &format!("/sessions/{session_id}/bash.exec"));
	let delete_url = http_url(addr, &format!("/sessions/{session_id}"));

	let request = tokio::spawn({
		let client = client.clone();
		async move {
			client
				.post(exec_url)
				.json(&json!({ "command": "sleep 30" }))
				.send()
				.await
				.expect("bash exec request")
		}
	});

	sleep(Duration::from_millis(100)).await;
	let delete_response = client
		.delete(delete_url)
		.send()
		.await
		.expect("delete session request");
	assert_eq!(delete_response.status(), reqwest::StatusCode::NO_CONTENT);

	let response = timeout(Duration::from_secs(1), request)
		.await
		.expect("bash request resolved before timeout")
		.expect("request task panicked");
	assert!(matches!(response.status().as_u16(), 499 | 503));
}
