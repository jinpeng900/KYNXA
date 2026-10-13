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
            .unwrap_or(json!("structured-units-v1"));
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
            "chunkerVersion": "structure-lines-v1",
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
            "chunkerVersion": "structure-lines-v1",
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
    source["chunkerVersion"] = json!("structure-lines-v1");
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
        && state["embedding_input_version"] == "source-context-v1"
}
pub fn publish_semantic(
    config: &GatewayConfig,
    source: &Value,
    local: &Value,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<Value, ContractError> {
    let states = sqlite_queries::apply(
        "storage.index.sourceState",
        &arguments(config, json!({"sourceIds":[source["sourceId"]]})),
    )?;
    if let Some(state) = states["states"].as_array().unwrap().iter().find(|state| {
        source_matches(state, source)
            && state["vector_chunks"] == state["chunk_count"]
            && state["vector_chunks"].as_u64().unwrap_or(0) > 0
            && (local["embeddingDevicePolicy"] != "cpu"
                || state["embedding_profile_id"] == "builtin-multilingual")
    }) {
        return Ok(
            json!({"chunks":state["chunk_count"],"vectorChunks":state["vector_chunks"],"cachedChunks":state["vector_chunks"],"profileId":state["embedding_profile_id"],"spaceId":state["embedding_space_id"],"reused":true}),
        );
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
fn embedding_projection(source: &Value, chunk: &Value) -> Value {
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
                if relation["isArchived"] == true
                    || relation["isFolderlessWorkspace"] == true
                    || relation["folderPath"] != source["locator"]["root"]
                {
                    return Err(gateway::error(
                        "工作目录已改变，旧派生内容不再发布。",
                        "STALE_RETRIEVAL_SOURCE",
                        409,
                    ));
                }
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
                "sourceType":source["sourceType"],"bindingRevision":source["bindingRevision"].as_u64().unwrap_or(0),"active":true,"sourceRef":reference(source,None)}})}).collect::<Vec<_>>();
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

/// The catalog deletion witness is rechecked under its lock, so an undo cannot be cleared by an old cleanup.
/// 在正式目录锁内重核删除见证，避免旧清理误伤撤销恢复；仅清理聊天派生数据，Trash及共享原文保留。
pub(crate) fn retire_deleted_chat(
    config: &GatewayConfig,
    deleted: &Value,
) -> Result<Value, ContractError> {
    // Bootstrap admission can wait independently; never hold the formal catalog while waiting for it.
    // 索引初始化准入可独立等待，不能在等待期间持有正式目录锁而堵住前台聊天。
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
            // Never hold the resident-map lock while acquiring LRU; queries acquire them in the other order.
            // 获取LRU前释放驻留表锁，避免与查询的相反锁顺序形成死锁；活动查询的Arc仍持有真实资源额度。
            if let Some(lru) = LRU.get() {
                lru.lock().unwrap().retain(|key| !retired.contains(key));
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
    // The index drops before its lease; active query clones retain both until real native use finishes.
    // 图对象先析构再归还租约；活跃查询持有同一个图与额度，不能提前释放驻留账目。
    _lease: crate::resources::gateway::ResourceLease,
}
static GRAPHS: OnceLock<Mutex<HashMap<String, Arc<Graph>>>> = OnceLock::new();
static GRAPH_BUILDS: OnceLock<Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>> = OnceLock::new();
static LRU: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
fn ann(
    config: &GatewayConfig,
    descriptor: &Value,
    vector: &[f32],
    scopes: &Value,
    identity: &Value,
    epoch: &Value,
    options: &Value,
    limit: usize,
) -> Result<Vec<Value>, ContractError> {
    let descriptor_identity = json!({"version":"rust-gateway-v1","databaseIdentity":identity,"epoch":epoch,"scopeKey":descriptor["scope_key"],"descriptor":descriptor});
    let key = hash(&descriptor_identity.to_string());
    let generation = descriptor["generation"].as_u64().unwrap();
    let dimensions = descriptor["dimensions"].as_u64().unwrap() as usize;
    let count = descriptor["count"].as_u64().unwrap() as usize;
    let cache = GRAPHS.get_or_init(Default::default);
    let resident = cache.lock().unwrap().get(&key).cloned();
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
            let lease = super::gateway_embedding::reserve_graph(estimate, &token)?;
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
            }
            let graph = Arc::new(Graph {
                index,
                database: database(config),
                scope: descriptor["scope_key"].as_str().unwrap_or("").to_string(),
                identity: descriptor_identity.to_string(),
                generation,
                count,
                _lease: lease,
            });
            cache.lock().unwrap().insert(key.clone(), graph.clone());
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
    let matches = graph
        .index
        .search(vector, limit)
        .map_err(|_| gateway::error("ANN 查询失败。", "RETRIEVAL_ANN_FAILED", 500))?;
    let mut lru = LRU.get_or_init(Default::default).lock().unwrap();
    lru.retain(|entry| entry != &key);
    lru.push_back(key);
    let maximum = options["maxCachedShards"]
        .as_u64()
        .unwrap_or(4)
        .clamp(1, 16) as usize;
    while lru.len() > maximum {
        if let Some(old) = lru.pop_front() {
            cache.lock().unwrap().remove(&old);
        }
    }
    Ok(matches
        .keys
        .iter()
        .zip(matches.distances)
        .map(|(id, distance)| json!({"id":id,"distance":distance,"identity":descriptor}))
        .collect())
}
pub fn search(config: &GatewayConfig, input: &Value) -> Result<Value, ContractError> {
    let boot = initialize(config)?;
    storage::retrieval::apply(
        "storage.retrieval.scopes",
        &json!({"value":input["scopeKeys"]}),
    )?;
    let query = input["query"].as_str().unwrap_or("");
    let expression=crate::retrieval::lexical::apply("retrieval.match-expression",&json!({"text":query,"normalizedText":query.nfkc().collect::<String>(),"domain":input["retrievalIntent"]["domain"]}))
        .map_err(|_|gateway::error("查询格式无效。","INVALID_RETRIEVAL_INPUT",400))?;
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
        json!({"requestId":request_id,"query":query,"scopes":input["scopeKeys"],"expression":expression,"literalQuery":query,
        "candidateLimit":input["channelCandidates"].as_u64().unwrap_or(64).clamp(1,160),"resultLimit":input["limit"].as_u64().unwrap_or(8).clamp(1,192),
        "modelProvided":input.get("embeddingModelVersion").is_some(),"model":input["embeddingModelVersion"],
        "spaceProvided":input.get("embeddingSpaceId").is_some(),"space":input["embeddingSpaceId"],"domain":input["retrievalIntent"]["domain"],
        "symbol":input["retrievalIntent"]["symbol"],"pathFilter":input["retrievalIntent"]["path"],"directExact":options["mode"]=="exact",
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
        let mut semantic = if prepared["vectorsReady"] == true {
            Some("exact")
        } else {
            None
        };
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
                            ) {
                                Ok(rows) => {
                                    matches.extend(rows);
                                    semantic = Some("ann");
                                }
                                Err(error) => {
                                    if shard["count"].as_u64().unwrap_or(u64::MAX)
                                        <= options["exactScanLimit"].as_u64().unwrap_or(1000000)
                                    {
                                        exact.push(shard);
                                        semantic = Some("exact");
                                    } else {
                                        return Err(error);
                                    }
                                }
                            }
                        }
                    } else {
                        exact.push(descriptor.clone());
                        semantic = Some("exact");
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
        let items = finalized["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(public_chunk)
            .collect::<Result<Vec<_>, _>>()?;
        let mut response = finalized["snapshot"].clone();
        response["items"] = json!(items);
        response["strategy"] = finalized["strategy"].clone();
        response["semanticBackend"] = json!(semantic);
        response["vectorAvailable"] = json!(config.vector_extension.is_file());
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
    Ok(
        json!({"sourceId":source["sourceId"],"scopeKey":source["scopeKey"],"sourceType":row["source_type"],"title":row["title"],
        "locator":serde_json::from_str::<Value>(row["locator"].as_str().unwrap_or("{}")).unwrap_or(json!({})),"excerpt":row["text"],
        "contentHash":source["contentHash"],"sourceRevision":source["sourceRevision"],"derivationSignature":row["derivation_signature"],"chunkId":row["chunk_id"],"chunkHash":row["chunk_hash"],
        "startOffset":row["start_offset"],"endOffset":row["end_offset"],"startLine":row["start_line"],"endLine":row["end_line"],"score":row["score"],"distance":row["distance"],"structure":serde_json::from_str::<Value>(row["structure_json"].as_str().unwrap_or("null")).unwrap_or(Value::Null),"sourceRef":reference(&source,Some(&chunk))}),
    )
}
pub fn status(config: &GatewayConfig) -> Result<Value, ContractError> {
    initialize(config)?;
    sqlite_queries::apply("storage.index.status", &arguments(config, json!({})))
}
