use axum::{
	extract::Request,
	http::{HeaderValue, header::HeaderName},
	middleware::Next,
	response::Response,
};
use tracing::Span;
use uuid::Uuid;

pub const X_REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestId(pub HeaderValue);

pub async fn middleware(request: Request, next: Next) -> Response {
	let request_id = request
		.headers()
		.get(&X_REQUEST_ID_HEADER)
		.cloned()
		.unwrap_or_else(generate_request_id);
	let request_id_for_span = String::from_utf8_lossy(request_id.as_bytes());
	Span::current().record("request_id", tracing::field::display(&request_id_for_span));

	let mut response = next.run(request).await;
	response
		.extensions_mut()
		.insert(RequestId(request_id.clone()));
	response
		.headers_mut()
		.insert(X_REQUEST_ID_HEADER, request_id);
	response
}

fn generate_request_id() -> HeaderValue {
	let mut buffer = Uuid::encode_buffer();
	let request_id = Uuid::new_v4().hyphenated().encode_lower(&mut buffer);
	HeaderValue::from_str(request_id).expect("uuid request id must be a valid header value")
}

#[cfg(test)]
mod tests {
	use axum::{
		Router,
		http::{Request as HttpRequest, StatusCode},
		middleware::from_fn,
		routing::get,
	};
	use tower::ServiceExt;

	use super::{RequestId, X_REQUEST_ID_HEADER, middleware};

	fn app() -> Router {
		Router::new()
			.route("/", get(|| async { StatusCode::NO_CONTENT }))
			.layer(from_fn(middleware))
	}

	#[tokio::test]
	async fn generated_request_id_is_added_to_response_and_extensions() {
		let response = app()
			.oneshot(
				HttpRequest::builder()
					.uri("/")
					.body(axum::body::Body::empty())
					.expect("request"),
			)
			.await
			.expect("response");

		let header = response
			.headers()
			.get(&X_REQUEST_ID_HEADER)
			.expect("request id header present");
		assert!(!header.as_bytes().is_empty(), "generated request id should be non-empty");
		assert_eq!(response.status(), StatusCode::NO_CONTENT);
		assert_eq!(
			response
				.extensions()
				.get::<RequestId>()
				.expect("request id extension present")
				.0,
			*header,
		);
	}

	#[tokio::test]
	async fn inbound_request_id_is_echoed_unchanged() {
		let response = app()
			.oneshot(
				HttpRequest::builder()
					.uri("/")
					.header(&X_REQUEST_ID_HEADER, "my-fixed-id")
					.body(axum::body::Body::empty())
					.expect("request"),
			)
			.await
			.expect("response");

		assert_eq!(
			response
				.headers()
				.get(&X_REQUEST_ID_HEADER)
				.expect("request id header present"),
			"my-fixed-id",
		);
	}
}
