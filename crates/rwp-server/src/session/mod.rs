//! Per-agent session: cwd, env, file-read cache, edit lock, events channel.

pub mod cache;
pub mod core;
pub mod registry;

pub use core::Session;

pub use cache::FileReadCache;
pub use registry::SessionRegistry;
