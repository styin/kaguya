//! MEMORY.md 管理 — 长期知识库。
//!
//! Spec §4.1: "A plain-text file managed entirely by the Gateway."
//! 启动时读取，缓存在内存中，文件变化时重新加载。
//! 每轮对话结束后评估是否有值得记忆的内容。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::error;

#[derive(Clone)]
pub struct Memory {
    content: Arc<RwLock<String>>,
    path: Arc<RwLock<PathBuf>>,
    dirty: Arc<AtomicBool>,
}

impl Memory {
    pub async fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let p = path.as_ref().to_path_buf();
        let content = tokio::fs::read_to_string(&p).await.unwrap_or_default();
        Ok(Self {
            content: Arc::new(RwLock::new(content)),
            path: Arc::new(RwLock::new(p)),
            dirty: Arc::new(AtomicBool::new(false)),
        })
    }

    pub async fn contents(&self) -> String {
        self.content.read().await.clone()
    }

    pub async fn reload(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        *self.content.write().await = tokio::fs::read_to_string(path).await?;
        Ok(())
    }

    /// Phase 1 简易记忆评估 — 关键词触发追加
    pub async fn evaluate_and_update(&self, user_input: &str, assistant_response: &str) {
        let triggers = [
            "记住", "我叫", "我的名字", "remember", "my name is",
            "我喜欢", "我讨厌", "i like", "i hate", "别忘了", "don't forget",
        ];

        let lower = user_input.to_lowercase();
        if !triggers.iter().any(|t| lower.contains(t)) {
            return;
        }

        let mut content = self.content.write().await;
        let entry = format!(
            "\n- [{}] User: {} → Noted: {}\n",
            chrono::Utc::now().format("%Y-%m-%d %H:%M"),
            user_input.chars().take(100).collect::<String>(),
            assistant_response.chars().take(200).collect::<String>(),
        );
        content.push_str(&entry);

        let path = self.path.read().await.clone();
        if let Err(e) = tokio::fs::write(&path, content.as_str()).await {
            error!("persist MEMORY.md failed: {e}");
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub async fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }
}