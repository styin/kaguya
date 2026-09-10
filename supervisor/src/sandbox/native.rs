//! Native backend — runs host interpreters in a per-session scratch directory.
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
    configure_process_group, run_spawned, sanitize_session, script_name, ExecRequest, ExecResult,
    SandboxBackend,
};

pub struct NativeBackend {
    root: PathBuf,
    sessions: Mutex<HashSet<String>>,
}

impl NativeBackend {
    pub fn new(_workspace_root: PathBuf) -> Self {
        Self {
            root: std::env::temp_dir().join("kaguya-sandbox"),
            sessions: Mutex::new(HashSet::new()),
        }
    }
    fn session_dir(&self, session: &str) -> PathBuf {
        self.root.join(sanitize_session(session))
    }
}

#[async_trait]
impl SandboxBackend for NativeBackend {
    async fn acquire(&self, session: &str) -> Result<(), String> {
        tokio::fs::create_dir_all(self.session_dir(session))
            .await
            .map_err(|error| format!("mkdir failed: {error}"))?;
        self.sessions.lock().await.insert(session.to_string());
        Ok(())
    }

    async fn execute(&self, session: &str, req: ExecRequest) -> ExecResult {
        let dir = self.session_dir(session);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            return ExecResult::backend_error(format!("mkdir failed: {e}"));
        }
        self.sessions.lock().await.insert(session.to_string());

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

    async fn cleanup(&self, session: &str) {
        let _ = tokio::fs::remove_dir_all(self.session_dir(session)).await;
        self.sessions.lock().await.remove(session);
    }

    fn name(&self) -> &'static str {
        "native"
    }
}
