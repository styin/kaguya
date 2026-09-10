//! Runtime topology configuration for the Supervisor.
//!
//! Parses `kaguya.runtime.toml` into a [`RuntimeConfig`] that describes the
//! full process graph: which processes to launch, their restart policies,
//! bind/endpoint port mappings, dependency ordering, and health-check URLs.
//!
//! Supports named profiles (e.g. `app`, `dev_standalone`) selected by the
//! `KAGUYA_RUNTIME_PROFILE` environment variable or the `profile` field in
//! the config file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::process::{ManagedProcessRestartPolicy, ManagedProcessSpec};

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RuntimeConfig {
    pub profile: Option<String>,
    #[serde(default = "default_http_addr")]
    pub supervisor_addr: String,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub processes: BTreeMap<String, ProcessSpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct RuntimeConfigFile {
    pub profile: Option<String>,
    #[serde(default = "default_http_addr")]
    pub supervisor_addr: String,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub processes: BTreeMap<String, ProcessSpec>,
    #[serde(default)]
    pub profiles: BTreeMap<String, RuntimeProfile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct RuntimeProfile {
    #[serde(default)]
    pub processes: BTreeMap<String, ProcessSpec>,
}

/// Declarative specification for a single process in the runtime topology.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProcessSpec {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub label: Option<String>,
    #[serde(default)]
    pub launch: LaunchMode,
    #[serde(default)]
    pub criticality: Criticality,
    #[serde(default)]
    pub restart: RestartPolicy,
    pub command: Option<String>,
    pub command_win32: Option<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub bind: BTreeMap<String, String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub endpoints: BTreeMap<String, String>,
    pub health_url: Option<String>,
    pub poll_interval_ms: Option<u64>,
    #[serde(default)]
    pub max_restarts: Option<u32>,
    #[serde(default)]
    pub restart_window_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    Eager,
    OnDemand,
    External,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Criticality {
    Required,
    DegradedUsable,
    Optional,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    KeepAlive,
}

impl RuntimeConfig {
    pub fn load_discover() -> anyhow::Result<ResolvedRuntimeConfig> {
        if let Ok(path) = std::env::var("KAGUYA_RUNTIME_CONFIG") {
            return Self::load(path);
        }

        for candidate in [
            "config/kaguya.runtime.toml",
            "../config/kaguya.runtime.toml",
        ] {
            let path = PathBuf::from(candidate);
            if path.exists() {
                return Self::load(path);
            }
        }

        anyhow::bail!("could not find config/kaguya.runtime.toml");
    }

    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<ResolvedRuntimeConfig> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        let mut config = RuntimeConfigFile::parse(&content, selected_profile())?;
        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        for process in config.processes.values_mut() {
            if let Some(cwd) = &process.cwd {
                if cwd.is_relative() {
                    process.cwd = Some(base_dir.join(cwd));
                }
            }
        }
        if config.sandbox.workspace_root.is_relative() {
            config.sandbox.workspace_root = base_dir.join(&config.sandbox.workspace_root);
        }
        Ok(ResolvedRuntimeConfig { config, base_dir })
    }

    pub fn gateway_grpc_endpoint(&self) -> Option<&str> {
        self.processes
            .get("gateway")
            .and_then(|gateway| gateway.endpoints.get("grpc"))
            .map(String::as_str)
    }
}

impl RuntimeConfigFile {
    fn parse(content: &str, profile_override: Option<String>) -> anyhow::Result<RuntimeConfig> {
        let file = toml::from_str::<RuntimeConfigFile>(content)?;
        let selected_profile = profile_override.or(file.profile.clone());
        let mut processes = if let Some(profile) = selected_profile.as_deref() {
            match file.profiles.get(profile) {
                Some(profile_config) => profile_config.processes.clone(),
                None if file.profiles.is_empty() => file.processes,
                None => anyhow::bail!("runtime profile '{profile}' is not defined"),
            }
        } else {
            file.processes
        };
        validate_runtime_topology(&processes)?;
        if let Some(gateway) = processes.get_mut("gateway") {
            let supervisor_url = if file.supervisor_addr.contains("://") {
                file.supervisor_addr.clone()
            } else {
                format!("http://{}", file.supervisor_addr)
            };
            gateway
                .env
                .entry("KAGUYA_SUPERVISOR_URL".into())
                .or_insert(supervisor_url);
        }

        Ok(RuntimeConfig {
            profile: selected_profile,
            supervisor_addr: file.supervisor_addr,
            sandbox: file.sandbox,
            processes,
        })
    }
}

fn selected_profile() -> Option<String> {
    std::env::var("KAGUYA_RUNTIME_PROFILE")
        .ok()
        .filter(|profile| !profile.trim().is_empty())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRuntimeConfig {
    pub config: RuntimeConfig,
    pub base_dir: PathBuf,
}

impl ProcessSpec {
    pub fn label(&self, id: &str) -> String {
        self.label.clone().unwrap_or_else(|| display_name(id))
    }

    pub fn is_managed(&self) -> bool {
        self.enabled && self.launch != LaunchMode::External
    }

    pub fn is_eager_managed(&self) -> bool {
        self.is_managed() && self.launch == LaunchMode::Eager
    }

    pub fn process_spec(&self, id: &str) -> anyhow::Result<ManagedProcessSpec> {
        let Some(command) = self.command.clone() else {
            anyhow::bail!("managed process '{id}' is missing command");
        };
        Ok(ManagedProcessSpec::new(id.to_string(), command)
            .with_command_win32(self.command_win32.clone())
            .with_cwd(self.cwd.clone())
            .with_env(self.resolved_env())
            .with_restart_policy(self.restart.into()))
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms.unwrap_or(5_000))
    }

    pub fn resolved_env(&self) -> BTreeMap<String, String> {
        let mut env = self.env.clone();
        env.extend(env_from_bind(&self.bind));
        env
    }
}

impl From<RestartPolicy> for ManagedProcessRestartPolicy {
    fn from(value: RestartPolicy) -> Self {
        match value {
            RestartPolicy::Never => Self::Never,
            RestartPolicy::OnFailure => Self::OnFailure,
            RestartPolicy::KeepAlive => Self::KeepAlive,
        }
    }
}

impl Default for LaunchMode {
    fn default() -> Self {
        Self::External
    }
}

impl Default for Criticality {
    fn default() -> Self {
        Self::Optional
    }
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::Never
    }
}

fn default_enabled() -> bool {
    true
}

// ── Supervisor-owned code-execution sandbox provider ───────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackendKind {
    #[default]
    Native,
    Docker,
    Bubblewrap,
    JobObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxModeKind {
    #[default]
    SingleUser,
    Hosted,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SandboxConfig {
    #[serde(default = "sandbox_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub backend: SandboxBackendKind,
    #[serde(default)]
    pub mode: SandboxModeKind,
    #[serde(default = "sandbox_workspace_root")]
    pub workspace_root: PathBuf,
    #[serde(default = "sandbox_timeout")]
    pub default_timeout_secs: u64,
    #[serde(default = "sandbox_output")]
    pub max_output_bytes: usize,
    #[serde(default = "sandbox_image")]
    pub image: String,
    #[serde(default)]
    pub pool_size: usize,
    #[serde(default = "sandbox_memory")]
    pub memory_limit_mb: u64,
    #[serde(default = "sandbox_pids")]
    pub pids_limit: u64,
    #[serde(default = "sandbox_cpus")]
    pub cpus: f64,
    #[serde(default)]
    pub network: bool,
    #[serde(default = "sandbox_languages")]
    pub allowed_languages: Vec<String>,
}

fn sandbox_enabled() -> bool {
    true
}

fn sandbox_workspace_root() -> PathBuf {
    PathBuf::from("..")
}

fn sandbox_timeout() -> u64 {
    30
}

fn sandbox_output() -> usize {
    16 * 1024
}

fn sandbox_image() -> String {
    "kaguya-sandbox:latest".into()
}

fn sandbox_memory() -> u64 {
    512
}

fn sandbox_pids() -> u64 {
    128
}

fn sandbox_cpus() -> f64 {
    1.0
}

fn sandbox_languages() -> Vec<String> {
    vec!["python".into(), "node".into(), "bash".into()]
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: sandbox_enabled(),
            backend: SandboxBackendKind::default(),
            mode: SandboxModeKind::default(),
            workspace_root: sandbox_workspace_root(),
            default_timeout_secs: sandbox_timeout(),
            max_output_bytes: sandbox_output(),
            image: sandbox_image(),
            pool_size: 0,
            memory_limit_mb: sandbox_memory(),
            pids_limit: sandbox_pids(),
            cpus: sandbox_cpus(),
            network: false,
            allowed_languages: sandbox_languages(),
        }
    }
}

fn default_http_addr() -> String {
    "127.0.0.1:3001".to_string()
}

fn env_from_bind(bind: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(addr) = bind.get("talker_grpc") {
        env.insert("KAGUYA_TALKER_LISTEN_ADDR".to_string(), addr.clone());
    }
    if let Some(addr) = bind.get("listener_grpc") {
        env.insert("KAGUYA_LISTENER_GRPC_ADDR".to_string(), addr.clone());
    }
    if let Some(addr) = bind.get("listener_audio") {
        let (host, port) = split_host_port(addr);
        env.insert("KAGUYA_LISTENER_AUDIO_ADDR".to_string(), host);
        env.insert("KAGUYA_LISTENER_AUDIO_PORT".to_string(), port);
    }
    env
}

fn split_host_port(addr: &str) -> (String, String) {
    let Some((host, port)) = addr.rsplit_once(':') else {
        return (addr.to_string(), String::new());
    };
    (host.trim_matches(['[', ']']).to_string(), port.to_string())
}

fn validate_runtime_topology(processes: &BTreeMap<String, ProcessSpec>) -> anyhow::Result<()> {
    for (process_id, process) in processes {
        for (endpoint_name, bind_addr) in &process.bind {
            let Some(connect_addr) = process.endpoints.get(endpoint_name) else {
                continue;
            };
            let Some(bind_port) = endpoint_port(bind_addr) else {
                anyhow::bail!(
                    "runtime process '{process_id}' bind.{endpoint_name} has no explicit port: {bind_addr}"
                );
            };
            let Some(connect_port) = endpoint_port(connect_addr) else {
                anyhow::bail!(
                    "runtime process '{process_id}' endpoints.{endpoint_name} has no explicit port: {connect_addr}"
                );
            };
            if bind_port != connect_port {
                anyhow::bail!(
                    "runtime process '{process_id}' bind/endpoints port mismatch for '{endpoint_name}': bind={bind_addr}, endpoint={connect_addr}"
                );
            }
        }
    }
    Ok(())
}

fn endpoint_port(addr: &str) -> Option<String> {
    let after_scheme = addr.split_once("://").map_or(addr, |(_, rest)| rest);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if let Some(rest) = authority.strip_prefix('[') {
        return rest
            .split_once("]:")
            .map(|(_, port)| port.trim().to_string())
            .filter(|port| !port.is_empty());
    }
    authority
        .rsplit_once(':')
        .map(|(_, port)| port.trim().to_string())
        .filter(|port| !port.is_empty())
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

    #[test]
    fn parses_runtime_process_graph() {
        let toml = r#"
            profile = "test"
            supervisor_addr = "127.0.0.1:3001"

            [processes.gateway]
            enabled = true
            launch = "eager"
            criticality = "required"
            restart = "on_failure"
            command = "cargo run"
            command_win32 = "cargo run"
            cwd = "../gateway"
            provides = ["gateway"]

            [processes.gateway.endpoints]
            grpc = "http://127.0.0.1:50051"

            [processes.llm_server]
            launch = "external"
            health_url = "http://localhost:1234/v1/models"
            poll_interval_ms = 5000
        "#;

        let config = RuntimeConfigFile::parse(toml, None).expect("config should parse");

        assert_eq!(config.profile.as_deref(), Some("test"));
        assert_eq!(
            config.gateway_grpc_endpoint(),
            Some("http://127.0.0.1:50051")
        );
        assert!(config.processes["gateway"].is_eager_managed());
        assert!(!config.processes["llm_server"].is_managed());
        assert_eq!(
            config.processes["gateway"]
                .resolved_env()
                .get("KAGUYA_SUPERVISOR_URL")
                .map(String::as_str),
            Some("http://127.0.0.1:3001")
        );
    }

    #[test]
    fn selects_named_runtime_profile() {
        let toml = r#"
            profile = "app"
            supervisor_addr = "127.0.0.1:3001"

            [profiles.app.processes.voice_stack]
            launch = "eager"
            criticality = "required"

            [profiles.dev_standalone.processes.voice_stack]
            launch = "external"
            criticality = "degraded_usable"
        "#;

        let config = RuntimeConfigFile::parse(toml, Some("dev_standalone".to_string()))
            .expect("profile should parse");

        assert_eq!(config.profile.as_deref(), Some("dev_standalone"));
        assert_eq!(
            config.processes["voice_stack"].criticality,
            Criticality::DegradedUsable
        );
        assert!(!config.processes["voice_stack"].is_managed());
    }

    #[test]
    fn parses_supervisor_owned_sandbox_provider() {
        let toml = r#"
            supervisor_addr = "127.0.0.1:3001"

            [sandbox]
            enabled = true
            backend = "docker"
            mode = "hosted"
            workspace_root = "../workspace"
            pool_size = 2
        "#;

        let config = RuntimeConfigFile::parse(toml, None).expect("config should parse");

        assert_eq!(config.sandbox.backend, SandboxBackendKind::Docker);
        assert_eq!(config.sandbox.mode, SandboxModeKind::Hosted);
        assert_eq!(config.sandbox.workspace_root, PathBuf::from("../workspace"));
        assert_eq!(config.sandbox.pool_size, 2);
    }

    #[test]
    fn bind_generates_first_party_runtime_env() {
        let toml = r#"
            [processes.voice_stack]
            command = "python main.py"

            [processes.voice_stack.env]
            KAGUYA_LLM_BASE_URL = "http://localhost:1234"
            KAGUYA_TALKER_LISTEN_ADDR = "stale"

            [processes.voice_stack.bind]
            talker_grpc = "0.0.0.0:50053"
            listener_grpc = "0.0.0.0:50055"
            listener_audio = "0.0.0.0:50056"
        "#;

        let config = RuntimeConfigFile::parse(toml, None).expect("config should parse");
        let env = config.processes["voice_stack"].resolved_env();

        assert_eq!(
            env.get("KAGUYA_TALKER_LISTEN_ADDR").map(String::as_str),
            Some("0.0.0.0:50053")
        );
        assert_eq!(
            env.get("KAGUYA_LISTENER_GRPC_ADDR").map(String::as_str),
            Some("0.0.0.0:50055")
        );
        assert_eq!(
            env.get("KAGUYA_LISTENER_AUDIO_ADDR").map(String::as_str),
            Some("0.0.0.0")
        );
        assert_eq!(
            env.get("KAGUYA_LISTENER_AUDIO_PORT").map(String::as_str),
            Some("50056")
        );
        assert_eq!(
            env.get("KAGUYA_LLM_BASE_URL").map(String::as_str),
            Some("http://localhost:1234")
        );
    }

    #[test]
    fn rejects_bind_endpoint_port_mismatch() {
        let toml = r#"
            [processes.voice_stack]

            [processes.voice_stack.bind]
            talker_grpc = "0.0.0.0:50054"

            [processes.voice_stack.endpoints]
            talker_grpc = "http://127.0.0.1:50053"
        "#;

        let err = RuntimeConfigFile::parse(toml, None).expect_err("mismatch should fail");

        assert!(err.to_string().contains("port mismatch"));
    }
}
