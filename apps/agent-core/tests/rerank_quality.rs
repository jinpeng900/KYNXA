//! Explicit real-model check for the same production candidate window and rank fusion.
//! 显式使用真实模型验证生产候选窗口与名次融合；不创建正式资料或下载模型。

use kynxa_agent_core_native::{models::gateway_inference::{self, InferenceOptions}, retrieval::fusion};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf, sync::{Arc, atomic::AtomicBool}, time::Instant};

fn recall(items: &[Value], relevant: &HashSet<String>) -> f64 {
    let mut seen = HashSet::new();
    let matches = items.iter().filter_map(|item| item["documentId"].as_str())
        .filter(|id| seen.insert((*id).to_owned())).take(10)
        .filter(|id| relevant.contains(*id)).count();
    matches as f64 / relevant.len() as f64
}

#[test]
#[ignore = "requires KYNXA_RERANK_LIVE_FIXTURE, KYNXA_RERANK_LIVE_BUNDLE and KYNXA_RERANK_LIVE_RESULT"]
fn real_bge_uses_query_windows_and_preserves_retrieval_signal() {
    let fixture = std::env::var("KYNXA_RERANK_LIVE_FIXTURE").expect("set fixture");
    let bundle = PathBuf::from(std::env::var("KYNXA_RERANK_LIVE_BUNDLE").expect("set bundle"));
    let output = std::env::var("KYNXA_RERANK_LIVE_RESULT").expect("set result");
    gateway_inference::initialize_runtime(&bundle.join("runtime/native/onnxruntime.dll")).unwrap();
    let root = bundle.join("runtime/rerank/builtin-multilingual-reranker");
    let rows: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    let options = InferenceOptions { cpu_threads: 4, batch_size: 4, batch_token_budget: 2048, ..Default::default() };
    let started = Instant::now();
    let mut results = Vec::new();
    for row in rows {
        let query = row["query"].as_str().unwrap();
        let items = row["items"].as_array().unwrap().clone();
        let relevant: HashSet<String> = row["relevant"].as_array().unwrap().iter()
            .map(|id| id.as_str().unwrap().to_owned()).collect();
        let plan = fusion::rerank_plan(&items, &json!({"query":query,"taskType":"research"}), 20);
        assert!(!plan.indices.is_empty(), "{}", row["id"]);
        let legacy_texts: Vec<_> = items.iter().take(20).map(|item| item["excerpt"].as_str().unwrap().to_owned()).collect();
        let legacy_scores = gateway_inference::rerank_blocking(&root, query, &legacy_texts, &options,
            Arc::new(AtomicBool::new(false))).unwrap();
        let scores = gateway_inference::rerank_blocking(&root, query, &plan.texts, &options,
            Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(scores.scores.len(), plan.indices.len());
        assert!(scores.scores.iter().all(|score| score.is_finite()));
        let mut legacy: Vec<_> = items.iter().cloned().zip(legacy_scores.scores.iter().copied()).collect();
        legacy.sort_by(|left,right| right.1.total_cmp(&left.1));
        let legacy: Vec<_> = legacy.into_iter().map(|(item,_)| item).collect();
        let mut fused = items.clone();
        assert!(fusion::fuse_reranked(&mut fused, &plan.indices, &scores.scores, scores.truncated_inputs_count));
        assert_eq!(fused.len(), items.len());
        for item in &fused {
            let original = items.iter().find(|source| source["chunkId"] == item["chunkId"]).unwrap();
            assert_eq!(item["excerpt"], original["excerpt"]);
            assert_eq!(item["sourceRef"], original["sourceRef"]);
        }
        results.push(json!({"id":row["id"],"language":row["language"],"baselineRecall10":recall(&items,&relevant),
            "legacyRecall10":recall(&legacy,&relevant),"fusedRecall10":recall(&fused,&relevant),
            "candidates":items.len(),"windowedInputs":plan.windowed_inputs,
            "legacyTruncatedInputs":legacy_scores.truncated_inputs_count,"newTruncatedInputs":scores.truncated_inputs_count}));
    }
    // A tiny paired check exercises actual CPU inference; the saved 300-query replay is reported separately.
    // 本小集只验证真实CPU推理及成对排序，另行报告300题已保存分数重放，不混为同一评测。
    let mean = |language: &str, field: &str| {
        let selected: Vec<_> = results.iter().filter(|row| row["language"] == language).collect();
        selected.iter().map(|row| row[field].as_f64().unwrap()).sum::<f64>() / selected.len() as f64
    };
    let result = json!({"realInference":true,"model":"bge-reranker-base Q8","device":"cpu",
        "queryCount":results.len(),"elapsedMs":started.elapsed().as_millis(),
        "english":{"baselineRecall10":mean("en","baselineRecall10"),"legacyRecall10":mean("en","legacyRecall10"),"fusedRecall10":mean("en","fusedRecall10")},
        "chineseSilver":{"baselineRecall10":mean("zh","baselineRecall10"),"legacyRecall10":mean("zh","legacyRecall10"),"fusedRecall10":mean("zh","fusedRecall10")},"cases":results});
    std::fs::write(output, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    assert_eq!(result["queryCount"], 6);
    for language in ["english", "chineseSilver"] {
        assert!(result[language]["fusedRecall10"].as_f64().unwrap() >= result[language]["baselineRecall10"].as_f64().unwrap(),
            "paired fixture regressed: {result}");
    }
}
