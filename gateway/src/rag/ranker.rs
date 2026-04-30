//! Reciprocal Rank Fusion (RRF) — merges rankings from multiple retrieval sources.
//! k=60 per Cormack et al., 2009.

use std::collections::HashMap;
use crate::proto;

const RRF_K: f64 = 60.0;

pub struct RankedItem {
    pub id: String,
    pub content: String,
}

pub fn reciprocal_rank_fusion(
    sources: Vec<(&str, Vec<RankedItem>)>,
) -> Vec<proto::RetrievalResult> {
    let mut scores: HashMap<String, (f64, String, String)> = HashMap::new();

    for (source_name, items) in sources {
        for (rank, item) in items.iter().enumerate() {
            let rrf = 1.0 / (RRF_K + rank as f64 + 1.0);
            let entry = scores.entry(item.id.clone())
                .or_insert((0.0, item.content.clone(), source_name.to_string()));
            entry.0 += rrf;
        }
    }

    let mut results: Vec<_> = scores.into_iter()
        .map(|(id, (score, content, source))| proto::RetrievalResult {
            id, content, source, score: score as f32,
        })
        .collect();
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results
}