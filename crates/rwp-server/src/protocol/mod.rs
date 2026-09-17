//! Wire types shared between handlers and the `OpenAPI` schema.

pub mod error;
pub mod events;
pub mod requests;
pub mod responses;

pub use error::{ApiError, ErrorBody};
pub use events::SessionEvent;
