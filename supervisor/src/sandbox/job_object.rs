//! Windows Job Object backend — resource limits and kill-on-close teardown.
//! Interpreters start suspended and resume only after assignment to the job.
//! A Job Object provides no filesystem or network isolation.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::{Child, Command};
use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

use super::{
    run_spawned, sanitize_allocation, script_name, ExecRequest, ExecResult, SandboxBackend,
    SANDBOX_CLEANUP_GRACE,
};
use crate::config::SandboxConfig;

pub struct JobObjectBackend {
    root: PathBuf,
    mem_bytes: usize,
    pids: u32,
}

impl JobObjectBackend {
    pub fn new(cfg: &SandboxConfig) -> anyhow::Result<Self> {
        let mem_bytes = usize::try_from(cfg.memory_limit_mb)?
            .checked_mul(1024 * 1024)
            .ok_or_else(|| anyhow::anyhow!("Job Object memory limit overflows host size"))?;
        let pids = u32::try_from(cfg.pids_limit)?;
        if mem_bytes == 0 || pids == 0 {
            anyhow::bail!("Job Object memory and process limits must be positive");
        }
        Ok(Self {
            root: std::env::temp_dir().join("kaguya-sandbox"),
            mem_bytes,
            pids,
        })
    }

    fn allocation_dir(&self, allocation: &str) -> PathBuf {
        self.root.join(sanitize_allocation(allocation))
    }

    fn create_job(&self) -> anyhow::Result<OwnedHandle> {
        let job = unsafe { CreateJobObjectW(None, None)? };
        // CreateJobObjectW returned a valid owned handle; every exit closes it.
        let job = unsafe { OwnedHandle::from_raw_handle(job.0) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.BasicLimitInformation.ActiveProcessLimit = self.pids;
        info.JobMemoryLimit = self.mem_bytes;
        unsafe {
            SetInformationJobObject(
                HANDLE(job.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&info) as u32,
            )?;
        }
        Ok(job)
    }
}

/// Stable Rust does not expose the primary thread handle. A newly suspended
/// process has one primary thread; require that unambiguous state before resume.
fn assign_and_resume(child: &Child, job: &OwnedHandle) -> anyhow::Result<()> {
    let raw = child
        .raw_handle()
        .ok_or_else(|| anyhow::anyhow!("child handle unavailable"))?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("child PID unavailable"))?;
    unsafe {
        AssignProcessToJobObject(HANDLE(job.as_raw_handle()), HANDLE(raw))?;
    }
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)? };
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    unsafe {
        Thread32First(HANDLE(snapshot.as_raw_handle()), &mut entry)?;
    }
    let mut thread_id = None;
    loop {
        if entry.th32OwnerProcessID == pid {
            if thread_id.replace(entry.th32ThreadID).is_some() {
                anyhow::bail!("suspended process has multiple threads; refusing ambiguous resume");
            }
        }
        if let Err(error) = unsafe { Thread32Next(HANDLE(snapshot.as_raw_handle()), &mut entry) } {
            if error.code() == windows::core::HRESULT::from_win32(ERROR_NO_MORE_FILES.0) {
                break;
            }
            return Err(error.into());
        }
    }
    let thread_id =
        thread_id.ok_or_else(|| anyhow::anyhow!("suspended primary thread not found"))?;
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, thread_id)? };
    let thread = unsafe { OwnedHandle::from_raw_handle(thread.0) };
    let previous_count = unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) };
    if previous_count != 1 {
        anyhow::bail!("unexpected primary thread suspend count: {previous_count}");
    }
    Ok(())
}

#[async_trait]
impl SandboxBackend for JobObjectBackend {
    async fn acquire(&self, allocation: &str) -> Result<(), String> {
        tokio::fs::create_dir_all(self.allocation_dir(allocation))
            .await
            .map_err(|error| format!("mkdir failed: {error}"))
    }

    async fn execute(&self, allocation: &str, req: ExecRequest) -> ExecResult {
        let job = match self.create_job() {
            Ok(job) => job,
            Err(error) => return ExecResult::backend_error(format!("create job: {error}")),
        };
        let dir = self.allocation_dir(allocation);
        if let Err(error) = tokio::fs::create_dir_all(&dir).await {
            return ExecResult::backend_error(format!("mkdir: {error}"));
        }
        let script = dir.join(script_name(req.language.ext()));
        if let Err(error) = tokio::fs::write(&script, &req.code).await {
            return ExecResult::backend_error(format!("write: {error}"));
        }

        let mut spawned = Err("interpreter not found".to_string());
        for candidate in req.language.native_candidates() {
            let mut command = Command::new(candidate);
            command
                .arg(&script)
                .current_dir(&dir)
                .creation_flags(CREATE_SUSPENDED.0)
                .kill_on_drop(true)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            match command.spawn() {
                Ok(child) => {
                    spawned = Ok(child);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    spawned = Err(format!("spawn {candidate}: {error}"));
                    break;
                }
            }
        }
        let result = match spawned {
            Err(error) => ExecResult::backend_error(error),
            Ok(mut child) => {
                if let Err(error) = assign_and_resume(&child, &job) {
                    drop(job);
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(SANDBOX_CLEANUP_GRACE, child.wait()).await;
                    ExecResult::backend_error(format!("assign and resume job process: {error}"))
                } else {
                    run_spawned(child, req.stdin, req.timeout, req.max_output_bytes, || {
                        drop(job)
                    })
                    .await
                }
            }
        };
        let _ = tokio::fs::remove_file(&script).await;
        result
    }

    async fn cleanup(&self, allocation: &str) {
        let _ = tokio::fs::remove_dir_all(self.allocation_dir(allocation)).await;
    }

    fn name(&self) -> &'static str {
        "job_object"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn interpreter_cannot_execute_before_job_assignment() {
        let marker =
            std::env::temp_dir().join(format!("kaguya-suspended-{}.txt", uuid::Uuid::new_v4()));
        let backend = JobObjectBackend::new(&SandboxConfig::default()).unwrap();
        let job = backend.create_job().unwrap();
        let mut child = Command::new("python")
            .args([
                "-c",
                "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('ran')",
            ])
            .arg(&marker)
            .creation_flags(CREATE_SUSPENDED.0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !marker.exists(),
            "suspended interpreter must not execute before assignment"
        );
        assign_and_resume(&child, &job).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ran");
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn invalid_job_limits_are_rejected() {
        for cfg in [
            SandboxConfig {
                memory_limit_mb: 0,
                ..SandboxConfig::default()
            },
            SandboxConfig {
                pids_limit: u64::MAX,
                ..SandboxConfig::default()
            },
        ] {
            assert!(JobObjectBackend::new(&cfg).is_err());
        }
    }
}
