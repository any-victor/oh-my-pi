use std::{
	collections::HashMap,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, Instant},
};

use globset::{Glob, GlobMatcher};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::protocol::{
	error::{ApiError, ApiResult},
	events::SessionEvent,
};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct SessionWatcher {
	inner: Mutex<Option<ActiveWatcher>>,
}

struct ActiveWatcher {
	_watcher: RecommendedWatcher,
	glob:     Option<String>,
}

impl SessionWatcher {
	pub fn start(
		&self,
		cwd: PathBuf,
		broadcast_tx: broadcast::Sender<SessionEvent>,
		glob: Option<String>,
	) -> ApiResult<()> {
		let matcher = compile_glob(glob.as_deref())?;
		let debounce = Arc::new(Mutex::new(HashMap::<String, Instant>::new()));
		let watched_cwd = cwd.clone();
		let mut watcher =
			notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
				let Ok(event) = result else {
					tracing::warn!(error = ?result.err(), "file watcher event failed");
					return;
				};
				handle_event(&watched_cwd, &broadcast_tx, matcher.as_ref(), &debounce, event);
			})
			.map_err(|error| ApiError::Internal(error.into()))?;
		watcher
			.watch(&cwd, RecursiveMode::Recursive)
			.map_err(|error| ApiError::Internal(error.into()))?;
		*self.inner.lock() = Some(ActiveWatcher { _watcher: watcher, glob });
		Ok(())
	}

	pub fn stop(&self) {
		*self.inner.lock() = None;
	}

	pub fn restart_if_enabled(
		&self,
		cwd: PathBuf,
		broadcast_tx: broadcast::Sender<SessionEvent>,
	) -> ApiResult<()> {
		let glob = {
			let guard = self.inner.lock();
			let Some(active) = guard.as_ref() else {
				return Ok(());
			};
			active.glob.clone()
		};
		self.start(cwd, broadcast_tx, glob)
	}
}

fn compile_glob(glob: Option<&str>) -> ApiResult<Option<GlobMatcher>> {
	glob
		.map(|pattern| {
			Glob::new(pattern)
				.map(|glob| glob.compile_matcher())
				.map_err(|error| {
					ApiError::BadRequest(format!("invalid watch glob {pattern:?}: {error}"))
				})
		})
		.transpose()
}

fn handle_event(
	cwd: &Path,
	broadcast_tx: &broadcast::Sender<SessionEvent>,
	matcher: Option<&GlobMatcher>,
	debounce: &Mutex<HashMap<String, Instant>>,
	event: notify::Event,
) {
	tracing::debug!(?event.kind, paths = ?event.paths, "file watcher event received");
	if !(event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove()) {
		tracing::debug!("event kind not create/modify/remove, skipping");
		return;
	}

	for path in event.paths {
		let Some(relative_path) = relative_event_path(cwd, &path) else {
			tracing::debug!(?path, "could not get relative path, skipping");
			continue;
		};
		tracing::debug!(relative_path, "got relative path");
		if let Some(matcher) = matcher
			&& !matcher.is_match(Path::new(&relative_path))
		{
			tracing::debug!(relative_path, "glob matcher rejected path");
			continue;
		}
		if !should_emit(debounce, &relative_path) {
			tracing::debug!(relative_path, "debounce rejected path");
			continue;
		}
		tracing::info!(relative_path, "sending FileChanged event");
		let _ = broadcast_tx.send(SessionEvent::FileChanged { path: relative_path, etag: None });
	}
}

fn relative_event_path(cwd: &Path, path: &Path) -> Option<String> {
	// On macOS, /var/folders is a symlink to /private/var/folders
	// Canonicalize both paths to handle this
	let cwd_canonical = std::fs::canonicalize(cwd).ok()?;
	let path_canonical = std::fs::canonicalize(path).ok()?;
	let relative = path_canonical.strip_prefix(&cwd_canonical).ok()?;
	if relative.as_os_str().is_empty() {
		return None;
	}
	Some(relative.to_string_lossy().replace('\\', "/"))
}

fn should_emit(debounce: &Mutex<HashMap<String, Instant>>, path: &str) -> bool {
	let now = Instant::now();
	let mut seen = debounce.lock();
	seen.retain(|_, instant| now.duration_since(*instant) <= DEBOUNCE_WINDOW);
	match seen.get(path) {
		Some(last) if now.duration_since(*last) < DEBOUNCE_WINDOW => false,
		_ => {
			seen.insert(path.to_owned(), now);
			true
		},
	}
}
