use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use axum::{Router, http::header::CONTENT_TYPE, routing::get};
use rwp_server::{AppState, build_router, session::Session};
use tempfile::TempDir;
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

async fn start_fixture_server(router: Router) -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind fixture listener");
	let addr = listener.local_addr().expect("fixture local addr");
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

fn endpoint(addr: SocketAddr, session_id: Uuid) -> String {
	format!("http://{addr}/sessions/{session_id}/read.lines")
}

async fn setup() -> (TempDir, Uuid, SocketAddr, reqwest::Client) {
	let state = AppState::new();
	let tempdir = tempfile::tempdir().expect("tempdir");
	let session_id = insert_session(&state, tempdir.path());
	let addr = start_rwp_server(state).await;
	(tempdir, session_id, addr, reqwest::Client::new())
}

#[tokio::test]
async fn read_lines_fetches_html_urls_in_reader_mode() {
	let (_tempdir, session_id, addr, client) = setup().await;
	let fixture_addr = start_fixture_server(Router::new().route(
		"/article",
		get(|| async move {
			(
				[(CONTENT_TYPE, "text/html; charset=utf-8")],
				"<!doctype html><html><head><title>Ignored</title></head><body><article><h1>Readable \
				 Heading</h1><p>Readable body text.</p></article></body></html>",
			)
		}),
	))
	.await;
	let remote_url = format!("http://{fixture_addr}/article");

	let response = client
		.get(endpoint(addr, session_id))
		.query(&[("path", remote_url.as_str()), ("reader", "markdown")])
		.send()
		.await
		.expect("html read request");
	let body = response.text().await.expect("html reader body");
	assert!(body.contains("Readable Heading"), "reader output missing heading: {body:?}");
	assert!(body.contains("Readable body text."), "reader output missing body: {body:?}");
	assert!(!body.contains("<html"), "reader output should not include raw html: {body:?}");
}

#[tokio::test]
async fn read_lines_passes_through_plain_text_urls() {
	let (_tempdir, session_id, addr, client) = setup().await;
	let fixture_addr = start_fixture_server(Router::new().route(
		"/plain",
		get(|| async move { ([(CONTENT_TYPE, "text/plain; charset=utf-8")], "alpha\nbeta\n") }),
	))
	.await;
	let remote_url = format!("http://{fixture_addr}/plain");

	let response = client
		.get(endpoint(addr, session_id))
		.query(&[("path", remote_url.as_str())])
		.send()
		.await
		.expect("plain read request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	assert_eq!(response.text().await.expect("plain text body"), "alpha\nbeta\n");
}
