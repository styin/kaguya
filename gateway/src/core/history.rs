//! Conversation History Management
//!
//! In-memory storage of recent conversation turns, including user inputs, assistant responses, and tool results.

use crate::proto;
use crate::proto::SessionRef;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct History {
    scope: SessionRef,
    turns: Arc<RwLock<Vec<proto::ChatMessage>>>,
    max_recent: usize,
}

impl History {
    pub fn new(max_recent: usize, scope: &SessionRef) -> Self {
        Self {
            scope: scope.clone(),
            turns: Arc::new(RwLock::new(Vec::new())),
            max_recent,
        }
    }

    pub fn matches_session(&self, scope: &SessionRef) -> bool {
        &self.scope == scope
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

    /// Return the configured recent message window for prompt snapshots.
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
        let scope = SessionRef {
            instance_id: Some(proto::InstanceId {
                value: "00000000-0000-4000-8000-000000000001".into(),
            }),
            session_id: Some(proto::SessionId {
                value: "00000000-0000-4000-8000-000000000002".into(),
            }),
        };
        let history = History::new(10, &scope);
        assert!(history.matches_session(&scope));
        let mut other_instance = scope.clone();
        other_instance.instance_id = Some(proto::InstanceId {
            value: "00000000-0000-4000-8000-000000000003".into(),
        });
        assert!(!history.matches_session(&other_instance));
        let mut other_session = scope;
        other_session.session_id = Some(proto::SessionId {
            value: "00000000-0000-4000-8000-000000000004".into(),
        });
        assert!(!history.matches_session(&other_session));
    }
}
