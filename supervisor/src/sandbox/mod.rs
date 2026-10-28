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
#[cfg(windows)]
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::app::validate_session_ref;
use crate::config::{SandboxBackendKind, SandboxConfig, SandboxModeKind};
use crate::proto;
use crate::telemetry::{TelemetryCorrelation, TelemetryHub};

const SANDBOX_CLEANUP_GRACE: Duration = Duration::from_secs(2);

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
    /// Provision or attach the allocation before an opaque handle is returned.
    async fn acquire(&self, allocation: &str) -> Result<(), String>;
    /// Run code in the allocation's sandbox. Same `allocation` ⇒ shared filesystem
    /// state for that allocation only.
    async fn execute(&self, allocation: &str, req: ExecRequest) -> ExecResult;
    /// Tear down an allocation's resources on release.
    async fn cleanup(&self, allocation: &str);
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
                 (one container/VM per allocation). For shared multi-tenant hosts, use \
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

    pub async fn acquire(&self, allocation: &str) -> anyhow::Result<()> {
        if !self.enabled {
            anyhow::bail!("sandbox is disabled");
        }
        debug!(
            %allocation,
            backend = self.backend.name(),
            "acquiring sandbox backend allocation"
        );
        self.backend
            .acquire(allocation)
            .await
            .map_err(anyhow::Error::msg)
    }

    pub async fn cleanup(&self, allocation: &str) {
        if self.enabled {
            debug!(
                %allocation,
                backend = self.backend.name(),
                "cleaning up sandbox backend allocation"
            );
            self.backend.cleanup(allocation).await;
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
    pub async fn exec_from_json(&self, allocation: &str, args_json: &str) -> String {
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
        self.backend.execute(allocation, req).await.to_tool_json()
    }
}

/// Supervisor-facing handle registry around the backend manager.
///
/// Handles are deliberately opaque to the Gateway. This keeps backend/allocation
/// identity and cleanup authority inside the runtime owner.
#[derive(Clone)]
pub struct SandboxProvider {
    manager: Arc<SandboxManager>,
    handles: Arc<Mutex<HashMap<String, AllocationRecord>>>,
    telemetry: Option<TelemetryHub>,
}

#[derive(Clone)]
struct AllocationRecord {
    owner: proto::SessionRef,
    allocation_id: String,
}

impl SandboxProvider {
    pub fn new(manager: SandboxManager) -> Self {
        Self {
            manager: Arc::new(manager),
            handles: Arc::new(Mutex::new(HashMap::new())),
            telemetry: None,
        }
    }

    pub fn disabled() -> Self {
        Self::new(SandboxManager::disabled())
    }

    pub fn with_telemetry(mut self, telemetry: TelemetryHub) -> Self {
        self.telemetry = Some(telemetry);
        self
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

    pub async fn acquire(
        &self,
        owner: &proto::SessionRef,
        allocation_id: &str,
    ) -> anyhow::Result<String> {
        if !self.is_enabled() {
            anyhow::bail!("sandbox provider is disabled");
        }
        validate_session_ref(Some(owner))?;
        let allocation_uuid = Uuid::parse_str(allocation_id)
            .map_err(|_| anyhow::anyhow!("sandbox allocation_id must be a UUID"))?;
        if allocation_uuid.is_nil() || allocation_uuid.to_string() != allocation_id {
            anyhow::bail!("sandbox allocation_id must be a non-nil canonical UUID");
        }

        if let Some((handle, existing)) = self
            .handles
            .lock()
            .await
            .iter()
            .find(|(_, existing)| existing.allocation_id == allocation_id)
        {
            if existing.owner != *owner {
                anyhow::bail!("sandbox allocation belongs to a different owner");
            }
            debug!(%allocation_id, %handle, "reusing sandbox handle");
            return Ok(handle.clone());
        }

        self.manager.acquire(allocation_id).await?;

        let mut handles = self.handles.lock().await;
        if let Some((handle, existing)) = handles
            .iter()
            .find(|(_, existing)| existing.allocation_id == allocation_id)
        {
            if existing.owner != *owner {
                anyhow::bail!("sandbox allocation belongs to a different owner");
            }
            return Ok(handle.clone());
        }
        let handle = Uuid::new_v4().to_string();
        handles.insert(
            handle.clone(),
            AllocationRecord {
                owner: owner.clone(),
                allocation_id: allocation_id.to_string(),
            },
        );
        info!(
            %allocation_id,
            %handle,
            backend = self.backend_name(),
            "acquired sandbox handle"
        );
        Ok(handle)
    }

    pub async fn execute(
        &self,
        owner: &proto::SessionRef,
        handle: &str,
        request_id: &str,
        args_json: &str,
    ) -> String {
        let (instance_id, session_id) = match validate_session_ref(Some(owner)) {
            Ok(pair) => pair,
            Err(error) => return ExecResult::backend_error(error.to_string()).to_tool_json(),
        };
        if request_id.trim().is_empty() {
            return ExecResult::backend_error("sandbox request_id must not be empty")
                .to_tool_json();
        }
        let record = self.handles.lock().await.get(handle).cloned();
        match record {
            Some(record) if record.owner == *owner => {
                let allocation_id = record.allocation_id;
                debug!(
                    %allocation_id,
                    %handle,
                    backend = self.backend_name(),
                    "executing sandbox request"
                );
                let started = std::time::Instant::now();
                let content = self.manager.exec_from_json(&allocation_id, args_json).await;
                if let Some(telemetry) = &self.telemetry {
                    let result = serde_json::from_str::<serde_json::Value>(&content)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    telemetry
                        .emit_with_correlation(
                            "supervisor",
                            "sandbox.exec.completed",
                            TelemetryCorrelation {
                                instance_id: Some(instance_id.to_owned()),
                                session_id: Some(session_id.to_owned()),
                                allocation_id: Some(allocation_id.clone()),
                            },
                            None,
                            Some(request_id.to_string()),
                            serde_json::json!({
                                "instance_id": instance_id,
                                "session_id": session_id,
                                "allocation_id": allocation_id,
                                "handle": handle,
                                "backend": self.backend_name(),
                                "duration_ms": started.elapsed().as_millis() as u64,
                                "exit_code": result.get("exit_code").and_then(|v| v.as_i64()),
                                "timed_out": result.get("timed_out").and_then(|v| v.as_bool()).unwrap_or(false),
                                "truncated": result.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false),
                                "stdout_bytes": result.get("stdout").and_then(|v| v.as_str()).map(str::len).unwrap_or(0),
                                "stderr_bytes": result.get("stderr").and_then(|v| v.as_str()).map(str::len).unwrap_or(0),
                                "error": result.get("error").cloned().unwrap_or(serde_json::Value::Null),
                            }),
                        )
                        .await;
                }
                content
            }
            Some(_) => ExecResult::backend_error("sandbox handle owner mismatch").to_tool_json(),
            None => ExecResult::backend_error("unknown or released sandbox handle").to_tool_json(),
        }
    }

    pub async fn release(&self, owner: &proto::SessionRef, handle: &str) -> anyhow::Result<()> {
        validate_session_ref(Some(owner))?;
        let mut handles = self.handles.lock().await;
        let Some(record) = handles.get(handle) else {
            anyhow::bail!("unknown or released sandbox handle");
        };
        if record.owner != *owner {
            anyhow::bail!("sandbox handle owner mismatch");
        }
        let allocation_id = handles.remove(handle).unwrap().allocation_id;
        drop(handles);
        self.manager.cleanup(&allocation_id).await;
        info!(%allocation_id, %handle, "released sandbox handle");
        Ok(())
    }

    pub async fn cleanup_allocations(&self) {
        let allocations = {
            let mut handles = self.handles.lock().await;
            handles
                .drain()
                .map(|(_, record)| record.allocation_id)
                .collect::<Vec<_>>()
        };
        for allocation_id in allocations {
            self.manager.cleanup(&allocation_id).await;
        }
    }

    pub async fn shutdown(&self) {
        self.cleanup_allocations().await;
        self.manager.shutdown().await;
    }
}

// ──────────────────────────────────────────
// Shared execution primitives (used by native / bubblewrap / docker)
// ──────────────────────────────────────────

pub(crate) fn sanitize_allocation(s: &str) -> String {
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
/// `sandbox_exec` calls in the same allocation share a scratch dir, so a fixed
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
            let _ = tokio::time::timeout(SANDBOX_CLEANUP_GRACE, child.wait()).await;
            -1
        }
    };

    let (out, ot) = collect_reader_task(out_task).await;
    let (err, et) = collect_reader_task(err_task).await;
    ExecResult {
        stdout: out,
        stderr: err,
        exit_code,
        timed_out,
        truncated: ot || et,
        backend_error: None,
    }
}

async fn collect_reader_task(mut task: JoinHandle<(String, bool)>) -> (String, bool) {
    match tokio::time::timeout(SANDBOX_CLEANUP_GRACE, &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => (String::new(), false),
        Err(_) => {
            task.abort();
            (String::new(), true)
        }
    }
}

/// Place a child in a fresh Unix process group so timeout cleanup can signal
/// every descendant that inherited the group. No-op on Windows.
pub(crate) fn configure_process_group(_cmd: &mut Command) {
    #[cfg(unix)]
    {
        _cmd.process_group(0);
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
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let pgid = pid as libc::pid_t;
            let rc = unsafe { libc::kill(-pgid, libc::SIGKILL) };
            if rc == 0 {
                return;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return;
            }
            warn!(pid, %error, "failed to signal sandbox process group; falling back to child kill");
        }
    }
    let _ = child.start_kill();
}

pub use SandboxBackend as _SandboxBackend; // keep trait importable via module
