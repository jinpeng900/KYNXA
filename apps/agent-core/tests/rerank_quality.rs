//! Replay paired evidence fixtures through the production lightweight ranker without loading a model.
//! 将成对证据夹具交给生产轻量排序器重放；不加载交叉编码器或将本重放冒充端到端推理。
use kynxa_agent_core_native::retrieval::fusion;
use serde_json::{Value,json};
use std::{collections::HashSet,time::Instant};
fn recall(items:&[Value],relevant:&HashSet<String>)->f64 {
    let mut seen=HashSet::new();
    let hits=items.iter().filter_map(|item|item["documentId"].as_str().or_else(||item["sourceId"].as_str()))
        .filter(|id|seen.insert((*id).to_owned())).take(10).filter(|id|relevant.contains(*id)).count();
    hits as f64 / relevant.len().max(1) as f64
}
#[test]
#[ignore="requires KYNXA_LIGHTWEIGHT_RANK_FIXTURE and KYNXA_LIGHTWEIGHT_RANK_RESULT"]
fn lightweight_ranking_replays_paired_evidence_without_a_reranker() {
    let fixture=std::env::var("KYNXA_LIGHTWEIGHT_RANK_FIXTURE").expect("set paired fixture");
    let output=std::env::var("KYNXA_LIGHTWEIGHT_RANK_RESULT").expect("set result");
    let rows:Vec<Value>=serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    let started=Instant::now();let mut results=Vec::new();
    for row in rows {
        let original=row["items"].as_array().unwrap().clone();
        let relevant:HashSet<String>=row["relevant"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned).collect();
        let mut ranked=original.clone();
        for item in &mut ranked {if item["sourceId"].is_null(){item["sourceId"]=item["documentId"].clone();}}
        let audit=fusion::lightweight(&mut ranked,&json!({"query":row["query"],"limit":60,"rrfK":60}));
        assert_eq!(audit["version"],fusion::SORT_VERSION);
        results.push(json!({"id":row["id"],"language":row["language"],"baselineRecall10":recall(&original,&relevant),
            "lightweightRecall10":recall(&ranked,&relevant),"inputCandidates":original.len(),"outputCandidates":ranked.len(),"ranking":audit}));
    }
    let result=json!({"realInference":false,"method":fusion::SORT_VERSION,"queryCount":results.len(),
        "elapsedMs":started.elapsed().as_millis(),"cases":results});
    std::fs::write(output,serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}
