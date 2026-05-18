use std::collections::HashMap;

/// Reciprocal Rank Fusion: combines multiple ranked result lists.
///
/// Formula: score(d) = sum(1.0 / (k + rank)) across all lists.
/// Original scores are ignored — only rank position matters.
pub fn reciprocal_rank_fusion(
    result_lists: &[Vec<(i64, f64)>],
    k: i32,
    limit: usize,
) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();

    for list in result_lists {
        for (rank, (chunk_id, _original_score)) in list.iter().enumerate() {
            *scores.entry(*chunk_id).or_default() += 1.0 / (k as f64 + rank as f64 + 1.0);
        }
    }

    let mut fused: Vec<(i64, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(limit);
    fused
}
