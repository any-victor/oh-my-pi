//! `rwp-server` binary.

use std::path::PathBuf;

use anyhow::{Context, bail};
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use http::HeaderValue;
use rwp_server::{AppState, build_router};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about = "Remote Workspace Protocol server")]
struct Cli {
	/// Bind address. `127.0.0.1:0` picks an ephemeral port.
	#[arg(long, default_value = "127.0.0.1:0")]
	bind:           String,
	/// Optional workspace root (informational; sessions set their own cwd).
	#[arg(long)]
	workspace_root: Option<String>,
	/// Allowed CORS origin. Repeat to allow multiple origins. Use `*` to allow
	/// any origin.
	#[arg(long = "cors-origin", value_name = "ORIGIN", value_parser = parse_cors_origin)]
	cors_origins:   Vec<String>,
	/// PEM-encoded TLS certificate chain.
	#[arg(long, value_name = "PATH")]
	tls_cert:       Option<PathBuf>,
	/// PEM-encoded TLS private key.
	#[arg(long, value_name = "PATH")]
	tls_key:        Option<PathBuf>,
}

fn parse_cors_origin(origin: &str) -> Result<String, String> {
	if origin == "*" {
		return Ok(origin.to_owned());
	}
	HeaderValue::from_str(origin)
		.map(|_| origin.to_owned())
		.map_err(|error| format!("invalid CORS origin `{origin}`: {error}"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.map_err(|_| anyhow::anyhow!("failed to install rustls default crypto provider"))?;
	tracing_subscriber::fmt()
		.with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
		.init();

	let cli = Cli::parse();
	let tls_paths = match (cli.tls_cert.as_deref(), cli.tls_key.as_deref()) {
		(Some(cert), Some(key)) => Some((cert, key)),
		(None, None) => None,
		_ => bail!("both --tls-cert and --tls-key must be provided together"),
	};
	let state = AppState::with_auth_token(std::env::var("RWP_TOKEN").ok());
	let router = build_router(state, cli.cors_origins.clone());

	let listener = TcpListener::bind(&cli.bind)
		.await
		.with_context(|| format!("bind {}", cli.bind))?;
	let local = listener
		.local_addr()
		.context("local address of bound listener")?;
	let scheme = if tls_paths.is_some() { "https" } else { "http" };
	tracing::info!(address = %local, scheme, workspace_root = ?cli.workspace_root, "rwp-server listening");
	// Stdout line so test harnesses can capture the chosen port reliably.
	println!("rwp-server listening on {scheme}://{local}");

	match tls_paths {
		Some((cert, key)) => {
			let config = RustlsConfig::from_pem_file(cert, key)
				.await
				.with_context(|| {
					format!("load TLS config from {} and {}", cert.display(), key.display())
				})?;
			let listener = listener
				.into_std()
				.context("convert TLS listener to std::net::TcpListener")?;
			axum_server::from_tcp_rustls(listener, config)
				.serve(router.into_make_service())
				.await
				.context("axum-server TLS serve terminated")?;
		},
		None => {
			axum::serve(listener, router)
				.await
				.context("axum::serve terminated")?;
		},
	}
	Ok(())
}
