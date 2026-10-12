//! Model-facing evidence actions reuse formal scopes, native version checks and the existing evidence ledger.
//! 面向模型的证据动作复用正式作用域、原生版本检查与既有证据账本，检索结果不是执行授权。

use super::{evidence::EvidenceLedger, gateway_index};
use crate::{
    gateway_contract::GatewayConfig,
    storage::{self, ContractError, ContractResult, gateway},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
};

#[derive(Default)]
struct TaskEvidence {
    ledger: EvidenceLedger,
    readings: HashMap<String, (Value, Vec<ReadFragment>)>,
}
struct ReadFragment {
    reference: Value,
    source_ref: String,
    text: String,
}
fn source_version(reference: &Value) -> String {
    gateway_index::hash(
        &json!([
            reference["scopeKey"],
            reference["sourceId"],
            reference["contentHash"],
            reference["sourceRevision"],
            reference["derivationSignature"]
        ])
        .to_string(),
    )
}
static TASKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<TaskEvidence>>>>> = OnceLock::new();
fn task_key(context: &Value) -> Result<String, ContractError> {
    let chat = storage::identity(&context["conversationId"])?;
    let request = storage::identity(&context["requestId"])?;
    Ok(format!(
        "{}:{}",
        chat.as_str().unwrap().to_ascii_lowercase(),
        request.as_str().unwrap().to_ascii_lowercase()
    ))
}
fn task(context: &Value) -> Result<Arc<Mutex<TaskEvidence>>, ContractError> {
    let key = task_key(context)?;
    let mut tasks = TASKS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| gateway::error("证据状态暂时不可用。", "STORAGE_IO_FAILED", 500))?;
    if tasks.len() >= 64 && !tasks.contains_key(&key) {
        return Err(gateway::error(
            "并发证据任务已达当前额度。",
            "RETRIEVAL_BUSY",
            503,
        ));
    }
    if let Some(task) = tasks.get(&key) {
        return Ok(task.clone());
    }
    let mut task = TaskEvidence::default();
    task.ledger
        .apply(
            "retrieval.evidence-open",
            &json!({"research":context["taskType"]=="research"}),
        )
        .map_err(|_| gateway::error("证据状态初始化失败。", "INVALID_RETRIEVAL_EVIDENCE", 500))?;
    let task = Arc::new(Mutex::new(task));
    tasks.insert(key, task.clone());
    Ok(task)
}
pub fn release_task(conversation: &str, request: &str) {
    if let Some(tasks) = TASKS.get() {
        if let Ok(mut tasks) = tasks.lock() {
            tasks.remove(&format!(
                "{}:{}",
                conversation.to_ascii_lowercase(),
                request.to_ascii_lowercase()
            ));
        }
    }
}
/// Removing a formal chat evicts only its derived task evidence; active holders still face final scope checks.
/// 删除正式聊天仅撤出它的派生任务证据；已持有对象的调用仍必须通过最终范围核验。
pub(crate) fn release_conversation_tasks(conversation: &str) {
    if let Some(tasks) = TASKS.get() {
        if let Ok(mut tasks) = tasks.lock() {
            let prefix = format!("{}:", conversation.to_ascii_lowercase());
            tasks.retain(|key, _| !key.starts_with(&prefix));
        }
    }
}
pub fn final_validate(config: &GatewayConfig, context: &Value) -> ContractResult {
    let key = task_key(context)?;
    let task = TASKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(&key)
        .cloned();
    let Some(task) = task else {
        return Ok(json!({"current":true,"checked":0,"invalidSources":[]}));
    };
    let references = task
        .lock()
        .unwrap()
        .readings
        .iter()
        .map(|(key, (reference, _))| (key.clone(), reference.clone()))
        .collect::<Vec<_>>();
    let scopes = authorized(config, context)?;
    let mut invalid = Vec::new();
    for (key, reference) in &references {
        if let Err(error) = read_current(config, reference, &scopes) {
            invalid.push(json!({"sourceRef":gateway_index::reference(reference,None),"sourceId":reference["sourceId"],"code":error.code(),"next":"re-read-current-source"}));
            task.lock()
                .unwrap()
                .ledger
                .apply(
                    "retrieval.evidence-invalidate-source",
                    &json!({"sourceKey":key}),
                )
                .map_err(|_| {
                    gateway::error("证据失效状态无效。", "INVALID_RETRIEVAL_EVIDENCE", 500)
                })?;
        }
    }
    Ok(
        json!({"current":invalid.is_empty(),"checked":references.len(),"invalidSources":invalid,"freshnessOnly":true,"correctnessCertified":false}),
    )
}
fn authorized(config: &GatewayConfig, context: &Value) -> Result<Value, ContractError> {
    let id = context["conversationId"]
        .as_str()
        .ok_or_else(|| gateway::error("缺少正式聊天范围。", "RETRIEVAL_SCOPE_REQUIRED", 403))?;
    Ok(gateway::context_scopes(&config.data_root, id)?["scopeKeys"].clone())
}
fn resolve_reference(
    config: &GatewayConfig,
    context: &Value,
    text: &str,
) -> Result<Value, ContractError> {
    if let Some(encoded) = text.strip_prefix("rag1:") {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| gateway::error("来源引用无效。", "INVALID_EVIDENCE_REFERENCE", 400))?;
        let reference: Value = serde_json::from_slice(&bytes)
            .map_err(|_| gateway::error("来源引用无效。", "INVALID_EVIDENCE_REFERENCE", 400))?;
        storage::retrieval::apply("storage.retrieval.reference", &json!({"value":reference}))?;
        return Ok(reference);
    }
    let parsed = storage::evidence::apply("storage.evidence.parse", &json!({"value":text}))?;
    let chat = context["conversationId"].as_str().unwrap();
    let messages = gateway::read_model_messages(&config.data_root, chat)?;
    let receipt = storage::evidence::apply(
        "storage.evidence.receipt",
        &json!({"messages":messages,"archiveId":parsed["archiveId"]}),
    )?;
    let message = &messages[receipt["message"].as_u64().unwrap() as usize];
    let reference = if receipt["activity"].is_null() {
        &message["RetrievalResultRef"]
    } else {
        &message["ToolActivities"][receipt["activity"].as_u64().unwrap() as usize]["resultRef"]
    };
    let archived = gateway::load_private_tool_result(
        &config.data_root,
        chat,
        parsed["archiveId"].as_str().unwrap(),
    )?;
    let owner = json!({"requestId":archived["requestId"],"toolCallId":archived["toolCallId"],"toolName":"knowledge.search"});
    let selection = storage::tool_results::apply(
        "storage.toolResults.reference",
        &json!({"document":archived,"reference":reference,"owner":owner,"number":parsed["referenceNumber"],"evidence":true}),
    )?;
    let item = &archived["canonical"]["structuredContent"]["items"]
        [selection["index"].as_u64().unwrap() as usize];
    let mut reference = json!({"sourceId":item["sourceId"],"scopeKey":item["scopeKey"],"contentHash":item["contentHash"],"sourceRevision":item["sourceRevision"]});
    for field in ["chunkId", "chunkHash", "derivationSignature"] {
        if let Some(value) = item.get(field) {
            reference[field] = value.clone();
        }
    }
    storage::retrieval::apply("storage.retrieval.reference", &json!({"value":reference}))?;
    Ok(reference)
}
fn read_current(config: &GatewayConfig, reference: &Value, scopes: &Value) -> ContractResult {
    storage::evidence::apply(
        "storage.evidence.authorize",
        &json!({"item":reference,"scopes":scopes}),
    )?;
    let receipt = storage::sqlite_queries::apply(
        "storage.index.readSource",
        &gateway_index::arguments(
            config,
            json!({"sourceId":reference["sourceId"],"chunkId":reference["chunkId"],
        "reference":reference,"revisionRaw":reference["sourceRevision"].to_string(),"scopes":scopes}),
        ),
    )?;
    let row = &receipt["source"];
    if row.is_null() {
        return Err(gateway::error(
            "来源不存在。",
            "RETRIEVAL_SOURCE_NOT_FOUND",
            404,
        ));
    }
    let locator: Value = serde_json::from_str(row["locator"].as_str().unwrap_or("{}"))
        .map_err(|_| gateway::error("来源定位损坏。", "CORRUPT_RETRIEVAL_SOURCE", 409))?;
    let mut text = row["text"].as_str().unwrap_or("").to_string();
    match row["source_type"].as_str().unwrap_or("") {
        "work-file" => {
            let project = row["scope_key"]
                .as_str()
                .unwrap()
                .strip_prefix("project:")
                .ok_or_else(|| gateway::error("代码范围无效。", "RETRIEVAL_SCOPE_REQUIRED", 403))?;
            let settings = super::gateway::effective(config, Some(project))?;
            if settings["projectIndexing"]["mountedFolder"] != true
                || settings["projectIndexing"]["bindingRevision"] != row["binding_revision"]
            {
                return Err(gateway::error(
                    "工作目录绑定已改变。",
                    "STALE_RETRIEVAL_SOURCE",
                    409,
                ));
            }
            let catalog = gateway::load_catalog(&config.data_root)?;
            let project = storage::catalog::apply(
                "storage.catalog.project",
                &json!({"current":catalog,"id":project}),
            )?;
            let root = std::path::Path::new(project["folderPath"].as_str().unwrap_or(""));
            let path = std::path::Path::new(locator["path"].as_str().unwrap_or(""));
            if !path.starts_with(root) || root.as_os_str().is_empty() {
                return Err(gateway::error(
                    "来源不在当前目录。",
                    "RETRIEVAL_SOURCE_NOT_FOUND",
                    404,
                ));
            }
            let current = if locator["fileWindow"].is_object() {
                crate::tools::service_source_reading::execute_io("readTextWindow",&json!({"path":path,"window":locator["fileWindow"],"maximumSourceBytes":settings["local"]["indexing"]["maximumSourceBytes"]}),&None)
                .map_err(|_|gateway::error("文件窗口已改变。","STALE_RETRIEVAL_SOURCE",409))?
            } else {
                super::gateway_documents::read_with_config(
                    config,
                    path,
                    &settings["local"]["indexing"],
                    &crate::service_cancellation::current_token()
                        .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
                )?
            };
            let current = if let Some(extraction) = locator.get("extraction") {
                current["documentWindows"]
                    .as_array()
                    .and_then(|windows| {
                        windows.iter().find(|window| {
                            window["extraction"]["pageWindow"] == extraction["pageWindow"]
                        })
                    })
                    .cloned()
                    .unwrap_or(current)
            } else {
                current
            };
            text = current["text"].as_str().unwrap_or("").to_string();
            if gateway_index::hash(&text) != row["content_hash"] {
                return Err(gateway::error(
                    "文件已改变，请重新检索。",
                    "STALE_RETRIEVAL_SOURCE",
                    409,
                ));
            }
        }
        "knowledge" => {
            let (registry, _) = gateway::read_json(
                &config.data_root,
                &config.data_root.join("Knowledge/catalog.json"),
                32 * 1024 * 1024,
            )?;
            if registry.as_ref().is_none_or(|registry| {
                !registry["sources"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|entry| {
                        entry["id"] == reference["sourceId"]
                            && entry["status"] == "ready"
                            && entry["revision"] == reference["sourceRevision"]
                    })
            }) {
                return Err(gateway::error(
                    "资料已撤销或改变。",
                    "STALE_RETRIEVAL_SOURCE",
                    409,
                ));
            }
            let snapshot = storage::library::apply(
                "storage.library.readSnapshot",
                &json!({"root":config.data_root,"id":reference["sourceId"]}),
            )?;
            text = snapshot["raw"].as_str().unwrap_or("").to_string();
            if gateway_index::hash(&text) != row["content_hash"] {
                return Err(gateway::error(
                    "资料快照已改变。",
                    "STALE_RETRIEVAL_SOURCE",
                    409,
                ));
            }
        }
        "memory" => {
            let chat = scopes
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .find_map(|scope| scope.strip_prefix("chat:"))
                .unwrap();
            let current = storage::gateway_memory::context(&config.data_root, chat)?;
            if !current["entries"].as_array().unwrap().iter().any(|entry| {
                entry["id"] == locator["memoryId"]
                    && entry["revision"] == reference["sourceRevision"]
                    && entry["status"] == "confirmed"
                    && gateway_index::hash(entry["content"].as_str().unwrap_or(""))
                        == row["content_hash"]
            }) {
                return Err(gateway::error(
                    "记忆已改变或撤销。",
                    "STALE_RETRIEVAL_SOURCE",
                    409,
                ));
            }
        }
        "message" => {
            let chat = locator["conversationId"].as_str().unwrap_or("");
            let current = gateway::read_model_messages(&config.data_root, chat)?;
            if !current.iter().any(|message| {
                message["Id"] == locator["messageId"]
                    && message["Status"] == "completed"
                    && gateway_index::hash(message["Content"].as_str().unwrap_or(""))
                        == row["content_hash"]
            }) {
                return Err(gateway::error(
                    "聊天来源已改变。",
                    "STALE_RETRIEVAL_SOURCE",
                    409,
                ));
            }
        }
        _ => {}
    }
    Ok(json!({"text":text,"row":row,"chunk":receipt["chunk"],"locator":locator}))
}
fn register_read(
    config: &GatewayConfig,
    context: &Value,
    reference: &Value,
    source_ref: &str,
    text: &str,
    start: usize,
    end: usize,
) -> ContractResult {
    let task = task(context)?;
    let mut task = task.lock().unwrap();
    let key =
        gateway_index::hash(&json!([reference["scopeKey"], reference["sourceId"]]).to_string());
    let version = source_version(reference);
    let fragments = task
        .readings
        .entry(key.clone())
        .or_insert_with(|| (reference.clone(), Vec::new()));
    if source_version(&fragments.0) != version {
        fragments.0 = reference.clone();
        fragments.1.clear();
    }
    fragments.1.push(ReadFragment {
        reference: reference.clone(),
        source_ref: source_ref.to_string(),
        text: text.to_string(),
    });
    while fragments.1.len() > 16
        || fragments
            .1
            .iter()
            .map(|fragment| fragment.text.len())
            .sum::<usize>()
            > 256 * 1024
    {
        fragments.1.remove(0);
    }
    let projected=fragments.1.iter().map(|fragment|json!({"sourceRef":fragment.source_ref,"length":fragment.text.encode_utf16().count()})).collect::<Vec<_>>();
    let receipt=task.ledger.apply("retrieval.evidence-read",&json!({"item":{"sourceKey":key,"version":version,"sourceRef":source_ref,"sourceId":reference["sourceId"],"scopeKey":reference["scopeKey"]},
        "textLength":text.encode_utf16().count(),"observation":gateway_index::hash(&json!([reference,start,end,text]).to_string()),"range":{"startOffset":start,"endOffset":end},
        "fragments":projected,"gap":context["gap"]}))
        .map_err(|_|gateway::error("证据回执无效。","INVALID_RETRIEVAL_EVIDENCE",400))?;
    for key in receipt["result"]["dropReadKeys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        task.readings.remove(key);
    }
    let _ = config;
    Ok(receipt["result"]["decision"].clone())
}
pub fn handle_tool(
    name: &str,
    input: &Value,
    context: &Value,
    config: &GatewayConfig,
) -> Option<ContractResult> {
    if name.starts_with("conversation.history.") {
        return Some(super::gateway_history::handle(config, name, input, context));
    }
    if name.starts_with("memory.") {
        return storage::gateway_memory::handle_tool(name, input, context, &config.data_root);
    }
    if ![
        "knowledge.search",
        "knowledge.read",
        "knowledge.relations",
        "knowledge.assess",
        "knowledge.experience",
    ]
    .contains(&name)
    {
        return None;
    }
    Some((|| {
        if name == "knowledge.search"
            && input.get("domain").is_some_and(|domain| {
                !matches!(domain.as_str(), Some("code" | "knowledge" | "mixed"))
            })
        {
            return Err(gateway::error(
                "检索领域无效，允许 knowledge、code 或 mixed。",
                "INVALID_RETRIEVAL_INPUT",
                400,
            ));
        }
        let scopes = authorized(config, context)?;
        gateway_index::initialize(config)?;
        match name {
            "knowledge.search" => {
                synchronize_conversation(config, context)?;
                let budget=crate::retrieval::budget::apply(&json!({"taskType":input["taskType"],"limit":input["limit"],"maximumTokens":input["maximumTokens"]}))
                    .map_err(|_|gateway::error("检索额度无效。","INVALID_RETRIEVAL_INPUT",400))?;
                let mut query = input.clone();
                query["scopeKeys"] = scopes.clone();
                query["channelCandidates"] = budget["channelCandidates"].clone();
                query["limit"] = budget["fusedCandidates"].clone();
                query["retrievalIntent"] = json!({"domain":input["domain"].as_str().filter(|domain|*domain!="mixed"),"symbol":input["symbol"],"path":input["path"]});
                let settings = super::gateway::effective(
                    config,
                    gateway::context_scopes(
                        &config.data_root,
                        context["conversationId"].as_str().unwrap(),
                    )?["relationship"]["projectId"]
                        .as_str(),
                )?;
                query["ann"] = settings["local"]["ann"].clone();
                if settings["local"]["semantic"] != "off" {
                    let text = input["query"].as_str().unwrap_or("").to_string();
                    match super::gateway_embedding::embed(
                        config,
                        &[text],
                        crate::models::gateway_inference::EmbedKind::Query,
                        &settings["local"],
                        crate::service_cancellation::current_token()
                            .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
                        context["automaticEvidence"] != true,
                    ) {
                        Ok(Some(batch)) => {
                            query["queryVector"] = json!(batch.vectors[0]);
                            query["embeddingProfileId"] = json!(batch.profile_id);
                            query["embeddingModelVersion"] = json!(batch.model_version);
                            query["embeddingSpaceId"] = json!(batch.space_id);
                        }
                        Ok(None) => {}
                        Err(error) => {
                            query["semanticDiagnostic"] =
                                json!({"code":error.code(),"message":error.message()});
                        }
                    }
                }
                let mut result = gateway_index::search(config, &query)?;
                if !settings["local"]["rerankProfileId"].is_null() {
                    let count = budget["rerankCandidates"]
                        .as_u64()
                        .unwrap_or(20)
                        .min(settings["local"]["rerankCandidates"].as_u64().unwrap_or(60))
                        as usize;
                    let texts = result["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .take(count)
                        .map(|item| item["excerpt"].as_str().unwrap_or("").to_string())
                        .collect::<Vec<_>>();
                    if !texts.is_empty() {
                        match super::gateway_embedding::rerank(
                            config,
                            input["query"].as_str().unwrap_or(""),
                            &texts,
                            &settings["local"],
                            crate::service_cancellation::current_token()
                                .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
                        ) {
                            Ok(ranked) => {
                                let items = result["items"].as_array_mut().unwrap();
                                for (index, score) in ranked.scores.iter().enumerate() {
                                    items[index]["rerankScore"] = json!(score);
                                }
                                items[..texts.len()].sort_by(|left, right| {
                                    right["rerankScore"]
                                        .as_f64()
                                        .unwrap_or(0.0)
                                        .total_cmp(&left["rerankScore"].as_f64().unwrap_or(0.0))
                                });
                                result["reranking"] = json!({"state":"applied","candidateCount":texts.len(),"truncatedInputsCount":ranked.truncated_inputs_count,"inferenceBackend":ranked.inference_status});
                            }
                            Err(error) => {
                                storage::sqlite_store::cancelled()?;
                                result["reranking"] =
                                    json!({"state":"unavailable","errorCode":error.code()});
                            }
                        }
                    }
                }
                let selected = select_evidence(&result["items"], input, &budget)?;
                result["items"] = selected["items"].clone();
                result["selection"] = selected["selection"].clone();
                result["evidenceAssessment"] = selected["evidenceAssessment"].clone();
                let mut current_sources = HashMap::new();
                for item in result["items"].as_array_mut().unwrap() {
                    let reference =
                        resolve_reference(config, context, item["sourceRef"].as_str().unwrap())?;
                    // Complete indexed fragments can be used directly after a current-source read; search headings alone cannot.
                    // 经过当前来源复核的完整索引片段可直接使用；仅命中标题不能冒充已读正文。
                    let version = source_version(&reference);
                    if !current_sources.contains_key(&version) {
                        let mut source = reference.clone();
                        source.as_object_mut().unwrap().remove("chunkId");
                        source.as_object_mut().unwrap().remove("chunkHash");
                        current_sources
                            .insert(version.clone(), read_current(config, &source, &scopes).ok());
                    }
                    if let Some(current) = current_sources.get(&version).and_then(Option::as_ref) {
                        let units: Vec<u16> =
                            current["text"].as_str().unwrap().encode_utf16().collect();
                        let start = item["startOffset"].as_u64().unwrap() as usize;
                        let end = item["endOffset"].as_u64().unwrap() as usize;
                        if end <= units.len() {
                            item["decision"] = register_read(
                                config,
                                context,
                                &reference,
                                item["sourceRef"].as_str().unwrap(),
                                &String::from_utf16_lossy(&units[start..end]),
                                start,
                                end,
                            )?;
                        }
                    }
                }
                let before = result["items"].as_array().unwrap().len();
                result["items"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|item| item.get("decision").is_some());
                let excluded = before - result["items"].as_array().unwrap().len();
                if excluded > 0 {
                    result["freshness"] =
                        json!({"excludedChangedSources":excluded,"requiresNewEvidence":true});
                }
                result["budgetAudit"] = budget["audit"].clone();
                result["budgetAudit"]["actual"] = json!({"limit":result["items"].as_array().unwrap().len(),"usedTokens":result["selection"]["usedTokens"]});
                result["semanticDiagnostic"] = query["semanticDiagnostic"].clone();
                Ok(result)
            }
            "knowledge.read" => {
                let reference =
                    resolve_reference(config, context, input["sourceRef"].as_str().unwrap_or(""))?;
                let source = read_current(config, &reference, &scopes)?;
                let text = source["text"].as_str().unwrap();
                let units: Vec<u16> = text.encode_utf16().collect();
                let maximum = input["limit"].as_u64().unwrap_or(4000).clamp(1, 16000) as usize;
                let anchor = input["anchorOffset"]
                    .as_u64()
                    .or_else(|| input["offset"].as_u64())
                    .or_else(|| source["chunk"]["start_offset"].as_u64())
                    .unwrap_or(0) as usize;
                if anchor > units.len() {
                    return Err(gateway::error(
                        "读取位置超出范围。",
                        "INVALID_RETRIEVAL_OFFSET",
                        400,
                    ));
                }
                let mode = input["mode"].as_str().unwrap_or("page");
                let mut start = if mode == "page" {
                    anchor
                } else {
                    anchor
                        .saturating_sub(input["beforeCharacters"].as_u64().unwrap_or(384) as usize)
                };
                let mut end = (start + maximum).min(units.len());
                if ["unit", "section"].contains(&mode) {
                    let structure: Value = serde_json::from_str(
                        source["row"]["structure_json"].as_str().unwrap_or("{}"),
                    )
                    .unwrap_or(json!({}));
                    if let Some(unit) =
                        structure["units"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|unit| {
                                unit["startOffset"].as_u64().unwrap_or(0) <= anchor as u64
                                    && unit["endOffset"].as_u64().unwrap_or(0) > anchor as u64
                            })
                    {
                        start = unit["startOffset"].as_u64().unwrap() as usize;
                        end = (start + maximum).min(unit["endOffset"].as_u64().unwrap() as usize);
                    }
                }
                if start > 0
                    && start < units.len()
                    && (0xdc00..=0xdfff).contains(&units[start])
                    && (0xd800..=0xdbff).contains(&units[start - 1])
                {
                    start -= 1;
                }
                if end < units.len() && end > start && (0xd800..=0xdbff).contains(&units[end - 1]) {
                    end -= 1;
                }
                if end == start && end < units.len() {
                    end = (end + 2).min(units.len());
                }
                let body = String::from_utf16_lossy(&units[start..end]);
                let decision = register_read(
                    config,
                    context,
                    &reference,
                    input["sourceRef"].as_str().unwrap(),
                    &body,
                    start,
                    end,
                )?;
                Ok(
                    json!({"sourceRef":input["sourceRef"],"text":body,"offset":start,"nextOffset":end,"totalCharacters":units.len(),"truncated":end<units.len(),"offsetUnit":"utf16-code-units","locator":source["locator"],"decision":decision}),
                )
            }
            "knowledge.relations" => {
                let mut arguments = input.clone();
                arguments["scopes"] = scopes;
                if let Some(reference) = input["sourceRef"].as_str() {
                    let reference = resolve_reference(config, context, reference)?;
                    read_current(config, &reference, &arguments["scopes"])?;
                    arguments["revisionRaw"] = json!(reference["sourceRevision"].to_string());
                    arguments["sourceId"] = reference["sourceId"].clone();
                    arguments["reference"] = reference;
                }
                storage::sqlite_queries::apply(
                    "storage.index.relations",
                    &gateway_index::arguments(config, arguments),
                )
            }
            "knowledge.assess" => {
                final_validate(config, context)?;
                let task = task(context)?;
                let mut task = task.lock().unwrap();
                let mut assessment = input.clone();
                for claim in assessment["claims"].as_array_mut().ok_or_else(|| {
                    gateway::error("证据评估缺少结论。", "INVALID_RETRIEVAL_ASSESSMENT", 400)
                })? {
                    for citation in claim["support"].as_array_mut().ok_or_else(|| {
                        gateway::error("结论引用格式无效。", "INVALID_RETRIEVAL_ASSESSMENT", 400)
                    })? {
                        let reference = resolve_reference(
                            config,
                            context,
                            citation["sourceRef"].as_str().unwrap_or(""),
                        )?;
                        let current = read_current(config, &reference, &scopes).is_ok();
                        citation["quotePresent"] = json!(
                            current
                                && task
                                    .readings
                                    .values()
                                    .any(|(read, fragments)| source_version(read)
                                        == source_version(&reference)
                                        && fragments.iter().any(|fragment| fragment.reference
                                            == reference
                                            && fragment.text.contains(
                                                citation["quote"].as_str().unwrap_or("")
                                            )))
                        );
                    }
                }
                task.ledger
                    .apply(
                        "retrieval.evidence-assess",
                        &json!({"assessment":assessment}),
                    )
                    .map(|value| value["result"].clone())
                    .map_err(|_| {
                        gateway::error("证据评估无效。", "INVALID_RETRIEVAL_ASSESSMENT", 400)
                    })
            }
            _ => {
                let path = config.data_root.join("Retrieval/experiences").join(format!(
                    "{}.json",
                    context["conversationId"].as_str().unwrap()
                ));
                let (document, _) = gateway::read_json(&config.data_root, &path, 8 * 1024 * 1024)?;
                Ok(
                    json!({"entries":document.unwrap_or(json!({"entries":[]}))["entries"],"freshnessRequired":true}),
                )
            }
        }
    })())
}
fn synchronize_conversation(config: &GatewayConfig, context: &Value) -> ContractResult {
    let chat = context["conversationId"].as_str().unwrap();
    let memory = storage::gateway_memory::context(&config.data_root, chat)?;
    let messages = gateway::read_model_messages(&config.data_root, chat)?;
    let scope = format!("chat:{}", chat.to_ascii_lowercase());
    let mut sources = Vec::new();
    for message in messages.iter().filter(|message| {
        message["Status"] == "completed"
            && message["Content"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
            && ["user", "assistant"].contains(&message["Role"].as_str().unwrap_or(""))
    }) {
        let text = message["Content"].as_str().unwrap();
        sources.push(json!({"sourceId":gateway_index::hash(&json!(["message",chat,message["Id"]]).to_string()),"scopeKey":scope,"sourceType":"message","text":text,"contentHash":gateway_index::hash(text),"sourceRevision":gateway_index::hash(text),"title":message["Role"],"locator":{"conversationId":chat,"messageId":message["Id"],"role":message["Role"]}}));
    }
    for entry in memory["entries"].as_array().unwrap() {
        let scope = if entry["scope"] == "user" {
            "user".to_string()
        } else {
            format!(
                "{}:{}",
                entry["scope"].as_str().unwrap(),
                entry["scopeId"].as_str().unwrap()
            )
        };
        sources.push(json!({"sourceId":gateway_index::hash(&json!(["memory",scope,entry["id"]]).to_string()),"scopeKey":scope,"sourceType":"memory","text":entry["content"],"sourceRevision":entry["revision"],"title":entry["kind"],"locator":{"memoryId":entry["id"]}}));
    }
    for batch in sources.chunks(100) {
        gateway_index::publish(config, batch)?;
    }
    Ok(Value::Null)
}

fn select_evidence(items: &Value, input: &Value, budget: &Value) -> ContractResult {
    use unicode_normalization::UnicodeNormalization;
    let items = items
        .as_array()
        .ok_or_else(|| gateway::error("检索候选格式无效。", "INVALID_RETRIEVAL_CANDIDATES", 500))?;
    let archive = gateway::new_id();
    let candidates=items.iter().enumerate().map(|(index,item)|->ContractResult{
        let path=item["locator"]["relativePath"].as_str().or_else(||item["locator"]["path"].as_str()).or_else(||item["title"].as_str()).unwrap_or("");
        let normalized=|text:&str|text.nfkc().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let short=storage::evidence::apply("storage.evidence.reference",&json!({"archiveId":archive,"number":index.min(59)+1}))?;
        let record=crate::models::gateway_protocol::retrieval_model_view(&json!({"reference":1,"sourceRef":short,"title":item["title"],"scope":item["scopeKey"],"locator":item["locator"],"structure":item["structure"],"excerpt":item["excerpt"]}))
            .map_err(|failure|gateway::error(&failure.message,"INVALID_RETRIEVAL_EVIDENCE",failure.status_code))?;
        let mut candidate=item.clone();candidate["normalizedExcerpt"]=json!(normalized(item["excerpt"].as_str().unwrap_or("")));candidate["normalizedPath"]=json!(normalized(path).replace('\\',"/"));candidate["rolePath"]=json!(path.to_lowercase());
        candidate["metadataText"]=json!(format!("{} {} {} {}",item["title"].as_str().unwrap_or(""),path,item["structure"]["symbolName"].as_str().unwrap_or(""),item["structure"]["qualifiedName"].as_str().unwrap_or("")));
        candidate["cost"]=json!(crate::models::protocol::estimate_tokens(&record.to_string())+1);
        let mut navigation=record;navigation["excerpt"]=json!("");navigation.as_object_mut().unwrap().remove("structure");navigation["navigationOnly"]=json!(true);navigation["next"]=json!("knowledge.read");navigation["evidenceSupport"]=json!("not-read");
        candidate["navigationCost"]=json!(crate::models::protocol::estimate_tokens(&navigation.to_string())+1);Ok(candidate)
    }).collect::<Result<Vec<_>,_>>()?;
    let selected=crate::retrieval::candidates::apply("retrieval.candidate-select",&json!({"items":candidates,"query":input["query"],"normalizedQuery":input["query"].as_str().unwrap_or("").nfkc().collect::<String>(),"limit":budget["limit"],"maximumTokens":budget["maximumTokens"],"intent":{"domain":input["domain"],"path":input["path"],"symbol":input["symbol"]},"existing":[]}))
        .map_err(|_|gateway::error("候选选择额度无效。","INVALID_RETRIEVAL_CANDIDATES",400))?;
    let restored = selected["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|selection| {
            let mut item = items[selection["index"].as_u64().unwrap() as usize].clone();
            if selection["navigation"] == true {
                item["excerpt"] = json!("");
                item.as_object_mut().unwrap().remove("structure");
                item["navigationOnly"] = json!(true);
            }
            item
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"items":restored,"selection":selected["selection"],"evidenceAssessment":selected["evidenceAssessment"]}),
    )
}

/// Short references are assigned only after selection and are backed by the saved, scoped archive.
/// 仅在证据选入后分配短引用；引用依托已保存并有聊天回执的作用域归档。
pub fn project_evidence_result(value: &Value, archive_id: &str) -> ContractResult {
    let structured = value.get("structuredContent").is_some();
    let payload = if structured {
        &value["structuredContent"]
    } else {
        value
    };
    if value["isError"] == true && !payload["items"].is_array() {
        return crate::models::gateway_protocol::retrieval_model_view(value).map_err(|error| {
            gateway::error(
                &error.message,
                "INVALID_RETRIEVAL_EVIDENCE",
                error.status_code,
            )
        });
    }
    let plan = storage::evidence::apply(
        "storage.evidence.payload",
        &json!({"value":payload,"archiveId":archive_id}),
    )?;
    let mut projected = payload.clone();
    let fields = plan["remove"].as_array().unwrap();
    let mut replacements = Vec::new();
    for field in fields.iter().filter_map(Value::as_str) {
        projected.as_object_mut().unwrap().remove(field);
    }
    for (index, item) in projected["items"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let text = item["sourceRef"]
            .as_str()
            .ok_or_else(|| gateway::error("来源引用无效。", "INVALID_EVIDENCE_REFERENCE", 400))?;
        let descriptor = resolve_canonical(text)?;
        let equal = [
            "sourceId",
            "scopeKey",
            "sourceRevision",
            "contentHash",
            "chunkId",
            "chunkHash",
            "derivationSignature",
        ]
        .iter()
        .filter(|field| item.get(**field).is_some())
        .map(|field| Value::Bool(item[*field] == descriptor[*field]))
        .collect::<Vec<_>>();
        storage::evidence::apply(
            "storage.evidence.validate",
            &json!({"item":item,"descriptor":descriptor,"equalFields":equal}),
        )?;
        replacements.push((
            text.to_string(),
            plan["references"][index].as_str().unwrap().to_string(),
        ));
        for field in fields.iter().filter_map(Value::as_str) {
            item.as_object_mut().unwrap().remove(field);
        }
        item["reference"] = json!(index + 1);
        item["sourceRef"] = plan["references"][index].clone();
    }
    let mut output = value.clone();
    if structured {
        output["structuredContent"] = projected;
    } else {
        output = projected;
    }
    if structured {
        for block in output["content"].as_array_mut().into_iter().flatten() {
            if block["type"] == "text" {
                if let Some(text) = block["text"].as_str() {
                    let mut text = text.to_string();
                    for (old, new) in &replacements {
                        text = text.replace(old, new);
                    }
                    block["text"] = json!(text);
                }
            }
        }
    }
    crate::models::gateway_protocol::retrieval_model_view(&output).map_err(|error| {
        gateway::error(
            &error.message,
            "INVALID_RETRIEVAL_EVIDENCE",
            error.status_code,
        )
    })
}
fn resolve_canonical(text: &str) -> ContractResult {
    let encoded = text
        .strip_prefix("rag1:")
        .ok_or_else(|| gateway::error("来源引用无效。", "INVALID_EVIDENCE_REFERENCE", 400))?;
    let value: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| gateway::error("来源引用无效。", "INVALID_EVIDENCE_REFERENCE", 400))?,
    )
    .map_err(|_| gateway::error("来源引用无效。", "INVALID_EVIDENCE_REFERENCE", 400))?;
    storage::retrieval::apply("storage.retrieval.reference", &json!({"value":value}))?;
    Ok(value)
}

#[cfg(test)]
mod gateway_tests {
    use super::*;
    #[test]
    fn research_selection_accepts_more_than_sixty_internal_candidates() {
        let items=(0..96).map(|index|{let text=format!("Research evidence about system performance, original result {index}.");let hash=gateway_index::hash(&text);let source=json!({"sourceId":format!("source-{index}"),"scopeKey":"user","contentHash":hash,"sourceRevision":1});let chunk=json!({"chunkId":format!("chunk-{index}"),"chunkHash":hash});json!({"sourceId":source["sourceId"],"scopeKey":"user","contentHash":hash,"sourceRevision":1,"sourceRef":gateway_index::reference(&source,Some(&chunk)),"chunkId":chunk["chunkId"],"chunkHash":hash,"title":format!("Document {index}"),"excerpt":text,"locator":{"relativePath":format!("doc-{index}.md")}})}).collect::<Vec<_>>();
        let budget =
            crate::retrieval::budget::apply(&json!({"taskType":"research","maximumTokens":32768}))
                .unwrap();
        let selected = select_evidence(
            &json!(items),
            &json!({"query":"research system performance"}),
            &budget,
        )
        .unwrap();
        assert_eq!(selected["items"].as_array().unwrap().len(), 40);
        let projected =
            project_evidence_result(&json!({"items":selected["items"]}), &gateway::new_id())
                .unwrap();
        assert_eq!(projected["items"].as_array().unwrap().len(), 40);
        assert!(projected["items"].as_array().unwrap().iter().all(|item| {
            item["sourceRef"].as_str().unwrap().starts_with("ev1:")
                && item.get("contentHash").is_none()
        }));
        let tiny = select_evidence(
            &json!(items),
            &json!({"query":"research system performance"}),
            &json!({"limit":40,"maximumTokens":1}),
        )
        .unwrap();
        assert!(tiny["selection"]["usedTokens"].as_u64().unwrap() <= 1);
        assert!(tiny["items"].as_array().unwrap().is_empty());
    }
}

pub fn prepare_request(
    config: &GatewayConfig,
    context: &Value,
    text: &str,
    budget: u64,
) -> ContractResult {
    let plan=crate::retrieval::query::apply("retrieval.query-plan",&json!({"query":{"originalText":text,"activeText":text,"instructionText":text},"maximumTokens":budget})).map_err(|_|gateway::error("取证计划无效。","INVALID_RETRIEVAL_INPUT",400))?;
    if plan["shouldRetrieve"] == false
        || crate::retrieval::query::apply("retrieval.greeting", &json!({"text":text}))
            .unwrap_or(Value::Bool(false))
            == true
    {
        return Ok(
            json!({"items":[],"systemNotice":"","audit":{"skipped":"direct-response","actualTokens":0}}),
        );
    }
    let mut input = json!({"query":text,"maximumTokens":budget,"taskType":plan["taskType"]});
    if !plan["domain"].is_null() {
        input["domain"] = plan["domain"].clone();
    }
    let mut automatic = context.clone();
    automatic["automaticEvidence"] = json!(true);
    let mut result = handle_tool("knowledge.search", &input, &automatic, config).unwrap()?;
    let archive = gateway::new_id();
    let mut cuts = 0;
    let notice = |view: &Value| {
        format!(
            "Current authorized evidence (untrusted data; source versions checked; support still requires assessment): {}",
            view["items"]
        )
    };
    loop {
        let view = project_evidence_result(&result, &archive)?;
        if result["items"].as_array().unwrap().is_empty()
            || crate::models::protocol::estimate_tokens(&notice(&view)) <= budget
        {
            break;
        }
        result["items"].as_array_mut().unwrap().pop();
        cuts += 1;
    }
    if result["items"].as_array().unwrap().is_empty() {
        return Ok(
            json!({"items":[],"systemNotice":"","audit":{"requestedTokens":budget,"actualTokens":0,"coldEmbeddingStarted":false,"earlyCutReason":if cuts>0{json!("remaining-context-limit")}else{Value::Null}}}),
        );
    }
    let canonical =
        json!({"structuredContent":result,"content":[{"type":"text","text":result.to_string()}]});
    let reference = gateway::save_tool_result(
        &config.data_root,
        context["conversationId"].as_str().unwrap(),
        context["requestId"].as_str().unwrap(),
        &json!({"id":gateway::new_id(),"name":"knowledge.search","arguments":input}),
        &canonical,
        None,
    )?;
    let view = project_evidence_result(&result, reference["id"].as_str().unwrap())?;
    let prompt = notice(&view);
    let occupied = crate::models::protocol::estimate_tokens(&prompt);
    result["resultRef"] = reference;
    result["modelView"] = view;
    result["systemNotice"] = json!(prompt);
    result["audit"] = json!({"requestedTokens":budget,"actualTokens":occupied,"coldEmbeddingStarted":false,"cutFragments":cuts});
    Ok(result)
}
