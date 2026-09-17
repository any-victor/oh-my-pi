use std::{
	fs::File,
	hash::Hasher,
	io::{self, BufReader, Read},
	path::{Path, PathBuf},
	sync::atomic::{AtomicU64, Ordering},
	time::SystemTime,
};

use twox_hash::XxHash3_64;

#[derive(Debug, Clone)]
pub struct EtagCacheEntry {
	pub mtime: SystemTime,
	pub size:  u64,
	pub hash:  String,
}

#[derive(Debug, Default)]
pub struct EtagCache {
	pub inner:           dashmap::DashMap<PathBuf, EtagCacheEntry>,
	pub hashes_computed: AtomicU64,
}

impl EtagCache {
	pub fn compute(&self, path: &Path) -> io::Result<String> {
		let metadata = std::fs::metadata(path)?;
		let mtime = metadata.modified()?;
		let size = metadata.len();
		if let Some(entry) = self.inner.get(path)
			&& entry.mtime == mtime
			&& entry.size == size
		{
			return Ok(entry.hash.clone());
		}

		let hash = hash_file(path)?;
		self.hashes_computed.fetch_add(1, Ordering::Relaxed);
		self
			.inner
			.insert(path.to_path_buf(), EtagCacheEntry { mtime, size, hash: hash.clone() });
		Ok(hash)
	}

	pub fn invalidate(&self, path: &Path) {
		self.inner.remove(path);
	}
}

fn hash_file(path: &Path) -> io::Result<String> {
	let file = File::open(path)?;
	let mut reader = BufReader::new(file);
	let mut hasher = XxHash3_64::new();
	let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
	loop {
		let read = reader.read(&mut buffer)?;
		if read == 0 {
			break;
		}
		hasher.write(&buffer[..read]);
	}
	Ok(format!("{:016x}", hasher.finish()))
}
