use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub const MANAGED_PROCESS_LOG_PREFIX: &str = "__KAGUYA_MANAGED_LOG__ ";

#[derive(Debug, Clone)]
pub struct ManagedProcessSpec {
    name: String,
    command: String,
    command_win32: Option<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    restart_policy: ManagedProcessRestartPolicy,
    log_sink: Option<mpsc::UnboundedSender<ManagedProcessLogLine>>,
}

impl ManagedProcessSpec {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            command_win32: None,
            cwd: None,
            env: BTreeMap::new(),
            restart_policy: ManagedProcessRestartPolicy::Never,
            log_sink: None,
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

    pub fn with_restart_policy(mut self, restart_policy: ManagedProcessRestartPolicy) -> Self {
        self.restart_policy = restart_policy;
        self
    }

    pub fn with_log_sink(
        mut self,
        log_sink: Option<mpsc::UnboundedSender<ManagedProcessLogLine>>,
    ) -> Self {
        self.log_sink = log_sink;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn restart_policy(&self) -> ManagedProcessRestartPolicy {
        self.restart_policy
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    fn command_for_platform(&self) -> &str {
        if cfg!(windows) {
            self.command_win32.as_deref().unwrap_or(&self.command)
        } else {
            &self.command
        }
    }
}

pub struct ManagedProcess {
    spec: ManagedProcessSpec,
    child: Child,
    pid: Option<u32>,
    exit_status: Option<ExitStatus>,
    restart_count: u64,
    log_tasks: Vec<JoinHandle<()>>,
}

impl ManagedProcess {
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn refresh_snapshot(&mut self) -> ManagedProcessSnapshot {
        self.poll_exit_status();

        ManagedProcessSnapshot {
            name: self.spec.name.clone(),
            pid: self.pid,
            status: self.status(),
            exit_code: self.exit_status.and_then(|status| status.code()),
            restart_policy: self.spec.restart_policy,
            restart_count: self.restart_count,
        }
    }

    pub fn apply_restart_policy(&mut self) {
        self.poll_exit_status();
        let Some(exit_status) = self.exit_status else {
            return;
        };
        if !self.should_restart(exit_status) {
            return;
        }

        self.abort_log_tasks();
        match spawn_managed_child(&self.spec) {
            Ok(started) => {
                self.child = started.child;
                self.pid = started.pid;
                self.log_tasks = started.log_tasks;
                self.exit_status = None;
                self.restart_count += 1;
                info!(
                    process = %self.spec.name,
                    restart_count = self.restart_count,
                    policy = ?self.spec.restart_policy,
                    "managed process restarted"
                );
            }
            Err(e) => {
                warn!(process = %self.spec.name, "failed restarting managed process: {e}");
            }
        }
    }

    pub async fn wait_for_exit(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<ExitStatus>> {
        let result = wait_for_process_exit(&mut self.child, timeout).await?;
        if let Some(status) = result {
            self.exit_status = Some(status);
        }
        Ok(result)
    }

    fn poll_exit_status(&mut self) {
        if self.exit_status.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    info!(
                        process = %self.spec.name,
                        status = %status,
                        "managed process exited"
                    );
                    self.exit_status = Some(status);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(process = %self.spec.name, "failed polling managed process status: {e}");
                }
            }
        }
    }

    fn status(&self) -> ManagedProcessStatus {
        match self.exit_status {
            None => ManagedProcessStatus::Running,
            Some(status) if status.success() => ManagedProcessStatus::Exited,
            Some(_) => ManagedProcessStatus::Failed,
        }
    }

    fn should_restart(&self, status: ExitStatus) -> bool {
        match self.spec.restart_policy {
            ManagedProcessRestartPolicy::Never => false,
            ManagedProcessRestartPolicy::OnFailure => !status.success(),
            ManagedProcessRestartPolicy::KeepAlive => true,
        }
    }

    fn abort_log_tasks(&mut self) {
        for task in self.log_tasks.drain(..) {
            task.abort();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagedProcessLogLine {
    pub source: String,
    pub stream: String,
    pub line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedProcessRestartPolicy {
    Never,
    OnFailure,
    KeepAlive,
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
    pub restart_policy: ManagedProcessRestartPolicy,
    pub restart_count: u64,
}

pub fn start_managed_process(spec: ManagedProcessSpec) -> anyhow::Result<ManagedProcess> {
    let started = spawn_managed_child(&spec)?;
    Ok(ManagedProcess {
        spec,
        child: started.child,
        pid: started.pid,
        exit_status: None,
        restart_count: 0,
        log_tasks: started.log_tasks,
    })
}

struct StartedManagedProcess {
    child: Child,
    pid: Option<u32>,
    log_tasks: Vec<JoinHandle<()>>,
}

fn spawn_managed_child(spec: &ManagedProcessSpec) -> anyhow::Result<StartedManagedProcess> {
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
            spec.log_sink.clone(),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        log_tasks.push(spawn_process_log_forwarder(
            spec.name.clone(),
            "stderr",
            stderr,
            spec.log_sink.clone(),
        ));
    }

    info!(
        process = %spec.name(),
        pid = ?child.id(),
        "managed process started"
    );

    let pid = child.id();
    Ok(StartedManagedProcess {
        child,
        pid,
        log_tasks,
    })
}

pub async fn stop_managed_process(mut process: ManagedProcess, shutdown_grace: Duration) {
    let name = process.spec.name.clone();
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
    log_sink: Option<mpsc::UnboundedSender<ManagedProcessLogLine>>,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => emit_managed_process_log(&process, stream, &line, &log_sink),
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

fn emit_managed_process_log(
    process: &str,
    stream: &str,
    line: &str,
    log_sink: &Option<mpsc::UnboundedSender<ManagedProcessLogLine>>,
) {
    if let Some(log_sink) = log_sink {
        let _ = log_sink.send(ManagedProcessLogLine {
            source: process.to_string(),
            stream: stream.to_string(),
            line: line.to_string(),
        });
        return;
    }

    let payload = ManagedProcessLogLine {
        source: process.to_string(),
        stream: stream.to_string(),
        line: line.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_managed_process_stops_child() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command Start-Sleep -Seconds 2"
        } else {
            "sleep 2"
        };

        let process = start_managed_process(ManagedProcessSpec::new("test-process", command))
            .expect("process should start");

        stop_managed_process(process, Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn process_snapshot_reports_failed_exit_status() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command exit 7"
        } else {
            "exit 7"
        };

        let mut process =
            start_managed_process(ManagedProcessSpec::new("failing-process", command))
                .expect("process should start");

        let mut snapshot = process.refresh_snapshot();
        for _ in 0..10 {
            snapshot = process.refresh_snapshot();
            if snapshot.status == ManagedProcessStatus::Failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert_eq!(snapshot.name, "failing-process");
        assert_eq!(snapshot.status, ManagedProcessStatus::Failed);
        assert_eq!(snapshot.exit_code, Some(7));
        assert_eq!(snapshot.restart_policy, ManagedProcessRestartPolicy::Never);
        assert_eq!(snapshot.restart_count, 0);

        stop_managed_process(process, Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn restart_policy_restarts_failed_process() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command Start-Sleep -Milliseconds 250; exit 7"
        } else {
            "sleep 0.25; exit 7"
        };

        let mut process = start_managed_process(
            ManagedProcessSpec::new("restart-process", command)
                .with_restart_policy(ManagedProcessRestartPolicy::OnFailure),
        )
        .expect("process should start");

        let mut restart_count = 0;
        for _ in 0..20 {
            process.apply_restart_policy();
            restart_count = process.refresh_snapshot().restart_count;
            if restart_count > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(restart_count > 0);

        stop_managed_process(process, Duration::from_millis(500)).await;
    }
}
