//! Recorded BGE scores exercise production rank fusion without neural inference.
//! 已保存的BGE分数通过生产名次融合实现重放，不重新执行模型推理。

use kynxa_agent_core_native::retrieval::fusion::fuse_reranked;
use serde_json::{Value, json};
use std::collections::HashSet;

/// Replay recorded model scores only; this is not a new neural inference benchmark.
/// 只重放已保存的模型分数，不把排序对照表述为重新执行神经模型评测。
#[test]
#[ignore = "requires explicit KYNXA_RERANK_QUALITY_FIXTURE and KYNXA_RERANK_QUALITY_RESULT"]
fn replay_recorded_rerank_quality() {
    let input=std::env::var("KYNXA_RERANK_QUALITY_FIXTURE").expect("set recorded rankings fixture");
    let output=std::env::var("KYNXA_RERANK_QUALITY_RESULT").expect("set result path");
    let recall=|items:&[Value],relevant:&HashSet<String>| {
        let mut seen=HashSet::new();
        let documents:Vec<_>=items.iter().filter_map(|item|item["documentId"].as_str())
            .filter(|document|seen.insert((*document).to_owned())).take(10).collect();
        documents.iter().filter(|document|relevant.contains(**document)).count() as f64/relevant.len() as f64
    };
    let (mut totals,mut queries,mut improved,mut regressed)=([0.0;3],0usize,0usize,0usize);
    let mut cases=Vec::new();
    for line in std::fs::read_to_string(&input).unwrap().lines().filter(|line|!line.trim().is_empty()) {
        let row:Value=serde_json::from_str(line).unwrap();
        let relevant:HashSet<String>=row["judgments"].as_array().unwrap().iter()
            .filter(|judgment|judgment["grade"].as_i64().unwrap_or(0)>0)
            .filter_map(|judgment|judgment["documentId"].as_str().map(str::to_owned)).collect();
        if relevant.is_empty() {continue;}
        let mut baseline=row["chunks"].as_array().unwrap().clone();
        baseline.sort_by(|left,right|right["score"].as_f64().unwrap_or(0.0).total_cmp(&left["score"].as_f64().unwrap_or(0.0)));
        let mut legacy=baseline.clone();
        legacy.sort_by(|left,right|right["rerankScore"].as_f64().unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&left["rerankScore"].as_f64().unwrap_or(f64::NEG_INFINITY)));
        let indices:Vec<_>=baseline.iter().enumerate().filter(|(_,item)|item["rerankScore"].as_f64().is_some()).map(|(index,_)|index).collect();
        let scores:Vec<_>=indices.iter().map(|index|baseline[*index]["rerankScore"].as_f64().unwrap()).collect();
        let mut fused=baseline.clone();
        assert!(fuse_reranked(&mut fused,&indices,&scores,row["truncatedInputsCount"].as_u64().unwrap_or(0) as usize));
        let values=[recall(&baseline,&relevant),recall(&legacy,&relevant),recall(&fused,&relevant)];
        for (total,value) in totals.iter_mut().zip(values) {*total+=value;}
        improved+=usize::from(values[2]>values[0]); regressed+=usize::from(values[2]<values[0]); queries+=1;
        cases.push(json!({"queryId":row["queryId"],"baselineRecall10":values[0],"legacyRecall10":values[1],"fusedRecall10":values[2]}));
    }
    assert!(queries>0);
    let result=json!({"method":"saved-BGE-score-replay","newInference":false,"coverage":"saved-candidates-only",
        "queries":queries,"baselineRecall10":totals[0]/queries as f64,"legacyRecall10":totals[1]/queries as f64,
        "fusedRecall10":totals[2]/queries as f64,"improvedAgainstBaseline":improved,"regressedAgainstBaseline":regressed,"cases":cases});
    let path=std::path::Path::new(&output);
    if let Some(parent)=path.parent() {std::fs::create_dir_all(parent).unwrap();}
    std::fs::write(path,serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    println!("{}",json!({"queries":queries,"baselineRecall10":result["baselineRecall10"],"legacyRecall10":result["legacyRecall10"],"fusedRecall10":result["fusedRecall10"]}));
}
