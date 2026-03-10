use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// SOUL.md + IDENTITY.md 文件管理。
/// 启动时读取，文件变化时重新加载。
pub struct Persona {
    soul_path: PathBuf,
    identity_path: PathBuf,
    soul: RwLock<String>,
    identity: RwLock<String>,
}

impl Persona {
    pub async fn load(soul_path: PathBuf, identity_path: PathBuf) -> anyhow::Result<Self> {
        let soul = tokio::fs::read_to_string(&soul_path)
            .await
            .unwrap_or_else(|e| {
                warn!("SOUL.md read failed: {e}");
                String::new()
            });
        let identity = tokio::fs::read_to_string(&identity_path)
            .await
            .unwrap_or_else(|e| {
                warn!("IDENTITY.md read failed: {e}");
                String::new()
            });

        info!(
            soul_bytes = soul.len(),
            identity_bytes = identity.len(),
            "Persona files loaded"
        );

        Ok(Self {
            soul_path,
            identity_path,
            soul: RwLock::new(soul),
            identity: RwLock::new(identity),
        })
    }

    pub async fn soul(&self) -> String {
        self.soul.read().await.clone()
    }

    pub async fn identity(&self) -> String {
        self.identity.read().await.clone()
    }

    /// 文件 watcher 触发时调用
    pub async fn reload(&self) {
        if let Ok(s) = tokio::fs::read_to_string(&self.soul_path).await {
            *self.soul.write().await = s;
            info!("SOUL.md reloaded");
        }
        if let Ok(s) = tokio::fs::read_to_string(&self.identity_path).await {
            *self.identity.write().await = s;
            info!("IDENTITY.md reloaded");
        }
    }
}