//! Hybrid RAG Engine — BM25 (FTS5) + incremental vector + RRF.
//! LSP integration is stale/deferred — see lsp.rs.

pub mod embedder;
pub mod lsp;
pub mod ranker;
pub mod retriever;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use crate::proto;
use store::{MemoryEntry, MemoryType, RagStore};
use retriever::HybridRetriever;
use embedder::Embedder;

pub struct RagEngine {
    pub store: Arc<RagStore>,
    retriever: HybridRetriever,
    pub embedder: Option<Arc<Embedder>>,
}

impl RagEngine {
    pub fn new(
        db_path: impl AsRef<std::path::Path>,
        _workspace_root: PathBuf,
        embedding_url: Option<String>,
        top_k: usize,
    ) -> anyhow::Result<Self> {
        let store = Arc::new(RagStore::open(db_path)?);
        let embedder = embedding_url.map(|url| Arc::new(Embedder::new(url, Arc::clone(&store))));
        let retriever = HybridRetriever::new(Arc::clone(&store), embedder.clone(), top_k);

        info!("RAG engine initialized (embedder={})", embedder.is_some());
        Ok(Self { store, retriever, embedder })
    }

    pub async fn retrieve(&self, query: &str) -> Vec<proto::RetrievalResult> {
        self.retriever.retrieve(query).await
    }

    /// Post-turn memory evaluation. Replaces old keyword-trigger MEMORY.md logic.
    pub async fn evaluate_and_store(
        &self,
        user_input: &str,
        assistant_response: &str,
        turn_id: &str,
    ) {
        let extractions = Self::extract_memories(user_input, assistant_response);
        for (content, mem_type) in extractions {
            let entry = MemoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                content,
                memory_type: mem_type,
                source: turn_id.to_string(),
                created_at: chrono::Utc::now().timestamp_millis(),
            };
            if let Err(e) = self.store.insert_memory(&entry).await {
                tracing::error!("Failed to store memory: {e}");
            }
        }
        if let Some(emb) = &self.embedder {
            emb.wake();
        }
    }

    fn extract_memories(user_input: &str, assistant_response: &str) -> Vec<(String, MemoryType)> {
        let mut results = Vec::new();
        let lower = user_input.to_lowercase();

        let pref = ["i like", "i prefer", "i hate", "i want",
                     "我喜欢", "我讨厌", "我偏好", "我习惯"];
        if pref.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("User preference: {}", truncate_chars(user_input, 200)),
                MemoryType::Preference,
            ));
        }

        let fact = ["my name is", "i work on", "i'm working on", "remember that",
                     "我叫", "我在做", "记住", "别忘了", "don't forget"];
        if fact.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("User: {} → Noted: {}",
                    truncate_chars(user_input, 150),
                    truncate_chars(assistant_response, 200)),
                MemoryType::Fact,
            ));
        }

        let proj = ["project", "repo", "codebase", "pipeline", "项目", "仓库"];
        if proj.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("Project: {}", truncate_chars(user_input, 200)),
                MemoryType::Project,
            ));
        }

        if user_input.len() > 20 && assistant_response.len() > 20 {
            results.push((
                format!("Q: {} → A: {}",
                    truncate_chars(user_input, 100),
                    truncate_chars(assistant_response, 150)),
                MemoryType::Conversation,
            ));
        }

        results
    }

    pub async fn export_memory_md(&self) -> String {
        self.store.export_as_markdown().await
    }
}

// Truncates `s` to at most `max_chars` Unicode characters, never splitting a
// multi-byte char. Plain `&s[..n]` panics when `n` falls inside a UTF-8
// sequence, which happens routinely on the Chinese trigger keywords above.
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_handles_multibyte_boundaries() {
        // 6 chars × 3 bytes = 18 bytes
        let s = "我喜欢喝咖啡";
        assert_eq!(truncate_chars(s, 3), "我喜欢");
        assert_eq!(truncate_chars(s, 100), s);
        assert_eq!(truncate_chars(s, 0), "");
    }

    #[test]
    fn extract_memories_does_not_panic_on_long_chinese_input() {
        let long_pref = "我喜欢".repeat(100); // 300 chars, 900 bytes
        let _ = RagEngine::extract_memories(&long_pref, "ok");
        let long_fact = "记住".to_string() + &"测试内容".repeat(80);
        let _ = RagEngine::extract_memories(&long_fact, &"回应".repeat(80));
        let long_proj = "项目".to_string() + &"详情".repeat(120);
        let _ = RagEngine::extract_memories(&long_proj, "ack");
        let long_conv_user = "u".to_string() + &"问".repeat(60);
        let long_conv_resp = "a".to_string() + &"答".repeat(60);
        let _ = RagEngine::extract_memories(&long_conv_user, &long_conv_resp);
    }
}
