//! Conversation History Management
//!
//! In-memory storage of recent conversation turns, including user inputs, assistant responses, and tool results.

use crate::instance::InstanceIdentity;
use crate::proto;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct History {
    instance_id: String,
    session_id: String,
    turns: Arc<RwLock<Vec<proto::ChatMessage>>>,
    max_recent: usize,
}

impl History {
    pub fn new(max_recent: usize, instance_context: &InstanceIdentity) -> Self {
        Self {
            instance_id: instance_context.instance_id.clone(),
            session_id: instance_context.default_session_id.clone(),
            turns: Arc::new(RwLock::new(Vec::new())),
            max_recent,
        }
    }

    pub fn matches_instance_session(&self, instance_id: &str, session_id: &str) -> bool {
        self.instance_id == instance_id && self.session_id == session_id
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_handle_matches_one_instance_and_session() {
        let instance_context = InstanceIdentity {
            schema_version: 1,
            instance_id: uuid::Uuid::new_v4().to_string(),
            default_session_id: uuid::Uuid::new_v4().to_string(),
        };
        let history = History::new(10, &instance_context);
        assert!(history.matches_instance_session(
            &instance_context.instance_id,
            &instance_context.default_session_id
        ));
        assert!(!history.matches_instance_session(
            &uuid::Uuid::new_v4().to_string(),
            &instance_context.default_session_id
        ));
        assert!(!history.matches_instance_session(
            &instance_context.instance_id,
            &uuid::Uuid::new_v4().to_string()
        ));
    }
}
