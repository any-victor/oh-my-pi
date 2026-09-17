use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use reqwest::StatusCode;
use rwp_server::{AppState, build_router, session::Session};
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpListener;
use uuid::Uuid;

struct TestServer {
	addr:    SocketAddr,
	client:  reqwest::Client,
	id:      Uuid,
	tempdir: TempDir,
}

impl TestServer {
	async fn start() -> Self {
		let tempdir = TempDir::new().expect("tempdir");
		let state = AppState::new();
		let session = Arc::new(Session::new(tempdir.path().to_path_buf(), BTreeMap::new()));
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

	fn session_endpoint(&self, suffix: &str) -> String {
		format!("http://{}/sessions/{}/{}", self.addr, self.id, suffix)
	}

	fn archive_endpoint(&self, suffix: &str) -> String {
		format!("http://{}/archive/{}", self.addr, suffix)
	}
}

fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
	let file = std::fs::File::create(path).expect("create zip");
	let mut writer = zip::ZipWriter::new(file);
	for (name, body) in entries {
		writer
			.start_file(*name, zip::write::SimpleFileOptions::default())
			.expect("start zip file");
		std::io::Write::write_all(&mut writer, body).expect("write zip file");
	}
	writer.finish().expect("finish zip");
}

#[derive(Debug, Deserialize)]
struct ArchiveOpenResponse {
	snapshot_id: Uuid,
	format:      String,
}

#[tokio::test]
async fn archive_snapshot_survives_source_removal_and_closes() {
	let server = TestServer::start().await;
	let archive = server.tempdir.path().join("fixture.zip");
	write_zip(&archive, &[("root.txt", b"root"), ("nested/readme.txt", b"nested")]);

	let open = server
		.client
		.post(server.session_endpoint("archive.open"))
		.query(&[("path", "fixture.zip")])
		.send()
		.await
		.expect("archive.open request");
	assert_eq!(open.status(), StatusCode::OK);
	let open_body: ArchiveOpenResponse = open.json().await.expect("archive.open json");
	assert_eq!(open_body.format, "zip");

	std::fs::remove_file(&archive).expect("remove archive after snapshot open");

	let entries = server
		.client
		.get(server.archive_endpoint(&format!("{}/entries", open_body.snapshot_id)))
		.send()
		.await
		.expect("snapshot entries request");
	assert_eq!(entries.status(), StatusCode::OK);
	let entries_body: Value = entries.json().await.expect("snapshot entries json");
	assert_eq!(entries_body.get("format"), Some(&Value::String("zip".to_owned())));
	assert_eq!(
		entries_body
			.get("entries")
			.and_then(Value::as_array)
			.expect("entries array")
			.len(),
		2
	);

	for _ in 0..2 {
		let read = server
			.client
			.get(server.archive_endpoint(&format!("{}/entry", open_body.snapshot_id)))
			.query(&[("path", "root.txt")])
			.send()
			.await
			.expect("snapshot entry request");
		assert_eq!(read.status(), StatusCode::OK);
		assert_eq!(read.bytes().await.expect("snapshot bytes").as_ref(), b"root");
	}

	let close = server
		.client
		.delete(server.archive_endpoint(&open_body.snapshot_id.to_string()))
		.send()
		.await
		.expect("snapshot close request");
	assert_eq!(close.status(), StatusCode::NO_CONTENT);

	let missing = server
		.client
		.get(server.archive_endpoint(&format!("{}/entries", open_body.snapshot_id)))
		.send()
		.await
		.expect("snapshot entries after close");
	assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
