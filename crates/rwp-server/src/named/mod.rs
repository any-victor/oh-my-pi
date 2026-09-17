//! Generic named-handle registry for `/eval/{name}`, `/lsp/{name}`,
//! `/dap/{name}`, `/cdp/{name}`.
//!
//! Each registry holds opaque handles (`H`) keyed by name. Handles carry their
//! own refcount via the active-WS counter; idle-reaper logic lives in the
//! per-endpoint handler module (it knows what "idle" means for that protocol).

use std::{
	sync::Arc,
	time::{Duration, Instant},
};

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use utoipa::ToSchema;
use uuid::Uuid;

/// Wire-level scope selector accepted on PUT config bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RequestedHandleScope {
	Global,
	Session,
}

/// Resolved scope stored with the live named handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleScope {
	Global,
	Session { session_id: Uuid },
}

impl HandleScope {
	#[must_use]
	pub const fn global() -> Self {
		Self::Global
	}
}

/// Wrapper around a handle that tracks active-WS refcount + last-active time.
/// The named handler keeps these as the value type.
#[derive(Debug)]
pub struct Handle<H> {
	pub inner:    Arc<H>,
	scope:        HandleScope,
	state:        Mutex<HandleState>,
	pub on_close: Arc<Notify>,
}

#[derive(Debug)]
struct HandleState {
	refcount:     u32,
	idle_timeout: Duration,
	last_active:  Instant,
}

impl<H> Handle<H> {
	pub fn new(inner: Arc<H>, scope: HandleScope, idle_timeout: Duration) -> Self {
		Self {
			inner,
			scope,
			state: Mutex::new(HandleState { refcount: 0, idle_timeout, last_active: Instant::now() }),
			on_close: Arc::new(Notify::new()),
		}
	}

	#[must_use]
	pub const fn scope(&self) -> HandleScope {
		self.scope
	}

	pub fn refcount(&self) -> u32 {
		self.state.lock().refcount
	}

	pub fn idle_timeout(&self) -> Duration {
		self.state.lock().idle_timeout
	}

	pub fn last_active(&self) -> Instant {
		self.state.lock().last_active
	}

	pub fn retain(self: &Arc<Self>) -> HandleGuard<H> {
		let mut s = self.state.lock();
		s.refcount += 1;
		s.last_active = Instant::now();
		HandleGuard { handle: Arc::clone(self) }
	}
}

/// RAII refcount: decrements + bumps `last_active` on drop.
#[must_use]
pub struct HandleGuard<H> {
	handle: Arc<Handle<H>>,
}

impl<H> HandleGuard<H> {
	pub fn inner(&self) -> &Arc<H> {
		&self.handle.inner
	}
}

impl<H> Drop for HandleGuard<H> {
	fn drop(&mut self) {
		let mut s = self.handle.state.lock();
		s.refcount = s.refcount.saturating_sub(1);
		s.last_active = Instant::now();
	}
}

#[derive(Debug)]
pub struct NamedRegistry<H> {
	map: DashMap<String, Arc<Handle<H>>>,
}

impl<H> Default for NamedRegistry<H> {
	fn default() -> Self {
		Self { map: DashMap::new() }
	}
}

impl<H> NamedRegistry<H> {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn get(&self, name: &str) -> Option<Arc<Handle<H>>> {
		self.map.get(name).map(|r| Arc::clone(r.value()))
	}

	/// Idempotent insert: returns the existing handle if one is already
	/// registered under `name`, otherwise calls `build` once and stores it.
	pub fn get_or_insert_with<F>(&self, name: &str, scope: HandleScope, build: F) -> Arc<Handle<H>>
	where
		F: FnOnce() -> Arc<H>,
	{
		self.get_or_insert_with_timeout(name, Duration::MAX, scope, build)
	}

	pub fn get_or_insert_with_timeout<F>(
		&self,
		name: &str,
		idle_timeout: Duration,
		scope: HandleScope,
		build: F,
	) -> Arc<Handle<H>>
	where
		F: FnOnce() -> Arc<H>,
	{
		if let Some(existing) = self.get(name) {
			return existing;
		}
		let entry = self
			.map
			.entry(name.to_owned())
			.or_insert_with(|| Arc::new(Handle::new(build(), scope, idle_timeout)));
		Arc::clone(entry.value())
	}

	pub fn remove(&self, name: &str) -> Option<Arc<Handle<H>>> {
		self.map.remove(name).map(|(_, v)| v)
	}

	pub fn remove_session_scoped(&self, session_id: Uuid) -> Vec<Arc<Handle<H>>> {
		let names = self
			.map
			.iter()
			.filter_map(|entry| match entry.value().scope() {
				HandleScope::Session { session_id: scoped } if scoped == session_id => {
					Some(entry.key().clone())
				},
				HandleScope::Global | HandleScope::Session { .. } => None,
			})
			.collect::<Vec<_>>();
		let mut removed = Vec::with_capacity(names.len());
		for name in names {
			if let Some(handle) = self.remove(&name)
				&& matches!(handle.scope(), HandleScope::Session { session_id: scoped } if scoped == session_id)
			{
				removed.push(handle);
			}
		}
		removed
	}

	pub fn names(&self) -> Vec<String> {
		self.map.iter().map(|r| r.key().clone()).collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn refcount_round_trip() {
		let h = Arc::new(Handle::new(Arc::new(()), HandleScope::Global, Duration::MAX));
		assert_eq!(h.refcount(), 0);
		let g1 = h.retain();
		let g2 = h.retain();
		assert_eq!(h.refcount(), 2);
		drop(g1);
		assert_eq!(h.refcount(), 1);
		drop(g2);
		assert_eq!(h.refcount(), 0);
	}

	#[test]
	fn registry_get_or_insert_is_idempotent() {
		let reg: NamedRegistry<u32> = NamedRegistry::new();
		let a = reg.get_or_insert_with("k", HandleScope::Global, || Arc::new(7));
		let b = reg.get_or_insert_with("k", HandleScope::Global, || Arc::new(99));
		assert!(Arc::ptr_eq(&a.inner, &b.inner));
		assert_eq!(*a.inner, 7);
	}

	#[test]
	fn remove_session_scoped_keeps_globals() {
		let reg: NamedRegistry<u32> = NamedRegistry::new();
		let session_id = Uuid::new_v4();
		reg.get_or_insert_with("global", HandleScope::Global, || Arc::new(1));
		reg.get_or_insert_with("session", HandleScope::Session { session_id }, || Arc::new(2));
		let removed = reg.remove_session_scoped(session_id);
		assert_eq!(removed.len(), 1);
		assert!(reg.get("global").is_some());
		assert!(reg.get("session").is_none());
	}
}
