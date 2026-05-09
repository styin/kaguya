//! Hybrid Retriever — BM25 + optional vector search.
//! LSP is stale; not included in active retrieval path.

use std::sync::Arc;
use tracing::debug;

use crate::proto;
use crate::rag::embedder::{cosine_similarity, Embedder};
use crate::rag::ranker::{self, RankedItem};
use crate::rag::store::RagStore;
use crate::rag::truncate_chars;

pub struct HybridRetriever {
    store: Arc<RagStore>,
    embedder: Option<Arc<Embedder>>,
    top_k: usize,
    /// Per-result content cap applied after RRF fusion. `None` = unlimited.
    /// The store always holds full content; this only governs what the
    /// Talker sees per turn.
    max_chars_per_result: Option<usize>,
}

impl HybridRetriever {
    pub fn new(
        store: Arc<RagStore>,
        embedder: Option<Arc<Embedder>>,
        top_k: usize,
        max_chars_per_result: Option<usize>,
    ) -> Self {
        Self {
            store,
            embedder,
            top_k,
            max_chars_per_result,
        }
    }

    pub async fn retrieve(&self, query: &str) -> Vec<proto::RetrievalResult> {
        if query.is_empty() { return vec![]; }

        let mut sources: Vec<(&str, Vec<RankedItem>)> = Vec::new();

        // ── BM25 via FTS5 (always available, fast) ──
        let bm25 = self.store.search_bm25(query, self.top_k * 2).await;
        if !bm25.is_empty() {
            debug!("BM25: {} hits", bm25.len());
            sources.push(("bm25", bm25.into_iter().map(|r| RankedItem {
                id: r.id, content: r.content,
            }).collect()));
        }

        // ── Vector similarity (optional, for semantic matching) ──
        if let Some(embedder) = &self.embedder {
            if let Ok(qvec) = embedder.embed(query).await {
                let all = self.store.all_embeddings().await;
                let mut scored: Vec<(String, f32)> = all.iter()
                    .map(|(id, v)| (id.clone(), cosine_similarity(&qvec, v)))
                    .collect();
                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(self.top_k * 2);

                if !scored.is_empty() {
                    debug!("Vector: {} hits", scored.len());
                    let mut items = Vec::new();
                    for (id, _) in scored {
                        if let Some(entry) = self.store.get_memory(&id).await {
                            items.push(RankedItem { id, content: entry.content });
                        }
                    }
                    sources.push(("vector", items));
                }
            }
        }

        let mut fused = ranker::reciprocal_rank_fusion(sources);
        fused.truncate(self.top_k);

        // Apply per-result content cap, if configured. Done at output time
        // so the store keeps full fidelity — only the prompt-injected slice
        // is bounded.
        if let Some(cap) = self.max_chars_per_result {
            for r in fused.iter_mut() {
                if r.content.chars().count() > cap {
                    r.content = truncate_chars(&r.content, cap).to_string();
                }
            }
        }

        debug!(
            "Hybrid retrieval: {} final results for '{}'",
            fused.len(),
            query.chars().take(50).collect::<String>()
        );
        fused
    }
}
