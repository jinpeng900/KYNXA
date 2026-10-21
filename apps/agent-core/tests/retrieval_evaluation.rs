//! Frozen development-corpus evaluation through the current native lexical index.
//! 使用固定开发语料评估当前原生词法索引；不调用模型、不读取用户数据，不代表公开语料泛化成绩。

use kynxa_agent_core_native::{
    gateway_contract::{GatewayConfig, new_id},
    retrieval::gateway_index,
    storage::sqlite_connections,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

const DATASET: &str = include_str!("../../../tests/retrieval-benchmark/datasets/native-baseline.json");
const CUTOFF: usize = 10;

struct Workspace {
    root: PathBuf,
    config: GatewayConfig,
}

impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-retrieval-evaluation-{}", new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let config = GatewayConfig {
            data_root: root.join("Data"),
            data_home: root.join("Models"),
            extension_root: root.join("Extensions"),
            application_root: root.join("App"),
            port: 0,
            toolhost_exe: root.join("toolhost.exe"),
            vector_extension: root.join("vec0.dll"),
            inference_library: root.join("onnxruntime.dll"),
        };
        Self { root, config }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Release the pooled SQLite handle before removing the owned Windows fixture.
        // 先释放连接池中的 SQLite 句柄，再删除本测试独占的 Windows 临时目录。
        let _ = sqlite_connections::close(
            &self.config.data_root,
            &gateway_index::database(&self.config),
        );
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!(
                "retrieval fixture cleanup failed: {}: {error}",
                self.root.display()
            );
        }
    }
}

/// Verify literal gold evidence against the frozen corpus before publishing or scoring.
/// 发布和评分前核对固定原文中的金标字面证据，避免数据与判定规则脱节。
fn verify_dataset(dataset: &Value) {
    assert_eq!(dataset["schemaVersion"], 1);
    assert_eq!(dataset["partition"], "development");
    let mut corpus = BTreeMap::new();
    for source in dataset["sources"].as_array().unwrap() {
        let id = source["sourceId"].as_str().unwrap();
        assert!(
            corpus.insert(id, source["text"].as_str().unwrap()).is_none(),
            "duplicate source: {id}"
        );
    }
    assert_eq!(corpus.len(), 40);
    let mut query_ids = HashSet::new();
    for query in dataset["queries"].as_array().unwrap() {
        let id = query["queryId"].as_str().unwrap();
        assert!(query_ids.insert(id), "duplicate query: {id}");
        let gold = query["gold"].as_array().unwrap();
        assert_eq!(
            query["noAnswer"] == true,
            gold.is_empty(),
            "invalid no-answer gold: {id}"
        );
        let mut source_ids = HashSet::new();
        for reference in gold {
            let source = reference["sourceId"].as_str().unwrap();
            assert!(
                source_ids.insert(source),
                "duplicate gold source: {id}/{source}"
            );
            let text = corpus.get(source).expect("gold source exists");
            let evidence = reference["evidence"].as_array().unwrap();
            assert!(
                !evidence.is_empty(),
                "empty gold evidence: {id}/{source}"
            );
            for span in evidence {
                let span = span.as_str().unwrap();
                assert!(
                    !span.is_empty() && text.contains(span),
                    "missing literal evidence: {id}/{source}/{span}"
                );
            }
        }
    }
    assert_eq!(query_ids.len(), 15);
}

fn published_sources(dataset: &Value) -> Vec<Value> {
    dataset["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            json!({
                "sourceId":source["sourceId"], "scopeKey":"user", "sourceType":"document",
                "title":source["title"], "text":source["text"],
                "locator":{"path":source["locatorPath"], "relativePath":source["locatorPath"]},
                "sourceRevision":1,
            })
        })
        .collect()
}

/// Collapse chunks in actual returned order; repeated chunks cannot inflate document recall.
/// 按生产返回顺序将分块折叠为文档；同一文档的多个块不能抬高文档召回率。
fn document_ranking(items: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .iter()
        .filter_map(|item| {
            let source = item["sourceId"].as_str().expect("result source identity");
            seen.insert(source.to_owned()).then(|| source.to_owned())
        })
        .collect()
}

fn metrics(ranked: &[String], gold: &HashSet<String>) -> Value {
    if gold.is_empty() {
        return json!({"ndcgAt10":null,"recallAt10":null,"mrrAt10":null});
    }
    let mut hits = 0;
    let mut dcg = 0.0;
    let mut mrr = 0.0;
    for (rank, source) in ranked.iter().take(CUTOFF).enumerate() {
        if gold.contains(source) {
            hits += 1;
            dcg += 1.0 / ((rank + 2) as f64).log2();
            if mrr == 0.0 {
                mrr = 1.0 / (rank + 1) as f64;
            }
        }
    }
    let ideal: f64 = (0..gold.len().min(CUTOFF))
        .map(|rank| 1.0 / ((rank + 2) as f64).log2())
        .sum();
    json!({"ndcgAt10":dcg/ideal,"recallAt10":hits as f64/gold.len() as f64,"mrrAt10":mrr})
}

fn aggregate(observations: &[Value], diagnostic: bool) -> Value {
    let selected = observations
        .iter()
        .filter(|item| {
            item["noAnswer"] != true && (item["diagnosticOnly"] == true) == diagnostic
        })
        .collect::<Vec<_>>();
    let mean = |field: &str| {
        if selected.is_empty() {
            Value::Null
        } else {
            json!(
                selected.iter().map(|item| item[field].as_f64().unwrap()).sum::<f64>()
                    / selected.len() as f64
            )
        }
    };
    json!({"queryCount":selected.len(),"ndcgAt10":mean("ndcgAt10"),"recallAt10":mean("recallAt10"),"mrrAt10":mean("mrrAt10")})
}

#[test]
fn native_retrieval_lexical_evaluation_reports_frozen_development_gold() {
    let dataset: Value = serde_json::from_str(DATASET).unwrap();
    verify_dataset(&dataset);
    let workspace = Workspace::new();
    gateway_index::publish(&workspace.config, &published_sources(&dataset)).unwrap();
    let index = gateway_index::status(&workspace.config).unwrap();
    let mut observations = Vec::new();
    let mut rankings = BTreeMap::new();
    for query in dataset["queries"].as_array().unwrap() {
        // Exercise the current lexical/literal/symbol/path fusion, without vectors or reranking.
        // 使用当前词法、字面、符号、路径融合链路；不注入向量，也不另接重排器。
        let response = gateway_index::search(
            &workspace.config,
            &json!({
                "query":query["query"], "scopeKeys":["user"],
                "channelCandidates":64, "limit":96, "retrievalIntent":{}, "ann":{},
            }),
        )
        .unwrap();
        let items = response["items"]
            .as_array()
            .expect("production search result items");
        let ranked = document_ranking(items);
        let gold = query["gold"]
            .as_array()
            .unwrap()
            .iter()
            .map(|reference| reference["sourceId"].as_str().unwrap().to_owned())
            .collect::<HashSet<_>>();
        let mut observation = metrics(&ranked, &gold);
        observation["queryId"] = query["queryId"].clone();
        observation["query"] = query["query"].clone();
        observation["gold"] = query["gold"].clone();
        observation["noAnswer"] = json!(query["noAnswer"] == true);
        observation["diagnosticOnly"] = json!(query["diagnosticOnly"] == true);
        observation["rankedDocumentsAt10"] =
            json!(ranked.into_iter().take(CUTOFF).collect::<Vec<_>>());
        observation["returnedChunkCount"] = json!(items.len());
        // Keep original chunk identities, offsets and excerpts available for audit in an optional report.
        // 可选报告保留原始块身份、偏移和摘录，文档折叠不掩盖块级问题。
        rankings.insert(query["queryId"].as_str().unwrap().to_owned(), response);
        observations.push(observation);
    }
    let primary = aggregate(&observations, false);
    let diagnostics = aggregate(&observations, true);
    let no_answer = observations
        .iter()
        .filter(|item| item["noAnswer"] == true)
        .cloned()
        .collect::<Vec<_>>();
    let summary = json!({
        "schemaVersion":1, "datasetId":dataset["datasetId"], "datasetVersion":dataset["datasetVersion"],
        "datasetSha256":format!("{:x}", Sha256::digest(DATASET.as_bytes())),
        "partition":dataset["partition"], "cutoff":CUTOFF,
        "scope":"frozen synthetic development corpus; native lexical fusion; document@10",
        "index":index, "primary":primary, "diagnostic":diagnostics, "noAnswer":no_answer,
        "observations":observations,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    if let Some(path) = std::env::var_os("KYNXA_RETRIEVAL_EVALUATION_REPORT") {
        let mut report = summary.clone();
        report["searchResults"] = json!(rankings);
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    assert_eq!(summary["primary"]["queryCount"], 11);
    assert_eq!(summary["diagnostic"]["queryCount"], 3);
    assert_eq!(summary["noAnswer"].as_array().unwrap().len(), 1);
    // Strong literal anchors are functional checks, not imported aggregate score thresholds.
    // 明确字面锚点仅作功能回归断言，不沿用旧实现的整体分数门槛。
    for id in [
        "q1-manifest",
        "q2-vector-cosine",
        "q4-extract-signature",
        "q11-bm25-b-parameter",
    ] {
        let observation = summary["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["queryId"] == id)
            .unwrap();
        assert_eq!(
            observation["recallAt10"],
            1.0,
            "literal anchor not retrieved: {id}: {observation}"
        );
    }
}
