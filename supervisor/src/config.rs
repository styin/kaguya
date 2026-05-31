use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::process::{ManagedProcessRestartPolicy, ManagedProcessSpec};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub profile: Option<String>,
    #[serde(default = "default_http_addr")]
    pub supervisor_addr: String,
    #[serde(default)]
    pub processes: BTreeMap<String, ProcessSpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct RuntimeConfigFile {
    pub profile: Option<String>,
    #[serde(default = "default_http_addr")]
    pub supervisor_addr: String,
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
    #[serde(default)]
    pub sandbox: SandboxConfig,
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SandboxConfig {
    #[serde(default)]
    pub provider: SandboxProvider,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProvider {
    None,
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
        let processes = if let Some(profile) = selected_profile.as_deref() {
            match file.profiles.get(profile) {
                Some(profile_config) => profile_config.processes.clone(),
                None if file.profiles.is_empty() => file.processes,
                None => anyhow::bail!("runtime profile '{profile}' is not defined"),
            }
        } else {
            file.processes
        };
        validate_runtime_topology(&processes)?;

        Ok(RuntimeConfig {
            profile: selected_profile,
            supervisor_addr: file.supervisor_addr,
            processes,
        })
    }
}

fn selected_profile() -> Option<String> {
    std::env::var("KAGUYA_RUNTIME_PROFILE")
        .ok()
        .filter(|profile| !profile.trim().is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            provider: SandboxProvider::None,
            required: false,
        }
    }
}

impl Default for SandboxProvider {
    fn default() -> Self {
        Self::None
    }
}

fn default_enabled() -> bool {
    true
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
            sandbox = { provider = "none", required = false }
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
