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
    pub provides: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub endpoints: BTreeMap<String, String>,
    pub health_url: Option<String>,
    pub poll_interval_ms: Option<u64>,
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
        let mut config = toml::from_str::<RuntimeConfig>(&content)?;
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
            .with_env(self.env.clone())
            .with_restart_policy(self.restart.into()))
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms.unwrap_or(5_000))
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

        let config: RuntimeConfig = toml::from_str(toml).expect("config should parse");

        assert_eq!(config.profile.as_deref(), Some("test"));
        assert_eq!(
            config.gateway_grpc_endpoint(),
            Some("http://127.0.0.1:50051")
        );
        assert!(config.processes["gateway"].is_eager_managed());
        assert!(!config.processes["llm_server"].is_managed());
    }
}
