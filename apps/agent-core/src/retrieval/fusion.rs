use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

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

/// Reranking observes a diverse subset; original evidence and source coordinates stay unchanged.
/// 重排只观察具有来源多样性的候选子集，原证据与来源坐标保持不变。
pub struct RerankPlan {
    pub indices: Vec<usize>,
    pub texts: Vec<String>,
    pub windowed_inputs: usize,
    pub reason: &'static str,
}

pub fn rerank_plan(items: &[Value], input: &Value, maximum: usize) -> RerankPlan {
    let skipped = |reason| RerankPlan { indices: vec![], texts: vec![], windowed_inputs: 0, reason };
    let query = input["query"].as_str().unwrap_or("").trim();
    if items.len() < 2 || maximum < 2 || query.is_empty() {
        return skipped("insufficient-candidates");
    }
    if items.iter().any(|item| {
        let mut target = item.clone();
        target["exactPath"] = json!(item["locator"]["relativePath"].as_str()
            .or_else(|| item["locator"]["path"].as_str()).unwrap_or("").replace('\\', "/").to_lowercase());
        let intent = json!({"path":input["path"].as_str().unwrap_or("").replace('\\', "/"),"symbol":input["symbol"]});
        item["exactTargetMatch"] == true || super::query::exact_target(&target, &intent)
    }) {
        return skipped("exact-path-or-symbol");
    }
    if query.chars().all(|character| character.is_ascii_alphanumeric() || "_./\\:-".contains(character))
        && !matches!(input["taskType"].as_str(), Some("complex" | "research")) {
        return skipped("literal-lookup");
    }
    let query_tokens = crate::text_metrics::estimate_tokens(query);
    if query_tokens >= 448 {
        return skipped("query-exceeds-reranker-window");
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut sources = HashMap::new();
    let mut seen_chunks = HashSet::new();
    let mut seen_texts = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let excerpt = item["excerpt"].as_str().unwrap_or("");
        if excerpt.trim().is_empty() { continue; }
        let source = json!([item["scopeKey"], item["sourceId"]]).to_string();
        let normalized = excerpt.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if !seen_texts.insert((source.clone(), normalized)) { continue; }
        if let Some(chunk) = item["chunkId"].as_str() {
            if !seen_chunks.insert((item["scopeKey"].clone().to_string(), chunk.to_owned())) { continue; }
        }
        let position = *sources.entry(source).or_insert_with(|| { groups.push(Vec::new()); groups.len() - 1 });
        let duplicate = groups[position].iter().any(|previous| {
            let left = &items[*previous];
            let range = |value: &Value| value["startOffset"].as_u64().zip(value["endOffset"].as_u64());
            let (Some((a,b)), Some((c,d))) = (range(left),range(item)) else { return false; };
            let shortest = b.saturating_sub(a).min(d.saturating_sub(c));
            shortest > 0 && b.min(d).saturating_sub(a.max(c)) as f64 / shortest as f64 >= 0.8
        });
        if !duplicate { groups[position].push(index); }
    }
    let mut indices = Vec::new();
    for depth in 0..items.len() {
        let before = indices.len();
        for group in &groups {
            if let Some(index) = group.get(depth) { indices.push(*index); }
            if indices.len() == maximum { break; }
        }
        if indices.len() == maximum || indices.len() == before { break; }
    }
    if indices.len() < 2 { return skipped("duplicate-candidates"); }
    let mut windowed_inputs = 0;
    let texts = indices.iter().map(|index| {
        let (passage, windowed) = rerank_passage(&items[*index], query, (448 - query_tokens) as usize);
        windowed_inputs += usize::from(windowed);
        passage
    }).collect();
    RerankPlan { indices, texts, windowed_inputs, reason: "semantic-disambiguation" }
}

fn fitted_end(text: &str, offsets: &[usize], start: usize, budget: usize) -> usize {
    let (mut low, mut high) = (start, offsets.len() - 1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if crate::text_metrics::estimate_tokens(&text[offsets[start]..offsets[middle]]) <= budget as u64 { low = middle; }
        else { high = middle - 1; }
    }
    low
}

/// A query-centered window avoids treating a 512-token prefix as a complete long passage.
/// 查询相关窗口避免用长片段的前512 token代替完整内容；真实分词截断仍由模型回执说明。
fn rerank_passage(item: &Value, query: &str, budget: usize) -> (String, bool) {
    let title = item["title"].as_str().unwrap_or("");
    let title_offsets: Vec<_> = title.char_indices().map(|(index,_)| index).chain(std::iter::once(title.len())).collect();
    let title_end = fitted_end(title, &title_offsets, 0, budget.min(64));
    let title = &title[..title_offsets[title_end]];
    let body_budget = budget.saturating_sub(crate::text_metrics::estimate_tokens(title) as usize + 1).max(1);
    let excerpt = item["excerpt"].as_str().unwrap_or("");
    if crate::text_metrics::estimate_tokens(excerpt) <= body_budget as u64 {
        return (format!("{title}\n{excerpt}"), false);
    }
    let offsets: Vec<_> = excerpt.char_indices().map(|(index,_)| index).chain(std::iter::once(excerpt.len())).collect();
    let lower = excerpt.to_lowercase();
    let terms: Vec<_> = super::lexical::collect_terms(query, 24, true).into_iter()
        .filter(|term| term.chars().count() > 1 && !["the","and","for","how","what","does","this","that","with","from"].contains(&term.as_str())).collect();
    let mut starts = vec![0];
    for term in &terms {
        for (position,_) in lower.match_indices(term.as_str()).take(2) {
            starts.push(lower[..position].chars().count().saturating_sub(64).min(offsets.len()-1));
        }
    }
    starts.sort_unstable(); starts.dedup();
    let mut best = (0, fitted_end(excerpt, &offsets, 0, body_budget), 0);
    for start in starts {
        let end = fitted_end(excerpt, &offsets, start, body_budget);
        let window = excerpt[offsets[start]..offsets[end]].to_lowercase();
        let matches = terms.iter().filter(|term| window.contains(term.as_str())).count();
        if matches > best.2 { best = (start,end,matches); }
    }
    (format!("{title}\n{}", &excerpt[offsets[best.0]..offsets[best.1]]), true)
}

/// Fuse ranks, not incomparable logits; an incomplete reranker view receives less weight.
/// 融合名次而非不同量纲的logit；重排输入仍被截断时降低其影响，保留原融合得分。
pub fn fuse_reranked(items: &mut [Value], indices: &[usize], scores: &[f64], truncated: usize) -> bool {
    if indices.len() != scores.len() || scores.iter().any(|score| !score.is_finite()) { return false; }
    let mut ranked: Vec<_> = indices.iter().copied().zip(scores.iter().copied()).collect();
    ranked.sort_by(|left,right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let weight = if truncated > 0 {0.25} else {0.5};
    for (index,item) in items.iter_mut().enumerate() {
        item["retrievalRank"] = json!(index + 1);
        item["rerankFusionScore"] = json!(1.0 / (61 + index) as f64);
    }
    for (rank,(index,score)) in ranked.iter().enumerate() {
        let item = &mut items[*index];
        item["rerankScore"] = json!(score);
        item["rerankRank"] = json!(rank + 1);
        item["rerankFusionScore"] = json!(item["rerankFusionScore"].as_f64().unwrap() + weight / (61 + rank) as f64);
    }
    items.sort_by(|left,right| (right["exactTargetMatch"] == true).cmp(&(left["exactTargetMatch"] == true))
        .then_with(|| right["rerankFusionScore"].as_f64().unwrap().total_cmp(&left["rerankFusionScore"].as_f64().unwrap()))
        .then_with(|| left["retrievalRank"].as_u64().cmp(&right["retrievalRank"].as_u64())));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_fusion_retains_retrieval_signal_exact_hits_and_unranked_candidates() {
        let original: Vec<_> = (0..10).map(|index| json!({"chunkId":format!("c{index}"),
            "score":1.0/(61+index) as f64,"exactTargetMatch":index==7})).collect();
        let indices: Vec<_> = (0..8).collect();
        let scores: Vec<_> = (0..8).map(|index| index as f64).collect();
        let mut first = original.clone();
        assert!(fuse_reranked(&mut first,&indices,&scores,0));
        assert_eq!(first[0]["chunkId"],"c7");
        assert_eq!(first[1]["chunkId"],"c0");
        assert_eq!(first.len(),original.len());
        for item in &first {
            assert_eq!(item["score"],original.iter().find(|previous| previous["chunkId"]==item["chunkId"]).unwrap()["score"]);
        }
        let mut scaled = original;
        let scores: Vec<_> = scores.iter().map(|score|score*1_000_000.0-20_000.0).collect();
        assert!(fuse_reranked(&mut scaled,&indices,&scores,0));
        assert_eq!(first.iter().map(|item|&item["chunkId"]).collect::<Vec<_>>(),
            scaled.iter().map(|item|&item["chunkId"]).collect::<Vec<_>>());
        assert!(first.iter().find(|item|item["chunkId"]=="c9").unwrap().get("rerankScore").is_none());
    }

    #[test]
    fn rerank_candidates_preserve_exact_paths_and_share_budget_across_sources() {
        let row=|source:&str,chunk:&str,text:&str,start:usize|json!({"scopeKey":"project:a","sourceId":source,
            "chunkId":chunk,"title":source,"excerpt":text,"startOffset":start,"endOffset":start+100,
            "locator":{"relativePath":format!("{source}.md")}});
        let items=vec![row("a","a0","Cache retains original bytes.",0),
            row("a","a1","Cache retains original bytes.",10),
            row("a","a2","Cache eviction releases old entries.",200),
            row("b","b0","Scope boundaries isolate cache entries.",0),
            row("c","c0","Cache versions invalidate earlier evidence.",0)];
        let plan=rerank_plan(&items,&json!({"query":"how cache entries are isolated"}),4);
        assert_eq!(plan.indices,vec![0,3,4,2]);
        assert_eq!(plan.texts.len(),4);
        let targeted=rerank_plan(&items,&json!({"query":"explain cache eviction","path":"a.md"}),4);
        assert_eq!(targeted.reason,"exact-path-or-symbol");
        assert!(targeted.indices.is_empty());
        assert_eq!(rerank_plan(&items,&json!({"query":"cache"}),4).reason,"literal-lookup");
    }

    #[test]
    fn rerank_window_keeps_late_english_and_chinese_evidence_without_replacing_sources() {
        for (query,excerpt,expected) in [
            ("memory throughput network contention",format!("{}The mechanism protects memory throughput during network contention.","Unrelated background material. ".repeat(300)),"network contention"),
            ("向量索引 作用域隔离",format!("{}新的核心结论：向量索引应按作用域隔离，删除来源后同步失效。","旧章节背景材料。".repeat(300)),"作用域隔离"),
        ] {
            let item=json!({"title":"Source title","excerpt":excerpt,"startOffset":123,"sourceRef":"unchanged-reference"});
            let original=item.clone();
            let (passage,windowed)=rerank_passage(&item,query,320);
            assert!(windowed); assert!(passage.contains(expected));
            assert!(passage.starts_with("Source title\n"));
            assert!(crate::text_metrics::estimate_tokens(&passage)<=320);
            assert_eq!(item,original);
        }
    }

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
