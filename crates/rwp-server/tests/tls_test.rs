use std::{
	net::SocketAddr,
	path::{Path, PathBuf},
	process::Stdio,
};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use reqwest::StatusCode;
use serde_json::Value;
use tempfile::TempDir;
use tokio::{
	io::{AsyncBufReadExt, BufReader},
	process::{Child, Command},
	time::{Duration, sleep, timeout},
};

struct TestServer {
	addr:  SocketAddr,
	child: Child,
}

impl TestServer {
	fn https_url(&self, path: &str) -> String {
		format!("https://localhost:{}{path}", self.addr.port())
	}

	async fn shutdown(mut self) {
		let _ = self.child.start_kill();
		let _ = self.child.wait().await;
	}
}

impl Drop for TestServer {
	fn drop(&mut self) {
		let _ = self.child.start_kill();
	}
}

fn write_tls_material(tempdir: &TempDir) -> (PathBuf, PathBuf) {
	let CertifiedKey { cert, key_pair } =
		generate_simple_self_signed(vec!["localhost".to_owned()]).expect("generate self-signed cert");
	let cert_path = tempdir.path().join("cert.pem");
	let key_path = tempdir.path().join("key.pem");
	std::fs::write(&cert_path, cert.pem()).expect("write cert pem");
	std::fs::write(&key_path, key_pair.serialize_pem()).expect("write key pem");
	(cert_path, key_path)
}

async fn spawn_tls_server(cert_path: &Path, key_path: &Path) -> TestServer {
	let mut child = Command::new(env!("CARGO_BIN_EXE_rwp-server"))
		.args([
			"--bind",
			"127.0.0.1:0",
			"--tls-cert",
			cert_path.to_str().expect("cert path utf8"),
			"--tls-key",
			key_path.to_str().expect("key path utf8"),
		])
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.expect("spawn rwp-server");
	let stdout = child.stdout.take().expect("capture server stdout");
	let mut lines = BufReader::new(stdout).lines();
	let addr = timeout(Duration::from_secs(10), async {
		loop {
			match lines.next_line().await.expect("read server stdout") {
				Some(line) => {
					if let Some(addr) = line.strip_prefix("rwp-server listening on https://") {
						return addr.parse::<SocketAddr>().expect("parse listen address");
					}
				},
				None => panic!("server exited before reporting HTTPS listen address"),
			}
		}
	})
	.await
	.expect("wait for HTTPS listen line");
	TestServer { addr, child }
}

#[tokio::test]
async fn serves_https_when_tls_cert_and_key_are_configured() {
	let tempdir = TempDir::new().expect("tempdir");
	let (cert_path, key_path) = write_tls_material(&tempdir);
	let server = spawn_tls_server(&cert_path, &key_path).await;
	let client = reqwest::Client::builder()
		.danger_accept_invalid_certs(true)
		.build()
		.expect("build https client");

	let response = timeout(Duration::from_secs(10), async {
		loop {
			match client.get(server.https_url("/openapi.json")).send().await {
				Ok(response) => return response,
				Err(_) => sleep(Duration::from_millis(50)).await,
			}
		}
	})
	.await
	.expect("wait for HTTPS response");
	assert_eq!(response.status(), StatusCode::OK);
	let body: Value = response.json().await.expect("decode openapi json");
	assert!(body.get("openapi").is_some(), "expected OpenAPI document");

	server.shutdown().await;
}

#[tokio::test]
async fn rejects_partial_tls_flag_configuration() {
	let tempdir = TempDir::new().expect("tempdir");
	let (cert_path, _key_path) = write_tls_material(&tempdir);
	let output = timeout(
		Duration::from_secs(10),
		Command::new(env!("CARGO_BIN_EXE_rwp-server"))
			.args(["--tls-cert", cert_path.to_str().expect("cert path utf8")])
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.output(),
	)
	.await
	.expect("wait for process exit")
	.expect("run rwp-server");

	assert!(!output.status.success(), "expected non-zero exit for partial TLS config");
	let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
	assert!(
		stderr.contains("both --tls-cert and --tls-key must be provided together"),
		"unexpected stderr: {stderr}"
	);
}
