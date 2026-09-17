use std::{
	collections::HashMap,
	net::TcpListener,
	process::Stdio,
	sync::{
		Arc, LazyLock, Weak,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	time::Duration,
};

use anyhow::{Context, Result, anyhow};
use axum::{
	extract::ws::{
		CloseFrame as AxumCloseFrame, Message as AxumMessage, WebSocket, WebSocketUpgrade,
	},
	response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::{
	io::{self, AsyncRead},
	process::Command,
	sync::mpsc,
	task::JoinHandle,
	time::sleep,
};
use tokio_tungstenite::{
	connect_async,
	tungstenite::{
		Message as TungsteniteMessage,
		protocol::{CloseFrame as TungsteniteCloseFrame, frame::coding::CloseCode},
	},
};
use tracing::{debug, warn};

use crate::{
	named::{Handle, HandleGuard, HandleScope, NamedRegistry},
	protocol::{
		error::{ApiError, ApiResult},
		responses::CdpHandleResponseKind,
	},
	state::{CdpHandle, CdpSpawnedProcess},
};

const DEFAULT_IDLE_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const SPAWN_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const VERSION_DISCOVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);

static IDLE_TIMEOUT_MS: AtomicU64 = AtomicU64::new(DEFAULT_IDLE_TIMEOUT_MS);
static CREATE_LOCK: LazyLock<tokio::sync::Mutex<()>> =
	LazyLock::new(|| tokio::sync::Mutex::new(()));
static HANDLE_TRACKERS: LazyLock<parking_lot::Mutex<HashMap<usize, HandleTracker>>> =
	LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

#[derive(Debug, Deserialize)]
struct VersionResponse {
	#[serde(rename = "webSocketDebuggerUrl")]
	web_socket_debugger_url: String,
}

#[derive(Clone)]
struct SpawnMonitor {
	exited:   Arc<AtomicBool>,
	notified: Arc<tokio::sync::Notify>,
}

struct BuiltHandle {
	handle:        CdpHandle,
	spawn_monitor: Option<SpawnMonitor>,
}

#[derive(Clone)]
struct HandleTracker {
	registry:     Weak<NamedRegistry<CdpHandle>>,
	name:         String,
	spawned:      bool,
	child_exited: Arc<AtomicBool>,
}

pub fn idle_timeout() -> Duration {
	Duration::from_millis(default_idle_timeout_ms())
}

pub fn set_idle_timeout_for_tests(duration: Duration) {
	let millis = duration.as_millis();
	let clamped = u64::try_from(millis).unwrap_or(u64::MAX);
	IDLE_TIMEOUT_MS.store(clamped, Ordering::Relaxed);
}
#[must_use]
pub fn default_idle_timeout_ms() -> u64 {
	IDLE_TIMEOUT_MS.load(Ordering::Relaxed)
}

pub async fn get_or_create_handle(
	registry: &Arc<NamedRegistry<CdpHandle>>,
	name: &str,
	config: crate::protocol::requests::NamedHandleConfig,
	scope: HandleScope,
) -> ApiResult<(Arc<Handle<CdpHandle>>, bool)> {
	if let Some(existing) = registry.get(name) {
		if existing.scope() != scope {
			return Err(ApiError::Conflict(format!(
				"cdp handle {name} already exists with different scope"
			)));
		}
		return Ok((existing, false));
	}

	let _create_guard = CREATE_LOCK.lock().await;
	if let Some(existing) = registry.get(name) {
		if existing.scope() != scope {
			return Err(ApiError::Conflict(format!(
				"cdp handle {name} already exists with different scope"
			)));
		}
		return Ok((existing, false));
	}

	let idle_timeout = config_idle_timeout(&config);
	let BuiltHandle { handle, spawn_monitor } = build_handle(config).await?;
	let fresh = Arc::new(handle);
	let stored =
		registry.get_or_insert_with_timeout(name, idle_timeout, scope, || Arc::clone(&fresh));
	let created = Arc::ptr_eq(&stored.inner, &fresh);
	if created {
		register_handle_tracking(&stored, registry, name, spawn_monitor);
		spawn_idle_reaper(Arc::clone(registry), name.to_owned(), Arc::clone(&stored));
		Ok((stored, true))
	} else {
		fresh.shutdown().await;
		if stored.scope() != scope {
			return Err(ApiError::Conflict(format!(
				"cdp handle {name} already exists with different scope"
			)));
		}
		Ok((stored, false))
	}
}

pub fn metadata(
	name: String,
	handle: &Handle<CdpHandle>,
) -> crate::protocol::responses::CdpHandleResponse {
	crate::protocol::responses::CdpHandleResponse {
		name,
		kind: match handle.inner.kind() {
			crate::state::CdpHandleKind::Spawned => CdpHandleResponseKind::Spawned,
			crate::state::CdpHandleKind::Attached => CdpHandleResponseKind::Attached,
		},
		ws_url: handle.inner.ws_url().to_owned(),
		ref_count: handle.refcount(),
		last_active_ms: saturating_millis(handle.last_active().elapsed()),
		args: handle.inner.args().to_vec(),
		headless: handle.inner.headless(),
		user_prefs: handle.inner.user_prefs().cloned(),
	}
}

pub fn websocket_response(
	upgrade: WebSocketUpgrade,
	handle: Arc<Handle<CdpHandle>>,
	name: String,
) -> Response {
	let ws_url = handle.inner.ws_url().to_owned();
	let guard = handle.retain();
	upgrade.on_upgrade(move |socket| {
		let handle = Arc::clone(&handle);
		async move {
			if let Err(error) = proxy_websocket(socket, ws_url, handle, guard).await {
				warn!(cdp_name = %name, %error, "cdp websocket proxy closed with error");
			}
		}
	})
}

pub async fn remove_handle(registry: &NamedRegistry<CdpHandle>, name: &str) -> bool {
	remove_current_handle(registry, name, None).await
}

async fn build_handle(
	config: crate::protocol::requests::NamedHandleConfig,
) -> ApiResult<BuiltHandle> {
	match config {
		crate::protocol::requests::NamedHandleConfig::CdpAttach { cdp_url, .. } => {
			Ok(BuiltHandle { handle: CdpHandle::attached(cdp_url), spawn_monitor: None })
		},
		crate::protocol::requests::NamedHandleConfig::CdpSpawn {
			path,
			args,
			headless,
			user_prefs,
			..
		} => {
			let path = resolve_spawn_path(path).await?;
			let (process, ws_url, spawn_monitor) = spawn_process(path, args.clone(), headless).await?;
			Ok(BuiltHandle {
				handle:        CdpHandle::spawned(ws_url, args, headless, user_prefs, process),
				spawn_monitor: Some(spawn_monitor),
			})
		},
		_ => Err(ApiError::BadRequest(
			"/cdp endpoints only accept cdp-spawn or cdp-attach configs".to_owned(),
		)),
	}
}

async fn resolve_spawn_path(path: Option<String>) -> ApiResult<String> {
	if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
		return Ok(path);
	}
	discover_default_browser().await.ok_or_else(|| {
		ApiError::BadRequest(
			"cdp-spawn path was omitted and no default Chromium/Chrome executable was found on the \
			 server"
				.to_owned(),
		)
	})
}

async fn discover_default_browser() -> Option<String> {
	for command in default_browser_commands() {
		if let Some(path) = which_executable(command).await {
			return Some(path);
		}
	}
	for path in default_browser_paths() {
		if std::path::Path::new(path).is_file() {
			return Some(path.to_string());
		}
	}
	None
}

async fn which_executable(command: &str) -> Option<String> {
	let output = Command::new(if cfg!(windows) { "where" } else { "which" })
		.arg(command)
		.output()
		.await
		.ok()?;
	if !output.status.success() {
		return None;
	}
	String::from_utf8(output.stdout)
		.ok()?
		.lines()
		.map(str::trim)
		.find(|line| !line.is_empty())
		.map(ToOwned::to_owned)
}

const fn default_browser_commands() -> &'static [&'static str] {
	if cfg!(target_os = "macos") {
		&["chromium", "google-chrome", "chromium-browser"]
	} else if cfg!(windows) {
		&["chrome.exe", "msedge.exe", "chromium.exe"]
	} else {
		&["chromium", "google-chrome", "chromium-browser", "google-chrome-stable"]
	}
}

const fn default_browser_paths() -> &'static [&'static str] {
	if cfg!(target_os = "macos") {
		&[
			"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
			"/Applications/Chromium.app/Contents/MacOS/Chromium",
			"/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
		]
	} else if cfg!(windows) {
		&[
			"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
			"C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
			"C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
		]
	} else {
		&[
			"/usr/bin/chromium",
			"/usr/bin/google-chrome",
			"/usr/bin/chromium-browser",
			"/usr/bin/google-chrome-stable",
			"/snap/bin/chromium",
		]
	}
}
fn config_idle_timeout(config: &crate::protocol::requests::NamedHandleConfig) -> Duration {
	match config {
		crate::protocol::requests::NamedHandleConfig::CdpAttach { idle_timeout_ms, .. }
		| crate::protocol::requests::NamedHandleConfig::CdpSpawn { idle_timeout_ms, .. } => {
			Duration::from_millis(idle_timeout_ms.unwrap_or(default_idle_timeout_ms()))
		},
		_ => idle_timeout(),
	}
}

async fn spawn_process(
	path: String,
	args: Vec<String>,
	headless: bool,
) -> ApiResult<(CdpSpawnedProcess, String, SpawnMonitor)> {
	let port = reserve_loopback_port().map_err(ApiError::Internal)?;
	let mut command = Command::new(&path);
	command
		.args(&args)
		.arg(format!("--remote-debugging-port={port}"))
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true);
	if headless {
		command.arg("--headless=new");
	}

	let mut child = command
		.spawn()
		.map_err(|error| ApiError::BadRequest(format!("failed to spawn {path}: {error}")))?;
	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| ApiError::Internal(anyhow!("spawned browser missing stdout pipe")))?;
	let stderr = child
		.stderr
		.take()
		.ok_or_else(|| ApiError::Internal(anyhow!("spawned browser missing stderr pipe")))?;
	let (io_done_tx, io_done_rx) = mpsc::unbounded_channel();
	let io_tasks =
		vec![spawn_io_drain(stdout, io_done_tx.clone()), spawn_io_drain(stderr, io_done_tx)];
	let spawn_monitor = spawn_exit_monitor(io_done_rx);

	if let Err(error) = wait_for_devtools(port, SPAWN_DISCOVERY_TIMEOUT).await {
		let process = CdpSpawnedProcess::new(child, io_tasks);
		process.kill().await;
		return Err(ApiError::BadRequest(format!(
			"timed out waiting for CDP startup from {path}: {error}"
		)));
	}

	let ws_url = match fetch_browser_ws_url(port).await {
		Ok(ws_url) => ws_url,
		Err(error) => {
			let process = CdpSpawnedProcess::new(child, io_tasks);
			process.kill().await;
			return Err(error);
		},
	};

	Ok((CdpSpawnedProcess::new(child, io_tasks), ws_url, spawn_monitor))
}

fn reserve_loopback_port() -> Result<u16> {
	let listener = TcpListener::bind("127.0.0.1:0").context("binding ephemeral CDP port")?;
	let port = listener
		.local_addr()
		.context("reading ephemeral CDP port")?
		.port();
	drop(listener);
	Ok(port)
}

fn spawn_io_drain<R>(mut reader: R, done_tx: mpsc::UnboundedSender<()>) -> JoinHandle<()>
where
	R: AsyncRead + Unpin + Send + 'static,
{
	tokio::spawn(async move {
		if let Err(error) = io::copy(&mut reader, &mut io::sink()).await {
			debug!(%error, "cdp io drain stopped");
		}
		let _ = done_tx.send(());
	})
}

fn spawn_exit_monitor(mut io_done_rx: mpsc::UnboundedReceiver<()>) -> SpawnMonitor {
	let exited = Arc::new(AtomicBool::new(false));
	let notified = Arc::new(tokio::sync::Notify::new());
	let exited_for_task = Arc::clone(&exited);
	let notified_for_task = Arc::clone(&notified);
	tokio::spawn(async move {
		let mut completed = 0_u8;
		while io_done_rx.recv().await.is_some() {
			completed = completed.saturating_add(1);
			if completed >= 2 {
				exited_for_task.store(true, Ordering::SeqCst);
				notified_for_task.notify_waiters();
				break;
			}
		}
	});
	SpawnMonitor { exited, notified }
}

async fn wait_for_devtools(port: u16, timeout: Duration) -> Result<()> {
	let client = reqwest::Client::new();
	let url = format!("http://127.0.0.1:{port}/json/version");
	let deadline = tokio::time::Instant::now() + timeout;
	let mut last_error = None;
	loop {
		match client.get(&url).send().await {
			Ok(response) if response.status().is_success() => return Ok(()),
			Ok(_) => {},
			Err(error) => last_error = Some(anyhow!(error)),
		}
		if tokio::time::Instant::now() >= deadline {
			break;
		}
		sleep(VERSION_DISCOVERY_POLL_INTERVAL).await;
	}
	match last_error {
		Some(error) => Err(anyhow!("{url} did not become ready: {error}")),
		None => Err(anyhow!("{url} did not become ready")),
	}
}

async fn fetch_browser_ws_url(port: u16) -> ApiResult<String> {
	let client = reqwest::Client::new();
	let url = format!("http://127.0.0.1:{port}/json/version");
	let response = client.get(&url).send().await.map_err(|error| {
		ApiError::BadRequest(format!("failed to query CDP version endpoint at {url}: {error}"))
	})?;
	if !response.status().is_success() {
		return Err(ApiError::BadRequest(format!(
			"CDP version endpoint at {url} returned {} during startup",
			response.status()
		)));
	}
	let version = response.json::<VersionResponse>().await.map_err(|error| {
		ApiError::Internal(anyhow!(error).context("failed to decode /json/version"))
	})?;
	Ok(version.web_socket_debugger_url)
}

fn register_handle_tracking(
	handle: &Arc<Handle<CdpHandle>>,
	registry: &Arc<NamedRegistry<CdpHandle>>,
	name: &str,
	spawn_monitor: Option<SpawnMonitor>,
) {
	let child_exited = spawn_monitor
		.as_ref()
		.map_or_else(|| Arc::new(AtomicBool::new(false)), |monitor| Arc::clone(&monitor.exited));
	HANDLE_TRACKERS
		.lock()
		.insert(handle_key(handle), HandleTracker {
			registry: Arc::downgrade(registry),
			name: name.to_owned(),
			spawned: spawn_monitor.is_some(),
			child_exited,
		});
	if let Some(monitor) = spawn_monitor {
		let handle_for_task = Arc::clone(handle);
		let registry = Arc::downgrade(registry);
		let name = name.to_owned();
		tokio::spawn(async move {
			if !monitor.exited.load(Ordering::SeqCst) {
				monitor.notified.notified().await;
			}
			let Some(registry) = registry.upgrade() else {
				return;
			};
			let _ = remove_current_handle(registry.as_ref(), &name, Some(&handle_for_task)).await;
		});
	}
}

fn handle_key(handle: &Arc<Handle<CdpHandle>>) -> usize {
	Arc::as_ptr(handle) as usize
}

fn tracker_for(handle: &Arc<Handle<CdpHandle>>) -> Option<HandleTracker> {
	HANDLE_TRACKERS.lock().get(&handle_key(handle)).cloned()
}

fn clear_handle_tracking(handle: &Arc<Handle<CdpHandle>>) {
	HANDLE_TRACKERS.lock().remove(&handle_key(handle));
}

async fn remove_current_handle(
	registry: &NamedRegistry<CdpHandle>,
	name: &str,
	expected: Option<&Arc<Handle<CdpHandle>>>,
) -> bool {
	let handle = {
		let _create_guard = CREATE_LOCK.lock().await;
		let Some(current) = registry.get(name) else {
			return false;
		};
		if let Some(expected) = expected
			&& !Arc::ptr_eq(&current, expected)
		{
			return false;
		}
		registry.remove(name)
	};
	let Some(handle) = handle else {
		return false;
	};
	clear_handle_tracking(&handle);
	handle.on_close.notify_waiters();
	handle.inner.abort_reaper();
	handle.inner.shutdown().await;
	true
}

async fn maybe_remove_after_disconnect(handle: Arc<Handle<CdpHandle>>) {
	let Some(tracker) = tracker_for(&handle) else {
		return;
	};
	if handle.refcount() != 0 {
		return;
	}
	if tracker.spawned && !tracker.child_exited.load(Ordering::SeqCst) {
		return;
	}
	let Some(registry) = tracker.registry.upgrade() else {
		return;
	};
	let _ = remove_current_handle(registry.as_ref(), &tracker.name, Some(&handle)).await;
}

fn spawn_idle_reaper(
	registry: Arc<NamedRegistry<CdpHandle>>,
	name: String,
	handle: Arc<Handle<CdpHandle>>,
) {
	let handle_for_task = Arc::clone(&handle);
	let abort_handle = tokio::spawn(async move {
		loop {
			let idle_for = handle_for_task.idle_timeout();
			let remaining = idle_for.saturating_sub(handle_for_task.last_active().elapsed());
			tokio::select! {
				() = handle_for_task.on_close.notified() => return,
				() = sleep(remaining.min(VERSION_DISCOVERY_POLL_INTERVAL)) => {
					let should_remove =
						handle_for_task.refcount() == 0 && handle_for_task.last_active().elapsed() >= idle_for;
					if !should_remove {
						continue;
					}
					let _ = remove_current_handle(registry.as_ref(), &name, Some(&handle_for_task)).await;
					return;
				}
			}
		}
	})
	.abort_handle();
	handle.inner.set_reaper(abort_handle);
}

async fn proxy_websocket(
	client_socket: WebSocket,
	upstream_url: String,
	handle: Arc<Handle<CdpHandle>>,
	guard: HandleGuard<CdpHandle>,
) -> anyhow::Result<()> {
	let result = async {
		let (upstream_socket, _) = connect_async(&upstream_url)
			.await
			.with_context(|| format!("failed to connect upstream CDP websocket {upstream_url}"))?;
		let (mut client_sink, mut client_stream) = client_socket.split();
		let (mut upstream_sink, mut upstream_stream) = upstream_socket.split();

		let client_to_upstream = async {
			while let Some(message) = client_stream.next().await {
				let message = message.context("client websocket receive failed")?;
				let message = map_client_message(message);
				upstream_sink
					.send(message)
					.await
					.context("sending client frame upstream failed")?;
			}
			anyhow::Result::<()>::Ok(())
		};

		let upstream_to_client = async {
			while let Some(message) = upstream_stream.next().await {
				let message = message.context("upstream websocket receive failed")?;
				let Some(message) = map_upstream_message(message) else {
					continue;
				};
				client_sink
					.send(message)
					.await
					.context("sending upstream frame to client failed")?;
			}
			anyhow::Result::<()>::Ok(())
		};

		let (left, right) = tokio::join!(client_to_upstream, upstream_to_client);
		left?;
		right?;
		Ok(())
	}
	.await;
	drop(guard);
	maybe_remove_after_disconnect(handle).await;
	result
}

fn map_client_message(message: AxumMessage) -> TungsteniteMessage {
	match message {
		AxumMessage::Text(text) => TungsteniteMessage::Text(text.to_string().into()),
		AxumMessage::Binary(bytes) => TungsteniteMessage::Binary(bytes),
		AxumMessage::Ping(bytes) => TungsteniteMessage::Ping(bytes),
		AxumMessage::Pong(bytes) => TungsteniteMessage::Pong(bytes),
		AxumMessage::Close(frame) => {
			TungsteniteMessage::Close(frame.map(map_close_frame_to_upstream))
		},
	}
}

fn map_upstream_message(message: TungsteniteMessage) -> Option<AxumMessage> {
	match message {
		TungsteniteMessage::Text(text) => Some(AxumMessage::Text(text.to_string().into())),
		TungsteniteMessage::Binary(bytes) => Some(AxumMessage::Binary(bytes)),
		TungsteniteMessage::Ping(bytes) => Some(AxumMessage::Ping(bytes)),
		TungsteniteMessage::Pong(bytes) => Some(AxumMessage::Pong(bytes)),
		TungsteniteMessage::Close(frame) => {
			Some(AxumMessage::Close(frame.map(map_close_frame_to_client)))
		},
		TungsteniteMessage::Frame(_) => None,
	}
}

fn map_close_frame_to_upstream(frame: AxumCloseFrame) -> TungsteniteCloseFrame {
	TungsteniteCloseFrame {
		code:   map_close_code_to_upstream(frame.code),
		reason: frame.reason.to_string().into(),
	}
}

fn map_close_frame_to_client(frame: TungsteniteCloseFrame) -> AxumCloseFrame {
	AxumCloseFrame {
		code:   map_close_code_to_client(frame.code),
		reason: frame.reason.to_string().into(),
	}
}

const fn map_close_code_to_upstream(code: u16) -> CloseCode {
	match code {
		1000 => CloseCode::Normal,
		1001 => CloseCode::Away,
		1002 => CloseCode::Protocol,
		1003 => CloseCode::Unsupported,
		1005 => CloseCode::Status,
		1006 => CloseCode::Abnormal,
		1007 => CloseCode::Invalid,
		1008 => CloseCode::Policy,
		1009 => CloseCode::Size,
		1010 => CloseCode::Extension,
		1011 => CloseCode::Error,
		1012 => CloseCode::Restart,
		1013 => CloseCode::Again,
		1015 => CloseCode::Tls,
		3000..=4999 => CloseCode::Library(code),
		_ => CloseCode::Bad(code),
	}
}

const fn map_close_code_to_client(code: CloseCode) -> u16 {
	match code {
		CloseCode::Normal => 1000,
		CloseCode::Away => 1001,
		CloseCode::Protocol => 1002,
		CloseCode::Unsupported => 1003,
		CloseCode::Status => 1005,
		CloseCode::Abnormal => 1006,
		CloseCode::Invalid => 1007,
		CloseCode::Policy => 1008,
		CloseCode::Size => 1009,
		CloseCode::Extension => 1010,
		CloseCode::Error => 1011,
		CloseCode::Restart => 1012,
		CloseCode::Again => 1013,
		CloseCode::Tls => 1015,
		CloseCode::Library(code) | CloseCode::Iana(code) | CloseCode::Bad(code) => code,
		_ => 1000,
	}
}

fn saturating_millis(duration: Duration) -> u64 {
	u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use axum::{Json, Router, routing::get};
	use serde_json::json;
	use tokio::net::TcpListener;

	use super::*;

	#[tokio::test]
	async fn wait_for_devtools_polls_until_endpoint_is_ready() {
		let port = reserve_loopback_port().expect("reserve CDP port");
		tokio::spawn(async move {
			sleep(Duration::from_millis(150)).await;
			let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
				.await
				.expect("bind delayed fixture");
			let router = Router::new().route(
				"/json/version",
				get(|| async {
					Json(json!({ "webSocketDebuggerUrl": "ws://127.0.0.1:9222/devtools/browser/test" }))
				}),
			);
			let _ = axum::serve(listener, router).await;
		});

		wait_for_devtools(port, Duration::from_secs(1))
			.await
			.expect("waits for delayed CDP endpoint");
	}

	#[tokio::test]
	async fn wait_for_devtools_times_out_when_endpoint_never_appears() {
		let port = reserve_loopback_port().expect("reserve unused port");
		let error = wait_for_devtools(port, Duration::from_millis(250))
			.await
			.expect_err("missing CDP endpoint should time out");
		assert!(error.to_string().contains("/json/version"));
	}
}
