//! Hybrid Retriever — Orchestrates BM25 + Vector + LSP search
//!
//! Query flow:
//!   1. BM25 via FTS5 → keyword matches
//!   2. Vector similarity (if embedder available) → semantic matches
//!   3. LSP symbol search (if query looks code-related) → precise matches
//!   4. RRF fusion → unified ranked list
//!   5. Top-K → inject into TalkerContext.retrieval_results

use std::sync::Arc;
use tracing::debug;

use crate::proto;
use crate::rag::embedder::{cosine_similarity, Embedder};
use crate::rag::lsp::LspClient;
use crate::rag::ranker::{self, RankedItem};
use crate::rag::store::RagStore;

pub struct HybridRetriever {
    store: Arc<RagStore>,
    embedder: Option<Arc<Embedder>>,
    lsp: Option<Arc<LspClient>>,
    top_k: usize,
}

impl HybridRetriever {
    pub fn new(
        store: Arc<RagStore>,
        embedder: Option<Arc<Embedder>>,
        lsp: Option<Arc<LspClient>>,
        top_k: usize,
    ) -> Self {
        Self { store, embedder, lsp, top_k }
    }

    /// Run hybrid retrieval for a user query
    pub async fn retrieve(&self, query: &str) -> Vec<proto::RetrievalResult> {
        let mut sources: Vec<(&str, Vec<RankedItem>)> = Vec::new();

        // ── 1. BM25 full-text search (always available, fast) ──
        let bm25_results = self.store.search_bm25(query, self.top_k * 2).await;
        if !bm25_results.is_empty() {
            debug!("BM25: {} results", bm25_results.len());
            sources.push(("bm25", bm25_results.into_iter().enumerate().map(|(i, r)| {
                RankedItem {
                    id: r.id,
                    content: r.content,
                    source: "bm25".into(),
                    score: -r.rank, // FTS5 rank is negative (lower = better)
                }
            }).collect()));
        }

        // ── 2. Vector search (if embedder available, for semantic matching) ──
        if let Some(embedder) = &self.embedder {
            if let Ok(query_vec) = embedder.embed_query(query).await {
                let all_embs = self.store.all_embeddings().await;
                let mut scored: Vec<(String, f32)> = all_embs.iter()
                    .map(|(id, vec)| (id.clone(), cosine_similarity(&query_vec, vec)))
                    .collect();
                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(self.top_k * 2);

                if !scored.is_empty() {
                    debug!("Vector: {} results", scored.len());
                    let mut vector_items = Vec::new();
                    for (id, score) in scored {
                        if let Some(entry) = self.store.get_memory(&id).await {
                            vector_items.push(RankedItem {
                                id,
                                content: entry.content,
                                source: "vector".into(),
                                score: score as f64,
                            });
                        }
                    }
                    sources.push(("vector", vector_items));
                }
            }
        }

        // ── 3. LSP search (if query looks code-related) ──
        if let Some(lsp) = &self.lsp {
            if Self::looks_like_code_query(query) {
                let symbols = lsp.search_symbols(query).await;
                if !symbols.is_empty() {
                    debug!("LSP: {} symbols", symbols.len());
                    sources.push(("lsp", symbols.into_iter().enumerate().map(|(i, s)| {
                        RankedItem {
                            id: format!("lsp-{}-{i}", s.location),
                            content: format!("{} ({}) at {}", s.name, s.kind, s.location),
                            source: "lsp".into(),
                            score: 1.0 / (i as f64 + 1.0),
                        }
                    }).collect()));
                }
            }
        }

        // ── 4. RRF fusion ──
        let mut fused = ranker::reciprocal_rank_fusion(sources);
        fused.truncate(self.top_k);

        debug!("Hybrid retrieval: {} final results for '{}'",
            fused.len(), &query[..query.len().min(50)]);

        fused
    }

    /// Heuristic: does this query look code-related?
    fn looks_like_code_query(query: &str) -> bool {
        let code_signals = [
            "function", "class", "struct", "impl", "def ", "fn ",
            "import", "require", "module", ".rs", ".py", ".ts", ".js",
            "error", "bug", "crash", "stack trace", "compile",
            "()", "::", "->", "=>",
        ];
        let lower = query.to_lowercase();
        code_signals.iter().any(|s| lower.contains(s))
    }
}

// Extend Embedder for query embedding
impl Embedder {
    pub async fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.embed_text(text).await
    }
}