//! Native backend — runs host interpreters in a per-allocation scratch directory.
//! It provides no host isolation: cwd, timeout, output caps, and tree-kill are
//! operational controls, not a security boundary. Intended only for trusted
//! self-hosted use or when Supervisor itself is externally isolated.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::Mutex;

use super::{
    configure_process_group, run_spawned, sanitize_allocation, script_name, ExecRequest,
    ExecResult, SandboxBackend,
};

pub struct NativeBackend {
    root: PathBuf,
    allocations: Mutex<HashSet<String>>,
}

impl NativeBackend {
    pub fn new(_workspace_root: PathBuf) -> Self {
        Self {
            root: std::env::temp_dir().join("kaguya-sandbox"),
            allocations: Mutex::new(HashSet::new()),
        }
    }
    fn session_dir(&self, allocation: &str) -> PathBuf {
        self.root.join(sanitize_allocation(allocation))
    }
}

#[async_trait]
impl SandboxBackend for NativeBackend {
    async fn acquire(&self, allocation: &str) -> Result<(), String> {
        tokio::fs::create_dir_all(self.session_dir(allocation))
            .await
            .map_err(|error| format!("mkdir failed: {error}"))?;
        self.allocations.lock().await.insert(allocation.to_string());
        Ok(())
    }

    async fn execute(&self, allocation: &str, req: ExecRequest) -> ExecResult {
        let dir = self.session_dir(allocation);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            return ExecResult::backend_error(format!("mkdir failed: {e}"));
        }
        self.allocations.lock().await.insert(allocation.to_string());

        let script = dir.join(script_name(req.language.ext()));
        if let Err(e) = tokio::fs::write(&script, &req.code).await {
            return ExecResult::backend_error(format!("write script failed: {e}"));
        }

        let mut last = String::new();
        let mut result = None;
        for cand in req.language.native_candidates() {
            let mut cmd = Command::new(cand);
            configure_process_group(&mut cmd);
            cmd.arg(&script)
                .current_dir(&dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            match cmd.spawn() {
                Ok(child) => {
                    result = Some(
                        run_spawned(child, req.stdin, req.timeout, req.max_output_bytes).await,
                    );
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    last = format!("{cand} not found");
                    continue;
                }
                Err(e) => {
                    result = Some(ExecResult::backend_error(format!(
                        "spawn {cand} failed: {e}"
                    )));
                    break;
                }
            }
        }
        // Best-effort remove the transient runner script; user-created files stay.
        let _ = tokio::fs::remove_file(&script).await;
        result.unwrap_or_else(|| {
            ExecResult::backend_error(format!("no interpreter for {:?} ({last})", req.language))
        })
    }

    async fn cleanup(&self, allocation: &str) {
        let _ = tokio::fs::remove_dir_all(self.session_dir(allocation)).await;
        self.allocations.lock().await.remove(allocation);
    }

    fn name(&self) -> &'static str {
        "native"
    }
}
