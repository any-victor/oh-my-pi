use std::{collections::BTreeMap, io::Cursor, net::SocketAddr, sync::Arc};

use reqwest::StatusCode;
use rwp_server::{AppState, build_router, session::Session};
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

	fn endpoint(&self, suffix: &str) -> String {
		format!("http://{}/sessions/{}/{}", self.addr, self.id, suffix)
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

fn append_tar_file<W: std::io::Write>(builder: &mut tar::Builder<W>, path: &str, body: &[u8]) {
	let mut header = tar::Header::new_gnu();
	header.set_size(body.len() as u64);
	header.set_mode(0o644);
	header.set_mtime(1);
	header.set_cksum();
	builder
		.append_data(&mut header, path, Cursor::new(body))
		.expect("append tar file");
}

fn write_tar(path: &std::path::Path, entries: &[(&str, &[u8])]) {
	let file = std::fs::File::create(path).expect("create tar");
	let mut builder = tar::Builder::new(file);
	for (name, body) in entries {
		append_tar_file(&mut builder, name, body);
	}
	builder.finish().expect("finish tar");
}

fn write_targz(path: &std::path::Path, entries: &[(&str, &[u8])]) {
	let file = std::fs::File::create(path).expect("create tar.gz");
	let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
	let mut builder = tar::Builder::new(encoder);
	for (name, body) in entries {
		append_tar_file(&mut builder, name, body);
	}
	builder.finish().expect("finish tar.gz");
}

async fn list_entries(server: &TestServer, archive_path: &str, extra: &[(&str, &str)]) -> Value {
	let mut request = server
		.client
		.get(server.endpoint("archive.entries"))
		.query(&[("path", archive_path)]);
	for (key, value) in extra {
		request = request.query(&[(*key, *value)]);
	}
	let response = request.send().await.expect("archive.entries request");
	assert_eq!(response.status(), StatusCode::OK);
	response.json().await.expect("archive.entries json")
}

#[tokio::test]
async fn zip_entries_read_write_and_if_match_round_trip() {
	let server = TestServer::start().await;
	let archive = server.tempdir.path().join("fixture.zip");
	write_zip(&archive, &[("root.txt", b"old"), ("docs/readme.txt", b"nested")]);

	let listed = list_entries(&server, "fixture.zip", &[]).await;
	assert_eq!(listed.get("format"), Some(&Value::String("zip".to_owned())));
	let entries = listed
		.get("entries")
		.and_then(Value::as_array)
		.expect("entries array");
	assert_eq!(entries.len(), 2);
	assert_eq!(entries[0].get("path"), Some(&Value::String("docs/readme.txt".to_owned())));
	assert_eq!(entries[1].get("path"), Some(&Value::String("root.txt".to_owned())));

	let read_before = server
		.client
		.get(server.endpoint("archive.read"))
		.query(&[("path", "fixture.zip"), ("entry", "root.txt")])
		.send()
		.await
		.expect("archive.read before write");
	assert_eq!(read_before.status(), StatusCode::OK);
	assert_eq!(
		read_before
			.bytes()
			.await
			.expect("zip bytes before")
			.as_ref(),
		b"old"
	);

	let archive_meta = server
		.client
		.get(server.endpoint("read.blob"))
		.query(&[("path", "fixture.zip"), ("size_only", "1")])
		.send()
		.await
		.expect("read.blob size_only");
	assert_eq!(archive_meta.status(), StatusCode::OK);
	let archive_etag = archive_meta
		.json::<Value>()
		.await
		.expect("archive meta json")
		.get("etag")
		.and_then(Value::as_str)
		.expect("archive etag")
		.to_owned();

	let write = server
		.client
		.put(server.endpoint("archive.write"))
		.query(&[("path", "fixture.zip"), ("entry", "root.txt")])
		.header(reqwest::header::IF_MATCH, format!("\"{archive_etag}\""))
		.body(Vec::from(b"new archive payload" as &[u8]))
		.send()
		.await
		.expect("archive.write request");
	assert_eq!(write.status(), StatusCode::OK);
	let write_body: Value = write.json().await.expect("archive.write json");
	assert!(write_body.get("etag").and_then(Value::as_str).is_some());

	let read_after = server
		.client
		.get(server.endpoint("archive.read"))
		.query(&[("path", "fixture.zip"), ("entry", "root.txt")])
		.send()
		.await
		.expect("archive.read after write");
	assert_eq!(read_after.status(), StatusCode::OK);
	assert_eq!(read_after.bytes().await.expect("zip bytes after").as_ref(), b"new archive payload");

	let stale = server
		.client
		.put(server.endpoint("archive.write"))
		.query(&[("path", "fixture.zip"), ("entry", "root.txt")])
		.header(reqwest::header::IF_MATCH, "\"stale\"")
		.body(Vec::from(b"should fail" as &[u8]))
		.send()
		.await
		.expect("archive.write stale request");
	assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn tar_entries_read_and_prefix_filter_work() {
	let server = TestServer::start().await;
	let archive = server.tempdir.path().join("fixture.tar");
	write_tar(&archive, &[
		("docs/a.txt", b"alpha"),
		("docs/b.txt", b"beta"),
		("misc/c.txt", b"gamma"),
	]);

	let listed = list_entries(&server, "fixture.tar", &[]).await;
	assert_eq!(listed.get("format"), Some(&Value::String("tar".to_owned())));
	assert_eq!(
		listed
			.get("entries")
			.and_then(Value::as_array)
			.expect("tar entries")
			.len(),
		3
	);

	let filtered = list_entries(&server, "fixture.tar", &[("prefix", "docs/")]).await;
	let filtered_entries = filtered
		.get("entries")
		.and_then(Value::as_array)
		.expect("filtered entries");
	assert_eq!(filtered_entries.len(), 2);
	assert!(filtered_entries.iter().all(|entry| {
		entry
			.get("path")
			.and_then(Value::as_str)
			.is_some_and(|path| path.starts_with("docs/"))
	}));

	let read = server
		.client
		.get(server.endpoint("archive.read"))
		.query(&[("path", "fixture.tar"), ("entry", "docs/a.txt")])
		.send()
		.await
		.expect("archive.read tar request");
	assert_eq!(read.status(), StatusCode::OK);
	assert_eq!(read.bytes().await.expect("tar bytes").as_ref(), b"alpha");
}

#[tokio::test]
async fn targz_entries_limit_and_missing_entry_work() {
	let server = TestServer::start().await;
	let archive = server.tempdir.path().join("fixture.tar.gz");
	write_targz(&archive, &[("one.txt", b"one"), ("two.txt", b"two"), ("three.txt", b"three")]);

	let listed = list_entries(&server, "fixture.tar.gz", &[("limit", "2")]).await;
	assert_eq!(listed.get("format"), Some(&Value::String("tar.gz".to_owned())));
	assert_eq!(listed.get("truncated"), Some(&Value::Bool(true)));
	assert_eq!(
		listed
			.get("entries")
			.and_then(Value::as_array)
			.expect("tar.gz entries")
			.len(),
		2
	);

	let read = server
		.client
		.get(server.endpoint("archive.read"))
		.query(&[("path", "fixture.tar.gz"), ("entry", "two.txt")])
		.send()
		.await
		.expect("archive.read tar.gz request");
	assert_eq!(read.status(), StatusCode::OK);
	assert_eq!(read.bytes().await.expect("tar.gz bytes").as_ref(), b"two");

	let missing = server
		.client
		.get(server.endpoint("archive.read"))
		.query(&[("path", "fixture.tar.gz"), ("entry", "missing.txt")])
		.send()
		.await
		.expect("archive.read missing request");
	assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
