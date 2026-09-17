use std::{collections::BTreeMap, net::SocketAddr};

use http::header::{
	ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
	ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN,
};
use reqwest::StatusCode;
use rwp_server::{AppState, build_router, protocol::requests::CreateSessionRequest};
use tempfile::TempDir;

async fn start_server(state: AppState, cors_origins: Vec<String>) -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(state, cors_origins);
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

#[tokio::test]
async fn no_cors_config_omits_allow_origin_header() {
	let addr = start_server(AppState::new(), Vec::new()).await;
	let tempdir = TempDir::new().expect("tempdir");
	let client = reqwest::Client::new();

	let response = client
		.post(url(addr, "/sessions"))
		.header(ORIGIN, "https://example.com")
		.json(&CreateSessionRequest { cwd: Some(tempdir_path(&tempdir)), env: BTreeMap::new() })
		.send()
		.await
		.expect("create session request");

	assert_eq!(response.status(), StatusCode::CREATED);
	assert!(
		response
			.headers()
			.get(ACCESS_CONTROL_ALLOW_ORIGIN)
			.is_none(),
		"unexpected allow-origin header: {:?}",
		response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN)
	);
}

#[tokio::test]
async fn specific_origin_allows_only_matching_origin() {
	let addr = start_server(AppState::new(), vec!["https://allowed.example".to_owned()]).await;
	let client = reqwest::Client::new();

	let allowed = client
		.get(url(addr, "/openapi.json"))
		.header(ORIGIN, "https://allowed.example")
		.send()
		.await
		.expect("allowed origin request");
	assert_eq!(allowed.status(), StatusCode::OK);
	assert_eq!(
		allowed
			.headers()
			.get(ACCESS_CONTROL_ALLOW_ORIGIN)
			.expect("allow origin header for matching origin"),
		"https://allowed.example"
	);

	let denied = client
		.get(url(addr, "/openapi.json"))
		.header(ORIGIN, "https://other.example")
		.send()
		.await
		.expect("disallowed origin request");
	assert_eq!(denied.status(), StatusCode::OK);
	assert!(
		denied.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
		"unexpected allow-origin header for disallowed origin: {:?}",
		denied.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN)
	);
}

#[tokio::test]
async fn wildcard_origin_returns_wildcard_header() {
	let addr = start_server(AppState::new(), vec!["*".to_owned()]).await;
	let client = reqwest::Client::new();

	let response = client
		.get(url(addr, "/openapi.json"))
		.header(ORIGIN, "https://any.example")
		.send()
		.await
		.expect("wildcard origin request");

	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(
		response
			.headers()
			.get(ACCESS_CONTROL_ALLOW_ORIGIN)
			.expect("allow origin header for wildcard"),
		"*"
	);
}

#[tokio::test]
async fn preflight_from_allowed_origin_returns_cors_headers_even_with_auth_enabled() {
	let addr = start_server(AppState::with_auth_token(Some("secret".to_owned())), vec![
		"https://allowed.example".to_owned(),
	])
	.await;
	let client = reqwest::Client::new();

	let response = client
		.request(reqwest::Method::OPTIONS, url(addr, "/sessions"))
		.header(ORIGIN, "https://allowed.example")
		.header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
		.header(ACCESS_CONTROL_REQUEST_HEADERS, "authorization, content-type, if-match, x-request-id")
		.send()
		.await
		.expect("preflight request");

	assert!(
		matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT),
		"unexpected preflight status: {}",
		response.status()
	);
	assert_eq!(
		response
			.headers()
			.get(ACCESS_CONTROL_ALLOW_ORIGIN)
			.expect("allow origin header for preflight"),
		"https://allowed.example"
	);
	let allow_methods = response
		.headers()
		.get(ACCESS_CONTROL_ALLOW_METHODS)
		.expect("allow methods header")
		.to_str()
		.expect("allow methods ascii");
	for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
		assert!(allow_methods.contains(method), "allow methods missing {method}: {allow_methods}");
	}
	let allow_headers = response
		.headers()
		.get(ACCESS_CONTROL_ALLOW_HEADERS)
		.expect("allow headers header")
		.to_str()
		.expect("allow headers ascii")
		.to_ascii_lowercase();
	for header in ["authorization", "content-type", "if-match", "x-request-id"] {
		assert!(allow_headers.contains(header), "allow headers missing {header}: {allow_headers}");
	}
}
