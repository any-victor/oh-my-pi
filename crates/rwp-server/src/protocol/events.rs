//! Push and streaming events emitted by server handlers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One push event delivered on `GET /sessions/{id}/events`. Serialized as a
/// single NDJSON record per event.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionEvent {
	/// File on disk was modified by the server (writes routed through this
	/// session) or an active watcher. Includes a content hash when known so the
	/// harness can invalidate its own caches.
	FileChanged { path: String, etag: Option<String> },
	/// Diagnostics arrived from an LSP server for a previously written file.
	/// `diagnostics` is the verbatim LSP `PublishDiagnosticsParams.diagnostics`
	/// array (server passes it through opaquely).
	Diagnostics { path: String, diagnostics: serde_json::Value },
	/// Liveness ping so the harness can detect dead-stream conditions even
	/// when the workspace is quiet.
	Heartbeat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
	Info,
	Warn,
	Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct LogRecord {
	pub ts_ms:   i64,
	pub level:   LogLevel,
	pub source:  String,
	pub message: String,
	#[serde(default)]
	pub fields:  BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BashRawArtifact {
	Path { path: String },
	Bytes { bytes: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BashExitMinimizer {
	pub minimized:       bool,
	pub original_lines:  usize,
	pub minimized_lines: usize,
	pub omitted_lines:   usize,
	pub truncated:       bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub raw_artifact:    Option<BashRawArtifact>,
}

/// One NDJSON event delivered on `POST /sessions/{id}/bash.exec`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BashEvent {
	Output {
		data: String,
	},
	Stdout {
		data: String,
	},
	Stderr {
		data: String,
	},
	Heartbeat,
	Exit {
		code:      Option<i32>,
		cancelled: bool,
		timed_out: bool,
		#[serde(skip_serializing_if = "Option::is_none")]
		minimizer: Option<BashExitMinimizer>,
	},
}
