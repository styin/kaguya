//! Reciprocal Rank Fusion (RRF) — Combines rankings from multiple sources
//!
//! RRF is parameter-free and outperforms simple score normalization.
//! Score = Σ 1/(k + rank_i) for each source where the item appears.
//! k=60 is the standard constant (Cormack et al., 2009).

use std::collections::HashMap;

use crate::proto;

const RRF_K: f64 = 60.0;

#[derive(Debug)]
pub struct RankedItem {
    pub id: String,
    pub content: String,
    pub source: String,
    pub score: f64,
}

/// Fuse multiple ranked lists into a single ranking using RRF
pub fn reciprocal_rank_fusion(sources: Vec<(&str, Vec<RankedItem>)>) -> Vec<proto::RetrievalResult> {
    let mut scores: HashMap<String, (f64, String, String)> = HashMap::new(); // id → (score, content, best_source)

    for (source_name, items) in sources {
        for (rank, item) in items.iter().enumerate() {
            let rrf_score = 1.0 / (RRF_K + rank as f64 + 1.0);
            let entry = scores.entry(item.id.clone())
                .or_insert((0.0, item.content.clone(), source_name.to_string()));
            entry.0 += rrf_score;
        }
    }

    let mut results: Vec<_> = scores.into_iter()
        .map(|(id, (score, content, source))| proto::RetrievalResult {
            id,
            content,
            source,
            score: score as f32,
        })
        .collect();

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results
}