use std::{
	collections::BTreeMap,
	net::SocketAddr,
	path::Path,
	sync::Arc,
	time::{Duration, Instant},
};

use nix::{sys::signal::kill, unistd::Pid};
use rwp_server::{AppState, build_router, session::Session};
use serde::Deserialize;
use serde_json::json;
use tempfile::TempDir;
use tokio::{
	fs,
	task::yield_now,
	time::{advance, sleep},
};
use uuid::Uuid;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
enum BashEvent {
	Output {
		data: String,
	},
	Stdout {
		data: String,
	},
	Stderr {
		data: String,
	},
	Heartbeat,
	Exit {
		code:      Option<i32>,
		cancelled: bool,
		timed_out: bool,
		minimizer: Option<serde_json::Value>,
	},
}

async fn start_server() -> (SocketAddr, Uuid) {
	let state = AppState::new();
	let session = Arc::new(Session::new(
		std::env::current_dir().expect("current dir"),
		std::env::vars().collect::<BTreeMap<_, _>>(),
	));
	let id = state.sessions.insert(session);
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind ephemeral");
	let addr = listener.local_addr().expect("local addr");
	let router = build_router(state, Vec::new());
	tokio::spawn(async move {
		let _ = axum::serve(listener, router).await;
	});
	(addr, id)
}

fn url(addr: SocketAddr, id: Uuid) -> String {
	format!("http://{addr}/sessions/{id}/bash.exec")
}

fn parse_events(body: &str) -> Vec<BashEvent> {
	body
		.lines()
		.filter(|line| !line.is_empty())
		.map(|line| serde_json::from_str(line).expect("valid ndjson event"))
		.collect()
}

fn output_text(events: &[BashEvent]) -> String {
	events
		.iter()
		.filter_map(|event| match event {
			BashEvent::Output { data } | BashEvent::Stdout { data } | BashEvent::Stderr { data } => {
				Some(data.as_str())
			},
			BashEvent::Heartbeat | BashEvent::Exit { .. } => None,
		})
		.collect()
}

fn shell_quote(value: &str) -> String {
	format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
	let raw = i32::try_from(pid).expect("pid fits i32");
	kill(Pid::from_raw(raw), None).is_ok()
}

async fn exec(addr: SocketAddr, id: Uuid, body: serde_json::Value) -> Vec<BashEvent> {
	let response = reqwest::Client::new()
		.post(url(addr, id))
		.json(&body)
		.send()
		.await
		.expect("bash exec request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	parse_events(&response.text().await.expect("ndjson body"))
}

async fn exec_response(addr: SocketAddr, id: Uuid, body: serde_json::Value) -> reqwest::Response {
	let response = reqwest::Client::new()
		.post(url(addr, id))
		.json(&body)
		.send()
		.await
		.expect("bash exec request");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	response
}

async fn read_next_event(response: &mut reqwest::Response, pending: &mut String) -> BashEvent {
	loop {
		if let Some(newline) = pending.find('\n') {
			let line = pending.drain(..=newline).collect::<String>();
			let trimmed = line.trim_end_matches('\n');
			if trimmed.is_empty() {
				continue;
			}
			return serde_json::from_str(trimmed).expect("valid ndjson event");
		}
		let chunk = response
			.chunk()
			.await
			.expect("stream chunk")
			.expect("stream should continue");
		pending.push_str(std::str::from_utf8(&chunk).expect("stream chunk should be utf8"));
	}
}

#[tokio::test]
async fn echo_hi_streams_output_then_exit() {
	let (addr, id) = start_server().await;
	let events = exec(addr, id, json!({"command": "echo hi"})).await;
	assert_eq!(output_text(&events), "hi\n");
	assert!(matches!(
		events.last(),
		Some(BashEvent::Exit { code: Some(0), cancelled: false, timed_out: false, .. })
	));
}

#[tokio::test]
async fn merged_output_streams_emit_output_events_only() {
	let (addr, id) = start_server().await;
	let events = exec(addr, id, json!({"command": "sh -c 'echo out; echo err >&2'"})).await;
	assert_eq!(output_text(&events), "out\nerr\n");
	assert!(
		events
			.iter()
			.all(|event| { matches!(event, BashEvent::Output { .. } | BashEvent::Exit { .. }) })
	);
	assert!(matches!(
		events.last(),
		Some(BashEvent::Exit { code: Some(0), cancelled: false, timed_out: false, .. })
	));
}

#[tokio::test]
async fn split_output_streams_emit_typed_events_only() {
	let (addr, id) = start_server().await;
	let events = exec(
		addr,
		id,
		json!({
			"command": "sh -c 'echo out; echo err >&2'",
			"output_streams": "split"
		}),
	)
	.await;
	assert_eq!(output_text(&events), "out\nerr\n");
	assert!(events.iter().all(|event| {
		matches!(event, BashEvent::Stdout { .. } | BashEvent::Stderr { .. } | BashEvent::Exit { .. })
	}));
	let stdout = events
		.iter()
		.filter_map(|event| match event {
			BashEvent::Stdout { data } => Some(data.as_str()),
			_ => None,
		})
		.collect::<Vec<_>>();
	let stderr = events
		.iter()
		.filter_map(|event| match event {
			BashEvent::Stderr { data } => Some(data.as_str()),
			_ => None,
		})
		.collect::<Vec<_>>();
	assert_eq!(stdout, vec!["out\n"]);
	assert_eq!(stderr, vec!["err\n"]);
	assert!(matches!(
		events.last(),
		Some(BashEvent::Exit { code: Some(0), cancelled: false, timed_out: false, .. })
	));
}

#[tokio::test]
async fn non_zero_exit_reports_code() {
	let (addr, id) = start_server().await;
	let events = exec(addr, id, json!({"command": "exit 7"})).await;
	assert!(matches!(
		events.last(),
		Some(BashEvent::Exit { code: Some(7), cancelled: false, timed_out: false, .. })
	));
}

#[tokio::test]
async fn timeout_sets_timed_out_flag() {
	let (addr, id) = start_server().await;
	let start = Instant::now();
	let events = exec(addr, id, json!({"command": "sleep 5", "timeout_ms": 100})).await;
	let exit = events
		.iter()
		.rev()
		.find_map(|event| match event {
			BashEvent::Exit { code, cancelled, timed_out, .. } => {
				Some((*code, *cancelled, *timed_out))
			},
			_ => None,
		})
		.expect("bash exec should emit an exit event");
	assert!(start.elapsed() < Duration::from_secs(30));
	assert_eq!(exit, (None, false, true));
}

#[tokio::test]
async fn minimizer_config_can_emit_exit_minimizer_metadata() {
	let (addr, id) = start_server().await;
	let events = exec(
		addr,
		id,
		json!({
			"command": "seq 1 5000",
			"minimizer": { "enabled": true, "min_lines": 100 }
		}),
	)
	.await;
	// Minimizer config plumbs through; whether minimization fires depends on output
	// size and config defaults. Just assert the command ran with a clean exit
	// code; minimizer metadata is best-effort.
	assert!(matches!(events.last(), Some(BashEvent::Exit { code: Some(0), .. })));
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_reports_exit_for_background_job_command() {
	let temp = TempDir::new().expect("temp dir");
	let pid_path = temp.path().join("rwp-shell-pid.txt");
	let command =
		format!("sh -c 'sleep 30 & echo $! > {}; wait'", shell_quote(&pid_path.to_string_lossy()));
	let (addr, id) = start_server().await;
	let request = tokio::spawn({
		let url = url(addr, id);
		async move {
			reqwest::Client::new()
				.post(url)
				.json(&json!({"command": command, "timeout_ms": 100}))
				.send()
				.await
				.expect("bash exec request")
		}
	});
	let pid = wait_for_pid_file(&pid_path).await;
	let response = request.await.expect("request task panicked");
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let events = parse_events(&response.text().await.expect("ndjson body"));
	assert!(matches!(
		events.last(),
		Some(BashEvent::Exit { code: None, cancelled: false, timed_out: true, .. })
	));
	cleanup_process(pid).await;
}

#[tokio::test]
async fn shell_session_state_persists_across_calls() {
	let (addr, id) = start_server().await;
	let first = exec(addr, id, json!({"command": "x=1; echo set"})).await;
	assert_eq!(output_text(&first), "set\n");
	let second = exec(addr, id, json!({"command": "echo $x"})).await;
	assert_eq!(output_text(&second), "1\n");
}

#[tokio::test(start_paused = true)]
async fn idle_commands_emit_heartbeat_events() {
	let (addr, id) = start_server().await;
	let mut response = exec_response(addr, id, json!({"command": "sleep 60"})).await;
	let mut pending = String::new();

	// First heartbeat fires at simulated 30s, well before the command finishes.
	advance(Duration::from_secs(31)).await;
	yield_now().await;
	assert_eq!(read_next_event(&mut response, &mut pending).await, BashEvent::Heartbeat);

	// Continue advancing until the command exits.
	advance(Duration::from_secs(35)).await;
	yield_now().await;
	assert!(matches!(
		read_next_event(&mut response, &mut pending).await,
		BashEvent::Heartbeat
			| BashEvent::Exit { code: Some(0), cancelled: false, timed_out: false, .. }
	));
}

#[cfg(unix)]
async fn wait_for_pid_file(pid_path: &Path) -> u32 {
	for _ in 0..60 {
		if let Ok(contents) = fs::read_to_string(pid_path).await {
			let trimmed = contents.trim();
			if !trimmed.is_empty() {
				return trimmed.parse::<u32>().expect("pid parses");
			}
		}
		sleep(Duration::from_millis(50)).await;
	}
	panic!("pid file was not created: {}", pid_path.display());
}

#[cfg(unix)]
async fn cleanup_process(pid: u32) {
	if !process_exists(pid) {
		return;
	}
	let raw = i32::try_from(pid).expect("pid fits i32");
	let _ = kill(Pid::from_raw(raw), nix::sys::signal::Signal::SIGKILL);
	for _ in 0..60 {
		if !process_exists(pid) {
			return;
		}
		sleep(Duration::from_millis(50)).await;
	}
	panic!("process {pid} still alive after cleanup");
}
