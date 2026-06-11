//! Application-level process orchestration.
//!
//! [`SupervisorApp`] is the top-level owner of the Kaguya process graph. It
//! starts, monitors, and restarts managed processes according to their
//! [`RestartPolicy`], polls external process health endpoints, and exposes a
//! status API consumed by the dev console.
//!
//! Key behaviours:
//! - Eager processes are launched in dependency order on [`start_app`](SupervisorApp::start_app).
//! - A sliding-window restart limit (`max_restarts` / `restart_window_secs`)
//!   prevents infinite crash loops — exhausted processes enter `Errored` and
//!   stop restarting until manually started again.
//! - Gateway shutdown uses a two-phase drain: gRPC graceful shutdown request,
//!   then SIGTERM after a timeout.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};

use crate::config::{LaunchMode, ProcessSpec, ResolvedRuntimeConfig, RestartPolicy, RuntimeConfig};
use crate::gateway;
use crate::logs::LogStore;
use crate::process::{
    start_managed_process, stop_managed_process, ManagedProcess, ManagedProcessLogLine,
    ManagedProcessSnapshot, ManagedProcessStatus,
};

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const GATEWAY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MONITOR_INTERVAL: Duration = Duration::from_millis(250);
const RESTART_BASE_DELAY: Duration = Duration::from_millis(500);
const RESTART_MAX_DELAY: Duration = Duration::from_secs(5);

/// Top-level process orchestrator.
///
/// Owns the full process graph and exposes start/stop/restart operations for
/// individual processes and the application as a whole. Cloneable — shared
/// between the HTTP server and the monitor loop.
#[derive(Clone)]
pub struct SupervisorApp {
    inner: std::sync::Arc<Mutex<SupervisorInner>>,
    logs: LogStore,
    log_tx: mpsc::UnboundedSender<ManagedProcessLogLine>,
    http: reqwest::Client,
}

struct SupervisorInner {
    config: RuntimeConfig,
    processes: BTreeMap<String, RuntimeProcessState>,
    restart_disabled: bool,
}

struct RuntimeProcessState {
    spec: ProcessSpec,
    process: Option<ManagedProcess>,
    started_at: Option<Instant>,
    manual_stopped: bool,
    next_restart_at: Option<Instant>,
    last_exit_code: Option<i32>,
    external_status: Option<ProcessStatus>,
    last_health_check: Option<Instant>,
    restart_timestamps: Vec<Instant>,
    restart_exhausted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppLifecycleState {
    Stopped,
    Running,
    Degraded,
    Stopping,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatusResponse {
    pub state: AppLifecycleState,
    pub processes: Vec<ProcessInfo>,
    pub gateway: Option<GatewayStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub name: String,
    pub label: String,
    pub managed: bool,
    pub status: ProcessStatus,
    pub pid: Option<u32>,
    pub uptime_secs: Option<u64>,
    pub exit_code: Option<i32>,
    pub restart_policy: RestartPolicy,
    pub restart_count: u64,
    pub restart_exhausted: bool,
    pub blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<RuntimeChildInfo>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Stopped,
    Starting,
    Running,
    Errored,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeChildInfo {
    pub name: String,
    pub label: String,
    pub kind: String,
    pub status: ProcessStatus,
    pub readiness: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayStatus {
    pub lifecycle: GatewayLifecycleStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayLifecycleStatus {
    pub task_count: usize,
    pub connections: Vec<GatewayConnectionStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConnectionStatus {
    pub name: String,
    pub readiness: String,
}

impl SupervisorApp {
    pub fn new(resolved: ResolvedRuntimeConfig) -> Self {
        let logs = LogStore::new();
        let (log_tx, mut log_rx) = mpsc::unbounded_channel::<ManagedProcessLogLine>();
        let log_store = logs.clone();
        tokio::spawn(async move {
            while let Some(line) = log_rx.recv().await {
                log_store.push_process_log(line);
            }
        });

        let processes = resolved
            .config
            .processes
            .iter()
            .map(|(id, spec)| {
                (
                    id.clone(),
                    RuntimeProcessState {
                        spec: spec.clone(),
                        process: None,
                        started_at: None,
                        manual_stopped: false,
                        next_restart_at: None,
                        last_exit_code: None,
                        external_status: None,
                        last_health_check: None,
                        restart_timestamps: Vec::new(),
                        restart_exhausted: false,
                    },
                )
            })
            .collect();

        Self {
            inner: std::sync::Arc::new(Mutex::new(SupervisorInner {
                config: resolved.config,
                processes,
                restart_disabled: false,
            })),
            logs,
            log_tx,
            http: reqwest::Client::new(),
        }
    }

    pub fn logs(&self) -> LogStore {
        self.logs.clone()
    }

    pub fn start_monitor(&self) {
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                app.enforce_restart_policy().await;
                tokio::time::sleep(MONITOR_INTERVAL).await;
            }
        });
    }

    pub async fn start_app(&self) -> anyhow::Result<()> {
        let names = {
            let inner = self.inner.lock().await;
            inner
                .processes
                .iter()
                .filter(|(_, state)| state.spec.is_eager_managed())
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };

        for name in names {
            self.start_process(&name).await?;
        }
        Ok(())
    }

    pub async fn shutdown_app(&self) -> anyhow::Result<()> {
        {
            let mut inner = self.inner.lock().await;
            inner.restart_disabled = true;
        }
        self.logs.push(
            "supervisor",
            "stdout",
            "[supervisor] app shutdown requested",
        );

        self.request_gateway_drain().await;

        let names = {
            let inner = self.inner.lock().await;
            inner.processes.keys().cloned().collect::<Vec<_>>()
        };

        for name in names {
            let _ = self.stop_process(&name).await;
        }
        Ok(())
    }

    pub async fn start_process(&self, name: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        inner.restart_disabled = false;
        let Some(state) = inner.processes.get_mut(name) else {
            anyhow::bail!("unknown process: {name}");
        };
        if !state.spec.enabled {
            anyhow::bail!("process disabled: {name}");
        }
        if state.spec.launch == LaunchMode::External {
            anyhow::bail!("process is external: {name}");
        }

        if let Some(process) = &mut state.process {
            let snapshot = process.refresh_snapshot();
            if snapshot.status == ManagedProcessStatus::Running {
                anyhow::bail!("{name} already running");
            }
        }

        let spec = state
            .spec
            .process_spec(name)?
            .with_log_sink(Some(self.log_tx.clone()));
        let process = start_managed_process(spec)?;
        let pid = process.pid();
        state.process = Some(process);
        state.started_at = Some(Instant::now());
        state.manual_stopped = false;
        state.next_restart_at = None;
        state.last_exit_code = None;
        state.restart_exhausted = false;
        state.restart_timestamps.clear();
        self.logs
            .push(name, "stdout", format!("[supervisor] started PID {pid:?}"));
        Ok(())
    }

    pub async fn stop_process(&self, name: &str) -> anyhow::Result<()> {
        let process = {
            let mut inner = self.inner.lock().await;
            let Some(state) = inner.processes.get_mut(name) else {
                anyhow::bail!("unknown process: {name}");
            };
            state.manual_stopped = true;
            state.next_restart_at = None;
            state.started_at = None;
            state.process.take()
        };

        if let Some(process) = process {
            self.logs
                .push(name, "stdout", "[supervisor] stopping process");
            stop_managed_process(process, SHUTDOWN_GRACE).await;
        }
        Ok(())
    }

    pub async fn restart_process(&self, name: &str) -> anyhow::Result<()> {
        self.stop_process(name).await?;
        self.start_process(name).await
    }

    pub async fn status(&self) -> AppStatusResponse {
        let gateway = self.fetch_gateway_status().await;
        let external_health = self.external_health().await;
        let mut inner = self.inner.lock().await;
        let mut processes = Vec::new();
        let statuses = collect_process_statuses(&mut inner.processes, &external_health);

        for (name, state) in inner.processes.iter_mut() {
            processes.push(process_info(
                name,
                state,
                gateway.as_ref(),
                external_health.get(name).copied(),
                &statuses,
            ));
        }

        AppStatusResponse {
            state: app_state(&processes, inner.restart_disabled),
            processes,
            gateway,
        }
    }

    pub async fn process_status(&self) -> Vec<ProcessInfo> {
        self.status().await.processes
    }

    async fn enforce_restart_policy(&self) {
        let mut inner = self.inner.lock().await;
        if inner.restart_disabled {
            return;
        }

        let now = Instant::now();
        for (name, state) in inner.processes.iter_mut() {
            let Some(process) = &mut state.process else {
                continue;
            };

            let snapshot = process.refresh_snapshot();
            if snapshot.status == ManagedProcessStatus::Running {
                state.next_restart_at = None;
                continue;
            }

            state.last_exit_code = snapshot.exit_code;
            if state.manual_stopped
                || state.restart_exhausted
                || !should_restart(state.spec.restart, &snapshot)
            {
                continue;
            }

            let next_restart_at = state
                .next_restart_at
                .get_or_insert_with(|| now + restart_delay(snapshot.restart_count));
            if now < *next_restart_at {
                continue;
            }

            if let Some(max_restarts) = state.spec.max_restarts {
                let window = Duration::from_secs(state.spec.restart_window_secs.unwrap_or(300));
                state
                    .restart_timestamps
                    .retain(|t| now.duration_since(*t) < window);
                state.restart_timestamps.push(now);
                if state.restart_timestamps.len() as u32 > max_restarts {
                    state.restart_exhausted = true;
                    self.logs.push(
                        name,
                        "stderr",
                        format!(
                            "[supervisor] restart exhaustion: {} restarts in {}s, suppressing further restarts",
                            state.restart_timestamps.len(),
                            window.as_secs(),
                        ),
                    );
                    continue;
                }
            }

            process.apply_restart_policy();
            let restarted = process.refresh_snapshot();
            if restarted.status == ManagedProcessStatus::Running {
                state.started_at = Some(now);
                state.next_restart_at = None;
                self.logs.push(
                    name,
                    "stderr",
                    format!(
                        "[supervisor] restarted process count={}",
                        restarted.restart_count
                    ),
                );
            } else {
                state.next_restart_at = Some(now + restart_delay(restarted.restart_count));
            }
        }
    }

    async fn request_gateway_drain(&self) {
        let endpoint = {
            let mut inner = self.inner.lock().await;
            let Some(gateway_state) = inner.processes.get_mut("gateway") else {
                return;
            };
            let Some(process) = gateway_state.process.as_mut() else {
                return;
            };
            if process.refresh_snapshot().status != ManagedProcessStatus::Running {
                return;
            }
            inner.config.gateway_grpc_endpoint().map(str::to_string)
        };
        let Some(endpoint) = endpoint else {
            return;
        };

        match gateway::request_gateway_shutdown(&endpoint, GATEWAY_DRAIN_TIMEOUT).await {
            Ok(()) => self.logs.push(
                "gateway",
                "stdout",
                "[supervisor] requested Gateway graceful shutdown",
            ),
            Err(e) => self.logs.push(
                "gateway",
                "stderr",
                format!("[supervisor] Gateway graceful shutdown unavailable: {e}"),
            ),
        }

        let mut gateway_process = {
            let mut inner = self.inner.lock().await;
            let Some(state) = inner.processes.get_mut("gateway") else {
                return;
            };
            state.process.take()
        };

        if let Some(process) = &mut gateway_process {
            if let Ok(Some(status)) = process.wait_for_exit(GATEWAY_DRAIN_TIMEOUT).await {
                let mut inner = self.inner.lock().await;
                if let Some(state) = inner.processes.get_mut("gateway") {
                    state.last_exit_code = status.code();
                }
                return;
            }
        }

        if let Some(process) = gateway_process {
            stop_managed_process(process, SHUTDOWN_GRACE).await;
        }
    }

    async fn fetch_gateway_status(&self) -> Option<GatewayStatus> {
        let endpoint = {
            let inner = self.inner.lock().await;
            inner
                .processes
                .get("gateway")
                .and_then(|state| {
                    state
                        .spec
                        .endpoints
                        .get("websocket")
                        .or_else(|| state.spec.endpoints.get("http"))
                })
                .cloned()
        }?;

        let url = format!("{}/capabilities/status", endpoint.trim_end_matches('/'));
        let response = self.http.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json::<GatewayStatus>().await.ok()
    }

    async fn external_health(&self) -> BTreeMap<String, ProcessStatus> {
        let mut statuses = BTreeMap::new();
        let checks = {
            let mut inner = self.inner.lock().await;
            let now = Instant::now();
            inner
                .processes
                .iter_mut()
                .filter_map(|(name, state)| {
                    if state.spec.launch != LaunchMode::External {
                        return None;
                    }
                    if let Some(status) = state.external_status {
                        statuses.insert(name.clone(), status);
                    }
                    let url = state.spec.health_url.clone()?;
                    let due = state
                        .last_health_check
                        .map(|last| now.duration_since(last) >= state.spec.poll_interval())
                        .unwrap_or(true);
                    if !due {
                        return None;
                    }
                    state.last_health_check = Some(now);
                    Some((name.clone(), url))
                })
                .collect::<Vec<_>>()
        };

        let mut updates = BTreeMap::new();
        for (name, url) in checks {
            let status =
                match tokio::time::timeout(Duration::from_secs(3), self.http.get(url).send()).await
                {
                    Ok(Ok(response)) if response.status().is_success() => ProcessStatus::Running,
                    Ok(Ok(_)) => ProcessStatus::Errored,
                    _ => ProcessStatus::Stopped,
                };
            updates.insert(name, status);
        }

        if !updates.is_empty() {
            let mut inner = self.inner.lock().await;
            for (name, status) in &updates {
                if let Some(state) = inner.processes.get_mut(name) {
                    state.external_status = Some(*status);
                }
                statuses.insert(name.clone(), *status);
            }
        }
        statuses
    }
}

fn process_info(
    name: &str,
    state: &mut RuntimeProcessState,
    gateway: Option<&GatewayStatus>,
    external_status: Option<ProcessStatus>,
    statuses: &BTreeMap<String, ProcessStatus>,
) -> ProcessInfo {
    let managed = state.spec.launch != LaunchMode::External;
    let mut status = ProcessStatus::Stopped;
    let mut pid = None;
    let mut exit_code = state.last_exit_code;
    let mut restart_count = 0;

    if let Some(process) = &mut state.process {
        let snapshot = process.refresh_snapshot();
        pid = snapshot.pid;
        exit_code = snapshot.exit_code.or(exit_code);
        restart_count = snapshot.restart_count;
        status = match snapshot.status {
            ManagedProcessStatus::Running => ProcessStatus::Running,
            ManagedProcessStatus::Exited => ProcessStatus::Stopped,
            ManagedProcessStatus::Failed => ProcessStatus::Errored,
        };
    } else if state.spec.launch == LaunchMode::External {
        status = external_status.unwrap_or(ProcessStatus::Stopped);
    }

    if state.restart_exhausted && status != ProcessStatus::Running {
        status = ProcessStatus::Errored;
    }

    ProcessInfo {
        name: name.to_string(),
        label: state.spec.label(name),
        managed,
        status,
        pid,
        uptime_secs: state
            .started_at
            .filter(|_| status == ProcessStatus::Running)
            .map(|started| started.elapsed().as_secs()),
        exit_code,
        restart_policy: state.spec.restart,
        restart_count,
        restart_exhausted: state.restart_exhausted,
        blocked_by: blocked_dependencies(&state.spec, statuses),
        children: capability_children(&state.spec, gateway, status),
    }
}

fn collect_process_statuses(
    processes: &mut BTreeMap<String, RuntimeProcessState>,
    external_health: &BTreeMap<String, ProcessStatus>,
) -> BTreeMap<String, ProcessStatus> {
    processes
        .iter_mut()
        .map(|(name, state)| {
            let status = observed_process_status(state, external_health.get(name).copied());
            (name.clone(), status)
        })
        .collect()
}

fn observed_process_status(
    state: &mut RuntimeProcessState,
    external_status: Option<ProcessStatus>,
) -> ProcessStatus {
    if let Some(process) = &mut state.process {
        let snapshot = process.refresh_snapshot();
        state.last_exit_code = snapshot.exit_code.or(state.last_exit_code);
        return match snapshot.status {
            ManagedProcessStatus::Running => ProcessStatus::Running,
            ManagedProcessStatus::Exited => ProcessStatus::Stopped,
            ManagedProcessStatus::Failed => ProcessStatus::Errored,
        };
    }

    if state.spec.launch == LaunchMode::External {
        return external_status.unwrap_or(ProcessStatus::Stopped);
    }

    ProcessStatus::Stopped
}

fn blocked_dependencies(
    spec: &ProcessSpec,
    statuses: &BTreeMap<String, ProcessStatus>,
) -> Vec<String> {
    spec.depends_on
        .iter()
        .filter(|dependency| statuses.get(dependency.as_str()) != Some(&ProcessStatus::Running))
        .cloned()
        .collect()
}

fn capability_children(
    spec: &ProcessSpec,
    gateway: Option<&GatewayStatus>,
    parent_status: ProcessStatus,
) -> Vec<RuntimeChildInfo> {
    let Some(gateway) = gateway else {
        return Vec::new();
    };
    gateway
        .lifecycle
        .connections
        .iter()
        .filter(|connection| {
            spec.provides
                .iter()
                .any(|capability| capability == &connection.name)
        })
        .map(|connection| RuntimeChildInfo {
            name: connection.name.clone(),
            label: display_name(&connection.name),
            kind: "connection".to_string(),
            status: if parent_status == ProcessStatus::Running {
                process_status_from_readiness(&connection.readiness)
            } else {
                ProcessStatus::Stopped
            },
            readiness: Some(if parent_status == ProcessStatus::Running {
                connection.readiness.clone()
            } else {
                "stopped".to_string()
            }),
        })
        .collect()
}

fn process_status_from_readiness(readiness: &str) -> ProcessStatus {
    match readiness {
        "ready" => ProcessStatus::Running,
        "starting" => ProcessStatus::Starting,
        "degraded" => ProcessStatus::Errored,
        _ => ProcessStatus::Stopped,
    }
}

fn app_state(processes: &[ProcessInfo], stopping: bool) -> AppLifecycleState {
    if stopping
        && processes
            .iter()
            .any(|process| process.managed && process.status == ProcessStatus::Running)
    {
        return AppLifecycleState::Stopping;
    }
    if processes
        .iter()
        .any(|process| process.managed && process.status == ProcessStatus::Errored)
    {
        return AppLifecycleState::Degraded;
    }
    if processes
        .iter()
        .any(|process| process.managed && process.status == ProcessStatus::Running)
    {
        return AppLifecycleState::Running;
    }
    AppLifecycleState::Stopped
}

fn should_restart(policy: RestartPolicy, snapshot: &ManagedProcessSnapshot) -> bool {
    match policy {
        RestartPolicy::Never => false,
        RestartPolicy::OnFailure => snapshot.status == ManagedProcessStatus::Failed,
        RestartPolicy::KeepAlive => snapshot.status != ManagedProcessStatus::Running,
    }
}

fn restart_delay(restart_count: u64) -> Duration {
    let multiplier = 2_u32.saturating_pow(restart_count.min(4) as u32);
    (RESTART_BASE_DELAY * multiplier).min(RESTART_MAX_DELAY)
}

fn display_name(id: &str) -> String {
    id.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Criticality, SandboxConfig};
    use axum::{routing::get, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn test_spec(command: String, restart: RestartPolicy) -> ProcessSpec {
        ProcessSpec {
            enabled: true,
            label: None,
            launch: LaunchMode::Eager,
            criticality: Criticality::Required,
            restart,
            sandbox: SandboxConfig::default(),
            command: Some(command),
            command_win32: None,
            cwd: None,
            env: BTreeMap::new(),
            bind: BTreeMap::new(),
            provides: vec![],
            depends_on: vec![],
            endpoints: BTreeMap::new(),
            health_url: None,
            poll_interval_ms: None,
            max_restarts: None,
            restart_window_secs: None,
        }
    }

    fn test_app(spec: ProcessSpec) -> SupervisorApp {
        let mut processes = BTreeMap::new();
        processes.insert("test".to_string(), spec);
        test_app_with_processes(processes)
    }

    fn test_app_with_processes(processes: BTreeMap<String, ProcessSpec>) -> SupervisorApp {
        SupervisorApp::new(ResolvedRuntimeConfig {
            config: RuntimeConfig {
                profile: Some("test".to_string()),
                supervisor_addr: "127.0.0.1:0".to_string(),
                processes,
            },
            base_dir: ".".into(),
        })
    }

    #[tokio::test]
    async fn manual_stop_suppresses_restart() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command Start-Sleep -Seconds 5"
        } else {
            "sleep 5"
        };
        let app = test_app(test_spec(command.to_string(), RestartPolicy::KeepAlive));

        app.start_process("test")
            .await
            .expect("process should start");
        app.stop_process("test").await.expect("process should stop");
        app.enforce_restart_policy().await;

        let status = app.process_status().await;
        assert_eq!(status[0].status, ProcessStatus::Stopped);
    }

    #[tokio::test]
    async fn failed_process_restarts_after_backoff() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command exit 7"
        } else {
            "exit 7"
        };
        let app = test_app(test_spec(command.to_string(), RestartPolicy::OnFailure));

        app.start_process("test")
            .await
            .expect("process should start");
        for _ in 0..12 {
            app.enforce_restart_policy().await;
            if app.process_status().await[0].restart_count > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        panic!("process should restart after backoff");
    }

    #[tokio::test]
    async fn child_logs_are_captured() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command Write-Output hello"
        } else {
            "printf hello"
        };
        let app = test_app(test_spec(command.to_string(), RestartPolicy::Never));

        app.start_process("test")
            .await
            .expect("process should start");
        for _ in 0..10 {
            let logs = app.logs().since(0);
            if logs.iter().any(|entry| entry.line.contains("hello")) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        panic!("child log should be captured");
    }

    #[tokio::test]
    async fn external_process_health_polling_reports_running() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener.local_addr().expect("local addr should exist");
        tokio::spawn(async move {
            let router = Router::new().route("/health", get(|| async { "OK" }));
            let _ = axum::serve(listener, router).await;
        });

        let mut spec = test_spec("unused".to_string(), RestartPolicy::Never);
        spec.launch = LaunchMode::External;
        spec.command = None;
        spec.health_url = Some(format!("http://{addr}/health"));
        let app = test_app(spec);

        let status = app.process_status().await;

        assert_eq!(status[0].status, ProcessStatus::Running);
        assert!(!status[0].managed);
    }

    #[tokio::test]
    async fn process_snapshot_reports_dependency_blockers() {
        let gateway = test_spec("unused".to_string(), RestartPolicy::Never);
        let mut voice_stack = test_spec("unused".to_string(), RestartPolicy::Never);
        voice_stack.depends_on.push("gateway".to_string());
        let mut processes = BTreeMap::new();
        processes.insert("gateway".to_string(), gateway);
        processes.insert("voice_stack".to_string(), voice_stack);
        let app = test_app_with_processes(processes);

        let status = app.process_status().await;
        let voice_stack = status
            .iter()
            .find(|process| process.name == "voice_stack")
            .expect("voice stack should be reported");

        assert_eq!(voice_stack.blocked_by, vec!["gateway"]);
    }

    #[tokio::test]
    async fn external_health_uses_poll_interval_cache() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener.local_addr().expect("local addr should exist");
        let request_count = Arc::new(AtomicUsize::new(0));
        let route_count = request_count.clone();
        tokio::spawn(async move {
            let router = Router::new().route(
                "/health",
                get(move || {
                    let route_count = route_count.clone();
                    async move {
                        route_count.fetch_add(1, Ordering::SeqCst);
                        "OK"
                    }
                }),
            );
            let _ = axum::serve(listener, router).await;
        });

        let mut spec = test_spec("unused".to_string(), RestartPolicy::Never);
        spec.launch = LaunchMode::External;
        spec.command = None;
        spec.health_url = Some(format!("http://{addr}/health"));
        spec.poll_interval_ms = Some(60_000);
        let app = test_app(spec);

        assert_eq!(app.process_status().await[0].status, ProcessStatus::Running);
        assert_eq!(app.process_status().await[0].status, ProcessStatus::Running);
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_app_stops_children_when_gateway_drain_unavailable() {
        let sleep_command = if cfg!(windows) {
            "powershell -NoProfile -Command Start-Sleep -Seconds 5"
        } else {
            "sleep 5"
        };
        let exit_command = if cfg!(windows) {
            "powershell -NoProfile -Command exit 0"
        } else {
            "exit 0"
        };
        let mut gateway = test_spec(exit_command.to_string(), RestartPolicy::OnFailure);
        gateway
            .endpoints
            .insert("grpc".to_string(), "http://127.0.0.1:1".to_string());
        let voice_stack = test_spec(sleep_command.to_string(), RestartPolicy::KeepAlive);
        let mut processes = BTreeMap::new();
        processes.insert("gateway".to_string(), gateway);
        processes.insert("voice_stack".to_string(), voice_stack);
        let app = test_app_with_processes(processes);

        app.start_app().await.expect("app should start");
        app.shutdown_app().await.expect("app should shut down");

        let statuses = app.process_status().await;
        assert!(statuses
            .iter()
            .all(|process| process.status == ProcessStatus::Stopped));
    }

    #[tokio::test]
    async fn restart_exhaustion_enters_errored_state() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command exit 1"
        } else {
            "exit 1"
        };
        let mut spec = test_spec(command.to_string(), RestartPolicy::KeepAlive);
        spec.max_restarts = Some(1);
        spec.restart_window_secs = Some(300);
        let app = test_app(spec);

        app.start_process("test")
            .await
            .expect("process should start");

        for _ in 0..40 {
            app.enforce_restart_policy().await;
            let status = app.process_status().await;
            if status[0].restart_exhausted {
                assert_eq!(status[0].status, ProcessStatus::Errored);
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        panic!("process should reach restart exhaustion");
    }

    #[tokio::test]
    async fn manual_start_resets_restart_exhaustion() {
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command exit 1"
        } else {
            "exit 1"
        };
        let mut spec = test_spec(command.to_string(), RestartPolicy::KeepAlive);
        spec.max_restarts = Some(1);
        spec.restart_window_secs = Some(300);
        let app = test_app(spec);

        app.start_process("test")
            .await
            .expect("process should start");

        for _ in 0..40 {
            app.enforce_restart_policy().await;
            if app.process_status().await[0].restart_exhausted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(
            app.process_status().await[0].restart_exhausted,
            "should be exhausted before reset"
        );

        app.start_process("test")
            .await
            .expect("manual start should succeed");
        let status = app.process_status().await;
        assert!(!status[0].restart_exhausted);
    }
}
