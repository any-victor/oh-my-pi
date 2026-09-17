//! Per-session state.

use std::{
	collections::{BTreeMap, HashMap},
	path::PathBuf,
	sync::Arc,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use pi_shell::{Shell as BrushShell, ShellOptions as BrushShellOptions};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::cache::FileReadCache;
use crate::{
	fs_ops::watcher::SessionWatcher,
	protocol::{
		error::ApiResult,
		events::{LogLevel, LogRecord, SessionEvent},
	},
};

const DEFAULT_EVENT_BUFFER: usize = 256;
const DEFAULT_LOG_BUFFER: usize = 256;
const DEFAULT_READ_CACHE_BYTES: usize = 64 * 1024 * 1024; // 64 MiB
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// One agent session. Owns mutable cwd/env, the file cache, and an exclusive
/// edit lock so concurrent writethroughs against the same session serialize.
pub struct Session {
	pub id:                 Uuid,
	state:                  RwLock<MutableState>,
	pub read_cache:         Arc<FileReadCache>,
	pub edit_lock:          Mutex<()>,
	shell:                  Mutex<Option<Arc<BrushShell>>>,
	pub events:             broadcast::Sender<SessionEvent>,
	logs:                   broadcast::Sender<LogRecord>,
	pub cancellation_token: CancellationToken,
	file_watcher:           SessionWatcher,
	heartbeat_interval:     Duration,
}

impl std::fmt::Debug for Session {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Session")
			.field("id", &self.id)
			.field("state", &self.state)
			.field("read_cache", &self.read_cache)
			.field("edit_lock", &self.edit_lock)
			.field("events", &self.events)
			.field("logs", &self.logs)
			.field("cancellation_token", &self.cancellation_token)
			.field("heartbeat_interval", &self.heartbeat_interval)
			.finish_non_exhaustive()
	}
}

#[derive(Debug, Clone)]
struct MutableState {
	cwd: PathBuf,
	env: BTreeMap<String, String>,
}

impl Session {
	#[must_use]
	pub fn new(cwd: PathBuf, env: BTreeMap<String, String>) -> Self {
		Self::with_heartbeat_interval(cwd, env, DEFAULT_HEARTBEAT_INTERVAL)
	}

	#[must_use]
	pub fn with_heartbeat_interval(
		cwd: PathBuf,
		env: BTreeMap<String, String>,
		heartbeat_interval: Duration,
	) -> Self {
		let (tx, _) = broadcast::channel(DEFAULT_EVENT_BUFFER);
		let (logs, _) = broadcast::channel(DEFAULT_LOG_BUFFER);
		Self {
			id: Uuid::new_v4(),
			state: RwLock::new(MutableState { cwd, env }),
			read_cache: Arc::new(FileReadCache::new(DEFAULT_READ_CACHE_BYTES)),
			edit_lock: Mutex::new(()),
			shell: Mutex::new(None),
			events: tx,
			logs,
			cancellation_token: CancellationToken::new(),
			file_watcher: SessionWatcher::default(),
			heartbeat_interval,
		}
	}

	pub fn cwd(&self) -> PathBuf {
		self.state.read().cwd.clone()
	}

	pub fn set_cwd(&self, cwd: PathBuf) -> ApiResult<()> {
		let previous = {
			let mut state = self.state.write();
			let previous = state.cwd.clone();
			state.cwd.clone_from(&cwd);
			previous
		};
		if let Err(error) = self
			.file_watcher
			.restart_if_enabled(cwd, self.events.clone())
		{
			self.state.write().cwd = previous;
			return Err(error);
		}
		Ok(())
	}

	pub fn configure_file_watch(&self, enabled: bool, glob: Option<String>) -> ApiResult<()> {
		tracing::info!(enabled, ?glob, "configure_file_watch called");
		if enabled {
			self
				.file_watcher
				.start(self.cwd(), self.events.clone(), glob)
		} else {
			self.file_watcher.stop();
			Ok(())
		}
	}

	pub fn env_snapshot(&self) -> BTreeMap<String, String> {
		self.state.read().env.clone()
	}

	#[must_use]
	pub const fn heartbeat_interval(&self) -> Duration {
		self.heartbeat_interval
	}

	#[must_use]
	pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
		self.events.subscribe()
	}

	#[must_use]
	pub fn subscribe_logs(&self) -> broadcast::Receiver<LogRecord> {
		self.logs.subscribe()
	}

	pub async fn brush_shell(&self) -> Arc<BrushShell> {
		let mut guard = self.shell.lock().await;
		if let Some(shell) = guard.as_ref() {
			return Arc::clone(shell);
		}

		let session_env: HashMap<String, String> = self.env_snapshot().into_iter().collect();
		let shell = Arc::new(BrushShell::new(Some(BrushShellOptions {
			session_env:   Some(session_env),
			snapshot_path: None,
			minimizer:     None,
		})));
		*guard = Some(Arc::clone(&shell));
		shell
	}

	/// Apply a patch: `Some(v)` sets, `None` unsets.
	pub fn patch_env(&self, patch: BTreeMap<String, Option<String>>) {
		let mut state = self.state.write();
		for (k, v) in patch {
			match v {
				Some(value) => {
					state.env.insert(k, value);
				},
				None => {
					state.env.remove(&k);
				},
			}
		}
	}

	pub fn emit_log(
		&self,
		level: LogLevel,
		source: impl Into<String>,
		message: impl Into<String>,
		fields: BTreeMap<String, serde_json::Value>,
	) {
		let ts_ms = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
			.unwrap_or_default();
		let _ = self.logs.send(LogRecord {
			ts_ms,
			level,
			source: source.into(),
			message: message.into(),
			fields,
		});
	}
}
