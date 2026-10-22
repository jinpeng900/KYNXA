//! Opt-in public-corpus evaluation through current native chunking, E5 and hybrid ranking.
//! 显式启用的公开语料评测，复用当前原生分块、E5 与混合排序，不访问用户数据或账号。

use kynxa_agent_core_native::{
    gateway_contract::{GatewayConfig, new_id},
    models::gateway_inference::{self, EmbedKind, InferenceOptions},
    retrieval::{gateway_index, token_fit},
    storage::sqlite_connections,
    tools::gateway::{self as tool_gateway, GatewayToolContext, ToolRuntime},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

const NATIVE_DATASET: &str = include_str!("../../../tests/retrieval-benchmark/datasets/native-baseline.json");
const MODEL_MANIFEST: &str = include_str!("../../desktop/Build/native-assets/embedding.json");
const CORPUS_SHA256: &str = "dec31c8182f3d744c7d2c09423756fd1d17cbef75808db13ba01cc0aab4d1ac6";
const QUERIES_SHA256: &str = "8ff84a7c903f722981cd8d595c022660140c51867b27608a6d4910db86080313";
const QRELS_SHA256: &str = "0864bb985e0ca2367ba217977e72004d549054b2b06666ed9d4825ac7c21284c";

fn digest(bytes: impl AsRef<[u8]>) -> String { format!("{:x}", Sha256::digest(bytes.as_ref())) }
fn file_digest(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap(); let mut hash = Sha256::new(); let mut buffer = [0_u8;65536];
    loop { let count=file.read(&mut buffer).unwrap(); if count==0 { break; } hash.update(&buffer[..count]); }
    format!("{:x}",hash.finalize())
}
fn evaluation_rrf_k() -> u64 {
    let k=std::env::var("KYNXA_SCIFACT_RRF_K").map_or(60,|value|value.parse::<u64>().expect("KYNXA_SCIFACT_RRF_K must be 20..100"));
    assert!((20..=100).contains(&k),"KYNXA_SCIFACT_RRF_K must be 20..100"); k
}
fn dataset_identity() -> Value { json!({"corpus.jsonl":CORPUS_SHA256,"queries.jsonl":QUERIES_SHA256,"qrels/test.tsv":QRELS_SHA256}) }
fn prepared_code_identity() -> Value {
    // Conservatively bind source preparation to its implementation; RRF parameter values stay separate.
    // 保守绑定来源制备实现，RRF 参数值单独处理，开发集变更参数不需要重算向量。
    let files: [(&str,&[u8]);8] = [
        ("retrieval/gateway_index.rs",include_bytes!("../src/retrieval/gateway_index.rs")),
        ("retrieval/gateway_structure.rs",include_bytes!("../src/retrieval/gateway_structure.rs")),
        ("retrieval/chunking.rs",include_bytes!("../src/retrieval/chunking.rs")),
        ("retrieval/token_fit.rs",include_bytes!("../src/retrieval/token_fit.rs")),
        ("retrieval/document.rs",include_bytes!("../src/retrieval/document.rs")),
        ("retrieval/lexical.rs",include_bytes!("../src/retrieval/lexical.rs")),
        ("models/gateway_inference.rs",include_bytes!("../src/models/gateway_inference.rs")),
        ("models/document_fit.rs",include_bytes!("../src/models/document_fit.rs")),
    ];
    json!(files.into_iter().map(|(path,bytes)|(path,digest(bytes))).collect::<BTreeMap<_,_>>())
}
fn model_identity(metadata: &Value) -> Value {
    let mut fields=serde_json::Map::new();
    for key in ["profileId","modelVersion","spaceId","dimensions","modelAssetSignature","assetSignature"] {
        assert!(!metadata[key].is_null(),"missing model identity: {key}"); fields.insert(key.into(),metadata[key].clone());
    }
    Value::Object(fields)
}
fn snapshot_counts(path: &Path, metadata: &Value, corpus: &[Value]) -> Value {
    let connection=rusqlite::Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let quick:String=connection.query_row("PRAGMA quick_check",[],|row|row.get(0)).unwrap(); assert_eq!(quick,"ok");
    let expected=corpus.iter().map(|item|(format!("beir:scifact:{}",item["_id"].as_str().unwrap()),digest(format!("{}\n\n{}",item["title"].as_str().unwrap(),item["text"].as_str().unwrap())))).collect::<HashMap<_,_>>();
    assert_eq!(expected.len(),5183);
    let mut statement=connection.prepare("SELECT source_id,scope_key,content_hash,text,parser_version,chunker_version,tokenizer_version,embedding_input_version FROM sources").unwrap();
    let rows=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,row.get::<_,String>(7)?))).unwrap();
    let mut sources=0_usize;
    for row in rows {
        let (id,scope,hash,text,parser,chunker,tokenizer,input)=row.unwrap();
        assert_eq!(expected.get(&id),Some(&hash),"prepared source content does not match original corpus: {id}");
        assert_eq!(digest(text),hash); assert_eq!(scope,"user");
        assert_eq!(parser,"rust-document-blocks-v1"); assert_eq!(chunker,"structure-lines-v1");
        assert_eq!(tokenizer,"han-bigram-code-tf-v2"); assert_eq!(input,"source-context-v1"); sources+=1;
    }
    assert_eq!(sources,5183); drop(statement);
    let chunks:i64=connection.query_row("SELECT count(*) FROM chunks",[],|row|row.get(0)).unwrap();
    let vectors:i64=connection.query_row("SELECT count(*) FROM chunks WHERE vector IS NOT NULL",[],|row|row.get(0)).unwrap();
    assert!(chunks>0); assert_eq!(chunks,vectors);
    let compatible:i64=connection.query_row("SELECT count(*) FROM chunks WHERE embedding_profile_id=?1 AND embedding_model_version=?2 AND embedding_space_id=?3 AND dimensions=384 AND length(vector)=1536 AND chunker_version='token-fitted-units-v1|structured-units-v1' AND tokenizer_version='han-bigram-code-tf-v2'",
        rusqlite::params![metadata["profileId"].as_str().unwrap(),metadata["modelVersion"].as_str().unwrap(),metadata["spaceId"].as_str().unwrap()],|row|row.get(0)).unwrap();
    assert_eq!(compatible,chunks,"prepared index contains stale vector spaces or chunk versions");
    let other_spaces:i64=connection.query_row("SELECT count(*) FROM chunk_vector_spaces WHERE embedding_profile_id<>?1 OR embedding_model_version<>?2 OR embedding_space_id<>?3 OR dimensions<>384 OR length(vector)<>1536",
        rusqlite::params![metadata["profileId"].as_str().unwrap(),metadata["modelVersion"].as_str().unwrap(),metadata["spaceId"].as_str().unwrap()],|row|row.get(0)).unwrap();
    assert_eq!(other_spaces,0,"prepared index retains another vector space");
    json!({"sources":sources,"chunks":chunks,"vectors":vectors})
}
fn load_prepared_index(config: &GatewayConfig, source: &Path, metadata: &Value, corpus: &[Value]) -> (usize,Value) {
    let directory=source.parent().unwrap();
    let manifest_path=directory.join("scifact-native-index.json");
    let manifest:Value=serde_json::from_slice(&fs::read(&manifest_path).expect("prepared index requires scifact-native-index.json beside the closed SQLite snapshot")).unwrap();
    assert_eq!(manifest["schemaVersion"],1); assert_eq!(manifest["datasetSha256"],dataset_identity());
    assert_eq!(manifest["model"],model_identity(metadata)); assert_eq!(manifest["modelManifestSha256"],digest(MODEL_MANIFEST));
    assert_eq!(manifest["sourceCodeSha256"],prepared_code_identity(),"prepared source/chunker implementation changed; recompute the public index");
    let before=file_digest(source); assert_eq!(manifest["databaseSha256"],before,"prepared SQLite snapshot checksum changed");
    assert_eq!(snapshot_counts(source,metadata,corpus),manifest["counts"]);
    let target=gateway_index::database(config); assert!(!target.exists()); fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(source,&target).unwrap();
    assert_eq!(file_digest(&target),before,"snapshot copy checksum mismatch"); assert_eq!(file_digest(source),before,"snapshot changed during copy");
    assert_eq!(snapshot_counts(&target,metadata,corpus),manifest["counts"]);
    (manifest["counts"]["chunks"].as_u64().unwrap() as usize,json!({"preparedIndexReused":true,"databaseSha256":before,
        "validatedCounts":manifest["counts"],"sourceCodeSha256":manifest["sourceCodeSha256"],"vectorsComputed":0,"queryVectorsStillComputed":true}))
}
fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| panic!("Set {name} to an existing public dataset or prepared payload."))
}
fn json_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path).unwrap().lines().filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap()).collect()
}

struct Workspace { root: PathBuf, config: GatewayConfig }
impl Workspace {
    fn new(bundle: &Path) -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-scifact-native-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        let config = GatewayConfig {
            data_root: root.join("Data"), data_home: root.join("Models"), extension_root: root.join("Extensions"),
            application_root: bundle.to_owned(), port: 0, toolhost_exe: root.join("unused-toolhost.exe"),
            vector_extension: bundle.join("runtime/native/vec0.dll"), inference_library: bundle.join("runtime/native/onnxruntime.dll"),
        };
        Self { root, config }
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = sqlite_connections::close(&self.config.data_root, &gateway_index::database(&self.config));
        // This unique fixture is the only writable root; public assets and vector caches stay read-only.
        // 只删除本次唯一夹具根；公开资产与历史向量缓存始终只读。
        if let Err(error) = fs::remove_dir_all(&self.root) { eprintln!("fixture cleanup: {}: {error}", self.root.display()); }
    }
}

#[derive(Default)]
struct VectorBank {
    vectors: HashMap<String, Vec<f32>>, validated_files: usize, rejected_files: usize,
    reused: usize, computed: usize, parity: Value,
}
impl VectorBank {
    fn load(root: Option<PathBuf>, metadata: &Value) -> Self {
        let mut bank = Self::default();
        let Some(root) = root else { return bank; };
        let mut manifests = Vec::new();
        for directory in fs::read_dir(root).unwrap().flatten().filter(|entry| entry.path().is_dir()) {
            for entry in fs::read_dir(directory.path()).unwrap().flatten() {
                if entry.path().extension().is_some_and(|extension| extension == "json") { manifests.push(entry.path()); }
            }
        }
        manifests.sort();
        for path in manifests {
            let result = (|| -> Option<Vec<(String, Vec<f32>)>> {
                let manifest: Value = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
                let identity = &manifest["identity"];
                if identity["modelIdentity"]["modelId"] != metadata["modelId"]
                    || identity["modelIdentity"]["modelVersion"] != metadata["modelVersion"]
                    || identity["modelIdentity"]["queryPrefix"] != "query: "
                    || identity["modelIdentity"]["documentPrefix"] != "passage: "
                    || identity["inputVersion"] != "source-context-v1"
                    || identity["dimensions"] != 384 || identity["encoding"] != "float32-little-endian" { return None; }
                let inputs = manifest["inputs"].as_array()?;
                let binary = fs::read(path.with_extension("bin")).ok()?;
                if inputs.len() != manifest["vectorCount"].as_u64()? as usize
                    || binary.len() != inputs.len() * 384 * 4
                    || manifest["vectorSha256"] != digest(&binary) { return None; }
                let mut values = Vec::new();
                for (index, input) in inputs.iter().enumerate() {
                    let text = input["input"].as_str()?;
                    let key = digest(text);
                    if input["inputSha256"] != key { return None; }
                    let vector = binary[index * 1536..(index + 1) * 1536].chunks_exact(4)
                        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())).collect::<Vec<_>>();
                    let norm: f64 = vector.iter().map(|value| f64::from(*value).powi(2)).sum();
                    if vector.iter().any(|value| !value.is_finite()) || (norm - 1.0).abs() > 1e-3 { return None; }
                    values.push((key, vector));
                }
                Some(values)
            })();
            if let Some(values) = result {
                bank.validated_files += 1;
                for (key, vector) in values { bank.vectors.entry(key).or_insert(vector); }
            } else { bank.rejected_files += 1; }
        }
        bank
    }

    fn embed(&mut self, root: &Path, texts: &[String], options: &InferenceOptions, cancel: &Arc<AtomicBool>) -> Vec<Vec<f32>> {
        let mut output = texts.iter().map(|text| self.vectors.get(&digest(text)).cloned()).collect::<Vec<_>>();
        // Validate an exact cache hit against this runtime before trusting historical CPU vectors.
        // 采用历史 CPU 向量前，用当前运行时核验实际输入完全一致的一条命中。
        if self.parity.is_null() {
            if let Some(index) = output.iter().position(Option::is_some) {
                let actual = gateway_inference::embed_with_options(root, &texts[index..index + 1], EmbedKind::Passage, options, cancel.clone()).unwrap();
                let cached = output[index].as_ref().unwrap();
                let difference = cached.iter().zip(&actual.vectors[0]).map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs()).fold(0.0_f64, f64::max);
                let dot: f64 = cached.iter().zip(&actual.vectors[0]).map(|(a, b)| f64::from(*a) * f64::from(*b)).sum();
                let accepted = difference <= 1e-4 && dot >= 0.99999;
                self.parity = json!({"inputSha256":digest(&texts[index]),"maxAbs":difference,"cosine":dot,"accepted":accepted});
                if !accepted { self.vectors.clear(); output.fill(None); }
            }
        }
        self.reused += output.iter().filter(|value| value.is_some()).count();
        let missing = output.iter().enumerate().filter_map(|(index, value)| value.is_none().then_some(index)).collect::<Vec<_>>();
        self.computed += missing.len();
        for indices in missing.chunks(32) {
            let batch = indices.iter().map(|index| texts[*index].clone()).collect::<Vec<_>>();
            let actual = gateway_inference::embed_with_options(root, &batch, EmbedKind::Passage, options, cancel.clone()).unwrap();
            for (index, vector) in indices.iter().zip(actual.vectors) { output[*index] = Some(vector); }
        }
        output.into_iter().map(Option::unwrap).collect()
    }
    fn audit(&self) -> Value {
        json!({"validatedManifestCount":self.validated_files,"incompatibleOrInvalidManifestCount":self.rejected_files,
            "cachedVectorsReused":self.reused,"vectorsComputed":self.computed,"runtimeParity":self.parity,
            "matching":"verified model identity/assets, exact current prefixed input, float32 checksum and norm; no gold-derived vectors"})
    }
}

fn publish(config: &GatewayConfig, sources: &[Value], model: &Path, metadata: &Value, bank: &mut VectorBank, options: &InferenceOptions, cancel: &Arc<AtomicBool>) -> usize {
    let mut pending = Vec::new(); let mut chunk_count = 0;
    for (index, source) in sources.iter().enumerate() {
        let prepared = gateway_index::prepare_source(source, None).unwrap();
        let chunks = prepared["chunks"].as_array().unwrap();
        let projections = chunks.iter().map(|chunk| gateway_index::embedding_projection(&prepared["source"], chunk)).collect::<Vec<_>>();
        let fits = gateway_inference::fit_documents(model, &projections, cancel.clone()).unwrap();
        let mut fitted = Vec::new();
        for ((chunk, projection), fit) in chunks.iter().zip(&projections).zip(&fits) {
            let mut segments = token_fit::apply("retrieval.token-fit", &json!({"source":source,"chunk":chunk,"projection":projection,
                "document":fit,"chunkOffset":fitted.len(),"maxInputTokens":512,"fittingVersion":"real-tokenizer-512-v1"})).unwrap();
            for segment in segments.as_array_mut().unwrap() { segment["structure"] = chunk["structure"].clone(); }
            fitted.extend(segments.as_array().unwrap().iter().cloned());
        }
        let inputs = fitted.iter().map(|chunk| token_fit::apply("retrieval.fitted-text", &json!({"chunk":chunk})).unwrap().as_str().unwrap().to_owned()).collect::<Vec<_>>();
        let vectors = bank.embed(model, &inputs, options, cancel);
        let mut current = source.clone();
        current["chunks"] = json!(fitted); current["embeddingProfileId"] = metadata["profileId"].clone();
        current["embeddingModelVersion"] = metadata["modelVersion"].clone(); current["embeddingSpaceId"] = metadata["spaceId"].clone();
        chunk_count += vectors.len();
        pending.push(gateway_index::prepare_source(&current, Some(&vectors)).unwrap());
        if pending.len() >= 64 || index + 1 == sources.len() {
            gateway_index::publish_prepared(config, &pending).unwrap(); pending.clear();
        }
        if (index + 1) % 256 == 0 { eprintln!("native indexing {}/{} sources; reused={} computed={}", index + 1, sources.len(), bank.reused, bank.computed); }
    }
    chunk_count
}

fn query(config: &GatewayConfig, model: &Path, metadata: &Value, options: &InferenceOptions, cancel: &Arc<AtomicBool>, text: &str, exact_symbol: Option<&str>) -> Value {
    let mut input = json!({"query":text,"scopeKeys":["user"],"channelCandidates":40,"limit":60,"rrfK":evaluation_rrf_k(),
        "retrievalIntent":{},"ann":{"mode":"exact"}});
    if let Some(symbol) = exact_symbol {
        // A technical identifier can occur in documentation, so do not invent a code-only scope.
        // 技术标识符也可能出现在文档中，不能凭名称把范围限制成仅代码。
        input["retrievalIntent"] = json!({"symbol":symbol});
    } else {
        // Bypass only the query-vector cache to include actual warm E5 inference in latency.
        // 只绕过查询向量缓存，延迟包含真实预热后 E5 推理，不更改模型输入。
        let embedding = gateway_inference::embed_with_options(model, &[text.to_owned()], EmbedKind::Query, options, cancel.clone()).unwrap();
        input["queryVector"] = json!(embedding.vectors[0]); input["embeddingProfileId"] = metadata["profileId"].clone();
        input["embeddingModelVersion"] = metadata["modelVersion"].clone(); input["embeddingSpaceId"] = metadata["spaceId"].clone();
    }
    gateway_index::search(config, &input).unwrap()
}
fn observation(response: &Value, gold: &HashSet<String>, elapsed: f64) -> Value {
    let items = response["items"].as_array().unwrap();
    let first_ten_item_documents = items.iter().take(10)
        .map(|item| item["sourceId"].as_str().unwrap().to_owned()).collect::<HashSet<_>>();
    let first_ten_hits = first_ten_item_documents.intersection(gold).count();
    let mut seen = HashSet::new();
    let ranked = items.iter().filter_map(|item| {
        let id = item["sourceId"].as_str().unwrap().to_owned(); seen.insert(id.clone()).then_some(id)
    }).take(10).collect::<Vec<_>>();
    let hits = ranked.iter().filter(|id| gold.contains(*id)).count();
    json!({"recallAt10":if gold.is_empty(){Value::Null}else{json!(hits as f64/gold.len() as f64)},
        "recallWithinFirst10ReturnedItems":if gold.is_empty(){Value::Null}else{json!(first_ten_hits as f64/gold.len() as f64)},
        "first10ReturnedItemDocuments":first_ten_item_documents,"returnedItemCount":items.len(),
        "top1":if gold.is_empty(){Value::Null}else{json!(ranked.first().is_some_and(|id| gold.contains(id)))},
        "elapsedMs":elapsed,"rankedDocumentsAt10":ranked,"gold":gold,"rankingAudit":response["ranking"]})
}
fn aggregate(items: &[Value]) -> Value {
    let labeled = items.iter().filter(|item| !item["recallAt10"].is_null()).collect::<Vec<_>>();
    let mut latencies = items.iter().map(|item| item["elapsedMs"].as_f64().unwrap()).collect::<Vec<_>>();
    latencies.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| latencies.get(((latencies.len() as f64 * fraction).ceil() as usize).saturating_sub(1)).copied();
    json!({"queryCount":items.len(),"labeledQueries":labeled.len(),
        "recallAt10":if labeled.is_empty(){Value::Null}else{json!(labeled.iter().map(|item| item["recallAt10"].as_f64().unwrap()).sum::<f64>()/labeled.len() as f64)},
        "recallWithinFirst10ReturnedItems":if labeled.is_empty(){Value::Null}else{json!(labeled.iter().map(|item| item["recallWithinFirst10ReturnedItems"].as_f64().unwrap()).sum::<f64>()/labeled.len() as f64)},
        "top1Accuracy":if labeled.is_empty(){Value::Null}else{json!(labeled.iter().filter(|item| item["top1"]==true).count() as f64/labeled.len() as f64)},
        "p50Ms":percentile(0.50),"p95Ms":percentile(0.95)})
}

#[cfg(windows)]
fn memory() -> Value {
    #[repr(C)]
    #[derive(Default)]
    struct Counters { cb: u32, faults: u32, peak_working: usize, working: usize, peak_paged: usize, paged: usize,
        peak_nonpaged: usize, nonpaged: usize, private: usize, peak_private: usize }
    #[link(name="kernel32")]
    unsafe extern "system" { fn GetCurrentProcess() -> *mut std::ffi::c_void; }
    #[link(name="psapi")]
    unsafe extern "system" { fn GetProcessMemoryInfo(process:*mut std::ffi::c_void, counters:*mut Counters, bytes:u32) -> i32; }
    let mut value = Counters { cb: std::mem::size_of::<Counters>() as u32, ..Default::default() };
    // The pseudo handle is borrowed and the initialized counters buffer lives through this synchronous call.
    // 伪句柄仅借用，已初始化的计数缓冲在同步调用期间始终有效。
    let success = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut value, std::mem::size_of::<Counters>() as u32) };
    if success == 0 { json!({"available":false}) } else {
        json!({"available":true,"workingSetBytes":value.working,"processPeakWorkingSetBytes":value.peak_working,
            "privateCommitBytes":value.private,"processPeakPrivateCommitBytes":value.peak_private})
    }
}
#[cfg(not(windows))]
fn memory() -> Value { json!({"available":false,"reason":"Windows x64 acceptance only"}) }

fn native_supplementary(bundle: &Path, model: &Path, metadata: &Value, options: &InferenceOptions, cancel: &Arc<AtomicBool>) -> Value {
    let dataset: Value = serde_json::from_str(NATIVE_DATASET).unwrap();
    let workspace = Workspace::new(bundle);
    let sources = dataset["sources"].as_array().unwrap().iter().map(|source| json!({
        "sourceId":source["sourceId"],"scopeKey":"user","sourceType":"document","title":source["title"],"text":source["text"],
        "locator":{"path":source["locatorPath"],"relativePath":source["locatorPath"]},"sourceRevision":1,
    })).collect::<Vec<_>>();
    let mut bank = VectorBank::default();
    publish(&workspace.config, &sources, model, metadata, &mut bank, options, cancel);
    let mut groups: BTreeMap<String,Vec<Value>> = BTreeMap::new();
    for record in dataset["queries"].as_array().unwrap() {
        if record["noAnswer"] == true { continue; }
        let id = record["queryId"].as_str().unwrap();
        let symbol = match id { "q2-vector-cosine"=>Some("vec_distance_cosine"), "q4-extract-signature"=>Some("extract"), _=>None };
        let gold = record["gold"].as_array().unwrap().iter().map(|item|item["sourceId"].as_str().unwrap().to_owned()).collect::<HashSet<_>>();
        let started = Instant::now();
        let response = query(&workspace.config, model, metadata, options, cancel, record["query"].as_str().unwrap(), symbol);
        let mut item = observation(&response, &gold, started.elapsed().as_secs_f64()*1000.0); item["queryId"]=record["queryId"].clone();
        let group = if symbol.is_some(){"technical-symbol-in-documents"}else if record["diagnosticOnly"]==true{"chinese-diagnostic"}else{"chinese-documents"};
        groups.entry(group.to_owned()).or_default().push(item);
    }
    // Freeze two derived two-source questions without rewriting the published native gold dataset.
    // 固定两道派生双来源题，原始 native 金标数据集不作改写。
    for (id, left, right) in [("multi-files-and-events","q1-manifest","q6-message-log-file"),("multi-memory-and-history","q7-draft-to-confirmed","q10-compression-originals")] {
        let records = dataset["queries"].as_array().unwrap();
        let left = records.iter().find(|item|item["queryId"]==left).unwrap();
        let right = records.iter().find(|item|item["queryId"]==right).unwrap();
        let text = format!("{}；{}",left["query"].as_str().unwrap(),right["query"].as_str().unwrap());
        let gold = left["gold"].as_array().unwrap().iter().chain(right["gold"].as_array().unwrap())
            .map(|item|item["sourceId"].as_str().unwrap().to_owned()).collect::<HashSet<_>>();
        assert_eq!(gold.len(),2);
        let started=Instant::now(); let response=query(&workspace.config,model,metadata,options,cancel,&text,None);
        let mut item=observation(&response,&gold,started.elapsed().as_secs_f64()*1000.0); item["queryId"]=json!(id);
        groups.entry("derived-multiple-documents".into()).or_default().push(item);
    }
    let summaries = groups.iter().map(|(group,items)|(group.clone(),aggregate(items))).collect::<BTreeMap<_,_>>();
    json!({"datasetSha256":digest(NATIVE_DATASET),"scope":"frozen synthetic development corpus; two derived multi-document questions; no public generalization claim",
        "technicalScope":"exact symbol lookup in documentation; real filesystem glob/regex coverage is tested separately",
        "groups":summaries,"observations":groups})
}

/// Locate actual code through the same model-facing filesystem dispatcher, with no model or index.
/// 通过模型使用的同一个文件工具入口定位真实代码，不使用模型、向量或预造检索结果。
#[tokio::test]
async fn native_code_locations_search_and_read_exact_lines_without_vectors() {
    let workspace = Workspace::new(Path::new("unused-code-fixture-application"));
    let work = workspace.root.join("work");
    for directory in [work.join("src"), work.join("ui"), work.join("generated"),
        workspace.root.join("unlinked"), workspace.config.data_root.clone(),
        workspace.config.extension_root.clone(), workspace.root.join("official")] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(work.join(".gitignore"), "generated/\n").unwrap();
    fs::write(work.join("src/math.rs"), "// arithmetic\npub fn add(left: i32, right: i32) -> i32 {\n    left + right\n}\n").unwrap();
    fs::write(work.join("src/workflow.rs"), "use crate::math::add;\npub fn run_total() -> i32 {\n    add(20, 22)\n}\n").unwrap();
    fs::write(work.join("ui/Catalog.cs"), "namespace Sample;\npublic static class Catalog\n{\n    public static string Slug(string name) => name.Trim().ToLowerInvariant();\n}\n").unwrap();
    fs::write(work.join("generated/ignored.rs"), "pub fn hidden_secret() {}\n").unwrap();
    let unlinked = workspace.root.join("unlinked/outside.rs");
    fs::write(&unlinked, "pub fn outside_scope() {}\n").unwrap();
    let (_cancellation_sender, cancellation) = tokio::sync::watch::channel(false);
    let context = GatewayToolContext {
        data_home: workspace.config.data_root.clone(), extension_root: workspace.config.extension_root.clone(),
        workdir: work.clone(), toolhost_exe: workspace.root.join("unused-toolhost.exe"),
        permission_mode: "smart".into(), conversation_id: new_id(), project_id: None,
        authorized_roots: vec![work.clone()], cancellation, runtime: Arc::new(ToolRuntime::default()),
        config: json!({"officialToolsRoot":workspace.root.join("official"),"mcpServers":[]}),
        approved: false, events: None, external_execute: None,
    };
    let mut observations = Vec::new();
    for (id, input, expected_path, expected_line, expected_text) in [
        ("known-rust-file", json!({"path":".","mode":"glob","query":"**/math.rs"}), "src/math.rs", 2_u64, "pub fn add("),
        ("rust-function-definition", json!({"path":"src","mode":"regex","query":r"^pub fn add\b"}), "src/math.rs", 2, "pub fn add("),
        ("csharp-method-definition", json!({"path":"ui","mode":"regex","query":r"public static string Slug\("}), "ui/Catalog.cs", 4, "public static string Slug("),
    ] {
        let started = Instant::now();
        let found = tool_gateway::execute("filesystem.search", input.clone(), &context).await.unwrap();
        assert_eq!(found["coverage"]["traversalComplete"], true);
        let names = found["matches"].as_array().unwrap();
        let relative_to_search = Path::new(expected_path).strip_prefix(input["path"].as_str().unwrap_or(".")).unwrap_or(Path::new(expected_path));
        let normalized_expected = relative_to_search.to_string_lossy().replace('\\', "/");
        let matching = names.iter().find(|item|item["path"].as_str().is_some_and(|path|path.replace('\\',"/")==normalized_expected)).unwrap();
        if matching["line"].is_null() {
            let symbol = tool_gateway::execute("filesystem.search", json!({"path":expected_path,"mode":"content","query":expected_text}), &context).await.unwrap();
            assert_eq!(symbol["matches"][0]["line"], expected_line);
        } else { assert_eq!(matching["line"], expected_line); }
        let read = tool_gateway::execute("filesystem.read", json!({"path":expected_path,"startLine":expected_line,"endLine":expected_line}), &context).await.unwrap();
        assert!(read["content"].as_str().unwrap().contains(expected_text));
        assert_eq!(read["lineRange"]["startLine"], expected_line);
        assert_eq!(fs::canonicalize(read["path"].as_str().unwrap()).unwrap(), fs::canonicalize(work.join(expected_path)).unwrap());
        assert_eq!(read["sha256"], digest(fs::read(work.join(expected_path)).unwrap()));
        observations.push(json!({"queryId":id,"hit":true,"path":expected_path,"line":expected_line,"sha256":read["sha256"],
            "elapsedMs":started.elapsed().as_secs_f64()*1000.0}));
    }
    let started = Instant::now();
    let cross_file = tool_gateway::execute("filesystem.search", json!({"path":"src","mode":"regex","query":r"\badd\("}), &context).await.unwrap();
    let hits = cross_file["matches"].as_array().unwrap();
    assert_eq!(cross_file["hasMore"], false); assert_eq!(hits.len(), 2);
    let mut references = Vec::new();
    for (file, line, evidence) in [("math.rs", 2_u64, "pub fn add("), ("workflow.rs", 3, "add(20, 22)")] {
        assert!(hits.iter().any(|item|item["path"]==file && item["line"]==line));
        let relative = format!("src/{file}");
        let read = tool_gateway::execute("filesystem.read", json!({"path":relative,"startLine":line,"endLine":line}), &context).await.unwrap();
        assert!(read["content"].as_str().unwrap().contains(evidence));
        assert_eq!(read["sha256"], digest(fs::read(work.join(&relative)).unwrap()));
        references.push(json!({"path":relative,"line":line,"sha256":read["sha256"]}));
    }
    observations.push(json!({"queryId":"cross-file-definition-and-call","hit":true,"references":references,
        "elapsedMs":started.elapsed().as_secs_f64()*1000.0}));
    let ignored = tool_gateway::execute("filesystem.search", json!({"path":".","mode":"content","query":"hidden_secret"}), &context).await.unwrap();
    assert!(ignored["matches"].as_array().unwrap().is_empty()); assert_eq!(ignored["hasMore"], false);
    let denied = tool_gateway::execute("filesystem.search", json!({"path":unlinked.parent().unwrap(),"mode":"regex","query":"outside_scope","reason":"fixture checks an unlinked root"}), &context).await.unwrap();
    assert_eq!(denied["requiresApproval"], true); assert_eq!(denied["status"], "not-dispatched");
    assert!(denied.get("matches").is_none());
    let mut durations = observations.iter().map(|item|item["elapsedMs"].as_f64().unwrap()).collect::<Vec<_>>(); durations.sort_by(f64::total_cmp);
    let report = json!({"scope":"four fixed functional code-location tasks through native filesystem.search and inclusive line reads; no generalization claim",
        "queryCount":observations.len(),"hitRate":observations.iter().filter(|item|item["hit"]==true).count() as f64/observations.len() as f64,
        "p50Ms":durations[1],"p95Ms":durations[3],"modelUsed":false,"vectorsUsed":false,
        "gitignoreVerified":true,"unlinkedRootNotDispatched":true,"observations":observations});
    if let Some(path) = std::env::var_os("KYNXA_CODE_LOCATION_REPORT") { fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap(); }
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    tool_gateway::shutdown(&context).await;
}

#[test]
#[ignore="requires KYNXA_SCIFACT_DATASET_ROOT and KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT; real CPU E5 public-corpus evaluation"]
fn scifact_current_native_e5_hybrid_and_supplementary_corpora() {
    let started=Instant::now();
    let dataset=required_path("KYNXA_SCIFACT_DATASET_ROOT");
    let bundle=required_path("KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT");
    let model=bundle.join("runtime/embedding/builtin-multilingual");
    let partition=std::env::var("KYNXA_SCIFACT_PARTITION").unwrap_or_else(|_|"all".into());
    assert!(["all","development","holdout"].contains(&partition.as_str()));
    let rrf_k=evaluation_rrf_k();
    for (path,expected) in [("corpus.jsonl",CORPUS_SHA256),("queries.jsonl",QUERIES_SHA256),("qrels/test.tsv",QRELS_SHA256)] {
        assert_eq!(digest(fs::read(dataset.join(path)).unwrap()),expected,"public corpus identity mismatch: {path}");
    }
    let assets: Value=serde_json::from_str(MODEL_MANIFEST).unwrap();
    for asset in assets["files"].as_array().unwrap() {
        let bytes=fs::read(model.join(asset["path"].as_str().unwrap())).unwrap();
        assert_eq!(bytes.len() as u64,asset["bytes"].as_u64().unwrap()); assert_eq!(digest(&bytes),asset["sha256"]);
    }
    let corpus=json_lines(&dataset.join("corpus.jsonl")); assert_eq!(corpus.len(),5183);
    let definitions=json_lines(&dataset.join("queries.jsonl")).into_iter().map(|item|(item["_id"].as_str().unwrap().to_owned(),item)).collect::<HashMap<_,_>>();
    let mut qrels: BTreeMap<String,HashSet<String>>=BTreeMap::new();
    for line in fs::read_to_string(dataset.join("qrels/test.tsv")).unwrap().lines().skip(1) {
        let cells=line.split('\t').collect::<Vec<_>>(); if cells.len()==3 && cells[2].parse::<i32>().unwrap()>0 {
            qrels.entry(cells[0].into()).or_default().insert(format!("beir:scifact:{}",cells[1]));
        }
    }
    assert_eq!(qrels.len(),300);
    let mut ids=qrels.keys().cloned().collect::<Vec<_>>();
    ids.sort_by_key(|id|digest(format!("kynxa-scifact-dev-v1:{id}")));
    let development=ids[..100].iter().cloned().collect::<HashSet<_>>();
    let split_hash=digest(serde_json::to_vec(&ids).unwrap());
    let workspace=Workspace::new(&bundle);
    gateway_inference::initialize_runtime(&workspace.config.inference_library).unwrap();
    let metadata=gateway_inference::embedding_metadata("cpu").unwrap();
    let options=InferenceOptions {cpu_threads:2,batch_size:8,batch_token_budget:4096,activation_memory_bytes:128*1024*1024,..Default::default()};
    let cancel=Arc::new(AtomicBool::new(false));
    let initial_memory=memory();
    gateway_inference::embed_with_options(&model,&["retrieval warmup".into()],EmbedKind::Query,&options,cancel.clone()).unwrap();
    let warm_memory=memory();
    let (chunk_count,vector_audit)=if let Some(path)=std::env::var_os("KYNXA_SCIFACT_PREPARED_INDEX") {
        load_prepared_index(&workspace.config,&PathBuf::from(path),&metadata,&corpus)
    } else {
        let mut bank=VectorBank::load(std::env::var_os("KYNXA_SCIFACT_VECTOR_CACHE").map(PathBuf::from),&metadata);
        eprintln!("native SciFact cache: {} vectors from {} validated manifests",bank.vectors.len(),bank.validated_files);
        let sources=corpus.iter().map(|item|json!({"sourceId":format!("beir:scifact:{}",item["_id"].as_str().unwrap()),"scopeKey":"user","sourceType":"document",
            "title":item["title"],"text":format!("{}\n\n{}",item["title"].as_str().unwrap(),item["text"].as_str().unwrap()),"locator":{},"sourceRevision":1})).collect::<Vec<_>>();
        let count=publish(&workspace.config,&sources,&model,&metadata,&mut bank,&options,&cancel);
        (count,bank.audit())
    };
    drop(corpus);
    let index_memory=memory();
    let mut query_peak=index_memory["workingSetBytes"].as_u64().unwrap_or(0);
    let mut observations=Vec::new();
    for (number,id) in ids.iter().enumerate() {
        let development_query=development.contains(id);
        if partition=="development" && !development_query || partition=="holdout" && development_query { continue; }
        let text=definitions[id]["text"].as_str().unwrap();
        let query_started=Instant::now(); let response=query(&workspace.config,&model,&metadata,&options,&cancel,text,None);
        let mut item=observation(&response,&qrels[id],query_started.elapsed().as_secs_f64()*1000.0);
        item["queryId"]=json!(id); item["partition"]=json!(if development_query{"development"}else{"holdout"});
        observations.push(item);
        query_peak=query_peak.max(memory()["workingSetBytes"].as_u64().unwrap_or(0));
        if (number+1)%25==0 { eprintln!("native SciFact queries {}/300",number+1); }
    }
    let final_memory=memory();
    let dev=observations.iter().filter(|item|item["partition"]=="development").cloned().collect::<Vec<_>>();
    let holdout=observations.iter().filter(|item|item["partition"]=="holdout").cloned().collect::<Vec<_>>();
    let all=aggregate(&observations);
    let connection=rusqlite::Connection::open_with_flags(gateway_index::database(&workspace.config),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let page_count:i64=connection.query_row("PRAGMA page_count",[],|row|row.get(0)).unwrap();
    let page_size:i64=connection.query_row("PRAGMA page_size",[],|row|row.get(0)).unwrap();
    drop(connection);
    let supplement=native_supplementary(&bundle,&model,&metadata,&options,&cancel);
    let report=json!({"schemaVersion":1,"createdAt":chrono::Utc::now().to_rfc3339(),"scope":"current native chunker, real CPU E5, exact native hybrid search and production lightweight ranker; document@10",
        "dataset":{"documents":5183,"testQueries":300,"files":{"corpus.jsonl":CORPUS_SHA256,"queries.jsonl":QUERIES_SHA256,"qrels/test.tsv":QRELS_SHA256}},
        "split":{"method":"first 100 by SHA256(kynxa-scifact-dev-v1:<queryId>); remaining 200 holdout","idOrderSha256":split_hash,
            "partitionExecuted":partition,"developmentIds":ids[..100],"holdoutIds":ids[100..]},
        "rrfK":rrf_k,"channelCandidates":40,"returnedChunks":60,"chunkCount":chunk_count,
        "buildProfile":if cfg!(debug_assertions){"debug"}else{"release"},
        "metricDefinitions":{"recallAt10":"gold document recall in the first 10 distinct documents, deduplicated from up to 60 returned evidence items",
            "recallWithinFirst10ReturnedItems":"gold document recall using only the first 10 returned evidence items; adjacent original chunks may have been merged by the production ranker",
            "top1Accuracy":"first returned document belongs to public positive qrels"},
        "model":metadata,"device":"cpu","gpuValidated":false,"queryVectorCacheUsed":false,"publicPassageCache":vector_audit,
        "metrics":{"allExecuted":all,"development":aggregate(&dev),"holdout":aggregate(&holdout)},
        "memory":{"initial":initial_memory,"modelWarmBaseline":warm_memory,"indexReady":index_memory,"afterQueries":final_memory,"sampledQueryWorkingSetPeakBytes":query_peak,
            "note":"Whole-process Windows counters include model, SQLite, candidate memory and loaded assets; sampled query peak can miss shorter spikes; OS process peak also covers indexing.",
            "sqliteFileBytes":page_count*page_size,"annMode":"exact; no ANN graph built"},
        "targets":{"recallAt10Minimum":0.828,"warmP50MsMaximum":300,"warmP95MsMaximum":1000,"residentIncrementBytesMaximum":200*1024*1024,"peakIncrementBytesMaximum":400*1024*1024},
        "supplementary":supplement,"observations":observations,"elapsedMs":started.elapsed().as_secs_f64()*1000.0});
    if let Some(path)=std::env::var_os("KYNXA_SCIFACT_REPORT") { fs::write(path,serde_json::to_vec_pretty(&report).unwrap()).unwrap(); }
    println!("{}",serde_json::to_string_pretty(&json!({"metrics":report["metrics"],"memory":report["memory"],"publicPassageCache":report["publicPassageCache"],"supplementary":report["supplementary"]["groups"]})).unwrap());
    // Measurements are reported unchanged; a below-target result must not be rewritten as a pass.
    // 测量原样报告，低于目标不能改写成达标；开发集与保留集不混作调参样本。
    assert!(report["observations"].as_array().unwrap().len()==if partition=="all"{300}else if partition=="development"{100}else{200});
    assert!(gateway_inference::retire_idle(&model,"cpu").unwrap());
}
