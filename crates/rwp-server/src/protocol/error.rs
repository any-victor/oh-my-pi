//! Structured error envelope returned for non-2xx responses.

use axum::{
	Json,
	http::StatusCode,
	response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// JSON body for any error response. Stable across endpoints so clients can
/// match on `code` (machine-readable) and surface `message` to humans.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
	/// Stable error code, kebab-case (e.g. `not-found`, `etag-mismatch`).
	pub code:    String,
	/// Human-readable explanation.
	pub message: String,
	/// Optional opaque detail object (per-error context).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub detail:  Option<serde_json::Value>,
}

/// Server-side error type. Maps to an HTTP status + [`ErrorBody`].
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
	#[error("not found: {0}")]
	NotFound(String),
	#[error("bad request: {0}")]
	BadRequest(String),
	#[error("conflict: {0}")]
	Conflict(String),
	#[error("etag mismatch")]
	EtagMismatch,
	#[error("request cancelled")]
	Cancelled,
	#[error("payload too large: {0}")]
	PayloadTooLarge(String),
	#[error("missing or invalid bearer token")]
	Unauthorized,
	#[error("unsupported media type: {0}")]
	UnsupportedMediaType(String),
	#[error("not implemented: {0}")]
	NotImplemented(&'static str),
	#[error(transparent)]
	Io(#[from] std::io::Error),
	#[error(transparent)]
	Internal(#[from] anyhow::Error),
}
impl ApiError {
	#[must_use]
	pub fn status(&self) -> StatusCode {
		match self {
			Self::NotFound(_) => StatusCode::NOT_FOUND,
			Self::BadRequest(_) => StatusCode::BAD_REQUEST,
			Self::Conflict(_) => StatusCode::CONFLICT,
			Self::EtagMismatch => StatusCode::PRECONDITION_FAILED,
			Self::Cancelled => StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST),
			Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
			Self::Unauthorized => StatusCode::UNAUTHORIZED,
			Self::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
			Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
			Self::Io(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
		}
	}

	#[must_use]
	pub const fn code(&self) -> &'static str {
		match self {
			Self::NotFound(_) => "not-found",
			Self::BadRequest(_) => "bad-request",
			Self::Conflict(_) => "conflict",
			Self::EtagMismatch => "etag-mismatch",
			Self::Cancelled => "cancelled",
			Self::PayloadTooLarge(_) => "payload-too-large",
			Self::Unauthorized => "unauthorized",
			Self::UnsupportedMediaType(_) => "unsupported-media-type",
			Self::NotImplemented(_) => "not-implemented",
			Self::Io(_) => "io-error",
			Self::Internal(_) => "internal",
		}
	}
}

impl IntoResponse for ApiError {
	fn into_response(self) -> Response {
		let body =
			ErrorBody { code: self.code().to_owned(), message: self.to_string(), detail: None };
		(self.status(), Json(body)).into_response()
	}
}

/// Convenience: every handler returns this.
pub type ApiResult<T> = Result<T, ApiError>;
