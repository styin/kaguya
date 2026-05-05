use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub clients: ClientsConfig,
    pub files: FilesConfig,
    pub history: HistoryConfig,
    pub silence: SilenceConfig,
    #[serde(default)]
    pub rag: RagConfig,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct RagConfig {
    pub db_path: PathBuf,
    pub embedding_url: Option<String>,
    pub top_k: usize,
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            db_path: "data/kaguya.db".into(),
            embedding_url: None,
            top_k: 10,
        }
    }
}

impl GatewayConfig {
    pub fn load(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
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
                listener_grpc_addr: "http://127.0.0.1:50055".into(),
                listener_audio_addr: "127.0.0.1:50056".into(),
            },
            files: FilesConfig {
                soul_path: "config/SOUL.md".into(),
                identity_path: "config/IDENTITY.md".into(),
                workspace_root: ".".into(),
            },
            history: HistoryConfig { max_recent_turns: 50 },
            silence: SilenceConfig {
                soft_prompt_secs: 3,
                follow_up_secs: 8,
                context_shift_secs: 30,
            },
            rag: RagConfig::default(),
        }
    }
}
