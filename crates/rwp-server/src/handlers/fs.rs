//! Filesystem reads, writes, glob, grep.

use std::{
	collections::{BTreeMap, HashSet},
	ffi::OsString,
	io::{BufRead, BufReader, Cursor, Read},
	path::{Component, Path as FsPath, PathBuf},
	sync::{
		Arc, LazyLock,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
	Json,
	body::{Body, Bytes},
	extract::{Path, Query, State},
	http::{
		HeaderMap, HeaderValue, StatusCode,
		header::{self, CONTENT_TYPE, HeaderName},
	},
	response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use dashmap::DashMap;
use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;
use pi_ast::{
	SupportLang, ops as ast_ops,
	summary::{SummaryOptions, SummaryResult},
};
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncReadExt, sync::mpsc, time::sleep};
use tokio_stream::wrappers::ReceiverStream;
use url::Url;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
	fs_ops::{
		self, HEARTBEAT_INTERVAL, ReadTarget, heartbeat_stream,
		write_through::{WriteRequest, write_through},
	},
	protocol::{
		SessionEvent,
		error::{ApiError, ApiResult, ErrorBody},
		requests::ArchiveBulkWriteRequest,
		responses::{
			ArchiveBulkWriteResponse, ArchiveEntriesResponse, ArchiveEntry, ArchiveWriteResponse,
			BlobSizeResponse, ImageMetadataResponse, ListWorkspaceResponse, ReadAstResponse,
			ReadAstSegment, StatResponse, WorkspaceEntry,
		},
	},
	session::Session,
	state::AppState,
};

const DEFAULT_GLOB_LIMIT: usize = 1_000;
const DEFAULT_GITIGNORE: bool = true;
const DEFAULT_HIDDEN: bool = false;
const DEFAULT_GREP_CONTEXT: usize = 2;
const DEFAULT_GREP_MAX_MATCHES: usize = 500;
const GLOB_RESPONSE_MAX_BYTES: usize = 50 * 1024;
const IMAGE_METADATA_HEADER_BYTES: usize = 256 * 1024;
const X_TRUNCATED: HeaderName = HeaderName::from_static("x-truncated");
const X_TOTAL_LINES: HeaderName = HeaderName::from_static("x-total-lines");
const ARCHIVE_SNAPSHOT_IDLE_SECS: u64 = 30;
const ARCHIVE_SNAPSHOT_REAPER_INTERVAL: Duration = Duration::from_secs(5);

static ARCHIVE_SNAPSHOTS: LazyLock<DashMap<Uuid, Arc<ArchiveSnapshotState>>> =
	LazyLock::new(DashMap::new);
static ARCHIVE_SNAPSHOT_REAPER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
pub struct GlobQuery {
	patterns:  String,
	#[serde(rename = "paths[]", default)]
	paths:     Option<String>,
	#[serde(default)]
	hidden:    Option<bool>,
	#[serde(default)]
	limit:     Option<usize>,
	#[serde(default)]
	gitignore: Option<bool>,
}

#[derive(Debug, Serialize)]
struct GlobPathEntry {
	path:  String,
	mtime: i64,
	size:  u64,
}

#[derive(Debug, Serialize)]
struct GlobResponse {
	paths:     Vec<GlobPathEntry>,
	truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct GrepQuery {
	pattern:        String,
	#[serde(default)]
	paths:          Option<String>,
	#[serde(default)]
	i:              Option<bool>,
	#[serde(default)]
	skip:           Option<usize>,
	#[serde(default)]
	gitignore:      Option<bool>,
	#[serde(default)]
	context:        Option<usize>,
	#[serde(default)]
	context_before: Option<usize>,
	#[serde(default)]
	context_after:  Option<usize>,
	#[serde(default)]
	max_matches:    Option<usize>,
}
#[derive(Debug, Serialize)]
struct GrepSummary {
	#[serde(rename = "type")]
	type_:         &'static str,
	#[serde(rename = "limitReached")]
	limit_reached: bool,
	#[serde(default, skip_serializing_if = "Option::is_none", rename = "truncated")]
	truncated:     Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ReadLinesQuery {
	path:      String,
	#[serde(default)]
	range:     Option<String>,
	#[serde(default)]
	reader:    Option<ReadLinesReader>,
	#[serde(default)]
	max_lines: Option<usize>,
	#[serde(default)]
	max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ReadLinesReader {
	Raw,
	Markdown,
	Text,
}

#[derive(Debug, Deserialize)]
pub struct ReadBlobQuery {
	path:      String,
	#[serde(default)]
	size_only: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadAstQuery {
	path:              String,
	#[serde(default)]
	language:          Option<String>,
	#[serde(default)]
	range:             Option<String>,
	#[serde(default)]
	min_body_lines:    Option<u32>,
	#[serde(default)]
	min_comment_lines: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ImageMetaQuery {
	path: String,
}

#[derive(Debug, Deserialize)]
pub struct ListWorkspaceQuery {
	path:              String,
	max_depth:         u32,
	#[serde(default)]
	hidden:            Option<bool>,
	#[serde(default)]
	gitignore:         Option<bool>,
	#[serde(default)]
	collect_agents_md: Option<bool>,
	#[serde(default)]
	timeout_ms:        Option<u32>,
}

#[derive(Debug, Clone, Copy)]
enum LineSelector {
	Whole,
	Raw,
	Single(usize),
	Range { start: usize, end: usize },
	FromStart { start: usize, len: usize },
}

#[derive(Debug, Deserialize)]
pub struct WriteQuery {
	path: String,
}

#[derive(Debug, Deserialize)]
pub struct DeleteFsQuery {
	path: String,
}

#[derive(Debug, Deserialize)]
pub struct MkdirQuery {
	path:      String,
	#[serde(default)]
	recursive: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RenameQuery {
	from:      String,
	to:        String,
	#[serde(default)]
	overwrite: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct StatQuery {
	path:            String,
	#[serde(default)]
	follow_symlinks: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ArchiveEntriesQuery {
	path:   String,
	#[serde(default)]
	prefix: Option<String>,
	#[serde(default)]
	limit:  Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ArchiveReadQuery {
	path:  String,
	entry: String,
}

#[derive(Debug, Deserialize)]
pub struct ArchiveWriteQuery {
	path:  String,
	entry: String,
}

#[derive(Debug, Deserialize)]
pub struct ArchivePathQuery {
	path: String,
}

#[derive(Debug, Deserialize)]
pub struct ArchiveSnapshotEntryQuery {
	path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ArchiveSnapshotOpenResponse {
	snapshot_id: Uuid,
	format:      String,
}

#[derive(Debug, Serialize)]
struct GrepRecord {
	path:      String,
	line:      u32,
	kind:      &'static str,
	text:      String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	truncated: Option<bool>,
}

#[derive(Debug, Default)]
struct MatchCollector {
	line_numbers: Vec<u32>,
}

impl Sink for MatchCollector {
	type Error = std::io::Error;

	fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
		if let Some(line_number) = mat.line_number() {
			let line_number = u32::try_from(line_number)
				.map_err(|_| std::io::Error::other("match line number overflow"))?;
			self.line_numbers.push(line_number);
		}
		Ok(true)
	}
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/read.lines",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "path inside the session cwd or `http(s)://` URL"),
		("range" = Option<String>, Query, description = "`A-B`, `A+N`, `A`, or `raw` line selector"),
		("reader" = Option<String>, Query, description = "`raw|markdown|text` URL reader mode for HTML responses"),
	),
	responses(
		(status = 200),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn read_lines(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ReadLinesQuery>,
) -> ApiResult<Response> {
	let session = get_session(&state, id)?;
	let (path, range) = fs_ops::split_inline_selector(&query.path, query.range);
	let selector = range
		.as_deref()
		.map(parse_line_selector)
		.transpose()?
		.unwrap_or(LineSelector::Whole);
	let url_reader = match query.reader.unwrap_or(ReadLinesReader::Markdown) {
		ReadLinesReader::Raw => fs_ops::UrlReaderMode::Raw,
		ReadLinesReader::Markdown => fs_ops::UrlReaderMode::Markdown,
		ReadLinesReader::Text => fs_ops::UrlReaderMode::Text,
	};
	let text = match fs_ops::resolve_read_target(&session, &path).await? {
		ReadTarget::File(path) => {
			if query.max_lines.is_some() || query.max_bytes.is_some() {
				read_file_text_capped(&path, query.max_lines, query.max_bytes).await?
			} else {
				let body = fs_ops::read_file_cached(&session, &path).await?;
				CappedTextRead {
					etag:      body.etag.to_string(),
					text:      String::from_utf8_lossy(&body.bytes).into_owned(),
					truncated: false,
				}
			}
		},
		ReadTarget::Url(url) => {
			let (etag, text) = fs_ops::fetch_url_text(&url, url_reader).await?;
			CappedTextRead { etag, text, truncated: false }
		},
	};
	let body = select_lines(&text.text, selector);
	let total_lines = fs_ops::count_lines(text.text.as_bytes()).to_string();
	let mut headers = HeaderMap::new();
	headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
	headers.insert(
		header::ETAG,
		HeaderValue::from_str(&format!("\"{}\"", text.etag)).map_err(anyhow::Error::from)?,
	);
	headers.insert(X_TOTAL_LINES, HeaderValue::from_str(&total_lines).map_err(anyhow::Error::from)?);
	if text.truncated {
		headers.insert(X_TRUNCATED, HeaderValue::from_static("true"));
	}
	Ok((headers, body).into_response())
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/read.blob",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "file path relative to the session cwd"),
		("size_only" = Option<String>, Query, description = "truthy flag returning blob size metadata without body bytes"),
	),
	responses(
		(status = 200),
		(status = 206),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn read_blob(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ReadBlobQuery>,
	headers: HeaderMap,
) -> ApiResult<Response> {
	let session = get_session(&state, id)?;
	let path = fs_ops::resolve_file_path(&session, &query.path).await?;
	let body = fs_ops::read_file_cached(&session, &path).await?;
	let content_type = sniff_content_type(&body.path, &body.bytes).to_string();
	if query_flag_enabled(query.size_only.as_deref()) {
		return Ok(Json(BlobSizeResponse {
			size:         u64::try_from(body.bytes.len())
				.map_err(|_| anyhow::anyhow!("blob too large"))?,
			etag:         body.etag.to_string(),
			content_type: Some(content_type),
		})
		.into_response());
	}
	let total_len = body.bytes.len();
	let range = headers
		.get(header::RANGE)
		.map(parse_byte_range)
		.transpose()?;
	let (status, response_bytes, content_range) = if let Some((start, end)) = range {
		let end = end.min(total_len.saturating_sub(1));
		if start >= total_len || start > end {
			return Err(ApiError::BadRequest(format!(
				"byte range {start}-{end} is out of bounds for {total_len} bytes"
			)));
		}
		(
			StatusCode::PARTIAL_CONTENT,
			body.bytes[start..=end].to_vec(),
			Some(format!("bytes {start}-{end}/{total_len}")),
		)
	} else {
		(StatusCode::OK, body.bytes.as_ref().to_vec(), None)
	};
	let mut response = Response::new(Body::from(response_bytes));
	*response.status_mut() = status;
	response
		.headers_mut()
		.insert(CONTENT_TYPE, HeaderValue::from_str(&content_type).map_err(anyhow::Error::from)?);
	response
		.headers_mut()
		.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
	response.headers_mut().insert(
		header::ETAG,
		HeaderValue::from_str(&format!("\"{}\"", body.etag)).map_err(anyhow::Error::from)?,
	);
	if let Some(content_range) = content_range {
		response.headers_mut().insert(
			header::CONTENT_RANGE,
			HeaderValue::from_str(&content_range).map_err(anyhow::Error::from)?,
		);
	}
	Ok(response)
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/read.ast",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "file path relative to the session cwd"),
		("language" = Option<String>, Query, description = "optional language override"),
		("range" = Option<String>, Query, description = "optional 1-based line range `start-end`"),
		("min_body_lines" = Option<u32>, Query, description = "minimum body lines before elision"),
		("min_comment_lines" = Option<u32>, Query, description = "minimum comment lines before elision"),
	),
	responses(
		(status = 200, body = ReadAstResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn read_ast(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ReadAstQuery>,
) -> ApiResult<Json<ReadAstResponse>> {
	let session = get_session(&state, id)?;
	let path = fs_ops::resolve_file_path(&session, &query.path).await?;
	let body = fs_ops::read_file_cached(&session, &path).await?;
	let source = normalize_ast_source(String::from_utf8_lossy(&body.bytes).as_ref());
	let source = apply_ast_range(source, query.range.as_deref())?;
	let summary = pi_ast::summary::summarize_code(SummaryOptions {
		code:               source,
		lang:               query.language,
		path:               Some(body.path.to_string_lossy().into_owned()),
		min_body_lines:     query.min_body_lines,
		min_comment_lines:  query.min_comment_lines,
		unfold_until_lines: None,
		unfold_limit_lines: None,
	})
	.map_err(ApiError::Internal)?;
	Ok(Json(read_ast_response(summary)))
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/image_meta",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "image path relative to the session cwd"),
	),
	responses(
		(status = 200, body = Option<ImageMetadataResponse>),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn image_meta(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ImageMetaQuery>,
) -> ApiResult<Json<Option<ImageMetadataResponse>>> {
	let session = get_session(&state, id)?;
	let path = fs_ops::resolve_file_path(&session, &query.path).await?;
	let mut file = tokio::fs::File::open(&path).await?;
	let mut header = vec![0_u8; IMAGE_METADATA_HEADER_BYTES];
	let read = file.read(&mut header).await?;
	header.truncate(read);
	Ok(Json(parse_image_metadata(&header)))
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/list_workspace",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "directory path relative to the session cwd"),
		("max_depth" = u32, Query, description = "maximum returned tree depth"),
		("hidden" = Option<bool>, Query, description = "include hidden files and directories"),
		("gitignore" = Option<bool>, Query, description = "respect .gitignore files"),
		("collect_agents_md" = Option<bool>, Query, description = "collect directory-scoped AGENTS.md files"),
		("timeout_ms" = Option<u32>, Query, description = "operation timeout in milliseconds"),
	),
	responses(
		(status = 200, body = ListWorkspaceResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn list_workspace(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ListWorkspaceQuery>,
) -> ApiResult<Json<ListWorkspaceResponse>> {
	let session = get_session(&state, id)?;
	let root = resolve_cwd_scoped_path(&session, &query.path).await?;
	let result = tokio::task::spawn_blocking(move || {
		fs_ops::workspace::list_workspace_blocking(fs_ops::workspace::ListWorkspaceOptions {
			path:              root.to_string_lossy().into_owned(),
			max_depth:         query.max_depth,
			hidden:            query.hidden,
			gitignore:         query.gitignore,
			collect_agents_md: query.collect_agents_md,
			timeout_ms:        query.timeout_ms,
		})
	})
	.await
	.map_err(|error| ApiError::Internal(error.into()))?
	.map_err(|error| ApiError::BadRequest(error.to_string()))?;
	Ok(Json(ListWorkspaceResponse {
		entries:         result
			.entries
			.into_iter()
			.map(|entry| WorkspaceEntry {
				path:      entry.path,
				file_type: native_file_type(entry.file_type),
				mtime:     entry.mtime,
				size:      entry.size,
			})
			.collect(),
		agents_md_files: result.agents_md_files,
		truncated:       result.truncated,
	}))
}

#[utoipa::path(
	put,
	path = "/sessions/{id}/write.lines",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Path relative to the session cwd")
	),
	request_body(content = String, content_type = "text/plain"),
	responses(
		(status = 204),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 412, body = ErrorBody, description = "etag mismatch"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn write_lines(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<WriteQuery>,
	headers: HeaderMap,
	body: Bytes,
) -> ApiResult<Response> {
	let session = get_session(&state, id)?;
	let outcome = write_through(WriteRequest {
		session,
		lsp: Some(state.lsp.clone()),
		path: PathBuf::from(query.path),
		new_bytes: body.to_vec(),
		if_match: if_match_header(&headers),
		preserve_text_conventions: true,
	})
	.await?;
	Ok(etag_response(outcome.etag))
}

#[utoipa::path(
	put,
	path = "/sessions/{id}/write.blob",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Path relative to the session cwd")
	),
	request_body(content = String, content_type = "application/octet-stream"),
	responses(
		(status = 204),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 412, body = ErrorBody, description = "etag mismatch"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn write_blob(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<WriteQuery>,
	headers: HeaderMap,
	body: Bytes,
) -> ApiResult<Response> {
	let session = get_session(&state, id)?;
	let outcome = write_through(WriteRequest {
		session,
		lsp: Some(state.lsp.clone()),
		path: PathBuf::from(query.path),
		new_bytes: body.to_vec(),
		if_match: if_match_header(&headers),
		preserve_text_conventions: false,
	})
	.await?;
	Ok(etag_response(outcome.etag))
}

#[utoipa::path(
	delete,
	path = "/sessions/{id}/fs",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Regular file path relative to the session cwd")
	),
	responses(
		(status = 204),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn delete_file(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<DeleteFsQuery>,
) -> ApiResult<StatusCode> {
	let session = get_session(&state, id)?;
	let path = resolve_cwd_scoped_path(&session, &query.path).await?;
	let metadata = tokio::fs::symlink_metadata(&path)
		.await
		.map_err(|error| match error.kind() {
			std::io::ErrorKind::NotFound => {
				ApiError::NotFound(format!("path not found: {}", query.path))
			},
			_ => ApiError::Io(error),
		})?;
	if metadata.is_dir() {
		return Err(ApiError::BadRequest(format!("path is a directory: {}", query.path)));
	}
	if !metadata.is_file() {
		return Err(ApiError::BadRequest(format!("path is not a regular file: {}", query.path)));
	}
	tokio::fs::remove_file(&path).await.map_err(ApiError::Io)?;
	invalidate_fs_caches(&state, &session, [&path]);
	let _ = session
		.events
		.send(SessionEvent::FileChanged { path: query.path, etag: None });
	Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
	post,
	path = "/sessions/{id}/mkdir",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Directory path relative to the session cwd"),
		("recursive" = Option<bool>, Query, description = "Create parent directories like `mkdir -p`")
	),
	responses(
		(status = 204),
		(status = 200),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 409, body = ErrorBody, description = "path exists as a non-directory"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn mkdir(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<MkdirQuery>,
) -> ApiResult<StatusCode> {
	let session = get_session(&state, id)?;
	let path = resolve_cwd_scoped_path(&session, &query.path).await?;
	match tokio::fs::symlink_metadata(&path).await {
		Ok(metadata) => {
			if metadata.is_dir() {
				invalidate_fs_caches(&state, &session, [&path]);
				return Ok(StatusCode::OK);
			}
			return Err(ApiError::Conflict(format!(
				"path already exists and is not a directory: {}",
				query.path
			)));
		},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
		Err(error) => return Err(ApiError::Io(error)),
	}
	if query.recursive.unwrap_or(false) {
		tokio::fs::create_dir_all(&path)
			.await
			.map_err(ApiError::Io)?;
	} else {
		tokio::fs::create_dir(&path).await.map_err(ApiError::Io)?;
	}
	invalidate_fs_caches(&state, &session, [&path]);
	Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
	post,
	path = "/sessions/{id}/rename",
	params(
		("id" = Uuid, Path),
		("from" = String, Query, description = "Source path relative to the session cwd"),
		("to" = String, Query, description = "Destination path relative to the session cwd"),
		("overwrite" = Option<bool>, Query, description = "Allow replacing an existing destination")
	),
	responses(
		(status = 204),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "source path not found"),
		(status = 409, body = ErrorBody, description = "destination exists and overwrite is false"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn rename_path(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<RenameQuery>,
) -> ApiResult<StatusCode> {
	let session = get_session(&state, id)?;
	let from = resolve_cwd_scoped_path(&session, &query.from).await?;
	let to = resolve_cwd_scoped_path(&session, &query.to).await?;
	match tokio::fs::symlink_metadata(&from).await {
		Ok(_) => {},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return Err(ApiError::NotFound(format!("path not found: {}", query.from)));
		},
		Err(error) => return Err(ApiError::Io(error)),
	}
	if !query.overwrite.unwrap_or(false) && tokio::fs::try_exists(&to).await.map_err(ApiError::Io)? {
		return Err(ApiError::Conflict(format!("destination already exists: {}", query.to)));
	}
	tokio::fs::rename(&from, &to).await.map_err(ApiError::Io)?;
	invalidate_fs_caches(&state, &session, [&from, &to]);
	let _ = session
		.events
		.send(SessionEvent::FileChanged { path: query.to, etag: None });
	Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/stat",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Path relative to the session cwd"),
		("follow_symlinks" = Option<bool>, Query, description = "Follow symlinks before reporting metadata")
	),
	responses(
		(status = 200, body = StatResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "session not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn stat(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<StatQuery>,
) -> ApiResult<Json<StatResponse>> {
	let session = get_session(&state, id)?;
	Ok(Json(stat_path(&session, &query.path, query.follow_symlinks.unwrap_or(false)).await?))
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/exists",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Path relative to the session cwd"),
		("follow_symlinks" = Option<bool>, Query, description = "Follow symlinks before reporting metadata")
	),
	responses(
		(status = 204),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path or session not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn exists(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<StatQuery>,
) -> ApiResult<StatusCode> {
	let session = get_session(&state, id)?;
	if stat_path(&session, &query.path, query.follow_symlinks.unwrap_or(false))
		.await?
		.exists
	{
		Ok(StatusCode::NO_CONTENT)
	} else {
		Err(ApiError::NotFound(query.path))
	}
}

#[utoipa::path(
	post,
	path = "/sessions/{id}/archive.open",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Archive path relative to the session cwd")
	),
	responses(
		(status = 200, body = ArchiveSnapshotOpenResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 413, body = ErrorBody, description = "payload too large"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn archive_open(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ArchivePathQuery>,
) -> ApiResult<Json<ArchiveSnapshotOpenResponse>> {
	let session = get_session(&state, id)?;
	let path = resolve_cwd_scoped_path(&session, &query.path).await?;
	ensure_archive_snapshot_reaper(&state);
	reap_archive_snapshots(&state);
	Ok(Json(open_archive_snapshot(id, path).await?))
}

#[utoipa::path(
	get,
	path = "/archive/{snapshot_id}/entries",
	params(("snapshot_id" = Uuid, Path)),
	responses(
		(status = 200, body = ArchiveEntriesResponse),
		(status = 404, body = ErrorBody, description = "archive snapshot not found"),
	),
)]
pub async fn archive_snapshot_entries(
	State(state): State<AppState>,
	Path(snapshot_id): Path<Uuid>,
) -> ApiResult<Json<ArchiveEntriesResponse>> {
	let snapshot = get_archive_snapshot(&state, snapshot_id)?;
	Ok(Json(ArchiveEntriesResponse {
		entries:   snapshot.entries.clone(),
		format:    snapshot.format.as_str().to_owned(),
		truncated: false,
	}))
}

#[utoipa::path(
	get,
	path = "/archive/{snapshot_id}/entry",
	params(
		("snapshot_id" = Uuid, Path),
		("path" = String, Query, description = "Archive entry path")
	),
	responses(
		(status = 200),
		(status = 206),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "archive snapshot or entry not found"),
	),
)]
pub async fn archive_snapshot_entry(
	State(state): State<AppState>,
	Path(snapshot_id): Path<Uuid>,
	Query(query): Query<ArchiveSnapshotEntryQuery>,
	headers: HeaderMap,
) -> ApiResult<Response> {
	let snapshot = get_archive_snapshot(&state, snapshot_id)?;
	archive_read_response(read_archive_snapshot_entry(&snapshot, &query.path)?, &headers)
}

#[utoipa::path(
	delete,
	path = "/archive/{snapshot_id}",
	params(("snapshot_id" = Uuid, Path)),
	responses((status = 204), (status = 404, body = ErrorBody, description = "archive snapshot not found")),
)]
pub async fn delete_archive_snapshot(Path(snapshot_id): Path<Uuid>) -> ApiResult<StatusCode> {
	ARCHIVE_SNAPSHOTS
		.remove(&snapshot_id)
		.map(|_| StatusCode::NO_CONTENT)
		.ok_or_else(|| ApiError::NotFound(format!("archive snapshot {snapshot_id} not found")))
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/archive.entries",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Archive path relative to the session cwd"),
		("prefix" = Option<String>, Query, description = "Filter entries by path prefix"),
		("limit" = Option<usize>, Query, description = "Maximum number of entries to return (default 10000)")
	),
	responses(
		(status = 200, body = ArchiveEntriesResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 413, body = ErrorBody, description = "payload too large"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn archive_entries(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ArchiveEntriesQuery>,
) -> ApiResult<Json<ArchiveEntriesResponse>> {
	let session = get_session(&state, id)?;
	let path = resolve_cwd_scoped_path(&session, &query.path).await?;
	let limit = query.limit.unwrap_or(10_000);
	Ok(Json(read_archive_entries(path, query.prefix, limit).await?))
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/archive.read",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Archive path relative to the session cwd"),
		("entry" = String, Query, description = "Archive entry path")
	),
	responses(
		(status = 200),
		(status = 206),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 413, body = ErrorBody, description = "payload too large"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn archive_read(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ArchiveReadQuery>,
	headers: HeaderMap,
) -> ApiResult<Response> {
	let session = get_session(&state, id)?;
	let path = resolve_cwd_scoped_path(&session, &query.path).await?;
	let archive = read_archive_entry(path, &query.entry).await?;
	archive_read_response(archive, &headers)
}

#[utoipa::path(
	put,
	path = "/sessions/{id}/archive.write",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Archive path relative to the session cwd"),
		("entry" = String, Query, description = "Archive entry path")
	),
	request_body(content = String, content_type = "application/octet-stream"),
	responses(
		(status = 200, body = ArchiveWriteResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 412, body = ErrorBody, description = "etag mismatch"),
		(status = 413, body = ErrorBody, description = "payload too large"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn archive_write(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ArchiveWriteQuery>,
	headers: HeaderMap,
	body: Bytes,
) -> ApiResult<Json<ArchiveWriteResponse>> {
	let session = get_session(&state, id)?;
	let path = resolve_cwd_scoped_path(&session, &query.path).await?;
	let current = fs_ops::read_file_cached(&session, &path).await?;
	if let Some(if_match) = if_match_header(&headers)
		&& !any_tag_matches(&if_match, &current.etag)
	{
		return Err(ApiError::EtagMismatch);
	}
	let request_path = query.path.clone();
	let entry = query.entry.clone();
	let next_etag = rewrite_archive_entry(path.clone(), &entry, body.to_vec()).await?;
	let _ = session
		.events
		.send(SessionEvent::FileChanged { path: request_path, etag: Some(next_etag.clone()) });
	Ok(Json(ArchiveWriteResponse { etag: next_etag }))
}

#[utoipa::path(
	put,
	path = "/sessions/{id}/archive.bulk_write",
	params(
		("id" = Uuid, Path),
		("path" = String, Query, description = "Archive path relative to the session cwd")
	),
	request_body = ArchiveBulkWriteRequest,
	responses(
		(status = 200, body = ArchiveBulkWriteResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 412, body = ErrorBody, description = "etag mismatch"),
		(status = 413, body = ErrorBody, description = "payload too large"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn archive_bulk_write(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<ArchivePathQuery>,
	headers: HeaderMap,
	Json(body): Json<ArchiveBulkWriteRequest>,
) -> ApiResult<Json<ArchiveBulkWriteResponse>> {
	let session = get_session(&state, id)?;
	let path = resolve_cwd_scoped_path(&session, &query.path).await?;
	let current = fs_ops::read_file_cached(&session, &path).await?;
	if let Some(if_match) = if_match_header(&headers)
		&& !any_tag_matches(&if_match, &current.etag)
	{
		return Err(ApiError::EtagMismatch);
	}
	let request_path = query.path.clone();
	let entries = body
		.entries
		.into_iter()
		.map(|entry| {
			Ok(ArchiveRewriteEntry {
				name:  normalize_archive_entry_path(&entry.name),
				bytes: BASE64_STANDARD.decode(entry.bytes).map_err(|error| {
					ApiError::BadRequest(format!("invalid base64 archive entry: {error}"))
				})?,
			})
		})
		.collect::<ApiResult<Vec<_>>>()?;
	let next = rewrite_archive_entries(path.clone(), entries).await?;
	let _ = session
		.events
		.send(SessionEvent::FileChanged { path: request_path, etag: Some(next.etag.clone()) });
	Ok(Json(ArchiveBulkWriteResponse { etag: next.etag, written: next.written }))
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/glob",
	params(
		("id" = Uuid, Path),
		("patterns" = String, Query, description = "comma-separated glob patterns to match"),
		("paths[]" = Option<Vec<String>>, Query, description = "optional include roots relative to the session cwd"),
		("hidden" = Option<bool>, Query, description = "include hidden files and directories"),
		("limit" = Option<usize>, Query, description = "maximum number of paths to return"),
		("gitignore" = Option<bool>, Query, description = "respect `.gitignore` rules"),
	),
	responses(
		(status = 200),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn glob(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<GlobQuery>,
) -> ApiResult<Response> {
	let session = get_session(&state, id)?;
	let patterns = parse_csv(Some(&query.patterns));
	if patterns.is_empty() {
		return Err(ApiError::BadRequest("`patterns` must contain at least one glob".to_owned()));
	}

	let scoped_paths = parse_csv(query.paths.as_deref());
	let scoped_paths = if scoped_paths.is_empty() {
		vec![String::from(".")]
	} else {
		scoped_paths
	};
	let mut paths = collect_glob_paths(
		&session.cwd(),
		&patterns,
		&scoped_paths,
		query.hidden.unwrap_or(DEFAULT_HIDDEN),
		query.gitignore.unwrap_or(DEFAULT_GITIGNORE),
	)?;
	paths.sort_unstable_by(|left, right| {
		right
			.mtime
			.cmp(&left.mtime)
			.then_with(|| left.path.cmp(&right.path))
	});

	let limit = query.limit.unwrap_or(DEFAULT_GLOB_LIMIT);
	let truncated = if paths.len() > limit {
		paths.truncate(limit);
		true
	} else {
		false
	};

	let mut response = GlobResponse { paths, truncated };
	let mut size_truncated = false;
	let mut body = serde_json::to_vec(&response).map_err(anyhow::Error::from)?;
	while body.len() > GLOB_RESPONSE_MAX_BYTES && !response.paths.is_empty() {
		response.paths.pop();
		response.truncated = true;
		size_truncated = true;
		body = serde_json::to_vec(&response).map_err(anyhow::Error::from)?;
	}

	let mut headers = HeaderMap::new();
	headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
	if size_truncated {
		headers.insert(X_TRUNCATED, HeaderValue::from_static("1"));
	}
	Ok((headers, body).into_response())
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/grep",
	params(
		("id" = Uuid, Path),
		("pattern" = String, Query, description = "regular expression to search for"),
		("paths" = Option<String>, Query, description = "optional comma-separated include paths"),
		("i" = Option<bool>, Query, description = "case-insensitive search"),
		("skip" = Option<usize>, Query, description = "number of matches to skip before streaming results"),
		("gitignore" = Option<bool>, Query, description = "respect `.gitignore` rules"),
		("context" = Option<usize>, Query, description = "context lines to include around each match"),
		("max_matches" = Option<usize>, Query, description = "maximum matches to emit"),
	),
	responses(
		(status = 200, content_type = "application/x-ndjson"),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn grep(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<GrepQuery>,
) -> ApiResult<Response> {
	if query.pattern.is_empty() {
		return Err(ApiError::BadRequest("`pattern` must not be empty".to_owned()));
	}

	let session = get_session(&state, id)?;
	let root = session.cwd();
	let matcher = RegexMatcherBuilder::new()
		.case_insensitive(query.i.unwrap_or(false))
		.build(&query.pattern)
		.map_err(|error| ApiError::BadRequest(format!("invalid regex: {error}")))?;
	let files = collect_grep_files(
		&root,
		&parse_csv(query.paths.as_deref()),
		query.gitignore.unwrap_or(DEFAULT_GITIGNORE),
	)?;
	let context_before = query
		.context_before
		.unwrap_or_else(|| query.context.unwrap_or(DEFAULT_GREP_CONTEXT));
	let context_after = query
		.context_after
		.unwrap_or_else(|| query.context.unwrap_or(DEFAULT_GREP_CONTEXT));
	let skip = query.skip.unwrap_or(0);
	let max_matches = query.max_matches.unwrap_or(DEFAULT_GREP_MAX_MATCHES);
	let session_cancel = session.cancellation_token.child_token();
	let cancel_race_token = session_cancel.clone();
	let response_cancel = session.cancellation_token.child_token();
	let (body_tx, body_rx) = mpsc::channel::<Bytes>(32);

	tokio::task::spawn_blocking(move || {
		let mut remaining_skip = skip;
		let mut emitted_matches = 0_usize;
		let mut limit_reached = false;
		let mut truncated = false;
		'files: for file in files {
			if emitted_matches >= max_matches || session_cancel.is_cancelled() {
				if emitted_matches >= max_matches {
					limit_reached = true;
				}
				break;
			}
			let match_lines = match collect_match_lines(&matcher, &file) {
				Ok(match_lines) => match_lines,
				Err(error) => {
					tracing::warn!(?error, path = %file.display(), "grep scan failed");
					break;
				},
			};
			if match_lines.is_empty() {
				continue;
			}
			let mut selected_lines = Vec::new();
			for line_number in match_lines {
				if remaining_skip > 0 {
					remaining_skip -= 1;
					continue;
				}
				if emitted_matches >= max_matches {
					limit_reached = true;
					break;
				}
				selected_lines.push(line_number);
				emitted_matches += 1;
			}
			if selected_lines.is_empty() {
				continue;
			}
			let lines = match read_text_lines(&file) {
				Ok(lines) => lines,
				Err(error) => {
					tracing::warn!(?error, path = %file.display(), "grep read failed");
					break;
				},
			};
			let display_path = relative_display_path(&root, &file);
			let mut records = Vec::new();
			if let Err(error) = append_grep_records(
				&display_path,
				&lines,
				&selected_lines,
				context_before,
				context_after,
				&mut records,
			) {
				tracing::warn!(?error, path = %file.display(), "grep encode failed");
				break;
			}
			for record in records {
				truncated = truncated
					|| std::str::from_utf8(record.as_ref())
						.is_ok_and(|line| line.contains("\"truncated\":true"));
				if session_cancel.is_cancelled() || body_tx.blocking_send(record).is_err() {
					break 'files;
				}
			}
		}
		if let Ok(summary) = serialize_ndjson(&GrepSummary {
			type_: "summary",
			limit_reached,
			truncated: if truncated { Some(true) } else { None },
		}) {
			let _ = body_tx.blocking_send(summary);
		}
	});

	tokio::select! {
		() = cancel_race_token.cancelled() => return Ok(cancelled_response()),
		() = tokio::time::sleep(std::time::Duration::from_millis(150)) => {}
	}

	let mut headers = HeaderMap::new();
	headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
	let stream = heartbeat_stream(
		ReceiverStream::new(body_rx),
		HEARTBEAT_INTERVAL,
		response_cancel,
		grep_heartbeat_event(),
	);
	Ok((headers, Body::from_stream(stream)).into_response())
}

fn parse_line_selector(raw: &str) -> ApiResult<LineSelector> {
	if raw == "raw" {
		return Ok(LineSelector::Raw);
	}
	if let Ok(line) = raw.parse::<usize>() {
		if line == 0 {
			return Err(ApiError::BadRequest("line selectors are 1-based".to_owned()));
		}
		return Ok(LineSelector::Single(line));
	}
	if let Some((start, end)) = raw.split_once('-') {
		let start = start
			.parse::<usize>()
			.map_err(|_| ApiError::BadRequest(format!("invalid line range {raw:?}")))?;
		let end = end
			.parse::<usize>()
			.map_err(|_| ApiError::BadRequest(format!("invalid line range {raw:?}")))?;
		if start == 0 || end == 0 || start > end {
			return Err(ApiError::BadRequest(format!("invalid line range {raw:?}")));
		}
		return Ok(LineSelector::Range { start, end });
	}
	if let Some((start, len)) = raw.split_once('+') {
		let start = start
			.parse::<usize>()
			.map_err(|_| ApiError::BadRequest(format!("invalid line count selector {raw:?}")))?;
		let len = len
			.parse::<usize>()
			.map_err(|_| ApiError::BadRequest(format!("invalid line count selector {raw:?}")))?;
		if start == 0 || len == 0 {
			return Err(ApiError::BadRequest(format!("invalid line count selector {raw:?}")));
		}
		return Ok(LineSelector::FromStart { start, len });
	}
	Err(ApiError::BadRequest(format!("invalid line selector {raw:?}")))
}

fn select_lines(text: &str, selector: LineSelector) -> String {
	if matches!(selector, LineSelector::Whole | LineSelector::Raw) {
		return text.to_owned();
	}
	let spans = line_spans(text);
	if spans.is_empty() {
		return String::new();
	}
	let (start, end) = match selector {
		LineSelector::Whole | LineSelector::Raw => unreachable!("handled above"),
		LineSelector::Single(line) => (line, line),
		LineSelector::Range { start, end } => (start, end),
		LineSelector::FromStart { start, len } => (start, start.saturating_add(len - 1)),
	};
	if start > spans.len() {
		return String::new();
	}
	let end = end.min(spans.len());
	text[spans[start - 1].0..spans[end - 1].1].to_owned()
}

fn line_spans(text: &str) -> Vec<(usize, usize)> {
	let mut spans = Vec::new();
	let mut start = 0;
	for (index, byte) in text.bytes().enumerate() {
		if byte == b'\n' {
			spans.push((start, index + 1));
			start = index + 1;
		}
	}
	if start < text.len() {
		spans.push((start, text.len()));
	}
	spans
}

fn parse_byte_range(raw: &HeaderValue) -> ApiResult<(usize, usize)> {
	let raw = raw
		.to_str()
		.map_err(|_| ApiError::BadRequest("Range header must be valid ASCII".to_owned()))?;
	let range = raw
		.strip_prefix("bytes=")
		.ok_or_else(|| ApiError::BadRequest(format!("unsupported range unit in {raw:?}")))?;
	let (start, end) = range
		.split_once('-')
		.ok_or_else(|| ApiError::BadRequest(format!("invalid byte range {raw:?}")))?;
	let start = start
		.parse::<usize>()
		.map_err(|_| ApiError::BadRequest(format!("invalid byte range {raw:?}")))?;
	let end = end
		.parse::<usize>()
		.map_err(|_| ApiError::BadRequest(format!("invalid byte range {raw:?}")))?;
	if start > end {
		return Err(ApiError::BadRequest(format!("invalid byte range {raw:?}")));
	}
	Ok((start, end))
}

fn sniff_content_type(path: &FsPath, bytes: &[u8]) -> mime::Mime {
	let sniff_len = bytes.len().min(256);
	infer::get(&bytes[..sniff_len])
		.and_then(|kind| kind.mime_type().parse::<mime::Mime>().ok())
		.unwrap_or_else(|| mime_guess::from_path(path).first_or_octet_stream())
}

fn normalize_ast_source(source: &str) -> String {
	let source = source.strip_prefix('\u{feff}').unwrap_or(source);
	if !source.as_bytes().contains(&b'\r') {
		return source.to_owned();
	}
	let mut normalized = String::with_capacity(source.len());
	let mut chars = source.chars().peekable();
	while let Some(ch) = chars.next() {
		if ch == '\r' {
			if chars.peek() == Some(&'\n') {
				chars.next();
			}
			normalized.push('\n');
		} else {
			normalized.push(ch);
		}
	}
	normalized
}

fn apply_ast_range(source: String, raw_range: Option<&str>) -> ApiResult<String> {
	let Some(raw_range) = raw_range else {
		return Ok(source);
	};
	let Some((start, end)) = raw_range.split_once('-') else {
		return Err(ApiError::BadRequest(format!("invalid AST line range {raw_range:?}")));
	};
	let start = start
		.parse::<isize>()
		.map_err(|_| ApiError::BadRequest(format!("invalid AST line range {raw_range:?}")))?
		.max(1) as usize;
	let end = end
		.parse::<isize>()
		.map_err(|_| ApiError::BadRequest(format!("invalid AST line range {raw_range:?}")))?
		.max(start as isize) as usize;
	let lines = source.split('\n').collect::<Vec<_>>();
	if start > lines.len() {
		return Ok(String::new());
	}
	Ok(lines[start - 1..end.min(lines.len())].join("\n"))
}

const fn native_file_type(file_type: pi_walker::FileType) -> u8 {
	match file_type {
		pi_walker::FileType::File => 1,
		pi_walker::FileType::Dir => 2,
		pi_walker::FileType::Symlink => 3,
	}
}

fn magic_equals(header: &[u8], offset: usize, magic: &[u8]) -> bool {
	header
		.get(offset..offset.saturating_add(magic.len()))
		.is_some_and(|candidate| candidate == magic)
}

fn read_be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
	Some(u16::from_be_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?))
}

fn read_le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
	Some(u16::from_le_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?))
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
	Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn read_le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
	Some(u32::from_le_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn image_metadata(mime_type: &str) -> ImageMetadataResponse {
	ImageMetadataResponse {
		mime_type: mime_type.to_owned(),
		width:     None,
		height:    None,
		channels:  None,
		has_alpha: None,
	}
}

fn parse_image_metadata(header: &[u8]) -> Option<ImageMetadataResponse> {
	parse_png_metadata(header)
		.or_else(|| parse_jpeg_metadata(header))
		.or_else(|| parse_gif_metadata(header))
		.or_else(|| parse_webp_metadata(header))
}

fn parse_png_metadata(header: &[u8]) -> Option<ImageMetadataResponse> {
	if !magic_equals(header, 0, b"\x89PNG\r\n\x1a\n") {
		return None;
	}
	if !magic_equals(header, 12, b"IHDR") {
		return Some(image_metadata("image/png"));
	}
	let Some(width) = read_be_u32(header, 16) else {
		return Some(image_metadata("image/png"));
	};
	let Some(height) = read_be_u32(header, 20) else {
		return Some(image_metadata("image/png"));
	};
	let Some(color_type) = header.get(25).copied() else {
		return Some(image_metadata("image/png"));
	};
	let (channels, has_alpha) = match color_type {
		0 => (Some(1), Some(false)),
		2 => (Some(3), Some(false)),
		3 => (Some(3), None),
		4 => (Some(2), Some(true)),
		6 => (Some(4), Some(true)),
		_ => (None, None),
	};
	Some(ImageMetadataResponse {
		mime_type: "image/png".to_owned(),
		width: Some(width),
		height: Some(height),
		channels,
		has_alpha,
	})
}

fn parse_jpeg_metadata(header: &[u8]) -> Option<ImageMetadataResponse> {
	if !magic_equals(header, 0, b"\xff\xd8\xff") {
		return None;
	}
	let mut offset = 2_usize;
	while offset + 9 < header.len() {
		if header[offset] != 0xff {
			offset += 1;
			continue;
		}
		let mut marker_offset = offset + 1;
		while marker_offset < header.len() && header[marker_offset] == 0xff {
			marker_offset += 1;
		}
		if marker_offset >= header.len() {
			break;
		}
		let marker = header[marker_offset];
		let segment_offset = marker_offset + 1;
		if marker == 0xd8 || marker == 0xd9 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
			offset = segment_offset;
			continue;
		}
		let Some(segment_length) = read_be_u16(header, segment_offset).map(usize::from) else {
			break;
		};
		if segment_length < 2 {
			break;
		}
		let is_start_of_frame =
			(0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc;
		if is_start_of_frame {
			let Some(height) = read_be_u16(header, segment_offset + 3).map(u32::from) else {
				break;
			};
			let Some(width) = read_be_u16(header, segment_offset + 5).map(u32::from) else {
				break;
			};
			let channels = header.get(segment_offset + 7).copied().map(u32::from);
			return Some(ImageMetadataResponse {
				mime_type: "image/jpeg".to_owned(),
				width: Some(width),
				height: Some(height),
				channels,
				has_alpha: Some(false),
			});
		}
		offset = segment_offset.saturating_add(segment_length);
	}
	Some(image_metadata("image/jpeg"))
}

fn parse_gif_metadata(header: &[u8]) -> Option<ImageMetadataResponse> {
	if !magic_equals(header, 0, b"GIF87a") && !magic_equals(header, 0, b"GIF89a") {
		return None;
	}
	let Some(width) = read_le_u16(header, 6).map(u32::from) else {
		return Some(image_metadata("image/gif"));
	};
	let Some(height) = read_le_u16(header, 8).map(u32::from) else {
		return Some(image_metadata("image/gif"));
	};
	Some(ImageMetadataResponse {
		mime_type: "image/gif".to_owned(),
		width:     Some(width),
		height:    Some(height),
		channels:  Some(3),
		has_alpha: None,
	})
}

fn parse_webp_metadata(header: &[u8]) -> Option<ImageMetadataResponse> {
	if !magic_equals(header, 0, b"RIFF") || !magic_equals(header, 8, b"WEBP") {
		return None;
	}
	if magic_equals(header, 12, b"VP8X") {
		let Some(alpha_flags) = header.get(20).copied() else {
			return Some(image_metadata("image/webp"));
		};
		let has_alpha = (alpha_flags & 0x10) != 0;
		let (Some(width0), Some(width1), Some(width2), Some(height0), Some(height1), Some(height2)) = (
			header.get(24).copied(),
			header.get(25).copied(),
			header.get(26).copied(),
			header.get(27).copied(),
			header.get(28).copied(),
			header.get(29).copied(),
		) else {
			return Some(image_metadata("image/webp"));
		};
		let width = u32::from(width0) | (u32::from(width1) << 8) | (u32::from(width2) << 16);
		let height = u32::from(height0) | (u32::from(height1) << 8) | (u32::from(height2) << 16);
		return Some(ImageMetadataResponse {
			mime_type: "image/webp".to_owned(),
			width:     Some(width + 1),
			height:    Some(height + 1),
			channels:  Some(if has_alpha { 4 } else { 3 }),
			has_alpha: Some(has_alpha),
		});
	}
	if magic_equals(header, 12, b"VP8L") {
		let Some(bits) = read_le_u32(header, 21) else {
			return Some(image_metadata("image/webp"));
		};
		let width = (bits & 0x3fff) + 1;
		let height = ((bits >> 14) & 0x3fff) + 1;
		let has_alpha = ((bits >> 28) & 0x1) == 1;
		return Some(ImageMetadataResponse {
			mime_type: "image/webp".to_owned(),
			width:     Some(width),
			height:    Some(height),
			channels:  Some(if has_alpha { 4 } else { 3 }),
			has_alpha: Some(has_alpha),
		});
	}
	if magic_equals(header, 12, b"VP8 ") {
		let Some(width) = read_le_u16(header, 26).map(|value| u32::from(value & 0x3fff)) else {
			return Some(image_metadata("image/webp"));
		};
		let Some(height) = read_le_u16(header, 28).map(|value| u32::from(value & 0x3fff)) else {
			return Some(image_metadata("image/webp"));
		};
		return Some(ImageMetadataResponse {
			mime_type: "image/webp".to_owned(),
			width:     Some(width),
			height:    Some(height),
			channels:  Some(3),
			has_alpha: Some(false),
		});
	}
	Some(image_metadata("image/webp"))
}

fn read_ast_response(summary: SummaryResult) -> ReadAstResponse {
	ReadAstResponse {
		language:    summary.language,
		parsed:      summary.parsed,
		elided:      summary.elided,
		total_lines: summary.total_lines,
		segments:    summary
			.segments
			.into_iter()
			.map(|segment| ReadAstSegment {
				kind:       segment.kind,
				start_line: segment.start_line,
				end_line:   segment.end_line,
				text:       segment.text,
			})
			.collect(),
	}
}

#[derive(Debug, Deserialize)]
pub struct GrepAstQuery {
	pattern:    String,
	paths:      String,
	#[serde(default)]
	language:   Option<String>,
	#[serde(default)]
	strictness: Option<String>,
	#[serde(default)]
	limit:      Option<usize>,
}

#[derive(Debug, Serialize)]
struct GrepAstLine {
	path:       String,
	line:       usize,
	column:     usize,
	end_line:   usize,
	end_column: usize,
	text:       String,
}

#[derive(Debug, Serialize)]
struct GrepAstParseError {
	file:    String,
	message: String,
}

#[derive(Debug, Serialize)]
struct GrepAstSummary {
	#[serde(rename = "type")]
	type_:          &'static str,
	#[serde(rename = "parseErrors")]
	parse_errors:   Vec<GrepAstParseError>,
	#[serde(rename = "filesSearched")]
	files_searched: usize,
	#[serde(rename = "limitReached")]
	limit_reached:  bool,
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/grep.ast",
	params(
		("id" = Uuid, Path),
		("pattern" = String, Query, description = "AST pattern to search for"),
		("paths" = String, Query, description = "comma-separated include paths"),
		("language" = Option<String>, Query, description = "override language detection for all matched files"),
		("strictness" = Option<String>, Query, description = "smart|relaxed|strict"),
		("limit" = Option<usize>, Query, description = "maximum matches to emit"),
	),
	responses(
		(status = 200, content_type = "application/x-ndjson"),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn grep_ast(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Query(query): Query<GrepAstQuery>,
) -> Result<Response, Response> {
	if query.pattern.is_empty() {
		return Err(ApiError::BadRequest("`pattern` must not be empty".to_owned()).into_response());
	}
	let session = get_session(&state, id).map_err(IntoResponse::into_response)?;
	let explicit_language =
		parse_ast_language(query.language.as_deref()).map_err(IntoResponse::into_response)?;
	let strictness = pi_ast::ops::resolve_strictness(
		parse_ast_strictness(query.strictness.as_deref()).map_err(IntoResponse::into_response)?,
	);
	let matched_files =
		ast_ops::collect_matched_files(&session.cwd(), &parse_csv(Some(&query.paths)))
			.map_err(ApiError::Io)
			.map_err(IntoResponse::into_response)?;
	let files_sought = matched_files.len();
	let limit = query.limit.unwrap_or(usize::MAX);
	let session_cancel = session.cancellation_token.child_token();
	let cancel_race_token = session_cancel.clone();
	let response_cancel = session.cancellation_token.child_token();
	let (body_tx, body_rx) = mpsc::channel::<Bytes>(32);

	tokio::spawn(async move {
		let mut parse_errors = Vec::new();
		let mut emitted = 0usize;
		let mut limit_reached = false;
		'files: for file in matched_files {
			if session_cancel.is_cancelled() {
				break;
			}
			let language = explicit_language.or_else(|| SupportLang::from_path(&file.absolute_path));
			let Some(language) = language else {
				continue;
			};
			let pattern = match ast_ops::compile_pattern(&query.pattern, None, &strictness, language) {
				Ok(pattern) => pattern,
				Err(error) => {
					parse_errors.push(parse_grep_ast_error(&file.relative_path, &error.to_string()));
					continue;
				},
			};
			let source = match tokio::fs::read_to_string(&file.absolute_path).await {
				Ok(source) => source,
				Err(error) => {
					parse_errors.push(parse_grep_ast_error(&file.relative_path, &error.to_string()));
					continue;
				},
			};
			for matched in ast_ops::collect_matches(&source, language, std::slice::from_ref(&pattern))
			{
				if emitted >= limit {
					limit_reached = true;
					break 'files;
				}
				let line = match serialize_ndjson(&GrepAstLine {
					path:       file.relative_path.clone(),
					line:       matched.line,
					column:     matched.column,
					end_line:   matched.end_line,
					end_column: matched.end_column,
					text:       matched.text,
				}) {
					Ok(line) => line,
					Err(error) => {
						tracing::warn!(
							?error,
							path = %file.absolute_path.display(),
							"grep.ast record serialization failed"
						);
						break 'files;
					},
				};
				tokio::select! {
					() = session_cancel.cancelled() => break 'files,
					result = body_tx.send(line) => {
						if result.is_err() {
							break 'files;
						}
						emitted = emitted.saturating_add(1);
					}
				}
			}
		}
		if let Ok(line) = serialize_ndjson(&GrepAstSummary {
			type_: "summary",
			parse_errors,
			files_searched: files_sought,
			limit_reached,
		}) {
			let _ = body_tx.send(line).await;
		}
	});

	tokio::select! {
		() = cancel_race_token.cancelled() => return Ok(cancelled_response()),
		() = tokio::time::sleep(std::time::Duration::from_millis(150)) => {}
	}

	let mut headers = HeaderMap::new();
	headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
	let stream = heartbeat_stream(
		ReceiverStream::new(body_rx),
		HEARTBEAT_INTERVAL,
		response_cancel,
		grep_heartbeat_event(),
	);
	Ok((headers, Body::from_stream(stream)).into_response())
}

fn parse_ast_language(raw: Option<&str>) -> ApiResult<Option<SupportLang>> {
	raw.map(|language| {
		SupportLang::from_alias(language)
			.ok_or_else(|| ApiError::BadRequest(format!("unsupported ast language `{language}`")))
	})
	.transpose()
}

fn parse_ast_strictness(raw: Option<&str>) -> ApiResult<Option<pi_ast::ops::AstMatchStrictness>> {
	match raw {
		None | Some("smart") => Ok(None),
		Some("relaxed") => Ok(Some(pi_ast::ops::AstMatchStrictness::Relaxed)),
		Some("strict") => Ok(Some(pi_ast::ops::AstMatchStrictness::Ast)),
		Some(other) => Err(ApiError::BadRequest(format!("unsupported ast strictness `{other}`"))),
	}
}

fn parse_grep_ast_error(file: &str, message: &str) -> GrepAstParseError {
	GrepAstParseError { file: file.to_owned(), message: message.to_owned() }
}
fn cancelled_response() -> Response {
	let mut response = Response::new(Body::empty());
	*response.status_mut() = StatusCode::from_u16(499).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
	response
}

const fn grep_heartbeat_event() -> Bytes {
	Bytes::from_static(b"{\"type\":\"heartbeat\"}\n")
}

fn invalidate_fs_caches<I, P>(state: &AppState, session: &Session, paths: I)
where
	I: IntoIterator<Item = P>,
	P: AsRef<FsPath>,
{
	for path in paths {
		let path = path.as_ref();
		state.etag_cache.invalidate(path);
		session.read_cache.invalidate(path);
	}
}

fn get_session(state: &AppState, id: Uuid) -> ApiResult<Arc<crate::session::Session>> {
	state
		.sessions
		.get(id)
		.ok_or_else(|| ApiError::NotFound(format!("session {id} not found")))
}

fn current_epoch_secs() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|duration| duration.as_secs())
		.unwrap_or_default()
}

fn ensure_archive_snapshot_reaper(state: &AppState) {
	if ARCHIVE_SNAPSHOT_REAPER_STARTED
		.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
		.is_ok()
	{
		let state = state.clone();
		tokio::spawn(async move {
			loop {
				sleep(ARCHIVE_SNAPSHOT_REAPER_INTERVAL).await;
				reap_archive_snapshots(&state);
			}
		});
	}
}

fn reap_archive_snapshots(state: &AppState) {
	let now = current_epoch_secs();
	let expired = ARCHIVE_SNAPSHOTS
		.iter()
		.filter_map(|entry| {
			let snapshot = entry.value();
			if snapshot.is_expired(now) || state.sessions.get(snapshot.session_id).is_none() {
				Some(*entry.key())
			} else {
				None
			}
		})
		.collect::<Vec<_>>();
	for snapshot_id in expired {
		ARCHIVE_SNAPSHOTS.remove(&snapshot_id);
	}
}

fn get_archive_snapshot(
	state: &AppState,
	snapshot_id: Uuid,
) -> ApiResult<Arc<ArchiveSnapshotState>> {
	let snapshot = ARCHIVE_SNAPSHOTS
		.get(&snapshot_id)
		.map(|entry| Arc::clone(entry.value()))
		.ok_or_else(|| ApiError::NotFound(format!("archive snapshot {snapshot_id} not found")))?;
	if state.sessions.get(snapshot.session_id).is_none() {
		ARCHIVE_SNAPSHOTS.remove(&snapshot_id);
		return Err(ApiError::NotFound(format!("archive snapshot {snapshot_id} not found")));
	}
	snapshot.touch();
	Ok(snapshot)
}

fn if_match_header(headers: &HeaderMap) -> Option<String> {
	headers
		.get(header::IF_MATCH)
		.and_then(|value| value.to_str().ok())
		.map(str::to_owned)
}

fn etag_response(etag: String) -> Response {
	(StatusCode::NO_CONTENT, [(header::ETAG, format!("\"{etag}\""))]).into_response()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
	Zip,
	Tar,
	TarGz,
}

#[derive(Debug)]
struct ArchiveReadResult {
	bytes: Bytes,
	path:  String,
}

#[derive(Debug, Clone)]
struct ArchiveRewriteEntry {
	name:  String,
	bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ArchiveRewriteOutcome {
	etag:    String,
	written: usize,
}

#[derive(Debug)]
struct ArchiveSnapshotPayload {
	format:  ArchiveFormat,
	entries: Vec<ArchiveEntry>,
	files:   BTreeMap<String, Bytes>,
}

#[derive(Debug)]
struct ArchiveSnapshotState {
	session_id:             Uuid,
	format:                 ArchiveFormat,
	entries:                Vec<ArchiveEntry>,
	files:                  BTreeMap<String, Bytes>,
	last_access_epoch_secs: AtomicU64,
}

impl ArchiveSnapshotState {
	fn new(session_id: Uuid, payload: ArchiveSnapshotPayload) -> Self {
		Self {
			session_id,
			format: payload.format,
			entries: payload.entries,
			files: payload.files,
			last_access_epoch_secs: AtomicU64::new(current_epoch_secs()),
		}
	}

	fn touch(&self) {
		self
			.last_access_epoch_secs
			.store(current_epoch_secs(), Ordering::Relaxed);
	}

	fn is_expired(&self, now: u64) -> bool {
		now.saturating_sub(self.last_access_epoch_secs.load(Ordering::Relaxed))
			>= ARCHIVE_SNAPSHOT_IDLE_SECS
	}
}

impl ArchiveFormat {
	const fn as_str(self) -> &'static str {
		match self {
			Self::Zip => "zip",
			Self::Tar => "tar",
			Self::TarGz => "tar.gz",
		}
	}

	fn detect(path: &FsPath) -> ApiResult<Self> {
		if path.file_name().is_some_and(|name| {
			name
				.to_string_lossy()
				.to_ascii_lowercase()
				.ends_with(".tar.gz")
		}) || path.file_name().is_some_and(|name| {
			name
				.to_string_lossy()
				.to_ascii_lowercase()
				.ends_with(".tgz")
		}) {
			Ok(Self::TarGz)
		} else if path
			.extension()
			.is_some_and(|ext| ext.eq_ignore_ascii_case("tar"))
		{
			Ok(Self::Tar)
		} else if path
			.extension()
			.is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
		{
			Ok(Self::Zip)
		} else {
			Err(ApiError::BadRequest(format!("unsupported archive format for {}", path.display())))
		}
	}
}

async fn stat_path(
	session: &Session,
	raw_path: &str,
	follow_symlinks: bool,
) -> ApiResult<StatResponse> {
	let path = resolve_cwd_scoped_path(session, raw_path).await?;
	let link_metadata = match tokio::fs::symlink_metadata(&path).await {
		Ok(metadata) => metadata,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return Ok(StatResponse {
				exists:    false,
				kind:      "other".to_owned(),
				size:      0,
				mtime_ms:  0,
				link_kind: None,
				etag:      None,
			});
		},
		Err(error) => return Err(ApiError::Io(error)),
	};
	let was_symlink = link_metadata.file_type().is_symlink();
	let metadata = if was_symlink && follow_symlinks {
		tokio::fs::metadata(&path).await.map_err(ApiError::Io)?
	} else {
		link_metadata
	};
	let file_type = metadata.file_type();
	let kind = if file_type.is_file() {
		"file"
	} else if file_type.is_dir() {
		"dir"
	} else if was_symlink && !follow_symlinks {
		"symlink"
	} else {
		"other"
	};
	let etag = if file_type.is_file() {
		Some(
			fs_ops::read_file_cached(session, &path)
				.await?
				.etag
				.to_string(),
		)
	} else {
		None
	};
	Ok(StatResponse {
		exists: true,
		kind: kind.to_owned(),
		size: metadata.len(),
		mtime_ms: modified_time_millis(&metadata)?,
		link_kind: was_symlink.then_some("symlink".to_owned()),
		etag,
	})
}

async fn resolve_cwd_scoped_path(session: &Session, raw_path: &str) -> ApiResult<PathBuf> {
	if raw_path.is_empty() {
		return Err(ApiError::BadRequest("path is required".to_owned()));
	}
	let root = tokio::fs::canonicalize(session.cwd())
		.await
		.map_err(ApiError::Io)?;
	let candidate = normalize_absolute_path(if raw_path.starts_with("file://") {
		Url::parse(raw_path)
			.map_err(|error| ApiError::BadRequest(format!("invalid file URL {raw_path:?}: {error}")))?
			.to_file_path()
			.map_err(|()| {
				ApiError::BadRequest(format!("file URL does not map to a local path: {raw_path:?}"))
			})?
	} else {
		let path = PathBuf::from(raw_path);
		if path.is_absolute() {
			path
		} else {
			root.join(path)
		}
	});
	let resolved = resolve_against_root(&root, &candidate).await?;
	if !resolved.starts_with(&root) {
		return Err(ApiError::BadRequest(format!("path escapes session cwd: {raw_path}")));
	}
	Ok(resolved)
}

fn normalize_absolute_path(path: PathBuf) -> PathBuf {
	let mut normalized = PathBuf::new();
	for component in path.components() {
		match component {
			Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
			Component::RootDir => normalized.push(FsPath::new(std::path::MAIN_SEPARATOR_STR)),
			Component::CurDir => {},
			Component::ParentDir => {
				let _ = normalized.pop();
			},
			Component::Normal(part) => normalized.push(part),
		}
	}
	normalized
}

async fn resolve_against_root(root: &FsPath, candidate: &FsPath) -> ApiResult<PathBuf> {
	let mut current = candidate.to_path_buf();
	let mut suffix = Vec::<OsString>::new();
	loop {
		match tokio::fs::symlink_metadata(&current).await {
			Ok(metadata) if metadata.is_dir() => {
				let canonical = tokio::fs::canonicalize(&current)
					.await
					.map_err(ApiError::Io)?;
				let mut resolved = canonical;
				for part in suffix.iter().rev() {
					resolved.push(part);
				}
				return Ok(resolved);
			},
			Ok(_) => {
				let Some(name) = current.file_name() else {
					break;
				};
				suffix.push(name.to_os_string());
				if !current.pop() {
					break;
				}
			},
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
				let Some(name) = current.file_name() else {
					break;
				};
				suffix.push(name.to_os_string());
				if !current.pop() {
					break;
				}
			},
			Err(error) => return Err(ApiError::Io(error)),
		}
	}
	if candidate == root {
		return Ok(root.to_path_buf());
	}
	Err(ApiError::BadRequest(format!("path escapes session cwd: {}", candidate.display())))
}

fn query_flag_enabled(raw: Option<&str>) -> bool {
	raw.is_some_and(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "on"))
}

fn archive_read_response(archive: ArchiveReadResult, headers: &HeaderMap) -> ApiResult<Response> {
	let etag = fs_ops::compute_etag_hex(&archive.bytes);
	let content_type = sniff_content_type(FsPath::new(&archive.path), &archive.bytes).to_string();
	let total_len = archive.bytes.len();
	let range = headers
		.get(header::RANGE)
		.map(parse_byte_range)
		.transpose()?;
	let (status, response_bytes, content_range) = if let Some((start, end)) = range {
		let end = end.min(total_len.saturating_sub(1));
		if start >= total_len || start > end {
			return Err(ApiError::BadRequest(format!(
				"byte range {start}-{end} is out of bounds for {total_len} bytes"
			)));
		}
		let end_exclusive = end.saturating_add(1);
		(
			StatusCode::PARTIAL_CONTENT,
			archive.bytes.slice(start..end_exclusive),
			Some(format!("bytes {start}-{end}/{total_len}")),
		)
	} else {
		(StatusCode::OK, archive.bytes, None)
	};
	let mut response = Response::new(Body::from(response_bytes));
	*response.status_mut() = status;
	response
		.headers_mut()
		.insert(CONTENT_TYPE, HeaderValue::from_str(&content_type).map_err(anyhow::Error::from)?);
	response
		.headers_mut()
		.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
	response.headers_mut().insert(
		header::ETAG,
		HeaderValue::from_str(&format!("\"{etag}\"")).map_err(anyhow::Error::from)?,
	);
	if let Some(content_range) = content_range {
		response.headers_mut().insert(
			header::CONTENT_RANGE,
			HeaderValue::from_str(&content_range).map_err(anyhow::Error::from)?,
		);
	}
	Ok(response)
}

async fn open_archive_snapshot(
	session_id: Uuid,
	path: PathBuf,
) -> ApiResult<ArchiveSnapshotOpenResponse> {
	let payload = load_archive_snapshot(path).await?;
	let snapshot_id = Uuid::new_v4();
	let format = payload.format.as_str().to_owned();
	ARCHIVE_SNAPSHOTS.insert(snapshot_id, Arc::new(ArchiveSnapshotState::new(session_id, payload)));
	Ok(ArchiveSnapshotOpenResponse { snapshot_id, format })
}

async fn load_archive_snapshot(path: PathBuf) -> ApiResult<ArchiveSnapshotPayload> {
	tokio::task::spawn_blocking(move || load_archive_snapshot_blocking(&path))
		.await
		.map_err(|error| ApiError::Internal(error.into()))?
}

fn load_archive_snapshot_blocking(path: &FsPath) -> ApiResult<ArchiveSnapshotPayload> {
	match ArchiveFormat::detect(path)? {
		ArchiveFormat::Zip => load_zip_archive_snapshot(path),
		ArchiveFormat::Tar => load_tar_archive_snapshot(path, false),
		ArchiveFormat::TarGz => load_tar_archive_snapshot(path, true),
	}
}

fn load_zip_archive_snapshot(path: &FsPath) -> ApiResult<ArchiveSnapshotPayload> {
	let metadata = std::fs::metadata(path)?;
	if metadata.len() > MAX_ZIP_ARCHIVE_BYTES {
		return Err(ApiError::PayloadTooLarge(format!(
			"zip archive exceeds {} bytes: {}",
			MAX_ZIP_ARCHIVE_BYTES,
			path.display()
		)));
	}
	let file = std::fs::File::open(path)?;
	let mut archive =
		zip::ZipArchive::new(file).map_err(|error| ApiError::BadRequest(error.to_string()))?;
	let mut entries = Vec::with_capacity(archive.len());
	let mut files = BTreeMap::new();
	for index in 0..archive.len() {
		let mut file = archive
			.by_index(index)
			.map_err(|error| ApiError::BadRequest(error.to_string()))?;
		let name = normalize_archive_entry_path(file.name());
		let is_dir = file.is_dir();
		entries.push(ArchiveEntry {
			path:            name.clone(),
			kind:            if is_dir { "dir" } else { "file" }.to_owned(),
			size:            file.size(),
			mtime_ms:        None,
			compressed_size: Some(file.compressed_size()),
		});
		if is_dir {
			continue;
		}
		let mut bytes = Vec::new();
		file.read_to_end(&mut bytes)?;
		files.insert(name, Bytes::from(bytes));
	}
	Ok(ArchiveSnapshotPayload { format: ArchiveFormat::Zip, entries, files })
}

fn load_tar_archive_snapshot(path: &FsPath, gzip: bool) -> ApiResult<ArchiveSnapshotPayload> {
	let file = std::fs::File::open(path)?;
	let mut archive = if gzip {
		tar::Archive::new(Box::new(flate2::read::GzDecoder::new(file)) as Box<dyn Read>)
	} else {
		tar::Archive::new(Box::new(file) as Box<dyn Read>)
	};
	let format = if gzip {
		ArchiveFormat::TarGz
	} else {
		ArchiveFormat::Tar
	};
	let mut entries = Vec::new();
	let mut files = BTreeMap::new();
	for item in archive.entries()? {
		let mut entry = item?;
		let path = normalize_archive_entry_path(&entry.path()?.to_string_lossy());
		let header = entry.header();
		let mtime_ms = header
			.mtime()
			.ok()
			.and_then(|mtime| i64::try_from(mtime).ok())
			.and_then(|mtime| mtime.checked_mul(1_000));
		let is_dir = header.entry_type().is_dir();
		entries.push(ArchiveEntry {
			path: path.clone(),
			kind: if is_dir { "dir" } else { "file" }.to_owned(),
			size: header.size().unwrap_or(0),
			mtime_ms,
			compressed_size: None,
		});
		if is_dir {
			continue;
		}
		let mut bytes = Vec::new();
		entry.read_to_end(&mut bytes)?;
		files.insert(path, Bytes::from(bytes));
	}
	Ok(ArchiveSnapshotPayload { format, entries, files })
}

fn read_archive_snapshot_entry(
	snapshot: &ArchiveSnapshotState,
	entry: &str,
) -> ApiResult<ArchiveReadResult> {
	let name = normalize_archive_entry_path(entry);
	if let Some(bytes) = snapshot.files.get(&name) {
		return Ok(ArchiveReadResult { bytes: bytes.clone(), path: name });
	}
	if snapshot
		.entries
		.iter()
		.any(|candidate| candidate.path == name && candidate.kind == "dir")
	{
		return Err(ApiError::BadRequest(format!("archive entry is a directory: {name}")));
	}
	Err(ApiError::NotFound(format!("archive entry not found: {name}")))
}

async fn read_archive_entries(
	path: PathBuf,
	prefix: Option<String>,
	limit: usize,
) -> ApiResult<ArchiveEntriesResponse> {
	tokio::task::spawn_blocking(move || {
		read_archive_entries_blocking(&path, prefix.as_deref(), limit)
	})
	.await
	.map_err(|error| ApiError::Internal(error.into()))?
}

fn read_archive_entries_blocking(
	path: &FsPath,
	prefix: Option<&str>,
	limit: usize,
) -> ApiResult<ArchiveEntriesResponse> {
	let format = ArchiveFormat::detect(path)?;
	let prefix = prefix.map(normalize_archive_entry_path);
	let mut entries = match format {
		ArchiveFormat::Zip => read_zip_entries(path)?,
		ArchiveFormat::Tar => read_tar_entries(path, false)?,
		ArchiveFormat::TarGz => read_tar_entries(path, true)?,
	};
	if let Some(prefix) = prefix.as_deref() {
		entries.retain(|entry| entry.path.starts_with(prefix));
	}
	entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
	let truncated = entries.len() > limit;
	if truncated {
		entries.truncate(limit);
	}
	Ok(ArchiveEntriesResponse { entries, format: format.as_str().to_owned(), truncated })
}

const MAX_ZIP_ARCHIVE_BYTES: u64 = 100 * 1024 * 1024;

fn read_zip_entries(path: &FsPath) -> ApiResult<Vec<ArchiveEntry>> {
	let metadata = std::fs::metadata(path)?;
	if metadata.len() > MAX_ZIP_ARCHIVE_BYTES {
		return Err(ApiError::PayloadTooLarge(format!(
			"zip archive exceeds {} bytes: {}",
			MAX_ZIP_ARCHIVE_BYTES,
			path.display()
		)));
	}
	let file = std::fs::File::open(path)?;
	let mut archive =
		zip::ZipArchive::new(file).map_err(|error| ApiError::BadRequest(error.to_string()))?;
	let mut entries = Vec::with_capacity(archive.len());
	for index in 0..archive.len() {
		let file = archive
			.by_index(index)
			.map_err(|error| ApiError::BadRequest(error.to_string()))?;
		entries.push(ArchiveEntry {
			path:            normalize_archive_entry_path(file.name()),
			kind:            if file.is_dir() { "dir" } else { "file" }.to_owned(),
			size:            file.size(),
			mtime_ms:        None,
			compressed_size: Some(file.compressed_size()),
		});
	}
	Ok(entries)
}

fn read_tar_entries(path: &FsPath, gzip: bool) -> ApiResult<Vec<ArchiveEntry>> {
	let file = std::fs::File::open(path)?;
	let mut archive = if gzip {
		tar::Archive::new(Box::new(flate2::read::GzDecoder::new(file)) as Box<dyn Read>)
	} else {
		tar::Archive::new(Box::new(file) as Box<dyn Read>)
	};
	let mut entries = Vec::new();
	for item in archive.entries()? {
		let entry = item?;
		let path = normalize_archive_entry_path(&entry.path()?.to_string_lossy());
		let header = entry.header();
		let mtime_ms = header
			.mtime()
			.ok()
			.and_then(|mtime| i64::try_from(mtime).ok())
			.and_then(|mtime| mtime.checked_mul(1_000));
		entries.push(ArchiveEntry {
			path,
			kind: if header.entry_type().is_dir() {
				"dir"
			} else {
				"file"
			}
			.to_owned(),
			size: header.size().unwrap_or(0),
			mtime_ms,
			compressed_size: None,
		});
	}
	Ok(entries)
}

async fn read_archive_entry(path: PathBuf, entry: &str) -> ApiResult<ArchiveReadResult> {
	let entry = normalize_archive_entry_path(entry);
	tokio::task::spawn_blocking(move || read_archive_entry_blocking(&path, &entry))
		.await
		.map_err(|error| ApiError::Internal(error.into()))?
}

fn read_archive_entry_blocking(path: &FsPath, entry: &str) -> ApiResult<ArchiveReadResult> {
	match ArchiveFormat::detect(path)? {
		ArchiveFormat::Zip => read_zip_entry(path, entry),
		ArchiveFormat::Tar => read_tar_entry(path, entry, false),
		ArchiveFormat::TarGz => read_tar_entry(path, entry, true),
	}
}

fn read_zip_entry(path: &FsPath, entry: &str) -> ApiResult<ArchiveReadResult> {
	let metadata = std::fs::metadata(path)?;
	if metadata.len() > MAX_ZIP_ARCHIVE_BYTES {
		return Err(ApiError::PayloadTooLarge(format!(
			"zip archive exceeds {} bytes: {}",
			MAX_ZIP_ARCHIVE_BYTES,
			path.display()
		)));
	}
	let file = std::fs::File::open(path)?;
	let mut archive =
		zip::ZipArchive::new(file).map_err(|error| ApiError::BadRequest(error.to_string()))?;
	for index in 0..archive.len() {
		let mut file = archive
			.by_index(index)
			.map_err(|error| ApiError::BadRequest(error.to_string()))?;
		let name = normalize_archive_entry_path(file.name());
		if name != entry {
			continue;
		}
		if file.is_dir() {
			return Err(ApiError::BadRequest(format!("archive entry is a directory: {entry}")));
		}
		let mut bytes = Vec::new();
		file.read_to_end(&mut bytes)?;
		return Ok(ArchiveReadResult { bytes: Bytes::from(bytes), path: name });
	}
	Err(ApiError::NotFound(format!("archive entry not found: {entry}")))
}

fn read_tar_entry(path: &FsPath, entry: &str, gzip: bool) -> ApiResult<ArchiveReadResult> {
	let file = std::fs::File::open(path)?;
	let mut archive = if gzip {
		tar::Archive::new(Box::new(flate2::read::GzDecoder::new(file)) as Box<dyn Read>)
	} else {
		tar::Archive::new(Box::new(file) as Box<dyn Read>)
	};
	for item in archive.entries()? {
		let mut file = item?;
		let name = normalize_archive_entry_path(&file.path()?.to_string_lossy());
		if name != entry {
			continue;
		}
		if file.header().entry_type().is_dir() {
			return Err(ApiError::BadRequest(format!("archive entry is a directory: {entry}")));
		}
		let mut bytes = Vec::new();
		file.read_to_end(&mut bytes)?;
		return Ok(ArchiveReadResult { bytes: Bytes::from(bytes), path: name });
	}
	Err(ApiError::NotFound(format!("archive entry not found: {entry}")))
}

async fn rewrite_archive_entry(path: PathBuf, entry: &str, body: Vec<u8>) -> ApiResult<String> {
	let outcome = rewrite_archive_entries(path, vec![ArchiveRewriteEntry {
		name:  normalize_archive_entry_path(entry),
		bytes: body,
	}])
	.await?;
	Ok(outcome.etag)
}

async fn rewrite_archive_entries(
	path: PathBuf,
	entries: Vec<ArchiveRewriteEntry>,
) -> ApiResult<ArchiveRewriteOutcome> {
	let request_id = Uuid::new_v4().to_string();
	tokio::task::spawn_blocking(move || {
		rewrite_archive_entries_blocking(&path, &entries, &request_id)
	})
	.await
	.map_err(|error| ApiError::Internal(error.into()))?
}

fn rewrite_archive_entries_blocking(
	path: &FsPath,
	entries: &[ArchiveRewriteEntry],
	request_id: &str,
) -> ApiResult<ArchiveRewriteOutcome> {
	let format = ArchiveFormat::detect(path)?;
	let next_bytes = match format {
		ArchiveFormat::Zip => rebuild_zip_archive(path, entries)?,
		ArchiveFormat::Tar => rebuild_tar_archive(path, entries, false)?,
		ArchiveFormat::TarGz => rebuild_tar_archive(path, entries, true)?,
	};
	let etag = fs_ops::compute_etag_hex(&next_bytes);
	let temp_path = path.with_file_name(format!(
		".{}.{}.tmp",
		path
			.file_name()
			.and_then(|value| value.to_str())
			.unwrap_or("archive"),
		request_id
	));
	std::fs::write(&temp_path, &next_bytes)?;
	std::fs::rename(&temp_path, path)?;
	Ok(ArchiveRewriteOutcome { etag, written: entries.iter().map(|entry| entry.bytes.len()).sum() })
}

fn rebuild_zip_archive(path: &FsPath, entries: &[ArchiveRewriteEntry]) -> ApiResult<Vec<u8>> {
	let metadata = std::fs::metadata(path)?;
	if metadata.len() > MAX_ZIP_ARCHIVE_BYTES {
		return Err(ApiError::PayloadTooLarge(format!(
			"zip archive exceeds {} bytes: {}",
			MAX_ZIP_ARCHIVE_BYTES,
			path.display()
		)));
	}
	let mut replacements = BTreeMap::new();
	for entry in entries {
		replacements.insert(entry.name.clone(), entry.bytes.as_slice());
	}
	let file = std::fs::File::open(path)?;
	let mut archive =
		zip::ZipArchive::new(file).map_err(|error| ApiError::BadRequest(error.to_string()))?;
	let cursor = Cursor::new(Vec::new());
	let mut writer = zip::ZipWriter::new(cursor);
	for index in 0..archive.len() {
		let mut file = archive
			.by_index(index)
			.map_err(|error| ApiError::BadRequest(error.to_string()))?;
		let name = normalize_archive_entry_path(file.name());
		let options = zip::write::SimpleFileOptions::default();
		if let Some(body) = replacements.remove(&name) {
			writer
				.start_file(&name, options)
				.map_err(|error| ApiError::BadRequest(error.to_string()))?;
			std::io::Write::write_all(&mut writer, body)?;
			continue;
		}
		if file.is_dir() {
			writer
				.add_directory(&name, options)
				.map_err(|error| ApiError::BadRequest(error.to_string()))?;
			continue;
		}
		writer
			.start_file(&name, options)
			.map_err(|error| ApiError::BadRequest(error.to_string()))?;
		std::io::copy(&mut file, &mut writer)?;
	}
	for (name, body) in replacements {
		writer
			.start_file(&name, zip::write::SimpleFileOptions::default())
			.map_err(|error| ApiError::BadRequest(error.to_string()))?;
		std::io::Write::write_all(&mut writer, body)?;
	}
	let cursor = writer
		.finish()
		.map_err(|error| ApiError::BadRequest(error.to_string()))?;
	Ok(cursor.into_inner())
}

fn rebuild_tar_archive(
	path: &FsPath,
	entries: &[ArchiveRewriteEntry],
	gzip: bool,
) -> ApiResult<Vec<u8>> {
	let mut replacements = BTreeMap::new();
	for entry in entries {
		replacements.insert(entry.name.clone(), entry.bytes.as_slice());
	}
	let source = std::fs::File::open(path)?;
	let source_reader: Box<dyn Read> = if gzip {
		Box::new(flate2::read::GzDecoder::new(source))
	} else {
		Box::new(source)
	};
	let mut archive = tar::Archive::new(source_reader);
	let mut out = Vec::new();
	if gzip {
		let encoder = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
		let mut builder = tar::Builder::new(encoder);
		copy_tar_entries(&mut archive, &mut builder, &mut replacements)?;
		builder.finish()?;
	} else {
		let mut builder = tar::Builder::new(&mut out);
		copy_tar_entries(&mut archive, &mut builder, &mut replacements)?;
		builder.finish()?;
	}
	Ok(out)
}

fn copy_tar_entries<W: std::io::Write>(
	archive: &mut tar::Archive<Box<dyn Read>>,
	builder: &mut tar::Builder<W>,
	replacements: &mut BTreeMap<String, &[u8]>,
) -> ApiResult<()> {
	for item in archive.entries()? {
		let mut file = item?;
		let name = normalize_archive_entry_path(&file.path()?.to_string_lossy());
		if let Some(body) = replacements.remove(&name) {
			append_tar_file(builder, &name, body)?;
			continue;
		}
		if file.header().entry_type().is_dir() {
			builder.append(&file.header().clone(), std::io::empty())?;
			continue;
		}
		let mut header = file.header().clone();
		builder.append_data(&mut header, &name, &mut file)?;
	}
	for (name, body) in replacements.iter() {
		append_tar_file(builder, name, body)?;
	}
	Ok(())
}

fn append_tar_file<W: std::io::Write>(
	builder: &mut tar::Builder<W>,
	path: &str,
	body: &[u8],
) -> ApiResult<()> {
	let mut header = tar::Header::new_gnu();
	header.set_size(u64::try_from(body.len()).map_err(|_| anyhow::anyhow!("tar entry too large"))?);
	header.set_mode(0o644);
	let now = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_err(anyhow::Error::from)?;
	header.set_mtime(now.as_secs());
	header.set_cksum();
	builder.append_data(&mut header, path, Cursor::new(body))?;
	Ok(())
}

fn normalize_archive_entry_path(raw: &str) -> String {
	raw.replace('\\', "/")
		.trim_start_matches("./")
		.trim_end_matches('/')
		.to_owned()
}

fn any_tag_matches(raw: &str, etag: &str) -> bool {
	raw.split(',').any(|candidate| {
		let trimmed = candidate.trim();
		let strong = trimmed.strip_prefix("W/").unwrap_or(trimmed);
		strong == "*" || strong.trim_matches('"') == etag
	})
}

fn parse_csv(raw: Option<&str>) -> Vec<String> {
	raw.into_iter()
		.flat_map(|value| value.split(','))
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.map(ToOwned::to_owned)
		.collect()
}

fn collect_glob_paths(
	root: &FsPath,
	patterns: &[String],
	raw_paths: &[String],
	include_hidden: bool,
	gitignore: bool,
) -> ApiResult<Vec<GlobPathEntry>> {
	let globset = build_globset(patterns)?;
	let mut results = Vec::new();
	let mut seen = HashSet::new();
	for raw_path in raw_paths {
		let resolved = resolve_session_path(root, raw_path);
		if resolved.is_file() {
			let match_root = resolved.parent().unwrap_or(root);
			append_glob_match(root, match_root, &resolved, &globset, &mut results, &mut seen)?;
			continue;
		}
		if !resolved.is_dir() {
			continue;
		}
		append_glob_matches(
			root,
			&resolved,
			&globset,
			include_hidden,
			gitignore,
			&mut results,
			&mut seen,
		)?;
	}
	Ok(results)
}

fn append_glob_matches(
	session_root: &FsPath,
	scope_root: &FsPath,
	globset: &GlobSet,
	include_hidden: bool,
	gitignore: bool,
	results: &mut Vec<GlobPathEntry>,
	seen: &mut HashSet<String>,
) -> ApiResult<()> {
	let mut builder = WalkBuilder::new(scope_root);
	configure_walk(&mut builder, include_hidden, gitignore);
	for entry in builder.build() {
		let entry = entry.map_err(anyhow::Error::from)?;
		if !entry
			.file_type()
			.is_some_and(|file_type| file_type.is_file())
		{
			continue;
		}
		append_glob_match(session_root, scope_root, entry.path(), globset, results, seen)?;
	}
	Ok(())
}

fn append_glob_match(
	session_root: &FsPath,
	scope_root: &FsPath,
	file_path: &FsPath,
	globset: &GlobSet,
	results: &mut Vec<GlobPathEntry>,
	seen: &mut HashSet<String>,
) -> ApiResult<()> {
	let relative_path = file_path.strip_prefix(scope_root).unwrap_or(file_path);
	if !globset.is_match(relative_path) {
		return Ok(());
	}
	let display_path = relative_display_path(session_root, file_path);
	if !seen.insert(display_path.clone()) {
		return Ok(());
	}
	let metadata = file_path.metadata().map_err(anyhow::Error::from)?;
	results.push(GlobPathEntry {
		path:  display_path,
		mtime: modified_time_millis(&metadata)?,
		size:  metadata.len(),
	});
	Ok(())
}

fn build_globset(patterns: &[String]) -> ApiResult<GlobSet> {
	let mut builder = GlobSetBuilder::new();
	for pattern in patterns {
		let glob = Glob::new(pattern)
			.map_err(|error| ApiError::BadRequest(format!("invalid glob `{pattern}`: {error}")))?;
		builder.add(glob);
	}
	builder
		.build()
		.map_err(|error| ApiError::BadRequest(format!("invalid glob set: {error}")))
}

fn configure_walk(builder: &mut WalkBuilder, include_hidden: bool, gitignore: bool) {
	builder
		.hidden(!include_hidden)
		.parents(gitignore)
		.ignore(gitignore)
		.git_ignore(gitignore)
		.git_global(gitignore)
		.git_exclude(gitignore)
		.require_git(false);
}

fn modified_time_millis(metadata: &std::fs::Metadata) -> ApiResult<i64> {
	let duration = metadata
		.modified()?
		.duration_since(UNIX_EPOCH)
		.map_err(anyhow::Error::from)?;
	i64::try_from(duration.as_millis()).map_err(|_| anyhow::anyhow!("mtime overflow").into())
}

fn collect_grep_files(
	root: &FsPath,
	raw_paths: &[String],
	gitignore: bool,
) -> ApiResult<Vec<PathBuf>> {
	let mut files = Vec::new();
	let mut seen = HashSet::new();
	if raw_paths.is_empty() {
		collect_files_under(root, gitignore, &mut files, &mut seen)?;
	} else {
		for raw_path in raw_paths {
			if has_glob_meta(raw_path) {
				let scope = [String::from(".")];
				for entry in
					collect_glob_paths(root, std::slice::from_ref(raw_path), &scope, false, gitignore)?
				{
					let path = root.join(&entry.path);
					if seen.insert(path.clone()) {
						files.push(path);
					}
				}
				continue;
			}
			let resolved = resolve_session_path(root, raw_path);
			if resolved.is_dir() {
				collect_files_under(&resolved, gitignore, &mut files, &mut seen)?;
				continue;
			}
			if resolved.exists() {
				if seen.insert(resolved.clone()) {
					files.push(resolved);
				}
				continue;
			}
			return Err(ApiError::NotFound(format!("path not found: {raw_path}")));
		}
	}
	files.sort_unstable_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
	Ok(files)
}

fn collect_files_under(
	start: &FsPath,
	gitignore: bool,
	files: &mut Vec<PathBuf>,
	seen: &mut HashSet<PathBuf>,
) -> ApiResult<()> {
	if start.is_file() {
		if seen.insert(start.to_path_buf()) {
			files.push(start.to_path_buf());
		}
		return Ok(());
	}

	let mut builder = WalkBuilder::new(start);
	builder
		.hidden(true)
		.parents(gitignore)
		.ignore(gitignore)
		.git_ignore(gitignore)
		.git_global(gitignore)
		.git_exclude(gitignore)
		.require_git(false);
	for entry in builder.build() {
		let entry = entry.map_err(anyhow::Error::from)?;
		if !entry
			.file_type()
			.is_some_and(|file_type| file_type.is_file())
		{
			continue;
		}
		let path = entry.path().to_path_buf();
		if seen.insert(path.clone()) {
			files.push(path);
		}
	}
	Ok(())
}

fn resolve_session_path(root: &FsPath, raw_path: &str) -> PathBuf {
	let path = PathBuf::from(raw_path);
	if path.is_absolute() {
		path
	} else {
		root.join(path)
	}
}

fn has_glob_meta(path: &str) -> bool {
	path
		.chars()
		.any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
}

fn collect_match_lines(matcher: &RegexMatcher, path: &FsPath) -> ApiResult<Vec<u32>> {
	let mut searcher = SearcherBuilder::new()
		.line_number(true)
		.binary_detection(BinaryDetection::quit(0))
		.build();
	let mut collector = MatchCollector::default();
	searcher.search_path(matcher, path, &mut collector)?;
	Ok(collector.line_numbers)
}

fn read_text_lines(path: &FsPath) -> ApiResult<Vec<String>> {
	let bytes = std::fs::read(path)?;
	if bytes.is_empty() {
		return Ok(Vec::new());
	}
	let mut lines = Vec::new();
	let mut start = 0_usize;
	for (index, byte) in bytes.iter().enumerate() {
		if *byte == b'\n' {
			lines.push(line_text(&bytes[start..=index]));
			start = index + 1;
		}
	}
	if start < bytes.len() {
		lines.push(line_text(&bytes[start..]));
	}
	Ok(lines)
}

#[derive(Debug)]
struct CappedTextRead {
	etag:      String,
	text:      String,
	truncated: bool,
}

async fn read_file_text_capped(
	path: &FsPath,
	max_lines: Option<usize>,
	max_bytes: Option<usize>,
) -> ApiResult<CappedTextRead> {
	let path = path.to_path_buf();
	tokio::task::spawn_blocking(move || {
		let file = std::fs::File::open(&path)?;
		let mut reader = BufReader::new(file);
		let mut buf = Vec::new();
		let mut line = Vec::new();
		let mut lines = 0_usize;
		let mut bytes = 0_usize;
		let mut truncated = false;
		loop {
			line.clear();
			let read = reader.read_until(b'\n', &mut line)?;
			if read == 0 {
				break;
			}
			if let Some(limit) = max_lines
				&& lines >= limit
			{
				truncated = true;
				break;
			}
			if let Some(limit) = max_bytes
				&& bytes.saturating_add(line.len()) > limit
			{
				truncated = true;
				break;
			}
			bytes = bytes.saturating_add(line.len());
			lines = lines.saturating_add(1);
			buf.extend_from_slice(&line);
		}
		Ok(CappedTextRead {
			etag: fs_ops::compute_etag_hex(&buf),
			text: String::from_utf8_lossy(&buf).into_owned(),
			truncated,
		})
	})
	.await
	.map_err(|error| ApiError::Internal(error.into()))?
}

fn line_text(bytes: &[u8]) -> String {
	let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
	let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
	String::from_utf8_lossy(bytes).into_owned()
}

fn append_grep_records(
	path: &str,
	lines: &[String],
	match_lines: &[u32],
	context_before: usize,
	context_after: usize,
	records: &mut Vec<Bytes>,
) -> ApiResult<()> {
	let mut sorted_matches = match_lines.to_vec();
	sorted_matches.sort_unstable();
	sorted_matches.dedup();
	if sorted_matches.is_empty() {
		return Ok(());
	}

	let context_before =
		u32::try_from(context_before).map_err(|_| anyhow::anyhow!("context overflow"))?;
	let context_after =
		u32::try_from(context_after).map_err(|_| anyhow::anyhow!("context overflow"))?;
	let line_count =
		u32::try_from(lines.len()).map_err(|_| anyhow::anyhow!("line count overflow"))?;
	let match_set: HashSet<u32> = sorted_matches.iter().copied().collect();
	let mut ranges: Vec<(u32, u32)> = Vec::new();
	for line_number in sorted_matches {
		let start = line_number.saturating_sub(context_before).max(1);
		let end = line_number.saturating_add(context_after).min(line_count);
		if let Some((_, previous_end)) = ranges.last_mut()
			&& start <= previous_end.saturating_add(1)
		{
			*previous_end = (*previous_end).max(end);
			continue;
		}
		ranges.push((start, end));
	}

	for (start, end) in ranges {
		for line_number in start..=end {
			let index =
				usize::try_from(line_number - 1).map_err(|_| anyhow::anyhow!("line index overflow"))?;
			let kind = if match_set.contains(&line_number) {
				"match"
			} else {
				"context"
			};
			records.push(serialize_ndjson(&GrepRecord {
				path: path.to_owned(),
				line: line_number,
				kind,
				text: lines.get(index).cloned().unwrap_or_default(),
				truncated: None,
			})?);
		}
	}
	Ok(())
}

fn serialize_ndjson<T: Serialize>(value: &T) -> ApiResult<Bytes> {
	let mut bytes = serde_json::to_vec(value).map_err(anyhow::Error::from)?;
	bytes.push(b'\n');
	Ok(Bytes::from(bytes))
}

fn relative_display_path(root: &FsPath, path: &FsPath) -> String {
	path
		.strip_prefix(root)
		.unwrap_or(path)
		.to_string_lossy()
		.replace('\\', "/")
}
