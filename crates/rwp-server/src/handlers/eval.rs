//! `/eval/{name}` — persistent eval kernels over stdio.

use std::{
	pin::Pin,
	sync::{Arc, Weak},
	task::{Context, Poll},
	time::Duration,
};

use axum::{
	Json,
	body::Body,
	extract::{Path, Query, State},
	http::{HeaderValue, StatusCode, header},
	response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::{
	eval_kernel::{
		EvalEvent, EvalExecRequest, EvalKernel, EvalLanguage, EvalStatusResponse, KernelState,
		default_idle_timeout_ms,
	},
	named::{Handle, HandleScope, RequestedHandleScope},
	protocol::{
		error::{ApiError, ApiResult, ErrorBody},
		requests::{EvalTransport, NamedHandleConfig, NamedHandleQuery},
	},
	state::{AppState, EvalHandle},
};

const IDLE_REAPER_INTERVAL: Duration = Duration::from_secs(30);
const NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";

struct CancelOnDropStream {
	inner:  ReceiverStream<Bytes>,
	cancel: CancellationToken,
}

impl Stream for CancelOnDropStream {
	type Item = Result<Bytes, std::convert::Infallible>;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		Pin::new(&mut self.inner)
			.poll_next(cx)
			.map(|item| item.map(Ok))
	}
}

impl Drop for CancelOnDropStream {
	fn drop(&mut self) {
		self.cancel.cancel();
	}
}

#[utoipa::path(
	get,
	path = "/eval/{name}",
	params(("name" = String, Path)),
	responses(
		(status = 200, body = EvalStatusResponse),
		(status = 404, body = ErrorBody, description = "kernel not found"),
	),
)]
pub async fn get_eval(
	State(state): State<AppState>,
	Path(name): Path<String>,
) -> ApiResult<impl IntoResponse> {
	let handle = state
		.eval
		.get(&name)
		.ok_or_else(|| ApiError::NotFound(format!("eval kernel {name} not found")))?;
	Ok(Json(EvalStatusResponse {
		name,
		lang: handle.inner.kernel.lang().as_str().to_owned(),
		status: handle.inner.kernel.state().await,
		ref_count: handle.refcount(),
		transport: Some(handle.inner.transport),
		idle_timeout_ms: Some(handle.inner.idle_timeout_ms),
	}))
}

#[utoipa::path(
	put,
	path = "/eval/{name}",
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
pub async fn put_eval(
	State(state): State<AppState>,
	Path(name): Path<String>,
	Query(query): Query<NamedHandleQuery>,
	Json(body): Json<NamedHandleConfig>,
) -> ApiResult<StatusCode> {
	let NamedHandleConfig::Eval { lang, kernelspec: _, transport, idle_timeout_ms, scope } = body
	else {
		return Err(ApiError::BadRequest("expected named eval config".to_owned()));
	};
	let lang =
		EvalLanguage::parse(&lang).map_err(|error| ApiError::BadRequest(error.to_string()))?;
	let scope = resolve_named_scope(scope, query.session)?;
	if let Some(existing) = state.eval.get(&name) {
		if existing.scope() != scope {
			return Err(ApiError::Conflict(format!(
				"eval kernel {name} already exists with different scope"
			)));
		}
		ensure_matching_lang(&name, &existing, lang)?;
		return Ok(StatusCode::OK);
	}

	let requested_transport = transport.unwrap_or(EvalTransport::Stdio);
	let idle_timeout_ms = idle_timeout_ms.unwrap_or(default_idle_timeout_ms());
	let prepared = Arc::new(EvalHandle::new(
		EvalKernel::spawn(lang, Some(requested_transport)).await?,
		requested_transport,
		idle_timeout_ms,
	));
	let idle_timeout = Duration::from_millis(idle_timeout_ms);
	let handle = state
		.eval
		.get_or_insert_with_timeout(&name, idle_timeout, scope, || Arc::clone(&prepared));
	if Arc::ptr_eq(&handle.inner, &prepared) {
		let reaper =
			tokio::spawn(run_idle_reaper(state.clone(), name.clone(), Arc::downgrade(&handle)));
		*prepared.reaper.lock().await = Some(reaper);
		Ok(StatusCode::CREATED)
	} else {
		prepared.kernel.shutdown().await?;
		if handle.scope() != scope {
			return Err(ApiError::Conflict(format!(
				"eval kernel {name} already exists with different scope"
			)));
		}
		ensure_matching_lang(&name, &handle, lang)?;
		Ok(StatusCode::OK)
	}
}

#[utoipa::path(
	delete,
	path = "/eval/{name}",
	params(("name" = String, Path)),
	responses(
		(status = 204),
		(status = 404, body = ErrorBody, description = "kernel not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn delete_eval(
	State(state): State<AppState>,
	Path(name): Path<String>,
) -> ApiResult<StatusCode> {
	let handle = state
		.eval
		.remove(&name)
		.ok_or_else(|| ApiError::NotFound(format!("eval kernel {name} not found")))?;
	let reaper = handle.inner.reaper.lock().await.take();
	if let Some(reaper) = reaper {
		reaper.abort();
	}
	handle.inner.kernel.shutdown().await?;
	Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
	post,
	path = "/eval/{name}",
	params(("name" = String, Path)),
	request_body = EvalExecRequest,
	responses(
		(status = 200, content_type = "application/x-ndjson"),
		(status = 404, body = ErrorBody, description = "kernel not found"),
	),
)]
pub async fn exec_eval(
	State(state): State<AppState>,
	Path(name): Path<String>,
	Json(body): Json<EvalExecRequest>,
) -> ApiResult<impl IntoResponse> {
	let handle = state
		.eval
		.get(&name)
		.ok_or_else(|| ApiError::NotFound(format!("eval kernel {name} not found")))?;
	let guard = handle.retain();
	let mut request = body;
	request.code = with_cwd_prelude(handle.inner.kernel.lang(), request.cwd.clone(), &request.code);
	let cancel = CancellationToken::new();
	let cancel_for_forwarder = cancel.clone();
	let cancel_for_runner = cancel.clone();
	let (event_tx, mut event_rx) = mpsc::channel::<EvalEvent>(32);
	let (body_tx, body_rx) = mpsc::channel::<Bytes>(32);

	tokio::spawn(async move {
		while let Some(event) = event_rx.recv().await {
			if body_tx.send(encode_event(&event)).await.is_err() {
				cancel_for_forwarder.cancel();
				break;
			}
		}
	});

	tokio::spawn(async move {
		if let Err(error) = guard
			.inner()
			.kernel
			.execute(&request, event_tx.clone(), cancel_for_runner.clone())
			.await && !cancel_for_runner.is_cancelled()
		{
			let _ = event_tx
				.send(EvalEvent::Error {
					ename:     "KernelError".to_owned(),
					evalue:    error.to_string(),
					traceback: vec![format!("kernel execution failed: {error:#}")],
				})
				.await;
			let _ = event_tx
				.send(EvalEvent::Status { state: KernelState::Idle })
				.await;
		}
	});

	let mut response = Response::new(Body::from_stream(CancelOnDropStream {
		inner: ReceiverStream::new(body_rx),
		cancel,
	}));
	*response.status_mut() = StatusCode::OK;
	response
		.headers_mut()
		.insert(header::CONTENT_TYPE, HeaderValue::from_static(NDJSON_CONTENT_TYPE));
	Ok(response)
}

fn with_cwd_prelude(lang: EvalLanguage, cwd: Option<String>, code: &str) -> String {
	let Some(cwd) = cwd else {
		return code.to_owned();
	};
	let quoted = serde_json::to_string(&cwd).unwrap_or_else(|_| "\".\"".to_owned());
	match lang {
		EvalLanguage::Python => format!("import os\nos.chdir({quoted})\n{code}"),
		EvalLanguage::Javascript => format!("process.chdir({quoted});\n{code}"),
	}
}

fn ensure_matching_lang(
	name: &str,
	handle: &Arc<Handle<EvalHandle>>,
	requested: EvalLanguage,
) -> ApiResult<()> {
	if handle.inner.kernel.lang() == requested {
		Ok(())
	} else {
		Err(ApiError::Conflict(format!(
			"eval kernel {name} already exists with lang {}",
			handle.inner.kernel.lang()
		)))
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

fn encode_event(event: &EvalEvent) -> Bytes {
	let mut encoded = serde_json::to_vec(event).expect("eval events should serialize");
	encoded.push(b'\n');
	Bytes::from(encoded)
}

async fn run_idle_reaper(state: AppState, name: String, expected: Weak<Handle<EvalHandle>>) {
	loop {
		let Some(handle) = expected.upgrade() else {
			return;
		};
		let idle_timeout = handle.idle_timeout();
		let remaining = idle_timeout.saturating_sub(handle.last_active().elapsed());
		tokio::time::sleep(remaining.min(IDLE_REAPER_INTERVAL)).await;
		let Some(current) = state.eval.get(&name) else {
			return;
		};
		if !Arc::ptr_eq(&handle, &current) {
			return;
		}
		if current.refcount() != 0 || current.last_active().elapsed() < idle_timeout {
			continue;
		}
		drop(current);
		if let Some(removed) = state.eval.remove(&name)
			&& Arc::ptr_eq(&removed, &handle)
		{
			let _ = removed.inner.kernel.shutdown().await;
		}
		return;
	}
}
