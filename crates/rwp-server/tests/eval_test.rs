use std::net::SocketAddr;

use reqwest::StatusCode;
use rwp_server::{AppState, build_router};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct EvalStatus {
	name:            String,
	lang:            String,
	status:          String,
	ref_count:       u32,
	transport:       Option<String>,
	idle_timeout_ms: Option<u64>,
}

#[allow(
	dead_code,
	reason = "integration test validates only the subset of eval events relevant here"
)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum EvalEvent {
	Stdout { data: String },
	Stderr { data: String },
	Display { mime: String, data: String },
	Result { text: String },
	Error { ename: String, evalue: String, traceback: Vec<String> },
	Status { state: String },
}

async fn start_server() -> SocketAddr {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(AppState::new(), Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});
	addr
}

fn url(addr: SocketAddr, name: &str) -> String {
	format!("http://{addr}/eval/{name}")
}

fn parse_events(body: &str) -> Vec<EvalEvent> {
	body
		.lines()
		.filter(|line| !line.is_empty())
		.map(|line| serde_json::from_str(line).expect("valid ndjson event"))
		.collect()
}

async fn put_python_kernel(client: &reqwest::Client, addr: SocketAddr, name: &str) {
	let response = client
		.put(url(addr, name))
		.json(
			&json!({"kind": "eval", "lang": "python", "transport": "stdio", "idle_timeout_ms": 1234}),
		)
		.send()
		.await
		.expect("put eval kernel");
	assert!(matches!(response.status(), StatusCode::CREATED | StatusCode::OK));
}

async fn exec(
	client: &reqwest::Client,
	addr: SocketAddr,
	name: &str,
	code: &str,
) -> Vec<EvalEvent> {
	let response = client
		.post(url(addr, name))
		.json(&json!({"code": code}))
		.send()
		.await
		.expect("post eval exec");
	assert_eq!(response.status(), StatusCode::OK);
	parse_events(&response.text().await.expect("ndjson body"))
}

#[tokio::test]
async fn put_get_exec_state_error_and_delete_python_kernel() {
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let name = "py-main";

	put_python_kernel(&client, addr, name).await;

	let get_response = client
		.get(url(addr, name))
		.send()
		.await
		.expect("get eval kernel");
	assert_eq!(get_response.status(), StatusCode::OK);
	let status: EvalStatus = get_response.json().await.expect("status json");
	assert_eq!(status.name, name);
	assert_eq!(status.lang, "python");
	assert_eq!(status.status, "idle");
	assert_eq!(status.ref_count, 0);
	assert_eq!(status.transport.as_deref(), Some("stdio"));
	assert_eq!(status.idle_timeout_ms, Some(1234));

	let print_events = exec(&client, addr, name, "print('hi')").await;
	assert!(
		print_events
			.iter()
			.any(|event| matches!(event, EvalEvent::Stdout { data } if data == "hi\n"))
	);
	assert!(matches!(print_events.last(), Some(EvalEvent::Status { state }) if state == "idle"));

	let first_state = exec(&client, addr, name, "x = 1").await;
	assert!(matches!(first_state.last(), Some(EvalEvent::Status { state }) if state == "idle"));
	let second_state = exec(&client, addr, name, "print(x)").await;
	assert!(
		second_state
			.iter()
			.any(|event| matches!(event, EvalEvent::Stdout { data } if data == "1\n"))
	);
	assert!(matches!(second_state.last(), Some(EvalEvent::Status { state }) if state == "idle"));

	let error_events = exec(&client, addr, name, "1/0").await;
	assert!(error_events.iter().any(|event| matches!(event, EvalEvent::Error { ename, evalue, .. } if ename == "ZeroDivisionError" && evalue.contains("division by zero"))));
	assert!(matches!(error_events.last(), Some(EvalEvent::Status { state }) if state == "idle"));

	let delete_response = client
		.delete(url(addr, name))
		.send()
		.await
		.expect("delete eval kernel");
	assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

	let missing_get = client
		.get(url(addr, name))
		.send()
		.await
		.expect("get deleted eval kernel");
	assert_eq!(missing_get.status(), StatusCode::NOT_FOUND);
}
