//! Docker backend — strongest isolation. Per-session container affinity so
//! files persist across calls in a conversation. `mode`:
//!   single_user → lazy: create on first use, keep for the session, destroy on
//!                 cleanup/shutdown. No warm pool.
//!   hosted      → warm pool of `pool_size` pre-created containers; pop on
//!                 bind, destroy + replenish on cleanup.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::config::{SandboxConfig, SandboxModeKind};

use super::{run_spawned, ExecRequest, ExecResult, SandboxBackend};

pub struct DockerBackend {
    image: String,
    mode: SandboxModeKind,
    pool_size: usize,
    mem_mb: u64,
    pids: u64,
    network: bool,
    state: Mutex<DockerState>,
}

#[derive(Default)]
struct DockerState {
    warm: Vec<String>,                  // idle container ids
    sessions: HashMap<String, String>,  // conversation_id → container id
}

impl DockerBackend {
    pub fn new(cfg: &SandboxConfig) -> anyhow::Result<Self> {
        let ok = std::process::Command::new("docker")
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            anyhow::bail!("docker backend selected but `docker` CLI / daemon unavailable");
        }
        Ok(Self {
            image: cfg.image.clone(),
            mode: cfg.mode,
            pool_size: cfg.pool_size,
            mem_mb: cfg.memory_limit_mb,
            pids: cfg.pids_limit,
            network: cfg.network,
            state: Mutex::new(DockerState::default()),
        })
    }

    async fn create_container(&self) -> Result<String, String> {
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--rm".into(),
            "-w".into(),
            "/home/sandbox".into(),
            "-u".into(),
            "1000:1000".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            format!("--memory={}m", self.mem_mb),
            format!("--memory-swap={}m", self.mem_mb), // == memory ⇒ swap disabled
            format!("--pids-limit={}", self.pids),
            "--cpus=1".into(),
        ];
        if !self.network {
            args.push("--network=none".into());
        }
        args.push(self.image.clone());
        args.push("sleep".into());
        args.push("infinity".into());

        let out = Command::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| format!("docker run: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "docker run: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if id.is_empty() {
            return Err("docker run returned empty id".into());
        }
        debug!("sandbox container up: {}", &id[..id.len().min(12)]);
        Ok(id)
    }

    async fn destroy(id: &str) {
        let _ = Command::new("docker").args(["rm", "-f", id]).output().await;
    }

    async fn container_for(&self, session: &str) -> Result<String, String> {
        {
            let st = self.state.lock().await;
            if let Some(id) = st.sessions.get(session) {
                return Ok(id.clone());
            }
        }
        // Acquire: pop warm (hosted) else create (lazy / pool exhausted).
        let pooled = {
            let mut st = self.state.lock().await;
            st.warm.pop()
        };
        let id = match pooled {
            Some(id) => id,
            None => self.create_container().await?,
        };
        let mut st = self.state.lock().await;
        if let Some(existing) = st.sessions.get(session) {
            // Lost a race; drop the extra container.
            let existing = existing.clone();
            drop(st);
            Self::destroy(&id).await;
            return Ok(existing);
        }
        st.sessions.insert(session.to_string(), id.clone());
        Ok(id)
    }
}

#[async_trait]
impl SandboxBackend for DockerBackend {
    async fn execute(&self, session: &str, req: ExecRequest) -> ExecResult {
        let id = match self.container_for(session).await {
            Ok(i) => i,
            Err(e) => return ExecResult::backend_error(e),
        };

        let ext = req.language.ext();
        let interp = req.language.unix_interp();
        let secs = req.timeout.as_secs().max(1);
        let runfile = format!("/home/sandbox/.kaguya_run.{ext}");
        // Code arrives on this exec's stdin; `cat` writes it to a file (no shell
        // escaping of user code), then in-container `timeout` runs it and
        // self-enforces the limit. Program stdin = EOF.
        let shell = format!("cat > {runfile} && exec timeout -k 2 {secs} {interp} {runfile}");

        let mut cmd = Command::new("docker");
        cmd.args(["exec", "-i", &id, "sh", "-c", &shell])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ExecResult::backend_error(format!("docker exec spawn: {e}")),
        };

        // Outer backstop in case docker itself hangs.
        let backstop = req.timeout + Duration::from_secs(5);
        let mut r = run_spawned(child, Some(req.code.clone()), backstop, req.max_output_bytes).await;
        if r.exit_code == 124 {
            // coreutils `timeout` ⇒ timed out
            r.timed_out = true;
        }
        r
    }

    async fn cleanup(&self, session: &str) {
        let id = {
            let mut st = self.state.lock().await;
            st.sessions.remove(session)
        };
        if let Some(id) = id {
            Self::destroy(&id).await;
            if self.mode == SandboxModeKind::Hosted {
                if let Ok(fresh) = self.create_container().await {
                    self.state.lock().await.warm.push(fresh);
                }
            }
        }
    }

    async fn prewarm(&self) {
        if self.mode != SandboxModeKind::Hosted {
            return; // single_user is lazy — nothing to prewarm
        }
        for _ in 0..self.pool_size {
            match self.create_container().await {
                Ok(id) => self.state.lock().await.warm.push(id),
                Err(e) => {
                    warn!("prewarm container failed: {e}");
                    break;
                }
            }
        }
        info!(
            "docker sandbox warm pool ready: {} containers",
            self.state.lock().await.warm.len()
        );
    }

    async fn shutdown(&self) {
        let (warm, sessions) = {
            let mut st = self.state.lock().await;
            (
                std::mem::take(&mut st.warm),
                std::mem::take(&mut st.sessions),
            )
        };
        for id in warm {
            Self::destroy(&id).await;
        }
        for (_, id) in sessions {
            Self::destroy(&id).await;
        }
    }

    fn name(&self) -> &'static str {
        "docker"
    }
}