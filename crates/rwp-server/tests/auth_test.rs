use std::net::SocketAddr;

use rwp_server::{AppState, ErrorBody, build_router};

async fn start_server(auth_token: Option<&str>) -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let state = AppState::with_auth_token(auth_token.map(str::to_owned));
	let router = build_router(state, Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});
	addr
}

fn url(addr: SocketAddr, path: &str) -> String {
	format!("http://{addr}{path}")
}

#[tokio::test]
async fn requests_succeed_without_authorization_when_token_is_disabled() {
	let addr = start_server(None).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, "/openapi.json"))
		.send()
		.await
		.expect("request");

	assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn missing_authorization_header_gets_unauthorized() {
	let addr = start_server(Some("secret")).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, "/openapi.json"))
		.send()
		.await
		.expect("request");

	assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
	let body: ErrorBody = response.json().await.expect("error body");
	assert_eq!(body.code, "unauthorized");
	assert_eq!(body.message, "missing or invalid bearer token");
	assert_eq!(body.detail, None);
}

#[tokio::test]
async fn wrong_bearer_token_gets_unauthorized() {
	let addr = start_server(Some("secret")).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, "/openapi.json"))
		.header(reqwest::header::AUTHORIZATION, "Bearer wrong")
		.send()
		.await
		.expect("request");

	assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn correct_bearer_token_succeeds() {
	let addr = start_server(Some("secret")).await;
	let client = reqwest::Client::new();
	let response = client
		.get(url(addr, "/openapi.json"))
		.header(reqwest::header::AUTHORIZATION, "Bearer secret")
		.send()
		.await
		.expect("request");

	assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn options_request_bypasses_auth() {
	let addr = start_server(Some("secret")).await;
	let client = reqwest::Client::new();
	let response = client
		.request(reqwest::Method::OPTIONS, url(addr, "/openapi.json"))
		.send()
		.await
		.expect("request");

	assert_ne!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}
