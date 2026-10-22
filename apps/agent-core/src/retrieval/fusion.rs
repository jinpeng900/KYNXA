use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub const SORT_VERSION: &str = "e5-lexical-rrf-structured-v2";

pub fn rrf_k(input: &Value) -> u64 {
    input["rrfK"].as_u64().unwrap_or(60).clamp(20, 100)
}

struct Candidate {
    chunk_id: String,
    positions: HashMap<&'static str, usize>,
    score: f64,
    collation_index: usize,
}

/// Fuse channel ranks, never scores from incompatible lexical/vector scales.
/// 融合通道名次，不相加词法和向量的不同量纲原始分数；结构通道仅提供小幅增益。
pub fn apply(arguments: &Value) -> Value {
    let k = rrf_k(arguments);
    let collation_order: HashMap<_, _> = arguments["collationOrder"].as_array().into_iter().flatten()
        .enumerate().filter_map(|(index, id)| id.as_str().map(|id| (id, index))).collect();
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    for (channel, weight) in [("lexical", 1.0), ("vector", 1.0), ("symbol", 0.12), ("path", 0.10)] {
        let mut channel_seen = HashSet::new();
        for (rank, row) in arguments[channel].as_array().into_iter().flatten().enumerate() {
            let Some(id) = row["chunkId"].as_str().filter(|id| !id.is_empty()) else { continue; };
            if !channel_seen.insert(id) { continue; }
            let candidate = candidates.entry(id.to_owned()).or_insert_with(|| Candidate {
                collation_index: collation_order.get(id).copied().unwrap_or(usize::MAX),
                chunk_id: id.to_owned(), positions: HashMap::new(), score: 0.0,
            });
            candidate.positions.insert(channel, rank);
            candidate.score += weight / (k + 1 + rank as u64) as f64;
        }
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by(|left, right| right.score.total_cmp(&left.score)
        .then_with(|| left.collation_index.cmp(&right.collation_index))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id)));
    candidates.truncate(arguments["limit"].as_u64().unwrap_or(8) as usize);
    json!(candidates.into_iter().map(|candidate| json!({"chunkId":candidate.chunk_id,
        "score":candidate.score,"positions":candidate.positions})).collect::<Vec<_>>())
}

fn normalized(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn source_key(item: &Value) -> String {
    json!([item["scopeKey"], item["sourceId"], item["contentHash"], item["sourceRevision"], item["derivationSignature"]]).to_string()
}

/// Merge only contiguous, byte-consistent source ranges; source authority is never invented.
/// 仅合并同一来源版本且内容一致的相邻原文区间；保留原片段凭据，适配器再生成来源级引用。
fn merge_adjacent(left: &mut Value, right: &Value) -> bool {
    if source_key(left) != source_key(right) { return false; }
    let range = |item: &Value| item["startOffset"].as_u64().zip(item["endOffset"].as_u64());
    let (Some((a,b)),Some((c,d))) = (range(left),range(right)) else { return false; };
    if b < a || d < c || b < c || d < a || b.max(d).saturating_sub(a.min(c)) > 8192 { return false; }
    let Some(ltext) = left["excerpt"].as_str() else { return false; };
    let Some(rtext) = right["excerpt"].as_str() else { return false; };
    let l: Vec<u16> = ltext.encode_utf16().collect();
    let r: Vec<u16> = rtext.encode_utf16().collect();
    if l.len() as u64 != b-a || r.len() as u64 != d-c { return false; }
    let overlap_start = a.max(c); let overlap_end = b.min(d);
    if overlap_end > overlap_start && l[(overlap_start-a)as usize..(overlap_end-a)as usize]
        != r[(overlap_start-c)as usize..(overlap_end-c)as usize] { return false; }
    let start = a.min(c); let end = b.max(d);
    let mut merged = vec![0u16; (end-start)as usize];
    merged[(a-start)as usize..(b-start)as usize].copy_from_slice(&l);
    merged[(c-start)as usize..(d-start)as usize].copy_from_slice(&r);
    let Ok(merged) = String::from_utf16(&merged) else { return false; };
    let descriptor = |item: &Value| json!({"sourceRef":item["sourceRef"],"chunkId":item["chunkId"],
        "chunkHash":item["chunkHash"],"startOffset":item["startOffset"],"endOffset":item["endOffset"]});
    let mut chunks = left["mergedChunks"].as_array().cloned().unwrap_or_else(||vec![descriptor(left)]);
    chunks.extend(right["mergedChunks"].as_array().cloned().unwrap_or_else(||vec![descriptor(right)]));
    left["mergedChunks"]=json!(chunks);
    left["excerpt"]=json!(merged); left["startOffset"]=json!(start); left["endOffset"]=json!(end);
    if c<a { left["startLine"]=right["startLine"].clone(); }
    if d>b { left["endLine"]=right["endLine"].clone(); }
    left["score"]=json!(left["score"].as_f64().unwrap_or(0.0).max(right["score"].as_f64().unwrap_or(0.0)));
    left["lightweightScore"]=json!(left["lightweightScore"].as_f64().unwrap_or(0.0).max(right["lightweightScore"].as_f64().unwrap_or(0.0)));
    if let Some(fields)=left.as_object_mut() { fields.remove("chunkId"); fields.remove("chunkHash"); }
    true
}

/// Keep a bounded top-ten source share without replacing stronger evidence with arbitrary raw scores.
/// 前十条同一文档最多三条；其余来源保留原排序，不能因缺少其他文档而回填第四条。
pub fn diversify(items: &mut Vec<Value>) {
    let mut counts = HashMap::new(); let mut selected=Vec::new(); let mut deferred=Vec::new();
    for item in items.drain(..) {
        if selected.len()<10 {
            let key=source_key(&item); let count=counts.entry(key).or_insert(0usize);
            if *count>=3 { deferred.push(item); continue; }
            *count+=1;
        }
        selected.push(item);
    }
    if selected.len()>=10 { selected.extend(deferred); }
    *items=selected;
}

/// Only exact path, whole symbol and title matches add small structured bonuses after RRF.
/// RRF 后仅使用精确路径、完整符号名和标题三类结构信号，不叠加关键词启发式规则。
pub fn lightweight(items: &mut Vec<Value>, input: &Value) -> Value {
    let k=rrf_k(input) as f64;
    let query=normalized(input["query"].as_str().unwrap_or(""));
    // The index contract carries constraints inside retrievalIntent; retain direct-call compatibility.
    // 索引合同将约束放在 retrievalIntent 中，同时兼容直接调用的顶层字段。
    let requested_path=input["retrievalIntent"]["path"].as_str().or_else(||input["path"].as_str())
        .unwrap_or("").replace('\\',"/").to_lowercase();
    let symbol=input["retrievalIntent"]["symbol"].as_str().or_else(||input["symbol"].as_str()).unwrap_or("");
    let before=items.len(); let mut seen=HashSet::new(); let mut unique:Vec<Value>=Vec::new();
    for mut item in items.drain(..) {
        let text=normalized(item["excerpt"].as_str().unwrap_or(""));
        if text.is_empty() || !seen.insert((source_key(&item),text)) { continue; }
        let path=item["locator"]["relativePath"].as_str().or_else(||item["locator"]["path"].as_str()).unwrap_or("").replace('\\',"/").to_lowercase();
        let exact_path=!requested_path.is_empty() && (path==requested_path || path.ends_with(&format!("/{requested_path}")));
        let exact_symbol=!symbol.is_empty() && ["symbolName","qualifiedName"].iter()
            .any(|key| item["structure"][key].as_str().is_some_and(|name|name==symbol));
        let title=normalized(item["title"].as_str().unwrap_or(""));
        let title_hit=title.chars().count()>=4 && query.contains(&title);
        let bonus=(if exact_path {0.10} else {0.0})+(if exact_symbol {0.12} else {0.0})+(if title_hit {0.08} else {0.0});
        item["lightweightScore"]=json!(item["score"].as_f64().unwrap_or(0.0)+bonus/(k+1.0));
        item["rankingSignals"]=json!({"exactPath":exact_path,"wholeSymbol":exact_symbol,"title":title_hit});
        if !unique.iter_mut().any(|previous|merge_adjacent(previous,&item)) { unique.push(item); }
    }
    unique.sort_by(|left,right|right["lightweightScore"].as_f64().unwrap_or(0.0)
        .total_cmp(&left["lightweightScore"].as_f64().unwrap_or(0.0)));
    diversify(&mut unique);
    unique.truncate(input["limit"].as_u64().unwrap_or(8).clamp(1,192) as usize);
    let result=json!({"state":"applied","method":"rank-rrf-structured","version":SORT_VERSION,
        "rrfK":k as u64,"removedOrMerged":before.saturating_sub(unique.len()),"topTenPerDocument":3});
    *items=unique; result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_channel_scores_follow_the_original_icu_collation_receipt() {
        let result=apply(&json!({"lexical":[{"chunkId":"A"},{"chunkId":"a"}],
            "vector":[{"chunkId":"a"},{"chunkId":"A"}],"collationOrder":["a","A"],"limit":2}));
        assert_eq!(result[0]["score"],result[1]["score"]); assert_eq!(result[0]["chunkId"],"a");
    }
    #[test]
    fn two_channel_rrf_ignores_raw_score_scale_and_bounds_k() {
        let mut args=json!({"lexical":[{"chunkId":"first","score":90000},{"chunkId":"both","score":-99}],
            "vector":[{"chunkId":"both","score":-999999}],"limit":2,"rrfK":60});
        assert_eq!(apply(&args)[0]["chunkId"],"both"); let original=apply(&args);
        args["vector"][0]["score"]=json!(999999); assert_eq!(apply(&args),original);
        args["rrfK"]=json!(0); assert_eq!(rrf_k(&args),20);
        args["rrfK"]=json!(1000); assert_eq!(rrf_k(&args),100);
    }
    #[test]
    fn merge_ranges_retains_provenance_and_refuses_conflicting_bytes_or_versions() {
        let row=|start,end,text:&str|json!({"scopeKey":"project:a","sourceId":"a","contentHash":"v1","sourceRevision":1,
            "chunkId":format!("c{start}"),"chunkHash":format!("h{start}"),"startOffset":start,"endOffset":end,"excerpt":text,
            "sourceRef":format!("ref{start}"),"score":0.03});
        let mut first=row(0,4,"ab中文"); let next=row(2,6,"中文cd");
        assert!(merge_adjacent(&mut first,&next)); assert_eq!(first["excerpt"],"ab中文cd");
        assert_eq!(first["mergedChunks"].as_array().unwrap().len(),2); assert!(first.get("chunkId").is_none());
        assert!(!merge_adjacent(&mut first,&row(4,8,"zzef")));
        let mut other=row(6,8,"ef");other["sourceRevision"]=json!(2);assert!(!merge_adjacent(&mut first,&other));
    }
    #[test]
    fn top_ten_source_share_never_backfills_a_fourth_fragment() {
        let mut rows=(0..15).map(|i|json!({"sourceId":if i<7{"a"}else{"b"},"scopeKey":"x","n":i})).collect();
        diversify(&mut rows);assert_eq!(rows.len(),6);
        let mut rows:Vec<_>=(0..15).map(|i|json!({"sourceId":if i<7{"a".to_owned()}else{format!("s{i}")},"n":i})).collect();
        diversify(&mut rows);assert_eq!(rows.len(),15);
        assert_eq!(rows.iter().take(10).filter(|item|item["sourceId"]=="a").count(),3);
    }

    #[test]
    fn index_intent_constraints_reach_structured_ranking_without_partial_symbol_matches() {
        let row=|id:&str,path:&str,symbol:&str|json!({"sourceId":id,"excerpt":format!("body {id}"),"score":0.03,
            "locator":{"relativePath":path},"structure":{"symbolName":symbol}});
        let mut rows=vec![row("partial","src/math-extra.rs","add_total"),row("exact","src/math.rs","add")];
        lightweight(&mut rows,&json!({"query":"find add","limit":2,"retrievalIntent":{"path":"src/math.rs","symbol":"add"}}));
        assert_eq!(rows[0]["sourceId"],"exact");
        assert_eq!(rows[0]["rankingSignals"]["exactPath"],true);
        assert_eq!(rows[0]["rankingSignals"]["wholeSymbol"],true);
        assert_eq!(rows[1]["rankingSignals"]["exactPath"],false);
        assert_eq!(rows[1]["rankingSignals"]["wholeSymbol"],false);
    }
}
