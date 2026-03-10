//! MEMORY.md 管理 — 长期知识库。
//!
//! Spec §4.1: "A plain-text file managed entirely by the Gateway."
//! 启动时读取，缓存在内存中，文件变化时重新加载。
//! 每轮对话结束后评估是否有值得记忆的内容。

use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

pub struct Memory {
    path: PathBuf,
    contents: RwLock<String>,
    /// 标记自上次 UpdatePersona 以来是否有变化
    dirty: RwLock<bool>,
}

impl Memory {
    pub async fn load(path: PathBuf) -> anyhow::Result<Self> {
        let contents = tokio::fs::read_to_string(&path).await.unwrap_or_else(|e| {
            warn!("MEMORY.md read failed: {e}, starting with template");
            "## User Profile\n\n## Project Context\n\n## Recent Context\n".into()
        });
        info!(bytes = contents.len(), "MEMORY.md loaded");
        Ok(Self {
            path,
            contents: RwLock::new(contents),
            dirty: RwLock::new(false),
        })
    }

    /// 获取缓存内容（~0ms，无磁盘 I/O）
    pub async fn contents(&self) -> String {
        self.contents.read().await.clone()
    }

    /// 外部编辑后重新加载
    pub async fn reload(&self) {
        if let Ok(s) = tokio::fs::read_to_string(&self.path).await {
            *self.contents.write().await = s;
            *self.dirty.write().await = true;
            info!("MEMORY.md reloaded (external edit detected)");
        }
    }

    /// 回合后评估：这轮对话中有没有值得记忆的内容？
    /// 如果有，追加到 MEMORY.md 并写盘。
    ///
    /// Phase 1: 规则匹配。Future: 轻量 LLM 分类。
    pub async fn evaluate_and_update(&self, user_input: &str, assistant_response: &str) {
        let facts = extract_facts(user_input, assistant_response);
        if facts.is_empty() {
            return;
        }

        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut contents = self.contents.write().await;

        for fact in &facts {
            let entry = format!("- [{}] {}\n", date, fact);
            // 插入到 "## Recent Context" 段落下方
            if let Some(pos) = contents.find("## Recent Context") {
                let insert = contents[pos..]
                    .find('\n')
                    .map(|p| pos + p + 1)
                    .unwrap_or(contents.len());
                contents.insert_str(insert, &entry);
            } else {
                contents.push_str(&format!("\n## Recent Context\n{}", entry));
            }
        }

        if let Err(e) = tokio::fs::write(&self.path, contents.as_bytes()).await {
            warn!("MEMORY.md write failed: {e}");
        } else {
            *self.dirty.write().await = true;
            debug!("MEMORY.md updated with {} facts", facts.len());
        }
    }

    /// 检查并清除 dirty 标记
    pub async fn take_dirty(&self) -> bool {
        let mut d = self.dirty.write().await;
        let was = *d;
        *d = false;
        was
    }
}

/// 规则匹配提取记忆要点
fn extract_facts(user: &str, _assistant: &str) -> Vec<String> {
    let lower = user.to_lowercase();
    let triggers = [
        "my name is",
        "i prefer",
        "i'm working on",
        "remember that",
        "note that",
        "important:",
    ];
    for t in triggers {
        if lower.contains(t) {
            return vec![user.to_string()];
        }
    }
    Vec::new()
}