use std::{process::Stdio, time::Duration};

use anyhow::{Context, anyhow};
use serde::Serialize;
use tokio::{
	io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
	process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
	sync::mpsc,
	task::JoinHandle,
	time::{Instant, sleep_until, timeout},
};
use tokio_util::sync::CancellationToken;

use super::{EvalEvent, EvalExecRequest, EvalLanguage, KernelState};

const PYTHON_WRAPPER: &str = r#"
import ast
import contextlib
import io
import json
import sys
import traceback

ns = {"__name__": "__main__"}

for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    req = json.loads(line)
    stdout_buf = io.StringIO()
    stderr_buf = io.StringIO()
    result = None
    try:
        module = ast.parse(req.get("code", ""), filename="<eval>", mode="exec")
        last_expr = None
        if module.body and isinstance(module.body[-1], ast.Expr):
            last_expr = ast.Expression(module.body.pop().value)
        exec_code = compile(ast.fix_missing_locations(module), "<eval>", "exec")
        with contextlib.redirect_stdout(stdout_buf), contextlib.redirect_stderr(stderr_buf):
            exec(exec_code, ns)
            if last_expr is not None:
                expr_code = compile(ast.fix_missing_locations(last_expr), "<eval>", "eval")
                result = eval(expr_code, ns)
    except Exception as exc:
        out = stdout_buf.getvalue()
        err = stderr_buf.getvalue()
        if out:
            print(json.dumps({"type": "stdout", "data": out}), flush=True)
        if err:
            print(json.dumps({"type": "stderr", "data": err}), flush=True)
        print(json.dumps({
            "type": "error",
            "ename": exc.__class__.__name__,
            "evalue": str(exc),
            "traceback": traceback.format_exception(type(exc), exc, exc.__traceback__),
        }), flush=True)
        print(json.dumps({"type": "status", "state": "idle"}), flush=True)
        continue

    out = stdout_buf.getvalue()
    err = stderr_buf.getvalue()
    if out:
        print(json.dumps({"type": "stdout", "data": out}), flush=True)
    if err:
        print(json.dumps({"type": "stderr", "data": err}), flush=True)
    if result is not None:
        print(json.dumps({"type": "result", "text": repr(result)}), flush=True)
    print(json.dumps({"type": "status", "state": "idle"}), flush=True)
"#;

const JAVASCRIPT_WRAPPER: &str = r"
const readline = require('node:readline');
const vm = require('node:vm');
const util = require('node:util');

const context = vm.createContext({
	console,
	setTimeout,
	clearTimeout,
	setInterval,
	clearInterval,
	Buffer,
	process,
	require,
	globalThis: null,
});
context.global = context;
context.globalThis = context;

const rl = readline.createInterface({
	input: process.stdin,
	crlfDelay: Infinity,
	terminal: false,
});

for await (const line of rl) {
	if (!line.trim()) {
		continue;
	}
	const req = JSON.parse(line);
	try {
		let result = vm.runInContext(req.code ?? '', context, { timeout: req.timeout_ms ?? undefined });
		if (result && typeof result.then === 'function') {
			result = await result;
		}
		if (result !== undefined) {
			process.stdout.write(JSON.stringify({ type: 'result', text: util.inspect(result) }) + '\n');
		}
	} catch (error) {
		process.stdout.write(JSON.stringify({
			type: 'error',
			ename: error?.name ?? 'Error',
			evalue: error?.message ?? String(error),
			traceback: error?.stack ? String(error.stack).split('\n') : [String(error)],
		}) + '\n');
	}
	process.stdout.write(JSON.stringify({ type: 'status', state: 'idle' }) + '\n');
}
";

#[derive(Debug, Serialize)]
struct WrapperExecRequest<'a> {
	code:          &'a str,
	store_history: bool,
}

pub(super) struct StdioKernelRuntime {
	lang:        EvalLanguage,
	child:       Child,
	stdin:       ChildStdin,
	stdout:      Lines<BufReader<ChildStdout>>,
	stderr_task: JoinHandle<()>,
}

impl StdioKernelRuntime {
	pub(super) fn spawn(lang: EvalLanguage) -> anyhow::Result<Self> {
		let (programs, args) = match lang {
			EvalLanguage::Python => (&["python3", "python"][..], vec!["-u", "-c", PYTHON_WRAPPER]),
			EvalLanguage::Javascript => (&["node"][..], vec!["-e", JAVASCRIPT_WRAPPER]),
		};
		let mut last_not_found = None;
		for program in programs {
			match Self::spawn_program(lang, program, &args) {
				Ok(runtime) => return Ok(runtime),
				Err(error)
					if error
						.downcast_ref::<std::io::Error>()
						.is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
				{
					last_not_found = Some(error);
				},
				Err(error) => return Err(error),
			}
		}
		Err(last_not_found.unwrap_or_else(|| anyhow!("no interpreter available for {lang}")))
	}

	pub(super) async fn execute(
		&mut self,
		request: &EvalExecRequest,
		events: mpsc::Sender<EvalEvent>,
		cancel: CancellationToken,
		shutdown: CancellationToken,
	) -> anyhow::Result<()> {
		let payload = serde_json::to_string(&WrapperExecRequest {
			code:          request.code.as_str(),
			store_history: request.store_history,
		})
		.context("serialize eval request")?;
		self.stdin.write_all(payload.as_bytes()).await?;
		self.stdin.write_all(b"\n").await?;
		self.stdin.flush().await?;
		let deadline = request
			.timeout_ms
			.map(|ms| Instant::now() + Duration::from_millis(ms));

		loop {
			let next_line = self.stdout.next_line();
			let line = if let Some(deadline) = deadline {
				tokio::select! {
					() = shutdown.cancelled() => return Ok(()),
					() = cancel.cancelled() => {
						self.restart().await?;
						return Ok(());
					}
					() = sleep_until(deadline) => {
						let _ = events.send(EvalEvent::Error {
							ename: "TimeoutError".to_owned(),
							evalue: format!("execution exceeded {} ms", request.timeout_ms.unwrap_or_default()),
							traceback: vec!["execution cancelled after timeout".to_owned()],
						}).await;
						let _ = events.send(EvalEvent::Status { state: KernelState::Idle }).await;
						self.restart().await?;
						return Ok(());
					}
					result = next_line => result?,
				}
			} else {
				tokio::select! {
					() = shutdown.cancelled() => return Ok(()),
					() = cancel.cancelled() => {
						self.restart().await?;
						return Ok(());
					}
					result = next_line => result?,
				}
			};

			let Some(line) = line else {
				self.restart().await?;
				return Err(anyhow!("eval kernel exited before reporting idle status"));
			};
			let event: EvalEvent =
				serde_json::from_str(&line).with_context(|| format!("decode eval event: {line}"))?;
			let is_idle = matches!(event, EvalEvent::Status { state: KernelState::Idle });
			if events.send(event).await.is_err() {
				self.restart().await?;
				return Ok(());
			}
			if is_idle {
				return Ok(());
			}
		}
	}

	pub(super) async fn shutdown(&mut self) -> anyhow::Result<()> {
		let _ = self.stdin.shutdown().await;
		if let Ok(wait_result) = timeout(Duration::from_secs(1), self.child.wait()).await {
			wait_result?;
		} else {
			let _ = self.child.kill().await;
			let _ = self.child.wait().await;
		}
		self.stderr_task.abort();
		Ok(())
	}

	async fn restart(&mut self) -> anyhow::Result<()> {
		self.shutdown().await?;
		*self = Self::spawn(self.lang)?;
		Ok(())
	}

	fn spawn_program(lang: EvalLanguage, program: &str, args: &[&str]) -> anyhow::Result<Self> {
		let mut child = Command::new(program)
			.args(args)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.kill_on_drop(true)
			.spawn()
			.with_context(|| format!("spawn eval interpreter {program}"))?;
		let stdin = child
			.stdin
			.take()
			.ok_or_else(|| anyhow!("missing child stdin"))?;
		let stdout = child
			.stdout
			.take()
			.ok_or_else(|| anyhow!("missing child stdout"))?;
		let stderr = child
			.stderr
			.take()
			.ok_or_else(|| anyhow!("missing child stderr"))?;
		let stderr_task = tokio::spawn(drain_stderr(program.to_owned(), stderr));
		Ok(Self { lang, child, stdin, stdout: BufReader::new(stdout).lines(), stderr_task })
	}
}

async fn drain_stderr(program: String, stderr: ChildStderr) {
	let mut lines = BufReader::new(stderr).lines();
	loop {
		match lines.next_line().await {
			Ok(Some(line)) => tracing::warn!(target: "rwp_server::eval", %program, stderr = %line),
			Ok(None) => return,
			Err(error) => {
				tracing::warn!(target: "rwp_server::eval", %program, %error, "stderr drain failed");
				return;
			},
		}
	}
}
