//! Top-level application state shared by every handler.

use std::{
	collections::HashMap,
	fmt,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
};

use tokio::{
	io::AsyncWrite,
	process::{Child, ChildStdin},
	sync::{Mutex, Notify, broadcast, oneshot},
};

use crate::{
	fs_ops::etag_cache::EtagCache, lsp_tunnel::LspConfig, named::NamedRegistry,
	session::SessionRegistry,
};

pub struct EvalHandle {
	pub kernel:          Arc<crate::eval_kernel::EvalKernel>,
	pub transport:       crate::protocol::requests::EvalTransport,
	pub idle_timeout_ms: u64,
	pub reaper:          Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for EvalHandle {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("EvalHandle")
			.field("lang", &self.kernel.lang())
			.field("transport", &self.transport)
			.field("idle_timeout_ms", &self.idle_timeout_ms)
			.finish_non_exhaustive()
	}
}

impl EvalHandle {
	#[must_use]
	pub fn new(
		kernel: crate::eval_kernel::EvalKernel,
		transport: crate::protocol::requests::EvalTransport,
		idle_timeout_ms: u64,
	) -> Self {
		Self { kernel: Arc::new(kernel), transport, idle_timeout_ms, reaper: Mutex::new(None) }
	}
}

#[derive(Debug)]
pub struct LspHandle {
	pub config:            LspConfig,
	pub initialize_result: serde_json::Value,
	pub capabilities:      serde_json::Value,
	pub project_loaded:    AtomicBool,
	pub diagnostics:       Mutex<HashMap<String, serde_json::Value>>,
	pub messages_tx:       broadcast::Sender<String>,
	pub stdin:             Mutex<ChildStdin>,
	pub child:             Mutex<Child>,
	pub pending:           Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
	pub next_request_id:   AtomicU64,
	pub document_versions: Mutex<HashMap<String, i32>>,
}

pub struct DapHandle {
	pub(crate) child:    Mutex<Option<Child>>,
	pub(crate) writer:   Mutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>,
	pub(crate) messages: broadcast::Sender<String>,
	pub(crate) closed:   Arc<Notify>,
	pub(crate) closing:  AtomicBool,
}

impl fmt::Debug for DapHandle {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("DapHandle")
			.field("closing", &self.closing.load(Ordering::SeqCst))
			.finish_non_exhaustive()
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdpHandleKind {
	Spawned,
	Attached,
}

pub struct CdpSpawnedProcess {
	child:    Mutex<Option<Child>>,
	io_tasks: parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl fmt::Debug for CdpSpawnedProcess {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("CdpSpawnedProcess").finish_non_exhaustive()
	}
}

impl CdpSpawnedProcess {
	#[must_use]
	pub fn new(child: Child, io_tasks: Vec<tokio::task::JoinHandle<()>>) -> Self {
		Self { child: Mutex::new(Some(child)), io_tasks: parking_lot::Mutex::new(io_tasks) }
	}

	pub async fn kill(&self) {
		let mut child = self.child.lock().await;
		if let Some(mut child) = child.take() {
			if let Err(error) = child.kill().await {
				tracing::debug!(%error, "cdp child kill returned error");
			}
			if let Err(error) = child.wait().await {
				tracing::debug!(%error, "cdp child wait returned error");
			}
		}
		for task in self.io_tasks.lock().drain(..) {
			task.abort();
		}
	}
}

pub struct CdpHandle {
	kind:       CdpHandleKind,
	ws_url:     String,
	args:       Vec<String>,
	headless:   Option<bool>,
	user_prefs: Option<std::collections::BTreeMap<String, serde_json::Value>>,
	process:    Option<CdpSpawnedProcess>,
	reaper:     parking_lot::Mutex<Option<tokio::task::AbortHandle>>,
}

impl fmt::Debug for CdpHandle {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("CdpHandle")
			.field("kind", &self.kind)
			.field("ws_url", &self.ws_url)
			.field("process", &self.process)
			.finish_non_exhaustive()
	}
}

impl CdpHandle {
	#[must_use]
	pub const fn attached(ws_url: String) -> Self {
		Self {
			kind: CdpHandleKind::Attached,
			ws_url,
			args: Vec::new(),
			headless: None,
			user_prefs: None,
			process: None,
			reaper: parking_lot::Mutex::new(None),
		}
	}

	#[must_use]
	pub const fn spawned(
		ws_url: String,
		args: Vec<String>,
		headless: bool,
		user_prefs: Option<std::collections::BTreeMap<String, serde_json::Value>>,
		process: CdpSpawnedProcess,
	) -> Self {
		Self {
			kind: CdpHandleKind::Spawned,
			ws_url,
			args,
			headless: Some(headless),
			user_prefs,
			process: Some(process),
			reaper: parking_lot::Mutex::new(None),
		}
	}

	#[must_use]
	pub const fn kind(&self) -> CdpHandleKind {
		self.kind
	}

	#[must_use]
	pub fn ws_url(&self) -> &str {
		&self.ws_url
	}

	#[must_use]
	pub fn args(&self) -> &[String] {
		&self.args
	}

	#[must_use]
	pub const fn headless(&self) -> Option<bool> {
		self.headless
	}

	#[must_use]
	pub const fn user_prefs(
		&self,
	) -> Option<&std::collections::BTreeMap<String, serde_json::Value>> {
		self.user_prefs.as_ref()
	}

	pub fn set_reaper(&self, abort_handle: tokio::task::AbortHandle) {
		*self.reaper.lock() = Some(abort_handle);
	}

	pub fn abort_reaper(&self) {
		let abort_handle = self.reaper.lock().take();
		if let Some(abort_handle) = abort_handle {
			abort_handle.abort();
		}
	}

	pub async fn shutdown(&self) {
		if let Some(process) = &self.process {
			process.kill().await;
		}
	}
}

#[derive(Debug, Default, Clone)]
pub struct AppState {
	pub sessions:   Arc<SessionRegistry>,
	pub eval:       Arc<NamedRegistry<EvalHandle>>,
	pub lsp:        Arc<NamedRegistry<LspHandle>>,
	pub dap:        Arc<NamedRegistry<DapHandle>>,
	pub cdp:        Arc<NamedRegistry<CdpHandle>>,
	pub etag_cache: Arc<EtagCache>,
	pub auth_token: Option<Arc<String>>,
}

impl AppState {
	#[must_use]
	pub fn new() -> Self {
		Self::with_auth_token(std::env::var("RWP_TOKEN").ok())
	}

	#[must_use]
	pub fn with_auth_token(auth_token: Option<String>) -> Self {
		Self { auth_token: auth_token.map(Arc::new), ..Self::default() }
	}

	pub async fn shutdown_session_scoped_handles(&self, session_id: uuid::Uuid) {
		for handle in self.eval.remove_session_scoped(session_id) {
			let mut reaper_guard = handle.inner.reaper.lock().await;
			if let Some(reaper) = reaper_guard.take() {
				reaper.abort();
			}
			let _ = handle.inner.kernel.shutdown().await;
		}

		for handle in self.lsp.remove_session_scoped(session_id) {
			handle.on_close.notify_waiters();
			let _ = handle.inner.shutdown().await;
		}

		for handle in self.dap.remove_session_scoped(session_id) {
			handle.on_close.notify_waiters();
			let _ = handle.inner.shutdown().await;
		}

		for handle in self.cdp.remove_session_scoped(session_id) {
			handle.on_close.notify_waiters();
			handle.inner.abort_reaper();
			handle.inner.shutdown().await;
		}
	}
}
