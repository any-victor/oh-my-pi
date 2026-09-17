//! Session lifecycle handlers.

use std::{
	path::{Path as StdPath, PathBuf},
	sync::Arc,
	time::Duration,
};

use axum::{
	Json,
	body::Body,
	extract::{Path, State},
	http::{HeaderValue, StatusCode, header},
	response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::stream;
use serde::Deserialize;
use serde_json::to_vec;
use tokio::sync::broadcast;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
	fs_ops::{HEARTBEAT_INTERVAL, heartbeat_stream},
	protocol::{
		error::{ApiError, ApiResult, ErrorBody},
		events::SessionEvent,
		requests::{CreateSessionRequest, PatchEnvRequest, SetCwdRequest},
		responses::CreateSessionResponse,
	},
	session::Session,
	state::AppState,
};

const NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";

#[derive(Debug, Deserialize, ToSchema)]
pub struct WatchRequest {
	enabled: bool,
	#[serde(default)]
	glob:    Option<String>,
}

#[utoipa::path(
	post,
	path = "/sessions",
	request_body = CreateSessionRequest,
	responses(
		(status = 201, description = "Session created", body = CreateSessionResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn create_session(
	State(state): State<AppState>,
	Json(body): Json<CreateSessionRequest>,
) -> ApiResult<(StatusCode, Json<CreateSessionResponse>)> {
	let cwd = match body.cwd {
		Some(cwd) => validate_session_dir(cwd)?,
		None => std::env::current_dir()?,
	};
	let session = Arc::new(Session::new(cwd, body.env));
	let id = state.sessions.insert(session);
	Ok((StatusCode::CREATED, Json(CreateSessionResponse { id })))
}

#[utoipa::path(
	delete,
	path = "/sessions/{id}",
	params(("id" = Uuid, Path)),
	responses(
		(status = 204, description = "Deleted"),
		(status = 404, body = ErrorBody, description = "session not found"),
	),
)]
pub async fn delete_session(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
	let session = get_session(&state, id)?;
	session.cancellation_token.cancel();
	state.shutdown_session_scoped_handles(id).await;
	state
		.sessions
		.remove(id)
		.map(|_| StatusCode::NO_CONTENT)
		.ok_or_else(|| ApiError::NotFound(format!("session {id} not found")))
}

#[utoipa::path(
	put,
	path = "/sessions/{id}/cwd",
	params(("id" = Uuid, Path)),
	request_body = SetCwdRequest,
	responses(
		(status = 204),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "session not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn set_cwd(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Json(body): Json<SetCwdRequest>,
) -> ApiResult<StatusCode> {
	let cwd = validate_session_dir(body.cwd)?;
	let session = get_session(&state, id)?;
	session.set_cwd(cwd)?;
	Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
	patch,
	path = "/sessions/{id}/env",
	params(("id" = Uuid, Path)),
	request_body = PatchEnvRequest,
	responses((status = 204), (status = 404, body = ErrorBody, description = "session not found")),
)]
pub async fn patch_env(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Json(body): Json<PatchEnvRequest>,
) -> ApiResult<StatusCode> {
	let session = get_session(&state, id)?;
	session.patch_env(body.env);
	Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
	put,
	path = "/sessions/{id}/watch",
	params(("id" = Uuid, Path)),
	request_body = WatchRequest,
	responses(
		(status = 204),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "session not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn put_watch(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Json(body): Json<WatchRequest>,
) -> ApiResult<StatusCode> {
	let session = get_session(&state, id)?;
	session.configure_file_watch(body.enabled, body.glob)?;
	Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/events",
	params(("id" = Uuid, Path)),
	responses(
		(status = 200, content_type = "application/x-ndjson"),
		(status = 404, body = ErrorBody, description = "session not found"),
	),
)]
pub async fn events(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
	let session = get_session(&state, id)?;
	Ok(events_response(
		session.subscribe_events(),
		session.heartbeat_interval(),
		session.cancellation_token.clone(),
	))
}

#[utoipa::path(
	get,
	path = "/sessions/{id}/logs",
	params(("id" = Uuid, Path)),
	responses(
		(status = 200, content_type = "application/x-ndjson"),
		(status = 404, body = ErrorBody, description = "session not found"),
	),
)]
pub async fn logs(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
	let session = get_session(&state, id)?;
	Ok(logs_response(session.subscribe_logs(), session.cancellation_token.clone()))
}

fn get_session(state: &AppState, id: Uuid) -> ApiResult<Arc<Session>> {
	state
		.sessions
		.get(id)
		.ok_or_else(|| ApiError::NotFound(format!("session {id} not found")))
}

fn validate_session_dir(cwd: String) -> ApiResult<PathBuf> {
	resolve_session_dir(StdPath::new(&cwd)).map_err(|error| ApiError::BadRequest(error.to_string()))
}

fn resolve_session_dir(path: &StdPath) -> std::io::Result<PathBuf> {
	let absolute = if path.is_absolute() {
		path.to_path_buf()
	} else {
		std::env::current_dir()?.join(path)
	};
	let canonical = absolute.canonicalize()?;
	if !canonical.is_dir() {
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidInput,
			format!("{} is not a directory", path.display()),
		));
	}
	Ok(canonical)
}

fn events_response(
	receiver: broadcast::Receiver<SessionEvent>,
	heartbeat_interval: Duration,
	cancellation_token: tokio_util::sync::CancellationToken,
) -> Response {
	let heartbeat = tokio::time::interval_at(
		tokio::time::Instant::now() + heartbeat_interval,
		heartbeat_interval,
	);
	let stream = stream::unfold(
		(receiver, heartbeat, cancellation_token.clone()),
		|(mut receiver, mut heartbeat, cancellation_token)| async move {
			loop {
				tokio::select! {
					() = cancellation_token.cancelled() => return None,
					result = receiver.recv() => match result {
						Ok(event) => return Some((Ok::<Bytes, std::convert::Infallible>(serialize_event(&event)), (receiver, heartbeat, cancellation_token))),
						Err(broadcast::error::RecvError::Lagged(_)) => {}
						Err(broadcast::error::RecvError::Closed) => return None,
					},
					_ = heartbeat.tick() => {
						return Some((Ok::<Bytes, std::convert::Infallible>(serialize_event(&SessionEvent::Heartbeat)), (receiver, heartbeat, cancellation_token)));
					}
				}
			}
		},
	);

	let mut response = Body::from_stream(stream).into_response();
	*response.status_mut() = if cancellation_token.is_cancelled() {
		client_closed_request_status()
	} else {
		StatusCode::OK
	};
	response
		.headers_mut()
		.insert(header::CONTENT_TYPE, HeaderValue::from_static(NDJSON_CONTENT_TYPE));
	response
}

fn logs_response(
	receiver: broadcast::Receiver<crate::protocol::events::LogRecord>,
	cancellation_token: tokio_util::sync::CancellationToken,
) -> Response {
	let stream = stream::unfold(
		(receiver, cancellation_token.clone()),
		|(mut receiver, cancellation_token)| async move {
			loop {
				tokio::select! {
					() = cancellation_token.cancelled() => return None,
					result = receiver.recv() => match result {
						Ok(record) => return Some((serialize_log(&record), (receiver, cancellation_token))),
						Err(broadcast::error::RecvError::Lagged(_)) => {}
						Err(broadcast::error::RecvError::Closed) => return None,
					},
				}
			}
		},
	);
	let stream = heartbeat_stream(
		stream,
		HEARTBEAT_INTERVAL,
		cancellation_token.clone(),
		serialize_event(&SessionEvent::Heartbeat),
	);

	let mut response = Body::from_stream(stream).into_response();
	*response.status_mut() = if cancellation_token.is_cancelled() {
		client_closed_request_status()
	} else {
		StatusCode::OK
	};
	response
		.headers_mut()
		.insert(header::CONTENT_TYPE, HeaderValue::from_static(NDJSON_CONTENT_TYPE));
	response
}

fn client_closed_request_status() -> StatusCode {
	StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST)
}

fn serialize_event(event: &SessionEvent) -> Bytes {
	let mut payload = to_vec(event).expect("session events always serialize");
	payload.push(b'\n');
	Bytes::from(payload)
}

fn serialize_log(record: &crate::protocol::events::LogRecord) -> Bytes {
	let mut payload = to_vec(record).expect("session logs always serialize");
	payload.push(b'\n');
	Bytes::from(payload)
}
