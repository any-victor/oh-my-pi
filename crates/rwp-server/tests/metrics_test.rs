use std::{collections::BTreeMap, net::SocketAddr};

use rwp_server::{
	AppState, build_router,
	protocol::{requests::CreateSessionRequest, responses::CreateSessionResponse},
};
use tempfile::TempDir;

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

fn tempdir_path(tempdir: &TempDir) -> String {
	tempdir.path().to_string_lossy().into_owned()
}

fn metric_value(metrics: &str, name: &str, required_labels: &[(&str, &str)]) -> Option<f64> {
	metrics.lines().find_map(|line| {
		if !line.starts_with(name) {
			return None;
		}
		for (key, value) in required_labels {
			let label = format!("{key}=\"{value}\"");
			if !line.contains(&label) {
				return None;
			}
		}
		line.split_whitespace().last()?.parse::<f64>().ok()
	})
}

#[tokio::test]
async fn metrics_endpoint_is_unauthenticated_and_counts_requests() {
	let addr = start_server(Some("secret")).await;
	let client = reqwest::Client::new();

	let protected = client
		.get(url(addr, "/openapi.json"))
		.header(reqwest::header::AUTHORIZATION, "Bearer secret")
		.send()
		.await
		.expect("openapi request");
	assert_eq!(protected.status(), reqwest::StatusCode::OK);

	let metrics = client
		.get(url(addr, "/metrics"))
		.send()
		.await
		.expect("metrics request");
	assert_eq!(metrics.status(), reqwest::StatusCode::OK);
	let body = metrics.text().await.expect("metrics body");

	let value = metric_value(&body, "rwp_requests_total", &[
		("path", "/openapi.json"),
		("method", "GET"),
		("status", "200"),
	])
	.expect("openapi request counter line");
	assert!(value >= 1.0, "expected request counter >= 1, got {value}: {body}");
}

#[tokio::test]
async fn metrics_report_live_sessions() {
	let addr = start_server(None).await;
	let client = reqwest::Client::new();
	let tempdir = TempDir::new().expect("tempdir");

	let response = client
		.post(url(addr, "/sessions"))
		.json(&CreateSessionRequest { cwd: Some(tempdir_path(&tempdir)), env: BTreeMap::new() })
		.send()
		.await
		.expect("create session request");
	assert_eq!(response.status(), reqwest::StatusCode::CREATED);
	let _: CreateSessionResponse = response.json().await.expect("create session response");

	let metrics = client
		.get(url(addr, "/metrics"))
		.send()
		.await
		.expect("metrics request");
	assert_eq!(metrics.status(), reqwest::StatusCode::OK);
	let body = metrics.text().await.expect("metrics body");

	let value = metric_value(&body, "rwp_sessions_live", &[]).expect("sessions gauge line");
	assert!(value >= 1.0, "expected sessions gauge >= 1, got {value}: {body}");
}
