use std::{
	collections::{BTreeMap, VecDeque},
	io::{Error, ErrorKind},
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	time::Duration,
};

use anyhow::{Context, anyhow};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::{
	io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
	net::TcpStream,
	process::Command,
	time::{sleep, timeout},
};
use tracing::{debug, warn};

use crate::{
	AppState,
	named::{Handle, HandleGuard, HandleScope},
	protocol::{
		error::{ApiError, ApiResult},
		requests::DapTransport,
	},
	state::DapHandle,
};

type BoxReader = Box<dyn AsyncRead + Send + Unpin>;
type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;

const HEADER_LIMIT_BYTES: usize = 16 * 1024;
const REAPER_POLL_INTERVAL: Duration = Duration::from_secs(5);
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const DELETE_GRACE_PERIOD: Duration = Duration::from_secs(1);
const STDERR_TAIL_LIMIT_BYTES: usize = 16 * 1024;

#[must_use]
pub const fn default_idle_timeout_ms() -> u64 {
	300_000
}

#[derive(Debug, Clone)]
pub struct DapSpawnConfig {
	pub command:         String,
	pub args:            Vec<String>,
	pub env:             BTreeMap<String, String>,
	pub transport:       DapTransport,
	pub host:            Option<String>,
	pub port:            Option<u16>,
	pub retry_ms:        u32,
	pub retry_attempts:  u32,
	pub idle_timeout_ms: u64,
	pub scope:           HandleScope,
}

impl DapHandle {
	pub async fn spawn(config: DapSpawnConfig) -> ApiResult<Arc<Self>> {
		match config.transport {
			DapTransport::Stdio => spawn_stdio(config),
			DapTransport::Tcp => spawn_tcp(config).await,
		}
	}

	pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<String> {
		self.messages.subscribe()
	}

	#[must_use]
	pub fn close_notifier(&self) -> Arc<tokio::sync::Notify> {
		Arc::clone(&self.closed)
	}

	pub async fn send_client_message(&self, message: Message) -> ApiResult<()> {
		if self.closing.load(Ordering::SeqCst) {
			return Err(ApiError::Conflict("DAP handle is closing".to_owned()));
		}
		let text = match message {
			Message::Text(text) => text.to_string(),
			Message::Binary(bytes) => String::from_utf8(bytes.to_vec())
				.map_err(|_| ApiError::BadRequest("DAP WS payload must be UTF-8 JSON".to_owned()))?,
			Message::Ping(_) | Message::Pong(_) | Message::Close(_) => {
				return Ok(());
			},
		};
		let value: serde_json::Value = serde_json::from_str(&text)
			.map_err(|error| ApiError::BadRequest(format!("invalid DAP JSON message: {error}")))?;
		let normalized = serde_json::to_string(&value).map_err(anyhow::Error::from)?;
		self.write_frame(&normalized).await.map_err(ApiError::from)
	}

	pub async fn shutdown(&self) -> ApiResult<()> {
		if self.closing.swap(true, Ordering::SeqCst) {
			return Ok(());
		}

		let disconnect = serde_json::to_string(&json!({
			"seq": 0,
			"type": "request",
			"command": "disconnect",
			"arguments": {}
		}))
		.map_err(anyhow::Error::from)?;
		if let Err(error) = self.write_frame(&disconnect).await {
			debug!(?error, "failed to send dap disconnect before shutdown");
		}

		self.closed.notify_waiters();
		let _old_writer = self.writer.lock().await.take();

		let child = self.child.lock().await.take();
		if let Some(mut child) = child {
			match timeout(DELETE_GRACE_PERIOD, child.wait()).await {
				Ok(Ok(_status)) => {},
				Ok(Err(error)) => return Err(ApiError::Io(error)),
				Err(_timeout) => {
					if let Err(error) = child.kill().await {
						warn!(?error, "failed to kill dap child after disconnect timeout");
					}
					if let Err(error) = child.wait().await {
						warn!(?error, "failed waiting for killed dap child");
					}
				},
			}
		}

		Ok(())
	}

	fn new(reader: BoxReader, writer: BoxWriter, child: Option<tokio::process::Child>) -> Arc<Self> {
		let (messages, _) = tokio::sync::broadcast::channel(64);
		let handle = Arc::new(Self {
			child: tokio::sync::Mutex::new(child),
			writer: tokio::sync::Mutex::new(Some(writer)),
			messages,
			closed: Arc::new(tokio::sync::Notify::new()),
			closing: AtomicBool::new(false),
		});
		spawn_reader_task(Arc::clone(&handle), reader);
		handle
	}

	async fn write_frame(&self, message: &str) -> std::io::Result<()> {
		let mut writer = self.writer.lock().await;
		let Some(writer) = writer.as_mut() else {
			return Err(Error::new(ErrorKind::BrokenPipe, "DAP transport is closed"));
		};
		write_frame(writer, message).await
	}
}

pub fn spawn_idle_reaper(state: AppState, name: String, handle: Arc<Handle<DapHandle>>) {
	tokio::spawn(async move {
		loop {
			let idle_timeout = handle.idle_timeout();
			let remaining = idle_timeout.saturating_sub(handle.last_active().elapsed());
			tokio::select! {
				() = handle.on_close.notified() => break,
				() = sleep(remaining.min(REAPER_POLL_INTERVAL)) => {
					if handle.refcount() == 0 && handle.last_active().elapsed() >= idle_timeout {
						if let Some(removed) = state.dap.remove(&name) {
							removed.on_close.notify_waiters();
							if let Err(error) = removed.inner.shutdown().await {
								warn!(?error, dap_name = %name, "failed shutting down idle dap handle");
							}
						}
						break;
					}
				}
			}
		}
	});
}

pub async fn serve_websocket(socket: WebSocket, guard: HandleGuard<DapHandle>) {
	let handle = Arc::clone(guard.inner());
	let closed = handle.close_notifier();
	let mut adapter_messages = handle.subscribe();
	let (mut ws_tx, mut ws_rx) = socket.split();

	loop {
		tokio::select! {
			() = closed.notified() => break,
			message = adapter_messages.recv() => {
				match message {
					Ok(message) => {
						if ws_tx.send(Message::Text(message.into())).await.is_err() {
							break;
						}
					}
					Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
						warn!(skipped, "dap websocket lagged behind adapter output");
					}
					Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
				}
			}
			message = ws_rx.next() => {
				match message {
					Some(Ok(Message::Ping(payload))) => {
						if ws_tx.send(Message::Pong(payload)).await.is_err() {
							break;
						}
					}
					Some(Ok(Message::Pong(_))) => {}
					Some(Ok(Message::Close(_))) | None => break,
					Some(Ok(message)) => {
						if let Err(error) = handle.send_client_message(message).await {
							warn!(?error, "failed forwarding websocket dap message");
							break;
						}
					}
					Some(Err(error)) => {
						warn!(?error, "dap websocket receive error");
						break;
					}
				}
			}
		}
	}
}

fn spawn_stdio(config: DapSpawnConfig) -> ApiResult<Arc<DapHandle>> {
	if config.command.is_empty() {
		return Err(ApiError::BadRequest(
			"DAP stdio transport requires a non-empty command".to_owned(),
		));
	}

	let mut command = Command::new(&config.command);
	command
		.args(&config.args)
		.envs(&config.env)
		.stdin(std::process::Stdio::piped())
		.stdout(std::process::Stdio::piped())
		.stderr(std::process::Stdio::piped())
		.kill_on_drop(true);
	let mut child = command
		.spawn()
		.with_context(|| format!("failed to spawn DAP adapter `{}`", config.command))?;
	if let Some(stderr) = child.stderr.take() {
		drop(spawn_stderr_pump(stderr, "DAP adapter stdio"));
	}
	let stdin = child
		.stdin
		.take()
		.ok_or_else(|| anyhow!("spawned DAP adapter missing stdin pipe"))?;
	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| anyhow!("spawned DAP adapter missing stdout pipe"))?;

	Ok(DapHandle::new(Box::new(stdout), Box::new(stdin), Some(child)))
}

async fn spawn_tcp(config: DapSpawnConfig) -> ApiResult<Arc<DapHandle>> {
	let host = config
		.host
		.clone()
		.ok_or_else(|| ApiError::BadRequest("DAP tcp transport requires `host`".to_owned()))?;
	let port = config
		.port
		.ok_or_else(|| ApiError::BadRequest("DAP tcp transport requires `port`".to_owned()))?;

	let (child, stderr_capture) = if config.command.is_empty() {
		(None, None)
	} else {
		let mut command = Command::new(&config.command);
		command
			.args(&config.args)
			.envs(&config.env)
			.stdin(std::process::Stdio::null())
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::piped())
			.kill_on_drop(true);
		let mut child = command
			.spawn()
			.with_context(|| format!("failed to spawn DAP adapter `{}`", config.command))?;
		let stderr_capture = child
			.stderr
			.take()
			.map(|stderr| spawn_stderr_pump(stderr, "DAP adapter tcp launcher"));
		(Some(child), stderr_capture)
	};

	let stream = match connect_tcp(&host, port, config.retry_ms, config.retry_attempts).await {
		Ok(stream) => stream,
		Err(error) => {
			if let Some(mut child) = child {
				if let Err(kill_error) = child.kill().await {
					warn!(?kill_error, "failed to kill dap child after tcp connect error");
				}
				if let Err(wait_error) = child.wait().await {
					warn!(?wait_error, "failed waiting for dap child after tcp connect error");
				}
			}
			let mut message =
				format!("failed to connect to DAP adapter tcp transport at {host}:{port}: {error}");
			if let Some(stderr_tail) = drain_stderr_capture(stderr_capture).await {
				message.push_str("; stderr tail: ");
				message.push_str(&stderr_tail);
			}
			return Err(ApiError::Internal(anyhow!(message)));
		},
	};
	if let Some(stderr_capture) = stderr_capture {
		drop(stderr_capture);
	}
	let (reader, writer) = stream.into_split();
	Ok(DapHandle::new(Box::new(reader), Box::new(writer), child))
}

#[derive(Debug, Default)]
struct StderrTail {
	bytes: VecDeque<u8>,
}

impl StderrTail {
	fn push(&mut self, chunk: &[u8]) {
		if chunk.len() >= STDERR_TAIL_LIMIT_BYTES {
			self.bytes.clear();
			self.bytes.extend(
				chunk[chunk.len().saturating_sub(STDERR_TAIL_LIMIT_BYTES)..]
					.iter()
					.copied(),
			);
			return;
		}
		let overflow = self
			.bytes
			.len()
			.saturating_add(chunk.len())
			.saturating_sub(STDERR_TAIL_LIMIT_BYTES);
		if overflow > 0 {
			self.bytes.drain(..overflow);
		}
		self.bytes.extend(chunk.iter().copied());
	}

	fn snapshot(&self) -> Option<String> {
		if self.bytes.is_empty() {
			return None;
		}
		let bytes = self.bytes.iter().copied().collect::<Vec<_>>();
		Some(String::from_utf8_lossy(&bytes).trim().to_owned())
	}
}

#[derive(Debug)]
struct StderrCapture {
	tail: Arc<Mutex<StderrTail>>,
	task: tokio::task::JoinHandle<()>,
}

fn spawn_stderr_pump<R>(mut stderr: R, context: &'static str) -> StderrCapture
where
	R: AsyncRead + Send + Unpin + 'static,
{
	let tail = Arc::new(Mutex::new(StderrTail::default()));
	let captured = Arc::clone(&tail);
	let task = tokio::spawn(async move {
		let mut chunk = [0_u8; 4096];
		loop {
			match stderr.read(&mut chunk).await {
				Ok(0) => {
					if let Some(stderr_tail) = captured.lock().ok().and_then(|tail| tail.snapshot()) {
						warn!(stderr = %stderr_tail, "{context} stderr");
					}
					break;
				},
				Ok(read) => {
					if let Ok(mut tail) = captured.lock() {
						tail.push(&chunk[..read]);
					}
				},
				Err(error) => {
					let stderr_tail = captured.lock().ok().and_then(|tail| tail.snapshot());
					warn!(?error, stderr = ?stderr_tail, "{context} stderr drain failed");
					break;
				},
			}
		}
	});
	StderrCapture { tail, task }
}

async fn drain_stderr_capture(stderr_capture: Option<StderrCapture>) -> Option<String> {
	let stderr_capture = stderr_capture?;
	let _ = stderr_capture.task.await;
	stderr_capture
		.tail
		.lock()
		.ok()
		.and_then(|tail| tail.snapshot())
}

async fn connect_tcp(
	host: &str,
	port: u16,
	retry_ms: u32,
	retry_attempts: u32,
) -> std::io::Result<TcpStream> {
	let addr = format!("{host}:{port}");
	for attempt in 0..=retry_attempts {
		match timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(&addr)).await {
			Ok(Ok(stream)) => return Ok(stream),
			Ok(Err(error)) if attempt < retry_attempts => {
				debug!(?error, %addr, attempt, retry_attempts, "retrying dap tcp connect");
				sleep(Duration::from_millis(u64::from(retry_ms))).await;
			},
			Err(error) if attempt < retry_attempts => {
				debug!(%addr, attempt, retry_attempts, %error, "retrying timed out dap tcp connect");
				sleep(Duration::from_millis(u64::from(retry_ms))).await;
			},
			Ok(Err(error)) => return Err(error),
			Err(_elapsed) => {
				return Err(Error::new(
					ErrorKind::TimedOut,
					format!("timed out connecting to DAP adapter tcp transport at {addr}"),
				));
			},
		}
	}
	unreachable!("retry loop always returns on success or final failure")
}

fn spawn_reader_task(handle: Arc<DapHandle>, reader: BoxReader) {
	tokio::spawn(async move {
		let mut reader = BufReader::new(reader);
		loop {
			match read_frame(&mut reader).await {
				Ok(Some(message)) => {
					let _ignored = handle.messages.send(message);
				},
				Ok(None) => break,
				Err(error) => {
					warn!(?error, "dap reader loop failed");
					break;
				},
			}
		}
		handle.closed.notify_waiters();
	});
}

async fn read_frame<R>(reader: &mut R) -> std::io::Result<Option<String>>
where
	R: AsyncRead + Unpin,
{
	let mut header = Vec::new();
	let mut byte = [0_u8; 1];
	loop {
		match reader.read_exact(&mut byte).await {
			Ok(_read) => {
				header.push(byte[0]);
				if header.len() > HEADER_LIMIT_BYTES {
					return Err(Error::new(ErrorKind::InvalidData, "DAP header too large"));
				}
				if header.ends_with(b"\r\n\r\n") {
					break;
				}
			},
			Err(error) if error.kind() == ErrorKind::UnexpectedEof && header.is_empty() => {
				return Ok(None);
			},
			Err(error) => return Err(error),
		}
	}

	let header_text = std::str::from_utf8(&header)
		.map_err(|_| Error::new(ErrorKind::InvalidData, "DAP header is not valid UTF-8"))?;
	let content_length = parse_content_length(header_text)?;
	let mut body = vec![0_u8; content_length];
	reader.read_exact(&mut body).await?;
	String::from_utf8(body)
		.map(Some)
		.map_err(|_| Error::new(ErrorKind::InvalidData, "DAP body is not valid UTF-8"))
}

fn parse_content_length(header: &str) -> std::io::Result<usize> {
	header
		.lines()
		.find_map(|line| {
			let (name, value) = line.split_once(':')?;
			name
				.trim()
				.eq_ignore_ascii_case("Content-Length")
				.then_some(value.trim())
		})
		.ok_or_else(|| Error::new(ErrorKind::InvalidData, "DAP frame missing Content-Length"))?
		.parse::<usize>()
		.map_err(|_| Error::new(ErrorKind::InvalidData, "invalid DAP Content-Length"))
}

async fn write_frame<W>(writer: &mut W, message: &str) -> std::io::Result<()>
where
	W: AsyncWrite + Unpin,
{
	writer
		.write_all(format!("Content-Length: {}\r\n\r\n", message.len()).as_bytes())
		.await?;
	writer.write_all(message.as_bytes()).await?;
	writer.flush().await
}
