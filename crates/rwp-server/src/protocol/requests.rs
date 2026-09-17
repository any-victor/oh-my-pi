//! Request body schemas. Stub forms — each handler may extend before Phase 1
//! implementation. Kept compact: the wire shape is the source of truth and
//! changes here ripple into the `OpenAPI` schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::named::RequestedHandleScope;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalTransport {
	Stdio,
	Jupyter,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSessionRequest {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cwd: Option<String>,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetCwdRequest {
	pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PatchEnvRequest {
	/// `null` value unsets the key. Otherwise sets it.
	pub env: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EditReplaceRequest {
	pub path:        String,
	pub old:         String,
	pub new:         String,
	#[serde(default)]
	pub fuzzy:       bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub if_match:    Option<String>,
	#[serde(default)]
	pub regex:       bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub regex_flags: Option<String>,
	#[serde(default)]
	pub all:         bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Hunk {
	/// 1-based starting line.
	pub start:    u32,
	/// Number of lines to delete starting at `start`.
	pub deleted:  u32,
	/// Replacement lines (no trailing newlines).
	pub inserted: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EditPatchRequest {
	pub path:     String,
	pub hunks:    Vec<Hunk>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub if_match: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AstOp {
	pub pat: String,
	pub out: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EditAstRequest {
	pub ops:      Vec<AstOp>,
	pub paths:    Vec<String>,
	#[serde(default)]
	pub dry_run:  bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BashExecMinimizer {
	pub enabled:       bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub aggressive:    Option<bool>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub min_lines:     Option<u32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub context_lines: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BashExecRequest {
	pub command:     String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cwd:         Option<String>,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub env:         BTreeMap<String, String>,
	#[serde(default)]
	pub pty:         bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout_ms:  Option<u64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub minimizer:   Option<BashExecMinimizer>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session_key: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NamedHandleScope {
	Global,
	Session,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct NamedHandleQuery {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NamedHandleConfig {
	Eval {
		lang:            String,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		kernelspec:      Option<String>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		transport:       Option<EvalTransport>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		idle_timeout_ms: Option<u64>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		scope:           Option<RequestedHandleScope>,
	},
	Lsp {
		command:                String,
		#[serde(default)]
		args:                   Vec<String>,
		#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
		env:                    BTreeMap<String, String>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		root_uri:               Option<String>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		initialization_options: Option<serde_json::Value>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		idle_timeout_ms:        Option<u64>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		scope:                  Option<RequestedHandleScope>,
	},
	Dap {
		command:         String,
		#[serde(default)]
		args:            Vec<String>,
		#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
		env:             BTreeMap<String, String>,
		#[serde(default)]
		transport:       DapTransport,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		host:            Option<String>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		port:            Option<u16>,
		#[serde(default = "default_dap_retry_ms")]
		retry_ms:        u32,
		#[serde(default)]
		retry_attempts:  u32,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		idle_timeout_ms: Option<u64>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		scope:           Option<RequestedHandleScope>,
	},
	CdpSpawn {
		#[serde(default, skip_serializing_if = "Option::is_none")]
		path:            Option<String>,
		#[serde(default)]
		args:            Vec<String>,
		#[serde(default)]
		headless:        bool,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		user_prefs:      Option<BTreeMap<String, serde_json::Value>>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		idle_timeout_ms: Option<u64>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		scope:           Option<RequestedHandleScope>,
	},
	CdpAttach {
		cdp_url:         String,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		idle_timeout_ms: Option<u64>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		scope:           Option<RequestedHandleScope>,
	},
}

impl NamedHandleConfig {
	#[must_use]
	pub const fn scope(&self) -> Option<RequestedHandleScope> {
		match self {
			Self::Eval { scope, .. }
			| Self::Lsp { scope, .. }
			| Self::Dap { scope, .. }
			| Self::CdpSpawn { scope, .. }
			| Self::CdpAttach { scope, .. } => *scope,
		}
	}
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DapTransport {
	#[default]
	Stdio,
	Tcp,
}
const fn default_dap_retry_ms() -> u32 {
	100
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ReadDbQuery {
	pub path:         String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub table:        Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub key:          Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub q:            Option<String>,
	#[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
	pub where_clause: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub order:        Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub limit:        Option<u64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub offset:       Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WriteDbOp {
	Insert,
	Update,
	Delete,
	Exec,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WriteDbRequest {
	pub path:  String,
	pub op:    WriteDbOp,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub table: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub key:   Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub row:   Option<std::collections::BTreeMap<String, serde_json::Value>>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sql:   Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArchiveBulkWriteEntry {
	pub name:  String,
	pub bytes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArchiveBulkWriteRequest {
	pub entries: Vec<ArchiveBulkWriteEntry>,
}
