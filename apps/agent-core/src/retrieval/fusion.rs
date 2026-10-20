use serde_json::{Value, json};
use std::collections::HashMap;

struct Candidate {
    chunk_id: String,
    positions: HashMap<&'static str, usize>,
    score: f64,
    exact_target: bool,
    domain: String,
    collation_index: usize,
}

/// Merge complete authorized channels before limiting; a soft domain preference is a bounded tie signal.
/// 完整合并已授权通道后再限制候选，软领域偏好仅增加有界排序信号。
pub fn apply(arguments: &Value) -> Value {
    let collation_order: HashMap<_, _> = arguments["collationOrder"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, id)| id.as_str().map(|id| (id, index)))
        .collect();
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    // Equal lexical and vector weights stay. On the frozen evaluation dataset lowering the vector weight to 0.6
    // neither restored the literal first hit nor kept paraphrase recall (diagnostic nDCG 0.83 -> 0.71), because in
    // additive RRF a candidate matched by two channels always outranks a single-channel first hit.
    // 词法与向量保持等权：固定评估集上把向量权重降到 0.6 既没找回字面首位，也损失了改写召回
    // （诊断 nDCG 0.83 → 0.71）；加法式 RRF 中两通道命中总会压过单通道首位，权重不是这里的杠杆。
    for (channel, weight) in [
        ("lexical", 1.0),
        ("vector", 1.0),
        ("symbol", 2.0),
        ("path", 1.5),
    ] {
        for (rank, row) in arguments[channel]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let chunk_id = row["chunkId"].as_str().unwrap_or("").to_owned();
            let candidate = candidates
                .entry(chunk_id.clone())
                .or_insert_with(|| Candidate {
                    collation_index: collation_order
                        .get(chunk_id.as_str())
                        .copied()
                        .unwrap_or(usize::MAX),
                    chunk_id,
                    positions: HashMap::new(),
                    score: 0.0,
                    exact_target: false,
                    domain: String::new(),
                });
            candidate.positions.insert(channel, rank);
            candidate.score += weight / (61 + rank) as f64;
            candidate.exact_target |= row["exactTargetMatch"] == true;
            candidate.domain = row["actualDomain"].as_str().unwrap_or("").to_owned();
        }
    }
    let preferred_domain = arguments["preferredDomain"].as_str().unwrap_or("");
    let mut candidates: Vec<_> = candidates.into_values().collect();
    for candidate in &mut candidates {
        if !preferred_domain.is_empty() && candidate.domain == preferred_domain {
            candidate.score += 0.05 / 61.0;
        }
    }
    candidates.sort_by(|left, right| {
        right
            .exact_target
            .cmp(&left.exact_target)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.collation_index.cmp(&right.collation_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates.truncate(arguments["limit"].as_u64().unwrap_or(8) as usize);
    json!(candidates.into_iter().map(|candidate| json!({"chunkId": candidate.chunk_id, "score": candidate.score, "positions": candidate.positions})).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_channel_scores_follow_the_original_icu_collation_receipt() {
        let result = apply(&json!({"lexical":[{"chunkId":"A"},{"chunkId":"a"}],
            "vector":[{"chunkId":"a"},{"chunkId":"A"}],"collationOrder":["a","A"],"limit":2}));
        assert_eq!(result[0]["score"], result[1]["score"]);
        assert_eq!(result[0]["chunkId"], "a");
        assert_eq!(result[1]["chunkId"], "A");
    }

    #[test]
    fn a_two_channel_hit_outranks_a_single_channel_first_hit() {
        // Additive RRF favours the candidate both channels agree on, even against a lexical first hit.
        // 加法式 RRF 偏向两个通道都命中的候选，即使对手是词法首位。
        let result = apply(&json!({"lexical":[{"chunkId":"first"},{"chunkId":"both"}],
            "vector":[{"chunkId":"both"}],"limit":2}));
        assert_eq!(result[0]["chunkId"], "both");
    }
}
