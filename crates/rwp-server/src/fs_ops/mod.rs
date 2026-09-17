pub mod etag_cache;
pub mod watcher;
pub mod workspace;
pub mod write_through;

use std::{
	convert::Infallible,
	ffi::OsString,
	io::Cursor,
	path::{Component, Path, PathBuf},
	pin::Pin,
	sync::Arc,
	time::Duration,
};

use bytes::Bytes;
use futures_util::{Stream, StreamExt, TryStreamExt, stream};
use mime::Mime;
use reqwest::{Url, header};
use tokio_util::sync::CancellationToken;
use xxhash_rust::xxh64::xxh64;

use crate::{
	protocol::error::{ApiError, ApiResult},
	session::{Session, cache::Snapshot},
};

#[derive(Debug, Clone)]
pub enum ReadTarget {
	File(PathBuf),
	Url(Url),
}

#[derive(Debug, Clone)]
pub struct FileBody {
	pub path:  PathBuf,
	pub bytes: Arc<[u8]>,
	pub etag:  Arc<str>,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum UrlReaderMode {
	Raw,
	#[default]
	Markdown,
	Text,
}

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

pub fn heartbeat_stream<S>(
	inner: S,
	heartbeat_interval: Duration,
	cancellation_token: CancellationToken,
	heartbeat: Bytes,
) -> impl Stream<Item = Result<Bytes, Infallible>>
where
	S: Stream<Item = Bytes>,
{
	let heartbeat_timer = tokio::time::interval_at(
		tokio::time::Instant::now() + heartbeat_interval,
		heartbeat_interval,
	);
	stream::unfold(
		(Pin::from(Box::new(inner)), heartbeat_timer, cancellation_token, heartbeat),
		|(mut inner, mut heartbeat_timer, cancellation_token, heartbeat)| async move {
			tokio::select! {
				() = cancellation_token.cancelled() => None,
				item = inner.next() => match item {
					Some(bytes) => {
						heartbeat_timer.reset();
						Some((Ok(bytes), (inner, heartbeat_timer, cancellation_token, heartbeat)))
					}
					None => None,
				},
				_ = heartbeat_timer.tick() => {
					Some((Ok(heartbeat.clone()), (inner, heartbeat_timer, cancellation_token, heartbeat)))
				},
			}
		},
	)
}

#[must_use]
pub fn split_inline_selector(
	raw_path: &str,
	explicit_range: Option<String>,
) -> (String, Option<String>) {
	if explicit_range.is_some()
		|| raw_path.starts_with("http://")
		|| raw_path.starts_with("https://")
		|| raw_path.starts_with("file://")
	{
		return (raw_path.to_owned(), explicit_range);
	}

	if let Some((path, selector)) = raw_path.rsplit_once(':')
		&& is_selector(selector)
	{
		return (path.to_owned(), Some(selector.to_owned()));
	}

	(raw_path.to_owned(), explicit_range)
}

pub async fn resolve_read_target(session: &Session, raw_path: &str) -> ApiResult<ReadTarget> {
	if raw_path.is_empty() {
		return Err(ApiError::BadRequest("path is required".to_owned()));
	}

	if raw_path.starts_with("http://") || raw_path.starts_with("https://") {
		let url = Url::parse(raw_path).map_err(|error| {
			ApiError::BadRequest(format!("invalid URL path {raw_path:?}: {error}"))
		})?;
		return Ok(ReadTarget::Url(url));
	}

	Ok(ReadTarget::File(resolve_file_path(session, raw_path).await?))
}

pub async fn resolve_file_path(session: &Session, raw_path: &str) -> ApiResult<PathBuf> {
	if raw_path.is_empty() {
		return Err(ApiError::BadRequest("path is required".to_owned()));
	}

	let candidate = if raw_path.starts_with("file://") {
		let url = Url::parse(raw_path).map_err(|error| {
			ApiError::BadRequest(format!("invalid file URL {raw_path:?}: {error}"))
		})?;
		url.to_file_path().map_err(|()| {
			ApiError::BadRequest(format!("file URL does not map to a local path: {raw_path:?}"))
		})?
	} else {
		let path = PathBuf::from(raw_path);
		if path.is_absolute() {
			path
		} else {
			session.cwd().join(path)
		}
	};

	tokio::fs::canonicalize(&candidate)
		.await
		.map_err(|error| map_io(error, &candidate))
}

pub async fn resolve_cwd_scoped_path(session: &Session, raw_path: &str) -> ApiResult<PathBuf> {
	if raw_path.is_empty() {
		return Err(ApiError::BadRequest("path is required".to_owned()));
	}
	if raw_path.starts_with("file://") {
		return Err(ApiError::BadRequest("absolute paths are not allowed".to_owned()));
	}
	let requested = PathBuf::from(raw_path);
	if requested.is_absolute() {
		return Err(ApiError::BadRequest("absolute paths are not allowed".to_owned()));
	}
	let root = tokio::fs::canonicalize(session.cwd())
		.await
		.map_err(ApiError::Io)?;
	let candidate = normalize_absolute_path(root.join(requested));
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
			Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
			Component::CurDir => {},
			Component::ParentDir => {
				let _ = normalized.pop();
			},
			Component::Normal(part) => normalized.push(part),
		}
	}
	normalized
}

async fn resolve_against_root(root: &Path, candidate: &Path) -> ApiResult<PathBuf> {
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

pub async fn read_file_cached(session: &Session, path: &Path) -> ApiResult<FileBody> {
	let metadata = tokio::fs::metadata(path)
		.await
		.map_err(|error| map_io(error, path))?;
	if !metadata.is_file() {
		return Err(ApiError::BadRequest(format!("path is not a file: {}", path.display())));
	}

	let modified = metadata.modified().map_err(ApiError::Io)?;
	let len = metadata.len();
	if let Some(snapshot) = session.read_cache.get(path)
		&& snapshot.mtime == modified
		&& snapshot.len == len
		&& let Some(bytes) = snapshot.bytes
	{
		return Ok(FileBody { path: snapshot.path, bytes, etag: snapshot.etag });
	}

	let bytes = tokio::fs::read(path)
		.await
		.map_err(|error| map_io(error, path))?;
	let bytes = Arc::<[u8]>::from(bytes);
	let etag: Arc<str> = Arc::from(compute_etag_hex(&bytes));
	let snapshot = Snapshot {
		path: path.to_path_buf(),
		mtime: modified,
		len,
		etag: etag.clone(),
		bytes: Some(bytes.clone()),
	};
	session.read_cache.insert(snapshot);

	Ok(FileBody { path: path.to_path_buf(), bytes, etag })
}

pub async fn fetch_url_body(url: &Url) -> ApiResult<(Vec<u8>, String)> {
	tracing::debug!(%url, "TODO: add reader-mode HTML extraction for read.lines URLs");
	let response = reqwest::Client::new()
		.get(url.clone())
		.send()
		.await
		.map_err(|error| ApiError::Internal(error.into()))?
		.error_for_status()
		.map_err(|error| ApiError::Internal(error.into()))?;
	let bytes = response
		.bytes()
		.await
		.map_err(|error| ApiError::Internal(error.into()))?;
	let body = bytes.to_vec();
	let etag = compute_etag_hex(&body);
	Ok((body, etag))
}

pub async fn fetch_url_text(url: &Url, reader: UrlReaderMode) -> ApiResult<(String, String)> {
	let response = reqwest::Client::new()
		.get(url.clone())
		.send()
		.await
		.map_err(|error| ApiError::Internal(error.into()))?
		.error_for_status()
		.map_err(|error| ApiError::Internal(error.into()))?;
	let content_type = response
		.headers()
		.get(header::CONTENT_TYPE)
		.and_then(|value| value.to_str().ok())
		.and_then(|value| value.parse::<Mime>().ok());
	let bytes = response
		.bytes_stream()
		.try_fold(Vec::new(), |mut buffer, chunk| async move {
			buffer.extend_from_slice(&chunk);
			Ok(buffer)
		})
		.await
		.map_err(|error: reqwest::Error| ApiError::Internal(error.into()))?;
	let rendered = if is_html_content_type(content_type.as_ref()) {
		transform_html(url, &bytes, reader)
	} else {
		write_through::decode_text_lossy(&bytes).into_owned()
	};
	let etag = compute_etag_hex(rendered.as_bytes());
	Ok((etag, rendered))
}

#[must_use]
pub fn compute_etag_hex(bytes: &[u8]) -> String {
	format!("{:016x}", xxh64(bytes, 0))
}

#[must_use]
pub fn count_lines(bytes: &[u8]) -> usize {
	if bytes.is_empty() {
		return 0;
	}

	let newlines = bytes
		.iter()
		.fold(0_usize, |count, byte| count + usize::from(*byte == b'\n'));
	if bytes.last() == Some(&b'\n') {
		newlines
	} else {
		newlines + 1
	}
}

fn transform_html(url: &Url, bytes: &[u8], reader: UrlReaderMode) -> String {
	let raw_html = write_through::decode_text_lossy(bytes).into_owned();
	if matches!(reader, UrlReaderMode::Raw) {
		return raw_html;
	}

	let extracted = readability::extractor::extract(&mut Cursor::new(raw_html.as_bytes()), url).ok();
	match reader {
		UrlReaderMode::Raw => unreachable!("handled above"),
		UrlReaderMode::Text => extracted.map_or_else(
			|| html2text::from_read(raw_html.as_bytes(), 120).unwrap_or_else(|_| raw_html.clone()),
			|product| product.text,
		),
		UrlReaderMode::Markdown => extracted.map_or_else(
			|| html2text::from_read(raw_html.as_bytes(), 120).unwrap_or_else(|_| raw_html.clone()),
			|product| {
				// If extracted content lacks headings, use raw HTML for better conversion
				let has_heading = product.content.contains("<h1>") || product.content.contains("<h2>");
				if has_heading {
					html2text::from_read(product.content.as_bytes(), 120).unwrap_or(product.text)
				} else {
					html2text::from_read(raw_html.as_bytes(), 120).unwrap_or_else(|_| raw_html.clone())
				}
			},
		),
	}
}

fn is_html_content_type(content_type: Option<&Mime>) -> bool {
	content_type
		.is_some_and(|mime| matches!(mime.essence_str(), "text/html" | "application/xhtml+xml"))
}

fn is_selector(selector: &str) -> bool {
	if selector == "raw" {
		return true;
	}
	if selector.bytes().all(|byte| byte.is_ascii_digit()) {
		return !selector.starts_with('0') || selector == "0";
	}
	if let Some((start, end)) = selector.split_once('-') {
		return !start.is_empty()
			&& !end.is_empty()
			&& start.bytes().all(|byte| byte.is_ascii_digit())
			&& end.bytes().all(|byte| byte.is_ascii_digit());
	}
	if let Some((start, len)) = selector.split_once('+') {
		return !start.is_empty()
			&& !len.is_empty()
			&& start.bytes().all(|byte| byte.is_ascii_digit())
			&& len.bytes().all(|byte| byte.is_ascii_digit());
	}
	false
}

fn map_io(error: std::io::Error, path: &Path) -> ApiError {
	if error.kind() == std::io::ErrorKind::NotFound {
		ApiError::NotFound(path.display().to_string())
	} else {
		ApiError::Io(error)
	}
}
