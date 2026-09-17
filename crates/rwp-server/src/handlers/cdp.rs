//! `/cdp/{name}` — Chrome `DevTools` Protocol tunnel.

use axum::{
	Json,
	extract::{
		Path, Query, State,
		ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
	},
	http::StatusCode,
	response::{IntoResponse, Response},
};

use crate::{
	cdp_tunnel,
	named::{HandleScope, RequestedHandleScope},
	protocol::{
		error::{ApiError, ApiResult, ErrorBody},
		requests::{NamedHandleConfig, NamedHandleQuery},
		responses::CdpHandleResponse,
	},
	state::AppState,
};

#[utoipa::path(
	get,
	path = "/cdp",
	responses(
		(status = 200, body = [CdpHandleResponse]),
	)
)]
pub async fn list_cdp(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
	let mut handles = state
		.cdp
		.names()
		.into_iter()
		.filter_map(|name| {
			state
				.cdp
				.get(&name)
				.map(|handle| cdp_tunnel::metadata(name, &handle))
		})
		.collect::<Vec<_>>();
	handles.sort_by(|left, right| left.name.cmp(&right.name));
	Ok(Json(handles))
}

#[utoipa::path(
	get,
	path = "/cdp/{name}",
	params(("name" = String, Path)),
	responses(
		(status = 101, description = "WebSocket upgrade"),
		(status = 200, body = CdpHandleResponse),
		(status = 404, body = ErrorBody, description = "handle not found")
	),
)]
pub async fn get_cdp(
	State(state): State<AppState>,
	Path(name): Path<String>,
	upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> ApiResult<Response> {
	let handle = state
		.cdp
		.get(&name)
		.ok_or_else(|| ApiError::NotFound(format!("cdp handle {name} not found")))?;
	if let Ok(upgrade) = upgrade {
		return Ok(cdp_tunnel::websocket_response(upgrade, handle, name));
	}
	Ok(Json(cdp_tunnel::metadata(name, &handle)).into_response())
}

#[utoipa::path(
	put,
	path = "/cdp/{name}",
	params(
		("name" = String, Path),
		("session" = Option<Uuid>, Query, description = "session UUID required when `scope=session`"),
	),
	request_body = NamedHandleConfig,
	responses(
		(status = 201),
		(status = 200),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 409, body = ErrorBody, description = "conflict"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn put_cdp(
	State(state): State<AppState>,
	Path(name): Path<String>,
	Query(query): Query<NamedHandleQuery>,
	Json(config): Json<NamedHandleConfig>,
) -> ApiResult<StatusCode> {
	let scope = resolve_named_scope(config.scope(), query.session)?;
	let (_, created) = cdp_tunnel::get_or_create_handle(&state.cdp, &name, config, scope).await?;
	Ok(if created {
		StatusCode::CREATED
	} else {
		StatusCode::OK
	})
}

#[utoipa::path(
	delete,
	path = "/cdp/{name}",
	params(("name" = String, Path)),
	responses(
		(status = 204),
		(status = 404, body = ErrorBody, description = "handle not found"),
	),
)]
pub async fn delete_cdp(
	State(state): State<AppState>,
	Path(name): Path<String>,
) -> ApiResult<StatusCode> {
	if cdp_tunnel::remove_handle(&state.cdp, &name).await {
		Ok(StatusCode::NO_CONTENT)
	} else {
		Err(ApiError::NotFound(format!("cdp handle {name} not found")))
	}
}

fn resolve_named_scope(
	requested: Option<RequestedHandleScope>,
	session_id: Option<uuid::Uuid>,
) -> ApiResult<HandleScope> {
	match requested.unwrap_or(RequestedHandleScope::Global) {
		RequestedHandleScope::Global => Ok(HandleScope::Global),
		RequestedHandleScope::Session => session_id
			.map(|session_id| HandleScope::Session { session_id })
			.ok_or_else(|| ApiError::BadRequest("scope=session requires ?session=<uuid>".to_owned())),
	}
}
