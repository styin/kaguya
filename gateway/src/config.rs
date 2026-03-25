use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub clients: ClientsConfig,
    pub files: FilesConfig,
    pub history: HistoryConfig,
    pub silence: SilenceConfig,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilesConfig {
    pub soul_path: PathBuf,
    pub identity_path: PathBuf,
    pub memory_path: PathBuf,
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
}

impl GatewayConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
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
            },
            files: FilesConfig {
                soul_path: "config/SOUL.md".into(),
                identity_path: "config/IDENTITY.md".into(),
                memory_path: "config/MEMORY.md".into(),
                workspace_root: ".".into(),
            },
            history: HistoryConfig {
                max_recent_turns: 50,
            },
            silence: SilenceConfig {
                soft_prompt_secs: 3,
                follow_up_secs: 8,
                context_shift_secs: 30,
            },
        }
    }
}