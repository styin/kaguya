//! RAG Engine — Hybrid Retrieval-Augmented Generation
//!
//! Replaces the old keyword-trigger MEMORY.md with:
//!   1. BM25 full-text search (SQLite FTS5)
//!   2. Incremental vector similarity (local embedding model)
//!   3. LSP code symbol search (ripgrep fallback)
//!   4. Reciprocal Rank Fusion ranking
//!
//! The Gateway is still the sole owner of all memory. The Talker receives
//! retrieval results via TalkerContext.retrieval_results — it never queries
//! the store directly.

pub mod embedder;
pub mod lsp;
pub mod ranker;
pub mod retriever;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use store::{MemoryEntry, MemoryType, RagStore};
use retriever::HybridRetriever;
use embedder::Embedder;
use lsp::LspClient;

use crate::proto;

/// Top-level RAG engine, owned by main.rs
pub struct RagEngine {
    pub store: Arc<RagStore>,
    pub retriever: HybridRetriever,
    pub embedder: Option<Arc<Embedder>>,
    lsp: Option<Arc<LspClient>>,
}

impl RagEngine {
    pub fn new(
        db_path: impl AsRef<std::path::Path>,
        workspace_root: PathBuf,
        embedding_url: Option<String>,
    ) -> anyhow::Result<Self> {
        let store = Arc::new(RagStore::open(db_path)?);

        let embedder = embedding_url.map(|url| {
            Arc::new(Embedder::new(url, Arc::clone(&store)))
        });

        let lsp = Some(Arc::new(LspClient::new(workspace_root)));

        let retriever = HybridRetriever::new(
            Arc::clone(&store),
            embedder.clone(),
            lsp.clone(),
            10, // top-K
        );

        info!("RAG engine initialized");
        Ok(Self { store, retriever, embedder, lsp })
    }

    /// Retrieve context for a user query (called during context assembly)
    pub async fn retrieve(&self, query: &str) -> Vec<proto::RetrievalResult> {
        self.retriever.retrieve(query).await
    }

    /// Evaluate a turn for memory-worthy content and store
    /// Replaces the old keyword-trigger approach with LLM-assisted extraction
    pub async fn evaluate_and_store(
        &self,
        user_input: &str,
        assistant_response: &str,
        turn_id: &str,
    ) {
        // Phase 1: Enhanced keyword triggers + pattern matching
        // Phase 2: Replace with lightweight LLM classification call
        let extractions = Self::extract_memories(user_input, assistant_response);

        for (content, mem_type) in extractions {
            let entry = MemoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                content,
                memory_type: mem_type,
                source: turn_id.to_string(),
                created_at: chrono::Utc::now().timestamp_millis(),
                embedding: None,
            };
            if let Err(e) = self.store.insert_memory(&entry).await {
                tracing::error!("Failed to store memory: {e}");
            }
        }

        // Signal embedder to process new entries
        if let Some(emb) = &self.embedder {
            emb.wake();
        }
    }

    /// Pattern-based memory extraction (Phase 1)
    fn extract_memories(user_input: &str, assistant_response: &str) -> Vec<(String, MemoryType)> {
        let mut results = Vec::new();
        let lower = user_input.to_lowercase();

        // Preference detection
        let pref_triggers = ["i like", "i prefer", "i hate", "i want",
                             "我喜欢", "我讨厌", "我偏好", "我习惯"];
        if pref_triggers.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("User preference: {}", &user_input[..user_input.len().min(200)]),
                MemoryType::Preference,
            ));
        }

        // Fact/identity detection
        let fact_triggers = ["my name is", "i work on", "i'm working on", "remember that",
                             "我叫", "我在做", "记住", "别忘了", "don't forget"];
        if fact_triggers.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("User stated: {} → Noted: {}",
                    &user_input[..user_input.len().min(150)],
                    &assistant_response[..assistant_response.len().min(200)]),
                MemoryType::Fact,
            ));
        }

        // Project detection
        let project_triggers = ["project", "repo", "codebase", "pipeline",
                                "项目", "仓库", "代码库"];
        if project_triggers.iter().any(|t| lower.contains(t)) {
            results.push((
                format!("Project context: {}", &user_input[..user_input.len().min(200)]),
                MemoryType::Project,
            ));
        }

        // Always store a conversation summary for non-trivial exchanges
        if user_input.len() > 20 && assistant_response.len() > 20 {
            results.push((
                format!("Q: {} → A: {}",
                    &user_input[..user_input.len().min(100)],
                    &assistant_response[..assistant_response.len().min(150)]),
                MemoryType::Conversation,
            ));
        }

        results
    }

    /// Export as markdown (backward compat with PersonaConfig.memory_md)
    pub async fn export_memory_md(&self) -> String {
        self.store.export_as_markdown().await
    }
}