//! 对话历史 — 内存中的 rolling log。
//!
//! Spec §4.2: "Short-term conversation history. In-memory state in Gateway."
//! 与 MEMORY.md 是两个独立概念：history 是短期，memory 是长期。

use std::sync::Arc;
use tokio::sync::RwLock;
use crate::proto;

#[derive(Clone)]
pub struct History {
    turns: Arc<RwLock<Vec<proto::ChatMessage>>>,
    max_recent: usize,
}

impl History {
    pub fn new(max_recent: usize) -> Self {
        Self {
            turns: Arc::new(RwLock::new(Vec::new())),
            max_recent,
        }
    }

    pub async fn append_user(&self, text: &str) {
        self.push(proto::Role::User, text, "").await;
    }

    pub async fn append_assistant(&self, text: &str) {
        self.push(proto::Role::Assistant, text, "").await;
    }

    pub async fn append_assistant_partial(&self, spoken: &str) {
        if !spoken.is_empty() {
            self.push(proto::Role::Assistant, spoken, "").await;
        }
    }

    pub async fn append_tool_result(&self, tool_name: &str, result: &str) {
        self.push(proto::Role::Tool, result, tool_name).await;
    }

    /// 获取最近 N 轮 — 直接作为 TalkerContext.history，零转换
    pub async fn recent(&self) -> Vec<proto::ChatMessage> {
        let t = self.turns.read().await;
        let start = t.len().saturating_sub(self.max_recent);
        t[start..].to_vec()
    }

    pub async fn last_user_input(&self) -> Option<String> {
        self.turns
            .read()
            .await
            .iter()
            .rev()
            .find(|t| t.role == proto::Role::User as i32)
            .map(|t| t.content.clone())
    }

    async fn push(&self, role: proto::Role, content: &str, name: &str) {
        let mut t = self.turns.write().await;
        t.push(proto::ChatMessage {
            role: role.into(),
            content: content.to_string(),
            name: name.to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        });
        if t.len() > self.max_recent * 2 {
            let excess = t.len().saturating_sub(self.max_recent);
            t.drain(0..excess);
        }
    }
}