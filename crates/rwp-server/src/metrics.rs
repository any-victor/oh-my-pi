use std::{sync::LazyLock, time::Instant};

use ::metrics::{counter, gauge, histogram};
use axum::{
	extract::{MatchedPath, Request, State},
	http::header,
	middleware::Next,
	response::{IntoResponse, Response},
};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use crate::state::AppState;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

static PROMETHEUS_HANDLE: LazyLock<PrometheusHandle> = LazyLock::new(|| {
	PrometheusBuilder::new()
		.install_recorder()
		.expect("prometheus recorder should install once per process")
});

pub fn install_recorder() -> &'static PrometheusHandle {
	&PROMETHEUS_HANDLE
}

pub async fn middleware(request: Request, next: Next) -> Response {
	let started = Instant::now();
	let method = request.method().to_string();
	let path = request
		.extensions()
		.get::<MatchedPath>()
		.map_or_else(|| request.uri().path().to_owned(), |matched| matched.as_str().to_owned());
	let response = next.run(request).await;
	let status = response.status().as_u16().to_string();

	counter!(
		"rwp_requests_total",
		"path" => path.clone(),
		"method" => method.clone(),
		"status" => status,
	)
	.increment(1);
	histogram!("rwp_request_duration_seconds", "path" => path, "method" => method)
		.record(started.elapsed().as_secs_f64());

	response
}

pub async fn scrape(State(state): State<AppState>) -> impl IntoResponse {
	gauge!("rwp_sessions_live").set(state.sessions.len() as f64);
	gauge!("rwp_handles_live", "kind" => "eval").set(state.eval.names().len() as f64);
	gauge!("rwp_handles_live", "kind" => "lsp").set(state.lsp.names().len() as f64);
	gauge!("rwp_handles_live", "kind" => "dap").set(state.dap.names().len() as f64);
	gauge!("rwp_handles_live", "kind" => "cdp").set(state.cdp.names().len() as f64);

	([(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)], install_recorder().render())
}
