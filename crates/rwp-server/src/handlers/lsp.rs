//! `/lsp/{name}` — LSP subprocess tunnel.

use std::sync::Arc;

use axum::{
	extract::{Json, Path, Query, State, WebSocketUpgrade},
	http::StatusCode,
	response::{IntoResponse, Response},
};

use crate::{
	lsp_tunnel::{LspConfig, remove_and_shutdown, serve_websocket, spawn_idle_reaper},
	named::{HandleScope, RequestedHandleScope},
	protocol::{
		error::{ApiError, ApiResult, ErrorBody},
		requests::{NamedHandleConfig, NamedHandleQuery},
		responses::LspGetResponse,
	},
	state::{AppState, LspHandle},
};

#[utoipa::path(
	get,
	path = "/lsp/{name}",
	responses(
		(status = 101, description = "WebSocket upgrade"),
		(status = 200, body = LspGetResponse),
		(status = 404, body = ErrorBody, description = "handle not found")
	)
)]
pub async fn get_lsp(
	State(state): State<AppState>,
	Path(name): Path<String>,
	ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> ApiResult<Response> {
	let handle = state
		.lsp
		.get(&name)
		.ok_or_else(|| ApiError::NotFound(format!("lsp handle not found: {name}")))?;
	if let Ok(ws) = ws {
		return Ok(ws
			.on_upgrade(move |socket| async move { serve_websocket(socket, handle).await })
			.into_response());
	}
	Ok(axum::Json(handle.inner.get_response(name, &handle).await).into_response())
}

#[utoipa::path(
	put,
	path = "/lsp/{name}",
	params(
		("name" = String, Path),
		("session" = Option<uuid::Uuid>, Query, description = "session UUID required when `scope=session`"),
	),
	request_body = NamedHandleConfig,
	responses(
		(status = 201, body = LspGetResponse),
		(status = 200, body = LspGetResponse),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 409, body = ErrorBody, description = "conflict"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	)
)]
pub async fn put_lsp(
	State(state): State<AppState>,
	Path(name): Path<String>,
	Query(query): Query<NamedHandleQuery>,
	Json(body): Json<NamedHandleConfig>,
) -> ApiResult<impl IntoResponse> {
	let config = parse_lsp_config(body, query.session)?;
	if let Some(existing) = state.lsp.get(&name) {
		if existing.inner.config != config {
			return Err(ApiError::Conflict(format!(
				"lsp handle already exists with different config: {name}"
			)));
		}
		if existing.scope() != config.scope {
			return Err(ApiError::Conflict(format!(
				"lsp handle already exists with different scope: {name}"
			)));
		}
		return Ok((StatusCode::OK, Json(existing.inner.get_response(name, &existing).await)));
	}

	let spawned = LspHandle::spawn(config.clone()).await?;
	let inserted = state
		.lsp
		.get_or_insert_with(&name, config.scope, || Arc::clone(&spawned));
	if !Arc::ptr_eq(&inserted.inner, &spawned) {
		let _ = spawned.shutdown().await;
		if inserted.inner.config != spawned.config {
			return Err(ApiError::Conflict(format!(
				"lsp handle already exists with different config: {name}"
			)));
		}
		if inserted.scope() != config.scope {
			return Err(ApiError::Conflict(format!(
				"lsp handle already exists with different scope: {name}"
			)));
		}
		return Ok((StatusCode::OK, Json(inserted.inner.get_response(name, &inserted).await)));
	}

	spawn_idle_reaper(state.clone(), name.clone(), Arc::clone(&inserted));
	Ok((StatusCode::CREATED, Json(inserted.inner.get_response(name, &inserted).await)))
}

#[utoipa::path(
	delete,
	path = "/lsp/{name}",
	params(("name" = String, Path)),
	responses(
		(status = 204),
		(status = 404, body = ErrorBody, description = "handle not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	)
)]
pub async fn delete_lsp(
	State(state): State<AppState>,
	Path(name): Path<String>,
) -> ApiResult<StatusCode> {
	remove_and_shutdown(&state, &name).await?;
	Ok(StatusCode::NO_CONTENT)
}

fn parse_lsp_config(
	config: NamedHandleConfig,
	session_id: Option<uuid::Uuid>,
) -> ApiResult<LspConfig> {
	match config {
		NamedHandleConfig::Lsp {
			command,
			args,
			env,
			root_uri,
			initialization_options,
			idle_timeout_ms,
			scope,
		} => Ok(LspConfig {
			command,
			args,
			env,
			root_uri,
			initialization_options,
			idle_timeout_ms: idle_timeout_ms.unwrap_or(crate::lsp_tunnel::default_idle_timeout_ms()),
			scope: resolve_named_scope(scope, session_id)?,
		}),
		_ => Err(ApiError::BadRequest("expected named handle config kind=lsp".to_owned())),
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
