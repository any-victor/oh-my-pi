//! `/dap/{name}` — DAP subprocess tunnel.

use std::sync::Arc;

use axum::{
	Json,
	extract::{Path, Query, State, WebSocketUpgrade},
	http::StatusCode,
	response::{IntoResponse, Response},
};

use crate::{
	dap_tunnel::{self, DapSpawnConfig},
	named::{HandleScope, RequestedHandleScope},
	protocol::{
		error::{ApiError, ApiResult, ErrorBody},
		requests::{NamedHandleConfig, NamedHandleQuery},
	},
	state::AppState,
};

#[utoipa::path(
	get,
	path = "/dap/{name}",
	params(("name" = String, Path)),
	responses(
		(status = 101, description = "WebSocket upgrade"),
		(status = 200, description = "Status"),
		(status = 404, body = ErrorBody, description = "handle not found"),
	)
)]
pub async fn get_dap(
	State(state): State<AppState>,
	Path(name): Path<String>,
	ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> ApiResult<Response> {
	let handle = state
		.dap
		.get(&name)
		.ok_or_else(|| ApiError::NotFound(format!("dap handle `{name}`")))?;
	if let Ok(upgrade) = ws {
		let guard = handle.retain();
		return Ok(upgrade.on_upgrade(move |socket| dap_tunnel::serve_websocket(socket, guard)));
	}
	Ok(Json(serde_json::json!({ "name": name, "kind": "dap" })).into_response())
}

#[utoipa::path(
	put,
	path = "/dap/{name}",
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
	)
)]
pub async fn put_dap(
	State(state): State<AppState>,
	Path(name): Path<String>,
	Query(query): Query<NamedHandleQuery>,
	Json(config): Json<NamedHandleConfig>,
) -> ApiResult<StatusCode> {
	let NamedHandleConfig::Dap {
		command,
		args,
		env,
		transport,
		host,
		port,
		retry_ms,
		retry_attempts,
		idle_timeout_ms,
		scope,
	} = config
	else {
		return Err(ApiError::BadRequest(format!("expected NamedHandleConfig::Dap for /dap/{name}")));
	};
	let scope = resolve_named_scope(scope, query.session)?;

	if let Some(existing) = state.dap.get(&name) {
		if existing.scope() != scope {
			return Err(ApiError::Conflict(format!(
				"dap handle `{name}` already exists with different scope"
			)));
		}
		return Ok(StatusCode::OK);
	}

	let spawned = crate::state::DapHandle::spawn(DapSpawnConfig {
		command,
		args,
		env,
		transport,
		host,
		port,
		retry_ms,
		retry_attempts,
		idle_timeout_ms: idle_timeout_ms.unwrap_or(dap_tunnel::default_idle_timeout_ms()),
		scope,
	})
	.await?;
	let handle = state.dap.get_or_insert_with_timeout(
		&name,
		std::time::Duration::from_millis(handle_idle_timeout_ms(idle_timeout_ms)),
		scope,
		|| Arc::clone(&spawned),
	);
	if Arc::ptr_eq(&handle.inner, &spawned) {
		dap_tunnel::spawn_idle_reaper(state, name, handle);
		Ok(StatusCode::CREATED)
	} else {
		spawned.shutdown().await?;
		if handle.scope() != scope {
			return Err(ApiError::Conflict(format!(
				"dap handle `{name}` already exists with different scope"
			)));
		}
		Ok(StatusCode::OK)
	}
}
const fn handle_idle_timeout_ms(idle_timeout_ms: Option<u64>) -> u64 {
	match idle_timeout_ms {
		Some(idle_timeout_ms) => idle_timeout_ms,
		None => dap_tunnel::default_idle_timeout_ms(),
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

#[utoipa::path(
	delete,
	path = "/dap/{name}",
	params(("name" = String, Path)),
	responses(
		(status = 204),
		(status = 404, body = ErrorBody, description = "handle not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	)
)]
pub async fn delete_dap(
	State(state): State<AppState>,
	Path(name): Path<String>,
) -> ApiResult<StatusCode> {
	let handle = state
		.dap
		.remove(&name)
		.ok_or_else(|| ApiError::NotFound(format!("dap handle `{name}`")))?;
	handle.on_close.notify_waiters();
	handle.inner.shutdown().await?;
	Ok(StatusCode::NO_CONTENT)
}
