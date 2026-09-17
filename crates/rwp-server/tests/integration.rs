use std::net::SocketAddr;

use rwp_server::{AppState, build_router};

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
async fn openapi_schema_is_served() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let resp = client
		.get(url(addr, "/openapi.json"))
		.send()
		.await
		.expect("openapi request");
	assert_eq!(resp.status().as_u16(), 200);
	let doc: serde_json::Value = resp.json().await.expect("json body");
	let paths = doc.get("paths").and_then(|p| p.as_object()).expect("paths");
	for needle in [
		"/sessions",
		"/sessions/{id}/read.lines",
		"/sessions/{id}/edit.patch",
		"/sessions/{id}/bash.exec",
		"/eval/{name}",
		"/lsp/{name}",
		"/dap/{name}",
		"/cdp/{name}",
	] {
		assert!(
			paths.contains_key(needle),
			"openapi paths missing {needle}; got {:?}",
			paths.keys().collect::<Vec<_>>(),
		);
	}
}
