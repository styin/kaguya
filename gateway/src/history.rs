//! 对话历史 — 内存中的 rolling log。
//!
//! Spec §4.2: "Short-term conversation history. In-memory state in Gateway."
//! 与 MEMORY.md 是两个独立概念：history 是短期，memory 是长期。

use tokio::sync::RwLock;
use crate::types::{Turn, TurnRole};

pub struct History {
    turns: RwLock<Vec<Turn>>,
    max_recent: usize,
}

impl History {
    pub fn new(max_recent: usize) -> Self {
        Self {
            turns: RwLock::new(Vec::new()),
            max_recent,
        }
    }

    pub async fn append_user(&self, text: &str) {
        self.push(TurnRole::User, text, false).await;
    }

    pub async fn append_assistant(&self, text: &str) {
        self.push(TurnRole::Assistant, text, false).await;
    }

    /// 被打断的回复 — 仅记录已说出的部分
    pub async fn append_assistant_partial(&self, spoken: &str) {
        if !spoken.is_empty() {
            self.push(TurnRole::Assistant, spoken, true).await;
        }
    }

    pub async fn append_tool_result(&self, tool: &str, result: &str) {
        self.push(TurnRole::Tool, &format!("[{tool}] {result}"), false).await;
    }

    pub async fn append_reasoner_result(&self, task_id: &str, result: &str) {
        self.push(TurnRole::Reasoner, &format!("[{task_id}] {result}"), false).await;
    }

    /// 获取最近的历史（给 context package）
    pub async fn recent(&self) -> Vec<Turn> {
        let t = self.turns.read().await;
        let start = t.len().saturating_sub(self.max_recent);
        t[start..].to_vec()
    }

    /// 获取最后一条用户输入
    pub async fn last_user_input(&self) -> Option<String> {
        self.turns
            .read()
            .await
            .iter()
            .rev()
            .find(|t| t.role == TurnRole::User)
            .map(|t| t.content.clone())
    }

    async fn push(&self, role: TurnRole, content: &str, interrupted: bool) {
        let mut t = self.turns.write().await;
        t.push(Turn {
            role,
            content: content.to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            was_interrupted: interrupted,
        });
        // 简单截断。Phase 2: 背景 LLM 做历史摘要压缩。
        if t.len() > self.max_recent * 2 {
            let excess = t.len().saturating_sub(self.max_recent);
            if excess > 0 {
                t.drain(0..excess);
            }
        }
    }
}