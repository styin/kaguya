//! Runtime topology configuration for the Supervisor.
//!
//! Parses `kaguya.runtime.toml` into a [`RuntimeConfig`] that describes the
//! full process graph: which processes to launch, their restart policies,
//! bind/endpoint port mappings, dependency ordering, and health-check URLs.
//!
//! The shared deployment selector supplies an absolute runtime file and its
//! selected profile. Managed children reload that same authoritative selection.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::deployment::{resolve_path, DeploymentSelection};
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
#[serde(deny_unknown_fields)]
struct RuntimeProfile {
    #[serde(default)]
    pub processes: BTreeMap<String, ProcessSpec>,
}

/// Declarative specification for a single process in the runtime topology.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
    pub capabilities: BTreeMap<String, CapabilitySpec>,
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

/// Gateway interprets these overrides; Supervisor validates their shape as
/// part of the shared deployment file.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySpec {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub criticality: Option<Criticality>,
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
    pub fn load_selected(selection: &DeploymentSelection) -> anyhow::Result<ResolvedRuntimeConfig> {
        let content = std::fs::read_to_string(&selection.runtime_config)?;
        let mut config = RuntimeConfigFile::parse(&content, selection.profile.clone())?;
        let base_dir = selection
            .runtime_config
            .parent()
            .ok_or_else(|| anyhow::anyhow!("selected runtime configuration has no parent"))?
            .to_path_buf();
        if !base_dir.is_absolute() {
            anyhow::bail!("selected runtime configuration must be absolute");
        }
        let runtime_path = selection.runtime_config.to_str().ok_or_else(|| {
            anyhow::anyhow!("runtime configuration path cannot be represented in child environment")
        })?;
        for process in config.processes.values_mut() {
            if let Some(cwd) = &process.cwd {
                process.cwd = Some(resolve_path(&base_dir, cwd)?);
            }
            // Selection is authoritative even when process.env tries to replace
            // it, including case aliases on Windows. Production discovery
            // rejects an empty inherited profile.
            process.env.retain(|key, _| {
                !key.eq_ignore_ascii_case("KAGUYA_RUNTIME_CONFIG")
                    && !key.eq_ignore_ascii_case("KAGUYA_RUNTIME_PROFILE")
            });
            process
                .env
                .insert("KAGUYA_RUNTIME_CONFIG".into(), runtime_path.into());
            if let Some(profile) = &selection.profile {
                process
                    .env
                    .insert("KAGUYA_RUNTIME_PROFILE".into(), profile.clone());
            }
        }
        Ok(ResolvedRuntimeConfig { config, base_dir })
    }

    /// Pin child startup scope after read-only manifest validation. These values
    /// override both inherited environment and per-process overrides.
    pub fn set_instance_session(&mut self, scope: kaguya_protocol::session_scope::SessionScope) {
        let (instance_id, session_id) = (
            scope.instance_id().to_string(),
            scope.session_id().to_string(),
        );
        for process in self.processes.values_mut() {
            process.env.retain(|key, _| {
                !key.eq_ignore_ascii_case("KAGUYA_INSTANCE_ID")
                    && !key.eq_ignore_ascii_case("KAGUYA_SESSION_ID")
            });
            process
                .env
                .insert("KAGUYA_INSTANCE_ID".into(), instance_id.clone());
            process
                .env
                .insert("KAGUYA_SESSION_ID".into(), session_id.clone());
        }
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
        let file = toml::from_str::<RuntimeConfigFile>(content).map_err(|error| {
            crate::deployment::toml_error("Supervisor runtime topology", content, &error)
        })?;
        let selected_profile = profile_override.or(file.profile.clone());
        let mut processes = if let Some(profile) = selected_profile.as_deref() {
            match file.profiles.get(profile) {
                Some(profile_config) => profile_config.processes.clone(),
                None => anyhow::bail!("runtime profile '{profile}' is not defined"),
            }
        } else {
            if !file.profiles.is_empty() {
                anyhow::bail!("runtime profile must be selected when named profiles are defined");
            }
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
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    #[serde(default = "sandbox_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub backend: SandboxBackendKind,
    #[serde(default)]
    pub mode: SandboxModeKind,
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
                    "runtime process '{process_id}' bind.{endpoint_name} has no explicit port"
                );
            };
            let Some(connect_port) = endpoint_port(connect_addr) else {
                anyhow::bail!(
                    "runtime process '{process_id}' endpoints.{endpoint_name} has no explicit port"
                );
            };
            if bind_port != connect_port {
                anyhow::bail!(
                    "runtime process '{process_id}' bind/endpoints port mismatch for '{endpoint_name}'"
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
    fn runtime_capability_overrides_are_validated_with_process_settings() {
        let config = RuntimeConfigFile::parse(
            "[processes.voice_stack.capabilities.listener]\nenabled = false\ncriticality = 'optional'\n",
            None,
        ).unwrap();
        assert!(!config.processes["voice_stack"].capabilities["listener"].enabled);
        for content in [
            "[processes.voice_stack]\nstale_setting = true\n",
            "[processes.voice_stack.capabilities.listener]\nstale_setting = true\n",
            "[sandbox]\nworkspace_root = '../old-workspace'\n",
        ] {
            assert!(RuntimeConfigFile::parse(content, None).is_err());
        }
    }

    #[test]
    fn selected_deployment_is_absolute_and_authoritative_for_children() {
        let root = std::env::temp_dir().join(format!(
            "kaguya selected config {} with spaces",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        let config_path = root.join("deployment.toml");
        std::fs::write(root.join("gateway settings.toml"), "").unwrap();
        std::fs::write(
            &config_path,
            r#"
            profile = "app"
            gateway_settings = "gateway settings.toml"
            [paths]
            mode = "portable"
            portable_root = "instance data"
            [profiles.app.processes.gateway]
            cwd = "gateway work"
            [profiles.app.processes.gateway.env]
            KAGUYA_RUNTIME_CONFIG = "wrong.toml"
            KAGUYA_RUNTIME_PROFILE = "wrong"
            [profiles.standalone.processes.gateway]
            cwd = "scratch/../standalone work"
            [profiles.standalone.processes.gateway.env]
            KAGUYA_RUNTIME_CONFIG = "wrong.toml"
            KAGUYA_RUNTIME_PROFILE = "wrong"
            kaguya_runtime_config = "wrong-lowercase.toml"
            Kaguya_Runtime_Config = "wrong-mixed-case.toml"
            kaguya_runtime_profile = "wrong-lowercase"
            Kaguya_Runtime_Profile = "wrong-mixed-case"
            CUSTOM = "retained"
            "#,
        )
        .unwrap();
        let selection = DeploymentSelection::load(&config_path, Some("standalone".into())).unwrap();
        let resolved = RuntimeConfig::load_selected(&selection).unwrap();
        let gateway = &resolved.config.processes["gateway"];
        let selected_root = selection.runtime_config.parent().unwrap();
        assert_eq!(resolved.base_dir, selected_root);
        assert_eq!(resolved.config.profile.as_deref(), Some("standalone"));
        assert_eq!(gateway.cwd, Some(selected_root.join("standalone work")));
        assert_eq!(
            gateway.resolved_env()["KAGUYA_RUNTIME_CONFIG"],
            selection.runtime_config.to_str().unwrap()
        );
        assert_eq!(
            gateway.resolved_env()["KAGUYA_RUNTIME_PROFILE"],
            "standalone"
        );
        let child_env = gateway.resolved_env();
        assert_eq!(child_env["CUSTOM"], "retained");
        for name in ["KAGUYA_RUNTIME_CONFIG", "KAGUYA_RUNTIME_PROFILE"] {
            assert_eq!(
                child_env
                    .keys()
                    .filter(|key| key.eq_ignore_ascii_case(name))
                    .count(),
                1,
                "a case alias must not override the selected {name} in a Windows child"
            );
        }
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd")
                .args([
                    "/D",
                    "/C",
                    "echo %KAGUYA_RUNTIME_CONFIG%&echo %KAGUYA_RUNTIME_PROFILE%",
                ])
                .envs(&child_env)
                .output()
                .unwrap();
            assert!(output.status.success());
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert_eq!(
                stdout.lines().collect::<Vec<_>>(),
                vec![selection.runtime_config.to_str().unwrap(), "standalone"]
            );
        }

        std::fs::write(
            &config_path,
            r#"
            gateway_settings = "gateway settings.toml"
            [paths]
            mode = "portable"
            portable_root = "instance data"
            [processes.gateway.env]
            KAGUYA_RUNTIME_PROFILE = "wrong"
            kaguya_runtime_profile = "wrong-lowercase"
            Kaguya_Runtime_Profile = "wrong-mixed-case"
            "#,
        )
        .unwrap();
        let no_profile = DeploymentSelection::load(&config_path, None).unwrap();
        let resolved = RuntimeConfig::load_selected(&no_profile).unwrap();
        assert!(!resolved.config.processes["gateway"]
            .resolved_env()
            .keys()
            .any(|key| key.eq_ignore_ascii_case("KAGUYA_RUNTIME_PROFILE")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_parse_errors_do_not_echo_configured_values() {
        let error =
            RuntimeConfigFile::parse("[processes.gateway]\nenabled = 'never-print-this'", None)
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid Supervisor runtime topology"));
        assert!(!error.to_string().contains("never-print-this"));
    }

    #[cfg(windows)]
    #[test]
    fn selected_child_paths_reject_windows_drive_relative_and_root_relative_forms() {
        let root = std::env::temp_dir().join(format!(
            "kaguya child path selection {} with spaces",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("gateway.toml"), "").unwrap();
        let runtime = root.join("deployment.toml");
        for cwd in [r"C:relative", r"\root-relative"] {
            std::fs::write(
                &runtime,
                format!(
                    "gateway_settings = 'gateway.toml'\n[paths]\nmode = 'portable'\nportable_root = 'data'\n[processes.gateway]\ncwd = '{cwd}'\n"
                ),
            )
            .unwrap();
            let selection = DeploymentSelection::load(&runtime, None).unwrap();
            assert!(RuntimeConfig::load_selected(&selection).is_err());
        }
        assert!(!root.join("data").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_runtime_process_graph() {
        let toml = r#"
            profile = "test"
            supervisor_addr = "127.0.0.1:3001"

            [profiles.test.processes.gateway]
            enabled = true
            launch = "eager"
            criticality = "required"
            restart = "on_failure"
            command = "cargo run"
            command_win32 = "cargo run"
            cwd = "../gateway"
            provides = ["gateway"]

            [profiles.test.processes.gateway.endpoints]
            grpc = "http://127.0.0.1:50051"

            [profiles.test.processes.llm_server]
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
    fn named_profile_never_falls_back_to_top_level_processes() {
        let content = "profile = 'missing'\n[processes.gateway]\n";
        assert!(RuntimeConfigFile::parse(content, None).is_err());
        assert!(RuntimeConfigFile::parse("[processes.gateway]\n", Some("missing".into())).is_err());
        assert!(RuntimeConfigFile::parse("[profiles.app.processes.gateway]\n", None).is_err());
    }

    #[test]
    fn parses_supervisor_owned_sandbox_provider() {
        let toml = r#"
            supervisor_addr = "127.0.0.1:3001"

            [sandbox]
            enabled = true
            backend = "docker"
            mode = "hosted"
            pool_size = 2
        "#;

        let config = RuntimeConfigFile::parse(toml, None).expect("config should parse");

        assert_eq!(config.sandbox.backend, SandboxBackendKind::Docker);
        assert_eq!(config.sandbox.mode, SandboxModeKind::Hosted);
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

    #[test]
    fn endpoint_validation_errors_do_not_echo_address_values() {
        for (bind, endpoint) in [
            ("secret-bind-without-port", "http://host:50053"),
            ("0.0.0.0:50053", "https://never-print-this@host"),
            ("0.0.0.0:50053", "https://never-print-this@host:50054"),
        ] {
            let content = format!(
                "[processes.voice_stack.bind]\ntalker_grpc = '{bind}'\n[processes.voice_stack.endpoints]\ntalker_grpc = '{endpoint}'\n"
            );
            let error = RuntimeConfigFile::parse(&content, None)
                .unwrap_err()
                .to_string();
            assert!(
                !error.contains(bind),
                "bind address appeared in error: {error}"
            );
            assert!(
                !error.contains(endpoint),
                "endpoint address appeared in error: {error}"
            );
            assert!(!error.contains("never-print-this"));
        }
    }

    #[test]
    fn manifest_scope_overrides_process_values_and_windows_case_aliases() {
        let mut config = RuntimeConfigFile::parse(
            r#"
            [processes.gateway]
            command = "gateway"
            [processes.gateway.env]
            KAGUYA_INSTANCE_ID = "wrong"
            kaguya_session_id = "wrong-case"
            CUSTOM = "retained"
            [processes.voice_stack]
            command = "voice"
            [processes.voice_stack.env]
            kaguya_instance_id = "wrong-case"
            KAGUYA_SESSION_ID = "wrong"
        "#,
            None,
        )
        .unwrap();
        let (instance_id, session_id) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        config.set_instance_session(
            kaguya_protocol::session_scope::SessionScope::parse(&instance_id, &session_id).unwrap(),
        );
        for process in config.processes.values() {
            let env = process.resolved_env();
            assert_eq!(env["KAGUYA_INSTANCE_ID"], instance_id);
            assert_eq!(env["KAGUYA_SESSION_ID"], session_id);
            assert!(!env.contains_key("kaguya_instance_id"));
            assert!(!env.contains_key("kaguya_session_id"));
        }
        assert_eq!(
            config.processes["gateway"].resolved_env()["CUSTOM"],
            "retained"
        );
    }
}
