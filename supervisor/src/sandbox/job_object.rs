//! Windows Job Object backend — memory/process limits and reliable
//! kill-on-close tree teardown. A Job Object is a resource boundary, not
//! filesystem or network isolation. Feature-gated behind `sandbox-jobobject`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::warn;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

use crate::config::SandboxConfig;

use super::{read_capped, sanitize_session, script_name, ExecRequest, ExecResult, SandboxBackend};

/// A `HANDLE` is `!Send` because it is a raw pointer, but a Job Object is a
/// process-wide kernel object identified by an opaque handle whose validity is
/// independent of the polling thread. Wrapping it keeps the `async_trait`
/// `execute` future `Send` while tokio migrates it across worker threads.
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

pub struct JobObjectBackend {
    root: PathBuf,
    sessions: Mutex<HashSet<String>>,
    mem_bytes: usize,
    pids: u32,
}

impl JobObjectBackend {
    pub fn new(cfg: &SandboxConfig, _workspace_root: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            root: std::env::temp_dir().join("kaguya-sandbox"),
            sessions: Mutex::new(HashSet::new()),
            // Honor the same [sandbox] limits as the Docker backend so config
            // behaves consistently across backends.
            mem_bytes: (cfg.memory_limit_mb as usize) * 1024 * 1024,
            pids: cfg.pids_limit as u32,
        })
    }
    fn session_dir(&self, session: &str) -> PathBuf {
        self.root.join(sanitize_session(session))
    }
}

#[async_trait]
impl SandboxBackend for JobObjectBackend {
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
            return ExecResult::backend_error(format!("mkdir: {e}"));
        }
        self.sessions.lock().await.insert(session.to_string());
        let script = dir.join(script_name(req.language.ext()));
        if let Err(e) = tokio::fs::write(&script, &req.code).await {
            return ExecResult::backend_error(format!("write: {e}"));
        }

        // Spawn interpreter.
        let mut child = {
            let mut spawned = None;
            for cand in req.language.native_candidates() {
                let mut cmd = Command::new(cand);
                cmd.arg(&script)
                    .current_dir(&dir)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                match cmd.spawn() {
                    Ok(c) => {
                        spawned = Some(c);
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return ExecResult::backend_error(format!("spawn {cand}: {e}")),
                }
            }
            match spawned {
                Some(c) => c,
                None => return ExecResult::backend_error("interpreter not found"),
            }
        };

        // Create job, set limits, assign process. (Small race before assign;
        // for stricter behavior spawn CREATE_SUSPENDED, assign, then resume.)
        let job = SendHandle(unsafe { CreateJobObjectW(None, None) }.unwrap_or_default());
        if !job.0.is_invalid() {
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_JOB_MEMORY
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            info.BasicLimitInformation.ActiveProcessLimit = self.pids;
            info.JobMemoryLimit = self.mem_bytes;
            unsafe {
                let _ = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of_val(&info) as u32,
                );
            }
            if let Some(raw) = child.raw_handle() {
                let h = HANDLE(raw as _);
                if let Err(e) = unsafe { AssignProcessToJobObject(job.0, h) } {
                    warn!("AssignProcessToJobObject: {e}");
                }
            }
        }

        if let Some(input) = req.stdin.as_deref() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.as_bytes()).await;
            }
        } else {
            let _ = child.stdin.take(); // EOF
        }
        let cap = req.max_output_bytes;
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
        let exit_code = match tokio::time::timeout(req.timeout, child.wait()).await {
            Ok(Ok(s)) => s.code().unwrap_or(-1),
            Ok(Err(e)) => {
                if !job.0.is_invalid() {
                    unsafe {
                        let _ = CloseHandle(job.0);
                    }
                }
                return ExecResult::backend_error(format!("wait: {e}"));
            }
            Err(_) => {
                timed_out = true;
                if !job.0.is_invalid() {
                    unsafe {
                        let _ = TerminateJobObject(job.0, 1);
                    }
                }
                let _ = child.wait().await;
                -1
            }
        };
        if !job.0.is_invalid() {
            // KILL_ON_JOB_CLOSE reaps any stragglers when the handle closes.
            unsafe {
                let _ = CloseHandle(job.0);
            }
        }

        let (out, ot) = out_task.await.unwrap_or_default();
        let (err, et) = err_task.await.unwrap_or_default();
        // Best-effort remove the transient runner script; user-created files stay.
        let _ = tokio::fs::remove_file(&script).await;
        ExecResult {
            stdout: out,
            stderr: err,
            exit_code,
            timed_out,
            truncated: ot || et,
            backend_error: None,
        }
    }

    async fn cleanup(&self, session: &str) {
        let _ = tokio::fs::remove_dir_all(self.session_dir(session)).await;
        self.sessions.lock().await.remove(session);
    }

    fn name(&self) -> &'static str {
        "job_object"
    }
}
