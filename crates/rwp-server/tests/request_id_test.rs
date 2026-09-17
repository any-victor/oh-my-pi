use std::net::SocketAddr;

use reqwest::{Client, StatusCode};
use rwp_server::{AppState, build_router, protocol::ErrorBody, request_id::X_REQUEST_ID_HEADER};
use uuid::Uuid;

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

fn url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

#[tokio::test]
async fn openapi_response_includes_generated_request_id() {
	let addr = start_server().await;
	let response = Client::new()
		.get(url(addr, "/openapi.json"))
		.send()
		.await
		.expect("openapi request");

	let request_id = response
		.headers()
		.get(&X_REQUEST_ID_HEADER)
		.expect("request id header present");
	assert!(!request_id.as_bytes().is_empty(), "request id header should be non-empty");
}

#[tokio::test]
async fn inbound_request_id_is_echoed_in_response() {
	let addr = start_server().await;
	let response = Client::new()
		.get(url(addr, "/openapi.json"))
		.header(&X_REQUEST_ID_HEADER, "my-request-id")
		.send()
		.await
		.expect("openapi request");

	assert_eq!(
		response
			.headers()
			.get(&X_REQUEST_ID_HEADER)
			.expect("request id header present"),
		"my-request-id",
	);
}

#[tokio::test]
async fn generated_request_ids_differ_between_requests() {
	let addr = start_server().await;
	let client = Client::new();

	let first = client
		.get(url(addr, "/openapi.json"))
		.send()
		.await
		.expect("first request");
	let second = client
		.get(url(addr, "/openapi.json"))
		.send()
		.await
		.expect("second request");

	assert_ne!(
		first
			.headers()
			.get(&X_REQUEST_ID_HEADER)
			.expect("first request id header"),
		second
			.headers()
			.get(&X_REQUEST_ID_HEADER)
			.expect("second request id header"),
	);
}

#[tokio::test]
async fn missing_session_error_still_includes_request_id() {
	let addr = start_server().await;
	let response = Client::new()
		.get(url(addr, &format!("/sessions/{}/events", Uuid::nil())))
		.send()
		.await
		.expect("events request");

	assert_eq!(response.status(), StatusCode::NOT_FOUND);
	let request_id = response
		.headers()
		.get(&X_REQUEST_ID_HEADER)
		.expect("request id header present")
		.clone();
	assert!(!request_id.as_bytes().is_empty(), "request id header should be non-empty");
	let error: ErrorBody = response.json().await.expect("error body");
	assert_eq!(error.code, "not-found");
}
