use std::{
	borrow::Cow,
	path::{Path, PathBuf},
	sync::Arc,
};

use pi_ast::SupportLang;
use similar::TextDiff;
use tokio::{
	fs,
	io::{AsyncWriteExt, BufWriter},
};
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

use crate::{
	fs_ops::resolve_cwd_scoped_path,
	named::{Handle, NamedRegistry},
	protocol::{
		error::{ApiError, ApiResult},
		events::SessionEvent,
		responses::EditOp,
	},
	session::{Session, cache::Snapshot},
	state::LspHandle,
};

#[derive(Debug, Clone)]
pub struct WriteRequest {
	pub session: Arc<Session>,
	pub lsp: Option<Arc<NamedRegistry<LspHandle>>>,
	pub path: PathBuf,
	pub new_bytes: Vec<u8>,
	pub if_match: Option<String>,
	pub preserve_text_conventions: bool,
}

#[derive(Debug, Clone)]
pub struct WriteOutcome {
	pub etag:               String,
	pub op:                 EditOp,
	pub first_changed_line: Option<u32>,
	pub diff:               String,
	pub change_hunks:       Vec<TextChangeHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChangeHunk {
	pub old_start_byte: usize,
	pub old_end_byte:   usize,
	pub inserted_text:  String,
}

#[derive(Debug, Clone)]
struct ExistingFile {
	bytes: Arc<[u8]>,
	etag:  Arc<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BomKind {
	Utf8,
	Utf16Le,
	Utf16Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
	Crlf,
	Lf,
	Cr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEndingProfile {
	None,
	Single(LineEnding),
	Mixed,
}

pub async fn write_through(req: WriteRequest) -> ApiResult<WriteOutcome> {
	let session = Arc::clone(&req.session);
	let path = resolve_cwd_scoped_path(&session, &req.path.to_string_lossy()).await?;
	let _edit_guard = session.edit_lock.lock().await;
	let existing = load_existing(&session, &path).await?;
	verify_if_match(req.if_match.as_deref(), existing.as_ref())?;

	let new_bytes = if req.preserve_text_conventions {
		apply_text_conventions(existing.as_ref(), &req.new_bytes)?
	} else {
		req.new_bytes
	};

	write_atomically(&path, &new_bytes).await?;
	let metadata = fs::metadata(&path).await?;
	let etag = compute_etag(&new_bytes);
	let bytes_arc: Arc<[u8]> = Arc::from(new_bytes);
	session.read_cache.insert(Snapshot {
		path:  path.clone(),
		mtime: metadata.modified()?,
		len:   metadata.len(),
		etag:  Arc::from(etag.as_str()),
		bytes: Some(Arc::clone(&bytes_arc)),
	});

	let old_text = existing
		.as_ref()
		.map_or_else(String::new, |file| decode_text_lossy(&file.bytes).into_owned());
	let new_text = decode_text_lossy(&bytes_arc).into_owned();
	let diff = TextDiff::from_lines(old_text.as_str(), new_text.as_str())
		.unified_diff()
		.header("before", "after")
		.to_string();
	let change_hunks = text_change_hunks(old_text.as_str(), new_text.as_str());
	let first_changed_line = existing
		.as_ref()
		.and_then(|_| first_changed_line(&old_text, &new_text));
	let op = if existing.is_some() {
		EditOp::Update
	} else {
		EditOp::Create
	};

	if let Some(lsp) = req.lsp.as_ref() {
		forward_lsp_write(lsp, &path, &old_text, &new_text, &change_hunks).await;
	}

	let event_path = req.path.to_string_lossy().into_owned();
	let _ = session
		.events
		.send(SessionEvent::FileChanged { path: event_path, etag: Some(etag.clone()) });

	Ok(WriteOutcome { etag, op, first_changed_line, diff, change_hunks })
}

async fn forward_lsp_write(
	lsp: &Arc<NamedRegistry<LspHandle>>,
	path: &Path,
	old_text: &str,
	new_text: &str,
	change_hunks: &[TextChangeHunk],
) {
	let Some(language) = SupportLang::from_path(path) else {
		return;
	};
	let Some(handle) = select_lsp_handle(lsp, language.canonical_name()) else {
		return;
	};
	if let Err(error) = handle
		.inner
		.send_file_update(path, language.canonical_name(), old_text, new_text, change_hunks)
		.await
	{
		tracing::warn!(
			?error,
			language = language.canonical_name(),
			path = %path.display(),
			"failed to forward write through LSP"
		);
	}
}

/// Route writes by inferred language id:
/// 1. exact `/lsp/{language_id}`
/// 2. otherwise, the earliest-registered `/lsp/{language_id}-*`
fn select_lsp_handle(
	registry: &NamedRegistry<LspHandle>,
	language_id: &str,
) -> Option<Arc<Handle<LspHandle>>> {
	if let Some(handle) = registry.get(language_id) {
		return Some(handle);
	}
	let prefix = format!("{language_id}-");
	let mut matches = registry
		.names()
		.into_iter()
		.filter(|name| name.starts_with(&prefix))
		.filter_map(|name| registry.get(&name))
		.collect::<Vec<_>>();
	matches.sort_by_key(|handle| crate::lsp_tunnel::registration_order(&handle.inner));
	matches.into_iter().next()
}

fn text_change_hunks(old_text: &str, new_text: &str) -> Vec<TextChangeHunk> {
	if old_text == new_text {
		return Vec::new();
	}
	let old_bytes = old_text.as_bytes();
	let new_bytes = new_text.as_bytes();
	let mut prefix = 0_usize;
	let shared_len = old_bytes.len().min(new_bytes.len());
	while prefix < shared_len && old_bytes[prefix] == new_bytes[prefix] {
		prefix += 1;
	}
	while prefix > 0 && (!old_text.is_char_boundary(prefix) || !new_text.is_char_boundary(prefix)) {
		prefix -= 1;
	}
	let mut old_suffix = 0_usize;
	let mut new_suffix = 0_usize;
	while prefix + old_suffix < old_bytes.len()
		&& prefix + new_suffix < new_bytes.len()
		&& old_bytes[old_bytes.len() - 1 - old_suffix] == new_bytes[new_bytes.len() - 1 - new_suffix]
	{
		old_suffix += 1;
		new_suffix += 1;
	}
	while old_bytes.len().saturating_sub(old_suffix) > prefix
		&& new_bytes.len().saturating_sub(new_suffix) > prefix
		&& (!old_text.is_char_boundary(old_bytes.len() - old_suffix)
			|| !new_text.is_char_boundary(new_bytes.len() - new_suffix))
	{
		old_suffix = old_suffix.saturating_sub(1);
		new_suffix = new_suffix.saturating_sub(1);
	}
	let old_end = old_bytes.len() - old_suffix;
	let new_end = new_bytes.len() - new_suffix;
	vec![TextChangeHunk {
		old_start_byte: prefix,
		old_end_byte:   old_end,
		inserted_text:  new_text[prefix..new_end].to_owned(),
	}]
}

async fn load_existing(session: &Session, path: &Path) -> ApiResult<Option<ExistingFile>> {
	let metadata = match fs::metadata(path).await {
		Ok(metadata) => metadata,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
		Err(error) => return Err(error.into()),
	};
	let mtime = metadata.modified()?;
	let len = metadata.len();
	if let Some(cached) = session.read_cache.get(path)
		&& cached.mtime == mtime
		&& cached.len == len
		&& let Some(bytes) = cached.bytes
	{
		return Ok(Some(ExistingFile { bytes, etag: cached.etag }));
	}
	let bytes = Arc::<[u8]>::from(fs::read(path).await?);
	let etag = compute_etag(&bytes);
	session.read_cache.insert(Snapshot {
		path: path.to_path_buf(),
		mtime,
		len,
		etag: Arc::from(etag.as_str()),
		bytes: Some(Arc::clone(&bytes)),
	});
	Ok(Some(ExistingFile { bytes, etag: Arc::from(etag) }))
}

fn verify_if_match(if_match: Option<&str>, existing: Option<&ExistingFile>) -> ApiResult<()> {
	match (existing, if_match) {
		(None, Some(value)) if value.trim() != "*" => Err(ApiError::EtagMismatch),
		(None, _) => Ok(()),
		(Some(_), Some(value)) if value.trim() == "*" => Ok(()),
		(Some(file), Some(value)) if any_tag_matches(value, &file.etag) => Ok(()),
		(Some(_), _) => Err(ApiError::EtagMismatch),
	}
}

fn any_tag_matches(raw: &str, etag: &str) -> bool {
	raw.split(',').any(|candidate| {
		let trimmed = candidate.trim();
		let strong = trimmed.strip_prefix("W/").unwrap_or(trimmed);
		strong.trim_matches('"') == etag
	})
}

fn apply_text_conventions(
	existing: Option<&ExistingFile>,
	incoming_bytes: &[u8],
) -> ApiResult<Vec<u8>> {
	let incoming_text = std::str::from_utf8(incoming_bytes)
		.map_err(|_| ApiError::BadRequest("write.lines body must be valid UTF-8 text".to_owned()))?;
	let mut normalized = incoming_text.to_owned();
	if let Some(file) = existing {
		let original_text = decode_text_lossy(&file.bytes);
		if let Some(target) = dominant_line_ending(&original_text)
			&& let LineEndingProfile::Single(source) = classify_line_endings(&normalized)
			&& source != target
		{
			normalized = rewrite_line_endings(&normalized, source, target);
		}
		return Ok(encode_text_with_bom(&normalized, detect_bom(&file.bytes)));
	}
	Ok(normalized.into_bytes())
}

fn detect_bom(bytes: &[u8]) -> Option<BomKind> {
	if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
		Some(BomKind::Utf8)
	} else if bytes.starts_with(&[0xff, 0xfe]) {
		Some(BomKind::Utf16Le)
	} else if bytes.starts_with(&[0xfe, 0xff]) {
		Some(BomKind::Utf16Be)
	} else {
		None
	}
}

pub(crate) fn decode_text_lossy(bytes: &[u8]) -> Cow<'_, str> {
	match detect_bom(bytes) {
		Some(BomKind::Utf8) => String::from_utf8_lossy(&bytes[3..]),
		Some(BomKind::Utf16Le) => Cow::Owned(decode_utf16_lossy(&bytes[2..], false)),
		Some(BomKind::Utf16Be) => Cow::Owned(decode_utf16_lossy(&bytes[2..], true)),
		None => String::from_utf8_lossy(bytes),
	}
}

fn decode_utf16_lossy(bytes: &[u8], big_endian: bool) -> String {
	let mut units = Vec::with_capacity(bytes.len() / 2);
	for chunk in bytes.chunks_exact(2) {
		let value = if big_endian {
			u16::from_be_bytes([chunk[0], chunk[1]])
		} else {
			u16::from_le_bytes([chunk[0], chunk[1]])
		};
		units.push(value);
	}
	String::from_utf16_lossy(&units)
}

fn encode_text_with_bom(text: &str, bom: Option<BomKind>) -> Vec<u8> {
	match bom {
		Some(BomKind::Utf8) => {
			let mut bytes = Vec::with_capacity(text.len() + 3);
			bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
			bytes.extend_from_slice(text.as_bytes());
			bytes
		},
		Some(BomKind::Utf16Le) => encode_utf16(text, false),
		Some(BomKind::Utf16Be) => encode_utf16(text, true),
		None => text.as_bytes().to_vec(),
	}
}

fn encode_utf16(text: &str, big_endian: bool) -> Vec<u8> {
	let mut bytes = Vec::with_capacity((text.len() + 1) * 2);
	if big_endian {
		bytes.extend_from_slice(&[0xfe, 0xff]);
		for unit in text.encode_utf16() {
			bytes.extend_from_slice(&unit.to_be_bytes());
		}
	} else {
		bytes.extend_from_slice(&[0xff, 0xfe]);
		for unit in text.encode_utf16() {
			bytes.extend_from_slice(&unit.to_le_bytes());
		}
	}
	bytes
}

fn dominant_line_ending(text: &str) -> Option<LineEnding> {
	let mut crlf = 0usize;
	let mut lf = 0usize;
	let mut cr = 0usize;
	let bytes = text.as_bytes();
	let mut index = 0usize;
	while index < bytes.len() {
		match bytes[index] {
			b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
				crlf += 1;
				index += 2;
			},
			b'\n' => {
				lf += 1;
				index += 1;
			},
			b'\r' => {
				cr += 1;
				index += 1;
			},
			_ => {
				index += 1;
			},
		}
	}
	[(crlf, LineEnding::Crlf), (lf, LineEnding::Lf), (cr, LineEnding::Cr)]
		.into_iter()
		.max_by_key(|(count, _)| *count)
		.and_then(|(count, ending)| if count == 0 { None } else { Some(ending) })
}

fn classify_line_endings(text: &str) -> LineEndingProfile {
	let mut found: Option<LineEnding> = None;
	let bytes = text.as_bytes();
	let mut index = 0usize;
	while index < bytes.len() {
		let next = match bytes[index] {
			b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
				index += 2;
				Some(LineEnding::Crlf)
			},
			b'\n' => {
				index += 1;
				Some(LineEnding::Lf)
			},
			b'\r' => {
				index += 1;
				Some(LineEnding::Cr)
			},
			_ => {
				index += 1;
				None
			},
		};
		if let Some(ending) = next {
			match found {
				None => found = Some(ending),
				Some(existing) if existing == ending => {},
				Some(_) => return LineEndingProfile::Mixed,
			}
		}
	}
	found.map_or(LineEndingProfile::None, LineEndingProfile::Single)
}

fn rewrite_line_endings(text: &str, from: LineEnding, to: LineEnding) -> String {
	text.replace(from.as_str(), to.as_str())
}

impl LineEnding {
	const fn as_str(self) -> &'static str {
		match self {
			Self::Crlf => "\r\n",
			Self::Lf => "\n",
			Self::Cr => "\r",
		}
	}
}

fn first_changed_line(old_text: &str, new_text: &str) -> Option<u32> {
	let old_lines = split_lines_preserving_endings(old_text);
	let new_lines = split_lines_preserving_endings(new_text);
	let shared = old_lines.len().min(new_lines.len());
	for index in 0..shared {
		if old_lines[index] != new_lines[index] {
			return u32::try_from(index.saturating_add(1)).ok();
		}
	}
	if old_lines.len() == new_lines.len() {
		None
	} else {
		u32::try_from(shared.saturating_add(1)).ok()
	}
}

fn split_lines_preserving_endings(text: &str) -> Vec<&str> {
	let mut lines = Vec::new();
	let bytes = text.as_bytes();
	let mut start = 0usize;
	let mut index = 0usize;
	while index < bytes.len() {
		match bytes[index] {
			b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
				index += 2;
				lines.push(&text[start..index]);
				start = index;
			},
			b'\n' | b'\r' => {
				index += 1;
				lines.push(&text[start..index]);
				start = index;
			},
			_ => index += 1,
		}
	}
	if start < text.len() {
		lines.push(&text[start..]);
	}
	lines
}

async fn write_atomically(path: &Path, bytes: &[u8]) -> ApiResult<()> {
	let parent = path
		.parent()
		.ok_or_else(|| ApiError::BadRequest(format!("path has no parent: {}", path.display())))?;
	fs::create_dir_all(parent).await?;
	let file_name = path.file_name().ok_or_else(|| {
		ApiError::BadRequest(format!("path has no terminal component: {}", path.display()))
	})?;
	let temp_path = parent.join(format!("{}.tmp.{}", file_name.to_string_lossy(), Uuid::new_v4()));
	let result = async {
		let file = fs::OpenOptions::new()
			.write(true)
			.create_new(true)
			.open(&temp_path)
			.await?;
		let mut writer = BufWriter::new(file);
		writer.write_all(bytes).await?;
		writer.flush().await?;
		writer.get_ref().sync_all().await?;
		drop(writer);
		fs::rename(&temp_path, path).await?;
		std::fs::File::open(parent)?.sync_all()?;
		Ok::<(), ApiError>(())
	}
	.await;
	if result.is_err() {
		let _ = fs::remove_file(&temp_path).await;
	}
	result
}

fn compute_etag(bytes: &[u8]) -> String {
	format!("{:016x}", xxh64(bytes, 0))
}
