//! HTTP handlers. Phase 0 ships stubs that return `501 Not Implemented`;
//! Phase 1 sub-agents fill them in one module at a time.

pub mod bash;
pub mod cdp;
pub mod dap;
pub mod edit;
pub mod eval;
pub mod fs;
pub mod lsp;
pub mod sessions;
pub mod sqlite;

use crate::protocol::error::ApiError;

/// Helper: produce a `501` carrying the canonical endpoint name.
pub const fn not_yet(endpoint: &'static str) -> ApiError {
	ApiError::NotImplemented(endpoint)
}
