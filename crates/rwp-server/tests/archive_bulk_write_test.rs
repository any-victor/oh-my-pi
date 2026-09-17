use std::{collections::BTreeMap, io::Cursor, net::SocketAddr, sync::Arc};

use reqwest::StatusCode;
use rwp_server::{AppState, build_router, session::Session};
use serde_json::{Value, json};
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

fn base64_encode(bytes: &[u8]) -> String {
	const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	let mut out = String::new();
	for chunk in bytes.chunks(3) {
		let b0 = *chunk.first().unwrap_or(&0);
		let b1 = *chunk.get(1).unwrap_or(&0);
		let b2 = *chunk.get(2).unwrap_or(&0);
		let n = u32::from(b0) << 16 | u32::from(b1) << 8 | u32::from(b2);
		out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
		out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
		out.push(if chunk.len() > 1 {
			TABLE[((n >> 6) & 0x3f) as usize] as char
		} else {
			'='
		});
		out.push(if chunk.len() > 2 {
			TABLE[(n & 0x3f) as usize] as char
		} else {
			'='
		});
	}
	out
}

async fn archive_etag(server: &TestServer, path: &str) -> String {
	server
		.client
		.get(server.endpoint("read.blob"))
		.query(&[("path", path), ("size_only", "1")])
		.send()
		.await
		.expect("read.blob size_only")
		.json::<Value>()
		.await
		.expect("size_only json")
		.get("etag")
		.and_then(Value::as_str)
		.expect("etag")
		.to_owned()
}

async fn archive_read(server: &TestServer, path: &str, entry: &str) -> Vec<u8> {
	server
		.client
		.get(server.endpoint("archive.read"))
		.query(&[("path", path), ("entry", entry)])
		.send()
		.await
		.expect("archive.read")
		.bytes()
		.await
		.expect("archive bytes")
		.to_vec()
}

async fn bulk_write(
	server: &TestServer,
	path: &str,
	entries: &[(&str, &[u8])],
	if_match: Option<&str>,
) -> reqwest::Response {
	let mut request = server
		.client
		.put(server.endpoint("archive.bulk_write"))
		.query(&[("path", path)])
		.json(&json!({
			"entries": entries
				.iter()
				.map(|(name, bytes)| json!({
					"name": *name,
					"bytes": base64_encode(bytes),
				}))
				.collect::<Vec<_>>()
		}));
	if let Some(etag) = if_match {
		request = request.header(reqwest::header::IF_MATCH, format!("\"{etag}\""));
	}
	request.send().await.expect("archive.bulk_write request")
}

#[tokio::test]
async fn archive_bulk_write_round_trips_zip_tar_and_targz() {
	let server = TestServer::start().await;
	write_zip(&server.tempdir.path().join("fixture.zip"), &[("a.txt", b"old"), ("b.txt", b"keep")]);
	write_tar(&server.tempdir.path().join("fixture.tar"), &[("a.txt", b"old"), ("b.txt", b"keep")]);
	write_targz(&server.tempdir.path().join("fixture.tar.gz"), &[
		("a.txt", b"old"),
		("b.txt", b"keep"),
	]);

	for archive in ["fixture.zip", "fixture.tar", "fixture.tar.gz"] {
		let response =
			bulk_write(&server, archive, &[("a.txt", b"new"), ("c.txt", b"added")], None).await;
		assert_eq!(response.status(), StatusCode::OK);
		let body: Value = response.json().await.expect("bulk json");
		assert_eq!(body.get("written"), Some(&Value::from(8_u64)));
		assert_eq!(archive_read(&server, archive, "a.txt").await, b"new");
		assert_eq!(archive_read(&server, archive, "b.txt").await, b"keep");
		assert_eq!(archive_read(&server, archive, "c.txt").await, b"added");
	}
}

#[tokio::test]
async fn archive_bulk_write_honors_if_match() {
	let server = TestServer::start().await;
	write_zip(&server.tempdir.path().join("fixture.zip"), &[("a.txt", b"old")]);
	let etag = archive_etag(&server, "fixture.zip").await;
	let ok = bulk_write(&server, "fixture.zip", &[("a.txt", b"new")], Some(&etag)).await;
	assert_eq!(ok.status(), StatusCode::OK);
	let stale = bulk_write(&server, "fixture.zip", &[("a.txt", b"stale")], Some("deadbeef")).await;
	assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn archive_bulk_write_is_atomic_on_invalid_payload() {
	let server = TestServer::start().await;
	write_zip(&server.tempdir.path().join("fixture.zip"), &[("a.txt", b"old")]);
	let response = server
		.client
		.put(server.endpoint("archive.bulk_write"))
		.query(&[("path", "fixture.zip")])
		.json(&json!({
			"entries": [
				{"name": "a.txt", "bytes": "not-base64"}
			]
		}))
		.send()
		.await
		.expect("invalid archive.bulk_write request");
	assert_eq!(response.status(), StatusCode::BAD_REQUEST);
	assert_eq!(archive_read(&server, "fixture.zip", "a.txt").await, b"old");
}
