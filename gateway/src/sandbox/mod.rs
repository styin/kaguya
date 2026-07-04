//! Pluggable code-execution sandbox.
//!
//! From the dialog flow's perspective a sandbox is just a tool: the LLM emits
//! `[TOOL:sandbox_exec({...})]`, the gateway routes it here, and a JSON result
//! string flows back through the normal P3 ToolResult path.
//!
//! The execution mechanism is swappable behind `SandboxBackend`. Backend choice
//! and resource limits are config-driven (`[sandbox]` in gateway.toml).

#[cfg(unix)]
mod bubblewrap;
mod docker;
#[cfg(all(windows, feature = "sandbox-jobobject"))]
mod job_object;
mod native;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::{SandboxBackendKind, SandboxConfig, SandboxModeKind};
use crate::proto;

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
    /// Run code in the session's sandbox. Same `session` ⇒ shared filesystem
    /// state (per-conversation affinity).
    async fn execute(&self, session: &str, req: ExecRequest) -> ExecResult;
    /// Tear down a session's resources (called when a conversation ends).
    async fn cleanup(&self, session: &str);
    /// Pre-warm resources at startup (hosted Docker warm pool). Default no-op.
    async fn prewarm(&self) {}
    /// Global teardown at gateway shutdown. Default no-op.
    async fn shutdown(&self) {}
    fn name(&self) -> &'static str;
}

// ──────────────────────────────────────────
// Manager (the thing tools.rs / main.rs hold)
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

        // Fail-safe posture warnings for hosting. The native backend runs
        // LLM-authored code with NO isolation from the host, so combining it
        // with a multi-tenant deployment is dangerous unless each Gateway
        // process is itself confined (one container / VM per user session).
        if cfg.backend == SandboxBackendKind::Native && cfg.mode == SandboxModeKind::Hosted {
            warn!(
                "Sandbox: backend=native + mode=hosted runs model-authored code with NO \
                 host isolation. Only safe if each Gateway process is confined per user \
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

    /// A no-op manager used when sandbox is disabled or backend init fails.
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

    /// The tool advertised to the Talker. None when disabled (so the LLM is
    /// never told a tool it can't use exists).
    pub fn tool_definition(&self) -> Option<proto::ToolDefinition> {
        if !self.enabled {
            return None;
        }
        Some(proto::ToolDefinition {
            name: "sandbox_exec".into(),
            // Deliberately does not claim "isolated": the native backend runs on
            // the host. Isolation is a deployment property of the chosen backend.
            description: "Execute code in a sandboxed subprocess and return \
                          {stdout, stderr, exit_code}. Files written under the working \
                          directory persist across calls within the same conversation. \
                          Optional 'stdin' is fed to the program. Languages: python, \
                          node, bash."
                .into(),
            args_schema: r#"{"type":"object","properties":{"language":{"type":"string","enum":["python","node","bash"]},"code":{"type":"string"},"stdin":{"type":"string"}},"required":["language","code"]}"#
                .into(),
        })
    }

    /// Parse the LLM's `args_json`, run, and return the tool-result JSON string.
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
