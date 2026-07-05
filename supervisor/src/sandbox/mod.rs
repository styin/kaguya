//! Supervisor-owned pluggable code-execution sandbox provider.
//!
//! The Gateway's Tool Manager requests an opaque handle from the Supervisor,
//! then uses that handle for execution. Backend construction, prewarming,
//! resource ownership, and teardown never cross into the Gateway process.
//!
//! The execution mechanism is swappable behind `SandboxBackend`. Backend choice
//! and resource limits are config-driven (`[sandbox]` in
//! `config/kaguya.runtime.toml`).

#[cfg(unix)]
mod bubblewrap;
mod docker;
#[cfg(all(windows, feature = "sandbox-jobobject"))]
mod job_object;
mod native;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::{SandboxBackendKind, SandboxConfig, SandboxModeKind};

// ──────────────────────────────────────────
// Shared types
// ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Python,
    Node,
    Bash,
}

impl Language {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "python" | "python3" | "py" => Some(Self::Python),
            "node" | "javascript" | "js" => Some(Self::Node),
            "bash" | "sh" | "shell" => Some(Self::Bash),
            _ => None,
        }
    }
    pub fn ext(&self) -> &'static str {
        match self {
            Self::Python => "py",
            Self::Node => "js",
            Self::Bash => "sh",
        }
    }
    /// Interpreter inside Linux containers / bubblewrap.
    pub fn unix_interp(&self) -> &'static str {
        match self {
            Self::Python => "python3",
            Self::Node => "node",
            Self::Bash => "bash",
        }
    }
    /// Candidate host interpreters, tried in order (platform-aware).
    pub fn native_candidates(&self) -> &'static [&'static str] {
        match self {
            Self::Python => {
                #[cfg(windows)]
                {
                    &["python", "python3"]
                }
                #[cfg(not(windows))]
                {
                    &["python3", "python"]
                }
            }
            Self::Node => &["node"],
            Self::Bash => &["bash", "sh"],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub language: Language,
    pub code: String,
    pub stdin: Option<String>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub truncated: bool,
    pub backend_error: Option<String>,
}

impl ExecResult {
    pub fn backend_error(msg: impl Into<String>) -> Self {
        Self {
            exit_code: -1,
            backend_error: Some(msg.into()),
            ..Default::default()
        }
    }
    /// JSON handed back to the LLM as the tool result content.
    pub fn to_tool_json(&self) -> String {
        serde_json::json!({
            "stdout": self.stdout,
            "stderr": self.stderr,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "truncated": self.truncated,
            "error": self.backend_error,
        })
        .to_string()
    }
}

// ──────────────────────────────────────────
// The pluggable contract
// ──────────────────────────────────────────

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Provision or attach the session before an opaque handle is returned.
    async fn acquire(&self, session: &str) -> Result<(), String>;
    /// Run code in the session's sandbox. Same `session` ⇒ shared filesystem
    /// state (per-conversation affinity).
    async fn execute(&self, session: &str, req: ExecRequest) -> ExecResult;
    /// Tear down a session's resources (called when a conversation ends).
    async fn cleanup(&self, session: &str);
    /// Pre-warm resources at startup (hosted Docker warm pool). Default no-op.
    async fn prewarm(&self) {}
    /// Global teardown at Supervisor shutdown. Default no-op.
    async fn shutdown(&self) {}
    fn name(&self) -> &'static str;
}

// ──────────────────────────────────────────
// Backend manager (owned only by the Supervisor)
// ──────────────────────────────────────────

pub struct SandboxManager {
    backend: Box<dyn SandboxBackend>,
    enabled: bool,
    default_timeout: Duration,
    max_output_bytes: usize,
    allowed: Vec<Language>,
}

impl SandboxManager {
    pub fn from_config(cfg: &SandboxConfig, workspace_root: PathBuf) -> anyhow::Result<Self> {
        let allowed: Vec<Language> = cfg
            .allowed_languages
            .iter()
            .filter_map(|s| Language::parse(s))
            .collect();

        let backend: Box<dyn SandboxBackend> = match cfg.backend {
            SandboxBackendKind::Native => Box::new(native::NativeBackend::new(workspace_root)),
            SandboxBackendKind::Docker => Box::new(docker::DockerBackend::new(cfg)?),
            SandboxBackendKind::Bubblewrap => {
                #[cfg(unix)]
                {
                    Box::new(bubblewrap::BubblewrapBackend::new(workspace_root)?)
                        as Box<dyn SandboxBackend>
                }
                #[cfg(not(unix))]
                {
                    anyhow::bail!("bubblewrap backend requires a unix host");
                }
            }
            SandboxBackendKind::JobObject => {
                #[cfg(all(windows, feature = "sandbox-jobobject"))]
                {
                    Box::new(job_object::JobObjectBackend::new(cfg, workspace_root)?)
                        as Box<dyn SandboxBackend>
                }
                #[cfg(not(all(windows, feature = "sandbox-jobobject")))]
                {
                    let _ = workspace_root;
                    anyhow::bail!(
                        "job_object backend requires Windows + `--features sandbox-jobobject`"
                    );
                }
            }
        };

        // Surface unsafe hosting combinations without silently changing the
        // configured backend. Native executes model-authored code on the host,
        // so a multi-tenant deployment must isolate each Supervisor externally.
        if cfg.backend == SandboxBackendKind::Native && cfg.mode == SandboxModeKind::Hosted {
            warn!(
                "Sandbox: backend=native + mode=hosted runs model-authored code with NO \
                 host isolation. Only safe if each Supervisor is confined per tenant \
                 (one container/VM per session). For shared multi-tenant hosts, use \
                 backend=docker."
            );
        }
        // A non-empty allow-list that parsed to nothing is almost certainly a
        // typo; the empty list otherwise means "allow all" (see exec_from_json).
        if allowed.is_empty() && !cfg.allowed_languages.is_empty() {
            warn!(
                "Sandbox: allowed_languages={:?} parsed to zero known languages; \
                 ALL languages will be permitted. Valid values: python, node, bash.",
                cfg.allowed_languages
            );
        }

        info!(
            "Sandbox: backend={} mode={:?} enabled={}",
            backend.name(),
            cfg.mode,
            cfg.enabled
        );

        Ok(Self {
            backend,
            enabled: cfg.enabled,
            default_timeout: Duration::from_secs(cfg.default_timeout_secs.max(1)),
            max_output_bytes: cfg.max_output_bytes,
            allowed,
        })
    }

    /// Disabled facade used when configuration turns the provider off or
    /// backend initialization fails. Backend methods remain gated by `enabled`.
    pub fn disabled() -> Self {
        Self {
            backend: Box::new(native::NativeBackend::new(std::env::temp_dir())),
            enabled: false,
            default_timeout: Duration::from_secs(30),
            max_output_bytes: 16 * 1024,
            allowed: vec![],
        }
    }

    pub async fn prewarm(&self) {
        if self.enabled {
            self.backend.prewarm().await;
        }
    }

    pub async fn acquire(&self, session: &str) -> anyhow::Result<()> {
        if !self.enabled {
            anyhow::bail!("sandbox is disabled");
        }
        self.backend
            .acquire(session)
            .await
            .map_err(anyhow::Error::msg)
    }

    pub async fn cleanup(&self, session: &str) {
        if self.enabled {
            self.backend.cleanup(session).await;
        }
    }

    pub async fn shutdown(&self) {
        if self.enabled {
            self.backend.shutdown().await;
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Parse Tool Manager arguments, execute through the selected backend, and
    /// return the canonical JSON payload placed on the P3 result path.
    pub async fn exec_from_json(&self, session: &str, args_json: &str) -> String {
        if !self.enabled {
            return ExecResult::backend_error("sandbox is disabled").to_tool_json();
        }
        let v: serde_json::Value = match serde_json::from_str(args_json) {
            Ok(v) => v,
            Err(e) => {
                return ExecResult::backend_error(format!("bad args JSON: {e}")).to_tool_json()
            }
        };
        let lang_s = v
            .get("language")
            .and_then(|x| x.as_str())
            .unwrap_or("python");
        let language = match Language::parse(lang_s) {
            Some(l) => l,
            None => {
                return ExecResult::backend_error(format!("unsupported language: {lang_s}"))
                    .to_tool_json()
            }
        };
        if !self.allowed.is_empty() && !self.allowed.contains(&language) {
            return ExecResult::backend_error(format!("language not allowed: {lang_s}"))
                .to_tool_json();
        }
        let code = match v.get("code").and_then(|x| x.as_str()) {
            Some(c) => c.to_string(),
            None => return ExecResult::backend_error("missing 'code'").to_tool_json(),
        };
        let stdin = v.get("stdin").and_then(|x| x.as_str()).map(str::to_string);

        let req = ExecRequest {
            language,
            code,
            stdin,
            timeout: self.default_timeout,
            max_output_bytes: self.max_output_bytes,
        };
        self.backend.execute(session, req).await.to_tool_json()
    }
}

/// Supervisor-facing handle registry around the backend manager.
///
/// Handles are deliberately opaque to the Gateway. This keeps backend/session
/// identity and cleanup authority inside the runtime owner.
#[derive(Clone)]
pub struct SandboxProvider {
    manager: Arc<SandboxManager>,
    handles: Arc<Mutex<HashMap<String, String>>>,
}

impl SandboxProvider {
    pub fn new(manager: SandboxManager) -> Self {
        Self {
            manager: Arc::new(manager),
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn disabled() -> Self {
        Self::new(SandboxManager::disabled())
    }

    pub fn is_enabled(&self) -> bool {
        self.manager.is_enabled()
    }

    pub fn backend_name(&self) -> &'static str {
        self.manager.backend_name()
    }

    pub async fn prewarm(&self) {
        self.manager.prewarm().await;
    }

    pub async fn acquire(&self, session: &str) -> anyhow::Result<String> {
        if !self.is_enabled() {
            anyhow::bail!("sandbox provider is disabled");
        }
        if session.trim().is_empty() {
            anyhow::bail!("sandbox session id must not be empty");
        }
        let mut handles = self.handles.lock().await;
        if let Some((handle, _)) = handles.iter().find(|(_, existing)| *existing == session) {
            return Ok(handle.clone());
        }
        self.manager.acquire(session).await?;
        let handle = Uuid::new_v4().to_string();
        handles.insert(handle.clone(), session.to_string());
        Ok(handle)
    }

    pub async fn execute(&self, handle: &str, args_json: &str) -> String {
        let session = self.handles.lock().await.get(handle).cloned();
        match session {
            Some(session) => self.manager.exec_from_json(&session, args_json).await,
            None => ExecResult::backend_error("unknown or released sandbox handle").to_tool_json(),
        }
    }

    pub async fn release(&self, handle: &str) -> anyhow::Result<()> {
        let session = self.handles.lock().await.remove(handle);
        let Some(session) = session else {
            anyhow::bail!("unknown or released sandbox handle");
        };
        self.manager.cleanup(&session).await;
        Ok(())
    }

    pub async fn cleanup_sessions(&self) {
        let sessions = {
            let mut handles = self.handles.lock().await;
            handles
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>()
        };
        for session in sessions {
            self.manager.cleanup(&session).await;
        }
    }

    pub async fn shutdown(&self) {
        self.cleanup_sessions().await;
        self.manager.shutdown().await;
    }
}

// ──────────────────────────────────────────
// Shared execution primitives (used by native / bubblewrap / docker)
// ──────────────────────────────────────────

pub(crate) fn sanitize_session(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Unique hidden runner-script filename for a single execution. Concurrent
/// `sandbox_exec` calls in the same session share a scratch dir, so a fixed
/// name would race; only this transient script is per-exec — files the user's
/// code creates still persist across calls.
pub(crate) fn script_name(ext: &str) -> String {
    format!(".kaguya-run-{}.{}", Uuid::new_v4(), ext)
}

/// Read a pipe to EOF, retaining at most `cap` bytes. Keeps draining past the
/// cap so the child never blocks on a full pipe.
pub(crate) async fn read_capped<R: AsyncReadExt + Unpin>(r: &mut R, cap: usize) -> (String, bool) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match r.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < cap {
                    let take = (cap - buf.len()).min(n);
                    buf.extend_from_slice(&chunk[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

/// Drive an already-spawned child: feed `program_stdin` (or close to EOF),
/// concurrently capture stdout/stderr, enforce `timeout`, tree-kill on expiry.
pub(crate) async fn run_spawned(
    mut child: Child,
    program_stdin: Option<String>,
    timeout: Duration,
    cap: usize,
) -> ExecResult {
    if let Some(input) = program_stdin {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(input.as_bytes()).await;
        } // dropped → EOF
    } else {
        let _ = child.stdin.take(); // close so stdin-reading programs get EOF
    }

    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let out_task = tokio::spawn(async move {
        match so {
            Some(ref mut s) => read_capped(s, cap).await,
            None => (String::new(), false),
        }
    });
    let err_task = tokio::spawn(async move {
        match se {
            Some(ref mut s) => read_capped(s, cap).await,
            None => (String::new(), false),
        }
    });

    let mut timed_out = false;
    let exit_code = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(e)) => return ExecResult::backend_error(format!("wait failed: {e}")),
        Err(_) => {
            timed_out = true;
            kill_tree(&mut child).await;
            let _ = child.wait().await;
            -1
        }
    };

    let (out, ot) = out_task.await.unwrap_or_default();
    let (err, et) = err_task.await.unwrap_or_default();
    ExecResult {
        stdout: out,
        stderr: err,
        exit_code,
        timed_out,
        truncated: ot || et,
        backend_error: None,
    }
}

/// Kill the child and (best-effort) its descendants.
pub(crate) async fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            // taskkill /T walks the parent-child tree by PID — no process
            // group needed, dependency-free.
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            return;
        }
    }
    let _ = child.start_kill();
}

pub use SandboxBackend as _SandboxBackend; // keep trait importable via module
