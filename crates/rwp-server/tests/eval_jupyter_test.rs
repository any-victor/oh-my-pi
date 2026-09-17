use std::{net::SocketAddr, process::Command, time::Duration};

use reqwest::StatusCode;
use rwp_server::{AppState, build_router};
use serde::Deserialize;
use serde_json::json;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

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

fn has_ipykernel() -> bool {
	["python3", "python"].into_iter().any(|program| {
		Command::new(program)
			.args(["-c", "import ipykernel"])
			.status()
			.is_ok_and(|status| status.success())
	})
}

fn ipykernel_process_count() -> usize {
	let output = Command::new("ps")
		.args(["-Ao", "command="])
		.output()
		.expect("run ps for ipykernel count");
	assert!(output.status.success(), "ps failed with status {:?}", output.status);
	String::from_utf8_lossy(&output.stdout)
		.lines()
		.filter(|line| line.contains("ipykernel_launcher"))
		.count()
}

async fn wait_for_process_delta(baseline: usize, minimum_delta: usize) {
	let deadline = Instant::now() + Duration::from_secs(10);
	loop {
		if ipykernel_process_count() >= baseline + minimum_delta {
			return;
		}
		assert!(Instant::now() < deadline, "ipykernel child did not appear");
		sleep(Duration::from_millis(100)).await;
	}
}

async fn wait_for_process_count_at_most(max_count: usize) {
	let deadline = Instant::now() + Duration::from_secs(10);
	loop {
		if ipykernel_process_count() <= max_count {
			return;
		}
		assert!(Instant::now() < deadline, "ipykernel child did not exit");
		sleep(Duration::from_millis(100)).await;
	}
}

/// Manual run: `cargo test -p rwp-server --test eval_jupyter_test -- --ignored`
#[tokio::test]
#[ignore = "requires a host python with ipykernel installed"]
async fn python_eval_uses_ipykernel_transport() {
	if !has_ipykernel() {
		eprintln!("skipping eval_jupyter_test: ipykernel is not installed");
		return;
	}

	let baseline_processes = ipykernel_process_count();
	let addr = start_server().await;
	let client = reqwest::Client::new();
	let name = format!("py-jupyter-{}", Uuid::new_v4().simple());

	let put_response = client
		.put(url(addr, &name))
		.json(&json!({"kind": "eval", "lang": "python", "transport": "jupyter"}))
		.send()
		.await
		.expect("put eval kernel");
	assert!(matches!(put_response.status(), StatusCode::CREATED | StatusCode::OK));
	wait_for_process_delta(baseline_processes, 1).await;

	let print_events = exec(&client, addr, &name, "print('hi')").await;
	assert!(
		print_events
			.iter()
			.any(|event| matches!(event, EvalEvent::Stdout { data } if data == "hi\n"))
	);
	assert!(matches!(print_events.last(), Some(EvalEvent::Status { state }) if state == "idle"));

	let error_events = exec(&client, addr, &name, "1 / 0").await;
	assert!(error_events.iter().any(
		|event| matches!(event, EvalEvent::Error { ename, .. } if ename == "ZeroDivisionError")
	));
	assert!(matches!(error_events.last(), Some(EvalEvent::Status { state }) if state == "idle"));

	let display_events =
		exec(&client, addr, &name, "import IPython.display; IPython.display.HTML('<b>ok</b>')").await;
	assert!(display_events.iter().any(
		|event| matches!(event, EvalEvent::Display { mime, data } if mime == "text/html" && data.contains("<b>ok</b>"))
	));
	assert!(matches!(display_events.last(), Some(EvalEvent::Status { state }) if state == "idle"));

	let delete_response = client
		.delete(url(addr, &name))
		.send()
		.await
		.expect("delete eval kernel");
	assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
	wait_for_process_count_at_most(baseline_processes).await;
}
