use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct Persona {
    soul: Arc<RwLock<String>>,
    identity: Arc<RwLock<String>>,
}

impl Persona {
    pub async fn load(
        soul_path: impl AsRef<Path>,
        identity_path: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        let soul = tokio::fs::read_to_string(soul_path).await.unwrap_or_default();
        let identity = tokio::fs::read_to_string(identity_path).await.unwrap_or_default();
        Ok(Self {
            soul: Arc::new(RwLock::new(soul)),
            identity: Arc::new(RwLock::new(identity)),
        })
    }

    pub async fn soul(&self) -> String {
        self.soul.read().await.clone()
    }

    pub async fn identity(&self) -> String {
        self.identity.read().await.clone()
    }

    pub async fn reload_soul(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        *self.soul.write().await = tokio::fs::read_to_string(path).await?;
        Ok(())
    }

    pub async fn reload_identity(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        *self.identity.write().await = tokio::fs::read_to_string(path).await?;
        Ok(())
    }
}