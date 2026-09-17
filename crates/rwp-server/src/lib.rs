//! Remote Workspace Protocol (RWP) server library.
//!
//! Thin remote workspace: sessions, filesystem ops, search, edit primitives,
//! bash, and tunnels to LSP/DAP/CDP/eval. Wire layer is HTTP/1.1 + WebSocket;
//! streaming responses use chunked `application/x-ndjson`. Cancellation is
//! implicit — closing the connection drops the future and `Drop` reaps any
//! child resources.
//!
//! The server is deliberately dumb: anchor / hashline semantics, fuzzy
//! stale-anchor recovery, and other harness conveniences live in the client
//! (the harness). The server returns plain bytes + `ETag`s and trusts the
//! client to mediate edits.
//!
//! See `docs/rwp-protocol.md` for protocol design and decisions.

pub mod auth;
pub mod cdp_tunnel;
pub mod dap_tunnel;
pub(crate) mod eval_kernel;
pub mod fs_ops;
pub mod handlers;
pub mod lsp_tunnel;
pub mod named;
pub mod protocol;
pub mod request_id;
pub mod router;
pub mod session;
pub mod state;

pub use protocol::ErrorBody;
pub use router::build_router;
pub use state::AppState;
