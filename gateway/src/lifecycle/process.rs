use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{info, warn};

const MANAGED_PROCESS_LOG_PREFIX: &str = "__KAGUYA_MANAGED_LOG__ ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedProcessSpec {
    name: String,
    command: String,
    command_win32: Option<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
}

impl ManagedProcessSpec {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            command_win32: None,
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    pub fn with_command_win32(mut self, command_win32: Option<String>) -> Self {
        self.command_win32 = command_win32;
        self
    }

    pub fn with_cwd(mut self, cwd: Option<PathBuf>) -> Self {
        self.cwd = cwd;
        self
    }

    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn command_for_platform(&self) -> &str {
        if cfg!(windows) {
            self.command_win32.as_deref().unwrap_or(&self.command)
        } else {
            &self.command
        }
    }
}

pub(crate) struct ManagedProcess {
    name: String,
    child: Child,
    pid: Option<u32>,
    exit_status: Option<ExitStatus>,
    log_tasks: Vec<JoinHandle<()>>,
}

impl ManagedProcess {
    pub(crate) fn refresh_snapshot(&mut self) -> ManagedProcessSnapshot {
        if self.exit_status.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    info!(
                        process = %self.name,
                        status = %status,
                        "managed process exited"
                    );
                    self.exit_status = Some(status);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(process = %self.name, "failed polling managed process status: {e}");
                }
            }
        }

        let status = match self.exit_status {
            None => ManagedProcessStatus::Running,
            Some(status) if status.success() => ManagedProcessStatus::Exited,
            Some(_) => ManagedProcessStatus::Failed,
        };

        ManagedProcessSnapshot {
            name: self.name.clone(),
            pid: self.pid,
            status,
            exit_code: self.exit_status.and_then(|status| status.code()),
        }
    }
}

#[derive(Serialize)]
struct ManagedProcessLogLine<'a> {
    source: &'a str,
    stream: &'a str,
    line: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedProcessStatus {
    Running,
    Exited,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedProcessSnapshot {
    pub name: String,
    pub pid: Option<u32>,
    pub status: ManagedProcessStatus,
    pub exit_code: Option<i32>,
}

pub(crate) fn start_managed_process(spec: ManagedProcessSpec) -> anyhow::Result<ManagedProcess> {
    let command = spec.command_for_platform();
    let mut child_command = shell_command(command);
    if let Some(cwd) = &spec.cwd {
        child_command.current_dir(cwd);
    }
    child_command
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = child_command.spawn()?;
    let mut log_tasks = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        log_tasks.push(spawn_process_log_forwarder(
            spec.name.clone(),
            "stdout",
            stdout,
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        log_tasks.push(spawn_process_log_forwarder(
            spec.name.clone(),
            "stderr",
            stderr,
        ));
    }

    info!(
        process = %spec.name(),
        pid = ?child.id(),
        "managed process started"
    );

    let pid = child.id();
    Ok(ManagedProcess {
        name: spec.name,
        child,
        pid,
        exit_status: None,
        log_tasks,
    })
}

pub(crate) async fn stop_managed_process(mut process: ManagedProcess, shutdown_grace: Duration) {
    let name = process.name;
    if let Some(status) = process.exit_status {
        info!(process = %name, status = %status, "managed process already exited");
        abort_log_tasks(process.log_tasks).await;
        return;
    }

    let pid = process.pid;
    info!(process = %name, pid = ?pid, "stopping managed process");

    if let Some(pid) = pid {
        match tokio::time::timeout(shutdown_grace, terminate_process(pid)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(process = %name, pid = pid, "failed to request process termination: {e}");
            }
            Err(_) => {
                warn!(
                    process = %name,
                    pid = pid,
                    "process termination request timed out"
                );
            }
        }
    }

    match wait_for_process_exit(&mut process.child, shutdown_grace).await {
        Ok(Some(status)) => info!(process = %name, status = %status, "managed process exited"),
        Ok(None) => {
            warn!(
                process = %name,
                "managed process did not stop within grace period; force killing"
            );
            if let Some(pid) = process.child.id() {
                match tokio::time::timeout(shutdown_grace, force_kill_process(pid)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        warn!(process = %name, pid = pid, "failed to force kill process tree: {e}");
                    }
                    Err(_) => {
                        warn!(process = %name, pid = pid, "force kill process tree timed out");
                    }
                }
            }
            if let Err(e) = process.child.start_kill() {
                warn!(process = %name, "failed to signal managed process kill: {e}");
            }
            match wait_for_process_exit(&mut process.child, shutdown_grace).await {
                Ok(Some(status)) => {
                    info!(process = %name, status = %status, "managed process killed")
                }
                Ok(None) => {
                    warn!(process = %name, "managed process still alive after force kill")
                }
                Err(e) => warn!(process = %name, "failed waiting for killed process: {e}"),
            }
        }
        Err(e) => warn!(process = %name, "failed waiting for managed process: {e}"),
    }

    abort_log_tasks(process.log_tasks).await;
}

async fn abort_log_tasks(log_tasks: Vec<JoinHandle<()>>) {
    for task in log_tasks {
        task.abort();
        let _ = task.await;
    }
}

fn spawn_process_log_forwarder<R>(
    process: String,
    stream: &'static str,
    reader: R,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => emit_managed_process_log(&process, stream, &line),
                Ok(None) => break,
                Err(e) => {
                    warn!(
                        process = %process,
                        stream,
                        "failed reading managed process log stream: {e}"
                    );
                    break;
                }
            }
        }
    })
}

fn emit_managed_process_log(process: &str, stream: &str, line: &str) {
    let payload = ManagedProcessLogLine {
        source: process,
        stream,
        line,
    };
    let Ok(encoded) = serde_json::to_string(&payload) else {
        warn!(process, stream, "failed encoding managed process log line");
        return;
    };
    let line = format!("{MANAGED_PROCESS_LOG_PREFIX}{encoded}");
    if stream == "stderr" {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{line}");
        let _ = stderr.flush();
    } else {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

fn shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-lc").arg(command);
        cmd
    }
}

async fn terminate_process(pid: u32) -> std::io::Result<()> {
    let pid = pid.to_string();
    if cfg!(windows) {
        Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
    } else {
        Command::new("kill")
            .args(["-TERM", &pid])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
    }
    Ok(())
}

async fn force_kill_process(pid: u32) -> std::io::Result<()> {
    let pid = pid.to_string();
    if cfg!(windows) {
        Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
    } else {
        Command::new("kill")
            .args(["-KILL", &pid])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
    }
    Ok(())
}

async fn wait_for_process_exit(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }

        tokio::time::sleep((deadline - now).min(Duration::from_millis(50))).await;
    }
}
