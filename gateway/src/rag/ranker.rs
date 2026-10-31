//! Reciprocal Rank Fusion (RRF) — merges rankings from multiple retrieval sources.
//! The denominator offset is supplied by Gateway configuration.

use kaguya_protocol::proto;
use std::collections::HashMap;

pub struct RankedItem {
    pub id: String,
    pub content: String,
}

pub fn reciprocal_rank_fusion(
    sources: Vec<(&str, Vec<RankedItem>)>,
    k: f64,
) -> Vec<proto::RetrievalResult> {
    let mut scores: HashMap<String, (f64, String, String)> = HashMap::new();

    for (source_name, items) in sources {
        for (rank, item) in items.iter().enumerate() {
            let rrf = 1.0 / (k + rank as f64 + 1.0);
            let entry = scores.entry(item.id.clone()).or_insert((
                0.0,
                item.content.clone(),
                source_name.to_string(),
            ));
            entry.0 += rrf;
        }
    }

    let mut results: Vec<_> = scores
        .into_iter()
        .map(|(id, (score, content, source))| proto::RetrievalResult {
            id,
            content,
            source,
            score: score as f32,
        })
        .collect();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}
