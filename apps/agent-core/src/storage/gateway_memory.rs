//! Revisioned memory scopes reuse the canonical catalog and existing repository transitions.
//! 带版本的记忆范围复用正式目录归属与既有仓储迁移规则。

use super::{
    ContractResult,
    gateway::{self, GatewayResult},
    memory, memory_repository, memory_service,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use unicode_normalization::UnicodeNormalization;

const MAXIMUM_BYTES: u64 = 8 * 1024 * 1024;
struct CandidateOwner {
    policy: Mutex<super::memory_lifecycle::MemoryLifecycleState>,
    queue: Mutex<VecDeque<(String, Value, u64)>>,
    running: AtomicBool,
    stopped: AtomicBool,
}
static CANDIDATES: OnceLock<Mutex<HashMap<PathBuf, Arc<CandidateOwner>>>> = OnceLock::new();
type IndexCallback = Arc<dyn Fn(&Value) + Send + Sync>;
static INDEXERS: OnceLock<Mutex<HashMap<PathBuf, IndexCallback>>> = OnceLock::new();
#[cfg(test)]
static CANDIDATE_COMMIT_OBSERVER: Mutex<Option<(PathBuf, IndexCallback)>> = Mutex::new(None);
pub fn set_indexer(root: &Path, callback: IndexCallback) {
    INDEXERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), callback);
}
pub(crate) fn validate_index_source(
    root: &Path,
    catalog: &Value,
    source: &Value,
) -> ContractResult {
    let scope = source["scopeKey"]
        .as_str()
        .ok_or_else(|| gateway::error("记忆范围无效。", "MEMORY_SCOPE_CHANGED", 409))?;
    let (kind, id) = scope.split_once(':').unwrap_or(("user", "user"));
    let (scope_id, path) = location(
        root,
        catalog,
        kind,
        id,
        if kind == "chat" { Some(id) } else { None },
    )?;
    let document = read_scope(root, &path, kind, &scope_id)?.0;
    let entry = document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["id"] == source["locator"]["memoryId"] && entry["status"] == "confirmed"
        })
        .ok_or_else(|| gateway::error("记忆已撤销。", "STALE_RETRIEVAL_SOURCE", 409))?;
    let active = memory_service::apply(
        "storage.memoryService.sourceStatus",
        &source_facts(root, catalog, entry)?,
    )?;
    if active["active"] != true
        || entry["revision"] != source["sourceRevision"]
        || candidate_content_hash(entry) != source["contentHash"]
    {
        return Err(gateway::error(
            "记忆来源已改变。",
            "STALE_RETRIEVAL_SOURCE",
            409,
        ));
    }
    Ok(entry.clone())
}
fn candidate_content_hash(entry: &Value) -> Value {
    json!(format!(
        "{:x}",
        Sha256::digest(entry["content"].as_str().unwrap_or("").as_bytes())
    ))
}
pub(crate) fn read_index_document(root: &Path, scope: &str, id: &str) -> ContractResult {
    let catalog = gateway::load_catalog(root)?;
    let (scope_id, path) = location(
        root,
        &catalog,
        scope,
        id,
        if scope == "chat" { Some(id) } else { None },
    )?;
    let (document, hash) = read_scope(root, &path, scope, &scope_id)?;
    let mut decorated = decorate_snapshot(
        root,
        &catalog,
        &[(document, path, hash)],
        if scope == "chat" { Some(id) } else { None },
    )?;
    Ok(decorated.remove(0))
}
/// Discover only authorized, confirmed records at startup; persisted drafts stay outside semantic preparation.
/// 启动仅发现合法范围的已确认记录；持久草稿和候选不进入语义准备队列。
pub(crate) fn startup_index_scopes(root: &Path, cancel: &Arc<AtomicBool>) -> ContractResult {
    // Snapshot only identities under the catalog lock; maintenance rechecks ownership before publication.
    // 仅在目录锁内取得身份快照；枚举不长期阻塞前台，实际维护发布前重新核验归属。
    let catalog = gateway::load_catalog(root)?;
    let mut candidates = vec![("user".to_string(), "user".to_string())];
    for chat in catalog["Chats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|chat| chat["IsArchived"] != true)
    {
        candidates.push(("chat".into(), chat["Id"].as_str().unwrap().to_string()));
    }
    for project in catalog["Projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|project| project["IsArchived"] != true)
    {
        if project["IsFolderlessWorkspace"] != true {
            candidates.push((
                "project".into(),
                project["Id"].as_str().unwrap().to_string(),
            ));
        }
        for chat in project["Chats"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|chat| chat["IsArchived"] != true)
        {
            candidates.push(("chat".into(), chat["Id"].as_str().unwrap().to_string()));
        }
    }
    let mut scopes = Vec::new();
    let mut failures = Vec::new();
    let mut failed = 0;
    for (scope, id) in candidates {
        if cancel.load(Ordering::Acquire) {
            return Err(gateway::error("记忆发现已取消。", "INDEX_CANCELLED", 409));
        }
        let result = (|| -> ContractResult {
            let (scope_id, path) = location(
                root,
                &catalog,
                &scope,
                &id,
                if scope == "chat" {
                    Some(id.as_str())
                } else {
                    None
                },
            )?;
            let (_, digest) = read_scope(root, &path, &scope, &scope_id)?;
            // Empty documents also clean a previous confirmed index; drafts are never selected for vectors.
            // 空文件同样清理之前已确认的索引；后续语义选择始终排除草稿。
            if digest.is_some() {
                Ok(json!({"scope":scope,"scopeId":scope_id}))
            } else {
                Ok(Value::Null)
            }
        })();
        match result {
            Ok(value) if !value.is_null() => scopes.push(value),
            Ok(_) => {}
            Err(error) => {
                failed += 1;
                if failures.len() < 64 {
                    failures.push(json!({"scope":scope,"scopeId":id,"code":error.code()}));
                }
            }
        }
    }
    Ok(json!({"scopes":scopes,"failed":failed,"failures":failures,"complete":failed==0}))
}
fn lifecycle(owner: &CandidateOwner, operation: &str, args: Value) -> ContractResult {
    let receipt = owner
        .policy
        .lock()
        .map_err(|_| gateway::error("记忆候选状态暂时不可用。", "MEMORY_CANDIDATE_FAILED", 503))?
        .apply(operation, &args)
        .map_err(|message| gateway::error(&message, "MEMORY_CANDIDATE_FAILED", 500))?;
    if let Some(error) = receipt.get("error") {
        return Err(gateway::error(
            error["message"].as_str().unwrap_or("记忆候选操作失败。"),
            if error["code"] == "MEMORY_CONFLICT" {
                "MEMORY_CONFLICT"
            } else {
                "INVALID_MEMORY"
            },
            error["statusCode"].as_u64().unwrap_or(400) as u16,
        ));
    }
    Ok(receipt["value"].clone())
}
fn candidate_owner(root: &Path) -> Result<Arc<CandidateOwner>, super::ContractError> {
    let mut owners = CANDIDATES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| gateway::error("记忆候选服务暂时不可用。", "MEMORY_CANDIDATE_FAILED", 503))?;
    if let Some(owner) = owners.get(root) {
        return Ok(owner.clone());
    }
    let settings = memory::apply("storage.memory.settings", &json!({"value":{}}))?;
    let mut policy = super::memory_lifecycle::MemoryLifecycleState::default();
    policy
        .apply(
            "storage.memoryLifecycle.init",
            &json!({"settings":settings,"override":false}),
        )
        .map_err(|message| gateway::error(&message, "MEMORY_CANDIDATE_FAILED", 500))?;
    let owner = Arc::new(CandidateOwner {
        policy: Mutex::new(policy),
        queue: Mutex::new(VecDeque::new()),
        running: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
    });
    owners.insert(root.to_path_buf(), owner.clone());
    Ok(owner)
}
pub fn candidate_status(root: &Path) -> ContractResult {
    let owner = candidate_owner(root)?;
    gateway::with_catalog(root, |_, _| {
        let document = read_scope(root, &root.join("Memory/entries.json"), "user", "user")?.0;
        lifecycle(
            &owner,
            "storage.memoryLifecycle.initialize",
            json!({"document":document}),
        )?;
        lifecycle(&owner, "storage.memoryLifecycle.status", json!({}))
    })
}
pub fn stop_candidates(root: &Path) {
    if let Some(owners) = CANDIDATES.get() {
        if let Some(owner) = owners
            .lock()
            .ok()
            .and_then(|owners| owners.get(root).cloned())
        {
            owner.stopped.store(true, Ordering::Release);
        }
    }
}
/// A stopped owner is replaced only after its last formal capture has settled.
/// 仅在最后一次正式候选提取结算后替换已停止的所有者，避免迁移重载与旧写入交叠。
pub fn resume_candidates(root: &Path) -> Result<(), super::ContractError> {
    let mut owners = CANDIDATES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| gateway::error("记忆候选服务暂时不可用。", "MEMORY_CANDIDATE_FAILED", 503))?;
    if let Some(owner) = owners.get(root) {
        if owner.stopped.load(Ordering::Acquire) {
            if owner.running.load(Ordering::Acquire) {
                return Err(gateway::error(
                    "记忆候选仍在结算。",
                    "MEMORY_CANDIDATE_BUSY",
                    409,
                ));
            }
            owners.remove(root);
        }
    }
    Ok(())
}
pub fn drain_candidates(root: &Path) -> Result<(), super::ContractError> {
    stop_candidates(root);
    let owner = CANDIDATES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| gateway::error("记忆候选服务暂时不可用。", "MEMORY_CANDIDATE_FAILED", 503))?
        .get(root)
        .cloned();
    if let Some(owner) = owner {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while owner.running.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                return Err(gateway::error(
                    "记忆候选仍未结算，原目录已保留。",
                    "MEMORY_CANDIDATE_BUSY",
                    409,
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    Ok(())
}

/// A saved explicit instruction grants confirmation; ordinary background proposals keep their draft boundary.
/// 已正式保存的明确记住指令授权立即确认；普通后台建议仍保留草稿确认边界。
pub fn capture_explicit_instruction(
    root: &Path,
    conversation: &str,
    user_message_id: &str,
) -> ContractResult {
    let source = memory::apply(
        "storage.memory.source",
        &json!({"value":{"type":"user-message","role":"user","conversationId":conversation,"messageId":user_message_id}}),
    )?;
    let conversation = source["conversationId"].as_str().unwrap();
    let message_id = source["messageId"].as_str().unwrap();
    let catalog = gateway::load_catalog(root)?;
    let relationship = super::catalog::apply(
        "storage.catalog.related",
        &json!({"current":catalog,"id":conversation}),
    )?;
    let mut history = super::conversation_history::read_latest(
        root,
        &gateway::locate(&catalog, conversation)?,
        Some(&HashSet::from([message_id.to_string()])),
        true,
    )?;
    let raw = history.messages.first().ok_or_else(|| {
        gateway::error("记忆来源须为已保存的真实用户消息。", "INVALID_MEMORY", 400)
    })?;
    let saved: Value = serde_json::from_str(&super::raw_json::scalar_projection(raw))
        .map_err(|_| gateway::error("记忆来源消息损坏。", "CORRUPT_CONVERSATION", 500))?;
    let saved = gateway::normalize_message(&saved)?;
    let instruction = memory::apply(
        "storage.memory.instruction",
        &json!({"message":saved["Content"],"hasProject":relationship["memoryScopes"].as_array().unwrap().contains(&json!("project"))}),
    )?;
    if instruction.is_null() {
        history.verify()?;
        return Ok(json!({"created":false,"reason":"no-trigger"}));
    }
    let scope = instruction["scope"].as_str().unwrap();
    let scope_owner = if scope == "project" {
        relationship["projectId"].as_str().unwrap()
    } else {
        conversation
    };
    let text_units = saved["Content"]
        .as_str()
        .unwrap()
        .encode_utf16()
        .collect::<Vec<_>>();
    let content = String::from_utf16(
        &text_units[instruction["startOffset"].as_u64().unwrap() as usize
            ..instruction["endOffset"].as_u64().unwrap() as usize],
    )
    .map_err(|_| gateway::error("记忆原话边界无效。", "INVALID_MEMORY", 400))?;
    let (result, changed_document) = gateway::with_catalog(root, |current, _| {
        gateway::verify_catalog_sources(&catalog, current, &[conversation.to_string()])?;
        let (scope_id, path) = location(root, current, scope, scope_owner, Some(conversation))?;
        let (document, digest) = read_scope(root, &path, scope, &scope_id)?;
        let mut input = json!({"scope":scope,"scopeId":scope_id,"content":content,"kind":instruction["kind"],"source":source});
        memory_service::apply(
            "storage.memoryService.validateSource",
            &source_facts(root, current, &input)?,
        )?;
        // Older captures may already hold this strict remember instruction as a draft; only that exact source may upgrade.
        // 旧提取可能已把严格记住指令保存成草稿；只迁移同一来源的 remember 候选，不接受模型推断或动作提案。
        let is_dismissed = document["dismissedSources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["conversationId"] == source["conversationId"]
                    && item["messageId"] == source["messageId"]
            });
        let legacy = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| {
                !is_dismissed
                    && entry["status"] == "draft"
                    && entry["source"] == source
                    && entry["candidate"]["trigger"] == "remember"
            });
        let (operation, entry_id) = if let Some(entry) = legacy {
            memory_service::apply(
                "storage.memoryService.validateSource",
                &source_facts(root, current, entry)?,
            )?;
            input["status"] = json!("confirmed");
            ("storage.repository.update", entry["id"].clone())
        } else {
            ("storage.repository.create", json!(gateway::new_id()))
        };
        let planned = memory_repository::apply(
            operation,
            &json!({"document":document,"input":input,"id":entry_id,"now":gateway::now()}),
        )?;
        let mut next = planned["document"].clone();
        history.verify()?;
        if planned["changed"] == true {
            next["revision"] = json!(document["revision"].as_u64().unwrap() + 1);
            memory::apply(
                "storage.memory.document",
                &json!({"value":next,"scope":scope,"scopeId":scope_id,"timestamps":gateway::timestamps(&next)}),
            )?;
            gateway::commit_json(root, &path, &next, digest.as_deref(), MAXIMUM_BYTES)?;
        }
        let entries = next["entries"].as_array().unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry["id"] == entry_id)
            .or_else(|| {
                entries.iter().find(|entry| {
                    entry["source"]["conversationId"] == source["conversationId"]
                        && entry["source"]["messageId"] == source["messageId"]
                })
            })
            .cloned()
            .unwrap_or(Value::Null);
        Ok((
            json!({"created":planned["changed"],"reason":if planned["changed"]==true{"created"}else{"deduplicated"},"scope":scope,"scopeId":scope_id,"revision":next["revision"],"entry":entry,"requiresConfirmation":entry["status"]=="draft"}),
            (planned["changed"] == true).then_some(next),
        ))
    })?;
    if let Some(document) = changed_document {
        if let Some(callback) = INDEXERS
            .get()
            .and_then(|owners| owners.lock().ok())
            .and_then(|owners| owners.get(root).cloned())
        {
            callback(&document);
        }
    }
    Ok(result)
}
/// Retire queued candidates only; a running capture still verifies the formal catalog before committing.
/// 仅撤出已删除聊天的排队候选；正在运行的提取仍在提交前核验正式目录，不影响其他聊天或共享确认记忆。
pub(crate) fn retire_chat_candidates(root: &Path, conversation: &str) -> ContractResult {
    let owner = CANDIDATES
        .get()
        .and_then(|owners| owners.lock().ok()?.get(root).cloned());
    let Some(owner) = owner else {
        return Ok(json!({"removed":0}));
    };
    let removed = {
        let mut queue = owner.queue.lock().map_err(|_| {
            gateway::error("记忆候选队列暂时不可用。", "MEMORY_CANDIDATE_FAILED", 503)
        })?;
        let mut removed = Vec::new();
        queue.retain(|(id, _, sequence)| {
            if id.eq_ignore_ascii_case(conversation) {
                removed.push((id.clone(), *sequence));
                false
            } else {
                true
            }
        });
        removed
    };
    for (id, sequence) in &removed {
        lifecycle(
            &owner,
            "storage.memoryLifecycle.settled",
            json!({"id":id,"sequence":sequence,"result":{"created":0,"reason":"conversation-deleted"}}),
        )?;
    }
    Ok(json!({"removed":removed.len()}))
}
pub fn schedule_candidates(root: &Path, conversation: &str, options: Value) -> ContractResult {
    let owner = candidate_owner(root)?;
    if owner.stopped.load(Ordering::Acquire) {
        return Ok(json!({"created":0,"reason":"stopped"}));
    }
    candidate_status(root)?;
    let admission = lifecycle(
        &owner,
        "storage.memoryLifecycle.schedule",
        json!({"id":conversation}),
    )?;
    if admission["admitted"] != true {
        return Ok(admission["result"].clone());
    }
    let sequence = admission["sequence"].as_u64().unwrap();
    let mut queue = owner
        .queue
        .lock()
        .map_err(|_| gateway::error("记忆候选队列暂时不可用。", "MEMORY_CANDIDATE_FAILED", 503))?;
    queue.retain(|(id, _, _)| !id.eq_ignore_ascii_case(conversation));
    queue.push_back((conversation.to_string(), options, sequence));
    if !owner.running.swap(true, Ordering::AcqRel) {
        let root = root.to_path_buf();
        let owner = owner.clone();
        std::thread::Builder::new()
            .name("kynxa-memory-candidates".into())
            .spawn(move || {
                loop {
                    let next = {
                        let mut queue = owner.queue.lock().unwrap();
                        let next = queue.pop_front();
                        if next.is_none() {
                            owner.running.store(false, Ordering::Release);
                        }
                        next
                    };
                    let Some((id, options, sequence)) = next else {
                        break;
                    };
                    let result = if owner.stopped.load(Ordering::Acquire) {
                        Ok(json!({"created":0,"reason":"stopped"}))
                    } else {
                        capture_candidates(&root, &id, &options, &owner)
                    };
                    let receipt = match result {
                        Ok(result) => json!({"id":id,"sequence":sequence,"result":result}),
                        Err(error) => {
                            json!({"id":id,"sequence":sequence,"failed":true,"code":error.code()})
                        }
                    };
                    let _ = lifecycle(&owner, "storage.memoryLifecycle.settled", receipt);
                }
            })
            .map_err(|_| gateway::error("记忆候选线程未启动。", "MEMORY_CANDIDATE_FAILED", 503))?;
    }
    Ok(json!({"queued":true,"sequence":sequence}))
}
fn capture_candidates(
    root: &Path,
    conversation: &str,
    options: &Value,
    owner: &CandidateOwner,
) -> ContractResult {
    let catalog_snapshot = gateway::load_catalog(root)?;
    let mut history =
        gateway::read_raw_snapshot(root, &gateway::locate(&catalog_snapshot, conversation)?)?;
    let messages = history
        .messages
        .iter()
        .map(|raw| {
            let message: Value = serde_json::from_str(&super::raw_json::scalar_projection(raw))
                .map_err(|_| gateway::error("记忆候选来源损坏。", "CORRUPT_CONVERSATION", 500))?;
            gateway::normalize_message(&message)
        })
        .collect::<Result<Vec<_>, _>>()?;
    (|| {
        let catalog = &catalog_snapshot;
        let relation = super::catalog::apply(
            "storage.catalog.related",
            &json!({"current":catalog,"id":conversation}),
        )?;
        let mut documents = Vec::new();
        for scope in relation["memoryScopes"].as_array().unwrap() {
            let scope = scope.as_str().unwrap();
            let id = if scope == "project" {
                relation["projectId"].as_str().unwrap()
            } else {
                conversation
            };
            let (scope_id, path) = location(root, catalog, scope, id, Some(conversation))?;
            let (document, digest) = read_scope(root, &path, scope, &scope_id)?;
            documents.push((scope.to_string(), scope_id, path, document, digest));
        }
        let user = documents.iter().find(|record| record.0 == "user").unwrap();
        let settings_revision = user.3["candidateSettingsRevision"].as_u64().unwrap_or(0);
        lifecycle(
            owner,
            "storage.memoryLifecycle.initialize",
            json!({"document":user.3}),
        )?;
        let settings = lifecycle(owner, "storage.memoryLifecycle.settings", json!({}))?;
        let projected = messages.iter().map(|message| {
            let text = message["Content"].as_str().unwrap_or("");
            let text_length_utf16 = text.encode_utf16().count();
            let eligible_length = text_length_utf16 <= 4000;
            json!({"id":if message["Role"]=="user"{message["Id"].clone()}else{Value::Null},"role":message["Role"],"status":message["Status"],"text":if message["Role"]=="user"&&eligible_length{message["Content"].clone()}else{json!("")},"textLengthUtf16":text_length_utf16,"eligibleLength":eligible_length,"hasContent":!text.trim().is_empty(),"estimatedTokens":text.len().div_ceil(3)})
        }).collect::<Vec<_>>();
        let plan = lifecycle(
            owner,
            "storage.memoryLifecycle.capturePlan",
            json!({"conversationId":conversation,"hasProject":relation["memoryScopes"].as_array().unwrap().contains(&json!("project")),"messages":projected,"documents":documents.iter().map(|record|record.3.clone()).collect::<Vec<_>>()}),
        )?;
        let extracted = memory::apply(
            "storage.memory.extract",
            &json!({"messages":projected,"phase":options["phase"].as_str().unwrap_or("completed"),"userMessageId":options["userMessageId"],"taskCompleted":options["taskCompleted"]==true,"settings":settings,"hasProject":plan["hasProject"],"consumedMessageIds":plan["consumedMessageIds"],"afterMessageId":plan["afterMessageId"]}),
        )?;
        let confirmed = documents
            .iter()
            .flat_map(|record| record.3["entries"].as_array().unwrap())
            .filter(|entry| entry["status"] == "confirmed")
            .map(|entry| candidate_fingerprint(&json!("chat"), &entry["content"]))
            .collect::<Vec<_>>();
        let mut created = 0;
        let mut deduplicated = 0;
        for proposed in extracted["candidates"].as_array().unwrap() {
            let quotes = proposed["indices"]
                .as_array()
                .unwrap()
                .iter()
                .map(|index| {
                    let message = &messages[index.as_u64().unwrap() as usize];
                    json!({"messageId":message["Id"],"text":message["Content"]})
                })
                .collect::<Vec<_>>();
            let content = quotes
                .iter()
                .map(|quote| quote["text"].as_str().unwrap().trim())
                .collect::<Vec<_>>()
                .join("\n\n");
            if confirmed.contains(&candidate_fingerprint(&json!("chat"), &json!(content))) {
                deduplicated += 1;
                continue;
            }
            let Some(record) = documents
                .iter_mut()
                .find(|record| record.0 == proposed["scope"])
            else {
                continue;
            };
            let mut descriptor = json!({"algorithm":"conservative-extractive-v1","trigger":proposed["trigger"],"fingerprint":candidate_fingerprint(&proposed["scope"],&json!(content)),"quotes":quotes});
            if let Some(cursor) = proposed.get("batchThroughMessageId") {
                descriptor["batchThroughMessageId"] = cursor.clone();
            }
            let source = json!({"type":"user-message","role":"user","conversationId":conversation,"messageId":quotes.last().unwrap()["messageId"]});
            memory::apply(
                "storage.memory.candidate",
                &json!({"value":descriptor,"source":source}),
            )?;
            let input = json!({"scope":record.0,"scopeId":record.1,"content":content,"kind":proposed["kind"],"source":source,"candidate":descriptor});
            let entry =
                json!({"scope":record.0,"scopeId":record.1,"source":source,"candidate":descriptor});
            memory_service::apply(
                "storage.memoryService.validateSource",
                &source_facts(root, catalog, &entry)?,
            )?;
            let fingerprints = record.3["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    (
                        entry["id"].as_str().unwrap().to_string(),
                        json!(candidate_fingerprint(&record.3["scope"], &entry["content"])),
                    )
                })
                .collect::<serde_json::Map<String, Value>>();
            let result = memory_repository::apply(
                "storage.repository.candidate",
                &json!({"document":record.3,"input":input,"id":gateway::new_id(),"now":gateway::now(),"candidateFingerprints":fingerprints}),
            )?;
            if result["changed"] == true {
                let mut next = result["document"].clone();
                next["revision"] = json!(record.3["revision"].as_u64().unwrap() + 1);
                memory::apply(
                    "storage.memory.document",
                    &json!({"value":next,"scope":record.0,"scopeId":record.1,"timestamps":gateway::timestamps(&next)}),
                )?;
                #[cfg(test)]
                {
                    let observer = CANDIDATE_COMMIT_OBSERVER
                        .lock()
                        .unwrap()
                        .as_ref()
                        .filter(|(owner, _)| owner == root)
                        .map(|(_, observer)| Arc::clone(observer));
                    if let Some(observer) = observer {
                        observer(&next);
                    }
                }
                let receipt = gateway::with_catalog(root, |current, _| {
                    gateway::verify_catalog_sources(catalog, current, &[conversation.to_string()])?;
                    let (_, current_path) =
                        location(root, current, &record.0, &record.1, Some(conversation))?;
                    if current_path != record.2 {
                        return Err(gateway::error(
                            "候选归属已改变。",
                            "MEMORY_SCOPE_CHANGED",
                            409,
                        ));
                    }
                    history.verify()?;
                    // A settings edit after capture invalidates this draft, including disabling while it is queued.
                    // 捕获后的设置修改使本次草稿失效，包括排队或提取过程中关闭自动候选。
                    let latest_user =
                        read_scope(root, &root.join("Memory/entries.json"), "user", "user")?.0;
                    if latest_user["candidateSettings"]["enabled"] == false {
                        return Ok(json!({"skippedReason":"disabled"}));
                    }
                    if latest_user["candidateSettingsRevision"]
                        .as_u64()
                        .unwrap_or(0)
                        != settings_revision
                    {
                        return Ok(json!({"skippedReason":"settings-changed"}));
                    }
                    gateway::commit_json(root, &record.2, &next, record.4.as_deref(), MAXIMUM_BYTES)
                })?;
                if let Some(reason) = receipt.get("skippedReason") {
                    return Ok(
                        json!({"created":created,"deduplicated":deduplicated,"reason":reason,"completeTurns":extracted["completeTurns"],"estimatedTokens":extracted["estimatedTokens"],"skippedOverlongMessages":extracted["skippedOverlongMessages"]}),
                    );
                }
                record.3 = next;
                record.4 = receipt["hash"].as_str().map(str::to_string);
                created += 1;
            } else {
                deduplicated += 1;
            }
        }
        gateway::with_catalog(root, |current, _| {
            gateway::verify_catalog_sources(catalog, current, &[conversation.to_string()])?;
            history.verify()
        })?;
        Ok(
            json!({"created":created,"deduplicated":deduplicated,"reason":extracted["reason"],"completeTurns":extracted["completeTurns"],"estimatedTokens":extracted["estimatedTokens"],"skippedOverlongMessages":extracted["skippedOverlongMessages"],"skippedTokenBudgetMessages":extracted["skippedTokenBudgetMessages"]}),
        )
    })()
}
pub use super::gateway::save_model_summary;
fn candidate_fingerprint(scope: &Value, content: &Value) -> String {
    let content = content
        .as_str()
        .unwrap_or("")
        .nfkc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([scope, content])).unwrap())
    )
}
fn proposal_fingerprint(
    scope: &Value,
    scope_id: &Value,
    content: &Value,
    kind: &Value,
    proposal: &Value,
) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([
                scope,
                scope_id,
                proposal["action"],
                content,
                kind,
                proposal["quotes"],
                proposal.get("target").unwrap_or(&Value::Null)
            ]))
            .unwrap()
        )
    )
}
pub fn entry_identity(entry: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([
                entry["id"],
                entry["scope"],
                entry["scopeId"],
                entry["revision"],
                entry["kind"],
                entry["content"],
                entry["source"],
                entry.get("candidate").unwrap_or(&Value::Null),
                entry.get("proposal").unwrap_or(&Value::Null)
            ]))
            .unwrap()
        )
    )
}
fn location(
    root: &Path,
    catalog: &Value,
    scope: &str,
    scope_id: &str,
    conversation: Option<&str>,
) -> Result<(String, PathBuf), super::ContractError> {
    memory::apply("storage.memory.scope", &json!({"value":scope}))?;
    match scope {
        "user" => Ok(("user".to_string(), root.join("Memory/entries.json"))),
        "project" => {
            let relationship = super::catalog::apply(
                "storage.catalog.project",
                &json!({"current":catalog,"id":scope_id}),
            )?;
            if relationship["isFolderlessWorkspace"] == true {
                return Err(gateway::error(
                    "无文件夹工作不提供共享记忆。",
                    "INVALID_MEMORY",
                    400,
                ));
            }
            Ok((
                relationship["projectId"].as_str().unwrap().to_string(),
                root.join("Projects")
                    .join(relationship["projectId"].as_str().unwrap())
                    .join("Memory/entries.json"),
            ))
        }
        "chat" => {
            let location = gateway::locate(catalog, conversation.unwrap_or(scope_id))?;
            Ok((
                location["Id"].as_str().unwrap().to_string(),
                super::conversation_logs::directory(root, &location)?.join("Memory/entries.json"),
            ))
        }
        _ => Err(gateway::error("记忆范围无效。", "INVALID_MEMORY", 400)),
    }
}
fn read_scope(
    root: &Path,
    path: &Path,
    scope: &str,
    scope_id: &str,
) -> Result<(Value, Option<String>), super::ContractError> {
    let (value, hash) = gateway::read_json(root, path, MAXIMUM_BYTES)?;
    let document=value.unwrap_or(json!({"schemaVersion":1,"scope":scope,"scopeId":scope_id,"revision":0,"entries":[],"dismissedSources":[]}));
    let normalized = memory::apply(
        "storage.memory.document",
        &json!({"value":document,"scope":scope,"scopeId":scope_id,"timestamps":gateway::timestamps(&document)}),
    )?;
    Ok((normalized, hash))
}
fn source_facts(root: &Path, catalog: &Value, entry: &Value) -> ContractResult {
    let mut sources = SourceReads::read(root, catalog, &[entry.clone()])?;
    let result = source_facts_cached(catalog, entry, &sources.users)?;
    sources.verify()?;
    Ok(result)
}

/// One operation groups quoted IDs by owning chat; only current user records cross into memory validation.
/// 单次操作按来源聊天合并引文身份，只将当前用户记录交给记忆校验，跳过巨大助手快照的正文解码。
struct SourceReads {
    users: HashMap<String, Value>,
    reads: Vec<(String, super::conversation_history::HistoryRead)>,
}
#[cfg(test)]
type SourceReadObserver = Arc<dyn Fn() + Send + Sync>;
#[cfg(test)]
static SOURCE_READ_OBSERVER: Mutex<Option<(PathBuf, SourceReadObserver)>> = Mutex::new(None);
impl SourceReads {
    fn read(root: &Path, catalog: &Value, entries: &[Value]) -> Result<Self, super::ContractError> {
        #[cfg(test)]
        {
            let observer = SOURCE_READ_OBSERVER
                .lock()
                .unwrap()
                .as_ref()
                .filter(|(owner, _)| owner == root)
                .map(|(_, observer)| Arc::clone(observer));
            if let Some(observer) = observer {
                observer();
            }
        }
        let mut ids: HashMap<String, HashSet<String>> = HashMap::new();
        for entry in entries
            .iter()
            .filter(|entry| entry["source"]["type"] != "manual")
        {
            let chat = entry["source"]["conversationId"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase();
            let messages = ids.entry(chat).or_default();
            if let Some(id) = entry["source"]["messageId"].as_str() {
                messages.insert(id.to_ascii_lowercase());
            }
            for quote in entry["candidate"]["quotes"]
                .as_array()
                .or_else(|| entry["proposal"]["quotes"].as_array())
                .into_iter()
                .flatten()
            {
                if let Some(id) = quote["messageId"].as_str() {
                    messages.insert(id.to_ascii_lowercase());
                }
            }
        }
        let mut result = Self {
            users: HashMap::new(),
            reads: Vec::new(),
        };
        for (chat, ids) in ids {
            let relationship = super::catalog::apply(
                "storage.catalog.relationship",
                &json!({"current":catalog,"id":chat}),
            );
            match relationship {
                Err(error) if [404, 410].contains(&error.status_code()) => continue,
                Err(error) => return Err(error),
                Ok(_) => {}
            }
            let read = super::conversation_history::read_latest(
                root,
                &gateway::locate(catalog, &chat)?,
                Some(&ids),
                true,
            )?;
            let mut users = serde_json::Map::new();
            for raw in &read.messages {
                let message: Value = serde_json::from_str(&super::raw_json::scalar_projection(raw))
                    .map_err(|_| {
                        gateway::error("记忆来源消息损坏。", "CORRUPT_CONVERSATION", 500)
                    })?;
                let message = gateway::normalize_message(&message)?;
                users.insert(
                    message["Id"].as_str().unwrap().to_ascii_lowercase(),
                    json!(serde_json::to_string(&message["Content"]).unwrap()),
                );
            }
            result.users.insert(chat.clone(), Value::Object(users));
            result.reads.push((chat, read));
        }
        Ok(result)
    }
    fn verify(&mut self) -> Result<(), super::ContractError> {
        for (_, read) in &mut self.reads {
            read.verify()?;
        }
        Ok(())
    }
    fn source_ids(&self) -> Vec<String> {
        self.reads.iter().map(|(id, _)| id.clone()).collect()
    }
}
fn source_facts_cached(
    catalog: &Value,
    entry: &Value,
    users: &HashMap<String, Value>,
) -> ContractResult {
    let source = &entry["source"];
    if source["type"] == "manual" {
        return Ok(json!({"entry":entry,"source":null}));
    }
    let id = source["conversationId"].as_str().unwrap_or("");
    let relationship = super::catalog::apply(
        "storage.catalog.relationship",
        &json!({"current":catalog,"id":id}),
    );
    let relationship = match relationship {
        Ok(value) => value,
        Err(error) if [404, 410].contains(&error.status_code()) => {
            return Ok(json!({"entry":entry,"source":null}));
        }
        Err(error) => return Err(error),
    };
    let users = users
        .get(&id.to_ascii_lowercase())
        .cloned()
        .unwrap_or(json!({}));
    let quotes = entry["candidate"]["quotes"]
        .as_array()
        .or_else(|| entry["proposal"]["quotes"].as_array())
        .cloned()
        .unwrap_or_default();
    Ok(
        json!({"entry":entry,"source":relationship,"usersRaw":users,"quotesRaw":quotes.iter().map(|quote|
        json!({"messageId":quote["messageId"],"textRaw":serde_json::to_string(&quote["text"]).unwrap()})).collect::<Vec<_>>()}),
    )
}
fn decorate(root: &Path, catalog: &Value, mut document: Value) -> ContractResult {
    let mut sources = SourceReads::read(root, catalog, document["entries"].as_array().unwrap())?;
    document = decorate_cached(catalog, document, &sources)?;
    sources.verify()?;
    Ok(document)
}
fn decorate_cached(catalog: &Value, mut document: Value, sources: &SourceReads) -> ContractResult {
    for entry in document["entries"].as_array_mut().unwrap() {
        let status = memory_service::apply(
            "storage.memoryService.sourceStatus",
            &source_facts_cached(catalog, entry, &sources.users)?,
        )?;
        entry
            .as_object_mut()
            .unwrap()
            .extend(status.as_object().unwrap().clone());
    }
    Ok(document)
}
type ScopeSnapshot = (Value, PathBuf, Option<String>);
fn decorate_snapshot(
    root: &Path,
    catalog: &Value,
    documents: &[ScopeSnapshot],
    conversation: Option<&str>,
) -> Result<Vec<Value>, super::ContractError> {
    let entries = documents
        .iter()
        .flat_map(|(document, _, _)| document["entries"].as_array().unwrap().iter().cloned())
        .collect::<Vec<_>>();
    let mut sources = SourceReads::read(root, catalog, &entries)?;
    let result = documents
        .iter()
        .map(|(document, _, _)| decorate_cached(catalog, document.clone(), &sources))
        .collect::<Result<Vec<_>, _>>()?;
    let mut ids = sources.source_ids();
    if let Some(chat) = conversation {
        ids.push(chat.to_string());
    }
    gateway::with_catalog(root, |current, _| {
        gateway::verify_catalog_sources(catalog, current, &ids)?;
        for (document, path, hash) in documents {
            let scope = document["scope"].as_str().unwrap();
            let scope_id = document["scopeId"].as_str().unwrap();
            let (_, current_path) = location(
                root,
                current,
                scope,
                scope_id,
                if scope == "chat" {
                    Some(scope_id)
                } else {
                    None
                },
            )?;
            if &current_path != path || gateway::read_json(root, path, MAXIMUM_BYTES)?.1 != *hash {
                return Err(gateway::error(
                    "记忆范围或版本已改变，请重新读取。",
                    "MEMORY_CONFLICT",
                    409,
                ));
            }
            if scope == "project" {
                let before = super::catalog::apply(
                    "storage.catalog.project",
                    &json!({"current":catalog,"id":scope_id}),
                )?;
                let after = super::catalog::apply(
                    "storage.catalog.project",
                    &json!({"current":current,"id":scope_id}),
                )?;
                if before["isArchived"] != after["isArchived"]
                    || before["isFolderlessWorkspace"] != after["isFolderlessWorkspace"]
                {
                    return Err(gateway::error(
                        "工作记忆范围已改变。",
                        "MEMORY_SCOPE_CHANGED",
                        409,
                    ));
                }
            }
        }
        sources.verify()?;
        Ok(())
    })?;
    Ok(result)
}
pub fn context(root: &Path, conversation_id: &str) -> ContractResult {
    let catalog = gateway::load_catalog(root)?;
    let relationship = super::catalog::apply(
        "storage.catalog.related",
        &json!({"current":&catalog,"id":conversation_id}),
    )?;
    let mut documents = Vec::new();
    for scope in relationship["memoryScopes"]
        .as_array()
        .cloned()
        .unwrap_or(json!(["chat", "user"]).as_array().unwrap().clone())
    {
        let scope = scope.as_str().unwrap();
        let id = if scope == "project" {
            relationship["projectId"].as_str().unwrap()
        } else {
            conversation_id
        };
        let (scope_id, path) = location(root, &catalog, scope, id, Some(conversation_id))?;
        let (document, hash) = read_scope(root, &path, scope, &scope_id)?;
        documents.push((document, path, hash));
    }
    let mut entries = Vec::new();
    for document in decorate_snapshot(root, &catalog, &documents, Some(conversation_id))? {
        entries.extend(
            document["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["status"] == "confirmed" && entry["active"] == true)
                .cloned(),
        );
    }
    Ok(
        json!({"conversationId":conversation_id,"projectId":relationship["projectId"],"isFolderlessWorkspace":relationship["isFolderlessWorkspace"],"entries":entries}),
    )
}
pub fn handle_tool(
    name: &str,
    input: &Value,
    context: &Value,
    root: &Path,
) -> Option<ContractResult> {
    if !["memory.read", "memory.propose"].contains(&name) {
        return None;
    }
    let operation = |catalog: &Value| {
        let chat = context["conversationId"]
            .as_str()
            .ok_or_else(|| gateway::error("缺少正式聊天身份。", "INVALID_MEMORY", 400))?;
        let relationship = super::catalog::apply(
            "storage.catalog.relationship",
            &json!({"current":catalog,"id":chat}),
        )?;
        let mut documents = Vec::new();
        for scope in ["chat", "project", "user"] {
            if scope == "project"
                && (relationship["projectId"].is_null()
                    || relationship["isFolderlessWorkspace"] == true
                    || relationship["projectArchived"] == true)
            {
                continue;
            }
            let id = if scope == "project" {
                relationship["projectId"].as_str().unwrap()
            } else {
                chat
            };
            let (scope_id, path) = location(root, catalog, scope, id, Some(chat))?;
            let (document, hash) = read_scope(root, &path, scope, &scope_id)?;
            documents.push((document, path, hash));
        }
        if name == "memory.read" {
            let page =
                memory_service::apply("storage.memoryService.queryInput", &json!({"input":input}))?;
            let query = input["query"]
                .as_str()
                .unwrap_or("")
                .nfkc()
                .collect::<String>()
                .to_lowercase();
            let mut records = Vec::new();
            let decorated_documents = decorate_snapshot(root, catalog, &documents, Some(chat))?;
            for (document, decorated) in documents
                .iter()
                .map(|(document, _, _)| document)
                .zip(&decorated_documents)
            {
                if input["scopes"]
                    .as_array()
                    .is_some_and(|scopes| !scopes.contains(&document["scope"]))
                {
                    continue;
                }
                for entry in decorated["entries"].as_array().unwrap() {
                    let own = entry["source"]["conversationId"]
                        .as_str()
                        .is_some_and(|id| id.eq_ignore_ascii_case(chat))
                        || entry["scope"] == "chat" && entry["scopeId"] == chat;
                    if entry["status"] != "confirmed" && !own {
                        continue;
                    }
                    let content = entry["content"]
                        .as_str()
                        .unwrap()
                        .nfkc()
                        .collect::<String>()
                        .to_lowercase();
                    let score = if query.is_empty() {
                        1.0
                    } else if content.contains(&query) {
                        2.0
                    } else {
                        let terms = crate::retrieval::lexical::collect_terms(&query, 64, false);
                        let found = terms
                            .iter()
                            .filter(|term| content.contains(term.as_str()))
                            .count();
                        found as f64 / terms.len().max(1) as f64
                    };
                    if score <= 0.0 {
                        continue;
                    }
                    let mut entry = entry.clone();
                    entry["target"] = json!({"id":entry["id"],"scope":document["scope"],"scopeId":document["scopeId"],"expectedRevision":document["revision"],"entryRevision":entry["revision"],"identity":entry_identity(&entry)});
                    if !own {
                        for field in ["source", "candidate", "proposal"] {
                            entry.as_object_mut().unwrap().remove(field);
                        }
                    }
                    records.push((score, entry));
                }
            }
            records.sort_by(|left, right| {
                right
                    .0
                    .total_cmp(&left.0)
                    .then_with(|| left.1["id"].as_str().cmp(&right.1["id"].as_str()))
            });
            let offset = page["offset"].as_u64().unwrap() as usize;
            let limit = page["limit"].as_u64().unwrap() as usize;
            let matched = records.len();
            let entries = records
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>();
            let next = offset + entries.len();
            return Ok(
                json!({"conversationId":chat,"projectId":relationship["projectId"],"entries":entries,
                "scopes":documents.iter().map(|(document,_,_)|json!({"scope":document["scope"],"scopeId":document["scopeId"],"revision":document["revision"]})).collect::<Vec<_>>(),
                "offset":offset,"hasMore":next<matched,"nextOffset":if next<matched{json!(next)}else{Value::Null},"matched":matched,"truncated":matched>limit,"correctnessCertified":false,"candidateSelection":"lexical-only-no-automatic-merge"}),
            );
        }
        let owner = candidate_owner(root)?;
        lifecycle(
            &owner,
            "storage.memoryLifecycle.proposalInput",
            json!({"input":input,"inputKeys":input.as_object().map(|input|input.keys().collect::<Vec<_>>()).unwrap_or_default()}),
        )?;
        if input["action"] == "noop" {
            return Ok(
                json!({"action":"noop","created":false,"reason":"no-change","requiresConfirmation":false,"correctnessCertified":false}),
            );
        }
        let user = documents
            .iter()
            .find(|(document, _, _)| document["scope"] == "user")
            .unwrap();
        let candidate_settings = memory::apply(
            "storage.memory.settings",
            &json!({"value":user.0.get("candidateSettings").unwrap_or(&json!({}))}),
        )?;
        if candidate_settings["enabled"] == false {
            return Ok(
                json!({"action":input["action"],"created":false,"reason":"disabled","requiresConfirmation":false}),
            );
        }
        let scope = input["scope"]
            .as_str()
            .or_else(|| input["target"]["scope"].as_str())
            .unwrap_or("chat");
        let (document, path, hash) = documents
            .iter()
            .find(|(document, _, _)| document["scope"] == scope)
            .ok_or_else(|| {
                gateway::error("建议不能跨越当前记忆范围。", "MEMORY_SCOPE_CHANGED", 409)
            })?;
        let target = input.get("target");
        lifecycle(
            &owner,
            "storage.memoryLifecycle.proposalScope",
            json!({"scope":scope,"document":document,"input":input,"target":target}),
        )?;
        let target_entry = target.and_then(|target| {
            document["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == target["id"])
        });
        let content = memory::apply(
            "storage.memory.content",
            &json!({"value":input.get("content").or_else(||target_entry.map(|entry|&entry["content"])).unwrap_or(&Value::Null)}),
        )?;
        let kind = memory::apply(
            "storage.memory.kind",
            &json!({"value":input.get("kind").or_else(||target_entry.map(|entry|&entry["kind"])).unwrap_or(&Value::Null),"scope":scope}),
        )?;
        let source = memory::apply(
            "storage.memory.source",
            &json!({"value":{"type":"user-message","role":"user","conversationId":chat,"messageId":input["quotes"].as_array().and_then(|quotes|quotes.last()).map(|quote|&quote["messageId"])}}),
        )?;
        let mut proposal = json!({"algorithm":"main-model-tool-v1","action":input["action"],"quotes":input["quotes"],"reason":input["reason"],"isInference":input["isInference"],"conflicts":input.get("conflicts").unwrap_or(&json!([])),"fingerprint":"0".repeat(64)});
        if let Some(target) = target {
            proposal["target"] = target.clone();
        }
        proposal = memory::apply(
            "storage.memory.proposal",
            &json!({"value":proposal,"source":source,"timestamps":{}}),
        )?;
        proposal["fingerprint"] = json!(proposal_fingerprint(
            &document["scope"],
            &document["scopeId"],
            &content,
            &kind,
            &proposal
        ));
        let pending = json!({"scope":scope,"scopeId":document["scopeId"],"content":content,"kind":kind,"source":source,"proposal":proposal});
        memory_service::apply(
            "storage.memoryService.validateSource",
            &source_facts(root, catalog, &pending)?,
        )?;
        let identities: serde_json::Map<String, Value> = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().unwrap().to_string(),
                    json!(entry_identity(entry)),
                )
            })
            .collect();
        let fingerprints: serde_json::Map<String, Value> = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().unwrap().to_string(),
                    json!(candidate_fingerprint(&document["scope"], &entry["content"])),
                )
            })
            .collect();
        let result = memory_repository::apply(
            "storage.repository.proposal",
            &json!({"document":document,"input":pending,"id":gateway::new_id(),"now":gateway::now(),"entryIdentities":identities,"candidateFingerprints":fingerprints,"contentFingerprint":candidate_fingerprint(&document["scope"],&content)}),
        )?;
        let mut next = result["document"].clone();
        if result["changed"] == true {
            next["revision"] = json!(document["revision"].as_u64().unwrap() + 1);
            gateway::commit_json(root, path, &next, hash.as_deref(), MAXIMUM_BYTES)?;
        }
        let entry = next["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == result["entryId"])
            .cloned()
            .unwrap_or(Value::Null);
        Ok(
            json!({"action":input["action"],"created":result["changed"],"reason":result["reason"],"scope":scope,"scopeId":document["scopeId"],"revision":next["revision"],"entry":entry,"requiresConfirmation":entry["status"]=="draft","sourceVerified":true,"correctnessCertified":false}),
        )
    };
    let result = if name == "memory.read" {
        gateway::load_catalog(root).and_then(|catalog| operation(&catalog))
    } else {
        gateway::with_catalog(root, |catalog, _| operation(catalog))
    };
    Some(result)
}

#[cfg(test)]
mod source_read_tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn deleting_chat_retires_its_queued_candidate_without_stopping_other_chats() {
        let root =
            std::env::temp_dir().join(format!("kynxa-retired-candidates-{}", gateway::new_id()));
        let deleted = gateway::new_id();
        let retained = gateway::new_id();
        let owner = candidate_owner(&root).unwrap();
        for id in [&deleted, &retained] {
            let admission =
                lifecycle(&owner, "storage.memoryLifecycle.schedule", json!({"id":id})).unwrap();
            assert_eq!(admission["admitted"], true);
            owner.queue.lock().unwrap().push_back((
                id.clone(),
                json!({}),
                admission["sequence"].as_u64().unwrap(),
            ));
        }
        assert_eq!(
            retire_chat_candidates(&root, &deleted.to_ascii_uppercase()).unwrap()["removed"],
            1
        );
        assert_eq!(owner.queue.lock().unwrap().front().unwrap().0, retained);
        assert!(!owner.stopped.load(Ordering::Acquire));
        assert_eq!(
            lifecycle(&owner, "storage.memoryLifecycle.status", json!({})).unwrap()["pendingTasks"],
            1
        );
        CANDIDATES.get().unwrap().lock().unwrap().remove(&root);
    }
    struct Fixture {
        root: PathBuf,
        old: String,
        current: String,
        user: String,
        path: PathBuf,
        entry: Value,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("kynxa-memory-source-{}", gateway::new_id()));
            std::fs::create_dir_all(&root).unwrap();
            let old = gateway::new_id();
            let current = gateway::new_id();
            let user = gateway::new_id();
            gateway::ensure_conversation(&root, &old, "旧长聊天").unwrap();
            gateway::ensure_conversation(&root, &current, "当前小聊天").unwrap();
            let catalog = gateway::load_catalog(&root).unwrap();
            let path = super::super::conversation_logs::directory(
                &root,
                &gateway::locate(&catalog, &old).unwrap(),
            )
            .unwrap()
            .join("events.jsonl");
            let mut writer = std::fs::File::create(&path).unwrap();
            let large = "x".repeat(2 * 1024 * 1024);
            let assistant = gateway::new_id();
            for _ in 0..12 {
                writeln!(writer,"{}",json!({"version":1,"type":"message.upsert","message":{"Id":assistant,"Role":"assistant","Status":"completed","Content":large}})).unwrap();
            }
            writeln!(writer,"{}",json!({"version":1,"type":"message.upsert","message":{"Id":user,"Role":"user","Status":"completed","Content":"以后用中文回答"}})).unwrap();
            writer.sync_all().unwrap();
            drop(writer);
            let now = gateway::now();
            let entry = json!({"id":gateway::new_id(),"scope":"user","scopeId":"user","content":"以后用中文回答","kind":"preference","status":"confirmed","revision":1,"createdAt":now,"updatedAt":now,
                "source":{"type":"user-message","role":"user","conversationId":old,"messageId":user},
                "candidate":{"algorithm":"conservative-extractive-v1","trigger":"remember","fingerprint":"a".repeat(64),"quotes":[{"messageId":user,"text":"以后用中文回答"}]}});
            gateway::commit_json(&root,&root.join("Memory/entries.json"),&json!({"schemaVersion":1,"scope":"user","scopeId":"user","revision":1,"entries":[entry],"dismissedSources":[]}),None,MAXIMUM_BYTES).unwrap();
            Self {
                root,
                old,
                current,
                user,
                path,
                entry,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let mut observer = SOURCE_READ_OBSERVER.lock().unwrap();
            if observer
                .as_ref()
                .is_some_and(|(root, _)| root == &self.root)
            {
                *observer = None;
            }
            drop(observer);
            stop_candidates(&self.root);
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn quoted_users_share_one_source_read_and_reject_changed_latest_record() {
        let fixture = Fixture::new();
        let catalog = gateway::load_catalog(&fixture.root).unwrap();
        let mut second = fixture.entry.clone();
        second["id"] = json!(gateway::new_id());
        let mut sources =
            SourceReads::read(&fixture.root, &catalog, &[fixture.entry.clone(), second]).unwrap();
        assert_eq!(sources.reads.len(), 1);
        assert_eq!(sources.reads[0].1.messages.len(), 1);
        assert!(sources.reads[0].1.messages[0].len() < 1024);
        let facts = source_facts_cached(&catalog, &fixture.entry, &sources.users).unwrap();
        assert_eq!(
            memory_service::apply("storage.memoryService.sourceStatus", &facts).unwrap()["active"],
            true
        );
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&fixture.path)
            .unwrap();
        writeln!(writer,"{}",json!({"version":1,"type":"message.upsert","message":{"Id":fixture.user,"Role":"user","Status":"completed","Content":"changed preference"}})).unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        assert!(sources.verify().is_err());
        drop(sources);
        let updated = SourceReads::read(&fixture.root, &catalog, &[fixture.entry.clone()]).unwrap();
        let facts = source_facts_cached(&catalog, &fixture.entry, &updated.users).unwrap();
        assert_eq!(
            memory_service::apply("storage.memoryService.sourceStatus", &facts).unwrap()["active"],
            false
        );
        assert_eq!(updated.reads[0].0, fixture.old);
    }
    #[test]
    fn paused_old_memory_read_does_not_block_catalog_or_small_chat() {
        let fixture = Fixture::new();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        *SOURCE_READ_OBSERVER.lock().unwrap() = Some((
            fixture.root.clone(),
            Arc::new(move || {
                entered_tx.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(15))
                    .unwrap();
            }),
        ));
        let root = fixture.root.clone();
        let current = fixture.current.clone();
        let old_reader = std::thread::spawn(move || context(&root, &current));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let root = fixture.root.clone();
        let chat = fixture.current.clone();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let foreground = std::thread::spawn(move || {
            let catalog = gateway::handle_text(
                "GET",
                "/api/conversations/catalog",
                &[("includeMessages".into(), "false".into())],
                &root,
            )
            .unwrap()
            .unwrap();
            assert_eq!(catalog.0, 200);
            let message = gateway::read_model_messages(&root, &chat).unwrap();
            assert!(message.is_empty());
            finished_tx.send(()).unwrap();
        });
        let ready = finished_rx.recv_timeout(std::time::Duration::from_secs(5));
        release_tx.send(()).unwrap();
        let result = old_reader.join().unwrap().unwrap();
        foreground.join().unwrap();
        assert!(
            ready.is_ok(),
            "metadata and the small chat must finish before releasing the old-source gate"
        );
        assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    }
}
pub fn handle(method: &str, path: &str, body: &Value, root: &Path) -> Option<GatewayResult> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.len() >= 3 && parts[..3] == ["api", "memory", "candidates"] {
        return Some(gateway::with_catalog(root, |_, _| {
            let path = root.join("Memory/entries.json");
            let (mut document, hash) = read_scope(root, &path, "user", "user")?;
            let owner = candidate_owner(root)?;
            lifecycle(
                &owner,
                "storage.memoryLifecycle.initialize",
                json!({"document":document}),
            )?;
            if method == "PATCH" && parts.len() == 4 && parts[3] == "settings" {
                let expected = lifecycle(
                    &owner,
                    "storage.memoryLifecycle.settingsInput",
                    json!({"input":body,"inputKeys":body.as_object().map(|fields|fields.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()}),
                )?;
                let patch = lifecycle(
                    &owner,
                    "storage.memoryLifecycle.mutateSettings",
                    json!({"document":document,"expected":expected,"patch":body["patch"]}),
                )?;
                document
                    .as_object_mut()
                    .unwrap()
                    .extend(patch.as_object().unwrap().clone());
                document["revision"] = json!(document["revision"].as_u64().unwrap() + 1);
                gateway::commit_json(root, &path, &document, hash.as_deref(), MAXIMUM_BYTES)?;
                lifecycle(
                    &owner,
                    "storage.memoryLifecycle.settingsSaved",
                    json!({"document":document}),
                )?;
            } else if method != "GET" {
                return Err(gateway::error("此方法不支持。", "METHOD_NOT_ALLOWED", 405));
            }
            lifecycle(&owner, "storage.memoryLifecycle.status", json!({}))
                .map(|status| (200, status))
        }));
    }
    let route = if parts.len() >= 3 && parts[..3] == ["api", "memory", "user"] {
        Some(("user", "user", parts.get(3).copied(), None))
    } else if parts.len() >= 4
        && parts[0] == "api"
        && parts[3] == "memory"
        && parts[1] == "projects"
    {
        Some(("project", parts[2], parts.get(4).copied(), None))
    } else if parts.len() >= 4
        && parts[0] == "api"
        && parts[3] == "memory"
        && parts[1] == "conversations"
    {
        Some(("chat", parts[2], parts.get(4).copied(), Some(parts[2])))
    } else {
        None
    };
    let (route_scope, route_id, entry_id, conversation) = route?;
    if method == "GET" && entry_id.is_none() {
        return Some((|| {
            if let Some(chat) = conversation {
                let catalog = gateway::load_catalog(root)?;
                let relation = super::catalog::apply(
                    "storage.catalog.related",
                    &json!({"current":&catalog,"id":chat}),
                )?;
                let mut documents = Vec::new();
                for scope in ["chat", "project", "user"] {
                    if scope == "project"
                        && (relation["projectId"].is_null()
                            || relation["isFolderlessWorkspace"] == true
                            || relation["projectArchived"] == true)
                    {
                        continue;
                    }
                    let id = if scope == "project" {
                        relation["projectId"].as_str().unwrap()
                    } else {
                        chat
                    };
                    let (scope_id, path) = location(root, &catalog, scope, id, Some(chat))?;
                    let (document, hash) = read_scope(root, &path, scope, &scope_id)?;
                    documents.push((document, path, hash));
                }
                let documents = decorate_snapshot(root, &catalog, &documents, Some(chat))?;
                Ok((
                    200,
                    json!({"conversationId":chat,"projectId":relation["projectId"],"isFolderlessWorkspace":relation["isFolderlessWorkspace"],"scopes":documents}),
                ))
            } else {
                read_index_document(root, route_scope, route_id).map(|document| (200, document))
            }
        })());
    }
    let result = gateway::with_catalog(root, |catalog, _| {
        let scope = if conversation.is_some() {
            body["scope"].as_str().unwrap_or("chat")
        } else {
            route_scope
        };
        if conversation.is_none() && body.get("scope").is_some_and(|value| value != scope) {
            return Err(gateway::error(
                "记忆范围须与接口一致。",
                "INVALID_MEMORY",
                400,
            ));
        }
        let scope_owner = if scope == "project" && conversation.is_some() {
            let relationship = super::catalog::apply(
                "storage.catalog.relationship",
                &json!({"current":catalog,"id":route_id}),
            )?;
            if relationship["projectArchived"] == true
                || relationship["isFolderlessWorkspace"] == true
            {
                return Err(gateway::error(
                    "此聊天未关联可共享记忆的工作。",
                    "INVALID_MEMORY",
                    400,
                ));
            }
            relationship["projectId"].as_str().unwrap_or("").to_string()
        } else {
            route_id.to_string()
        };
        let (scope_id, path) = location(root, catalog, scope, &scope_owner, conversation)?;
        let (document, hash) = read_scope(root, &path, scope, &scope_id)?;
        let expected = gateway::normalize_transport_numbers(memory::apply(
            "storage.memory.revision",
            &json!({"value":body["expectedRevision"],"required":true}),
        )?);
        if document["revision"] != expected {
            return Err(gateway::error(
                "记忆已更新，请重新读取后重试。",
                "MEMORY_CONFLICT",
                409,
            ));
        }
        let mut input = body.clone();
        input["scope"] = json!(scope);
        let operation = match (method, entry_id) {
            ("POST", None) => {
                if input.get("candidate").is_some()
                    || input.get("proposal").is_some()
                    || input
                        .get("status")
                        .is_some_and(|value| value != "confirmed")
                {
                    return Err(gateway::error(
                        "手动创建不能伪造自动候选。",
                        "INVALID_MEMORY",
                        400,
                    ));
                }
                input["content"] =
                    memory::apply("storage.memory.content", &json!({"value":input["content"]}))?;
                input["kind"] = memory::apply(
                    "storage.memory.kind",
                    &json!({"value":input["kind"],"scope":scope}),
                )?;
                let default = if let Some(id) = conversation {
                    json!({"type":"manual","role":"user","conversationId":id})
                } else {
                    json!({"type":"manual","role":"user"})
                };
                input["source"] = memory::apply(
                    "storage.memory.source",
                    &json!({"value":input.get("source").unwrap_or(&default)}),
                )?;
                if conversation.is_none() && input["source"]["type"] != "manual" {
                    return Err(gateway::error(
                        "独立记忆管理只接受手动来源。",
                        "INVALID_MEMORY",
                        400,
                    ));
                }
                if let Some(id) = conversation {
                    if input["source"]["conversationId"] != id {
                        return Err(gateway::error(
                            "记忆来源须属于当前聊天。",
                            "INVALID_MEMORY",
                            400,
                        ));
                    }
                }
                "create"
            }
            ("PATCH", Some(_)) => {
                memory::apply("storage.memory.updateInput", &json!({"value":input}))?;
                "update"
            }
            ("DELETE", Some(_)) => "delete",
            _ => {
                return Err(gateway::error(
                    "此记忆操作不支持。",
                    "METHOD_NOT_ALLOWED",
                    405,
                ));
            }
        };
        let id = entry_id.map(str::to_string).unwrap_or_else(gateway::new_id);
        if operation == "update" {
            let preflight = memory_repository::apply(
                "storage.repository.updatePreflight",
                &json!({"document":document,"input":input,"id":id}),
            )?;
            if preflight["requiresSourceValidation"] == true {
                memory_service::apply(
                    "storage.memoryService.validateSource",
                    &source_facts(root, catalog, &preflight["entry"])?,
                )?;
            }
        } else if operation == "create" && input["source"]["type"] == "user-message" {
            let entry = json!({"scope":scope,"scopeId":scope_id,"source":input["source"]});
            memory_service::apply(
                "storage.memoryService.validateSource",
                &source_facts(root, catalog, &entry)?,
            )?;
        }
        let fingerprints: serde_json::Map<String, Value> = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().unwrap().to_string(),
                    json!(candidate_fingerprint(&document["scope"], &entry["content"])),
                )
            })
            .collect();
        let identities: serde_json::Map<String, Value> = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().unwrap().to_string(),
                    json!(entry_identity(entry)),
                )
            })
            .collect();
        let mut arguments = json!({"document":document,"input":input,"id":id,"now":gateway::now(),"candidateFingerprints":fingerprints,"entryIdentities":identities,"contentFingerprint":candidate_fingerprint(&document["scope"],&input["content"])});
        if operation == "update" {
            if let Some(previous) = document["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| {
                    entry["id"] == id && entry["status"] == "draft" && entry["proposal"].is_object()
                })
            {
                arguments["updatedProposalFingerprint"] = json!(proposal_fingerprint(
                    &document["scope"],
                    &document["scopeId"],
                    input.get("content").unwrap_or(&previous["content"]),
                    input.get("kind").unwrap_or(&previous["kind"]),
                    &previous["proposal"]
                ));
            }
        }
        let planned =
            memory_repository::apply(&format!("storage.repository.{operation}"), &arguments)?;
        let mut next = planned["document"].clone();
        if planned["changed"] == true {
            next["revision"] = json!(document["revision"].as_u64().unwrap() + 1);
            memory::apply(
                "storage.memory.document",
                &json!({"value":next,"scope":scope,"scopeId":scope_id,"timestamps":gateway::timestamps(&next)}),
            )?;
            gateway::commit_json(root, &path, &next, hash.as_deref(), MAXIMUM_BYTES)?;
        }
        decorate(root, catalog, next).map(|value| (if method == "POST" { 201 } else { 200 }, value))
    });
    if ["POST", "PATCH", "DELETE"].contains(&method) {
        if let Ok((_, document)) = &result {
            if let Some(callback) = INDEXERS
                .get()
                .and_then(|owners| owners.lock().ok())
                .and_then(|owners| owners.get(root).cloned())
            {
                callback(document);
            }
        }
    }
    Some(result)
}

#[cfg(test)]
mod candidate_transition_tests {
    use super::*;
    use std::{
        io::Write,
        time::{Duration, Instant},
    };

    struct Fixture {
        root: PathBuf,
        chat: String,
        log: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("kynxa-memory-transitions-{}", gateway::new_id()));
            let chat = gateway::new_id();
            gateway::ensure_conversation(&root, &chat, "合成记忆回归").unwrap();
            let catalog = gateway::load_catalog(&root).unwrap();
            let log = super::super::conversation_logs::directory(
                &root,
                &gateway::locate(&catalog, &chat).unwrap(),
            )
            .unwrap()
            .join("events.jsonl");
            Self { root, chat, log }
        }
        fn append(&self, role: &str, content: &str) -> String {
            let id = gateway::new_id();
            let mut writer = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log)
                .unwrap();
            writeln!(writer, "{}", json!({"version":1,"type":"message.upsert","message":{"Id":id,"Role":role,"Status":"completed","Content":content}})).unwrap();
            writer.sync_all().unwrap();
            id
        }
        fn turns(&self, start: usize, count: usize) -> Vec<String> {
            (start..start + count)
                .map(|index| {
                    let id = self.append("user", &format!("我喜欢第 {index} 种展示约定。"));
                    self.append("assistant", "已收到这条合成讨论。");
                    id
                })
                .collect()
        }
        fn document(&self) -> Value {
            read_index_document(&self.root, "chat", &self.chat).unwrap()
        }
        fn wait(&self) -> Value {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let status = candidate_status(&self.root).unwrap();
                if status["pendingTasks"] == 0 {
                    return status;
                }
                assert!(
                    Instant::now() < deadline,
                    "candidate did not settle: {status}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        fn capture(&self) -> Value {
            capture_candidates(
                &self.root,
                &self.chat,
                &json!({"phase":"completed","userMessageId":null}),
                &candidate_owner(&self.root).unwrap(),
            )
            .unwrap()
        }
        fn patch(&self, id: &str, revision: &Value, patch: Value) -> Value {
            let mut body = patch;
            body["expectedRevision"] = revision.clone();
            handle(
                "PATCH",
                &format!("/api/conversations/{}/memory/{id}", self.chat),
                &body,
                &self.root,
            )
            .unwrap()
            .unwrap()
            .1
        }
        fn delete(&self, id: &str, revision: &Value) -> Value {
            handle(
                "DELETE",
                &format!("/api/conversations/{}/memory/{id}", self.chat),
                &json!({"expectedRevision":revision}),
                &self.root,
            )
            .unwrap()
            .unwrap()
            .1
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            stop_candidates(&self.root);
            self.wait();
            let mut observer = CANDIDATE_COMMIT_OBSERVER.lock().unwrap();
            if observer
                .as_ref()
                .is_some_and(|(root, _)| root == &self.root)
            {
                *observer = None;
            }
            drop(observer);
            if let Some(owners) = CANDIDATES.get() {
                owners.lock().unwrap().remove(&self.root);
            }
            if let Some(owners) = INDEXERS.get() {
                owners.lock().unwrap().remove(&self.root);
            }
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn saved_assistant_completion_creates_draft_then_confirmation_crud_preserves_batch_dismissals()
    {
        let fixture = Fixture::new();
        let mut users = fixture.turns(0, 11);
        let final_user = gateway::new_id();
        gateway::upsert_message(&fixture.root, &fixture.chat, &json!({"Id":final_user,"Role":"user","Status":"completed","Content":"我喜欢第 11 种展示约定。"})).unwrap();
        fixture.wait();
        assert!(fixture.document()["entries"].as_array().unwrap().is_empty());
        let assistant = gateway::new_id();
        gateway::upsert_message(&fixture.root, &fixture.chat, &json!({"Id":assistant,"Role":"assistant","Status":"streaming","Content":"先完成合成答复。"})).unwrap();
        assert!(fixture.document()["entries"].as_array().unwrap().is_empty());
        gateway::upsert_message(&fixture.root, &fixture.chat, &json!({"Id":assistant,"Role":"assistant","Status":"completed","Content":"已完成合成答复。"})).unwrap();
        let settled = fixture.wait();
        assert_eq!(settled["lastResult"]["created"], 1, "{settled}");
        users.push(final_user.clone());
        let draft = fixture.document();
        let entry = &draft["entries"][0];
        assert_eq!(entry["status"], "draft");
        assert_eq!(entry["scopeId"], fixture.chat);
        assert_eq!(entry["source"]["messageId"], final_user);
        assert_eq!(entry["candidate"]["batchThroughMessageId"], final_user);
        assert_eq!(entry["candidate"]["quotes"].as_array().unwrap().len(), 12);
        assert!(
            context(&fixture.root, &fixture.chat).unwrap()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let id = entry["id"].as_str().unwrap();
        let confirmed = fixture.patch(id, &draft["revision"], json!({"status":"confirmed"}));
        assert_eq!(
            context(&fixture.root, &fixture.chat).unwrap()["entries"][0]["id"],
            id
        );
        let stale = handle(
            "PATCH",
            &format!("/api/conversations/{}/memory/{id}", fixture.chat),
            &json!({"expectedRevision":draft["revision"],"content":"旧表单不能覆盖"}),
            &fixture.root,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(stale.code(), "MEMORY_CONFLICT");
        let edited = fixture.patch(
            id,
            &confirmed["revision"],
            json!({"content":"用户编辑后的约定"}),
        );
        assert_eq!(edited["entries"][0]["content"], "用户编辑后的约定");
        let deleted = fixture.delete(id, &edited["revision"]);
        assert!(deleted["entries"].as_array().unwrap().is_empty());
        assert_eq!(deleted["dismissedSources"].as_array().unwrap().len(), 12);
        assert!(
            deleted["dismissedSources"]
                .as_array()
                .unwrap()
                .iter()
                .all(|source| source["candidateBatchThroughMessageId"] == final_user)
        );
        assert_eq!(fixture.capture()["created"], 0);
        assert!(fixture.document()["entries"].as_array().unwrap().is_empty());
        let next_users = fixture.turns(12, 12);
        assert_eq!(fixture.capture()["created"], 1);
        let next = fixture.document();
        let quotes = next["entries"][0]["candidate"]["quotes"]
            .as_array()
            .unwrap();
        assert_eq!(quotes.len(), 12);
        assert!(quotes.iter().all(|quote| {
            next_users.contains(&quote["messageId"].as_str().unwrap().to_string())
                && !users.contains(&quote["messageId"].as_str().unwrap().to_string())
        }));
    }

    #[test]
    fn capture_rechecks_disabled_and_changed_settings_before_draft_commit() {
        for (patch, reason) in [
            (json!({"enabled":false}), "disabled"),
            (json!({"maxCandidates":4}), "settings-changed"),
        ] {
            let fixture = Fixture::new();
            fixture.turns(0, 12);
            let root = fixture.root.clone();
            *CANDIDATE_COMMIT_OBSERVER.lock().unwrap() = Some((
                root.clone(),
                Arc::new(move |_| {
                    handle(
                        "PATCH",
                        "/api/memory/candidates/settings",
                        &json!({"expectedRevision":0,"patch":patch}),
                        &root,
                    )
                    .unwrap()
                    .unwrap();
                }),
            ));
            let result = fixture.capture();
            assert_eq!(result["created"], 0);
            assert_eq!(result["reason"], reason);
            assert!(fixture.document()["entries"].as_array().unwrap().is_empty());
            assert_eq!(
                candidate_status(&fixture.root).unwrap()["settingsRevision"],
                1
            );
        }

        let fixture = Fixture::new();
        fixture.turns(0, 12);
        let root = fixture.root.clone();
        let chat = fixture.chat.clone();
        let project = gateway::new_id();
        std::fs::create_dir(root.join("work")).unwrap();
        let moved = Arc::new(Mutex::new(None));
        let move_receipt = moved.clone();
        *CANDIDATE_COMMIT_OBSERVER.lock().unwrap() = Some((
            root.clone(),
            Arc::new(move |_| {
                let root = root.clone();
                let chat = chat.clone();
                let project = project.clone();
                // Pause capture after extraction and use another thread's real catalog transaction.
                // 在提取完成后暂停捕获，由另一个线程执行正式目录移动，不能靠等候读取线程退出消除占用。
                let receipt = std::thread::spawn(move || {
                    let mut catalog = gateway::load_catalog(&root)?;
                    let chats = catalog["Chats"].as_array_mut().unwrap();
                    let position = chats
                        .iter()
                        .position(|record| record["Id"] == chat)
                        .unwrap();
                    let record = chats.remove(position);
                    catalog["Projects"].as_array_mut().unwrap().push(json!({
                        "Id":project,"Name":"移动目标","FolderPath":root.join("work"),
                        "IsFolderlessWorkspace":false,"IsArchived":false,"IsPinned":false,
                        "Chats":[record]
                    }));
                    gateway::handle(
                        "PUT",
                        "/api/conversations/catalog",
                        &[("includeMessages".into(), "false".into())],
                        &catalog,
                        &root,
                    )
                    .unwrap()
                })
                .join()
                .unwrap();
                *move_receipt.lock().unwrap() = Some(receipt);
            }),
        ));
        let capture = capture_candidates(
            &fixture.root,
            &fixture.chat,
            &json!({"phase":"completed","userMessageId":null}),
            &candidate_owner(&fixture.root).unwrap(),
        );
        let receipt = moved.lock().unwrap().take().unwrap().unwrap();
        assert_eq!(
            receipt.0, 200,
            "the in-flight capture must not pin its source directory"
        );
        assert_eq!(capture.unwrap_err().code(), "WORKSPACE_CHANGED");
        assert!(fixture.document()["entries"].as_array().unwrap().is_empty());
        assert_eq!(
            gateway::read_model_messages(&fixture.root, &fixture.chat)
                .unwrap()
                .len(),
            24
        );
    }

    #[test]
    fn overlong_source_is_skipped_without_truncation_or_losing_other_batch_quotes() {
        let fixture = Fixture::new();
        fixture.append("user", &format!("我喜欢{}", "长".repeat(4000)));
        fixture.append("assistant", "已收到超长合成输入。");
        let valid = fixture.turns(1, 11);
        let result = fixture.capture();
        assert_eq!(result["created"], 1);
        assert_eq!(result["skippedOverlongMessages"], 1);
        let document = fixture.document();
        let quotes = document["entries"][0]["candidate"]["quotes"]
            .as_array()
            .unwrap();
        assert_eq!(quotes.len(), 11);
        assert!(
            quotes
                .iter()
                .all(|quote| valid.contains(&quote["messageId"].as_str().unwrap().to_string()))
        );
    }

    #[test]
    fn changed_user_quote_cannot_confirm_the_previous_candidate() {
        let fixture = Fixture::new();
        let users = fixture.turns(0, 12);
        assert_eq!(fixture.capture()["created"], 1);
        let document = fixture.document();
        gateway::upsert_message(&fixture.root, &fixture.chat, &json!({"Id":users[0],"Role":"user","Status":"completed","Content":"已更改的原话不再支持旧候选。"})).unwrap();
        fixture.wait();
        let entry = &document["entries"][0];
        let error = handle(
            "PATCH",
            &format!(
                "/api/conversations/{}/memory/{}",
                fixture.chat,
                entry["id"].as_str().unwrap()
            ),
            &json!({"expectedRevision":document["revision"],"status":"confirmed"}),
            &fixture.root,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code(), "MEMORY_SOURCE_UNAVAILABLE");
        let current = fixture.document();
        assert_eq!(current["revision"], document["revision"]);
        assert_eq!(current["entries"][0]["status"], "draft");
        assert_eq!(current["entries"][0]["sourceAvailable"], false);
    }

    #[test]
    fn explicit_scope_shares_only_authorized_work_and_pauses_after_source_moves() {
        let mut fixture = Fixture::new();
        fixture.append("user", "已有合成聊天。");
        let first_work = gateway::new_id();
        let second_work = gateway::new_id();
        let sibling = gateway::new_id();
        let foreign = gateway::new_id();
        for folder in ["first-work", "second-work"] {
            std::fs::create_dir_all(fixture.root.join(folder)).unwrap();
        }
        let original = gateway::load_catalog(&fixture.root).unwrap();
        gateway::handle("PUT", "/api/conversations/catalog", &[], &json!({"Revision":original["Revision"],"Chats":[],"Projects":[
            {"Id":first_work,"Name":"合成工作一","FolderPath":fixture.root.join("first-work"),"Chats":[
                {"Id":fixture.chat,"Title":"合成来源","Messages":[]},
                {"Id":sibling,"Title":"合成同工作聊天","Messages":[{"Id":gateway::new_id(),"Role":"user","Content":"同工作合成输入"}]}]},
            {"Id":second_work,"Name":"合成工作二","FolderPath":fixture.root.join("second-work"),"Chats":[
                {"Id":foreign,"Title":"合成其他工作聊天","Messages":[{"Id":gateway::new_id(),"Role":"user","Content":"其他工作合成输入"}]}]}
        ]}), &fixture.root).unwrap().unwrap();
        let catalog = gateway::load_catalog(&fixture.root).unwrap();
        fixture.log = super::super::conversation_logs::directory(
            &fixture.root,
            &gateway::locate(&catalog, &fixture.chat).unwrap(),
        )
        .unwrap()
        .join("events.jsonl");
        let mut entries = HashMap::new();
        for (text, scope) in [
            ("记住：共享工作约定", "project"),
            ("全局记住：跨工作偏好", "user"),
            ("聊天记住：私有聊天约定", "chat"),
        ] {
            let message = fixture.append("user", text);
            let saved =
                capture_explicit_instruction(&fixture.root, &fixture.chat, &message).unwrap();
            assert_eq!(saved["scope"], scope);
            entries.insert(scope, saved["entry"]["id"].clone());
        }
        let same_work = context(&fixture.root, &sibling).unwrap();
        let same_entries = same_work["entries"].as_array().unwrap();
        assert_eq!(same_entries.len(), 2);
        assert!(
            same_entries
                .iter()
                .any(|entry| entry["id"] == entries["project"])
        );
        assert!(
            same_entries
                .iter()
                .any(|entry| entry["id"] == entries["user"])
        );
        let other_work = context(&fixture.root, &foreign).unwrap();
        assert_eq!(other_work["entries"].as_array().unwrap().len(), 1);
        assert_eq!(other_work["entries"][0]["id"], entries["user"]);
        let mut moved = gateway::handle(
            "GET",
            "/api/conversations/catalog",
            &[("includeMessages".into(), "false".into())],
            &Value::Null,
            &fixture.root,
        )
        .unwrap()
        .unwrap()
        .1;
        let index = moved["Projects"][0]["Chats"]
            .as_array()
            .unwrap()
            .iter()
            .position(|chat| chat["Id"] == fixture.chat)
            .unwrap();
        let source = moved["Projects"][0]["Chats"]
            .as_array_mut()
            .unwrap()
            .remove(index);
        moved["Projects"][1]["Chats"]
            .as_array_mut()
            .unwrap()
            .push(source);
        gateway::handle(
            "PUT",
            "/api/conversations/catalog",
            &[],
            &moved,
            &fixture.root,
        )
        .unwrap()
        .unwrap();
        let old_work = read_index_document(&fixture.root, "project", &first_work).unwrap();
        assert_eq!(old_work["entries"][0]["active"], false);
        assert_eq!(old_work["entries"][0]["sourceAvailable"], true);
        let remaining = context(&fixture.root, &sibling).unwrap();
        assert_eq!(remaining["entries"].as_array().unwrap().len(), 1);
        assert_eq!(remaining["entries"][0]["id"], entries["user"]);
        let moved_chat = context(&fixture.root, &fixture.chat).unwrap();
        assert_eq!(moved_chat["entries"].as_array().unwrap().len(), 2);
        assert!(
            moved_chat["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == entries["chat"])
        );
    }

    #[test]
    fn legacy_remember_draft_upgrades_only_after_rechecking_the_saved_instruction() {
        let fixture = Fixture::new();
        let user = fixture.append("user", "聊天记住：旧草稿😀内容");
        assert_eq!(fixture.capture()["created"], 1);
        let before = fixture.document();
        assert_eq!(before["entries"][0]["status"], "draft");
        assert_eq!(before["entries"][0]["candidate"]["trigger"], "remember");
        let saved = capture_explicit_instruction(&fixture.root, &fixture.chat, &user).unwrap();
        assert_eq!(saved["created"], true);
        assert_eq!(saved["entry"]["id"], before["entries"][0]["id"]);
        assert_eq!(saved["entry"]["revision"], 2);
        assert_eq!(saved["entry"]["status"], "confirmed");
        assert_eq!(saved["entry"]["content"], "旧草稿😀内容");
        assert_eq!(
            saved["entry"]["candidate"]["quotes"][0]["text"],
            "聊天记住：旧草稿😀内容"
        );
        assert_eq!(fixture.capture()["created"], 0);
        assert_eq!(fixture.document()["entries"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn explicit_instruction_reads_saved_utf16_source_and_stays_confirmed_when_candidates_disabled()
    {
        let fixture = Fixture::new();
        handle(
            "PATCH",
            "/api/memory/candidates/settings",
            &json!({"expectedRevision":0,"patch":{"enabled":false}}),
            &fixture.root,
        )
        .unwrap()
        .unwrap();
        let ordinary = fixture.append("user", "普通讨论里提到记住：不会自动确认。");
        assert_eq!(
            capture_explicit_instruction(&fixture.root, &fixture.chat, &ordinary).unwrap()["reason"],
            "no-trigger"
        );
        let user = fixture.append("user", "  聊天记住：  中文😀偏好  ");
        let callbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = callbacks.clone();
        set_indexer(
            &fixture.root,
            Arc::new(move |document| {
                assert!(
                    document["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|entry| entry["status"] == "confirmed")
                );
                observed.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let saved = capture_explicit_instruction(&fixture.root, &fixture.chat, &user).unwrap();
        assert_eq!(saved["created"], true);
        assert_eq!(saved["entry"]["content"], "中文😀偏好");
        assert_eq!(saved["entry"]["status"], "confirmed");
        assert_eq!(saved["entry"]["source"]["messageId"], user);
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
        assert_eq!(
            capture_explicit_instruction(&fixture.root, &fixture.chat, &user).unwrap()["created"],
            false
        );
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
        let deleted = fixture.delete(saved["entry"]["id"].as_str().unwrap(), &saved["revision"]);
        assert!(deleted["entries"].as_array().unwrap().is_empty());
        assert_eq!(
            capture_explicit_instruction(&fixture.root, &fixture.chat, &user).unwrap()["created"],
            false
        );
        assert_eq!(
            candidate_status(&fixture.root).unwrap()["settings"]["enabled"],
            false
        );
        CANDIDATES
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .remove(&fixture.root);
        let restarted = candidate_status(&fixture.root).unwrap();
        assert_eq!(restarted["settings"]["enabled"], false);
        assert_eq!(restarted["settingsRevision"], 1);
        assert!(
            capture_explicit_instruction(
                &fixture.root,
                &fixture.chat,
                &fixture.append("assistant", "记住：助手不能授权")
            )
            .is_err()
        );
    }

    #[test]
    fn update_and_delete_proposals_require_explicit_action_and_fresh_target_identity() {
        let fixture = Fixture::new();
        let original = fixture.append("user", "聊天记住：初始约定");
        let created =
            capture_explicit_instruction(&fixture.root, &fixture.chat, &original).unwrap();
        let target_id = created["entry"]["id"].as_str().unwrap().to_string();
        for action in ["update", "delete"] {
            let quote = if action == "update" {
                "请把约定改成新约定。"
            } else {
                "请删除这条约定。"
            };
            let message = fixture.append("user", quote);
            let read = handle_tool(
                "memory.read",
                &json!({"scopes":["chat"]}),
                &json!({"conversationId":fixture.chat}),
                &fixture.root,
            )
            .unwrap()
            .unwrap();
            let target = read["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == target_id)
                .unwrap()["target"]
                .clone();
            let input = json!({"action":action,"scope":"chat","content":"新约定","quotes":[{"messageId":message,"text":quote}],"reason":"用户提出修改动作","isInference":false,"target":target});
            let proposal = handle_tool(
                "memory.propose",
                &input,
                &json!({"conversationId":fixture.chat}),
                &fixture.root,
            )
            .unwrap()
            .unwrap();
            assert_eq!(proposal["entry"]["status"], "draft");
            let id = proposal["entry"]["id"].as_str().unwrap();
            let wrong = handle(
                "PATCH",
                &format!("/api/conversations/{}/memory/{id}", fixture.chat),
                &json!({"expectedRevision":proposal["revision"],"status":"confirmed"}),
                &fixture.root,
            )
            .unwrap()
            .unwrap_err();
            assert_eq!(wrong.code(), "MEMORY_PROPOSAL_CONFIRMATION_REQUIRED");
            if action == "delete" {
                let changed = fixture.patch(
                    &target_id,
                    &proposal["revision"],
                    json!({"content":"用户刚修改的目标"}),
                );
                let stale = handle("PATCH", &format!("/api/conversations/{}/memory/{id}", fixture.chat), &json!({"expectedRevision":changed["revision"],"status":"confirmed","proposalAction":"delete"}), &fixture.root).unwrap().unwrap_err();
                assert_eq!(stale.code(), "MEMORY_TARGET_CHANGED");
                fixture.delete(id, &changed["revision"]);
                let latest = handle_tool(
                    "memory.read",
                    &json!({"scopes":["chat"]}),
                    &json!({"conversationId":fixture.chat}),
                    &fixture.root,
                )
                .unwrap()
                .unwrap();
                let fresh_message = fixture.append("user", "确认删除最新约定。");
                let fresh_input = json!({"action":"delete","scope":"chat","content":latest["entries"][0]["content"],"quotes":[{"messageId":fresh_message,"text":"确认删除最新约定。"}],"reason":"用户重新确认最新目标","isInference":false,"target":latest["entries"][0]["target"]});
                let fresh = handle_tool(
                    "memory.propose",
                    &fresh_input,
                    &json!({"conversationId":fixture.chat}),
                    &fixture.root,
                )
                .unwrap()
                .unwrap();
                let deleted = fixture.patch(
                    fresh["entry"]["id"].as_str().unwrap(),
                    &fresh["revision"],
                    json!({"status":"confirmed","proposalAction":"delete"}),
                );
                assert!(deleted["entries"].as_array().unwrap().is_empty());
                assert_eq!(
                    handle_tool(
                        "memory.propose",
                        &fresh_input,
                        &json!({"conversationId":fixture.chat}),
                        &fixture.root
                    )
                    .unwrap()
                    .unwrap()["created"],
                    false
                );
                continue;
            }
            let applied = fixture.patch(
                id,
                &proposal["revision"],
                json!({"status":"confirmed","proposalAction":action}),
            );
            assert_eq!(applied["entries"].as_array().unwrap().len(), 1);
            assert_eq!(applied["entries"][0]["id"], target_id);
            assert_eq!(applied["entries"][0]["content"], "新约定");
            assert!(applied["entries"][0]["proposal"]["appliedAt"].is_string());
            assert_eq!(
                handle_tool(
                    "memory.propose",
                    &input,
                    &json!({"conversationId":fixture.chat}),
                    &fixture.root
                )
                .unwrap()
                .unwrap()["created"],
                false
            );
        }
    }
}
