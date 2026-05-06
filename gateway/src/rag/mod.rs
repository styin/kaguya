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

use crate::config::RagConfig;
use crate::proto;
use embedder::Embedder;
use retriever::HybridRetriever;
use store::{MemoryEntry, MemoryType, RagStore};

pub struct RagEngine {
    pub store: Arc<RagStore>,
    retriever: HybridRetriever,
    pub embedder: Option<Arc<Embedder>>,
    max_storage_chars: Option<usize>,
    max_chars_per_md_entry: Option<usize>,
}

impl RagEngine {
    pub fn new(config: &RagConfig, _workspace_root: PathBuf) -> anyhow::Result<Self> {
        let store = Arc::new(RagStore::open(&config.db_path)?);
        let embedder = config
            .embedding_url
            .as_ref()
            .map(|url| Arc::new(Embedder::new(url.clone(), Arc::clone(&store))));
        let retriever = HybridRetriever::new(
            Arc::clone(&store),
            embedder.clone(),
            config.top_k,
            config.max_chars_per_result,
        );

        info!(
            "RAG engine initialized (embedder={}, top_k={}, max_storage_chars={:?})",
            embedder.is_some(),
            config.top_k,
            config.max_storage_chars,
        );
        Ok(Self {
            store,
            retriever,
            embedder,
            max_storage_chars: config.max_storage_chars,
            max_chars_per_md_entry: config.max_chars_per_md_entry,
        })
    }

    pub async fn retrieve(&self, query: &str) -> Vec<proto::RetrievalResult> {
        self.retriever.retrieve(query).await
    }

    /// Post-turn memory evaluation. Phase-1 placeholder: keyword triggers
    /// produce typed memory entries. Stores full user/assistant content
    /// (only the defensive `max_storage_chars` cap applies); any output-side
    /// budgeting happens at retrieval / export time, not here. To be
    /// replaced with LLM-based extraction in Phase 2.
    pub async fn evaluate_and_store(
        &self,
        user_input: &str,
        assistant_response: &str,
        turn_id: &str,
    ) {
        let extractions = Self::extract_memories(user_input, assistant_response);
        for (raw_content, mem_type) in extractions {
            let content = match self.max_storage_chars {
                Some(cap) => truncate_chars(&raw_content, cap).to_string(),
                None => raw_content,
            };
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
        let user = user_input.trim();
        let resp = assistant_response.trim();
        let lower = user.to_lowercase();

        let pref = [
            "i like", "i prefer", "i hate", "i want",
            "我喜欢", "我讨厌", "我偏好", "我习惯",
        ];
        if pref.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("User preference: {user}"),
                MemoryType::Preference,
            ));
        }

        let fact = [
            "my name is", "i work on", "i'm working on", "remember that",
            "我叫", "我在做", "记住", "别忘了", "don't forget",
        ];
        if fact.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("User: {user} → Noted: {resp}"),
                MemoryType::Fact,
            ));
        }

        let proj = ["project", "repo", "codebase", "pipeline", "项目", "仓库"];
        if proj.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("Project: {user}"),
                MemoryType::Project,
            ));
        }

        if user.chars().count() > 20 && resp.chars().count() > 20 {
            results.push((
                format!("Q: {user} → A: {resp}"),
                MemoryType::Conversation,
            ));
        }

        results
    }

    pub async fn export_memory_md(&self) -> String {
        self.store
            .export_as_markdown(self.max_chars_per_md_entry)
            .await
    }
}

/// Truncate `s` to at most `max_chars` Unicode characters, never splitting a
/// multi-byte char. Plain `&s[..n]` panics when `n` falls inside a UTF-8
/// sequence, which happens routinely on Chinese / emoji input.
///
/// Public so the retriever and store can use the same helper for output-side
/// caps (`max_chars_per_result`, `max_chars_per_md_entry`) — keeping a single
/// truncation primitive avoids subtle drift between layers.
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> &str {
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
        // Regression: original byte-slice form panicked at byte boundaries
        // mid-multi-byte-char on inputs that hit the trigger keywords.
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

    #[test]
    fn extract_memories_preserves_full_user_input() {
        // Long but realistic voice-utterance length — well below any
        // sensible storage sanity cap. Asserts the full statement survives
        // (no storage-time truncation).
        let user = "I prefer working in Rust over Python because the type \
                    system catches more issues at compile time, especially \
                    for the trading platform I'm migrating off Java";
        let mems = RagEngine::extract_memories(user, "ok");
        let pref = mems
            .iter()
            .find(|(_, t)| *t == MemoryType::Preference)
            .expect("preference category should match 'i prefer'");
        // Full user statement is embedded; nothing chopped.
        assert!(pref.0.contains("trading platform I'm migrating off Java"));
        assert!(pref.0.starts_with("User preference: "));
    }

    #[test]
    fn extract_memories_preserves_chinese_content_in_full() {
        // Same property for Chinese — previously the original code
        // panicked here; the now-not-panicking code must also preserve
        // every char, not just stop short of the byte cap.
        let user = "我喜欢使用 Rust 因为它的类型系统在编译时就能捕获许多在 \
                    Python 中只有运行时才会发现的问题，特别是对于我正在 \
                    从 Java 迁移过来的交易平台来说更为重要。";
        let mems = RagEngine::extract_memories(user, "ok");
        let pref = mems
            .iter()
            .find(|(_, t)| *t == MemoryType::Preference)
            .expect("preference category should match '我喜欢'");
        assert!(pref.0.contains("交易平台"));
        assert!(pref.0.ends_with("更为重要。"));
    }

    #[test]
    fn extract_memories_handles_short_turn_pair() {
        // Short turns shouldn't flood the store with conversation rows —
        // the > 20-char gate filters out greetings.
        let mems = RagEngine::extract_memories("hi", "hello");
        assert!(
            !mems.iter().any(|(_, t)| *t == MemoryType::Conversation),
            "short turns must not produce a Conversation entry"
        );
    }
}
