//! Incremental Embedder — Only embeds NEW memories, never recomputes
//!
//! Calls a local embedding model via HTTP (e.g., llama.cpp /v1/embeddings
//! or a dedicated sentence-transformers server).
//! Background task processes the queue at idle time.

use crate::rag::store::RagStore;
use std::sync::Arc;
use tokio::sync::Notify;
use tracing::{debug, warn};

pub struct Embedder {
    base_url: String,
    client: reqwest::Client,
    store: Arc<RagStore>,
    notify: Arc<Notify>,
}

impl Embedder {
    pub fn new(base_url: String, store: Arc<RagStore>) -> Self {
        Self {
            base_url,
            client: reqwest::Client::new(),
            store,
            notify: Arc::new(Notify::new()),
        }
    }

    /// Signal that new memories need embedding
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Background loop: process unembedded memories incrementally
    pub async fn run(&self) {
        loop {
            self.notify.notified().await;

            let pending = self.store.unembedded_ids().await;
            if pending.is_empty() {
                continue;
            }

            debug!("{} memories need embedding", pending.len());

            for (id, content) in &pending {
                match self.embed_text(content).await {
                    Ok(vec) => {
                        if let Err(e) = self.store.store_embedding(id, &vec, "local").await {
                            warn!("Failed to store embedding for {id}: {e}");
                        }
                    }
                    Err(e) => {
                        warn!("Embedding failed for {id}: {e}");
                        break; // model might be down, stop trying
                    }
                }
            }
        }
    }

    /// Call local embedding model
    async fn embed_text(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        #[derive(serde::Serialize)]
        struct Req<'a> { input: &'a str, model: &'a str }

        #[derive(serde::Deserialize)]
        struct Resp { data: Vec<EmbData> }
        #[derive(serde::Deserialize)]
        struct EmbData { embedding: Vec<f32> }

        let resp: Resp = self.client
            .post(format!("{}/v1/embeddings", self.base_url))
            .json(&Req { input: text, model: "local" })
            .send().await?
            .json().await?;

        resp.data.into_iter().next()
            .map(|d| d.embedding)
            .ok_or_else(|| anyhow::anyhow!("empty embedding response"))
    }
}

/// Cosine similarity between two vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() { return 0.0; }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { return 0.0; }
    dot / (norm_a * norm_b)
}