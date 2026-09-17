use std::{
	path::PathBuf,
	thread::JoinHandle,
	time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use hex::FromHex;
use hmac::{Hmac, Mac};
use nix::{
	sys::signal::{Signal, kill},
	unistd::Pid,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::{
	process::{Child, ChildStderr, Command},
	sync::{Mutex, mpsc},
	time::timeout,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{EvalEvent, EvalExecRequest, KernelState};

type HmacSha256 = Hmac<Sha256>;

const PYTHON_PROGRAMS: &[&str] = &["python3", "python"];
const JUPYTER_MESSAGE_DELIMITER: &[u8] = b"<IDS|MSG>";
const SOCKET_POLL_TIMEOUT_MS: i64 = 100;
const KERNEL_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize)]
struct ConnectionFile {
	transport:        String,
	ip:               String,
	control_port:     u16,
	shell_port:       u16,
	stdin_port:       u16,
	hb_port:          u16,
	iopub_port:       u16,
	key:              String,
	signature_scheme: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct JupyterHeader {
	msg_id:   String,
	username: String,
	session:  String,
	date:     String,
	msg_type: String,
	version:  String,
}

#[derive(Debug)]
struct WireMessage {
	_identities:   Vec<Vec<u8>>,
	header:        JupyterHeader,
	parent_header: Value,
	content:       Value,
}

/// Messages sent from the async layer to the worker thread
#[derive(Debug)]
enum OutboundMessage {
	ExecuteRequest {
		#[allow(dead_code, reason = "Reserved for future request tracking")]
		msg_id: String,
		code:   String,
	},
	#[allow(dead_code, reason = "Reserved for future graceful shutdown support")]
	ShutdownRequest,
	/// Poison pill to stop the worker thread
	Shutdown,
}

/// Messages sent from the worker thread to the async layer
#[derive(Debug)]
#[allow(
	clippy::large_enum_variant,
	reason = "WireMessage contains dynamic JSON content; Boxing would be more complex"
)]
enum InboundMessage {
	IopubEvent(WireMessage),
	ShellReply {
		msg_type: String,
		msg_id:   String,
		content:  Value,
	},
	#[allow(dead_code, reason = "Reserved for future kernel readiness signaling")]
	ReadyResult(anyhow::Result<()>),
	/// Worker thread is shutting down
	Shutdown,
}

pub(super) struct JupyterKernelRuntime {
	child:           Child,
	stderr_task:     tokio::task::JoinHandle<()>,
	// Channels to communicate with worker thread
	outbound_tx:     mpsc::UnboundedSender<OutboundMessage>,
	inbound_rx:      Mutex<mpsc::UnboundedReceiver<InboundMessage>>,
	worker:          Option<JoinHandle<()>>,
	#[allow(dead_code, reason = "Held for kernel lifetime and future use")]
	session_id:      String,
	#[allow(dead_code, reason = "Held for kernel lifetime and future use")]
	key:             Vec<u8>,
	connection_file: PathBuf,
}

impl Drop for JupyterKernelRuntime {
	fn drop(&mut self) {
		// Kill the child process
		drop(self.child.kill());
		// Abort the stderr drain task
		self.stderr_task.abort();
		// Abort the worker thread
		// Note: worker is a std::thread::JoinHandle, not tokio::task::JoinHandle
		// The worker thread will exit when the child process is killed and sockets
		// close Clean up connection file
		cleanup_connection_file(&self.connection_file);
	}
}

enum IopubAction {
	Continue,
	Idle,
	ReceiverClosed,
}

impl JupyterKernelRuntime {
	pub(super) async fn spawn() -> anyhow::Result<Option<Self>> {
		let Some(program) = probe_ipykernel_program() else {
			return Ok(None);
		};
		let reserved = reserve_ports()?;
		let ports = [
			reserved[0].local_addr().context("read shell port")?.port(),
			reserved[1].local_addr().context("read iopub port")?.port(),
			reserved[2].local_addr().context("read stdin port")?.port(),
			reserved[3]
				.local_addr()
				.context("read control port")?
				.port(),
			reserved[4].local_addr().context("read hb port")?.port(),
		];
		let key = Uuid::new_v4().simple().to_string();
		let connection_file = write_connection_file(ports, &key)?;
		drop(reserved);
		let mut child = Command::new(&program)
			.args(["-m", "ipykernel_launcher", "-f"])
			.arg(&connection_file)
			.stdin(std::process::Stdio::null())
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::piped())
			.kill_on_drop(true)
			.spawn()
			.with_context(|| format!("spawn ipykernel via {program}"))?;
		let stderr = child
			.stderr
			.take()
			.ok_or_else(|| anyhow!("missing ipykernel stderr"))?;
		let stderr_task = tokio::spawn(drain_stderr(program.clone(), stderr));

		// Create channels for worker thread communication
		let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
		let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

		let session_id = Uuid::new_v4().to_string();
		let hmac_key = key.clone().into_bytes();

		let worker_handle = spawn_zmq_worker(
			ConnectionFile {
				transport:        "tcp".to_owned(),
				ip:               "127.0.0.1".to_owned(),
				shell_port:       ports[0],
				iopub_port:       ports[1],
				stdin_port:       ports[2],
				control_port:     ports[3],
				hb_port:          ports[4],
				key:              key.clone(),
				signature_scheme: "hmac-sha256".to_owned(),
			},
			session_id.clone(),
			hmac_key,
			outbound_rx,
			inbound_tx,
		);

		let mut runtime = Self {
			child,
			stderr_task,
			outbound_tx,
			inbound_rx: Mutex::new(inbound_rx),
			worker: Some(worker_handle),
			session_id,
			key: key.into_bytes(),
			connection_file,
		};

		// Wait for kernel ready
		runtime
			.outbound_tx
			.send(OutboundMessage::ExecuteRequest {
				msg_id: Uuid::new_v4().to_string(),
				code:   String::new(),
			})
			.map_err(|_| anyhow!("worker channel closed during ready check"))?;

		if let Ok(result) = timeout(KERNEL_READY_TIMEOUT, wait_for_ready(&runtime.inbound_rx)).await {
			result?;
			Ok(Some(runtime))
		} else {
			// Cleanup resources before returning error
			let _kill = runtime.child.start_kill();
			runtime.stderr_task.abort();
			cleanup_connection_file(&runtime.connection_file);
			bail!("ipykernel did not become ready within {}s", KERNEL_READY_TIMEOUT.as_secs())
		}
	}

	pub(super) async fn execute(
		&mut self,
		request: &EvalExecRequest,
		events: mpsc::Sender<EvalEvent>,
		cancel: CancellationToken,
		shutdown: CancellationToken,
	) -> anyhow::Result<()> {
		let execute_id = Uuid::new_v4().to_string();
		self
			.outbound_tx
			.send(OutboundMessage::ExecuteRequest {
				msg_id: execute_id.clone(),
				code:   request.code.clone(),
			})
			.map_err(|_| anyhow!("worker channel closed"))?;

		let deadline = request
			.timeout_ms
			.map(|ms| Instant::now() + Duration::from_millis(ms));
		let mut saw_idle = false;
		let mut saw_reply = false;
		#[allow(
			unused_assignments,
			reason = "Initial value is overwritten before use but provides default"
		)]
		let mut need_restart = false;

		loop {
			if shutdown.is_cancelled() {
				return Ok(());
			}

			if cancel.is_cancelled() {
				need_restart = true;
				break;
			}
			if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
				let _ = events
					.send(EvalEvent::Error {
						ename:     "TimeoutError".to_owned(),
						evalue:    format!(
							"execution exceeded {} ms",
							request.timeout_ms.unwrap_or_default()
						),
						traceback: vec!["execution cancelled after timeout".to_owned()],
					})
					.await;
				let _ = events
					.send(EvalEvent::Status { state: KernelState::Idle })
					.await;

				need_restart = true;
				break;
			}

			let mut inbound = self.inbound_rx.lock().await;
			match tokio::time::timeout(Duration::from_millis(100), inbound.recv()).await {
				Ok(Some(message)) => match message {
					InboundMessage::IopubEvent(msg) if self.parent_matches(&msg, &execute_id) => {
						match self.forward_iopub_event(&events, &msg).await? {
							IopubAction::Continue => {},
							IopubAction::Idle => saw_idle = true,

							IopubAction::ReceiverClosed => {
								need_restart = true;
								break;
							},
						}
					},

					InboundMessage::ShellReply { msg_type, msg_id, content }
						if msg_id == execute_id && msg_type == "execute_reply" =>
					{
						if content.get("status").and_then(Value::as_str) == Some("error")
							&& content.get("ename").is_some()
						{
							tracing::debug!(target: "rwp_server::eval", msg_id = %execute_id, "execute_reply returned error status");
						}
						saw_reply = true;
					},
					_ => {},
				},
				Ok(None) => {
					// Worker thread shut down
					return Ok(());
				},
				Err(_) => {
					// Timeout, continue loop
				},
			}
			drop(inbound);

			if saw_idle && saw_reply {
				return Ok(());
			}
		}

		if need_restart {
			self.restart().await?;
		}

		Ok(())
	}

	pub(super) async fn shutdown(&mut self) -> anyhow::Result<()> {
		if let Some(pid) = self.child.id() {
			let _ =
				kill(Pid::from_raw(i32::try_from(pid).context("convert pid to i32")?), Signal::SIGTERM);
		}
		if let Ok(wait_result) = timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await {
			wait_result?;
		} else {
			let _ = self.child.kill().await;
			let _ = self.child.wait().await;
		}
		self.stderr_task.abort();
		let _ = self.outbound_tx.send(OutboundMessage::Shutdown);
		let _ = self.worker.take().map(|h| h.join());
		cleanup_connection_file(&self.connection_file);
		Ok(())
	}

	async fn restart(&mut self) -> anyhow::Result<()> {
		self.shutdown().await?;
		let new_self = Self::spawn()
			.await?
			.ok_or_else(|| anyhow!("ipykernel unavailable while restarting python eval kernel"))?;
		*self = new_self;
		Ok(())
	}

	async fn forward_iopub_event(
		&self,
		events: &mpsc::Sender<EvalEvent>,
		message: &WireMessage,
	) -> anyhow::Result<IopubAction> {
		match message.header.msg_type.as_str() {
			"stream" => {
				let name = message
					.content
					.get("name")
					.and_then(Value::as_str)
					.unwrap_or("stdout");
				let text = message
					.content
					.get("text")
					.and_then(Value::as_str)
					.unwrap_or_default();
				let event = if name == "stderr" {
					EvalEvent::Stderr { data: text.to_owned() }
				} else {
					EvalEvent::Stdout { data: text.to_owned() }
				};
				if events.send(event).await.is_err() {
					return Ok(IopubAction::ReceiverClosed);
				}
				Ok(IopubAction::Continue)
			},
			"display_data" | "execute_result" => {
				let Some(data) = message.content.get("data") else {
					return Ok(IopubAction::Continue);
				};
				if let Some(event) = pick_display_event(data)
					&& events.send(event).await.is_err()
				{
					return Ok(IopubAction::ReceiverClosed);
				}
				Ok(IopubAction::Continue)
			},
			"error" => {
				let event = EvalEvent::Error {
					ename:     message
						.content
						.get("ename")
						.and_then(Value::as_str)
						.unwrap_or("Error")
						.to_owned(),
					evalue:    message
						.content
						.get("evalue")
						.and_then(Value::as_str)
						.unwrap_or_default()
						.to_owned(),
					traceback: message
						.content
						.get("traceback")
						.and_then(Value::as_array)
						.map(|items| {
							items
								.iter()
								.filter_map(|item| item.as_str().map(ToOwned::to_owned))
								.collect()
						})
						.unwrap_or_default(),
				};
				if events.send(event).await.is_err() {
					return Ok(IopubAction::ReceiverClosed);
				}
				Ok(IopubAction::Continue)
			},
			"status" => {
				if message
					.content
					.get("execution_state")
					.and_then(Value::as_str)
					== Some("idle")
				{
					if events
						.send(EvalEvent::Status { state: KernelState::Idle })
						.await
						.is_err()
					{
						return Ok(IopubAction::ReceiverClosed);
					}
					return Ok(IopubAction::Idle);
				}
				Ok(IopubAction::Continue)
			},
			_ => Ok(IopubAction::Continue),
		}
	}

	#[allow(
		clippy::unused_self,
		reason = "Method signature required by trait; `self` reserved for future use"
	)]
	fn parent_matches(&self, message: &WireMessage, expected_parent_id: &str) -> bool {
		message.parent_header.get("msg_id").and_then(Value::as_str) == Some(expected_parent_id)
	}
}

/// Worker thread function that handles all zmq socket I/O
fn spawn_zmq_worker(
	ports: ConnectionFile,
	session_id: String,
	hmac_key: Vec<u8>,
	outbound_rx: mpsc::UnboundedReceiver<OutboundMessage>,
	inbound_tx: mpsc::UnboundedSender<InboundMessage>,
) -> JoinHandle<()> {
	std::thread::spawn(move || {
		let context = zmq::Context::new();
		let shell = connect_dealer(&context, ports.shell_port).expect("shell socket");
		let iopub = connect_iopub(&context, ports.iopub_port).expect("iopub socket");
		let control = connect_dealer(&context, ports.control_port).expect("control socket");

		let mut outbound_rx = outbound_rx;

		// Poll loop
		loop {
			// Drain outbound channel non-blocking
			while let Ok(msg) = outbound_rx.try_recv() {
				match msg {
					OutboundMessage::Shutdown => {
						let _ = inbound_tx.send(InboundMessage::Shutdown);
						return;
					},
					OutboundMessage::ShutdownRequest => {
						if let Err(e) = send_request(
							&control,
							&session_id,
							&hmac_key,
							"shutdown_request",
							json!({ "restart": false }),
						) {
							tracing::error!("failed to send shutdown request: {}", e);
						}
					},

					OutboundMessage::ExecuteRequest { msg_id: _, ref code } => {
						// For kernel info probe, send kernel_info_request instead
						if let Err(e) = send_request(
							&shell,
							&session_id,
							&hmac_key,
							if code.is_empty() {
								"kernel_info_request"
							} else {
								"execute_request"
							},
							if code.is_empty() {
								json!({})
							} else {
								json!({
									"code": code,
									"silent": false,
									"store_history": false,
									"allow_stdin": false,
									"stop_on_error": true,
								})
							},
						) {
							tracing::error!(
								"failed to send {}: {}",
								if code.is_empty() {
									"kernel_info_request"
								} else {
									"execute_request"
								},
								e
							);
						}
					},
				}
			}

			// Poll zmq sockets
			let mut poll_items = [shell.as_poll_item(zmq::POLLIN), iopub.as_poll_item(zmq::POLLIN)];
			let _ = zmq::poll(&mut poll_items, SOCKET_POLL_TIMEOUT_MS);

			// Handle iopub messages
			#[allow(
				clippy::result_large_err,
				reason = "SendError carries the InboundMessage we never re-handle here"
			)]
			if poll_items[1].is_readable() {
				let _ = recv_message(&iopub, &hmac_key)
					.ok()
					.flatten()
					.map(|msg| inbound_tx.send(InboundMessage::IopubEvent(msg)));
			}

			// Handle shell messages
			#[allow(
				clippy::result_large_err,
				reason = "SendError carries the InboundMessage we never re-handle here"
			)]
			if poll_items[0].is_readable() {
				let _ = recv_message(&shell, &hmac_key).ok().flatten().map(|msg| {
					inbound_tx.send(InboundMessage::ShellReply {
						msg_type: msg.header.msg_type,
						msg_id:   msg.header.msg_id,
						content:  msg.content,
					})
				});
			}
		}
	})
}

async fn wait_for_ready(
	inbound_rx: &Mutex<mpsc::UnboundedReceiver<InboundMessage>>,
) -> anyhow::Result<()> {
	let deadline = Instant::now() + KERNEL_READY_TIMEOUT;
	let mut saw_idle = false;
	let mut saw_reply = false;

	while Instant::now() < deadline {
		let mut inbound = inbound_rx.lock().await;
		match tokio::time::timeout(Duration::from_millis(100), inbound.recv()).await {
			Ok(Some(message)) => match message {
				InboundMessage::IopubEvent(msg)
					if msg.header.msg_type == "status"
						&& msg.content.get("execution_state").and_then(Value::as_str) == Some("idle") =>
				{
					saw_idle = true;
				},
				InboundMessage::ShellReply { msg_type, .. } if msg_type == "kernel_info_reply" => {
					saw_reply = true;
				},
				_ => {},
			},
			Ok(None) => bail!("worker channel closed during ready check"),
			Err(_) => {}, // timeout, continue
		}
		drop(inbound);

		if saw_idle && saw_reply {
			return Ok(());
		}
	}

	bail!("ipykernel did not become ready")
}

fn send_request(
	socket: &zmq::Socket,
	session_id: &str,
	hmac_key: &[u8],
	msg_type: &str,
	content: Value,
) -> anyhow::Result<String> {
	let header = JupyterHeader {
		msg_id:   Uuid::new_v4().to_string(),
		username: "rwp-server".to_owned(),
		session:  session_id.to_owned(),
		date:     Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
		msg_type: msg_type.to_owned(),
		version:  "5.3".to_owned(),
	};
	let parent_header = json!({});
	let metadata = json!({});
	let header_json = serde_json::to_vec(&header).context("serialize jupyter header")?;
	let parent_json =
		serde_json::to_vec(&parent_header).context("serialize jupyter parent header")?;
	let metadata_json = serde_json::to_vec(&metadata).context("serialize jupyter metadata")?;
	let content_json = serde_json::to_vec(&content).context("serialize jupyter content")?;
	let signature = sign(hmac_key, &header_json, &parent_json, &metadata_json, &content_json)?;
	let frames = vec![
		JUPYTER_MESSAGE_DELIMITER.to_vec(),
		signature.into_bytes(),
		header_json,
		parent_json,
		metadata_json,
		content_json,
	];
	socket
		.send_multipart(frames, 0)
		.context("send jupyter request")?;
	Ok(header.msg_id)
}

#[allow(clippy::result_large_err, reason = "WireMessage variant carries the parsed payload")]
fn recv_message(socket: &zmq::Socket, hmac_key: &[u8]) -> anyhow::Result<Option<WireMessage>> {
	let frames = match socket.recv_multipart(0) {
		Ok(frames) => frames,
		Err(zmq::Error::EAGAIN) => return Ok(None),
		Err(error) => return Err(anyhow!(error)).context("receive jupyter message"),
	};
	let delimiter = frames
		.iter()
		.position(|frame| frame.as_slice() == JUPYTER_MESSAGE_DELIMITER)
		.ok_or_else(|| anyhow!("jupyter message missing delimiter"))?;
	if frames.len() < delimiter + 6 {
		bail!("jupyter message too short: {} frames", frames.len());
	}
	let identities = frames[..delimiter].to_vec();
	let signature = hex::decode(&frames[delimiter + 1]).context("decode jupyter signature")?;
	let header_json = &frames[delimiter + 2];
	let parent_json = &frames[delimiter + 3];
	let metadata_json = &frames[delimiter + 4];
	let content_json = &frames[delimiter + 5];
	verify_signature(hmac_key, &signature, header_json, parent_json, metadata_json, content_json)?;
	let header: JupyterHeader =
		serde_json::from_slice(header_json).context("decode jupyter header")?;
	let parent_header =
		serde_json::from_slice(parent_json).context("decode jupyter parent header")?;
	let _: Value = serde_json::from_slice(metadata_json).context("decode jupyter metadata")?;
	let content = serde_json::from_slice(content_json).context("decode jupyter content")?;
	Ok(Some(WireMessage { _identities: identities, header, parent_header, content }))
}

fn sign(
	hmac_key: &[u8],
	header: &[u8],
	parent_header: &[u8],
	metadata: &[u8],
	content: &[u8],
) -> anyhow::Result<String> {
	let mut hmac = HmacSha256::new_from_slice(hmac_key).context("build hmac signer")?;
	hmac.update(header);
	hmac.update(parent_header);
	hmac.update(metadata);
	hmac.update(content);
	Ok(hex::encode(hmac.finalize().into_bytes()))
}

fn verify_signature(
	hmac_key: &[u8],
	signature: &[u8],
	header: &[u8],
	parent_header: &[u8],
	metadata: &[u8],
	content: &[u8],
) -> anyhow::Result<()> {
	let mut hmac = HmacSha256::new_from_slice(hmac_key).context("build hmac verifier")?;
	hmac.update(header);
	hmac.update(parent_header);
	hmac.update(metadata);
	hmac.update(content);
	hmac
		.verify_slice(signature)
		.map_err(|_| anyhow!("invalid jupyter message signature"))
}

fn pick_display_event(data: &Value) -> Option<EvalEvent> {
	let map = data.as_object()?;

	// Try PNG first
	// Try PNG first
	if let Some(base64) = map
		.get("image/png")
		.and_then(Value::as_str)
		.and_then(|s| Vec::<u8>::from_hex(s).ok())
	{
		return Some(EvalEvent::Display { mime: "image/png".to_owned(), data: hex::encode(base64) });
	}

	// Try SVG
	if let Some(svg) = map.get("image/svg+xml").and_then(Value::as_str) {
		return Some(EvalEvent::Display { mime: "image/svg+xml".to_owned(), data: svg.to_owned() });
	}

	// Try JSON
	if let Some(json) = map.get("application/json") {
		return Some(EvalEvent::Display {
			mime: "application/json".to_owned(),
			data: json.to_string(),
		});
	}

	// Try plain text
	if let Some(text) = map.get("text/plain").and_then(Value::as_str) {
		return Some(EvalEvent::Display { mime: "text/plain".to_owned(), data: text.to_owned() });
	}

	None
}

fn probe_ipykernel_program() -> Option<String> {
	for program in PYTHON_PROGRAMS {
		if std::process::Command::new(program)
			.args(["-c", "import ipykernel; print('ok')"])
			.output()
			.ok()
			.is_some_and(|o| o.status.success())
		{
			return Some(program.to_string());
		}
	}
	None
}

fn reserve_ports() -> anyhow::Result<[std::net::TcpListener; 5]> {
	let listeners = [
		std::net::TcpListener::bind("127.0.0.1:0").context("bind shell port")?,
		std::net::TcpListener::bind("127.0.0.1:0").context("bind iopub port")?,
		std::net::TcpListener::bind("127.0.0.1:0").context("bind stdin port")?,
		std::net::TcpListener::bind("127.0.0.1:0").context("bind control port")?,
		std::net::TcpListener::bind("127.0.0.1:0").context("bind hb port")?,
	];
	Ok(listeners)
}

fn write_connection_file(ports: [u16; 5], key: &str) -> anyhow::Result<PathBuf> {
	let connection = ConnectionFile {
		transport:        "tcp".to_owned(),
		ip:               "127.0.0.1".to_owned(),
		shell_port:       ports[0],
		iopub_port:       ports[1],
		stdin_port:       ports[2],
		control_port:     ports[3],
		hb_port:          ports[4],
		key:              key.to_owned(),
		signature_scheme: "hmac-sha256".to_owned(),
	};
	let path = std::env::temp_dir().join(format!("jupyter-{}.json", Uuid::new_v4()));
	std::fs::write(
		&path,
		serde_json::to_string_pretty(&connection).context("serialize connection file")?,
	)
	.context("write connection file")?;
	Ok(path)
}

fn connect_dealer(context: &zmq::Context, port: u16) -> anyhow::Result<zmq::Socket> {
	let socket = context
		.socket(zmq::DEALER)
		.context("create dealer socket")?;
	socket.set_linger(0).context("set linger")?;
	socket.set_rcvtimeo(100).context("set receive timeout")?;
	socket
		.connect(&format!("tcp://127.0.0.1:{port}"))
		.context("connect dealer socket")?;
	Ok(socket)
}

fn connect_iopub(context: &zmq::Context, port: u16) -> anyhow::Result<zmq::Socket> {
	let socket = context.socket(zmq::SUB).context("create sub socket")?;
	socket.set_linger(0).context("set linger")?;
	socket.set_rcvtimeo(100).context("set receive timeout")?;
	socket
		.connect(&format!("tcp://127.0.0.1:{port}"))
		.context("connect sub socket")?;
	socket
		.set_subscribe(b"")
		.context("subscribe to all iopub messages")?;
	Ok(socket)
}

fn cleanup_connection_file(path: &PathBuf) {
	let _ = std::fs::remove_file(path);
}

async fn drain_stderr(program: String, stderr: ChildStderr) {
	use tokio::io::{AsyncBufReadExt, BufReader};
	let reader = BufReader::new(stderr);
	let mut lines = reader.lines();
	while let Ok(Some(line)) = lines.next_line().await {
		if !line.is_empty() {
			tracing::warn!(target: "rwp_server::eval", program = %program, stderr = %line);
		}
	}
}
