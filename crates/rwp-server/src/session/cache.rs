//! `FileReadCache` — server-side coalescing cache.
//!
//! Two roles:
//!
//! 1. **Read coalescing.** Subsequent reads of an unchanged file (same `mtime`
//!    and `len`) hit memory instead of disk. Crucial when the harness pulls
//!    overlapping line ranges from large files repeatedly.
//! 2. **LSP backing store.** When the edit writethrough path needs to fire
//!    `textDocument/didChange`, it needs the prior buffer text to compute a
//!    diff (or just send a full-text sync). The cache holds that view.
//!
//! Eviction is LRU bounded by total bytes — see [`FileReadCache::new`].
//! Anchor concepts (hashes, fuzzy recovery) live in the harness; the cache
//! stores raw `Arc<str>` (or `Arc<[u8]>` for binary files) so writes can
//! cheaply share the post-edit snapshot with the LSP forwarder.

use std::{
	collections::HashMap,
	path::{Path, PathBuf},
	sync::Arc,
	time::SystemTime,
};

use parking_lot::Mutex;

/// Snapshot of a file as the server most recently observed it.
#[derive(Debug, Clone)]
pub struct Snapshot {
	/// Canonicalised absolute path. Used as the cache key.
	pub path:  PathBuf,
	/// `mtime` at read time; used for coherence checks before serving stale.
	pub mtime: SystemTime,
	/// File size in bytes (cheap pre-check before reading).
	pub len:   u64,
	/// Content hash (e.g. xxhash64 hex). Returned to clients as `ETag`.
	pub etag:  Arc<str>,
	/// Body. `None` for entries seeded by metadata-only probes.
	pub bytes: Option<Arc<[u8]>>,
}

/// Bounded LRU file cache.
#[derive(Debug)]
pub struct FileReadCache {
	inner:     Mutex<Inner>,
	max_bytes: usize,
}

#[derive(Debug, Default)]
struct Inner {
	/// path -> snapshot. Recency tracked via `order`.
	map:        HashMap<PathBuf, Snapshot>,
	/// Most-recently-used path at the back.
	order:      Vec<PathBuf>,
	/// Sum of bytes held in `map`.
	bytes_held: usize,
}

impl FileReadCache {
	#[must_use]
	pub fn new(max_bytes: usize) -> Self {
		Self { inner: Mutex::new(Inner::default()), max_bytes }
	}

	/// Look up a path. Caller is expected to compare `mtime`/`len` against the
	/// current disk metadata before trusting the result.
	pub fn get(&self, path: &Path) -> Option<Snapshot> {
		let mut inner = self.inner.lock();
		let snap = inner.map.get(path).cloned()?;
		Self::touch(&mut inner, path);
		Some(snap)
	}

	/// Insert a snapshot, evicting LRU entries until under budget.
	pub fn insert(&self, snap: Snapshot) {
		let added = snap.bytes.as_ref().map_or(0, |b| b.len());
		let path = snap.path.clone();
		let mut inner = self.inner.lock();
		if let Some(old) = inner.map.remove(&path) {
			inner.bytes_held = inner
				.bytes_held
				.saturating_sub(old.bytes.as_ref().map_or(0, |b| b.len()));
			if let Some(pos) = inner.order.iter().position(|p| p == &path) {
				inner.order.remove(pos);
			}
		}
		inner.map.insert(path.clone(), snap);
		inner.order.push(path);
		inner.bytes_held = inner.bytes_held.saturating_add(added);
		Self::evict(&mut inner, self.max_bytes);
	}

	/// Drop a path explicitly (e.g. on `DELETE`).
	pub fn invalidate(&self, path: &Path) {
		let mut inner = self.inner.lock();
		if let Some(old) = inner.map.remove(path) {
			inner.bytes_held = inner
				.bytes_held
				.saturating_sub(old.bytes.as_ref().map_or(0, |b| b.len()));
			if let Some(pos) = inner.order.iter().position(|p| p == path) {
				inner.order.remove(pos);
			}
		}
	}

	/// Current byte footprint. Useful for tests and telemetry.
	pub fn bytes_held(&self) -> usize {
		self.inner.lock().bytes_held
	}

	fn touch(inner: &mut Inner, path: &Path) {
		if let Some(pos) = inner.order.iter().position(|p| p == path) {
			let p = inner.order.remove(pos);
			inner.order.push(p);
		}
	}

	fn evict(inner: &mut Inner, max_bytes: usize) {
		while inner.bytes_held > max_bytes {
			let Some(victim) = inner.order.first().cloned() else {
				break;
			};
			inner.order.remove(0);
			if let Some(old) = inner.map.remove(&victim) {
				inner.bytes_held = inner
					.bytes_held
					.saturating_sub(old.bytes.as_ref().map_or(0, |b| b.len()));
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::UNIX_EPOCH;

	use super::*;

	fn snap(path: &str, body: &[u8]) -> Snapshot {
		Snapshot {
			path:  PathBuf::from(path),
			mtime: UNIX_EPOCH,
			len:   body.len() as u64,
			etag:  Arc::from("etag"),
			bytes: Some(Arc::from(body)),
		}
	}

	#[test]
	fn round_trip_get_insert() {
		let cache = FileReadCache::new(1024);
		assert!(cache.get(Path::new("/a")).is_none());
		cache.insert(snap("/a", b"hello"));
		let got = cache.get(Path::new("/a")).expect("just inserted");
		assert_eq!(got.bytes.as_deref(), Some(b"hello".as_slice()));
	}

	#[test]
	fn evicts_lru_under_budget() {
		let cache = FileReadCache::new(10);
		cache.insert(snap("/a", b"aaaaa")); // 5 bytes
		cache.insert(snap("/b", b"bbbbb")); // 5 bytes -> total 10
		cache.insert(snap("/c", b"ccccc")); // 5 bytes -> evicts /a
		assert!(cache.get(Path::new("/a")).is_none());
		assert!(cache.get(Path::new("/b")).is_some());
		assert!(cache.get(Path::new("/c")).is_some());
	}

	#[test]
	fn touch_keeps_recent_entry() {
		let cache = FileReadCache::new(10);
		cache.insert(snap("/a", b"aaaaa"));
		cache.insert(snap("/b", b"bbbbb"));
		// Touching /a makes /b the LRU victim.
		let _ = cache.get(Path::new("/a"));
		cache.insert(snap("/c", b"ccccc"));
		assert!(cache.get(Path::new("/a")).is_some());
		assert!(cache.get(Path::new("/b")).is_none());
	}

	#[test]
	fn invalidate_releases_bytes() {
		let cache = FileReadCache::new(1024);
		cache.insert(snap("/a", b"hello"));
		assert_eq!(cache.bytes_held(), 5);
		cache.invalidate(Path::new("/a"));
		assert_eq!(cache.bytes_held(), 0);
	}
}
