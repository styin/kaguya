//! Gateway lifecycle supervision primitives.
//!
//! This first pass tracks root Gateway tasks and centralizes shutdown. Deeper
//! component-level tasks can migrate here incrementally without changing the
//! public event loop semantics.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    P0Shutdown,
    OsSignal,
    Fatal,
    LoopEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Starting,
    Ready,
    Degraded,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    max_attempts: usize,
    initial_delay: Duration,
    max_delay: Duration,
    attempt_timeout: Duration,
}

impl ReconnectPolicy {
    pub fn bounded(
        max_attempts: usize,
        initial_delay: Duration,
        max_delay: Duration,
        attempt_timeout: Duration,
    ) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            initial_delay,
            max_delay,
            attempt_timeout,
        }
    }

    pub fn max_attempts(self) -> usize {
        self.max_attempts
    }

    pub fn attempt_timeout(self) -> Duration {
        self.attempt_timeout
    }

    pub fn retry_delays(self) -> Vec<Duration> {
        let mut delay = self.initial_delay;
        let mut delays = Vec::with_capacity(self.max_attempts.saturating_sub(1));
        for _ in 1..self.max_attempts {
            delays.push(delay.min(self.max_delay));
            delay = delay.saturating_mul(2);
        }
        delays
    }

    pub fn worst_case_elapsed(self) -> Duration {
        self.attempt_timeout
            .saturating_mul(self.max_attempts as u32)
            + self.retry_delays().into_iter().sum::<Duration>()
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::bounded(
            3,
            Duration::from_millis(250),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
    }
}

#[derive(Debug, Clone)]
pub struct ManagedConnection {
    name: String,
    readiness: Readiness,
}

impl ManagedConnection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            readiness: Readiness::Starting,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn readiness(&self) -> Readiness {
        self.readiness
    }

    pub fn set_readiness(&mut self, readiness: Readiness) {
        self.readiness = readiness;
    }
}

#[derive(Clone)]
pub struct ManagedConnectionHandle {
    name: String,
    connections: Arc<Mutex<HashMap<String, ManagedConnection>>>,
}

impl ManagedConnectionHandle {
    fn new(
        name: impl Into<String>,
        connections: Arc<Mutex<HashMap<String, ManagedConnection>>>,
    ) -> Self {
        Self {
            name: name.into(),
            connections,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn readiness(&self) -> Readiness {
        self.connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .get(&self.name)
            .map(ManagedConnection::readiness)
            .unwrap_or(Readiness::Stopped)
    }

    pub fn set_readiness(&self, readiness: Readiness) {
        let mut guard = self
            .connections
            .lock()
            .expect("managed connection registry lock poisoned");
        let connection = guard
            .entry(self.name.clone())
            .or_insert_with(|| ManagedConnection::new(self.name.clone()));
        connection.set_readiness(readiness);
    }
}

pub struct ManagedTask {
    name: String,
    handle: JoinHandle<()>,
}

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

pub struct ManagedProcess {
    name: String,
    child: Child,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedProcessSnapshot {
    pub name: String,
    pub pid: Option<u32>,
}

#[derive(Clone)]
pub struct TaskSpawner {
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<ManagedTask>>>,
}

impl TaskSpawner {
    fn new(shutdown: CancellationToken, tasks: Arc<Mutex<Vec<ManagedTask>>>) -> Self {
        Self { shutdown, tasks }
    }

    pub fn spawn<F>(&self, name: impl Into<String>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let name = name.into();
        let child = self.shutdown.child_token();
        let task_name = name.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = child.cancelled() => {
                    debug!(task = %task_name, "managed task cancelled");
                }
                _ = future => {
                    debug!(task = %task_name, "managed task completed");
                }
            }
        });

        self.tasks
            .lock()
            .expect("managed task registry lock poisoned")
            .push(ManagedTask { name, handle });
    }
}

pub struct LifecycleSupervisor {
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<ManagedTask>>>,
    processes: Arc<Mutex<Vec<ManagedProcess>>>,
    connections: Arc<Mutex<HashMap<String, ManagedConnection>>>,
    shutdown_grace: Duration,
}

impl LifecycleSupervisor {
    pub fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tasks: Arc::new(Mutex::new(Vec::new())),
            processes: Arc::new(Mutex::new(Vec::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
            shutdown_grace: Duration::from_secs(5),
        }
    }

    pub fn with_shutdown_grace(mut self, shutdown_grace: Duration) -> Self {
        self.shutdown_grace = shutdown_grace;
        self
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn spawner(&self) -> TaskSpawner {
        TaskSpawner::new(self.shutdown.clone(), Arc::clone(&self.tasks))
    }

    pub fn register_connection(&self, name: impl Into<String>) -> ManagedConnectionHandle {
        let name = name.into();
        self.connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .entry(name.clone())
            .or_insert_with(|| ManagedConnection::new(name.clone()));
        ManagedConnectionHandle::new(name, Arc::clone(&self.connections))
    }

    pub fn connection_readiness(&self, name: &str) -> Option<Readiness> {
        self.connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .get(name)
            .map(ManagedConnection::readiness)
    }

    pub fn connections_snapshot(&self) -> Vec<ManagedConnection> {
        let mut connections = self
            .connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        connections.sort_by(|a, b| a.name().cmp(b.name()));
        connections
    }

    pub fn task_count(&self) -> usize {
        self.tasks
            .lock()
            .expect("managed task registry lock poisoned")
            .len()
    }

    pub fn process_count(&self) -> usize {
        self.processes
            .lock()
            .expect("managed process registry lock poisoned")
            .len()
    }

    pub fn processes_snapshot(&self) -> Vec<ManagedProcessSnapshot> {
        let mut processes = self
            .processes
            .lock()
            .expect("managed process registry lock poisoned")
            .iter()
            .map(|process| ManagedProcessSnapshot {
                name: process.name.clone(),
                pid: process.child.id(),
            })
            .collect::<Vec<_>>();
        processes.sort_by(|a, b| a.name.cmp(&b.name));
        processes
    }

    pub fn spawn<F>(&self, name: impl Into<String>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.spawner().spawn(name, future);
    }

    pub fn start_process(&self, spec: ManagedProcessSpec) -> anyhow::Result<()> {
        let command = spec.command_for_platform();
        let mut child_command = shell_command(command);
        if let Some(cwd) = &spec.cwd {
            child_command.current_dir(cwd);
        }
        child_command
            .envs(&spec.env)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let child = child_command.spawn()?;
        info!(
            process = %spec.name(),
            pid = ?child.id(),
            "managed process started"
        );

        self.processes
            .lock()
            .expect("managed process registry lock poisoned")
            .push(ManagedProcess {
                name: spec.name,
                child,
            });
        Ok(())
    }

    pub async fn shutdown(&mut self, reason: ShutdownReason) {
        if self.shutdown.is_cancelled() && self.task_count() == 0 && self.process_count() == 0 {
            return;
        }

        self.shutdown.cancel();
        self.mark_connections(Readiness::Stopped);

        let tasks = {
            let mut guard = self
                .tasks
                .lock()
                .expect("managed task registry lock poisoned");
            std::mem::take(&mut *guard)
        };

        info!(
            reason = ?reason,
            task_count = tasks.len(),
            process_count = self.process_count(),
            "lifecycle shutdown requested"
        );

        for task in tasks {
            let name = task.name;
            let mut handle = task.handle;
            tokio::select! {
                result = &mut handle => {
                    match result {
                        Ok(()) => debug!(task = %name, "managed task joined"),
                        Err(e) if e.is_cancelled() => {
                            debug!(task = %name, "managed task aborted");
                        }
                        Err(e) => {
                            warn!(task = %name, "managed task failed during shutdown: {e}");
                        }
                    }
                }
                _ = tokio::time::sleep(self.shutdown_grace) => {
                    warn!(task = %name, "managed task did not stop within grace period; aborting");
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }

        let processes = {
            let mut guard = self
                .processes
                .lock()
                .expect("managed process registry lock poisoned");
            std::mem::take(&mut *guard)
        };

        for process in processes {
            self.stop_process(process).await;
        }
    }

    async fn stop_process(&self, mut process: ManagedProcess) {
        let name = process.name;
        let pid = process.child.id();
        info!(process = %name, pid = ?pid, "stopping managed process");

        if let Some(pid) = pid {
            if let Err(e) = terminate_process(pid).await {
                warn!(process = %name, pid = pid, "failed to request process termination: {e}");
            }
        }

        tokio::select! {
            result = process.child.wait() => {
                match result {
                    Ok(status) => info!(process = %name, status = %status, "managed process exited"),
                    Err(e) => warn!(process = %name, "failed waiting for managed process: {e}"),
                }
            }
            _ = tokio::time::sleep(self.shutdown_grace) => {
                warn!(process = %name, "managed process did not stop within grace period; force killing");
                if let Some(pid) = process.child.id() {
                    if let Err(e) = force_kill_process(pid).await {
                        warn!(process = %name, pid = pid, "failed to force kill process tree: {e}");
                    }
                } else if let Err(e) = process.child.kill().await {
                    warn!(process = %name, "failed to kill managed process: {e}");
                }
                let _ = process.child.wait().await;
            }
        }
    }

    fn mark_connections(&self, readiness: Readiness) {
        for connection in self
            .connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .values_mut()
        {
            connection.set_readiness(readiness);
        }
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
            .args(["/PID", &pid, "/T"])
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

impl Default for LifecycleSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use super::*;

    #[tokio::test]
    async fn shutdown_cancels_and_joins_managed_task() {
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_task = Arc::clone(&stopped);
        let mut supervisor =
            LifecycleSupervisor::new().with_shutdown_grace(Duration::from_millis(100));

        supervisor.spawn("test-task", async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            stopped_task.store(true, Ordering::SeqCst);
        });

        assert_eq!(supervisor.task_count(), 1);
        supervisor.shutdown(ShutdownReason::P0Shutdown).await;
        assert_eq!(supervisor.task_count(), 0);
        assert!(!stopped.load(Ordering::SeqCst));
    }

    #[test]
    fn managed_connection_tracks_readiness() {
        let supervisor = LifecycleSupervisor::new();
        let connection = supervisor.register_connection("talker");
        assert_eq!(connection.name(), "talker");
        assert_eq!(connection.readiness(), Readiness::Starting);
        assert_eq!(
            supervisor.connection_readiness("talker"),
            Some(Readiness::Starting)
        );

        connection.set_readiness(Readiness::Ready);
        assert_eq!(connection.readiness(), Readiness::Ready);
        assert_eq!(
            supervisor.connection_readiness("talker"),
            Some(Readiness::Ready)
        );
    }

    #[test]
    fn reconnect_policy_is_bounded_and_exponential() {
        let policy = ReconnectPolicy::bounded(
            4,
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_secs(3),
        );

        assert_eq!(policy.max_attempts(), 4);
        assert_eq!(policy.attempt_timeout(), Duration::from_secs(3));
        assert_eq!(
            policy.retry_delays(),
            vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(250),
            ]
        );
    }

    #[test]
    fn reconnect_policy_allows_at_least_one_attempt() {
        let policy = ReconnectPolicy::bounded(
            0,
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_secs(3),
        );

        assert_eq!(policy.max_attempts(), 1);
        assert!(policy.retry_delays().is_empty());
    }

    #[test]
    fn reconnect_policy_reports_worst_case_elapsed_budget() {
        let policy = ReconnectPolicy::bounded(
            3,
            Duration::from_millis(250),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );

        assert_eq!(policy.worst_case_elapsed(), Duration::from_millis(6_750));
    }

    #[tokio::test]
    async fn shutdown_marks_connections_stopped() {
        let mut supervisor =
            LifecycleSupervisor::new().with_shutdown_grace(Duration::from_millis(100));
        let connection = supervisor.register_connection("listener");

        connection.set_readiness(Readiness::Ready);
        supervisor.shutdown(ShutdownReason::P0Shutdown).await;

        assert_eq!(connection.readiness(), Readiness::Stopped);
    }

    #[test]
    fn duplicate_connection_handles_share_one_readiness_slot() {
        let supervisor = LifecycleSupervisor::new();
        let listener_grpc = supervisor.register_connection("listener");
        let listener_audio = supervisor.register_connection("listener");

        listener_grpc.set_readiness(Readiness::Ready);
        assert_eq!(
            supervisor.connection_readiness("listener"),
            Some(Readiness::Ready)
        );

        listener_audio.set_readiness(Readiness::Degraded);
        assert_eq!(listener_grpc.readiness(), Readiness::Degraded);
        assert_eq!(supervisor.connections_snapshot().len(), 1);
    }

    #[tokio::test]
    async fn task_spawned_after_shutdown_sees_cancelled_token_and_can_be_drained() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_task = Arc::clone(&ran);
        let mut supervisor =
            LifecycleSupervisor::new().with_shutdown_grace(Duration::from_millis(100));
        let spawner = supervisor.spawner();

        supervisor.shutdown(ShutdownReason::P0Shutdown).await;
        spawner.spawn("late-task", async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            ran_task.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(supervisor.task_count(), 1);
        assert!(!ran.load(Ordering::SeqCst));

        supervisor.shutdown(ShutdownReason::LoopEnded).await;
        assert_eq!(supervisor.task_count(), 0);
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn shutdown_stops_managed_process() {
        let mut supervisor =
            LifecycleSupervisor::new().with_shutdown_grace(Duration::from_millis(500));
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command \"Start-Sleep -Seconds 60\""
        } else {
            "sleep 60"
        };

        supervisor
            .start_process(ManagedProcessSpec::new("test-process", command))
            .expect("process should start");
        assert_eq!(supervisor.process_count(), 1);
        assert_eq!(supervisor.processes_snapshot().len(), 1);

        supervisor.shutdown(ShutdownReason::P0Shutdown).await;

        assert_eq!(supervisor.process_count(), 0);
    }
}
