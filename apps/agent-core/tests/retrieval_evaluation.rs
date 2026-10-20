//! Fixed offline evaluation of the production native retrieval chain.
//! 用固定数据集离线评估原生检索链路：在隔离数据根发布真实索引，跑生产融合排序并按公开金标打分。
//!
//! The runner never reads user data and never calls a model provider. Sources and gold evidence come from
//! `tests/retrieval-benchmark/datasets/native-baseline.json`; the same file is consumable by
//! `tests/retrieval-benchmark/metrics.mjs` so recorded scores stay comparable. The lexical baseline always
//! runs. The semantic baseline additionally needs the prepared Windows payload through
//! `KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT` and is `#[ignore]`d, so a default suite never depends on model assets.
//! 本入口不读取用户数据、不调用模型供应商；数据集与金标来自同一固定 JSON，`metrics.mjs` 也能读取它。
//! 词法基线始终运行；语义基线另需 `KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT` 指向已准备载荷，默认被 ignore，
//! 因此常规测试套件不依赖模型资产。

use kynxa_agent_core_native::{
    gateway_contract::{GatewayConfig, new_id},
    retrieval::gateway_index,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

const DATASET: &str = include_str!("../../../tests/retrieval-benchmark/datasets/native-baseline.json");
/// Document-level cut-off; chunk rankings collapse to their document in returned order.
/// 文档级截断；分块排序按返回顺序折叠为其所属文档。
const CUTOFF: usize = 10;
/// Recorded floors for the frozen dataset. A lower score means the ranking regressed for these queries.
/// 固定数据集的记录下限；低于此值表示这批查询的排序出现回归。
const BASELINE_NDCG: f64 = 0.88;
const BASELINE_RECALL: f64 = 0.95;
/// The hybrid column has its own floor: with the current RRF weights the vector channel lifts paraphrases and
/// costs a little on literal lookups, so it is not scored against the lexical floor.
/// 混合列单独设下限：当前 RRF 权重下向量通道换来语义改写提升，代价是少量字面查询，不能按词法下限计分。
const SEMANTIC_BASELINE_NDCG: f64 = 0.85;
const SEMANTIC_BASELINE_DIAGNOSTIC: f64 = 0.75;
/// The production rerank stage only reorders the first `min(rerankCandidates, 20)` fused candidates.
/// 生产重排阶段只重排融合后的前 `min(rerankCandidates, 20)` 条候选。
const RERANK_CANDIDATES: usize = 20;
const RERANK_BASELINE_NDCG: f64 = 0.95;

struct Workspace {
    root: PathBuf,
    config: GatewayConfig,
}

impl Workspace {
    /// An isolated temporary root keeps the evaluation away from the user's data root and index.
    /// 独立临时根目录让评测与用户正式数据根、正式索引彻底分开。
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
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Dataset text must literally contain its gold evidence, so scoring cannot drift from the corpus.
/// 数据集的原文必须字面包含金标证据，评分不会与语料脱节。
fn verified_corpus(dataset: &Value) -> BTreeMap<String, String> {
    let mut corpus = BTreeMap::new();
    for source in dataset["sources"].as_array().unwrap() {
        let id = source["sourceId"].as_str().unwrap().to_owned();
        let text = source["text"].as_str().unwrap().to_owned();
        assert!(
            corpus.insert(id.clone(), text).is_none(),
            "重复的资料身份：{id}"
        );
    }
    for query in dataset["queries"].as_array().unwrap() {
        let query_id = query["queryId"].as_str().unwrap();
        let no_answer = query["noAnswer"] == true;
        let gold = query["gold"].as_array().unwrap();
        assert_eq!(
            no_answer,
            gold.is_empty(),
            "{query_id} 的无答案标记与空金标不一致"
        );
        for item in gold {
            let source_id = item["sourceId"].as_str().unwrap();
            let text = corpus
                .get(source_id)
                .unwrap_or_else(|| panic!("{query_id} 引用了不存在的资料 {source_id}"));
            for evidence in item["evidence"].as_array().unwrap() {
                let evidence = evidence.as_str().unwrap();
                assert!(
                    text.contains(evidence),
                    "{query_id} 的金标证据不在原文中：{evidence}"
                );
            }
        }
    }
    corpus
}

/// Publishing reuses the production source contract; the lexical baseline supplies no query vectors, so it
/// measures the lexical, literal, symbol and path channels plus their weighted fusion.
/// 发布复用正式资料合同；词法基线不提供查询向量，因此度量词法、字面、符号、路径四路召回及其加权融合。
fn published_sources(dataset: &Value) -> Vec<Value> {
    dataset["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            json!({
                "sourceId": source["sourceId"],
                "scopeKey": "user",
                "sourceType": "document",
                "title": source["title"],
                "text": source["text"],
                "locator": {"path": source["locatorPath"]},
            })
        })
        .collect()
}

/// Raw fused candidates stay available so a rerank column can reorder the same rows the production stage sees.
/// 保留融合后的原始候选，重排列才能对生产阶段看到的同一批结果重新排序。
fn search_items(config: &GatewayConfig, query: &Value, extra: &Value) -> Vec<Value> {
    let mut input = json!({
        "query": query["query"],
        "scopeKeys": ["user"],
        "channelCandidates": 64,
        "limit": 96,
        "retrievalIntent": {},
        "ann": {},
    });
    if let Some(fields) = extra.as_object() {
        for (key, value) in fields {
            input[key] = value.clone();
        }
    }
    let response = gateway_index::search(config, &input).unwrap();
    response["items"].as_array().cloned().unwrap_or_default()
}

/// Ranked document identities keep the fused order and drop repeated chunks of the same document.
/// 排序结果保留融合顺序，并折叠同一文档的重复分块。
fn fold_documents(items: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .iter()
        .filter_map(|item| item["sourceId"].as_str())
        .filter(|id| seen.insert((*id).to_owned()))
        .map(|id| id.to_owned())
        .collect()
}

fn ranked_documents(config: &GatewayConfig, query: &Value, extra: &Value) -> Vec<String> {
    fold_documents(&search_items(config, query, extra))
}

/// Binary gains use the standard log2 discount so the score matches the shared metric definition.
/// 二值收益使用标准 log2 折扣，与共享指标定义保持一致。
fn discounted_gain(ranked: &[String], gold: &HashSet<String>, cutoff: usize) -> f64 {
    ranked
        .iter()
        .take(cutoff)
        .enumerate()
        .filter(|(_, id)| gold.contains(id.as_str()))
        .map(|(index, _)| 1.0 / ((index as f64 + 2.0).log2()))
        .sum()
}

fn ndcg_at(ranked: &[String], gold: &HashSet<String>, cutoff: usize) -> f64 {
    if gold.is_empty() {
        return 0.0;
    }
    let ideal = (0..gold.len().min(cutoff))
        .map(|index| 1.0 / ((index as f64 + 2.0).log2()))
        .sum::<f64>();
    // Clamp to zero so an empty hit list never reports a negative-zero score in the report.
    // 归零下限保证无命中时报告里不会出现负零分数。
    (discounted_gain(ranked, gold, cutoff) / ideal).max(0.0)
}

fn recall_at(ranked: &[String], gold: &HashSet<String>, cutoff: usize) -> f64 {
    if gold.is_empty() {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(cutoff)
        .filter(|id| gold.contains(id.as_str()))
        .count();
    hits as f64 / gold.len() as f64
}

fn reciprocal_rank_at(ranked: &[String], gold: &HashSet<String>, cutoff: usize) -> f64 {
    ranked
        .iter()
        .take(cutoff)
        .position(|id| gold.contains(id.as_str()))
        .map(|index| 1.0 / (index as f64 + 1.0))
        .unwrap_or(0.0)
}

/// One report builder serves every mode, so lexical, hybrid and diagnostic rankings share one metric definition.
/// 同一报告装配服务所有模式，词法、混合与诊断排序因此共享一套指标定义。
fn score(
    dataset: &Value,
    rankings: &BTreeMap<String, Vec<String>>,
    scope: &str,
    index: &Value,
) -> Value {
    let mut observations = Vec::new();
    for query in dataset["queries"].as_array().unwrap() {
        let query_id = query["queryId"].as_str().unwrap();
        let ranked = rankings.get(query_id).cloned().unwrap_or_default();
        let gold = query["gold"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["sourceId"].as_str().unwrap().to_owned())
            .collect::<HashSet<_>>();
        observations.push(json!({
            "queryId": query_id,
            "query": query["query"],
            "noAnswer": query["noAnswer"] == true,
            "diagnosticOnly": query["diagnosticOnly"] == true,
            "ranked": ranked,
            "gold": gold.iter().cloned().collect::<Vec<_>>(),
            "ndcgAt10": ndcg_at(&ranked, &gold, CUTOFF),
            "recallAt10": recall_at(&ranked, &gold, CUTOFF),
            "reciprocalRankAt10": reciprocal_rank_at(&ranked, &gold, CUTOFF),
            // A no-answer query reports whether the chain returned neighbours; it is not an answer-refusal score.
            // 无答案查询只记录是否仍返回无关邻居，不代表生成拒答能力。
            "emptyResult": ranked.is_empty(),
        }));
    }
    let primary = observations
        .iter()
        .filter(|entry| entry["noAnswer"] != true && entry["diagnosticOnly"] != true)
        .cloned()
        .collect::<Vec<_>>();
    assert!(!primary.is_empty(), "评估集没有可计分的查询");
    let mean = |field: &str| {
        primary
            .iter()
            .map(|entry| entry[field].as_f64().unwrap())
            .sum::<f64>()
            / primary.len() as f64
    };
    let misses = primary
        .iter()
        .filter(|entry| entry["ndcgAt10"].as_f64().unwrap() < 1.0)
        .map(|entry| entry["queryId"].clone())
        .collect::<Vec<_>>();
    // Diagnostic queries are paraphrases the lexical channels are known to miss; they are reported, not scored.
    // 诊断项是已知会被词法通道漏掉的口语化改写；只报告，不计入主指标。
    let diagnostic = observations
        .iter()
        .filter(|entry| entry["diagnosticOnly"] == true)
        .cloned()
        .collect::<Vec<_>>();
    let diagnostic_ndcg = if diagnostic.is_empty() {
        Value::Null
    } else {
        json!(diagnostic
            .iter()
            .map(|entry| entry["ndcgAt10"].as_f64().unwrap())
            .sum::<f64>()
            / diagnostic.len() as f64)
    };
    json!({
        "scope":scope,
        "index":{"sources":index["sources"],"chunks":index["chunks"]},
        "primaryQueryCount":primary.len(),
        "ndcgAt10":mean("ndcgAt10"),
        "recallAt10":mean("recallAt10"),
        "mrrAt10":mean("reciprocalRankAt10"),
        "queriesBelowPerfectNdcg":misses,
        "diagnosticQueryCount":diagnostic.len(),
        "diagnosticNdcgAt10":diagnostic_ndcg,
        "observations":observations,
    })
}

fn dataset_header(dataset: &Value) -> Value {
    json!({
        "schemaVersion":1,
        "datasetId":dataset["datasetId"],
        "datasetVersion":dataset["datasetVersion"],
        "partition":dataset["partition"],
        "cutoff":CUTOFF,
    })
}

fn write_report(variable: &str, report: &Value) {
    if let Some(path) = std::env::var_os(variable) {
        std::fs::write(
            std::path::Path::new(&path),
            serde_json::to_vec_pretty(report).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn native_retrieval_baseline_scores_against_frozen_gold() {
    let dataset: Value = serde_json::from_str(DATASET).unwrap();
    let _corpus = verified_corpus(&dataset);
    let workspace = Workspace::new();
    gateway_index::publish(&workspace.config, &published_sources(&dataset)).unwrap();
    let index = gateway_index::status(&workspace.config).unwrap();
    let mut rankings = BTreeMap::new();
    for query in dataset["queries"].as_array().unwrap() {
        rankings.insert(
            query["queryId"].as_str().unwrap().to_owned(),
            ranked_documents(&workspace.config, query, &json!({})),
        );
    }
    let scored = score(
        &dataset,
        &rankings,
        "production fused ranking (lexical, literal, symbol and path channels) without query vectors",
        &index,
    );
    let mut report = dataset_header(&dataset);
    report["lexical"] = scored.clone();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    write_report("KYNXA_RETRIEVAL_EVALUATION_REPORT", &report);
    let ndcg = scored["ndcgAt10"].as_f64().unwrap();
    let recall = scored["recallAt10"].as_f64().unwrap();
    let misses = scored["queriesBelowPerfectNdcg"].clone();
    assert!(
        ndcg >= BASELINE_NDCG,
        "nDCG@{CUTOFF} 低于记录基线 {BASELINE_NDCG}：{ndcg:.4}；未命中查询：{misses}"
    );
    assert!(
        recall >= BASELINE_RECALL,
        "Recall@{CUTOFF} 低于记录基线 {BASELINE_RECALL}：{recall:.4}；未命中查询：{misses}"
    );
}

/// Semantic entry: real local E5 passage and query embeddings through the production hybrid fusion.
/// 语义入口：使用真实本地 E5 的分块与查询嵌入，走生产混合融合。
///
/// The vector-only column is an in-evaluation diagnostic computed from the published chunk vectors; it is not a
/// production channel and exists to explain how much the fusion loses relative to pure cosine ordering.
/// 纯向量列是评估内基于已发布分块向量计算的诊断结果，不是生产通道，只用于说明融合相对纯余弦排序损失多少。
#[test]
#[ignore = "requires KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT pointing to the prepared Windows x64 payload"]
fn native_retrieval_semantic_baseline_scores_against_frozen_gold() {
    use kynxa_agent_core_native::models::gateway_inference::{self, EmbedKind, InferenceOptions};
    use std::sync::{Arc, atomic::AtomicBool};

    let bundle_root = std::env::var_os("KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT")
        .map(PathBuf::from)
        .expect("Select a prepared payload with KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT");
    let embedding_root = bundle_root.join("runtime/embedding/builtin-multilingual");
    gateway_inference::initialize_runtime(&bundle_root.join("runtime/native/onnxruntime.dll"))
        .unwrap();
    let dataset: Value = serde_json::from_str(DATASET).unwrap();
    let _corpus = verified_corpus(&dataset);
    let mut workspace = Workspace::new();
    // The precise cosine path loads the packaged sqlite-vec extension; an isolated root has no copy.
    // 精确余弦路径需要加载随包 sqlite-vec 扩展，隔离数据根里没有该文件。
    workspace.config.vector_extension = bundle_root.join("runtime/native/vec0.dll");
    let cancel = Arc::new(AtomicBool::new(false));
    let options = InferenceOptions::default();
    let sources = published_sources(&dataset);

    // Prepare once to obtain the exact production chunk boundaries and embedding projections.
    // 先按生产规则切块，取得与生产一致的分块边界和嵌入投影输入。
    let topology = sources
        .iter()
        .map(|source| gateway_index::prepare_source(source, None).unwrap())
        .collect::<Vec<_>>();
    let mut inputs = Vec::new();
    for prepared in &topology {
        for chunk in prepared["chunks"].as_array().unwrap() {
            let projection: Value =
                serde_json::from_str(chunk["projectionJson"].as_str().unwrap_or("{}"))
                    .unwrap_or(json!({}));
            inputs.push(format!(
                "{}{}",
                projection["context"].as_str().unwrap_or(""),
                chunk["text"].as_str().unwrap_or("")
            ));
        }
    }
    let passages =
        gateway_inference::embed_with_options(&embedding_root, &inputs, EmbedKind::Passage, &options, cancel.clone())
            .unwrap();
    assert_eq!(passages.vectors.len(), inputs.len(), "分块嵌入数量与分块不一致");
    let profile = passages.profile_id.clone();
    let model_version = passages.model_version.clone();
    let space = passages.space_id.clone();
    let dimensions = passages.dimensions;

    // Re-prepare each source with its own vectors so the published space matches the query space.
    // 每个来源带上自己的向量重新准备，保证发布空间与查询空间一致。
    let mut prepared = Vec::new();
    let mut chunk_vectors: Vec<(String, Vec<f32>)> = Vec::new();
    let mut offset = 0;
    for (source, topology) in sources.iter().zip(&topology) {
        let count = topology["chunks"].as_array().unwrap().len();
        let vectors = passages.vectors[offset..offset + count].to_vec();
        offset += count;
        for vector in &vectors {
            chunk_vectors.push((source["sourceId"].as_str().unwrap().to_owned(), vector.clone()));
        }
        let mut sourced = source.clone();
        sourced["embeddingProfileId"] = json!(profile);
        sourced["embeddingModelVersion"] = json!(model_version);
        sourced["embeddingSpaceId"] = json!(space);
        prepared.push(gateway_index::prepare_source(&sourced, Some(&vectors)).unwrap());
    }
    gateway_index::publish_prepared(&workspace.config, &prepared).unwrap();
    let index = gateway_index::status(&workspace.config).unwrap();

    let mut hybrid = BTreeMap::new();
    let mut vector_only = BTreeMap::new();
    let mut reranked = BTreeMap::new();
    let mut rerank_profile = Value::Null;
    let reranker_root = bundle_root.join("runtime/rerank/builtin-multilingual-reranker");
    let rerank_started = std::time::Instant::now();
    let mut rerank_calls = 0_usize;
    let mut rerank_first_ms = f64::NAN;
    for query in dataset["queries"].as_array().unwrap() {
        let query_id = query["queryId"].as_str().unwrap().to_owned();
        let text = query["query"].as_str().unwrap().to_string();
        let batch = gateway_inference::embed_with_options(
            &embedding_root,
            std::slice::from_ref(&text),
            EmbedKind::Query,
            &options,
            cancel.clone(),
        )
        .unwrap();
        let query_vector = batch.vectors[0].clone();
        assert_eq!(query_vector.len(), dimensions, "查询向量维度与分块不一致");
        let extra = json!({
            "queryVector":&query_vector,
            "embeddingProfileId":batch.profile_id,
            "embeddingModelVersion":batch.model_version,
            "embeddingSpaceId":batch.space_id,
            "ann":{"mode":"exact"},
        });
        let items = search_items(&workspace.config, query, &extra);
        hybrid.insert(query_id.clone(), fold_documents(&items));
        let mut scored = chunk_vectors
            .iter()
            .map(|(source, vector)| (cosine(query_vector.as_slice(), vector), source.clone()))
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| right.0.total_cmp(&left.0));
        let mut seen = HashSet::new();
        vector_only.insert(
            query_id.clone(),
            scored
                .into_iter()
                .filter(|(_, source)| seen.insert(source.clone()))
                .map(|(_, source)| source)
                .collect::<Vec<_>>(),
        );
        // The rerank column mirrors the production stage: same model, same excerpt text, same 20-candidate cap.
        // 重排列复现生产阶段：同一模型、同一摘录文本、同一 20 条候选上限。
        let count = items.len().min(RERANK_CANDIDATES);
        if count == 0 {
            reranked.insert(query_id, Vec::new());
            continue;
        }
        let texts = items[..count]
            .iter()
            .map(|item| item["excerpt"].as_str().unwrap_or("").to_owned())
            .collect::<Vec<_>>();
        // The first call carries model load; steady-state latency is what a chat turn actually pays.
        // 首次调用包含模型加载；聊天回合实际承担的是稳态延迟。
        let call_started = std::time::Instant::now();
        let ordered =
            gateway_inference::rerank_blocking(&reranker_root, &text, &texts, &options, cancel.clone())
                .unwrap();
        if rerank_first_ms.is_nan() {
            rerank_first_ms = call_started.elapsed().as_secs_f64() * 1000.0;
        }
        rerank_calls += 1;
        if rerank_profile.is_null() {
            rerank_profile = json!({"profile":ordered.profile_id,"modelVersion":ordered.model_version,
                "dimensions":ordered.scores.len(),"truncatedInputsCount":ordered.truncated_inputs_count,
                "device":ordered.inference_status["device"]});
        }
        let mut order = (0..count).collect::<Vec<_>>();
        order.sort_by(|left, right| ordered.scores[*right].total_cmp(&ordered.scores[*left]));
        let mut sorted = order
            .into_iter()
            .map(|index| items[index].clone())
            .collect::<Vec<_>>();
        sorted.extend(items[count..].iter().cloned());
        reranked.insert(query_id, fold_documents(&sorted));
    }
    let hybrid_scored = score(
        &dataset,
        &hybrid,
        "production hybrid fusion (lexical, literal, symbol, path and vector channels) with local E5 query vectors",
        &index,
    );
    let vector_scored = score(
        &dataset,
        &vector_only,
        "diagnostic pure-cosine ordering computed from published chunk vectors; not a production channel",
        &index,
    );
    let reranked_scored = score(
        &dataset,
        &reranked,
        "diagnostic rerank of the hybrid candidates through the packaged BGE model; the production stage additionally requires resource-service admission",
        &index,
    );
    let mut report = dataset_header(&dataset);
    report["embedding"] = json!({
        "profile":profile,"modelVersion":model_version,"spaceId":space,"dimensions":dimensions,
        "device":passages.inference_status["device"],"vectorCount":chunk_vectors.len(),
    });
    let rerank_total_ms = rerank_started.elapsed().as_secs_f64() * 1000.0;
    report["rerank"] = json!({"candidateLimit":RERANK_CANDIDATES,"assets":rerank_profile,
        "calls":rerank_calls,"elapsedMs":rerank_total_ms,"firstCallMs":rerank_first_ms,
        "steadyStateMs":if rerank_calls > 1 { (rerank_total_ms - rerank_first_ms) / (rerank_calls - 1) as f64 } else { f64::NAN },
        "note":"diagnostic replication through rerank_blocking; the production stage also waits for resource-service admission"});
    report["hybrid"] = hybrid_scored.clone();
    report["vectorOnly"] = vector_scored.clone();
    report["reranked"] = reranked_scored.clone();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    write_report("KYNXA_RETRIEVAL_SEMANTIC_REPORT", &report);
    let hybrid_ndcg = hybrid_scored["ndcgAt10"].as_f64().unwrap();
    let hybrid_diagnostic = hybrid_scored["diagnosticNdcgAt10"].as_f64();
    assert!(
        hybrid_ndcg >= SEMANTIC_BASELINE_NDCG,
        "混合 nDCG@{CUTOFF} 低于记录基线 {SEMANTIC_BASELINE_NDCG}：{hybrid_ndcg:.4}"
    );
    assert!(
        hybrid_diagnostic.is_some_and(|value| value >= SEMANTIC_BASELINE_DIAGNOSTIC),
        "语义诊断项低于记录基线 {SEMANTIC_BASELINE_DIAGNOSTIC}：{hybrid_diagnostic:?}"
    );
    // Reranking must not make the fused order worse; the recorded floor follows the first measurement.
    // 重排不得使融合顺序变差；记录下限取自首次实测。
    let reranked_ndcg = reranked_scored["ndcgAt10"].as_f64().unwrap();
    assert!(
        reranked_ndcg >= RERANK_BASELINE_NDCG,
        "重排 nDCG@{CUTOFF} 低于记录基线 {RERANK_BASELINE_NDCG}：{reranked_ndcg:.4}"
    );
}

/// Rows are L2-normalised by the model, so the dot product is the cosine similarity.
/// 模型输出已做 L2 归一化，点积即余弦相似度。
fn cosine(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum()
}
