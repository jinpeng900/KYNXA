//! Native corpus publication and retrieval do not cross a JavaScript or helper boundary.
//! 原生语料发布与检索不经过 JavaScript 或 Node helper 边界。

use crate::gateway_contract::GatewayConfig;
use crate::storage::{
    self, ContractError, gateway, sqlite_bootstrap, sqlite_index, sqlite_queries, sqlite_search,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use unicode_normalization::UnicodeNormalization;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

pub fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Validate each source's own mount; unrelated mounts preserve evidence and legacy locators.
/// 仅核验来源自身挂载，其他挂载变更不撤销证据或旧定位。
pub(crate) fn mounted_root(relation: &Value, locator: &Value) -> Result<String, ContractError> {
    let mounts = relation["mounts"].as_array();
    let found = mounts.into_iter().flatten().find(|mount| {
        mount["path"] == locator["root"] && (locator["mountId"].is_null()
            || (mount["id"] == locator["mountId"] && mount["bindingRevision"] == locator["mountBindingRevision"]))
    });
    let root = found.and_then(|mount| mount["path"].as_str()).or_else(|| {
        (locator["mountId"].is_null() && relation["folderPath"] == locator["root"])
            .then(|| relation["folderPath"].as_str()).flatten()
    });
    if relation["isArchived"] == true || relation["isFolderlessWorkspace"] == true || root.is_none_or(str::is_empty) {
        return Err(gateway::error("来源挂载已移除或改变，旧派生内容不再使用。", "STALE_RETRIEVAL_SOURCE", 409));
    }
    Ok(root.unwrap().to_owned())
}

pub(crate) fn cached_work_sources(config: &GatewayConfig, scope: &str) -> Result<HashMap<String, Vec<Value>>, ContractError> {
    initialize(config)?;
    let (registry, _) = gateway::read_json(&config.data_root, &config.data_root.join("Retrieval/source-identities.json"), 32*1024*1024)?;
    let registry = registry.unwrap_or(json!({"sources":{}}));
    let mut cache: HashMap<String, Vec<Value>> = HashMap::new();
    let mut after = Value::Null;
    loop {
        let page = sqlite_queries::apply("storage.index.listSources", &arguments(config,
            json!({"scopes":[scope],"sourceType":"work-file","afterSourceId":after})))?;
        for row in page["rows"].as_array().into_iter().flatten() {
            let mut source = row.clone();
            for field in ["locator", "sourceRevision"] {
                if let Some(raw) = source[field].as_str() {
                    source[field] = serde_json::from_str(raw).map_err(|_| gateway::error("索引来源结构无效。", "CORRUPT_RETRIEVAL_INDEX", 409))?;
                }
            }
            let record = &registry["sources"][hash(source["sourceId"].as_str().unwrap_or(""))];
            if record["active"] != true || record["scopeKey"] != source["scopeKey"]
                || record["contentHash"] != source["contentHash"] || record["sourceRevision"] != source["sourceRevision"] { continue; }
            if source["parserVersion"] != super::gateway_structure::parser_version(source["locator"]["relativePath"].as_str().unwrap_or(""))
                || source["chunkerVersion"] != super::gateway_structure::CHUNKER_VERSION { continue; }
            source["fileMetadata"] = record["fileMetadata"].clone();
            source["scanLimitsSignature"] = record["scanLimitsSignature"].clone();
            source["cachedTextBytes"] = record["cachedTextBytes"].clone();
            if let Some(path) = source["locator"]["path"].as_str() { cache.entry(path.to_owned()).or_default().push(source); }
        }
        if page["rows"].as_array().unwrap().len() < 256 { break; }
        after = page["nextSourceId"].clone();
    }
    Ok(cache)
}

pub(crate) fn observe_files(config: &GatewayConfig, sources: &[Value]) -> Result<(), ContractError> {
    if sources.is_empty() { return Ok(()); }
    gateway::with_catalog(&config.data_root, |_, _| {
        let path = config.data_root.join("Retrieval/source-identities.json");
        let (registry, digest) = gateway::read_json(&config.data_root, &path, 32*1024*1024)?;
        let Some(registry) = registry else { return Ok(()); };
        let observed = sources.iter().map(|source| json!({"key":hash(source["sourceId"].as_str().unwrap_or("")),
            "sourceId":source["sourceId"],"scopeKey":source["scopeKey"],"sourceRevision":source["sourceRevision"],
            "contentHash":source["contentHash"],"fileMetadata":source["fileMetadata"],
            "scanLimitsSignature":source["scanLimitsSignature"],"cachedTextBytes":source["cachedTextBytes"]})).collect::<Vec<_>>();
        let plan = storage::index_registry::apply("storage.registry.observeFiles", &json!({"registry":registry,"sources":observed}))?;
        if plan["changed"] == true { gateway::commit_json(&config.data_root, &path, &plan["registry"], digest.as_deref(), 32*1024*1024)?; }
        Ok(())
    })
}
pub fn database(config: &GatewayConfig) -> PathBuf {
    config.data_root.join("Index/retrieval.sqlite")
}
pub(crate) fn arguments(config: &GatewayConfig, mut input: Value) -> Value {
    input["root"] = json!(config.data_root);
    input["path"] = json!(database(config));
    input
}
type BootstrapReceipt = ((u64, u64), Value);
static BOOTSTRAPS: OnceLock<Mutex<HashMap<PathBuf, BootstrapReceipt>>> = OnceLock::new();
pub fn initialize(config: &GatewayConfig) -> Result<Value, ContractError> {
    let path = database(config);
    let existing = BOOTSTRAPS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(&path)
        .cloned();
    if let Some((identity, receipt)) = existing {
        let connection = storage::sqlite_connections::acquire(&config.data_root, &path, 120000)?;
        if connection.database_identity() == identity {
            return Ok(receipt);
        }
    }
    let receipt = sqlite_bootstrap::apply(
        "storage.index.bootstrap",
        &arguments(config, json!({"epoch":gateway::new_id()})),
    )?;
    let connection = storage::sqlite_connections::acquire(&config.data_root, &path, 120000)?;
    let mut bootstraps = BOOTSTRAPS.get().unwrap().lock().unwrap();
    if bootstraps.len() >= 16 {
        if let Some(key) = bootstraps.keys().next().cloned() {
            bootstraps.remove(&key);
        }
    }
    bootstraps.insert(path, (connection.database_identity(), receipt.clone()));
    Ok(receipt)
}
pub fn reference(source: &Value, chunk: Option<&Value>) -> String {
    let mut descriptor = json!({"sourceId":source["sourceId"],"scopeKey":source["scopeKey"],"contentHash":source["contentHash"],"sourceRevision":source["sourceRevision"]});
    if let Some(signature) = source.get("derivationSignature") {
        descriptor["derivationSignature"] = signature.clone();
    }
    if let Some(chunk) = chunk {
        descriptor["chunkId"] = chunk["chunkId"].clone();
        descriptor["chunkHash"] = chunk["chunkHash"].clone();
    }
    format!(
        "rag1:{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&descriptor).unwrap())
    )
}
pub fn prepare_source(input: &Value, vectors: Option<&[Vec<f32>]>) -> Result<Value, ContractError> {
    storage::retrieval::apply(
        "storage.retrieval.sourceId",
        &json!({"value":input["sourceId"]}),
    )?;
    storage::retrieval::apply(
        "storage.retrieval.scopes",
        &json!({"value":[input["scopeKey"]]}),
    )?;
    let text = input["text"]
        .as_str()
        .ok_or_else(|| gateway::error("资料正文缺失。", "INVALID_RETRIEVAL_SOURCE", 400))?;
    let content_hash = hash(text);
    if input
        .get("contentHash")
        .is_some_and(|value| value != &content_hash)
    {
        return Err(gateway::error(
            "资料内容已改变。",
            "RETRIEVAL_SOURCE_CHANGED",
            409,
        ));
    }
    let mut source = input.clone();
    source["contentHash"] = json!(content_hash);
    if source["sourceRevision"].is_null() {
        source["sourceRevision"] = source["contentHash"].clone();
    }
    if source["locator"].is_null() {
        source["locator"] = json!({});
    }
    if source["title"].is_null() {
        source["title"] = source["sourceId"].clone();
    }
    if source["sourceType"].is_null() {
        source["sourceType"] = json!("document");
    }
    let path = source["locator"]["relativePath"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let structure = super::gateway_structure::parse(text, &path)?;
    let ranges = if let Some(chunks) = input["chunks"].as_array() {
        chunks.clone()
    } else {
        super::gateway_structure::chunks(text, &structure, 384)?
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let source_prefix = &hash(source["sourceId"].as_str().unwrap())[..24];
    let parser_version = structure["parserVersion"]
        .as_str()
        .unwrap_or("plain-text-v1");
    let mut chunks = Vec::new();
    let mut derived = Vec::new();
    let mut embedding = Vec::new();
    for (number, range) in ranges.iter().enumerate() {
        storage::sqlite_store::cancelled()?;
        let body = String::from_utf16_lossy(
            &units[range["startOffset"].as_u64().unwrap() as usize
                ..range["endOffset"].as_u64().unwrap() as usize],
        );
        let chunk_hash = hash(&body);
        let lexical = crate::retrieval::lexical::collect_terms(
            &format!("{} {path} {body}", source["title"].as_str().unwrap()),
            20000,
            false,
        )
        .join(" ");
        let topology = json!({"chunkId":format!("{source_prefix}:{}:{number}",&chunk_hash[..24]),"chunkIndex":number,"chunkHash":chunk_hash,
            "startOffset":range["startOffset"],"endOffset":range["endOffset"],"startLine":range["startLine"],"endLine":range["endLine"]});
        let mut chunk = topology.clone();
        chunk["text"] = json!(body);
        chunk["lexicalText"] = json!(lexical);
        chunk["chunkerVersion"] = range
            .get("chunkerVersion")
            .cloned()
            .unwrap_or(json!(super::gateway_structure::CHUNKER_VERSION));
        chunk["tokenizerVersion"] = json!("han-bigram-code-tf-v2");
        chunk["structure"] = range["structure"].clone();
        chunk["structureJson"] = json!(range["structure"].to_string());
        chunk["projectionJson"] = json!(
            range
                .get("embeddingProjection")
                .map(Value::to_string)
                .unwrap_or_default()
        );
        let mut lexical_topology = topology.clone();
        lexical_topology["chunkerVersion"] = chunk["chunkerVersion"].clone();
        lexical_topology["tokenizerVersion"] = chunk["tokenizerVersion"].clone();
        lexical_topology["lexicalHash"] = json!(hash(&lexical));
        derived.push(lexical_topology);
        let mut embedding_topology = topology;
        embedding_topology["chunkerVersion"] = chunk["chunkerVersion"].clone();
        let projection = range["embeddingProjection"]["context"]
            .as_str()
            .unwrap_or("");
        embedding_topology["inputHash"] = json!(hash(&format!("{projection}{body}")));
        embedding.push(embedding_topology);
        chunks.push(chunk);
    }
    let dimensions = vectors.and_then(|vectors| vectors.first()).map(Vec::len);
    if let Some(vectors) = vectors {
        if vectors.len() != chunks.len()
            || dimensions.is_none_or(|dimensions| dimensions == 0 || dimensions > 4096)
            || vectors.iter().any(|vector| {
                Some(vector.len()) != dimensions
                    || vector.iter().any(|value| !value.is_finite())
                    || vector.iter().all(|value| *value == 0.0)
            })
        {
            return Err(gateway::error(
                "嵌入与分块不一致。",
                "INVALID_RETRIEVAL_VECTOR",
                400,
            ));
        }
    }
    let input_signature = hash(
        &json!({
            "version": "source-derivation-v1",
            "parserVersion": parser_version,
            "chunkerVersion": super::gateway_structure::CHUNKER_VERSION,
            "embeddingInputVersion": "source-context-v1",
            "chunks": embedding
        })
        .to_string(),
    );
    let metadata = json!({
        "sourceId": source["sourceId"],
        "scopeKey": source["scopeKey"],
        "contentHash": source["contentHash"],
        "sourceRevision": source["sourceRevision"],
        "bindingRevision": source.get("bindingRevision").unwrap_or(&json!(0)),
        "sourceType": source["sourceType"],
        "title": source["title"],
        "locator": source["locator"]
    });
    source["derivationSignature"] = json!(hash(
        &json!({
            "version": "source-derivation-v1",
            "parserVersion": parser_version,
            "chunkerVersion": super::gateway_structure::CHUNKER_VERSION,
            "tokenizerVersion": "han-bigram-code-tf-v2",
            "embeddingInputVersion": "source-context-v1",
            "source": metadata,
            "chunks": derived,
            "embeddingInputSignature": input_signature
        })
        .to_string()
    ));
    source["embeddingInputSignature"] = json!(input_signature);
    source["parserVersion"] = json!(parser_version);
    source["chunkerVersion"] = json!(super::gateway_structure::CHUNKER_VERSION);
    source["tokenizerVersion"] = json!("han-bigram-code-tf-v2");
    source["embeddingInputVersion"] = json!("source-context-v1");
    let encodings: Vec<Value> = chunks
        .iter()
        .enumerate()
        .map(|(number, _)| {
            vectors
                .map(|vectors| {
                    json!(
                        STANDARD.encode(
                            vectors[number]
                                .iter()
                                .flat_map(|value| value.to_le_bytes())
                                .collect::<Vec<_>>()
                        )
                    )
                })
                .unwrap_or(Value::Null)
        })
        .collect();
    let signature =
        dimensions.map(|dimensions| {
            hash(&json!({
        "profile": source["embeddingProfileId"],
        "modelVersion": source["embeddingModelVersion"],
        "space": source["embeddingSpaceId"],
        "dimensions": dimensions,
        "inputSignature": input_signature,
        "chunks": chunks.iter().map(|chunk| chunk["chunkId"].clone()).collect::<Vec<_>>(),
        "vectors": encodings
    }).to_string())
        });
    let relations =
        crate::retrieval::relations::derive(&json!({"title":source["title"],"chunks":chunks}))
            .map_err(|_| gateway::error("关系派生失败。", "INVALID_RETRIEVAL_STRUCTURE", 422))?;
    let edges = relations["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| {
            let mut edge = edge.clone();
            edge["edgeId"] = json!(hash(&edge.to_string()));
            edge
        })
        .collect::<Vec<_>>();
    source["structure"] = structure.clone();
    Ok(
        json!({"source":source,"chunks":chunks,"vectors":vectors.map(|vectors|json!(vectors)).unwrap_or(json!([])),"dimensions":dimensions,"embeddingSignature":signature,
        "locatorJson":source["locator"].to_string(),"sourceRevisionJson":source["sourceRevision"].to_string(),"structureJson":structure.to_string(),
        "relativePath":storage::retrieval::normalize_path(&path),"sourceRef":reference(&source,None),"relations":{"edges":edges,"truncated":relations["truncated"]}}),
    )
}
pub fn publish(config: &GatewayConfig, sources: &[Value]) -> Result<Value, ContractError> {
    initialize(config)?;
    let states = sqlite_queries::apply(
        "storage.index.sourceState",
        &arguments(
            config,
            json!({"sourceIds":sources.iter().map(|source|source["sourceId"].clone()).collect::<Vec<_>>()}),
        ),
    )?;
    let changed = sources
        .iter()
        .filter(|source| {
            source.get("chunks").is_some()
                || !states["states"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|state| source_matches(state, source))
        })
        .collect::<Vec<_>>();
    if changed.is_empty() {
        return Ok(json!({"published":[],"reusedSources":sources.len(),"changed":false}));
    }
    let prepared = changed
        .iter()
        .map(|source| prepare_source(source, None))
        .collect::<Result<Vec<_>, _>>()?;
    publish_prepared(config, &prepared)
}
fn source_matches(state: &Value, source: &Value) -> bool {
    let text = source["text"].as_str().unwrap_or("");
    let digest = hash(text);
    source_matches_hash(state, source, &digest)
}
fn source_matches_hash(state: &Value, source: &Value, digest: &str) -> bool {
    let revision = source
        .get("sourceRevision")
        .cloned()
        .unwrap_or(json!(digest));
    source
        .get("contentHash")
        .is_none_or(|expected| *expected == digest)
        && state["source_id"] == source["sourceId"]
        && state["scope_key"] == source["scopeKey"]
        && state["source_type"] == source["sourceType"]
        && state["content_hash"] == digest
        && state["source_revision"] == revision.to_string()
        && state["binding_revision"].as_u64().unwrap_or(0)
            == source["bindingRevision"].as_u64().unwrap_or(0)
        && state["title"] == source["title"]
        && state["locator"] == source["locator"].to_string()
        && state["parser_version"]
            == super::gateway_structure::parser_version(
                source["locator"]["relativePath"].as_str().unwrap_or(""),
            )
        && state["chunker_version"] == super::gateway_structure::CHUNKER_VERSION
        && state["embedding_input_version"] == "source-context-v1"
}
/// Reuse vectors only for matching source identity, revision and derivation inputs.
/// 来源身份、版本和派生输入均匹配时才复用向量。
pub(crate) fn reuse_semantic(
    config: &GatewayConfig,
    source: &Value,
    local: &Value,
) -> Result<Option<Value>, ContractError> {
    let digest = if let Some(text) = source["text"].as_str() {
        hash(text)
    } else if let Some(digest) = source["contentHash"].as_str() {
        digest.to_string()
    } else {
        return Ok(None);
    };
    let states = sqlite_queries::apply(
        "storage.index.sourceState",
        &arguments(config, json!({"sourceIds":[source["sourceId"]]})),
    )?;
    Ok(states["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|state| {
            source_matches_hash(state, source, &digest)
                && state["vector_chunks"] == state["chunk_count"]
                && state["vector_chunks"].as_u64().unwrap_or(0) > 0
                && (local["embeddingDevicePolicy"] != "cpu"
                    || state["embedding_profile_id"] == "builtin-multilingual")
        })
        .map(|state| {
            json!({"chunks":state["chunk_count"],"vectorChunks":state["vector_chunks"],"cachedChunks":state["vector_chunks"],"profileId":state["embedding_profile_id"],"spaceId":state["embedding_space_id"],"reused":true})
        }))
}
pub fn publish_semantic(
    config: &GatewayConfig,
    source: &Value,
    local: &Value,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<Value, ContractError> {
    if let Some(cached) = reuse_semantic(config, source, local)? {
        return Ok(cached);
    }
    let prepared = prepare_source(source, None)?;
    let chunks = prepared["chunks"].as_array().unwrap();
    if chunks.is_empty() {
        return Ok(json!({"chunks":0,"vectorChunks":0,"cachedChunks":0,"empty":true}));
    }
    let projections = chunks
        .iter()
        .map(|chunk| embedding_projection(&prepared["source"], chunk))
        .collect::<Vec<_>>();
    let fits = super::gateway_embedding::fit(config, &projections, local, cancel.clone())?;
    let mut fitted = Vec::new();
    for ((chunk, projection), fit) in chunks.iter().zip(&projections).zip(&fits) {
        let mut segments=super::token_fit::apply("retrieval.token-fit",&json!({"source":source,"chunk":chunk,"projection":projection,"document":fit,"chunkOffset":fitted.len(),"maxInputTokens":512,"fittingVersion":"real-tokenizer-512-v1"}))
            .map_err(|message|gateway::error(&message,"INVALID_RETRIEVAL_TOKEN_FIT",422))?;
        for segment in segments.as_array_mut().unwrap() {
            segment["structure"] = chunk["structure"].clone();
        }
        fitted.extend(segments.as_array().unwrap().iter().cloned());
    }
    let inputs = fitted
        .iter()
        .map(|chunk| {
            format!(
                "{}{}",
                chunk["embeddingProjection"]["context"]
                    .as_str()
                    .unwrap_or(""),
                chunk["text"].as_str().unwrap()
            )
        })
        .collect::<Vec<_>>();
    let batch = super::gateway_embedding::embed(
        config,
        &inputs,
        crate::models::gateway_inference::EmbedKind::Passage,
        local,
        cancel.clone(),
        true,
    )?
    .ok_or_else(|| gateway::error("嵌入模型尚未就绪。", "EMBEDDING_UNAVAILABLE", 503))?;
    let mut source = source.clone();
    source["chunks"] = json!(fitted);
    source["embeddingProfileId"] = json!(batch.profile_id);
    source["embeddingModelVersion"] = json!(batch.model_version);
    source["embeddingSpaceId"] = json!(batch.space_id);
    let prepared = prepare_source(&source, Some(&batch.vectors))?;
    let receipt = publish_prepared(config, &[prepared.clone()])?;
    Ok(
        json!({"receipt":receipt,"chunks":prepared["chunks"].as_array().unwrap().len(),"vectorChunks":batch.vectors.len(),"profileId":source["embeddingProfileId"],"spaceId":source["embeddingSpaceId"],"device":batch.device}),
    )
}
/// Use the same source projection for publication and native offline evaluation.
/// 正式发布与原生离线评测共用同一来源投影，不复制或漂移嵌入输入格式。
pub fn embedding_projection(source: &Value, chunk: &Value) -> Value {
    fn bounded(text: &str, count: usize) -> String {
        let units = text.encode_utf16().take(count).collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
            .replace(['\r', '\n', '\0'], " ")
            .trim()
            .to_string()
    }
    let mut metadata = Vec::new();
    let structure = &chunk["structure"];
    for (key, label) in [
        ("symbolName", "Symbol"),
        ("parentSymbol", "Parent"),
        ("sectionTitle", "Section"),
    ] {
        if let Some(value) = structure[key]
            .as_str()
            .filter(|value| !value.trim().is_empty())
        {
            metadata.push(format!("{label}: {}", bounded(value, 32)));
        }
    }
    for (value, label) in [
        (&source["title"], "Title"),
        (&source["locator"]["relativePath"], "Path"),
    ] {
        if let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) {
            metadata.push(format!("{label}: {}", bounded(value, 32)));
        }
    }
    let text = chunk["text"].as_str().unwrap();
    let capacity = 128.min(512usize.saturating_sub(text.encode_utf16().count() + 2));
    let context = bounded(&metadata.join("\n"), capacity);
    json!({"context":if context.is_empty(){context}else{format!("{context}\n\n")},"text":text})
}
pub fn publish_prepared(
    config: &GatewayConfig,
    prepared: &[Value],
) -> Result<Value, ContractError> {
    initialize(config)?;
    gateway::with_catalog(&config.data_root, |catalog, _| {
        for item in prepared {
            let source = &item["source"];
            if let Some(chat) = source["scopeKey"]
                .as_str()
                .and_then(|scope| scope.strip_prefix("chat:"))
            {
                storage::catalog::apply(
                    "storage.catalog.relationship",
                    &json!({"current":catalog,"id":chat}),
                )?;
            }
            if source["sourceType"] == "work-file" {
                let project = source["scopeKey"]
                    .as_str()
                    .and_then(|scope| scope.strip_prefix("project:"))
                    .ok_or_else(|| {
                        gateway::error("来源工作范围无效。", "RETRIEVAL_SCOPE_REQUIRED", 403)
                    })?;
                let relation = storage::catalog::apply(
                    "storage.catalog.project",
                    &json!({"current":catalog,"id":project}),
                )?;
                mounted_root(&relation, &source["locator"])?;
                let (settings, _) = gateway::read_json(
                    &config.data_root,
                    &config
                        .data_root
                        .join("Projects")
                        .join(project)
                        .join("retrieval.json"),
                    8 * 1024 * 1024,
                )?;
                let settings = settings.unwrap_or(storage::settings::apply(
                    "storage.settings.defaultProject",
                    &json!({"projectId":project}),
                )?);
                if settings["indexingSources"]["mountedFolder"]["bindingRevision"]
                    != source["bindingRevision"]
                {
                    return Err(gateway::error(
                        "工作索引绑定已改变。",
                        "STALE_RETRIEVAL_SOURCE",
                        409,
                    ));
                }
                if let Some(metadata) = source.get("fileMetadata") {
                    let current = crate::tools::service_source_reading::execute_io(
                        "metadata",
                        &json!({"path":source["locator"]["path"]}),
                        &None,
                    )
                    .map_err(|_| {
                        gateway::error("来源文件已改变。", "STALE_RETRIEVAL_SOURCE", 409)
                    })?;
                    if current["metadata"] != *metadata {
                        return Err(gateway::error(
                            "来源文件已改变，旧向量不会发布。",
                            "STALE_RETRIEVAL_SOURCE",
                            409,
                        ));
                    }
                }
            } else if source["sourceType"] == "memory" {
                storage::gateway_memory::validate_index_source(&config.data_root, catalog, source)?;
            } else if source["sourceType"] == "knowledge" {
                let (library, _) = gateway::read_json(
                    &config.data_root,
                    &config.data_root.join("Knowledge/catalog.json"),
                    32 * 1024 * 1024,
                )?;
                if library.as_ref().is_none_or(|library| {
                    !library["sources"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|entry| {
                            entry["id"] == source["sourceId"]
                                && entry["status"] == "ready"
                                && entry["revision"] == source["sourceRevision"]
                                && entry["contentHash"] == source["contentHash"]
                        })
                }) {
                    return Err(gateway::error(
                        "资料已撤销或改变。",
                        "STALE_RETRIEVAL_SOURCE",
                        409,
                    ));
                }
            }
        }
        let path = config.data_root.join("Retrieval/source-identities.json");
        let (value, digest) = gateway::read_json(&config.data_root, &path, 32 * 1024 * 1024)?;
        let registry = value.unwrap_or(json!({"schemaVersion":1,"revision":0,"sources":{}}));
        let states = sqlite_queries::apply(
            "storage.index.sourceState",
            &arguments(
                config,
                json!({"sourceIds":prepared.iter().map(|source|source["source"]["sourceId"].clone()).collect::<Vec<_>>()}),
            ),
        )?;
        let registrations=prepared.iter().map(|prepared|{let source=&prepared["source"];json!({"key":hash(source["sourceId"].as_str().unwrap()),"source":source,"revisionRaw":source["sourceRevision"].to_string(),
            "current":states["states"].as_array().unwrap().iter().find(|state|state["source_id"]==source["sourceId"]).cloned().unwrap_or(Value::Null),
            "record":{"sourceId":source["sourceId"],"scopeKey":source["scopeKey"],"sourceRevision":source["sourceRevision"],"contentHash":source["contentHash"],"locator":source["locator"],
                "sourceType":source["sourceType"],"bindingRevision":source["bindingRevision"].as_u64().unwrap_or(0),
                "fileMetadata":source["fileMetadata"],"scanLimitsSignature":source["scanLimitsSignature"],
                "cachedTextBytes":source["cachedTextBytes"],"active":true,"sourceRef":reference(source,None)}})}).collect::<Vec<_>>();
        let plan = storage::index_registry::apply(
            "storage.registry.publish",
            &json!({"registry":registry,"sources":registrations}),
        )?;
        gateway::commit_json(
            &config.data_root,
            &path,
            &plan["registry"],
            digest.as_deref(),
            32 * 1024 * 1024,
        )?;
        sqlite_index::apply(
            "storage.index.publish",
            &arguments(
                config,
                json!({"sources":prepared,"createdAt":gateway::now()}),
            ),
        )
    })
}
pub fn remove(
    config: &GatewayConfig,
    source_id: &str,
    scopes: &Value,
    permanent: bool,
) -> Result<Value, ContractError> {
    initialize(config)?;
    storage::retrieval::apply("storage.retrieval.scopes", &json!({"value":scopes}))?;
    gateway::with_catalog(&config.data_root, |_, _| {
        let path = config.data_root.join("Retrieval/source-identities.json");
        let (value, digest) = gateway::read_json(&config.data_root, &path, 32 * 1024 * 1024)?;
        let registry = value.unwrap_or(json!({"schemaVersion":1,"revision":0,"sources":{}}));
        let states = sqlite_queries::apply(
            "storage.index.sourceState",
            &arguments(config, json!({"sourceIds":[source_id]})),
        )?;
        let current = states["states"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or(Value::Null);
        let plan = storage::index_registry::apply(
            "storage.registry.remove",
            &json!({"registry":registry,"key":hash(source_id),"sourceId":source_id,"scopes":scopes,"current":current,"permanent":permanent}),
        )?;
        if plan["allowed"] != true {
            return Err(gateway::error(
                "来源不在授权范围。",
                "RETRIEVAL_SOURCE_NOT_FOUND",
                404,
            ));
        }
        if plan["coverageOnly"] != true {
            gateway::commit_json(
                &config.data_root,
                &path,
                &plan["registry"],
                digest.as_deref(),
                32 * 1024 * 1024,
            )?;
        }
        sqlite_index::apply(
            "storage.index.remove",
            &arguments(
                config,
                json!({"sourceId":source_id,"scopeKey":plan["scopeKey"].as_str().or_else(||scopes[0].as_str()),"corpusChanged":plan["corpusChanged"]}),
            ),
        )
    })
}

/// Recheck deletion under the catalog lock; preserve undo, Trash and shared originals.
/// 目录锁内重核删除状态，保留撤销恢复、Trash 和共享原文。
pub(crate) fn retire_deleted_chat(
    config: &GatewayConfig,
    deleted: &Value,
) -> Result<Value, ContractError> {
    // Release the catalog lock before waiting for index initialization admission.
    // 等待索引初始化准入前释放目录锁。
    if storage::managed_files::inspect(&config.data_root, &database(config), false, true, u64::MAX)?
    {
        initialize(config)?;
    }
    gateway::with_catalog(&config.data_root, |catalog, _| {
        let id = storage::identity(&deleted["Id"])?;
        let id = id.as_str().unwrap();
        let current = catalog["Tombstones"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["Id"]
                    .as_str()
                    .is_some_and(|current| current.eq_ignore_ascii_case(id))
                    && item["DeletedAt"] == deleted["DeletedAt"]
            });
        if !current {
            return Ok(
                json!({"retired":false,"reason":"conversation-restored-or-deletion-changed"}),
            );
        }
        match storage::catalog::apply(
            "storage.catalog.relationship",
            &json!({"current":catalog,"id":id}),
        ) {
            Ok(_) => {
                return Ok(
                    json!({"retired":false,"reason":"conversation-restored-or-deletion-changed"}),
                );
            }
            Err(error) if [404, 410].contains(&error.status_code()) => {}
            Err(error) => return Err(error),
        }
        let scope = format!("chat:{}", id.to_ascii_lowercase());
        let path = config.data_root.join("Retrieval/source-identities.json");
        let (registry, digest) = gateway::read_json(&config.data_root, &path, 32 * 1024 * 1024)?;
        let mut identities = 0;
        if let Some(registry) = registry {
            let plan = storage::index_registry::apply(
                "storage.registry.retireChat",
                &json!({"registry":registry,"conversationId":id}),
            )?;
            identities = plan["removed"].as_u64().unwrap();
            if identities > 0 {
                gateway::commit_json(
                    &config.data_root,
                    &path,
                    &plan["registry"],
                    digest.as_deref(),
                    32 * 1024 * 1024,
                )?;
            }
        }
        let mut rows = 0;
        let mut has_more = false;
        if storage::managed_files::inspect(
            &config.data_root,
            &database(config),
            false,
            true,
            u64::MAX,
        )? {
            let receipt = storage::sqlite_index::apply(
                "storage.index.retireChat",
                &arguments(config, json!({"scopeKey":scope})),
            )?;
            rows = receipt["removed"].as_u64().unwrap_or(0);
            has_more = receipt["hasMore"] == true;
        }
        if let Some(graphs) = GRAPHS.get() {
            let retired = {
                let mut graphs = graphs
                    .lock()
                    .map_err(|_| gateway::error("向量缓存暂时不可用。", "RETRIEVAL_BUSY", 503))?;
                let keys = graphs
                    .iter()
                    .filter(|(_, graph)| graph.database == database(config) && graph.scope == scope)
                    .map(|(key, _)| key.clone())
                    .collect::<std::collections::HashSet<_>>();
                graphs.retain(|key, _| !keys.contains(key));
                keys
            };
            // Release the resident-map lock before acquiring LRU to preserve query lock order.
            // 取 LRU 锁前释放驻留表锁，遵守查询锁顺序。
            if let Some(lru) = LRU.get() {
                let mut lru = lru.lock().unwrap();
                lru.shards.retain(|(key, _)| !retired.contains(key));
                lru.workspaces.retain(|workspace| workspace.database != database(config) || workspace.scope != scope);
            }
        }
        let cache = storage::ann_cache::apply(
            "storage.ann.retireChat",
            &json!({"root":config.data_root.join("Index"),"scopeKey":scope}),
        )?;
        let candidates = storage::gateway_memory::retire_chat_candidates(&config.data_root, id)?;
        super::gateway::retire_chat_index_work(&config.data_root, id);
        super::gateway_tools::release_conversation_tasks(id);
        Ok(
            json!({"retired":true,"complete":!has_more&&cache["complete"]!=false,"hasMore":has_more,"scopeKey":scope,"removedSources":rows,"removedIdentities":identities,"ann":cache,"candidates":candidates}),
        )
    })
}

struct Graph {
    index: Index,
    database: PathBuf,
    scope: String,
    identity: String,
    generation: u64,
    count: usize,
    // Drop the index before its lease; active queries retain both until use ends.
    // 先释放图对象再归还租约，活动查询持续持有两者。
    _lease: crate::resources::gateway::ResourceLease,
}
static GRAPHS: OnceLock<Mutex<HashMap<String, Arc<Graph>>>> = OnceLock::new();
static GRAPH_BUILDS: OnceLock<Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
struct GraphWorkspace {
    database: PathBuf,
    scope: String,
}

/// Drive workspace LRU by foreground demand and budget shards per workspace.
/// 按前台需求维护工作区 LRU，每个工作区单独计分片额度。
#[derive(Default)]
struct GraphResidency {
    workspaces: VecDeque<GraphWorkspace>,
    shards: VecDeque<(String, GraphWorkspace)>,
}

impl GraphResidency {
    fn demand(&mut self, workspace: GraphWorkspace) -> Vec<String> {
        self.workspaces.retain(|entry| entry != &workspace);
        self.workspaces.push_back(workspace);
        while self.workspaces.len() > 4 { self.workspaces.pop_front(); }
        let retired = self.shards.iter().filter(|(_, owner)| !self.workspaces.contains(owner))
            .map(|(key, _)| key.clone()).collect();
        self.shards.retain(|(_, owner)| self.workspaces.contains(owner));
        retired
    }

    fn remember(&mut self, key: &str, workspace: GraphWorkspace, maximum_shards: usize) -> Option<Vec<String>> {
        if !self.workspaces.contains(&workspace) { return None; }
        self.shards.retain(|(entry, _)| entry != key);
        self.shards.push_back((key.to_owned(), workspace.clone()));
        let mut count = self.shards.iter().filter(|(_, owner)| owner == &workspace).count();
        let mut retired = Vec::new();
        while count > maximum_shards.clamp(1, 16) {
            let position = self.shards.iter().position(|(_, owner)| owner == &workspace).unwrap();
            retired.push(self.shards.remove(position).unwrap().0);
            count -= 1;
        }
        Some(retired)
    }
}

static LRU: OnceLock<Mutex<GraphResidency>> = OnceLock::new();

fn request_graph_workspace(database: PathBuf, scope: &str) {
    let mut lru = LRU.get_or_init(Default::default).lock().unwrap();
    let retired = lru.demand(GraphWorkspace { database, scope: scope.to_owned() });
    let mut graphs = GRAPHS.get_or_init(Default::default).lock().unwrap();
    for key in retired { graphs.remove(&key); }
}

fn remember_graph(key: &str, graph: &Arc<Graph>, options: &Value) {
    // Lock LRU before the map; eviction drops only the cache Arc, not active query leases.
    // 先锁 LRU 再锁图表；淘汰只释放缓存 Arc，不释放活动查询租约。
    let mut lru = LRU.get_or_init(Default::default).lock().unwrap();
    let maximum = options["maxCachedShards"].as_u64().unwrap_or(4).clamp(1, 16) as usize;
    let workspace = GraphWorkspace { database: graph.database.clone(), scope: graph.scope.clone() };
    let Some(retired) = lru.remember(key, workspace, maximum) else { return; };
    let mut graphs = GRAPHS.get_or_init(Default::default).lock().unwrap();
    graphs.insert(key.to_owned(), graph.clone());
    for key in retired { graphs.remove(&key); }
}

static ANN_BACKGROUND: OnceLock<Mutex<HashMap<PathBuf, super::ann_queue::AnnSession>>> =
    OnceLock::new();
static ANN_PARTITIONS: OnceLock<Mutex<HashMap<String, Vec<Value>>>> = OnceLock::new();

fn graph_identity(descriptor: &Value, identity: &Value, epoch: &Value, options: &Value) -> Value {
    // Reuse graphs across restarts only when keys include construction parameters.
    // 图键包含构建参数后才跨重启复用，旧键无法证明参数。
    json!({"version":"rust-gateway-v2","databaseIdentity":identity,"epoch":epoch,"scopeKey":descriptor["scope_key"],"descriptor":descriptor,
        "buildParameters":{"metric":"cos","quantization":"f32","connectivity":options["connectivity"],"expansionAdd":options["expansionAdd"],"expansionSearch":options["expansionSearch"]}})
}
fn background_operation(config: &GatewayConfig, operation: &str, input: Value) -> Result<Value, ContractError> {
    let mut owners = ANN_BACKGROUND.get_or_init(Default::default).lock().unwrap();
    let session = owners.entry(database(config)).or_default();
    let receipt = session.apply(&format!("retrieval.ann.queue.{operation}"), &input)
        .map_err(|_| gateway::error("ANN 后台队列不可用。", "RETRIEVAL_BUSY", 503))?;
    if let Some(error) = receipt.get("error") {
        return Err(gateway::error(error["message"].as_str().unwrap_or("ANN 后台队列不可用。"),
            match error["code"].as_str() { Some("RETRIEVAL_ANN_QUEUE_FULL") => "RETRIEVAL_ANN_QUEUE_FULL", _ => "RETRIEVAL_BUSY" }, 503));
    }
    Ok(receipt["value"].clone())
}
pub(crate) fn resume_ann_background(config: &GatewayConfig) {
    let _ = background_operation(config, "init", json!({"concurrency":1}));
    let _ = background_operation(config, "drained", json!({}));
}
pub(crate) fn stop_ann_background(config: &GatewayConfig) {
    let _ = background_operation(config, "drain", json!({}));
}
pub(crate) fn ann_background_pending(config: &GatewayConfig) -> bool {
    background_operation(config, "shouldSchedule", json!({})).ok() == Some(json!(true))
}

/// Maintenance owns queued graph builds; shutdown/migration joins it, not request cancellation.
/// 维护线程负责队列构图，停机或迁移等待该线程，不随单次查询取消。
pub(crate) fn maintain_ann(config: &GatewayConfig, stop: &Arc<std::sync::atomic::AtomicBool>) -> Result<(), ContractError> {
    let job = background_operation(config, "start", json!({}))?;
    if job.is_null() { return Ok(()); }
    let id = job["id"].clone();
    let request = &job["descriptor"];
    let result = prepare_ann_request(config, request, stop, &id);
    let _ = background_operation(config, "finish", json!({"id":id,"succeeded":result.is_ok(),
        "cancelled":stop.load(std::sync::atomic::Ordering::Acquire),"aborted":result.as_ref().err().is_some_and(|error| error.code()=="RETRIEVAL_ANN_STALE_BUILD"),
        "code":result.as_ref().err().map(ContractError::code),"now":chrono::Utc::now().timestamp_millis()}));
    result
}

fn partitions(config: &GatewayConfig, descriptor: &Value, options: &Value) -> Result<Vec<Value>, ContractError> {
    let per_vector = descriptor["dimensions"].as_u64().unwrap_or(384) * 4
        + options["connectivity"].as_u64().unwrap_or(16) * 16 + 256;
    let capacity = options["maxShardBytes"].as_u64().unwrap_or(256 * 1024 * 1024)
        .saturating_sub(1024 * 1024).checked_div(per_vector).unwrap_or(0).max(1);
    let segments = descriptor["count"].as_u64().unwrap().div_ceil(capacity);
    let plan = if segments > 1 {
        sqlite_queries::apply("storage.index.vectorPartition", &arguments(config, json!({"descriptor":descriptor,"segments":segments})))?
    } else { json!([{"count":descriptor["count"]}]) };
    Ok(plan.as_array().unwrap().iter().map(|part| {
        let mut shard = descriptor.clone();
        shard.as_object_mut().unwrap().extend(part.as_object().unwrap().clone());
        shard
    }).collect())
}
fn auto_options(descriptor: &Value, options: &Value) -> Value {
    let mut effective = options.clone();
    let per_vector = descriptor["dimensions"].as_u64().unwrap_or(384) * 4
        + options["connectivity"].as_u64().unwrap_or(16) * 16 + 256;
    let capacity = options["maxShardBytes"].as_u64().unwrap_or(256 * 1024 * 1024)
        .saturating_sub(1024 * 1024).checked_div(per_vector).unwrap_or(0).max(1);
    let shards = descriptor["count"].as_u64().unwrap_or(0).div_ceil(capacity);
    // The default is an idle-cache starting point, not an immutable hardware limit.
    // 默认驻留数是空闲缓存起点；完整集合可按需上调，每片仍由资源服务实际准入。
    if options["adaptive"] == true && options["maxCachedShards"] == 4 {
        effective["maxCachedShards"] = json!(shards.clamp(4, 16));
    }
    effective
}
fn validate_graph_source(config: &GatewayConfig, descriptor: &Value, identity: &Value, epoch: &Value) -> Result<(), ContractError> {
    let connection = storage::sqlite_connections::acquire(&config.data_root, &database(config), 120000)?;
    let actual = connection.database_identity();
    drop(connection);
    if identity != &json!(format!("{:016x}:{:016x}", actual.0, actual.1))
        || initialize(config)?["epoch"] != *epoch
        || sqlite_queries::apply("storage.index.scopeGeneration", &arguments(config, json!({"scopeKey":descriptor["scope_key"]})))? != descriptor["generation"] {
        return Err(gateway::error("ANN 来源版本已变化。", "RETRIEVAL_ANN_STALE_BUILD", 409));
    }
    Ok(())
}
fn prepare_ann_request(config: &GatewayConfig, request: &Value, stop: &Arc<std::sync::atomic::AtomicBool>, job: &Value) -> Result<(), ContractError> {
    let descriptor = &request["sourceDescriptor"];
    let options = &request["options"];
    validate_graph_source(config, descriptor, &request["databaseIdentity"], &request["epoch"])?;
    let parts = partitions(config, descriptor, options)?;
    if parts.len() as u64 > options["maxCachedShards"].as_u64().unwrap_or(4) {
        return Err(gateway::error("当前常驻预算无法容纳完整图集合，继续使用有界精确检索。", "RETRIEVAL_ANN_RESOURCE_LIMIT", 503));
    }
    let mut completed = 0;
    for part in &parts {
        if stop.load(std::sync::atomic::Ordering::Acquire) {
            return Err(gateway::error("ANN 后台构建已停止。", "ABORT_ERR", 409));
        }
        ann(config, part, &[], &request["scopes"], &request["databaseIdentity"], &request["epoch"], options, 0, true, true)?;
        completed += part["count"].as_u64().unwrap_or(0);
        let _ = background_operation(config, "patch", json!({"id":job,"completedVectors":completed}));
    }
    let key = hash(&graph_identity(descriptor, &request["databaseIdentity"], &request["epoch"], options).to_string());
    let mut plans = ANN_PARTITIONS.get_or_init(Default::default).lock().unwrap();
    if plans.len() >= 64 { if let Some(old) = plans.keys().next().cloned() { plans.remove(&old); } }
    plans.insert(key, parts.clone());
    drop(plans);
    let mut protected = GRAPHS.get_or_init(Default::default).lock().unwrap().iter()
        .filter(|(_, graph)| graph.database == database(config)).map(|(key, _)| json!(key)).collect::<Vec<_>>();
    protected.extend(parts.iter().map(|part| json!(hash(&graph_identity(part, &request["databaseIdentity"], &request["epoch"], options).to_string()))));
    let _ = storage::ann_cache::apply("storage.ann.prune", &json!({"root":config.data_root.join("Index"),"options":options,"resident":protected}));
    Ok(())
}

fn enqueue_ann_descriptor(config: &GatewayConfig, descriptor: &Value, scopes: &Value, identity: &Value, epoch: &Value, options: &Value) -> Result<(), ContractError> {
    let mut queued = descriptor.clone();
    queued["sourceDescriptor"] = descriptor.clone(); queued["options"] = options.clone();
    queued["scopes"] = scopes.clone(); queued["databaseIdentity"] = identity.clone(); queued["epoch"] = epoch.clone();
    queued["buildBudgetKey"] = json!([options["maxShardBytes"], options["maxCachedShards"], options["connectivity"], options["expansionAdd"], options["expansionSearch"]]);
    let mut scheduling = graph_identity(descriptor, identity, epoch, options);
    scheduling["descriptor"].as_object_mut().unwrap().remove("generation");
    scheduling["descriptor"].as_object_mut().unwrap().remove("count");
    let queue_key = hash(&scheduling.to_string());
    background_operation(config, "enqueue", json!({"key":queue_key,"descriptor":queued,"now":chrono::Utc::now().timestamp_millis()}))?;
    Ok(())
}

/// Queue graph builds after vector publication, using existing parameters and admission.
/// 向量发布后合并构图请求，沿用参数及准入，减轻首次查询负担。
pub(crate) fn prepare_scope_ann(config: &GatewayConfig, scope: &str, local: &Value) -> Result<u64, ContractError> {
    let options = storage::settings::apply("storage.settings.ann", &json!({"value":local["ann"]}))?;
    if options["mode"] != "auto" { return Ok(0); }
    let boot = initialize(config)?;
    let connection = storage::sqlite_connections::acquire(&config.data_root, &database(config), 120000)?;
    let actual = connection.database_identity(); drop(connection);
    let identity = json!(format!("{:016x}:{:016x}", actual.0, actual.1));
    let scopes = json!([scope]);
    let descriptors = sqlite_queries::apply("storage.index.vectorDescriptors", &arguments(config, json!({"scopes":scopes,"allProfiles":true})))?;
    let mut queued = 0;
    for descriptor in descriptors.as_array().into_iter().flatten().filter(|descriptor|
        descriptor["count"].as_u64().unwrap_or(0) > options["threshold"].as_u64().unwrap_or(50000)) {
        enqueue_ann_descriptor(config, descriptor, &scopes, &identity, &boot["epoch"], &auto_options(descriptor, &options))?;
        queued += 1;
    }
    Ok(queued)
}

fn ann_auto(config: &GatewayConfig, descriptor: &Value, vector: &[f32], scopes: &Value, identity: &Value, epoch: &Value, options: &Value, limit: usize) -> Result<Vec<Value>, ContractError> {
    request_graph_workspace(database(config), descriptor["scope_key"].as_str().unwrap_or(""));
    let effective = auto_options(descriptor, options);
    let options = &effective;
    let key = hash(&graph_identity(descriptor, identity, epoch, options).to_string());
    let parts = ANN_PARTITIONS.get_or_init(Default::default).lock().unwrap().get(&key).cloned();
    if let Some(parts) = parts.filter(|parts| parts.len() as u64 <= options["maxCachedShards"].as_u64().unwrap_or(4)) {
        let mut rows = Vec::new();
        let mut complete = true;
        for part in parts {
            match ann(config, &part, vector, scopes, identity, epoch, options, limit, false, false) {
                Ok(found) => rows.extend(found),
                Err(error) if error.code() == "RETRIEVAL_ANN_BUILD_PENDING" => { complete = false; break; }
                Err(error) => return Err(error),
            }
        }
        // Return a complete shard set only; missing shards must not silently reduce semantic coverage.
        // 仅返回完整分片集合，缺图时不能静默降低语义覆盖范围。
        if complete { return Ok(rows); }
    }
    enqueue_ann_descriptor(config, descriptor, scopes, identity, epoch, options)?;
    Err(gateway::error("ANN 正在后台准备，当前使用可用检索。", "RETRIEVAL_ANN_BUILD_PENDING", 503))
}
fn graph_page_interrupted(started: std::time::Instant) -> bool {
    crate::service_cancellation::is_cancelled()
        || started.elapsed() >= std::time::Duration::from_secs(120)
}
fn admit_exact_descriptor(descriptor: &Value, remaining: &mut u64) -> bool {
    let count = descriptor["count"].as_u64().unwrap_or(u64::MAX);
    if count > *remaining { return false; }
    *remaining -= count;
    true
}

fn ann_readiness_error(code: &str) -> bool {
    ["RETRIEVAL_ANN_BUILD_PENDING", "RETRIEVAL_ANN_QUEUE_FULL", "RETRIEVAL_ANN_RESOURCE_LIMIT",
        "RESOURCE_RUNTIME_UNAVAILABLE"].contains(&code)
}
fn ann(
    config: &GatewayConfig,
    descriptor: &Value,
    vector: &[f32],
    scopes: &Value,
    identity: &Value,
    epoch: &Value,
    options: &Value,
    limit: usize,
    allow_prepare: bool,
    background: bool,
) -> Result<Vec<Value>, ContractError> {
    if !background {
        request_graph_workspace(database(config), descriptor["scope_key"].as_str().unwrap_or(""));
    }
    let descriptor_identity = graph_identity(descriptor, identity, epoch, options);
    let key = hash(&descriptor_identity.to_string());
    let generation = descriptor["generation"].as_u64().unwrap();
    let dimensions = descriptor["dimensions"].as_u64().unwrap() as usize;
    let count = descriptor["count"].as_u64().unwrap() as usize;
    let cache = GRAPHS.get_or_init(Default::default);
    let resident = cache.lock().unwrap().get(&key).cloned();
    if resident.is_none() && !allow_prepare {
        return Err(gateway::error("ANN 正在后台准备。", "RETRIEVAL_ANN_BUILD_PENDING", 503));
    }
    let build_owner = if resident.is_none() {
        let mut builds = GRAPH_BUILDS.get_or_init(Default::default).lock().unwrap();
        builds.retain(|_, owner| owner.strong_count() > 0);
        Some(match builds.get(&key).and_then(std::sync::Weak::upgrade) {
            Some(owner) => owner,
            None => {
                if builds.len() >= 16 {
                    return Err(gateway::error(
                        "建图队列已满，精确查询仍可用。",
                        "RETRIEVAL_ANN_RESOURCE_LIMIT",
                        503,
                    ));
                }
                let owner = Arc::new(Mutex::new(()));
                builds.insert(key.clone(), Arc::downgrade(&owner));
                owner
            }
        })
    } else {
        None
    };
    let _building = build_owner.as_ref().map(|owner| owner.lock().unwrap());
    let resident = resident.or_else(|| cache.lock().unwrap().get(&key).cloned());
    let graph = match resident {
        Some(graph) => graph,
        None => {
            let estimate = (count as u64)
                .saturating_mul(
                    (dimensions as u64)
                        .saturating_mul(4)
                        .saturating_add(
                            options["connectivity"]
                                .as_u64()
                                .unwrap_or(16)
                                .saturating_mul(16),
                        )
                        .saturating_add(256),
                )
                .saturating_add(1024 * 1024);
            if estimate
                > options["maxShardBytes"]
                    .as_u64()
                    .unwrap_or(256 * 1024 * 1024)
            {
                return Err(gateway::error(
                    "当前分片超过获准资源范围，改用精确查询。",
                    "RETRIEVAL_ANN_RESOURCE_LIMIT",
                    503,
                ));
            }
            let token = crate::service_cancellation::current_token()
                .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));
            let lease = super::gateway_embedding::reserve_graph(estimate, &token, background)?;
            if lease.allocation["memoryBytes"].as_u64().unwrap_or(0) < estimate {
                return Err(gateway::error(
                    "图资源实际授额不足。",
                    "RETRIEVAL_ANN_RESOURCE_LIMIT",
                    503,
                ));
            }
            let settings = IndexOptions {
                dimensions,
                metric: MetricKind::Cos,
                quantization: ScalarKind::F32,
                connectivity: options["connectivity"].as_u64().unwrap_or(16) as usize,
                expansion_add: options["expansionAdd"].as_u64().unwrap_or(128) as usize,
                expansion_search: options["expansionSearch"].as_u64().unwrap_or(128) as usize,
                ..Default::default()
            };
            let index = Index::new(&settings)
                .map_err(|_| gateway::error("原生 ANN 创建失败。", "RETRIEVAL_ANN_FAILED", 500))?;
            let directory = config.data_root.join("Index/ann");
            storage::managed_files::inspect(&config.data_root, &directory, true, false, u64::MAX)?;
            let metadata = storage::ann_cache::apply(
                "storage.ann.load",
                &json!({"root":config.data_root.join("Index"),"key":key,"identityRaw":descriptor_identity.to_string(),
            "generation":generation,"count":count,"maximumBytes":options["maxShardBytes"]}),
            )?;
            let mut loaded = false;
            if metadata["eligible"] == true {
                if let Some(bytes) = storage::managed_files::read(
                    &config.data_root,
                    &directory.join(format!("{key}.usearch")),
                    options["maxShardBytes"]
                        .as_u64()
                        .unwrap_or(256 * 1024 * 1024),
                )? {
                    loaded = index.load_from_buffer(&bytes).is_ok() && index.size() == count;
                }
            }
            if !loaded {
                index.reserve(count).map_err(|_| {
                    gateway::error("ANN 内存准入失败。", "RETRIEVAL_ANN_RESOURCE_LIMIT", 503)
                })?;
                let connection = storage::sqlite_connections::acquire(
                    &config.data_root,
                    &database(config),
                    120000,
                )?;
                connection
                    .execute_batch("BEGIN")
                    .map_err(storage::sqlite_store::failure)?;
                let result = (|| {
                    let mut last_id = 0;
                    loop {
                        storage::sqlite_store::cancelled()?;
                        if background {
                            // Bound each SQL page; graph progress renews the next page's deadline.
                            // SQL 每页有期限，构图取得进展后为下一页续期。
                            let page_started = std::time::Instant::now();
                            connection.progress_handler(1000, Some(move || graph_page_interrupted(page_started)))
                                .map_err(storage::sqlite_store::failure)?;
                        }
                        let args = json!({"descriptor":descriptor,"scopes":scopes,"binaryRequiredScope":true,"lastId":last_id});
                        let Some((sql, parameters)) =
                            storage::sqlite_vector_reads::vector_page_query(&connection, &args)?
                        else {
                            break;
                        };
                        let mut statement = connection
                            .prepare_cached(&sql)
                            .map_err(storage::sqlite_store::failure)?;
                        let rows = statement
                            .query_map(rusqlite::params_from_iter(parameters), |row| {
                                Ok((row.get::<_, u64>(0)?, row.get::<_, Vec<u8>>(2)?))
                            })
                            .map_err(storage::sqlite_store::failure)?;
                        let mut seen = 0;
                        for row in rows {
                            storage::sqlite_store::cancelled()?;
                            let (id, bytes) = row.map_err(storage::sqlite_store::failure)?;
                            last_id = id;
                            seen += 1;
                            let values: Vec<f32> = bytes
                                .chunks_exact(4)
                                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                                .collect();
                            if values.len() != dimensions
                                || values.iter().any(|value| !value.is_finite())
                            {
                                return Err(gateway::error(
                                    "已存向量无效。",
                                    "INVALID_RETRIEVAL_VECTOR",
                                    409,
                                ));
                            }
                            index.add(id, &values).map_err(|_| {
                                gateway::error("ANN 添加失败。", "RETRIEVAL_ANN_FAILED", 500)
                            })?;
                        }
                        if seen == 0 {
                            break;
                        }
                    }
                    if index.size() != count {
                        return Err(gateway::error(
                            "图与当前语料数量不一致。",
                            "RETRIEVAL_ANN_STALE_BUILD",
                            409,
                        ));
                    }
                    Ok(())
                })();
                connection
                    .execute_batch("ROLLBACK")
                    .map_err(storage::sqlite_store::failure)?;
                result?;
                let temporary = format!("{key}.usearch.{}.tmp", gateway::new_id());
                let path = directory.join(&temporary);
                let mut bytes = vec![0; index.serialized_length()];
                index.save_to_buffer(&mut bytes).map_err(|_| {
                    gateway::error("ANN 编码失败。", "RETRIEVAL_ANN_CACHE_WRITE_FAILED", 500)
                })?;
                storage::managed_files::atomic_write(
                    &config.data_root,
                    &path,
                    &bytes,
                    options["maxShardBytes"]
                        .as_u64()
                        .unwrap_or(256 * 1024 * 1024),
                )?;
                let receipt = storage::ann_cache::apply(
                    "storage.ann.publish",
                    &json!({"root":config.data_root.join("Index"),"key":key,"temporary":temporary,"identityRaw":descriptor_identity.to_string(),
                "generation":generation,"count":count,"maximumBytes":options["maxShardBytes"]}),
                );
                if receipt.is_err() {
                    storage::ann_cache::apply(
                        "storage.ann.discardTemporary",
                        &json!({"root":config.data_root.join("Index"),"key":key,"temporary":temporary}),
                    )?;
                }
                receipt?;
            }
            validate_graph_source(config, descriptor, identity, epoch)?;
            let graph = Arc::new(Graph {
                index,
                database: database(config),
                scope: descriptor["scope_key"].as_str().unwrap_or("").to_string(),
                identity: descriptor_identity.to_string(),
                generation,
                count,
                _lease: lease,
            });
            graph
        }
    };
    if graph.generation != generation
        || graph.count != count
        || graph.identity != descriptor_identity.to_string()
    {
        return Err(gateway::error(
            "图版本已变化。",
            "RETRIEVAL_ANN_STALE_BUILD",
            409,
        ));
    }
    // Publish cold graphs to disk; retain residency only for foreground-used workspaces.
    // 冷图可后台写盘，仅前台使用过的工作区保留驻留。
    remember_graph(&key, &graph, options);
    let matches = if limit > 0 {
        Some(graph.index.search(vector, limit)
            .map_err(|_| gateway::error("ANN 查询失败。", "RETRIEVAL_ANN_FAILED", 500))?)
    } else { None };
    Ok(matches.map(|matches| matches.keys.iter().zip(matches.distances)
        .map(|(id, distance)| json!({"id":id,"distance":distance,"identity":descriptor})).collect()).unwrap_or_default())
}
pub fn search(config: &GatewayConfig, input: &Value) -> Result<Value, ContractError> {
    let boot = initialize(config)?;
    storage::retrieval::apply(
        "storage.retrieval.scopes",
        &json!({"value":input["scopeKeys"]}),
    )?;
    // Admit optional cache memory before opening the SQLite snapshot, without delaying chat.
    // 进入 SQLite 快照前准入可选缓存内存，不阻塞聊天。
    let candidate_bytes = super::gateway_embedding::cache_memory_limit(
        "retrieval-candidates", super::cache_candidates::DEFAULT_BYTES, super::cache_candidates::reclaim,
    );
    let _candidate_cache_grant = if candidate_bytes > 0 {
        super::gateway_cache::memory_grant("retrieval-candidates")
    } else { None };
    let passage_bytes = super::gateway_embedding::cache_memory_limit(
        "source-passages", super::cache_passages::DEFAULT_BYTES, super::cache_passages::reclaim,
    );
    let _passage_cache_grant = if passage_bytes > 0 {
        super::gateway_cache::memory_grant("source-passages")
    } else { None };
    let query = input["query"].as_str().unwrap_or("");
    let expression=crate::retrieval::lexical::apply("retrieval.match-expression",&json!({"text":query,"normalizedText":query.nfkc().collect::<String>(),"domain":input["retrievalIntent"]["domain"]}))
        .map_err(|_|gateway::error("查询格式无效。","INVALID_RETRIEVAL_INPUT",400))?;
    // Use FTS/vectors for language; literal scans only for explicit targets or untokenizable input.
    // 自然语言走 FTS 与向量，仅显式目标或无法分词时回退字面扫描。
    let has_explicit_target = ["path", "symbol"].iter().any(|field| {
        input["retrievalIntent"][*field].as_str().is_some_and(|value| !value.trim().is_empty())
    });
    let literal_query = input["literalQuery"].as_str().unwrap_or_else(|| {
        if has_explicit_target || expression.as_str().is_none_or(str::is_empty) { query } else { "" }
    });
    // Use stable database IDs for cached descriptors; recheck file identity, epochs and scope.
    // 描述符缓存使用稳定数据库 ID，仍核验文件身份、版本和范围。
    let descriptor_cache_id = format!("native-index:{}", hash(&database(config).to_string_lossy()));
    let options = storage::settings::apply(
        "storage.settings.ann",
        &json!({"value":input.get("ann").unwrap_or(&json!({}))}),
    )?;
    let vector = input["queryVector"].as_array().map(|items| {
        items
            .iter()
            .map(|value| value.as_f64().unwrap_or(f64::NAN) as f32)
            .collect::<Vec<_>>()
    });
    let request_id = gateway::new_id();
    let mut begin = arguments(
        config,
        json!({"requestId":request_id,"cacheId":descriptor_cache_id,"query":query,"scopes":input["scopeKeys"],"expression":expression,"literalQuery":literal_query,
        "metadataReady":input["metadataReady"],"partial":input["partial"],
        "candidateLimit":input["channelCandidates"].as_u64().unwrap_or(64).clamp(1,160),"resultLimit":input["limit"].as_u64().unwrap_or(8).clamp(1,192).saturating_mul(4).min(192),
        "modelProvided":input.get("embeddingModelVersion").is_some(),"model":input["embeddingModelVersion"],
        "spaceProvided":input.get("embeddingSpaceId").is_some(),"space":input["embeddingSpaceId"],"domain":input["retrievalIntent"]["domain"],
        "symbol":input["retrievalIntent"]["symbol"],"pathFilter":input["retrievalIntent"]["path"],"directExact":options["mode"]=="exact",
        "rrfK":input["rrfK"].as_u64().unwrap_or(60).clamp(20,100),"passageCacheAdmitted":_passage_cache_grant.is_some(),
        "exactScanLimit":options["exactScanLimit"],"extensionPath":config.vector_extension}),
    );
    if let Some(vector) = &vector {
        begin["queryVector"] = json!(vector);
        begin["profile"] = input["embeddingProfileId"].clone();
    }
    let prepared = sqlite_search::apply("storage.index.queryBegin", &begin)?;
    let query_id = prepared["queryId"].clone();
    let result = (|| {
        let mut candidates = prepared["candidateIds"].clone();
        let mut semantic_scan_limited = prepared["exactScanLimited"] == true;
        let mut exact_remaining = options["exactScanLimit"].as_u64().unwrap_or(1000000);
        let mut semantic = if prepared["vectorsReady"] == true && !semantic_scan_limited {
            Some("exact")
        } else {
            None
        };
        let mut ann_pending = false;
        if let Some(vector) = &vector {
            if prepared["vectorsReady"] != true {
                let mut matches = Vec::new();
                let mut exact = Vec::new();
                for descriptor in prepared["descriptors"].as_array().unwrap() {
                    let approximate = options["mode"] == "ann"
                        || (options["mode"] == "auto"
                            && descriptor["count"].as_u64().unwrap()
                                > options["threshold"].as_u64().unwrap());
                    if approximate {
                        if options["mode"] == "auto" {
                            match ann_auto(config, descriptor, vector, &input["scopeKeys"],
                                &prepared["databaseIdentity"], &boot["epoch"], &options,
                                begin["candidateLimit"].as_u64().unwrap() as usize) {
                                Ok(rows) => { matches.extend(rows); semantic = Some("ann"); }
                                Err(error) if ann_readiness_error(error.code()) => {
                                    ann_pending = true;
                                    // Use bounded exact search when ANN is cold or resource-limited.
                                    // ANN 未就绪或资源不足时，沿用有界精确检索。
                                    if admit_exact_descriptor(descriptor, &mut exact_remaining) {
                                        exact.push(descriptor.clone());
                                        semantic = Some("exact");
                                    } else { semantic_scan_limited = true; }
                                }
                                Err(error) => return Err(error),
                            }
                            continue;
                        }
                        let dimensions = descriptor["dimensions"].as_u64().unwrap_or(384);
                        let connectivity = options["connectivity"].as_u64().unwrap_or(16);
                        let per_vector = dimensions * 4 + connectivity * 16 + 256;
                        let capacity = options["maxShardBytes"]
                            .as_u64()
                            .unwrap_or(256 * 1024 * 1024)
                            .saturating_sub(1024 * 1024)
                            .checked_div(per_vector)
                            .unwrap_or(0)
                            .max(1);
                        let segments = descriptor["count"].as_u64().unwrap().div_ceil(capacity);
                        let partitions = if segments > 1 {
                            sqlite_queries::apply(
                                "storage.index.vectorPartition",
                                &arguments(
                                    config,
                                    json!({"descriptor":descriptor,"segments":segments}),
                                ),
                            )?
                        } else {
                            json!([{"count":descriptor["count"]}])
                        };
                        for partition in partitions.as_array().unwrap() {
                            let mut shard = descriptor.clone();
                            shard
                                .as_object_mut()
                                .unwrap()
                                .extend(partition.as_object().unwrap().clone());
                            match ann(
                                config,
                                &shard,
                                vector,
                                &input["scopeKeys"],
                                &prepared["databaseIdentity"],
                                &boot["epoch"],
                                &options,
                                begin["candidateLimit"].as_u64().unwrap() as usize,
                                true,
                                false,
                            ) {
                                Ok(rows) => {
                                    matches.extend(rows);
                                    semantic = Some("ann");
                                }
                                Err(error) if error.code() == "ABORT_ERR" => return Err(error),
                                Err(error) => {
                                    if admit_exact_descriptor(&shard, &mut exact_remaining) {
                                        exact.push(shard);
                                        semantic = Some("exact");
                                    } else if ann_readiness_error(error.code()) {
                                        semantic_scan_limited = true;
                                    } else {
                                        return Err(error);
                                    }
                                }
                            }
                        }
                    } else {
                        if admit_exact_descriptor(descriptor, &mut exact_remaining) {
                            exact.push(descriptor.clone());
                            semantic = Some("exact");
                        } else { semantic_scan_limited = true; }
                    }
                }
                let receipt = sqlite_search::apply(
                    "storage.index.queryVectors",
                    &arguments(
                        config,
                        json!({"queryId":query_id,"requestId":request_id,"matches":matches,"exactDescriptors":exact,"extensionPath":config.vector_extension}),
                    ),
                )?;
                candidates = receipt["candidateIds"].clone();
            }
        }
        candidates
            .as_array_mut()
            .unwrap()
            .sort_by(|left, right| left.as_str().unwrap().cmp(right.as_str().unwrap()));
        let finalized = sqlite_search::apply(
            "storage.index.queryFinish",
            &arguments(
                config,
                json!({"queryId":query_id,"requestId":request_id,"collationOrder":candidates}),
            ),
        )?;
        let mut items = finalized["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(public_chunk)
            .collect::<Result<Vec<_>, _>>()?;
        let ranking = super::fusion::lightweight(&mut items, input);
        for item in &mut items {
            if item["mergedChunks"].is_array() {
                item["sourceRef"] = json!(reference(item, None));
            }
        }
        let mut response = finalized["snapshot"].clone();
        response["items"] = json!(items);
        response["ranking"] = ranking;
        response["strategy"] = finalized["strategy"].clone();
        response["semanticBackend"] = json!(semantic);
        response["semanticScanLimited"] = json!(semantic_scan_limited);
        response["vectorAvailable"] = json!(config.vector_extension.is_file());
        if options["mode"] == "auto" {
            let mut background = background_operation(config, "status", json!({"now":chrono::Utc::now().timestamp_millis()}))?;
            background["state"] = json!(if ann_pending {
                if background["buildErrorCode"] == "RETRIEVAL_ANN_RESOURCE_LIMIT" { "resource_limited" } else { "preparing" }
            } else if semantic == Some("ann") { "ready" } else { "idle" });
            response["annBackground"] = background;
        }
        Ok(response)
    })();
    if result.is_err() {
        let _ = sqlite_search::apply(
            "storage.index.queryDiscard",
            &arguments(config, json!({"queryId":query_id,"requestId":request_id})),
        );
    }
    result
}
fn public_chunk(row: &Value) -> Result<Value, ContractError> {
    let source = json!({"sourceId":row["source_id"],"scopeKey":row["scope_key"],"contentHash":row["content_hash"],
        "sourceRevision":serde_json::from_str::<Value>(row["source_revision"].as_str().unwrap_or("null")).unwrap_or(Value::Null),"derivationSignature":row["derivation_signature"]});
    let chunk = json!({"chunkId":row["chunk_id"],"chunkHash":row["chunk_hash"]});
    let locator = serde_json::from_str::<Value>(row["locator"].as_str().unwrap_or("{}"))
        .unwrap_or(json!({}));
    let exact_path = storage::retrieval::normalize_path(locator["relativePath"].as_str()
        .or_else(||locator["path"].as_str()).unwrap_or(""));
    // Keep native locator facts through projection so exact evidence does not trigger extra inference.
    // 原生定位事实随投影保留，避免有效精确证据因字段丢失触发额外推理。
    Ok(
        json!({"sourceId":source["sourceId"],"scopeKey":source["scopeKey"],"sourceType":row["source_type"],"title":row["title"],
        "locator":locator,"exactPath":exact_path,"exactTargetMatch":row["exact_target_match"]==true,"excerpt":row["text"],
        "contentHash":source["contentHash"],"sourceRevision":source["sourceRevision"],"derivationSignature":row["derivation_signature"],"chunkId":row["chunk_id"],"chunkHash":row["chunk_hash"],
        "startOffset":row["start_offset"],"endOffset":row["end_offset"],"startLine":row["start_line"],"endLine":row["end_line"],"score":row["score"],"distance":row["distance"],"structure":serde_json::from_str::<Value>(row["structure_json"].as_str().unwrap_or("null")).unwrap_or(Value::Null),"sourceRef":reference(&source,Some(&chunk))}),
    )
}
pub fn status(config: &GatewayConfig) -> Result<Value, ContractError> {
    initialize(config)?;
    sqlite_queries::apply("storage.index.status", &arguments(config, json!({})))
}

#[cfg(test)]
mod native_query_route_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn structural_merge_derivation_invalidates_only_old_chunker_reuse() {
        let prepared = prepare_source(&json!({"sourceId":"grouped-fixture","scopeKey":"project:fixture",
            "sourceType":"work-file","title":"Notes","locator":{"relativePath":"notes.md"},
            "text":"# Section\n\nFirst paragraph.\n\nSecond paragraph.\n"}), None).unwrap();
        let source = &prepared["source"];
        assert_eq!(source["chunkerVersion"], super::super::gateway_structure::CHUNKER_VERSION);
        assert_eq!(prepared["chunks"].as_array().unwrap().len(), 2);
        let mut state = json!({"source_id":source["sourceId"],"scope_key":source["scopeKey"],
            "source_type":source["sourceType"],"content_hash":source["contentHash"],
            "source_revision":source["sourceRevision"].to_string(),"binding_revision":0,
            "title":source["title"],"locator":source["locator"].to_string(),
            "parser_version":source["parserVersion"],"chunker_version":source["chunkerVersion"],
            "embedding_input_version":"source-context-v1"});
        assert!(source_matches(&state, source));
        state["chunker_version"] = json!("structure-lines-v1");
        assert!(!source_matches(&state, source));
        state["chunker_version"] = source["chunkerVersion"].clone();
        assert!(source_matches(&state, source));
    }

    struct SearchFixture {
        root: PathBuf,
        config: GatewayConfig,
    }

    impl SearchFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("kynxa-native-query-route-{}", gateway::new_id()));
            std::fs::create_dir_all(&root).unwrap();
            let config = GatewayConfig {
                data_home: root.join("Models"), data_root: root.join("Data"),
                extension_root: root.join("Extensions"), application_root: root.clone(), port: 0,
                toolhost_exe: root.join("unused-toolhost.exe"), vector_extension: root.join("unused-vec0.dll"),
                inference_library: root.join("unused-ort.dll"),
            };
            for directory in [&config.data_home, &config.data_root, &config.extension_root] {
                std::fs::create_dir_all(directory).unwrap();
            }
            Self { root, config }
        }

        fn publish(&self, id: &str, text: &str) {
            let source = json!({"sourceId":id,"scopeKey":"project:query-route","sourceType":"document",
                "title":id,"locator":{},"sourceRevision":1,"text":text,
                "embeddingProfileId":"fixture","embeddingModelVersion":"fixture-v1","embeddingSpaceId":"fixture-space"});
            let prepared = prepare_source(&source, Some(&[vec![1.0, 0.1]])).unwrap();
            assert_eq!(prepared["chunks"].as_array().unwrap().len(), 1);
            publish_prepared(&self.config, &[prepared]).unwrap();
        }

        fn cache_id(&self) -> String {
            format!("native-index:{}", hash(&database(&self.config).to_string_lossy()))
        }

        fn descriptor_status(&self) -> Value {
            storage::sqlite_vector_reads::cache_operation("storage.index.vectorCacheStatus",
                &json!({"cacheId":self.cache_id()})).unwrap()["descriptorCache"].clone()
        }
    }

    impl Drop for SearchFixture {
        fn drop(&mut self) {
            stop_ann_background(&self.config);
            let _ = storage::sqlite_vector_reads::cache_operation("storage.index.vectorCacheClose",
                &json!({"cacheId":self.cache_id()}));
            let _ = storage::sqlite_connections::close(&self.config.data_root, &database(&self.config));
            if let Some(bootstraps) = BOOTSTRAPS.get() {
                if let Ok(mut bootstraps) = bootstraps.lock() { bootstraps.remove(&database(&self.config)); }
            }
            if self.root.starts_with(std::env::temp_dir()) { let _ = std::fs::remove_dir_all(&self.root); }
        }
    }

    #[test]
    fn natural_queries_skip_literal_scan_and_explicit_targets_keep_compatibility() {
        let fixture = SearchFixture::new();
        fixture.publish("matching", "needle target ???");
        fixture.publish("sentinel", "unique_sentinel_only_unrelated");
        let scanned_sentinel = Arc::new(AtomicUsize::new(0));
        {
            let connection = storage::sqlite_connections::acquire(
                &fixture.config.data_root, &database(&fixture.config), 120000).unwrap();
            let observed = scanned_sentinel.clone();
            // Instrument the real SQLite lower() while preserving its ASCII behavior; an unrelated row exposes a full literal scan.
            // 在真实 SQLite 上保留 lower() 的 ASCII 语义并计数；无关行被执行可直接证明发生全库子串扫描。
            connection.create_scalar_function("lower", 1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
                move |context| {
                    let value = context.get::<Option<String>>(0)?;
                    if value.as_deref().is_some_and(|value| value.contains("unique_sentinel_only_unrelated")) {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(value.map(|value| value.to_ascii_lowercase()))
                }).unwrap();
        }
        let query = json!({"query":"needle","scopeKeys":["project:query-route"]});
        let result = search(&fixture.config, &query).unwrap();
        assert_eq!(result["items"][0]["sourceId"], "matching");
        assert_eq!(scanned_sentinel.load(Ordering::SeqCst), 0,
            "An ordinary FTS query must not run lower() on an unrelated chunk.");
        for target in [json!({"path":"target.rs"}), json!({"symbol":"TargetSymbol"})] {
            scanned_sentinel.store(0, Ordering::SeqCst);
            let mut exact = query.clone();
            exact["retrievalIntent"] = target;
            search(&fixture.config, &exact).unwrap();
            assert!(scanned_sentinel.load(Ordering::SeqCst) > 0,
                "Explicit path/symbol intent retains the compatible literal channel.");
        }
        let untokenizable = search(&fixture.config,
            &json!({"query":"???","scopeKeys":["project:query-route"]})).unwrap();
        assert_eq!(untokenizable["items"][0]["sourceId"], "matching");
        assert_eq!(untokenizable["items"].as_array().unwrap().len(),1);
        // Candidate reuse and connection pooling may skip the instrumented lower(); results prove the fallback.
        // 候选复用及连接池可能跳过被计数的 lower()，以只有字面回退才能找到的结果验证行为。
        let mut explicit_literal = query.clone();
        explicit_literal["query"] = json!("unmatched_natural_query");
        explicit_literal["literalQuery"] = json!("needle");
        let explicit_result = search(&fixture.config, &explicit_literal).unwrap();
        assert_eq!(explicit_result["items"][0]["sourceId"], "matching");
        assert_eq!(explicit_result["items"].as_array().unwrap().len(),1);
    }

    #[test]
    fn native_descriptor_cache_reuses_stable_owner_and_rechecks_addition_deletion_and_scopes() {
        let fixture = SearchFixture::new();
        fixture.publish("first", "needle first evidence");
        fixture.publish("other", "unrelated evidence");
        // A cold ANN plus a one-row exact allowance exercises descriptors without loading any external vector DLL or model.
        // 冷 ANN 与一行精确预算只检查描述符，不加载外部向量 DLL 或模型；来源始终至少两行。
        let query = json!({"query":"needle","scopeKeys":["project:query-route"],"queryVector":[1.0,0.1],
            "embeddingProfileId":"fixture","embeddingModelVersion":"fixture-v1","embeddingSpaceId":"fixture-space",
            "ann":{"mode":"auto","threshold":1,"exactScanLimit":1},"limit":16});
        assert_eq!(search(&fixture.config, &query).unwrap()["items"].as_array().unwrap().len(), 1);
        let cold = fixture.descriptor_status();
        assert_eq!(cold["misses"], 1);
        assert_eq!(cold["hits"], 0);
        search(&fixture.config, &query).unwrap();
        assert_eq!(fixture.descriptor_status()["hits"], 1);
        fixture.publish("added", "needle added evidence");
        assert_eq!(search(&fixture.config, &query).unwrap()["items"].as_array().unwrap().len(), 2);
        assert_eq!(fixture.descriptor_status()["misses"], 2,
            "A newly indexed source changes the scope generation and cannot reuse old descriptors.");
        remove(&fixture.config, "added", &json!(["project:query-route"]), true).unwrap();
        let removed = search(&fixture.config, &query).unwrap();
        assert_eq!(removed["items"].as_array().unwrap().len(), 1);
        assert!(removed["items"].as_array().unwrap().iter().all(|item| item["sourceId"] != "added"));
        assert_eq!(fixture.descriptor_status()["misses"], 3);
        let mut unauthorized = query.clone();
        unauthorized["scopeKeys"] = json!(["project:unlinked"]);
        assert!(search(&fixture.config, &unauthorized).unwrap()["items"].as_array().unwrap().is_empty());
        assert_eq!(fixture.descriptor_status()["misses"], 4,
            "An identical query vector must not reuse descriptors from an unauthorized scope.");
        search(&fixture.config, &query).unwrap();
        assert_eq!(fixture.descriptor_status()["hits"], 2);
    }
}

#[cfg(all(test, windows))]
mod ann_background_tests {
    use super::*;

    fn workspace(database: &str, scope: &str) -> GraphWorkspace {
        GraphWorkspace { database: PathBuf::from(database), scope: scope.to_owned() }
    }

    #[test]
    fn graph_residency_keeps_four_workspaces_with_independent_shard_budgets() {
        let mut state = GraphResidency::default();
        for database in ["first.sqlite", "second.sqlite"] {
            for scope in ["project:a", "project:b"] {
                let owner = workspace(database, scope);
                assert!(state.demand(owner.clone()).is_empty());
                for shard in 0..8 {
                    let key = format!("{database}:{scope}:{shard}");
                    assert!(state.remember(&key, owner.clone(), 8).unwrap().is_empty());
                }
            }
        }
        assert_eq!(state.workspaces.len(), 4);
        assert_eq!(state.shards.len(), 32, "Four workspaces are not a global four-graph limit.");
        let evicted = state.demand(workspace("third.sqlite", "project:a"));
        assert_eq!(evicted.len(), 8);
        assert!(evicted.iter().all(|key| key.starts_with("first.sqlite:project:a:")),
            "Equal scope names in different databases remain separate workspaces.");
        assert_eq!(state.shards.len(), 24);
    }

    #[test]
    fn graph_residency_evicts_only_the_cold_workspace_and_preserves_recent_access() {
        let mut state = GraphResidency::default();
        for index in 0..4 {
            let owner = workspace("index.sqlite", &format!("project:{index}"));
            state.demand(owner.clone());
            state.remember(&format!("graph:{index}"), owner, 4).unwrap();
        }
        assert!(state.demand(workspace("index.sqlite", "project:0")).is_empty());
        assert_eq!(state.demand(workspace("index.sqlite", "project:4")), vec!["graph:1"]);
        assert!(state.shards.iter().any(|(key, _)| key == "graph:0"));
    }

    #[test]
    fn graph_residency_background_publication_stays_lazy_and_shards_do_not_starve_other_workspaces() {
        let mut state = GraphResidency::default();
        let first = workspace("index.sqlite", "project:first");
        let second = workspace("index.sqlite", "project:second");
        assert!(state.remember("unrequested", first.clone(), 16).is_none());
        assert!(state.workspaces.is_empty());
        state.demand(first.clone());
        state.demand(second.clone());
        state.remember("other", second.clone(), 4).unwrap();
        for index in 0..16 { state.remember(&format!("first:{index}"), first.clone(), 16).unwrap(); }
        assert_eq!(state.shards.len(), 17);
        assert_eq!(state.remember("first:new", first, 16).unwrap(), vec!["first:0"]);
        assert!(state.shards.iter().any(|(key, owner)| key == "other" && owner == &second));
    }

    #[test]
    fn auto_ann_shard_allowance_remains_adaptive_and_respects_explicit_limits() {
        let defaults = json!({"adaptive":true,"maxCachedShards":4,"maxShardBytes":1049600,"connectivity":16});
        assert_eq!(auto_options(&json!({"dimensions":2,"count":1}), &defaults)["maxCachedShards"], 4);
        assert_eq!(auto_options(&json!({"dimensions":2,"count":8}), &defaults)["maxCachedShards"], 8);
        assert_eq!(auto_options(&json!({"dimensions":2,"count":100}), &defaults)["maxCachedShards"], 16);
        let mut explicit = defaults;
        explicit["adaptive"] = json!(false);
        explicit["maxCachedShards"] = json!(1);
        assert_eq!(auto_options(&json!({"dimensions":2,"count":8}), &explicit)["maxCachedShards"], 1);
    }

    #[test]
    fn exact_ann_fallback_shares_one_scan_allowance_across_descriptors() {
        let mut remaining = 10;
        assert!(admit_exact_descriptor(&json!({"count":6}), &mut remaining));
        assert!(!admit_exact_descriptor(&json!({"count":5}), &mut remaining));
        assert_eq!(remaining, 4, "A declined fallback does not consume the remaining allowance.");
        assert!(admit_exact_descriptor(&json!({"count":4}), &mut remaining));
        assert_eq!(remaining, 0);
        assert!(!admit_exact_descriptor(&json!({}), &mut remaining));
    }

    #[test]
    fn ann_not_ready_falls_back_without_hiding_cancellation_or_corruption() {
        assert!(ann_readiness_error("RETRIEVAL_ANN_BUILD_PENDING"));
        assert!(ann_readiness_error("RETRIEVAL_ANN_RESOURCE_LIMIT"));
        assert!(ann_readiness_error("RESOURCE_RUNTIME_UNAVAILABLE"));
        assert!(!ann_readiness_error("ABORT_ERR"));
        assert!(!ann_readiness_error("CORRUPT_RETRIEVAL_INDEX"));
    }

    #[test]
    fn graph_page_deadline_renews_without_removing_cancellation() {
        let previous = std::time::Instant::now() - std::time::Duration::from_secs(121);
        assert!(graph_page_interrupted(previous));
        assert!(!graph_page_interrupted(std::time::Instant::now()));
        let cancellation = crate::service_cancellation::CancellationLease::acquire(&gateway::new_id()).unwrap();
        let _scope = cancellation.enter();
        cancellation.token.store(true, std::sync::atomic::Ordering::Release);
        assert!(graph_page_interrupted(std::time::Instant::now()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires KYNXA_TEST_RESOURCE_SERVICE pointing to the isolated native resource helper"]
    async fn auto_ann_cold_query_background_reuse_restart_and_update() {
        let helper = std::env::var_os("KYNXA_TEST_RESOURCE_SERVICE")
            .expect("set KYNXA_TEST_RESOURCE_SERVICE to the independently built resource helper");
        let service = crate::resources::gateway::ResourceService::start(std::path::Path::new(&helper))
            .await.unwrap();
        crate::resources::gateway::install(service.clone());
        super::super::gateway_embedding::install_runtime(tokio::runtime::Handle::current());
        let root = std::env::temp_dir().join(format!("kynxa-ann-background-{}", gateway::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let config = GatewayConfig {
            data_home: root.join("Models"), data_root: root.join("Data"),
            extension_root: root.join("Extensions"), application_root: root.clone(), port: 0,
            toolhost_exe: root.join("unused-toolhost.exe"), vector_extension: root.join("unused-vec0.dll"),
            inference_library: root.join("unused-ort.dll"),
        };
        for directory in [&config.data_home, &config.data_root, &config.extension_root] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let fixture = config.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let source = |index: usize, revision: u64| json!({"sourceId":format!("fixture-{index}"),
                    "scopeKey":"project:fixture","sourceType":"document","title":format!("Alpha {index}"),
                    "locator":{},"sourceRevision":revision,"text":format!("alpha physics evidence {index} revision {revision}"),
                    "embeddingProfileId":"fixture","embeddingModelVersion":"fixture-v1","embeddingSpaceId":"fixture-space"});
                let prepared = (0..8).map(|index| prepare_source(&source(index, 1),
                    Some(&[vec![1.0, (index as f32 + 1.0) / 10.0]])).unwrap()).collect::<Vec<_>>();
                publish_prepared(&fixture, &prepared).unwrap();
                resume_ann_background(&fixture);
                let query = json!({"query":"alpha","scopeKeys":["project:fixture"],"queryVector":[1.0,0.1],
                    "embeddingProfileId":"fixture","embeddingModelVersion":"fixture-v1","embeddingSpaceId":"fixture-space",
                    "ann":{"mode":"auto","threshold":1,"exactScanLimit":1,"maxShardBytes":1049600}});
                let first = search(&fixture, &query).unwrap();
                assert_eq!(first["semanticBackend"], Value::Null);
                assert_eq!(first["annBackground"]["state"], "preparing");
                assert!(!first["items"].as_array().unwrap().is_empty());
                search(&fixture, &query).unwrap();
                let queued = background_operation(&fixture, "status", json!({"now":0})).unwrap();
                assert_eq!(queued["pendingBuilds"], 1, "Repeated cold queries share one queued build.");
                assert!(GRAPHS.get_or_init(Default::default).lock().unwrap().values()
                    .all(|graph| graph.database != database(&fixture)), "Cold queries do not construct graphs.");

                let cancellation = crate::service_cancellation::CancellationLease::acquire(&gateway::new_id()).unwrap();
                let _scope = cancellation.enter();
                maintain_ann(&fixture, &cancellation.token).unwrap();
                let warm = search(&fixture, &query).unwrap();
                assert_eq!(warm["semanticBackend"], "ann");
                assert_eq!(warm["annBackground"]["state"], "ready");
                assert_eq!(GRAPHS.get().unwrap().lock().unwrap().values()
                    .filter(|graph| graph.database == database(&fixture)).count(), 8,
                    "Adaptive admission retains the complete set beyond the old four-shard default.");
                let other = search(&fixture, &json!({"query":"alpha","scopeKeys":["project:other"],"queryVector":[1.0,0.1],
                    "embeddingProfileId":"fixture","ann":{"mode":"auto","threshold":1}})).unwrap();
                assert!(other["items"].as_array().unwrap().is_empty());

                let saved = std::fs::read_dir(fixture.data_root.join("Index/ann")).unwrap()
                    .map(|entry| entry.unwrap().path()).filter(|path| path.extension().is_some_and(|extension| extension == "usearch"))
                    .map(|path| { let modified = path.metadata().unwrap().modified().unwrap(); (path, modified) }).collect::<Vec<_>>();
                assert_eq!(saved.len(), 8);
                GRAPHS.get().unwrap().lock().unwrap().retain(|_, graph| graph.database != database(&fixture));
                ANN_PARTITIONS.get().unwrap().lock().unwrap().clear();
                assert_eq!(search(&fixture, &query).unwrap()["annBackground"]["state"], "preparing");
                maintain_ann(&fixture, &cancellation.token).unwrap();
                assert_eq!(search(&fixture, &query).unwrap()["semanticBackend"], "ann");
                for (path, modified) in saved { assert_eq!(path.metadata().unwrap().modified().unwrap(), modified,
                    "Restart preparation loads the published graph rather than rebuilding it."); }

                let updated = prepare_source(&source(0, 2), Some(&[vec![0.1, 1.0]])).unwrap();
                publish_prepared(&fixture, &[updated]).unwrap();
                let changed = search(&fixture, &query).unwrap();
                assert_eq!(changed["annBackground"]["state"], "preparing");
                assert_eq!(changed["semanticBackend"], Value::Null);
                maintain_ann(&fixture, &cancellation.token).unwrap();
                assert_eq!(search(&fixture, &query).unwrap()["semanticBackend"], "ann");

                let mut constrained = query.clone();
                constrained["ann"]["adaptive"] = json!(false);
                constrained["ann"]["maxCachedShards"] = json!(1);
                // Discard only this fixture's resident plans to exercise a genuinely constrained cold request.
                // 仅清除此夹具驻留计划，检查明确自定义预算不足时不会循环完成无用构图。
                ANN_PARTITIONS.get().unwrap().lock().unwrap().clear();
                search(&fixture, &constrained).unwrap();
                let limited = maintain_ann(&fixture, &cancellation.token).unwrap_err();
                assert_eq!(limited.code(), "RETRIEVAL_ANN_RESOURCE_LIMIT");
                assert_eq!(search(&fixture, &constrained).unwrap()["annBackground"]["state"], "resource_limited");
                assert!(!ann_background_pending(&fixture));
            }))
        }).await.unwrap();
        stop_ann_background(&config);
        GRAPHS.get_or_init(Default::default).lock().unwrap().retain(|_, graph| graph.database != database(&config));
        storage::sqlite_connections::close(&config.data_root, &database(&config)).unwrap();
        service.close().await;
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(&root).unwrap();
        if let Err(panic) = outcome { std::panic::resume_unwind(panic); }
    }
}
