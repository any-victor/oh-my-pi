use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use reqwest::StatusCode;
use rwp_server::{AppState, build_router, session::Session};
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpListener;
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

struct TestServer {
	addr:    SocketAddr,
	client:  reqwest::Client,
	id:      Uuid,
	tempdir: TempDir,
}

impl TestServer {
	async fn start() -> Self {
		let tempdir = TempDir::new().expect("tempdir");
		let cwd = tempdir.path().to_path_buf();
		Self::start_with_cwd(cwd, tempdir).await
	}

	async fn start_with_cwd(cwd: std::path::PathBuf, tempdir: TempDir) -> Self {
		let state = AppState::new();
		let session = Arc::new(Session::new(cwd, BTreeMap::new()));
		let id = session.id;
		state.sessions.insert(session);
		let listener = TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind ephemeral");
		let addr = listener.local_addr().expect("local addr");
		let router = build_router(state, Vec::new());
		tokio::spawn(async move {
			let _ = axum::serve(listener, router).await;
		});
		Self { addr, client: reqwest::Client::new(), id, tempdir }
	}

	fn endpoint(&self, suffix: &str) -> String {
		format!("http://{}/sessions/{}/{}", self.addr, self.id, suffix)
	}

	fn base_url(&self, path: &str) -> String {
		format!("http://{}{}", self.addr, path)
	}
}

fn etag_for(bytes: &[u8]) -> String {
	format!("{:016x}", xxh64(bytes, 0))
}

#[tokio::test]
async fn stat_reports_file_metadata_and_etag() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("note.txt");
	tokio::fs::write(&path, b"alpha\nbeta\n")
		.await
		.expect("write file");

	let response = server
		.client
		.get(server.endpoint("stat"))
		.query(&[("path", path.to_string_lossy().to_string())])
		.send()
		.await
		.expect("stat request");
	assert_eq!(response.status(), StatusCode::OK);
	let body: Value = response.json().await.expect("stat json");
	assert_eq!(body.get("exists"), Some(&Value::Bool(true)));
	assert_eq!(body.get("kind"), Some(&Value::String("file".to_owned())));
	assert_eq!(body.get("size"), Some(&Value::from(11_u64)));
	assert_eq!(body.get("etag"), Some(&Value::String(etag_for(b"alpha\nbeta\n"))));
	assert!(
		body
			.get("mtime_ms")
			.and_then(Value::as_i64)
			.is_some_and(|mtime| mtime > 0)
	);
}

#[tokio::test]
async fn stat_reports_directory_and_symlink_kinds() {
	let server = TestServer::start().await;
	let dir = server.tempdir.path().join("nested");
	let target = server.tempdir.path().join("target.txt");
	let link = server.tempdir.path().join("target-link.txt");
	std::fs::create_dir(&dir).expect("create dir");
	std::fs::write(&target, b"payload").expect("write target");
	std::os::unix::fs::symlink(&target, &link).expect("create symlink");

	let dir_response = server
		.client
		.get(server.endpoint("stat"))
		.query(&[("path", dir.to_string_lossy().to_string())])
		.send()
		.await
		.expect("dir stat request");
	assert_eq!(dir_response.status(), StatusCode::OK);
	let dir_body: Value = dir_response.json().await.expect("dir stat json");
	assert_eq!(dir_body.get("kind"), Some(&Value::String("dir".to_owned())));
	assert_eq!(dir_body.get("etag"), Some(&Value::Null));

	let link_response = server
		.client
		.get(server.endpoint("stat"))
		.query(&[("path", link.to_string_lossy().to_string())])
		.send()
		.await
		.expect("symlink stat request");
	assert_eq!(link_response.status(), StatusCode::OK);
	let link_body: Value = link_response.json().await.expect("symlink stat json");
	assert_eq!(link_body.get("kind"), Some(&Value::String("symlink".to_owned())));
	assert_eq!(link_body.get("etag"), Some(&Value::Null));

	let followed_response = server
		.client
		.get(server.endpoint("stat"))
		.query(&[
			("path", link.to_string_lossy().to_string()),
			("follow_symlinks", "true".to_owned()),
		])
		.send()
		.await
		.expect("follow symlink stat request");
	assert_eq!(followed_response.status(), StatusCode::OK);
	let followed_body: Value = followed_response
		.json()
		.await
		.expect("followed symlink stat json");
	assert_eq!(followed_body.get("kind"), Some(&Value::String("file".to_owned())));
	assert_eq!(followed_body.get("link_kind"), Some(&Value::String("symlink".to_owned())));
}

#[tokio::test]
async fn stat_reports_missing_and_exists_endpoint_tracks_presence() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("present.txt");
	std::fs::write(&path, b"present").expect("write present file");

	let missing = server
		.client
		.get(server.endpoint("stat"))
		.query(&[(
			"path",
			server
				.tempdir
				.path()
				.join("missing.txt")
				.to_string_lossy()
				.to_string(),
		)])
		.send()
		.await
		.expect("missing stat request");
	assert_eq!(missing.status(), StatusCode::OK);
	let missing_body: Value = missing.json().await.expect("missing stat json");
	assert_eq!(missing_body.get("exists"), Some(&Value::Bool(false)));
	assert_eq!(missing_body.get("size"), Some(&Value::from(0_u64)));
	assert_eq!(missing_body.get("mtime_ms"), Some(&Value::from(0_i64)));
	assert_eq!(missing_body.get("etag"), Some(&Value::Null));

	let exists = server
		.client
		.get(server.endpoint("exists"))
		.query(&[("path", path.to_string_lossy().to_string())])
		.send()
		.await
		.expect("exists request");
	assert_eq!(exists.status(), StatusCode::NO_CONTENT);

	let not_found = server
		.client
		.get(server.endpoint("exists"))
		.query(&[(
			"path",
			server
				.tempdir
				.path()
				.join("nope.txt")
				.to_string_lossy()
				.to_string(),
		)])
		.send()
		.await
		.expect("missing exists request");
	assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stat_rejects_paths_that_escape_the_session_cwd() {
	let root = TempDir::new().expect("root tempdir");
	let cwd = root.path().join("cwd");
	std::fs::create_dir(&cwd).expect("create cwd");
	let server = TestServer::start_with_cwd(cwd, root).await;

	let response = server
		.client
		.get(server.endpoint("stat"))
		.query(&[("path", "../outside.txt")])
		.send()
		.await
		.expect("escape stat request");
	assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn read_blob_size_only_returns_json_metadata() {
	let server = TestServer::start().await;
	let path = server.tempdir.path().join("fixture.bin");
	std::fs::write(&path, b"abcdef").expect("write fixture");

	let response = server
		.client
		.get(server.endpoint("read.blob"))
		.query(&[("path", path.to_string_lossy().to_string()), ("size_only", "1".to_owned())])
		.send()
		.await
		.expect("size_only request");
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(
		response
			.headers()
			.get(reqwest::header::CONTENT_TYPE)
			.and_then(|value| value.to_str().ok()),
		Some("application/json")
	);
	let body: Value = response.json().await.expect("size_only json");
	assert_eq!(body.get("size"), Some(&Value::from(6_u64)));
	assert_eq!(body.get("etag"), Some(&Value::String(etag_for(b"abcdef"))));
	assert_eq!(body.get("content_type").and_then(Value::as_str), Some("application/octet-stream"));
}

#[tokio::test]
async fn openapi_lists_new_filesystem_paths() {
	let server = TestServer::start().await;
	let response = server
		.client
		.get(server.base_url("/openapi.json"))
		.send()
		.await
		.expect("openapi request");
	assert_eq!(response.status(), StatusCode::OK);
	let body: Value = response.json().await.expect("openapi json");
	let paths = body
		.get("paths")
		.and_then(Value::as_object)
		.expect("openapi paths");
	for needle in [
		"/sessions/{id}/stat",
		"/sessions/{id}/exists",
		"/sessions/{id}/archive.entries",
		"/sessions/{id}/archive.read",
		"/sessions/{id}/archive.write",
	] {
		assert!(paths.contains_key(needle), "missing openapi path {needle}");
	}
}
