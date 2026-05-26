use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::lifecycle::ManagedProcessSpec;

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub clients: ClientsConfig,
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

#[derive(Debug, Clone, Deserialize)]
pub struct ClientsConfig {
    pub talker_addr: String,
    pub reasoner_addr: String,
    pub listener_grpc_addr: String,
    pub listener_audio_addr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedClientsConfig {
    pub talker_addr: String,
    pub reasoner_addr: String,
    pub listener_grpc_addr: String,
    pub listener_audio_addr: String,
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
pub enum Activation {
    Eager,
    OnDemand,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRestartPolicy {
    Never,
    OnFailure,
    KeepAlive,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FallbackPolicy {
    None,
    Stub,
    Limited,
    External,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub profile: Option<String>,
    #[serde(default)]
    pub manage_processes: bool,
    #[serde(default)]
    pub runtimes: BTreeMap<String, RuntimeSpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RuntimeSpec {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub managed: bool,
    #[serde(default)]
    pub criticality: Option<Criticality>,
    #[serde(default)]
    pub activation: Option<Activation>,
    #[serde(default)]
    pub restart: Option<RuntimeRestartPolicy>,
    #[serde(default)]
    pub fallback: Option<FallbackPolicy>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub command_win32: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub health_url: Option<String>,
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub endpoints: BTreeMap<String, String>,
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
    #[serde(default)]
    pub fallback: Option<FallbackPolicy>,
}

fn default_enabled() -> bool {
    true
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            profile: None,
            manage_processes: false,
            runtimes: BTreeMap::new(),
        }
    }
}

impl RuntimeConfig {
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

    pub fn eager_managed_process_specs(&self) -> anyhow::Result<Vec<ManagedProcessSpec>> {
        let mut specs = Vec::new();
        for (id, runtime) in &self.runtimes {
            if !runtime.enabled || !runtime.managed {
                continue;
            }
            if runtime.activation != Some(Activation::Eager) {
                continue;
            }
            let Some(command) = runtime.command.clone() else {
                anyhow::bail!("managed eager runtime '{id}' is missing command");
            };
            specs.push(
                ManagedProcessSpec::new(id.clone(), command)
                    .with_command_win32(runtime.command_win32.clone())
                    .with_cwd(runtime.cwd.clone())
                    .with_env(runtime.env.clone()),
            );
        }
        Ok(specs)
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
        Ok(toml::from_str(&content)?)
    }

    pub fn resolved_clients(&self) -> ResolvedClientsConfig {
        ResolvedClientsConfig {
            talker_addr: self
                .runtime
                .endpoint("voice_stack", "talker_grpc")
                .unwrap_or(&self.clients.talker_addr)
                .to_string(),
            reasoner_addr: self
                .runtime
                .endpoint("reasoner", "grpc")
                .unwrap_or(&self.clients.reasoner_addr)
                .to_string(),
            listener_grpc_addr: self
                .runtime
                .endpoint("voice_stack", "listener_grpc")
                .unwrap_or(&self.clients.listener_grpc_addr)
                .to_string(),
            listener_audio_addr: self
                .runtime
                .endpoint("voice_stack", "listener_audio")
                .unwrap_or(&self.clients.listener_audio_addr)
                .to_string(),
        }
    }

    pub fn listener_enabled(&self) -> bool {
        self.runtime.capability_enabled("voice_stack", "listener")
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                ws_addr: "127.0.0.1:8080".into(),
                grpc_addr: "0.0.0.0:50051".into(),
            },
            clients: ClientsConfig {
                talker_addr: "http://127.0.0.1:50053".into(),
                reasoner_addr: "http://127.0.0.1:50054".into(),
                listener_grpc_addr: "http://127.0.0.1:50055".into(),
                listener_audio_addr: "127.0.0.1:50056".into(),
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
    fn legacy_clients_resolve_without_runtime_config() {
        let config = GatewayConfig::default();
        let clients = config.resolved_clients();

        assert_eq!(clients.talker_addr, "http://127.0.0.1:50053");
        assert_eq!(clients.reasoner_addr, "http://127.0.0.1:50054");
        assert_eq!(clients.listener_grpc_addr, "http://127.0.0.1:50055");
        assert_eq!(clients.listener_audio_addr, "127.0.0.1:50056");
        assert!(config.listener_enabled());
    }

    #[test]
    fn runtime_endpoints_override_legacy_clients() {
        let toml = r#"
            [server]
            ws_addr = "127.0.0.1:8080"
            grpc_addr = "0.0.0.0:50051"

            [clients]
            talker_addr = "http://legacy-talker"
            reasoner_addr = "http://legacy-reasoner"
            listener_grpc_addr = "http://legacy-listener"
            listener_audio_addr = "legacy-audio:1"

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
            activation = "eager"
            restart = "keep_alive"
            provides = ["talker", "listener"]

            [runtime.runtimes.voice_stack.endpoints]
            talker_grpc = "http://runtime-talker"
            listener_grpc = "http://runtime-listener"
            listener_audio = "runtime-audio:1"

            [runtime.runtimes.reasoner]
            enabled = true
            criticality = "degraded_usable"
            activation = "on_demand"
            fallback = "stub"

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
    fn disabled_listener_capability_is_not_part_of_runtime_profile() {
        let mut config = GatewayConfig::default();
        let mut voice_stack = RuntimeSpec {
            enabled: true,
            managed: false,
            criticality: None,
            activation: None,
            restart: None,
            fallback: None,
            command: None,
            command_win32: None,
            cwd: None,
            health_url: None,
            poll_interval_ms: None,
            env: BTreeMap::new(),
            endpoints: BTreeMap::new(),
            provides: vec!["talker".into(), "listener".into()],
            capabilities: BTreeMap::new(),
        };
        voice_stack.capabilities.insert(
            "listener".into(),
            CapabilitySpec {
                enabled: false,
                criticality: Some(Criticality::Optional),
                fallback: Some(FallbackPolicy::Limited),
            },
        );
        config
            .runtime
            .runtimes
            .insert("voice_stack".into(), voice_stack);

        assert!(!config.listener_enabled());
    }

    #[test]
    fn eager_managed_process_specs_include_enabled_eager_runtimes_only() {
        let toml = r#"
            manage_processes = true

            [runtimes.voice_stack]
            enabled = true
            managed = true
            activation = "eager"
            command = "python main.py"
            command_win32 = ".venv\\Scripts\\python.exe main.py"
            cwd = "../talker"

            [runtimes.voice_stack.env]
            KAGUYA_LOG_LEVEL = "INFO"

            [runtimes.reasoner]
            enabled = true
            managed = true
            activation = "on_demand"
            command = "npm run start"

            [runtimes.disabled]
            enabled = false
            managed = true
            activation = "eager"
            command = "disabled"
        "#;

        let runtime: RuntimeConfig = toml::from_str(toml).expect("runtime config should parse");
        let specs = runtime
            .eager_managed_process_specs()
            .expect("runtime process specs should resolve");

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name(), "voice_stack");
    }

    #[test]
    fn eager_managed_runtime_requires_command() {
        let toml = r#"
            [runtimes.voice_stack]
            enabled = true
            managed = true
            activation = "eager"
        "#;

        let runtime: RuntimeConfig = toml::from_str(toml).expect("runtime config should parse");

        assert!(runtime.eager_managed_process_specs().is_err());
    }
}
