//! Response body schemas.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSessionResponse {
	pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EditResult {
	pub diff:               String,
	pub first_changed_line: Option<u32>,
	pub op:                 EditOp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EditOp {
	Create,
	Update,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AstEditResult {
	pub changes:        Vec<AstFileChange>,
	pub file_changes:   Vec<AstEditFileChange>,
	pub files_searched: u32,
	pub limit_reached:  bool,
	pub parse_errors:   Vec<AstParseError>,
	pub written:        bool,
	pub truncated:      bool,
	pub exceeded_limit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AstFileChange {
	pub path:         String,
	pub replacements: u32,
	pub diff:         String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AstEditFileChange {
	pub path:         String,
	pub replacements: usize,
	pub before_lines: Vec<String>,
	pub after_lines:  Vec<String>,
	pub hunks:        Vec<AstEditHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AstEditHunk {
	pub before_start: u32,
	pub before_lines: Vec<String>,
	pub after_lines:  Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AstParseError {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub file:    Option<String>,
	pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReadAstResponse {
	pub language:    Option<String>,
	pub parsed:      bool,
	pub elided:      bool,
	pub total_lines: u32,
	pub segments:    Vec<ReadAstSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReadAstSegment {
	pub kind:       String,
	pub start_line: u32,
	pub end_line:   u32,
	pub text:       Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatResponse {
	pub exists:    bool,
	pub kind:      String,
	pub size:      u64,
	pub mtime_ms:  i64,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub link_kind: Option<String>,

	pub etag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArchiveEntry {
	pub path:            String,
	pub kind:            String,
	pub size:            u64,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mtime_ms:        Option<i64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub compressed_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArchiveEntriesResponse {
	pub entries:   Vec<ArchiveEntry>,
	pub format:    String,
	pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArchiveReadHeaders {
	pub etag:          String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub content_type:  Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub content_range: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArchiveWriteResponse {
	pub etag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArchiveBulkWriteResponse {
	pub etag:    String,
	pub written: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BlobSizeResponse {
	pub size:         u64,
	pub etag:         String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub content_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImageMetadataResponse {
	pub mime_type: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub width:     Option<u32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub height:    Option<u32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub channels:  Option<u32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub has_alpha: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceEntry {
	pub path:      String,
	pub file_type: u8,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mtime:     Option<f64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub size:      Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ListWorkspaceResponse {
	pub entries:         Vec<WorkspaceEntry>,
	pub agents_md_files: Vec<String>,
	pub truncated:       bool,
}

pub type SqliteRow = std::collections::BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqliteColumn {
	pub name:   String,
	#[serde(rename = "type")]
	pub r#type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqliteTableInfo {
	pub name:      String,
	pub row_count: u64,
	pub columns:   Vec<SqliteColumn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqliteTablesResponse {
	pub tables: Vec<SqliteTableInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadDbResponse {
	pub rows:         Vec<SqliteRow>,
	pub columns:      Vec<SqliteColumn>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub rowid_column: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WriteDbResponse {
	pub affected: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CdpHandleResponseKind {
	Spawned,
	Attached,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CdpHandleResponse {
	pub name:           String,
	pub kind:           CdpHandleResponseKind,
	pub ws_url:         String,
	pub ref_count:      u32,
	pub last_active_ms: u64,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub args:           Vec<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub headless:       Option<bool>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[schema(value_type = Object)]
	pub user_prefs:     Option<std::collections::BTreeMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LspGetResponse {
	pub name:           String,
	pub initialized:    bool,
	#[schema(value_type = Object)]
	pub capabilities:   serde_json::Value,
	pub project_loaded: bool,
	pub open_files:     Vec<String>,
	#[schema(value_type = Object)]
	pub diagnostics:    std::collections::BTreeMap<String, serde_json::Value>,
	pub ref_count:      u32,
	pub last_active_ms: u64,
}
