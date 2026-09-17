//! Eval kernel v1 implementation.
//!
//! Python prefers a Jupyter v5 `ZeroMQ` transport backed by `ipykernel`; other
//! languages, and Python when `ipykernel` is unavailable, fall back to the
//! persistent stdio wrapper transport.

use std::fmt;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

use crate::protocol::requests::EvalTransport;
#[cfg(feature = "jupyter")]
mod jupyter;
mod stdio;

#[must_use]
pub const fn default_idle_timeout_ms() -> u64 {
	300_000
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EvalLanguage {
	Python,
	Javascript,
}

impl EvalLanguage {
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Python => "python",
			Self::Javascript => "javascript",
		}
	}

	pub fn parse(value: &str) -> Result<Self, anyhow::Error> {
		match value {
			"python" => Ok(Self::Python),
			"javascript" => Ok(Self::Javascript),
			other => Err(anyhow!("unsupported eval language: {other}")),
		}
	}
}

impl fmt::Display for EvalLanguage {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KernelState {
	Starting,
	Busy,
	Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvalExecRequest {
	pub code:          String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cwd:           Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout_ms:    Option<u64>,
	#[serde(default = "default_store_history")]
	pub store_history: bool,
}

const fn default_store_history() -> bool {
	true
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvalStatusResponse {
	pub name:            String,
	pub lang:            String,
	pub status:          KernelState,
	pub ref_count:       u32,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub transport:       Option<EvalTransport>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub idle_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EvalEvent {
	Stdout { data: String },
	Stderr { data: String },
	Display { mime: String, data: String },
	Result { text: String },
	Error { ename: String, evalue: String, traceback: Vec<String> },
	Status { state: KernelState },
}

pub struct EvalKernel {
	lang:     EvalLanguage,
	state:    RwLock<KernelState>,
	runtime:  Mutex<KernelRuntime>,
	shutdown: CancellationToken,
}

impl fmt::Debug for EvalKernel {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("EvalKernel")
			.field("lang", &self.lang)
			.finish_non_exhaustive()
	}
}

impl EvalKernel {
	pub async fn spawn(
		lang: EvalLanguage,
		transport: Option<EvalTransport>,
	) -> anyhow::Result<Self> {
		let runtime = KernelRuntime::spawn(lang, transport).await?;
		Ok(Self {
			lang,
			state: RwLock::new(KernelState::Idle),
			runtime: Mutex::new(runtime),
			shutdown: CancellationToken::new(),
		})
	}

	#[must_use]
	pub const fn lang(&self) -> EvalLanguage {
		self.lang
	}

	pub async fn state(&self) -> KernelState {
		*self.state.read().await
	}

	pub async fn execute(
		&self,
		request: &EvalExecRequest,
		events: mpsc::Sender<EvalEvent>,
		cancel: CancellationToken,
	) -> anyhow::Result<()> {
		if self.shutdown.is_cancelled() {
			return Err(anyhow!("eval kernel is shutting down"));
		}
		let shutdown = self.shutdown.clone();
		let mut runtime = self.runtime.lock().await;
		*self.state.write().await = KernelState::Busy;
		let outcome = runtime.execute(request, events, cancel, shutdown).await;
		*self.state.write().await = if self.shutdown.is_cancelled() {
			KernelState::Starting
		} else {
			KernelState::Idle
		};
		outcome
	}

	pub async fn shutdown(&self) -> anyhow::Result<()> {
		self.shutdown.cancel();
		let mut runtime = self.runtime.lock().await;
		*self.state.write().await = KernelState::Starting;
		runtime.shutdown().await
	}
}

enum KernelRuntime {
	Stdio(stdio::StdioKernelRuntime),
	#[cfg(feature = "jupyter")]
	Jupyter(jupyter::JupyterKernelRuntime),
}

impl KernelRuntime {
	async fn spawn(lang: EvalLanguage, transport: Option<EvalTransport>) -> anyhow::Result<Self> {
		let use_jupyter =
			matches!((lang, transport), (EvalLanguage::Python, Some(EvalTransport::Jupyter)));

		match lang {
			EvalLanguage::Python if use_jupyter => {
				#[cfg(feature = "jupyter")]
				{
					match jupyter::JupyterKernelRuntime::spawn().await {
						Ok(Some(runtime)) => Ok(Self::Jupyter(runtime)),
						Ok(None) => {
							tracing::warn!(target: "rwp_server::eval", "ipykernel unavailable; falling back to stdio python eval");
							Ok(Self::Stdio(stdio::StdioKernelRuntime::spawn(lang)?))
						},
						Err(e) => {
							tracing::warn!(target: "rwp_server::eval", %e, "ipykernel spawn failed; falling back to stdio python eval");
							Ok(Self::Stdio(stdio::StdioKernelRuntime::spawn(lang)?))
						},
					}
				}
				#[cfg(not(feature = "jupyter"))]
				{
					tracing::warn!(
						target: "rwp_server::eval",
						"jupyter eval transport disabled in this build; falling back to stdio python eval",
					);
					Ok(Self::Stdio(stdio::StdioKernelRuntime::spawn(lang)?))
				}
			},
			_ => Ok(Self::Stdio(stdio::StdioKernelRuntime::spawn(lang)?)),
		}
	}

	async fn execute(
		&mut self,
		request: &EvalExecRequest,
		events: mpsc::Sender<EvalEvent>,
		cancel: CancellationToken,
		shutdown: CancellationToken,
	) -> anyhow::Result<()> {
		match self {
			Self::Stdio(runtime) => runtime.execute(request, events, cancel, shutdown).await,
			#[cfg(feature = "jupyter")]
			Self::Jupyter(runtime) => runtime.execute(request, events, cancel, shutdown).await,
		}
	}

	async fn shutdown(&mut self) -> anyhow::Result<()> {
		match self {
			Self::Stdio(runtime) => runtime.shutdown().await,
			#[cfg(feature = "jupyter")]
			Self::Jupyter(runtime) => runtime.shutdown().await,
		}
	}
}
