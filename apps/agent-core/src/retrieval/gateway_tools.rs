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
    refinements_restored: bool,
    readings: HashMap<String, (Value, Vec<ReadFragment>)>,
}
#[derive(Clone)]
struct ReadFragment {
    reference: Value,
    source_ref: String,
    text: String,
    start_offset: usize,
    end_offset: usize,
    locator: Value,
    binding_revision: Value,
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
/// Retire only revoked memory projections; formal archives and unrelated evidence remain intact.
/// 仅撤出已失效记忆的派生投影，保留正式归档、成功回执及其他来源的版本核验。
pub(crate) fn retire_memory_sources(context: &Value, invalid_sources: &Value) -> ContractResult {
    let identities = invalid_sources.as_array().into_iter().flatten()
        .filter_map(|source| source["memoryId"].as_str()).collect::<std::collections::HashSet<_>>();
    if identities.is_empty() { return Ok(json!({"retired":0})); }
    let key = task_key(context)?;
    let evidence = TASKS.get_or_init(Default::default).lock()
        .map_err(|_|gateway::error("证据状态暂时不可用。","STORAGE_IO_FAILED",500))?
        .get(&key).cloned();
    let Some(evidence) = evidence else { return Ok(json!({"retired":0})); };
    let mut evidence = evidence.lock()
        .map_err(|_|gateway::error("证据状态暂时不可用。","STORAGE_IO_FAILED",500))?;
    let retired = evidence.readings.iter().filter_map(|(key,(reference,fragments))| {
        fragments.iter().any(|fragment| fragment.locator["memoryId"].as_str().is_some_and(|id| {
            identities.contains(id) && reference["sourceId"].as_str() == Some(
                gateway_index::hash(&json!(["memory",reference["scopeKey"],id]).to_string()).as_str())
        })).then(||key.clone())
    }).collect::<Vec<_>>();
    for key in &retired {
        evidence.ledger.apply("retrieval.evidence-invalidate-source",&json!({"sourceKey":key}))
            .map_err(|_|gateway::error("记忆证据失效状态无效。","INVALID_RETRIEVAL_EVIDENCE",500))?;
        evidence.readings.remove(key);
    }
    Ok(json!({"retired":retired.len()}))
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
        .map(|(key, (reference, fragments))| (key.clone(), reference.clone(), fragments.clone()))
        .collect::<Vec<_>>();
    let scopes = authorized(config, context)?;
    let mut invalid = Vec::new();
    let mut original_readings_revalidated = 0;
    for (key, reference, fragments) in &references {
        if let Err(error) = read_current(config, context, reference, &scopes) {
            // Semantic token fitting can replace chunk topology without changing the original text.
            // 语义 token 适配可替换分块而不改变原文；仅已读且逐范围回验相同的原文可保留，旧结构引用仍失效。
            let recovered = if error.code() == "STALE_RETRIEVAL_SOURCE" && !fragments.is_empty() {
                read_original_current(config, context, reference, &scopes).is_ok_and(|(current, _)| {
                    fragments.iter().all(|fragment| {
                        validate_saved_read_window(
                            &current,
                            &fragment.reference,
                            &json!({"text":fragment.text,"offset":fragment.start_offset,
                                "nextOffset":fragment.end_offset,"locator":fragment.locator,
                                "bindingRevision":fragment.binding_revision,
                                "offsetUnit":"utf16-code-units"}),
                            false,
                        )
                        .is_ok()
                    })
                })
            } else {
                false
            };
            if recovered {
                original_readings_revalidated += 1;
                continue;
            }
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
        json!({"current":invalid.is_empty(),"checked":references.len(),"invalidSources":invalid,
            "originalReadingsRevalidated":original_readings_revalidated,
            "derivedReferencesRevalidated":false,"freshnessOnly":true,"correctnessCertified":false}),
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
/// Task snapshots bound workspace evidence even when the same project gains new mounts later.
/// 任务快照限定工作目录证据范围；同一项目后加挂载不会扩大进行中或恢复任务的读取权限。
fn authorize_locator(context: &Value, source_type: &str, locator: &Value) -> Result<(), ContractError> {
    if source_type != "work-file" { return Ok(()); }
    let snapshot = &context["bindingSnapshot"];
    let mounts = context["workspaceMounts"].as_array().or_else(|| snapshot["mounts"].as_array());
    let same_path = |a: &Value, b: &Value| a.as_str().zip(b.as_str()).is_some_and(|(a,b)| {
        if cfg!(windows) { a.replace('/', "\\").trim_end_matches('\\').eq_ignore_ascii_case(b.replace('/', "\\").trim_end_matches('\\')) }
        else { a.trim_end_matches('/') == b.trim_end_matches('/') }
    });
    let allowed = if let Some(mounts) = mounts {
        mounts.iter().any(|mount| same_path(&mount["path"], &locator["root"])
            && (locator["mountId"].is_null() || (mount["id"] == locator["mountId"]
                && mount["bindingRevision"] == locator["mountBindingRevision"])))
    } else if snapshot.is_object() {
        same_path(&snapshot["folderPath"], &locator["root"])
    } else { return Ok(()); };
    if allowed { Ok(()) } else { Err(gateway::error("来源不在本次任务的工作目录范围内。", "RETRIEVAL_SCOPE_MISMATCH", 403)) }
}

fn restrict_candidates(context: &Value, result: &mut Value) {
    if let Some(items) = result["items"].as_array_mut() {
        let before = items.len();
        items.retain(|item| {
            let locator = item["locator"].as_str().map(|raw| serde_json::from_str::<Value>(raw).unwrap_or(Value::Null));
            authorize_locator(context, item["sourceType"].as_str().unwrap_or(""), locator.as_ref().unwrap_or(&item["locator"])).is_ok()
        });
        if items.len() != before { result["scopeFiltered"] = json!(true); }
    }
}

fn read_current(config: &GatewayConfig, context: &Value, reference: &Value, scopes: &Value) -> ContractResult {
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
    authorize_locator(context, row["source_type"].as_str().unwrap_or(""), &locator)?;
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
            let mounted = gateway_index::mounted_root(&project, &locator)?;
            let root = std::path::Path::new(&mounted);
            let path = std::path::Path::new(locator["path"].as_str().unwrap_or(""));
            if !path.starts_with(root) || root.as_os_str().is_empty() {
                return Err(gateway::error(
                    "来源不在当前目录。",
                    "RETRIEVAL_SOURCE_NOT_FOUND",
                    404,
                ));
            }
            let current = if locator["archive"].is_object() {
                super::gateway_archives::read_member(
                    config,
                    &locator,
                    &settings["local"]["indexing"],
                    &crate::service_cancellation::current_token()
                        .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
                )?
            } else if locator["fileWindow"].is_object() {
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
            super::gateway_documents::verify_extraction(
                &locator["extraction"],
                &current["extraction"],
            )?;
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
                    && (message["Role"] != "assistant" || message["TaskContinues"] != true)
                    && (locator["role"].is_null() || locator["role"] == message["Role"])
                    && ["ReplyTo", "InReplyToUserMessageId", "OriginUserMessageId", "PreviousReplyMessageId"]
                        .iter().all(|field| locator.get(*field).is_none_or(|value| value == &message[*field]))
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

/// Revalidate only an archived original-text window; the caller must first verify its formal archive identity.
/// 仅复核正式归档中实际读过的原文窗口；调用端必须先验证归档 ID、哈希、字节数及所属范围。
pub(crate) fn verify_saved_read(
    config: &GatewayConfig,
    context: &Value,
    source_ref: &str,
    saved_read: &Value,
) -> ContractResult {
    let reference = resolve_reference(config, context, source_ref)?;
    let scopes = authorized(config, context)?;
    validate_saved_read_content(config, context, &reference, saved_read, &scopes)
}

fn validate_saved_read_content(
    config: &GatewayConfig,
    context: &Value,
    reference: &Value,
    saved_read: &Value,
    scopes: &Value,
) -> ContractResult {
    let (current, derived_reference_current) = read_original_current(config, context, reference, scopes)?;
    validate_saved_read_window(&current, reference, saved_read, derived_reference_current)
}

fn read_original_current(
    config: &GatewayConfig,
    context: &Value,
    reference: &Value,
    scopes: &Value,
) -> Result<(Value, bool), ContractError> {
    match read_current(config, context, reference, scopes) {
        Ok(current) => Ok((current, true)),
        Err(error) if error.code() == "STALE_RETRIEVAL_SOURCE" => {
            // Whole-original identity is separate from replaceable chunk/relationship derivation.
            // 完整原文身份与可替换的分块/关系身份分开；不修改通用引用校验，不将旧结构关系升级为有效。
            let mut original = reference.clone();
            let fields = original.as_object_mut().ok_or_else(|| {
                gateway::error("已读原文引用无效。", "INVALID_EVIDENCE_REFERENCE", 400)
            })?;
            fields.remove("chunkId");
            fields.remove("chunkHash");
            fields.remove("derivationSignature");
            read_current(config, context, &original, scopes).map(|current| (current, false))
        }
        Err(error) => Err(error),
    }
}

fn validate_saved_read_window(
    current: &Value,
    reference: &Value,
    saved_read: &Value,
    derived_reference_current: bool,
) -> ContractResult {
    let invalid = || {
        gateway::error(
            "已读原文窗口已改变或无法核验。",
            "STALE_RETRIEVAL_SOURCE",
            409,
        )
    };
    let saved_text = saved_read["text"]
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 256 * 1024)
        .ok_or_else(invalid)?;
    let start = saved_read["offset"]
        .as_u64()
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or_else(invalid)?;
    let end = saved_read["nextOffset"]
        .as_u64()
        .and_then(|offset| usize::try_from(offset).ok())
        .filter(|end| *end >= start)
        .ok_or_else(invalid)?;
    if !saved_read["locator"].is_object()
        || saved_read
            .get("offsetUnit")
            .is_some_and(|unit| unit != "utf16-code-units")
    {
        return Err(invalid());
    }
    if current["locator"] != saved_read["locator"]
        || saved_read.get("bindingRevision").is_some_and(|revision| {
            revision.as_u64().unwrap_or(0)
                != current["row"]["binding_revision"].as_u64().unwrap_or(0)
        })
    {
        return Err(invalid());
    }
    let units = current["text"]
        .as_str()
        .ok_or_else(invalid)?
        .encode_utf16()
        .collect::<Vec<_>>();
    if !units
        .get(start..end)
        .is_some_and(|window| window == saved_text.encode_utf16().collect::<Vec<_>>().as_slice())
    {
        return Err(invalid());
    }
    Ok(json!({"current":true,"originalTextOnly":true,
        "derivedReferenceCurrent":derived_reference_current,"scopeVerified":true,
        "sourceId":reference["sourceId"],"offset":start,"nextOffset":end,
        "contentHash":reference["contentHash"],"sourceRevision":reference["sourceRevision"]}))
}

fn register_read(
    config: &GatewayConfig,
    context: &Value,
    reference: &Value,
    source_ref: &str,
    text: &str,
    start: usize,
    end: usize,
    current_source: &Value,
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
        start_offset: start,
        end_offset: end,
        locator: current_source["locator"].clone(),
        binding_revision: current_source["row"]["binding_revision"].clone(),
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
/// Load formal gap-counter snapshots without holding a task mutex.
/// 在任务锁外读取正式缺口次数快照；改写关键词、暂停和进程重启不能重置补搜余量。
fn load_refinement_receipts(config:&GatewayConfig,context:&Value) -> Result<Vec<Value>,ContractError> {
    let messages=gateway::read_model_messages(&config.data_root,context["conversationId"].as_str().unwrap_or(""))?;
    let request=context["requestId"].as_str().unwrap_or("");
    let mut seen=std::collections::HashSet::new();
    let mut receipts=Vec::new();
    for message in &messages {
        if !message["RequestId"].as_str().is_some_and(|id|id.eq_ignore_ascii_case(request)) {continue;}
        let references=std::iter::once(&message["RetrievalResultRef"])
            .chain(message["ToolActivities"].as_array().into_iter().flatten()
                .filter(|activity|activity["name"]=="knowledge.search").map(|activity|&activity["resultRef"]));
        for reference in references {
            let Some(id)=reference["id"].as_str() else {continue;};
            if !seen.insert(id) {continue;}
            let archive=gateway::load_private_tool_result(&config.data_root,context["conversationId"].as_str().unwrap_or(""),id)?;
            if archive["requestId"].as_str()!=Some(request) || archive["toolName"]!="knowledge.search" {continue;}
            let refinement=&archive["canonical"]["structuredContent"]["refinement"];
            if refinement["gapKey"].as_str().is_some() {
                receipts.push(refinement.clone());
            }
        }
    }
    Ok(receipts)
}

fn refinement_ticket(config:&GatewayConfig,context:&Value,input:&Value) -> Result<Value,ContractError> {
    reserve_refinement(context,input,||load_refinement_receipts(config,context))
}

fn reserve_refinement(
    context:&Value,input:&Value,load:impl FnOnce()->Result<Vec<Value>,ContractError>,
)->Result<Value,ContractError> {
    let task=task(context)?;
    let needs_restore={ !task.lock().unwrap().refinements_restored };
    // Catalog and archive I/O must finish before taking the task mutex: memory revocation takes the opposite path.
    // 正式目录与归档读取必须在任务锁外完成，避免撤销记忆的目录→任务锁路径发生锁逆序。
    let receipts=if needs_restore {Some(load()?)}else{None};
    let mut task=task.lock().unwrap();
    if let Some(receipts)=receipts {
        // Two loaders may finish concurrently; max-restore must preserve reservations made in between.
        // 两个恢复读取可并发完成；幂等max恢复不能覆盖其间已预留的次数。
        for receipt in receipts {
            task.ledger.apply("retrieval.refinement-restore",&receipt)
                .map_err(|_|gateway::error("检索进度回执无效。","INVALID_RETRIEVAL_EVIDENCE",500))?;
        }
        task.refinements_restored=true;
    }
    // A stable explicit gap can be refined independently; changing only query text cannot create a new gap.
    // 显式缺口可独立求证；仅改query不会创建新的缺口，也不限制其他工具或独立任务。
    let gap=input["gap"].as_str().filter(|gap|!gap.trim().is_empty()).unwrap_or("original-user-question");
    let gap_key=gateway_index::hash(&json!([context["requestId"],gap]).to_string());
    task.ledger.apply("retrieval.refinement-reserve",&json!({"gapKey":gap_key}))
        .map(|receipt|receipt["result"].clone())
        .map_err(|_|gateway::error("检索进度无效。","INVALID_RETRIEVAL_EVIDENCE",500))
}

pub fn handle_tool(
    name: &str,
    input: &Value,
    context: &Value,
    config: &GatewayConfig,
) -> Option<ContractResult> {
    let semantic=name=="knowledge.search" && !["path","symbol"].iter().any(|field|
        input[*field].as_str().is_some_and(|value|!value.is_empty()));
    if !semantic {return handle_tool_attempt(name,input,context,config,0);}
    let ticket=match refinement_ticket(config,context,input) {Ok(ticket)=>ticket,Err(error)=>return Some(Err(error))};
    if ticket["executed"]==false {
        return Some(Ok(json!({"items":[],"refinement":ticket,"coverage":"unresolved",
            "notice":"This evidence gap remains unresolved after the initial lookup and two refinements. State the missing evidence or use a different evidence type; the task and conversation remain active."})));
    }
    let mut result=handle_tool_attempt(name,input,context,config,0);
    if let Some(Ok(value))=&mut result {
        let mut refinement=ticket;refinement["state"]=json!(if value["items"].as_array().is_some_and(|items|!items.is_empty()) {"evidence-available"}else{"unresolved"});
        value["refinement"]=refinement;
    } else if let Ok(task)=task(context) {
        if let Ok(mut task)=task.lock(){let _=task.ledger.apply("retrieval.refinement-release",&ticket);}
    }
    result
}

fn retry_changed_search_scope(
    scopes: &Value,
    input: &Value,
    context: &Value,
    config: &GatewayConfig,
    attempt: u8,
) -> Option<ContractResult> {
    match authorized(config, context) {
        Ok(current) if current == *scopes => None,
        Ok(_) if attempt < 2 => {
            handle_tool_attempt("knowledge.search", input, context, config, attempt + 1)
        }
        Ok(_) => Some(Err(gateway::error(
            "检索范围正在更新，请使用最新范围重试。",
            "RETRIEVAL_QUERY_CHANGED",
            409,
        ))),
        Err(error) => Some(Err(error)),
    }
}

fn handle_tool_attempt(
    name: &str,
    input: &Value,
    context: &Value,
    config: &GatewayConfig,
    scope_attempt: u8,
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
                // Reuse inference only within the freshly authorized conversation scope.
                // 推理缓存只在本次重新授权的会话范围内复用，最终来源和权限检查仍照常执行。
                query["rrfK"] = settings["local"]["rrfK"].clone();
                let cache_scope = json!({
                    "scopeKeys":scopes,"conversationId":context["conversationId"],
                    "permissionMode":context["permissionMode"]
                });
                // Exact locators use lexical/structure lookup and current-source validation without embedding inference.
                // 明确路径和符号使用词法／结构定位并核验当前原文，不触发嵌入推理。
                let targeted = ["path", "symbol"].iter().any(|field| input[*field].as_str().is_some_and(|value| !value.is_empty()));
                let preliminary = if targeted {
                    let mut result = gateway_index::search(config, &query)?;
                    restrict_candidates(context, &mut result); Some(result)
                } else { None };
                let mut current_sources = HashMap::new();
                if let Some(preliminary) = &preliminary {
                    for item in preliminary["items"].as_array().into_iter().flatten()
                        .filter(|item| super::query::exact_target(item, &query["retrievalIntent"]))
                    {
                        if let Ok(mut reference) = resolve_reference(config, context, item["sourceRef"].as_str().unwrap_or("")) {
                            let version = source_version(&reference);
                            reference.as_object_mut().unwrap().remove("chunkId");
                            reference.as_object_mut().unwrap().remove("chunkHash");
                            let current = current_sources.entry(version).or_insert_with(|| read_current(config, context, &reference, &scopes).ok());
                            let _ = current;
                        }
                    }
                }
                if !targeted && settings["local"]["semantic"] != "off" {
                    let text = input["query"].as_str().unwrap_or("").to_string();
                    match super::gateway_embedding::embed_query_cached(
                        config,
                        &[text],
                        &cache_scope,
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
                // Scope changes during shared inference restart this lookup internally.
                // 共享推理期间范围变化时，在内部按最新授权重查，不向模型交付旧范围内容。
                if let Some(retried) = retry_changed_search_scope(&scopes, input, context, config, scope_attempt) {
                    return retried;
                }
                let mut result = if query["queryVector"].is_null() && preliminary.is_some() {
                    preliminary.unwrap()
                } else { gateway_index::search(config, &query)? };
                restrict_candidates(context, &mut result);
                if let Some(retried) = retry_changed_search_scope(&scopes, input, context, config, scope_attempt) {
                    return retried;
                }
                let selected = select_evidence(&result["items"], input, &budget)?;
                result["items"] = selected["items"].clone();
                super::fusion::diversify(result["items"].as_array_mut().unwrap());
                result["selection"] = selected["selection"].clone();
                result["evidenceAssessment"] = selected["evidenceAssessment"].clone();
                let expand = automatic_expansion_allowed(&result["selection"], &budget);
                let mut used_tokens = result["selection"]["usedTokens"].as_u64().unwrap_or(0);
                let maximum_tokens = budget["maximumTokens"].as_u64().unwrap_or(0);
                let mut expanded = 0;
                for item in result["items"].as_array_mut().unwrap() {
                    let mut reference =
                        resolve_reference(config, context, item["sourceRef"].as_str().unwrap())?;
                    // Complete indexed fragments can be used directly after a current-source read; search headings alone cannot.
                    // 经过当前来源复核的完整索引片段可直接使用；仅命中标题不能冒充已读正文。
                    let version = source_version(&reference);
                    if !current_sources.contains_key(&version) {
                        let mut source = reference.clone();
                        source.as_object_mut().unwrap().remove("chunkId");
                        source.as_object_mut().unwrap().remove("chunkHash");
                        current_sources
                            .insert(version.clone(), read_current(config, context, &source, &scopes).ok());
                    }
                    if let Some(current) = current_sources.get(&version).and_then(Option::as_ref) {
                        let units: Vec<u16> =
                            current["text"].as_str().unwrap().encode_utf16().collect();
                        if expand {
                            if let Some(extra) = expand_evidence_window(context, &mut reference, item,
                                &units, maximum_tokens.saturating_sub(used_tokens))? {
                                used_tokens = used_tokens.saturating_add(extra);
                                expanded += 1;
                            }
                        }
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
                                current,
                            )?;
                        }
                    }
                }
                result["selection"]["usedTokens"] = json!(used_tokens);
                result["selection"]["automaticExpansion"] = json!({"eligible":expand,"expandedCount":expanded});
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
                result["retrievalStrategy"] = json!(if targeted { "exact-locator" }
                    else if query["queryVector"].is_array() { "hybrid" } else { "lexical-structure" });
                if let Some(retried) = retry_changed_search_scope(&scopes, input, context, config, scope_attempt) {
                    return retried;
                }
                Ok(result)
            }
            "knowledge.read" => {
                let reference =
                    resolve_reference(config, context, input["sourceRef"].as_str().unwrap_or(""))?;
                let source = read_current(config, context, &reference, &scopes)?;
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
                    &source,
                )?;
                Ok(
                    json!({"sourceRef":input["sourceRef"],"text":body,"offset":start,"nextOffset":end,"totalCharacters":units.len(),"truncated":end<units.len(),"offsetUnit":"utf16-code-units","locator":source["locator"],"bindingRevision":source["row"]["binding_revision"],"decision":decision}),
                )
            }
            "knowledge.relations" => {
                let mut arguments = input.clone();
                arguments["scopes"] = scopes;
                if let Some(reference) = input["sourceRef"].as_str() {
                    let reference = resolve_reference(config, context, reference)?;
                    read_current(config, context, &reference, &arguments["scopes"])?;
                    arguments["revisionRaw"] = json!(reference["sourceRevision"].to_string());
                    arguments["sourceId"] = reference["sourceId"].clone();
                    arguments["reference"] = reference;
                }
                let mut result = storage::sqlite_queries::apply(
                    "storage.index.relations",
                    &gateway_index::arguments(config, arguments),
                )?;
                restrict_candidates(context, &mut result);
                Ok(result)
            }
            "knowledge.assess" => {
                final_validate(config, context)?;
                let task = task(context)?;
                let mut assessment = input.clone();
                let mut checked = Vec::new();
                // Resolve and reread under catalog authority before acquiring the evidence mutex.
                // 先在任务锁外按正式目录核验引用和原文，再锁内匹配当前已读窗口，避免目录/任务锁逆序。
                for (claim_index, claim) in assessment["claims"].as_array_mut().ok_or_else(|| {
                    gateway::error("证据评估缺少结论。", "INVALID_RETRIEVAL_ASSESSMENT", 400)
                })?.iter_mut().enumerate() {
                    for (citation_index, citation) in claim["support"].as_array_mut().ok_or_else(|| {
                        gateway::error("结论引用格式无效。", "INVALID_RETRIEVAL_ASSESSMENT", 400)
                    })?.iter_mut().enumerate() {
                        let reference = resolve_reference(config, context, citation["sourceRef"].as_str().unwrap_or(""))?;
                        let current = read_current(config, context, &reference, &scopes).is_ok();
                        checked.push((claim_index, citation_index, reference, current));
                    }
                }
                let mut task = task.lock().unwrap();
                for (claim_index, citation_index, reference, current) in checked {
                    let citation = &mut assessment["claims"][claim_index]["support"][citation_index];
                    citation["quotePresent"] = json!(current && task.readings.values().any(|(read, fragments)|
                        source_version(read) == source_version(&reference)
                        && fragments.iter().any(|fragment| fragment.reference == reference
                            && fragment.text.contains(citation["quote"].as_str().unwrap_or("")))));
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
    let mut sources = super::gateway::conversation_message_sources(chat, &messages);
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
    super::gateway::enqueue_conversation_semantic(config, chat);
    Ok(Value::Null)
}

fn automatic_expansion_allowed(selection: &Value, budget: &Value) -> bool {
    let used = selection["usedTokens"].as_u64().unwrap_or(u64::MAX);
    let maximum = budget["maximumTokens"].as_u64().unwrap_or(0);
    used.saturating_mul(2) < maximum
        && selection["earlyCutReasons"].as_array().into_iter().flatten()
            .any(|cut| cut["reason"] == "selected-fragment-limit" && cut["count"].as_u64().unwrap_or(0) > 0)
}

fn evidence_window_cost(item: &Value) -> Result<u64, ContractError> {
    let record = crate::models::gateway_protocol::retrieval_model_view(&json!({
        "reference":1,"sourceRef":"s1","title":item["title"],"scope":item["scopeKey"],
        "locator":item["locator"],"structure":item["structure"],"excerpt":item["excerpt"]}))
        .map_err(|error| gateway::error(&error.message,"INVALID_RETRIEVAL_EVIDENCE",error.status_code))?;
    Ok(crate::models::protocol::estimate_tokens(&record.to_string()) + 1)
}

/// Expand only already-authorized current text; both explicit and automatic reads use the same task ledger.
/// 仅扩展已授权且已核验版本的当前原文；显式读取与自动扩读共用任务已读范围及回执账本。
fn expand_evidence_window(
    context: &Value,
    reference: &mut Value,
    item: &mut Value,
    units: &[u16],
    remaining_tokens: u64,
) -> Result<Option<u64>, ContractError> {
    if item["navigationOnly"] == true || remaining_tokens == 0 { return Ok(None); }
    let Some((old_start, old_end)) = item["startOffset"].as_u64().zip(item["endOffset"].as_u64()) else { return Ok(None); };
    let (old_start, old_end) = (old_start as usize, old_end as usize);
    if old_start > old_end || old_end > units.len() { return Ok(None); }
    let unit = item["structure"]["unitStartOffset"].as_u64()
        .zip(item["structure"]["unitEndOffset"].as_u64())
        .map(|(start,end)|(start as usize,end as usize))
        .filter(|(start,end)| *start <= old_start && *end >= old_end && *end <= units.len());
    if item["structure"]["domain"] == "code" && unit.is_none() { return Ok(None); }
    let (lower, upper) = unit.unwrap_or((0, units.len()));
    let (mut start, mut end) = if upper.saturating_sub(lower) <= 4000 {
        (lower, upper)
    } else {
        (old_start.saturating_sub(600).max(lower), old_end.saturating_add(600).min(upper))
    };
    let owner = task(context)?;
    {
        let evidence = owner.lock().unwrap();
        let key = gateway_index::hash(&json!([reference["scopeKey"],reference["sourceId"]]).to_string());
        if let Some((_, fragments)) = evidence.readings.get(&key)
            .filter(|(previous,_)| source_version(previous) == source_version(reference)) {
            for fragment in fragments {
                if fragment.end_offset > start && fragment.start_offset < old_start {
                    start = start.max(fragment.end_offset.min(old_start));
                }
                if fragment.start_offset < end && fragment.end_offset > old_end {
                    end = end.min(fragment.start_offset.max(old_end));
                }
            }
        }
    }
    if super::chunking::split_pair(units, start) { start = start.saturating_sub(1); }
    if super::chunking::split_pair(units, end) { end = (end + 1).min(units.len()); }
    if start == old_start && end == old_end { return Ok(None); }
    let mut candidate = item.clone();
    candidate["excerpt"] = json!(String::from_utf16_lossy(&units[start..end]));
    let extra = evidence_window_cost(&candidate)?.saturating_sub(evidence_window_cost(item)?);
    if extra > remaining_tokens { return Ok(None); }
    reference.as_object_mut().unwrap().remove("chunkId");
    reference.as_object_mut().unwrap().remove("chunkHash");
    candidate["sourceRef"] = json!(gateway_index::reference(reference, None));
    for field in ["chunkId", "chunkHash", "mergedChunks"] { candidate.as_object_mut().unwrap().remove(field); }
    candidate["startOffset"] = json!(start);
    candidate["endOffset"] = json!(end);
    candidate["automaticReadRange"] = json!({"startOffset":start,"endOffset":end,"offsetUnit":"utf16-code-units"});
    *item = candidate;
    Ok(Some(extra))
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
        // Merged chunk identities stay in the private receipt, never in model-facing evidence.
        // 合并片段身份只保留在私有回执，面向模型仅交付已映射的来源级短引用。
        item.as_object_mut().unwrap().remove("mergedChunks");
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
    let mut view = crate::models::gateway_protocol::retrieval_model_view(&output).map_err(|error| {
        gateway::error(
            &error.message,
            "INVALID_RETRIEVAL_EVIDENCE",
            error.status_code,
        )
    })?;
    let items = view.get("structuredContent").unwrap_or(&view)["items"].as_array().cloned().unwrap_or_default();
    let navigation = super::code_units::navigation(&items, 512);
    if navigation["entries"].as_array().is_some_and(|entries| !entries.is_empty()) {
        if structured { view["structuredContent"]["codeNavigation"] = navigation; }
        else { view["codeNavigation"] = navigation; }
    }
    Ok(view)
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
    fn automatic_expansion_requires_count_truncation_and_unused_budget() {
        let budget = json!({"maximumTokens":1000});
        let mut selection = json!({"usedTokens":100,"earlyCutReasons":[]});
        assert!(!automatic_expansion_allowed(&selection, &budget));
        selection["earlyCutReasons"] = json!([{"reason":"evidence-token-budget","count":4}]);
        assert!(!automatic_expansion_allowed(&selection, &budget));
        selection["earlyCutReasons"] = json!([{"reason":"selected-fragment-limit","count":4}]);
        assert!(automatic_expansion_allowed(&selection, &budget));
        selection["usedTokens"] = json!(500);
        assert!(!automatic_expansion_allowed(&selection, &budget));
    }

    #[test]
    fn automatic_expansion_reuses_verified_original_and_shared_task_readings() {
        let workspace = ReadWorkspace::new();
        let result = gateway_index::search(&workspace.config, &json!({
            "query":"原文","scopeKeys":[workspace.source["scopeKey"]]})).unwrap();
        let mut item = result["items"][0].clone();
        assert!(item.is_object());
        let mut reference = resolve_reference(&workspace.config, &workspace.context,
            item["sourceRef"].as_str().unwrap()).unwrap();
        let current = read_current(&workspace.config, &workspace.context, &reference,
            &json!([workspace.source["scopeKey"]])).unwrap();
        let units = current["text"].as_str().unwrap().encode_utf16().collect::<Vec<_>>();
        let text = current["text"].as_str().unwrap();
        let anchor = text[..text.find("第 10").unwrap()].encode_utf16().count();
        item["startOffset"] = json!(anchor);
        item["endOffset"] = json!(anchor + 4);
        item["excerpt"] = json!(String::from_utf16_lossy(&units[anchor..anchor+4]));
        item["structure"] = json!({"domain":"knowledge","unitStartOffset":0,"unitEndOffset":units.len()});
        let original_item = item.clone();
        let original_reference = reference.clone();
        assert!(expand_evidence_window(&workspace.context, &mut reference, &mut item, &units, 0).unwrap().is_none());
        assert_eq!(item, original_item);
        assert_eq!(reference, original_reference);
        assert!(expand_evidence_window(&workspace.context, &mut reference, &mut item, &units, 10_000).unwrap().is_some());
        let start = item["startOffset"].as_u64().unwrap() as usize;
        let end = item["endOffset"].as_u64().unwrap() as usize;
        assert_eq!(item["excerpt"], String::from_utf16_lossy(&units[start..end]));
        assert!(reference.get("chunkId").is_none());
        assert_eq!(item["automaticReadRange"]["offsetUnit"], "utf16-code-units");
        register_read(&workspace.config, &workspace.context, &reference, item["sourceRef"].as_str().unwrap(),
            item["excerpt"].as_str().unwrap(), start, end, &current).unwrap();
        let mut again = original_item;
        let mut again_reference = original_reference;
        assert!(expand_evidence_window(&workspace.context, &mut again_reference, &mut again, &units, 10_000).unwrap().is_none(),
            "knowledge.read and automatic expansion must share the existing recorded ranges");
    }

    #[test]
    fn retiring_revoked_memory_keeps_other_sources_and_other_request_evidence() {
        let chat=gateway::new_id();let request=gateway::new_id();
        let context=json!({"conversationId":chat,"requestId":request});
        let other=json!({"conversationId":chat,"requestId":gateway::new_id()});
        let memory_id=gateway::new_id();let scope=format!("user:{}",gateway::new_id());
        let reference=json!({"scopeKey":scope,"sourceId":gateway_index::hash(&json!(["memory",scope,memory_id]).to_string())});
        let fragment=ReadFragment {reference:reference.clone(),source_ref:"saved-memory".into(),
            text:"Withdrawn preference".into(),start_offset:0,end_offset:20,
            locator:json!({"memoryId":memory_id}),binding_revision:json!(0)};
        let current=task(&context).unwrap();let unrelated=task(&other).unwrap();
        current.lock().unwrap().readings.insert("revoked".into(),(reference.clone(),vec![fragment.clone()]));
        current.lock().unwrap().readings.insert("file".into(),(json!({"sourceId":"actual-file"}),vec![fragment.clone()]));
        unrelated.lock().unwrap().readings.insert("other-request".into(),(reference,vec![fragment]));
        let invalid=json!([{"memoryId":memory_id,"code":"MEMORY_SOURCE_UNAVAILABLE"}]);
        assert_eq!(retire_memory_sources(&context,&invalid).unwrap()["retired"],1);
        assert!(!current.lock().unwrap().readings.contains_key("revoked"));
        assert!(current.lock().unwrap().readings.contains_key("file"));
        assert!(unrelated.lock().unwrap().readings.contains_key("other-request"));
        assert_eq!(retire_memory_sources(&context,&invalid).unwrap()["retired"],0);
        release_task(&chat,&request);
        release_task(&chat,other["requestId"].as_str().unwrap());
    }
    #[test]
    fn task_mount_snapshot_excludes_later_root_from_reads_and_search_candidates() {
        let root = std::env::temp_dir().join("kynxa-mount-snapshot");
        let a = root.join("A"); let b = root.join("B");
        let id = gateway::new_id(); let other = gateway::new_id();
        let context = json!({"bindingSnapshot":{"mounts":[{"id":id,"path":a,"bindingRevision":1}]}});
        let a_locator = json!({"root":a,"mountId":id,"mountBindingRevision":1});
        let b_locator = json!({"root":b,"mountId":other,"mountBindingRevision":1});
        assert!(authorize_locator(&context, "work-file", &a_locator).is_ok());
        assert_eq!(authorize_locator(&context, "work-file", &b_locator).unwrap_err().code(), "RETRIEVAL_SCOPE_MISMATCH");
        let mut result = json!({"items":[{"sourceType":"work-file","locator":a_locator},{"sourceType":"work-file","locator":b_locator}]});
        restrict_candidates(&context, &mut result);
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert!(authorize_locator(&context, "work-file", &json!({"root":a})).is_ok());
        assert!(authorize_locator(&json!({}), "work-file", &b_locator).is_ok());
        assert!(authorize_locator(&json!({"workspaceMounts":[]}), "work-file", &a_locator).is_err());
        let mut rebound = a_locator; rebound["mountBindingRevision"] = json!(2);
        assert!(authorize_locator(&context, "work-file", &rebound).is_err());
    }
    struct ReadWorkspace {
        root: std::path::PathBuf,
        config: GatewayConfig,
        context: Value,
        source: Value,
        reference: String,
        saved_read: Value,
    }
    impl ReadWorkspace {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("kynxa-original-read-{}", gateway::new_id()));
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
            let chat = gateway::new_id();
            gateway::ensure_conversation(&config.data_root, &chat, "原文核验夹具").unwrap();
            let context = json!({"conversationId":chat,"requestId":gateway::new_id()});
            let text = (0..30)
                .map(|index| format!("原文 🔬 第 {index} 行保持不变，只更新语义向量分块。\n"))
                .collect::<String>();
            let source = json!({"sourceId":"stable-original-source","scopeKey":format!("chat:{chat}"),
                "sourceType":"document","title":"original.txt","text":text,
                "locator":{"relativePath":"original.txt"},"sourceRevision":gateway_index::hash(&text)});
            let prepared = gateway_index::prepare_source(&source, None).unwrap();
            gateway_index::publish_prepared(&config, &[prepared.clone()]).unwrap();
            let reference =
                gateway_index::reference(&prepared["source"], Some(&prepared["chunks"][0]));
            let saved_read = handle_tool(
                "knowledge.read",
                &json!({"sourceRef":reference,"limit":80}),
                &context,
                &config,
            )
            .unwrap()
            .unwrap();
            Self {
                root,
                config,
                context,
                source,
                reference,
                saved_read,
            }
        }
        fn refit_chunks(&self) {
            let text = self.source["text"].as_str().unwrap();
            let structure = super::super::gateway_structure::parse(text, "original.txt").unwrap();
            let mut chunks = super::super::gateway_structure::chunks(text, &structure, 64).unwrap();
            for chunk in &mut chunks {
                chunk["chunkerVersion"] = json!("token-fitted-regression-v1");
            }
            let vectors = vec![vec![1.0, 0.0]; chunks.len()];
            let mut source = self.source.clone();
            source["chunks"] = json!(chunks);
            source["embeddingProfileId"] = json!("fixture-profile");
            source["embeddingModelVersion"] = json!("fixture-model-v1");
            source["embeddingSpaceId"] = json!("fixture-space-v1");
            let prepared = gateway_index::prepare_source(&source, Some(&vectors)).unwrap();
            gateway_index::publish_prepared(&self.config, &[prepared]).unwrap();
        }
    }
    impl Drop for ReadWorkspace {
        fn drop(&mut self) {
            release_task(
                self.context["conversationId"].as_str().unwrap(),
                self.context["requestId"].as_str().unwrap(),
            );
            storage::gateway_memory::stop_candidates(&self.config.data_root);
            let _ = storage::sqlite_queries::apply(
                "storage.index.closeConnections",
                &gateway_index::arguments(&self.config, json!({})),
            );
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn semantic_refit_preserves_only_verified_original_windows() {
        let workspace = ReadWorkspace::new();
        workspace.refit_chunks();
        let recovered = verify_saved_read(
            &workspace.config,
            &workspace.context,
            &workspace.reference,
            &workspace.saved_read,
        )
        .unwrap();
        assert_eq!(recovered["current"], true);
        assert_eq!(recovered["originalTextOnly"], true);
        assert_eq!(recovered["derivedReferenceCurrent"], false);
        let final_state = final_validate(&workspace.config, &workspace.context).unwrap();
        assert_eq!(final_state["current"], true);
        assert_eq!(final_state["originalReadingsRevalidated"], 1);
        assert_eq!(final_state["derivedReferencesRevalidated"], false);
        assert_eq!(
            handle_tool(
                "knowledge.read",
                &json!({"sourceRef":workspace.reference}),
                &workspace.context,
                &workspace.config
            )
            .unwrap()
            .unwrap_err()
            .code(),
            "STALE_RETRIEVAL_SOURCE"
        );
        assert_eq!(
            handle_tool(
                "knowledge.relations",
                &json!({"sourceRef":workspace.reference}),
                &workspace.context,
                &workspace.config
            )
            .unwrap()
            .unwrap_err()
            .code(),
            "STALE_RETRIEVAL_SOURCE"
        );
        for field in ["text", "offset", "locator", "bindingRevision"] {
            let mut changed = workspace.saved_read.clone();
            changed[field] = match field {
                "text" => json!("被替换的正文"),
                "offset" => json!(1),
                "locator" => json!({"relativePath":"another.txt"}),
                _ => json!(1),
            };
            assert_eq!(
                verify_saved_read(
                    &workspace.config,
                    &workspace.context,
                    &workspace.reference,
                    &changed
                )
                .unwrap_err()
                .code(),
                "STALE_RETRIEVAL_SOURCE",
                "{field}"
            );
        }
        let foreign = gateway::new_id();
        gateway::ensure_conversation(&workspace.config.data_root, &foreign, "其他聊天").unwrap();
        assert_eq!(
            verify_saved_read(
                &workspace.config,
                &json!({"conversationId":foreign,"requestId":gateway::new_id()}),
                &workspace.reference,
                &workspace.saved_read
            )
            .unwrap_err()
            .code(),
            "RETRIEVAL_SOURCE_NOT_FOUND"
        );
    }

    #[test]
    fn refinement_receipt_loading_releases_task_lock_and_stale_snapshots_keep_reservations() {
        let chat = gateway::new_id(); let request = gateway::new_id();
        let context = json!({"conversationId":chat,"requestId":request});
        let state = task(&context).unwrap();
        let gap = gateway_index::hash(&json!([request,"original-user-question"]).to_string());
        let saved = vec![json!({"gapKey":gap,"attempt":1})];
        // Interleave a second recovery while the first loader is outstanding, without timing-sensitive threads.
        // 在首个快照尚未返回时穿插第二次恢复，确定性验证读盘锁边界和旧快照不能回滚新预约。
        let outer = reserve_refinement(&context,&json!({}),|| {
            assert!(state.try_lock().is_ok(),"formal receipt I/O must not hold the task mutex");
            let inner = reserve_refinement(&context,&json!({}),|| Ok(saved.clone())).unwrap();
            assert_eq!(inner["attempt"],2);
            Ok(saved.clone())
        }).unwrap();
        assert_eq!(outer["attempt"],3);
        let exhausted = reserve_refinement(&context,&json!({}),||panic!("already restored; no additional I/O")).unwrap();
        assert_eq!(exhausted["executed"],false);
        assert_eq!(exhausted["remainingSupplementalSearches"],0);
        release_task(&chat,&request);
    }

    #[test]
    fn semantic_refinements_restore_after_restart_and_do_not_charge_new_gaps() {
        let workspace=ReadWorkspace::new();
        let first=refinement_ticket(&workspace.config,&workspace.context,&json!({"query":"initial wording"})).unwrap();
        assert_eq!(first["attempt"],1);
        let second=refinement_ticket(&workspace.config,&workspace.context,&json!({"query":"rewritten keywords"})).unwrap();
        assert_eq!(second["attempt"],2);
        let request=workspace.context["requestId"].as_str().unwrap();
        let chat=workspace.context["conversationId"].as_str().unwrap();
        let call=gateway::new_id();
        let reference=gateway::save_tool_result(&workspace.config.data_root,chat,request,
            &json!({"id":call,"name":"knowledge.search","arguments":{"query":"rewritten keywords"}}),
            &json!({"structuredContent":{"items":[],"refinement":second},"content":[]}),None).unwrap();
        gateway::upsert_message(&workspace.config.data_root,chat,&json!({"Id":request,"RequestId":request,
            "Role":"assistant","Status":"streaming","Content":"Working",
            "ToolActivities":[{"toolCallId":call,"name":"knowledge.search","arguments":{"query":"rewritten keywords"},"status":"completed","resultRef":reference}]})).unwrap();
        release_task(chat,request);
        let recovered=refinement_ticket(&workspace.config,&workspace.context,&json!({"query":"yet another rewrite"})).unwrap();
        assert_eq!(recovered["attempt"],3);
        assert_eq!(recovered["remainingSupplementalSearches"],0);
        let exhausted=refinement_ticket(&workspace.config,&workspace.context,&json!({"query":"fourth rewrite"})).unwrap();
        assert_eq!(exhausted["state"],"unresolved"); assert_eq!(exhausted["executed"],false);
        let independent=refinement_ticket(&workspace.config,&workspace.context,&json!({"query":"different fact","gap":"new independent fact"})).unwrap();
        assert_eq!(independent["attempt"],1);
        let continuing=gateway::read_model_messages(&workspace.config.data_root,chat).unwrap();
        assert_eq!(continuing.iter().find(|message|message["Id"]==request).unwrap()["Status"],"streaming");
    }

    #[test]
    fn changed_or_deleted_originals_cannot_use_saved_window_recovery() {
        let workspace = ReadWorkspace::new();
        workspace.refit_chunks();
        let mut changed = workspace.source.clone();
        changed["text"] = json!("新的原文版本，旧窗口不可继续使用。");
        changed["sourceRevision"] = json!(gateway_index::hash(changed["text"].as_str().unwrap()));
        gateway_index::publish(&workspace.config, &[changed]).unwrap();
        assert_eq!(
            verify_saved_read(
                &workspace.config,
                &workspace.context,
                &workspace.reference,
                &workspace.saved_read
            )
            .unwrap_err()
            .code(),
            "STALE_RETRIEVAL_SOURCE"
        );
        assert_eq!(
            final_validate(&workspace.config, &workspace.context).unwrap()["current"],
            false
        );
        gateway_index::remove(
            &workspace.config,
            workspace.source["sourceId"].as_str().unwrap(),
            &json!([workspace.source["scopeKey"]]),
            true,
        )
        .unwrap();
        assert_eq!(
            verify_saved_read(
                &workspace.config,
                &workspace.context,
                &workspace.reference,
                &workspace.saved_read
            )
            .unwrap_err()
            .code(),
            "RETRIEVAL_SOURCE_NOT_FOUND"
        );
    }

    #[test]
    fn automatic_evidence_gate_skips_chat_and_preserves_local_knowledge_queries() {
        for text in ["你好", "谢谢你", "讲个笑话", "现在什么时间"] {
            assert!(!should_prepare_request(text).unwrap(), "{text}");
        }
        let local = "根据项目文档解释之前讨论的缓存机制";
        assert!(should_prepare_request(local).unwrap());
        let budgeted = request_plan(local, Some(1)).unwrap();
        assert!(should_prepare_plan(local, &budgeted).unwrap());
    }

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

fn request_plan(text: &str, budget: Option<u64>) -> ContractResult {
    let mut arguments = json!({"query":{"originalText":text,"activeText":text,"instructionText":text}});
    if let Some(budget) = budget {
        arguments["maximumTokens"] = json!(budget);
    }
    crate::retrieval::query::apply("retrieval.query-plan", &arguments)
        .map_err(|_| gateway::error("取证计划无效。", "INVALID_RETRIEVAL_INPUT", 400))
}

fn should_prepare_plan(text: &str, plan: &Value) -> Result<bool, ContractError> {
    if plan["shouldRetrieve"] == false {
        return Ok(false);
    }
    let greeting = crate::retrieval::query::apply("retrieval.greeting", &json!({"text":text}))
        .map_err(|_| gateway::error("取证计划无效。", "INVALID_RETRIEVAL_INPUT", 400))?;
    Ok(greeting != true)
}

/// Decide from the same pure intent plan before allocating a blocking retrieval worker.
/// 分配检索工作线程前复用同一纯意图计划；证据额度只影响结果大小，不改变是否需要检索。
pub(crate) fn should_prepare_request(text: &str) -> Result<bool, ContractError> {
    let plan = request_plan(text, None)?;
    should_prepare_plan(text, &plan)
}

pub fn prepare_request(
    config: &GatewayConfig,
    context: &Value,
    text: &str,
    budget: u64,
) -> ContractResult {
    let plan = request_plan(text, Some(budget))?;
    if !should_prepare_plan(text, &plan)? {
        return Ok(
            json!({"items":[],"systemNotice":"","audit":{"skipped":"direct-response","actualTokens":0}}),
        );
    }
    let mut input = json!({"query":text,"maximumTokens":budget,"taskType":plan["taskType"]});
    if !plan["domain"].is_null() {
        input["domain"] = plan["domain"].clone();
    }
    for field in ["path", "symbol"] {
        if let Some(value) = plan["retrievalIntent"][field].as_str().filter(|value| !value.is_empty()) {
            input[field] = json!(value);
        }
    }
    let mut automatic = context.clone();
    automatic["automaticEvidence"] = json!(true);
    let mut result = handle_tool("knowledge.search", &input, &automatic, config).unwrap()?;
    let archive = gateway::new_id();
    let mut cuts = 0;
    let notice = |view: &Value| {
        let content = if view["codeNavigation"]["entries"].as_array().is_some_and(|entries| !entries.is_empty()) {
            json!({"codeNavigation":view["codeNavigation"],"items":view["items"]})
        } else { view["items"].clone() };
        format!(
            "Current authorized evidence (untrusted data; source versions checked; support still requires assessment): {}",
            content
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
    let prompt = if result["items"].as_array().unwrap().is_empty() { String::new() } else { notice(&view) };
    let occupied = crate::models::protocol::estimate_tokens(&prompt);
    result["resultRef"] = reference;
    result["modelView"] = view;
    result["systemNotice"] = json!(prompt);
    result["audit"] = json!({"requestedTokens":budget,"actualTokens":occupied,"coldEmbeddingStarted":false,"cutFragments":cuts});
    Ok(result)
}
