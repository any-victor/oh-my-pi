use std::{
	collections::{BTreeMap, HashMap},
	io,
	path::Path,
	sync::{
		Arc, LazyLock, Mutex as StdMutex,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use anyhow::anyhow;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
	io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
	process::{ChildStderr, ChildStdout, Command},
	sync::broadcast,
	time::{sleep, timeout},
};
use url::Url;
use utoipa::ToSchema;

use crate::{
	fs_ops::write_through::TextChangeHunk,
	named::{Handle, HandleScope},
	protocol::{
		error::{ApiError, ApiResult},
		responses::LspGetResponse,
	},
	state::{AppState, LspHandle},
};

const INITIALIZE_REQUEST_ID: &str = "rwp-initialize";
const BROADCAST_CAPACITY: usize = 128;
const MAX_HEADER_BYTES: usize = 8192;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextDocumentSyncKind {
	None,
	Full,
	Incremental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LspHandleMetadata {
	registration_order:      u64,
	text_document_sync_kind: TextDocumentSyncKind,
}

static NEXT_REGISTRATION_ORDER: AtomicU64 = AtomicU64::new(1);
static HANDLE_METADATA: LazyLock<StdMutex<HashMap<usize, LspHandleMetadata>>> =
	LazyLock::new(|| StdMutex::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LspConfig {
	pub command:                String,
	pub args:                   Vec<String>,
	pub env:                    BTreeMap<String, String>,
	pub root_uri:               Option<String>,
	pub initialization_options: Option<Value>,
	#[serde(default = "default_idle_timeout_ms")]
	pub idle_timeout_ms:        u64,
	#[serde(skip, default = "HandleScope::global")]
	pub scope:                  HandleScope,
}

impl LspConfig {
	#[must_use]
	pub const fn idle_timeout(&self) -> Duration {
		Duration::from_millis(self.idle_timeout_ms)
	}
}

#[must_use]
pub const fn default_idle_timeout_ms() -> u64 {
	300_000
}

impl LspHandle {
	pub async fn spawn(config: LspConfig) -> ApiResult<Arc<Self>> {
		let mut command = Command::new(&config.command);
		command
			.args(&config.args)
			.envs(&config.env)
			.kill_on_drop(true)
			.stdin(std::process::Stdio::piped())
			.stdout(std::process::Stdio::piped())
			.stderr(std::process::Stdio::piped());
		let mut child = command.spawn()?;
		let mut stdin = child
			.stdin
			.take()
			.ok_or_else(|| ApiError::Internal(anyhow!("spawned LSP process missing stdin")))?;
		let mut stdout = child
			.stdout
			.take()
			.ok_or_else(|| ApiError::Internal(anyhow!("spawned LSP process missing stdout")))?;
		let stderr = child
			.stderr
			.take()
			.ok_or_else(|| ApiError::Internal(anyhow!("spawned LSP process missing stderr")))?;

		let initialize = json!({
			"jsonrpc": "2.0",
			"id": INITIALIZE_REQUEST_ID,
			"method": "initialize",
			"params": {
				"processId": null,
				"clientInfo": {
					"name": "rwp-server",
					"version": env!("CARGO_PKG_VERSION"),
				},
				"rootUri": config.root_uri,
				"capabilities": {},
				"initializationOptions": config.initialization_options,
			}
		});
		write_frame(&mut stdin, initialize.to_string().as_bytes()).await?;
		let initialize_response = read_frame(&mut stdout).await?;
		let initialize_value: Value = serde_json::from_str(&initialize_response)
			.map_err(|error| ApiError::Internal(error.into()))?;
		let initialize_result = initialize_value
			.get("result")
			.cloned()
			.ok_or_else(|| ApiError::Internal(anyhow!("LSP initialize response missing result")))?;
		let capabilities = initialize_result
			.get("capabilities")
			.cloned()
			.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
		write_frame(
			&mut stdin,
			json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} })
				.to_string()
				.as_bytes(),
		)
		.await?;

		let (messages_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
		let handle = Arc::new(Self {
			config,
			initialize_result,
			capabilities,
			project_loaded: AtomicBool::new(true),
			diagnostics: tokio::sync::Mutex::new(std::collections::HashMap::new()),
			messages_tx,
			stdin: tokio::sync::Mutex::new(stdin),
			child: tokio::sync::Mutex::new(child),
			pending: tokio::sync::Mutex::new(std::collections::HashMap::new()),
			next_request_id: std::sync::atomic::AtomicU64::new(1),
			document_versions: tokio::sync::Mutex::new(std::collections::HashMap::new()),
		});
		register_handle_metadata(&handle);
		spawn_stdout_pump(Arc::clone(&handle), stdout);
		spawn_stderr_pump(stderr);
		Ok(handle)
	}

	pub async fn send_json_bytes(&self, body: &[u8]) -> io::Result<()> {
		let mut stdin = self.stdin.lock().await;
		write_frame(&mut *stdin, body).await
	}

	pub async fn send_file_update(
		&self,
		path: &Path,
		language_id: &str,
		old_text: &str,
		new_text: &str,
		change_hunks: &[TextChangeHunk],
	) -> ApiResult<()> {
		let uri = Url::from_file_path(path)
			.map_err(|()| {
				ApiError::BadRequest(format!(
					"path is not representable as file URI: {}",
					path.display()
				))
			})?
			.to_string();
		let sync_kind = text_document_sync_kind(self);
		let (method, params) = {
			let mut document_versions = self.document_versions.lock().await;
			if let Some(version) = document_versions.get_mut(&uri) {
				*version += 1;
				let content_changes = if sync_kind == TextDocumentSyncKind::Incremental {
					incremental_content_changes(old_text, change_hunks)?
				} else {
					vec![json!({ "text": new_text })]
				};
				(
					"textDocument/didChange",
					json!({
						"textDocument": {
							"uri": uri,
							"version": *version,
						},
						"contentChanges": content_changes,
					}),
				)
			} else {
				document_versions.insert(uri.clone(), 1);
				(
					"textDocument/didOpen",
					json!({
						"textDocument": {
							"uri": uri,
							"languageId": language_id,
							"version": 1,
							"text": new_text,
						},
					}),
				)
			}
		};
		let payload = json!({
			"jsonrpc": "2.0",
			"method": method,
			"params": params,
		});
		self
			.send_json_bytes(payload.to_string().as_bytes())
			.await
			.map_err(ApiError::Io)
	}

	pub async fn shutdown(&self) -> ApiResult<()> {
		let request_id =
			format!("rwp-internal-{}", self.next_request_id.fetch_add(1, Ordering::Relaxed));
		let pending_key = id_key(&Value::String(request_id.clone()));
		let (response_tx, response_rx) = tokio::sync::oneshot::channel();
		self
			.pending
			.lock()
			.await
			.insert(pending_key.clone(), response_tx);
		let shutdown = json!({
			"jsonrpc": "2.0",
			"id": request_id,
			"method": "shutdown",
			"params": null,
		});
		if let Err(error) = self.send_json_bytes(shutdown.to_string().as_bytes()).await {
			self.pending.lock().await.remove(&pending_key);
			remove_handle_metadata(self);
			return Err(ApiError::Io(error));
		}
		let _ = timeout(Duration::from_secs(2), response_rx).await;
		self.pending.lock().await.remove(&pending_key);
		let _ = self
			.send_json_bytes(
				json!({ "jsonrpc": "2.0", "method": "exit", "params": null })
					.to_string()
					.as_bytes(),
			)
			.await;
		let mut child = self.child.lock().await;
		if timeout(Duration::from_secs(1), child.wait()).await.is_err() {
			let _ = child.kill().await;
			let _ = child.wait().await;
		}
		remove_handle_metadata(self);
		Ok(())
	}

	#[must_use]
	pub async fn get_response(&self, name: String, handle: &Handle<Self>) -> LspGetResponse {
		let last_active_ms = duration_millis(Instant::now().duration_since(handle.last_active()));
		let open_files = self
			.document_versions
			.lock()
			.await
			.keys()
			.cloned()
			.collect::<Vec<_>>();
		let diagnostics = self
			.diagnostics
			.lock()
			.await
			.iter()
			.map(|(uri, value)| (uri.clone(), value.clone()))
			.collect::<std::collections::BTreeMap<_, _>>();
		LspGetResponse {
			name,
			initialized: true,
			capabilities: self.capabilities.clone(),
			project_loaded: self.project_loaded.load(Ordering::Relaxed),
			open_files,
			diagnostics,
			ref_count: handle.refcount(),
			last_active_ms,
		}
	}
}

pub async fn serve_websocket(socket: WebSocket, handle: Arc<Handle<LspHandle>>) {
	let _guard = handle.retain();
	let (mut sender, mut receiver) = socket.split();
	let mut broadcast_rx = handle.inner.messages_tx.subscribe();
	let on_close = Arc::clone(&handle.on_close);
	let recv_close = Arc::clone(&handle.on_close);
	let send_task = tokio::spawn(async move {
		loop {
			tokio::select! {
				() = on_close.notified() => break,
				message = broadcast_rx.recv() => match message {
					Ok(frame) => {
						if sender.send(Message::Text(frame.into())).await.is_err() {
							break;
						}
					}
					Err(broadcast::error::RecvError::Lagged(_)) => {},
					Err(broadcast::error::RecvError::Closed) => break,
				},
			}
		}
	});

	loop {
		let message_result = tokio::select! {
			() = recv_close.notified() => break,
			message = receiver.next() => message,
		};
		let Some(message_result) = message_result else {
			break;
		};
		let Ok(message) = message_result else {
			break;
		};
		match message {
			Message::Text(text) => {
				if let Ok(value) = serde_json::from_str::<Value>(&text) {
					update_document_state_from_client(&handle.inner, &value).await;
				}
				if handle.inner.send_json_bytes(text.as_bytes()).await.is_err() {
					break;
				}
			},
			Message::Binary(bytes) => {
				if let Ok(value) = serde_json::from_slice::<Value>(bytes.as_ref()) {
					update_document_state_from_client(&handle.inner, &value).await;
				}
				if handle.inner.send_json_bytes(bytes.as_ref()).await.is_err() {
					break;
				}
			},
			Message::Close(_) => break,
			Message::Ping(_) | Message::Pong(_) => {},
		}
	}

	send_task.abort();
}

pub fn spawn_idle_reaper(state: AppState, name: String, handle: Arc<Handle<LspHandle>>) {
	tokio::spawn(async move {
		loop {
			let idle_for = Instant::now().duration_since(handle.last_active());
			let idle_timeout = handle.inner.config.idle_timeout();
			let remaining = idle_timeout.saturating_sub(idle_for);
			tokio::select! {
				() = handle.on_close.notified() => break,
				() = sleep(remaining) => {
					if handle.refcount() == 0 && Instant::now().duration_since(handle.last_active()) >= idle_timeout {
						if let Some(removed) = state.lsp.remove(&name) {
							removed.on_close.notify_waiters();
							let _ = removed.inner.shutdown().await;
						}
						break;
					}
				}
			}
		}
	});
}

pub async fn remove_and_shutdown(state: &AppState, name: &str) -> ApiResult<()> {
	let handle = state
		.lsp
		.remove(name)
		.ok_or_else(|| ApiError::NotFound(format!("lsp handle not found: {name}")))?;
	handle.on_close.notify_waiters();
	handle.inner.shutdown().await
}

async fn stdout_pump(handle: Arc<LspHandle>, stdout: ChildStdout) -> ApiResult<()> {
	let mut stdout = stdout;
	loop {
		let frame = match read_frame(&mut stdout).await {
			Ok(frame) => frame,
			Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
			Err(error) => return Err(ApiError::Io(error)),
		};
		if let Ok(value) = serde_json::from_str::<Value>(&frame) {
			if let Some(id_key) = response_id_key(&value)
				&& let Some(sender) = handle.pending.lock().await.remove(&id_key)
			{
				let _ = sender.send(value.clone());
			}
			update_runtime_state(&handle, &value).await;
		}
		let _ = handle.messages_tx.send(frame);
	}
	Ok(())
}
async fn update_runtime_state(handle: &Arc<LspHandle>, message: &Value) {
	let Some(method) = message.get("method").and_then(Value::as_str) else {
		return;
	};
	match method {
		"textDocument/publishDiagnostics" => {
			let Some(uri) = message.pointer("/params/uri").and_then(Value::as_str) else {
				return;
			};
			let diagnostics = message
				.pointer("/params/diagnostics")
				.cloned()
				.unwrap_or_else(|| Value::Array(Vec::new()));
			handle
				.diagnostics
				.lock()
				.await
				.insert(uri.to_owned(), diagnostics);
		},
		"$/progress" => match message
			.pointer("/params/value/kind")
			.and_then(Value::as_str)
		{
			Some("begin") => handle.project_loaded.store(false, Ordering::Relaxed),
			Some("end") => handle.project_loaded.store(true, Ordering::Relaxed),
			_ => {},
		},
		_ => {},
	}
}
async fn update_document_state_from_client(handle: &LspHandle, message: &Value) {
	let Some(method) = message.get("method").and_then(Value::as_str) else {
		return;
	};
	let Some(uri) = message
		.pointer("/params/textDocument/uri")
		.and_then(Value::as_str)
	else {
		return;
	};
	match method {
		"textDocument/didOpen" | "textDocument/didChange" => {
			let version = message
				.pointer("/params/textDocument/version")
				.and_then(Value::as_i64)
				.and_then(|value| i32::try_from(value).ok())
				.unwrap_or(1);
			handle
				.document_versions
				.lock()
				.await
				.insert(uri.to_owned(), version);
		},
		"textDocument/didClose" => {
			handle.document_versions.lock().await.remove(uri);
			handle.diagnostics.lock().await.remove(uri);
		},
		_ => {},
	}
}

fn spawn_stdout_pump(handle: Arc<LspHandle>, stdout: ChildStdout) {
	tokio::spawn(async move {
		if let Err(error) = stdout_pump(handle, stdout).await {
			tracing::warn!(?error, "lsp stdout pump failed");
		}
	});
}

fn spawn_stderr_pump(stderr: ChildStderr) {
	tokio::spawn(async move {
		let mut lines = BufReader::new(stderr).lines();
		loop {
			match lines.next_line().await {
				Ok(Some(line)) => tracing::debug!(%line, "lsp stderr"),
				Ok(None) => break,
				Err(error) => {
					tracing::warn!(?error, "lsp stderr pump failed");
					break;
				},
			}
		}
	});
}

fn response_id_key(value: &Value) -> Option<String> {
	let id = value.get("id")?;
	if value.get("method").is_some() {
		return None;
	}
	Some(id_key(id))
}

fn id_key(id: &Value) -> String {
	match id {
		Value::String(value) => format!("s:{value}"),
		Value::Number(value) => format!("n:{value}"),
		_ => id.to_string(),
	}
}

fn duration_millis(duration: Duration) -> u64 {
	u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn registration_order(handle: &Arc<LspHandle>) -> u64 {
	handle_metadata(handle).map_or(u64::MAX, |metadata| metadata.registration_order)
}

fn register_handle_metadata(handle: &Arc<LspHandle>) {
	let metadata = LspHandleMetadata {
		registration_order:      NEXT_REGISTRATION_ORDER.fetch_add(1, Ordering::Relaxed),
		text_document_sync_kind: extract_text_document_sync_kind(&handle.initialize_result),
	};
	HANDLE_METADATA
		.lock()
		.expect("lsp handle metadata lock")
		.insert(handle_key(handle), metadata);
}

fn remove_handle_metadata(handle: &LspHandle) {
	HANDLE_METADATA
		.lock()
		.expect("lsp handle metadata lock")
		.remove(&handle_key_ref(handle));
}

fn handle_metadata(handle: &Arc<LspHandle>) -> Option<LspHandleMetadata> {
	HANDLE_METADATA
		.lock()
		.expect("lsp handle metadata lock")
		.get(&handle_key(handle))
		.copied()
}

fn text_document_sync_kind(handle: &LspHandle) -> TextDocumentSyncKind {
	HANDLE_METADATA
		.lock()
		.expect("lsp handle metadata lock")
		.get(&handle_key_ref(handle))
		.map_or(TextDocumentSyncKind::Full, |metadata| metadata.text_document_sync_kind)
}

fn handle_key(handle: &Arc<LspHandle>) -> usize {
	Arc::as_ptr(handle) as usize
}

fn handle_key_ref(handle: &LspHandle) -> usize {
	core::ptr::addr_of!(*handle) as usize
}

fn extract_text_document_sync_kind(initialize_result: &Value) -> TextDocumentSyncKind {
	let Some(sync) = initialize_result.pointer("/capabilities/textDocumentSync") else {
		return TextDocumentSyncKind::Full;
	};
	match sync {
		Value::Number(kind) => match kind.as_u64() {
			Some(2) => TextDocumentSyncKind::Incremental,
			Some(0) => TextDocumentSyncKind::None,
			_ => TextDocumentSyncKind::Full,
		},
		Value::Object(object) => match object.get("change").and_then(Value::as_u64) {
			Some(2) => TextDocumentSyncKind::Incremental,
			Some(0) => TextDocumentSyncKind::None,
			_ => TextDocumentSyncKind::Full,
		},
		_ => TextDocumentSyncKind::Full,
	}
}

fn incremental_content_changes(
	old_text: &str,
	change_hunks: &[TextChangeHunk],
) -> ApiResult<Vec<Value>> {
	if change_hunks.is_empty() {
		return Ok(vec![json!({ "text": old_text })]);
	}
	change_hunks
		.iter()
		.map(|hunk| {
			let start = byte_offset_to_position(old_text, hunk.old_start_byte)?;
			let end = byte_offset_to_position(old_text, hunk.old_end_byte)?;
			Ok(json!({
				"range": {
					"start": { "line": start.line, "character": start.character },
					"end": { "line": end.line, "character": end.character },
				},
				"text": hunk.inserted_text,
			}))
		})
		.collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LspPosition {
	line:      u32,
	character: u32,
}

fn byte_offset_to_position(text: &str, target: usize) -> ApiResult<LspPosition> {
	if target > text.len() || !text.is_char_boundary(target) {
		return Err(ApiError::Internal(anyhow!(
			"LSP byte offset {target} is not a valid UTF-8 boundary for {} bytes",
			text.len()
		)));
	}
	let mut line = 0_u32;
	let mut character = 0_u32;
	let mut chars = text.char_indices().peekable();
	while let Some((byte_index, ch)) = chars.next() {
		if byte_index == target {
			return Ok(LspPosition { line, character });
		}
		match ch {
			'\r' => {
				if let Some(&(next_index, '\n')) = chars.peek() {
					if next_index == target {
						return Ok(LspPosition { line: line.saturating_add(1), character: 0 });
					}
					let _ = chars.next();
				}
				line = line.saturating_add(1);
				character = 0;
			},
			'\n' => {
				line = line.saturating_add(1);
				character = 0;
			},
			_ => {
				character = character.saturating_add(u32::try_from(ch.len_utf16()).unwrap_or(u32::MAX));
			},
		}
	}
	if target == text.len() {
		return Ok(LspPosition { line, character });
	}
	Err(ApiError::Internal(anyhow!("LSP byte offset {target} overshot text length {}", text.len())))
}

pub async fn read_frame<R>(reader: &mut R) -> io::Result<String>
where
	R: AsyncRead + Unpin,
{
	let mut header = Vec::new();
	loop {
		let mut byte = [0_u8; 1];
		reader.read_exact(&mut byte).await?;
		header.push(byte[0]);
		if header.ends_with(b"\r\n\r\n") {
			break;
		}
		if header.len() > MAX_HEADER_BYTES {
			return Err(io::Error::new(io::ErrorKind::InvalidData, "LSP header exceeds limit"));
		}
	}
	let content_length = parse_content_length(&header)?;
	let mut body = vec![0_u8; content_length];
	reader.read_exact(&mut body).await?;
	String::from_utf8(body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn write_frame<W>(writer: &mut W, body: &[u8]) -> io::Result<()>
where
	W: AsyncWrite + Unpin,
{
	writer
		.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
		.await?;
	writer.write_all(body).await?;
	writer.flush().await
}

fn parse_content_length(header: &[u8]) -> io::Result<usize> {
	let header = std::str::from_utf8(header)
		.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
	for line in header.split("\r\n") {
		if let Some(value) = line.strip_prefix("Content-Length:") {
			return value
				.trim()
				.parse::<usize>()
				.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
		}
	}
	Err(io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header"))
}
