use axum::{
	extract::{Request, State},
	http::{Method, header},
	middleware::Next,
	response::Response,
};

use crate::{protocol::ApiError, state::AppState};

pub async fn require_bearer_auth(
	State(state): State<AppState>,
	request: Request,
	next: Next,
) -> Result<Response, ApiError> {
	if request.method() == Method::OPTIONS {
		return Ok(next.run(request).await);
	}

	let Some(expected_token) = state.auth_token.as_deref() else {
		return Ok(next.run(request).await);
	};

	let Some(authorization) = request.headers().get(header::AUTHORIZATION) else {
		return Err(ApiError::Unauthorized);
	};

	if is_valid_bearer_token(authorization, expected_token) {
		Ok(next.run(request).await)
	} else {
		Err(ApiError::Unauthorized)
	}
}

fn is_valid_bearer_token(authorization: &header::HeaderValue, expected_token: &str) -> bool {
	let Ok(value) = authorization.to_str() else {
		return false;
	};
	let Some(provided_token) = value.strip_prefix("Bearer ") else {
		return false;
	};
	constant_time_eq(expected_token.as_bytes(), provided_token.as_bytes())
}

fn constant_time_eq(expected: &[u8], provided: &[u8]) -> bool {
	let max_len = expected.len().max(provided.len());
	let mut diff = expected.len() ^ provided.len();
	for index in 0..max_len {
		let expected_byte = expected.get(index).copied().unwrap_or_default();
		let provided_byte = provided.get(index).copied().unwrap_or_default();
		diff |= usize::from(expected_byte ^ provided_byte);
	}
	diff == 0
}

#[cfg(test)]
mod tests {
	use axum::{
		Router,
		body::Body,
		http::{Request, StatusCode},
		routing::get,
	};
	use tower::ServiceExt;

	use super::*;

	#[test]
	fn bearer_token_parser_accepts_only_exact_match() {
		let header = header::HeaderValue::from_static("Bearer secret");
		assert!(is_valid_bearer_token(&header, "secret"));
		assert!(!is_valid_bearer_token(&header, "wrong"));
		assert!(!is_valid_bearer_token(&header, "secret-extra"));
	}

	#[test]
	fn bearer_token_parser_rejects_missing_or_wrong_scheme() {
		let basic = header::HeaderValue::from_static("Basic secret");
		let missing_token = header::HeaderValue::from_static("Bearer ");
		assert!(!is_valid_bearer_token(&basic, "secret"));
		assert!(!is_valid_bearer_token(&missing_token, "secret"));
	}

	#[tokio::test]
	async fn options_requests_skip_auth() {
		let state = AppState::with_auth_token(Some("secret".to_owned()));
		let app = Router::new()
			.route(
				"/probe",
				get(|| async { StatusCode::OK }).options(|| async { StatusCode::NO_CONTENT }),
			)
			.layer(axum::middleware::from_fn_with_state(state.clone(), require_bearer_auth))
			.with_state(state);
		let request = Request::builder()
			.method(Method::OPTIONS)
			.uri("/probe")
			.body(Body::empty())
			.expect("request");

		let response = app.oneshot(request).await.expect("response");
		assert_eq!(response.status(), StatusCode::NO_CONTENT);
	}
}
