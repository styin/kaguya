//! Incremental Embedder — background task, only processes NEW memories.

use crate::rag::store::RagStore;
use std::sync::Arc;
use std::time::Duration;
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

    pub fn wake(&self) { self.notify.notify_one(); }

    pub async fn run(&self) {
        loop {
            self.notify.notified().await;
            loop {
                let pending = self.store.unembedded_ids().await;
                if pending.is_empty() { break; }
                debug!("{} memories need embedding", pending.len());
                for (id, content) in &pending {
                    match self.embed(content).await {
                        Ok(vec) => {
                            if let Err(e) = self.store.store_embedding(id, &vec, "local").await {
                                warn!("store embedding {id}: {e}");
                            }
                        }
                        Err(e) => { warn!("embed failed: {e}"); break; }
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    /// Call local embedding model (/v1/embeddings OpenAI-compatible).
    pub async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        #[derive(serde::Serialize)]
        struct Req<'a> { input: &'a str, model: &'a str }
        #[derive(serde::Deserialize)]
        struct Resp { data: Vec<EmbData> }
        #[derive(serde::Deserialize)]
        struct EmbData { embedding: Vec<f32> }

        let resp: Resp = self.client
            .post(format!("{}/v1/embeddings", self.base_url))
            .json(&Req { input: text, model: "local" })
            .send().await?.json().await?;
        resp.data.into_iter().next()
            .map(|d| d.embedding)
            .ok_or_else(|| anyhow::anyhow!("empty embedding response"))
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() { return 0.0; }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}