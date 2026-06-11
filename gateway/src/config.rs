//! Gateway-local configuration.
//!
//! [`GatewayConfig`] is loaded from `gateway.toml` and covers server addresses,
//! persona file paths, history depth, silence timer tuning, RAG settings, and
//! the runtime topology section that resolves client endpoint addresses.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub files: FilesConfig,
    pub history: HistoryConfig,
    pub silence: SilenceConfig,
    #[serde(default)]
    pub rag: RagConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub ws_addr: String,
    pub grpc_addr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedClientsConfig {
    pub talker_addr: String,
    pub reasoner_addr: String,
    pub listener_grpc_addr: String,
    pub listener_audio_addr: String,
}

impl Default for ResolvedClientsConfig {
    fn default() -> Self {
        Self {
            talker_addr: "http://127.0.0.1:50053".into(),
            reasoner_addr: "http://127.0.0.1:50054".into(),
            listener_grpc_addr: "http://127.0.0.1:50055".into(),
            listener_audio_addr: "127.0.0.1:50056".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Criticality {
    Required,
    DegradedUsable,
    Optional,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    Eager,
    OnDemand,
    External,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub profile: Option<String>,
    #[serde(default)]
    pub runtimes: BTreeMap<String, RuntimeSpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct RuntimeTopologyFile {
    pub profile: Option<String>,
    #[serde(default)]
    pub processes: BTreeMap<String, RuntimeSpec>,
    #[serde(default)]
    pub profiles: BTreeMap<String, RuntimeProfile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct RuntimeProfile {
    #[serde(default)]
    pub processes: BTreeMap<String, RuntimeSpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RuntimeSpec {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub launch: Option<LaunchMode>,
    #[serde(default)]
    pub criticality: Option<Criticality>,
    #[serde(default)]
    pub endpoints: BTreeMap<String, String>,
    #[serde(default)]
    pub bind: BTreeMap<String, String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, CapabilitySpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CapabilitySpec {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub criticality: Option<Criticality>,
}

fn default_enabled() -> bool {
    true
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            profile: None,
            runtimes: BTreeMap::new(),
        }
    }
}

impl RuntimeConfig {
    pub fn load_topology(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let topology = toml::from_str::<RuntimeTopologyFile>(&content)?;
        Self::from_topology(topology, selected_runtime_profile())
    }

    fn from_topology(
        topology: RuntimeTopologyFile,
        profile_override: Option<String>,
    ) -> anyhow::Result<Self> {
        let selected_profile = profile_override.or(topology.profile.clone());
        let processes = if let Some(profile) = selected_profile.as_deref() {
            match topology.profiles.get(profile) {
                Some(profile_config) => profile_config.processes.clone(),
                None if topology.profiles.is_empty() => topology.processes,
                None => anyhow::bail!("runtime profile '{profile}' is not defined"),
            }
        } else {
            topology.processes
        };
        validate_runtime_topology(&processes)?;
        Ok(Self {
            profile: selected_profile,
            runtimes: processes,
        })
    }

    pub fn endpoint(&self, runtime_id: &str, endpoint_name: &str) -> Option<&str> {
        self.runtimes
            .get(runtime_id)
            .filter(|runtime| runtime.enabled)
            .and_then(|runtime| runtime.endpoints.get(endpoint_name))
            .map(String::as_str)
    }

    pub fn capability_enabled(&self, runtime_id: &str, capability: &str) -> bool {
        let Some(runtime) = self.runtimes.get(runtime_id) else {
            return true;
        };
        if !runtime.enabled {
            return false;
        }
        runtime
            .capabilities
            .get(capability)
            .map(|capability| capability.enabled)
            .unwrap_or(true)
    }

    pub fn runtime_expected(&self, runtime_id: &str) -> bool {
        self.runtimes
            .get(runtime_id)
            .map(|runtime| {
                runtime.enabled
                    && runtime.launch != Some(LaunchMode::External)
                    && runtime.criticality != Some(Criticality::Optional)
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilesConfig {
    pub soul_path: PathBuf,
    pub identity_path: PathBuf,
    pub workspace_root: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryConfig {
    pub max_recent_turns: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SilenceConfig {
    pub soft_prompt_secs: u64,
    pub follow_up_secs: u64,
    pub context_shift_secs: u64,
    /// Gate the proactive silence-triggered LLM dispatch (P4 SilenceExceeded).
    /// Timers themselves keep running — tiers still tick and emit events for
    /// telemetry — but the dispatch is suppressed when this is `false`. See
    /// B10 in TODO.md for the eventual prompt-engineering fix that will make
    /// the LLM optionally silent and let this flag flip back to `true`.
    #[serde(default = "default_silence_enabled")]
    pub enabled: bool,
}

fn default_silence_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct RagConfig {
    pub db_path: PathBuf,
    pub embedding_url: Option<String>,
    pub top_k: usize,
    /// Hard cap on stored memory content length (chars). Defensive only;
    /// real voice utterances never approach this. Prevents pathological
    /// inputs (paste-bombs, adversarial content) from poisoning the index.
    /// `None` = unlimited.
    #[serde(default = "default_max_storage_chars")]
    pub max_storage_chars: Option<usize>,
    /// Per-RetrievalResult.content cap injected into the talker prompt.
    /// `None` = unlimited (let the model's context budget govern). Set to
    /// bound per-turn prompt cost when many retrievals fire.
    #[serde(default)]
    pub max_chars_per_result: Option<usize>,
    /// Per-row cap on the "Recent Context" section of the exported
    /// memory_md (the long-term-persona prefix delivered via UpdatePersona).
    /// `None` = unlimited.
    #[serde(default)]
    pub max_chars_per_md_entry: Option<usize>,
}

fn default_max_storage_chars() -> Option<usize> {
    Some(4096)
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            db_path: "data/kaguya.db".into(),
            embedding_url: None,
            top_k: 10,
            max_storage_chars: default_max_storage_chars(),
            max_chars_per_result: None,
            max_chars_per_md_entry: None,
        }
    }
}

impl GatewayConfig {
    pub fn load(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut config = toml::from_str::<Self>(&content)?;
        if let Some(runtime) = load_runtime_topology()? {
            config.runtime = runtime;
        }
        Ok(config)
    }

    pub fn resolved_clients(&self) -> ResolvedClientsConfig {
        let mut clients = ResolvedClientsConfig::default();
        if let Some(addr) = self.runtime.endpoint("voice_stack", "talker_grpc") {
            clients.talker_addr = addr.to_string();
        }
        if let Some(addr) = self.runtime.endpoint("reasoner", "grpc") {
            clients.reasoner_addr = addr.to_string();
        }
        if let Some(addr) = self.runtime.endpoint("voice_stack", "listener_grpc") {
            clients.listener_grpc_addr = addr.to_string();
        }
        if let Some(addr) = self.runtime.endpoint("voice_stack", "listener_audio") {
            clients.listener_audio_addr = addr.to_string();
        }
        clients
    }

    pub fn listener_enabled(&self) -> bool {
        self.runtime.capability_enabled("voice_stack", "listener")
    }
}

fn load_runtime_topology() -> anyhow::Result<Option<RuntimeConfig>> {
    if let Ok(path) = std::env::var("KAGUYA_RUNTIME_CONFIG") {
        return RuntimeConfig::load_topology(path).map(Some);
    }

    for candidate in [
        "../config/kaguya.runtime.toml",
        "config/kaguya.runtime.toml",
    ] {
        let path = std::path::Path::new(candidate);
        if path.exists() {
            return RuntimeConfig::load_topology(path).map(Some);
        }
    }

    Ok(None)
}

fn selected_runtime_profile() -> Option<String> {
    std::env::var("KAGUYA_RUNTIME_PROFILE")
        .ok()
        .filter(|profile| !profile.trim().is_empty())
}

fn validate_runtime_topology(processes: &BTreeMap<String, RuntimeSpec>) -> anyhow::Result<()> {
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

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                ws_addr: "127.0.0.1:8080".into(),
                grpc_addr: "0.0.0.0:50051".into(),
            },
            files: FilesConfig {
                soul_path: "config/SOUL.md".into(),
                identity_path: "config/IDENTITY.md".into(),
                workspace_root: ".".into(),
            },
            history: HistoryConfig {
                max_recent_turns: 50,
            },
            silence: SilenceConfig {
                soft_prompt_secs: 3,
                follow_up_secs: 8,
                context_shift_secs: 30,
                enabled: true,
            },
            rag: RagConfig::default(),
            runtime: RuntimeConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_clients_resolve_without_runtime_config() {
        let config = GatewayConfig::default();
        let clients = config.resolved_clients();

        assert_eq!(clients.talker_addr, "http://127.0.0.1:50053");
        assert_eq!(clients.reasoner_addr, "http://127.0.0.1:50054");
        assert_eq!(clients.listener_grpc_addr, "http://127.0.0.1:50055");
        assert_eq!(clients.listener_audio_addr, "127.0.0.1:50056");
        assert!(config.listener_enabled());
    }

    #[test]
    fn runtime_endpoints_override_default_clients() {
        let toml = r#"
            [server]
            ws_addr = "127.0.0.1:8080"
            grpc_addr = "0.0.0.0:50051"

            [files]
            soul_path = "SOUL.md"
            identity_path = "IDENTITY.md"
            workspace_root = "."

            [history]
            max_recent_turns = 50

            [silence]
            soft_prompt_secs = 3
            follow_up_secs = 8
            context_shift_secs = 30

            [runtime]
            profile = "test"

            [runtime.runtimes.voice_stack]
            enabled = true
            criticality = "required"
            provides = ["talker", "listener"]

            [runtime.runtimes.voice_stack.endpoints]
            talker_grpc = "http://runtime-talker"
            listener_grpc = "http://runtime-listener"
            listener_audio = "runtime-audio:1"

            [runtime.runtimes.reasoner]
            enabled = true
            criticality = "degraded_usable"

            [runtime.runtimes.reasoner.endpoints]
            grpc = "http://runtime-reasoner"
        "#;

        let config: GatewayConfig = toml::from_str(toml).expect("config should parse");
        let clients = config.resolved_clients();

        assert_eq!(clients.talker_addr, "http://runtime-talker");
        assert_eq!(clients.reasoner_addr, "http://runtime-reasoner");
        assert_eq!(clients.listener_grpc_addr, "http://runtime-listener");
        assert_eq!(clients.listener_audio_addr, "runtime-audio:1");
    }

    #[test]
    fn gateway_config_parses_without_clients_block() {
        let toml = r#"
            [server]
            ws_addr = "127.0.0.1:8080"
            grpc_addr = "0.0.0.0:50051"

            [files]
            soul_path = "SOUL.md"
            identity_path = "IDENTITY.md"
            workspace_root = "."

            [history]
            max_recent_turns = 50

            [silence]
            soft_prompt_secs = 3
            follow_up_secs = 8
            context_shift_secs = 30
        "#;

        let config: GatewayConfig = toml::from_str(toml).expect("config should parse");

        assert_eq!(
            config.resolved_clients().talker_addr,
            "http://127.0.0.1:50053"
        );
    }

    #[test]
    fn disabled_listener_capability_is_not_part_of_runtime_profile() {
        let mut config = GatewayConfig::default();
        let mut voice_stack = RuntimeSpec {
            enabled: true,
            launch: None,
            criticality: None,
            endpoints: BTreeMap::new(),
            bind: BTreeMap::new(),
            provides: vec!["talker".into(), "listener".into()],
            capabilities: BTreeMap::new(),
        };
        voice_stack.capabilities.insert(
            "listener".into(),
            CapabilitySpec {
                enabled: false,
                criticality: Some(Criticality::Optional),
            },
        );
        config
            .runtime
            .runtimes
            .insert("voice_stack".into(), voice_stack);

        assert!(!config.listener_enabled());
    }

    #[test]
    fn runtime_expected_follows_enabled_and_optional_criticality() {
        let toml = r#"
            [runtimes.voice_stack]
            enabled = true
            criticality = "required"

            [runtimes.reasoner]
            enabled = true
            criticality = "optional"
        "#;

        let runtime: RuntimeConfig = toml::from_str(toml).expect("runtime config should parse");

        assert!(runtime.runtime_expected("voice_stack"));
        assert!(!runtime.runtime_expected("reasoner"));
        assert!(!runtime.runtime_expected("missing"));
    }

    #[test]
    fn runtime_topology_selects_named_profile() {
        let toml = r#"
            profile = "app"

            [profiles.app.processes.voice_stack]
            enabled = true
            launch = "eager"
            criticality = "required"

            [profiles.app.processes.voice_stack.endpoints]
            talker_grpc = "http://managed-talker"

            [profiles.dev_standalone.processes.voice_stack]
            enabled = true
            launch = "external"
            criticality = "degraded_usable"

            [profiles.dev_standalone.processes.voice_stack.endpoints]
            talker_grpc = "http://standalone-talker"
        "#;
        let topology = toml::from_str::<RuntimeTopologyFile>(toml).expect("topology parses");
        let runtime = RuntimeConfig::from_topology(topology, Some("dev_standalone".to_string()))
            .expect("profile resolves");

        assert_eq!(runtime.profile.as_deref(), Some("dev_standalone"));
        assert_eq!(
            runtime.endpoint("voice_stack", "talker_grpc"),
            Some("http://standalone-talker")
        );
        assert!(!runtime.runtime_expected("voice_stack"));
    }

    #[test]
    fn runtime_topology_rejects_bind_endpoint_port_mismatch() {
        let toml = r#"
            [processes.voice_stack]

            [processes.voice_stack.bind]
            talker_grpc = "0.0.0.0:50054"

            [processes.voice_stack.endpoints]
            talker_grpc = "http://127.0.0.1:50053"
        "#;
        let topology = toml::from_str::<RuntimeTopologyFile>(toml).expect("topology parses");
        let err = RuntimeConfig::from_topology(topology, None).expect_err("mismatch should fail");

        assert!(err.to_string().contains("port mismatch"));
    }
}
