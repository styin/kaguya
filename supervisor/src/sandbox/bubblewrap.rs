//! Bubblewrap backend — Linux namespace isolation without a Docker daemon.
//! `bwrap` supplies the mount/process boundary and unshares networking.
//!
//! Minimal usable bind list (see the debugging notes in the PR description):
//!   ro:    /usr /bin /sbin /lib /lib64 /etc   — interpreters, shared libs,
//!                                                CA certs, /etc/passwd
//!   proc:  /proc                              — some runtimes stat /proc
//!   dev:   /dev (minimal devtmpfs: null/zero/urandom/tty)
//!   tmpfs: /tmp                               — scratch, isolated per run
//!   bind:  <session dir> → /home/sandbox      — the only writable workspace,
//!                                                persistent across calls in a
//!                                                conversation
//! Namespaces: `--unshare-all` (including network) + `--die-with-parent`.
//! Env: cleared, then a minimal known-good set so HOME/TMPDIR are writable.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::Mutex;

use super::{
    configure_process_group, run_spawned, sanitize_session, script_name, ExecRequest, ExecResult,
    SandboxBackend,
};

pub struct BubblewrapBackend {
    root: PathBuf,
    sessions: Mutex<HashSet<String>>,
}

impl BubblewrapBackend {
    pub fn new(_workspace_root: PathBuf) -> anyhow::Result<Self> {
        let ok = std::process::Command::new("bwrap")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            anyhow::bail!(
                "bubblewrap backend selected but `bwrap` not found (apt install bubblewrap)"
            );
        }
        Ok(Self {
            root: std::env::temp_dir().join("kaguya-sandbox"),
            sessions: Mutex::new(HashSet::new()),
        })
    }
    fn session_dir(&self, session: &str) -> PathBuf {
        self.root.join(sanitize_session(session))
    }
}

#[async_trait]
impl SandboxBackend for BubblewrapBackend {
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

        let script = script_name(req.language.ext());
        if let Err(e) = tokio::fs::write(dir.join(&script), &req.code).await {
            return ExecResult::backend_error(format!("write script failed: {e}"));
        }

        let interp = req.language.unix_interp();
        let mut cmd = Command::new("bwrap");
        configure_process_group(&mut cmd);

        // Read-only system dirs that exist on this host.
        for d in ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"] {
            if Path::new(d).exists() {
                cmd.arg("--ro-bind").arg(d).arg(d);
            }
        }

        cmd.arg("--proc")
            .arg("/proc")
            .arg("--dev")
            .arg("/dev")
            .arg("--tmpfs")
            .arg("/tmp")
            // The only writable mount: this session's scratch dir.
            .arg("--bind")
            .arg(&dir)
            .arg("/home/sandbox")
            .arg("--chdir")
            .arg("/home/sandbox")
            // New namespaces (user/pid/ipc/uts/cgroup/net). No net ⇒ offline.
            .arg("--unshare-all")
            .arg("--die-with-parent")
            // Clean env, then a minimal known-good set. Without HOME pointing at
            // a writable dir, Python/Node fail trying to write caches; without
            // PATH, `bash`/subprocess lookups fail.
            .arg("--clearenv")
            .arg("--setenv")
            .arg("HOME")
            .arg("/home/sandbox")
            .arg("--setenv")
            .arg("PATH")
            .arg("/usr/local/bin:/usr/bin:/bin")
            .arg("--setenv")
            .arg("LANG")
            .arg("C.UTF-8")
            .arg("--setenv")
            .arg("TMPDIR")
            .arg("/tmp")
            .arg("--setenv")
            .arg("PYTHONUNBUFFERED")
            .arg("1")
            .arg("--")
            .arg(interp)
            .arg(format!("/home/sandbox/{script}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let result = match cmd.spawn() {
            Ok(child) => run_spawned(child, req.stdin, req.timeout, req.max_output_bytes).await,
            Err(e) => ExecResult::backend_error(format!("bwrap spawn: {e}")),
        };
        // Best-effort remove the transient runner script; user-created files stay.
        let _ = tokio::fs::remove_file(dir.join(&script)).await;
        result
    }

    async fn cleanup(&self, session: &str) {
        let _ = tokio::fs::remove_dir_all(self.session_dir(session)).await;
        self.sessions.lock().await.remove(session);
    }

    fn name(&self) -> &'static str {
        "bubblewrap"
    }
}
