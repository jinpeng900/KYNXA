//! Settings, source registration and persistent indexing jobs share the existing data layout.
//! 配置、资料登记与持久索引任务沿用现有数据布局。

use super::gateway_index;
pub use super::gateway_tools::{
    final_validate, handle_tool, prepare_request, project_evidence_result, release_task,
};
use crate::gateway_contract::GatewayConfig;
use crate::storage::{
    self, ContractError,
    gateway::{self, GatewayResult},
    jobs, library, settings,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

static CANCELLATIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
static BACKGROUND_GATES: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<Mutex<()>>>>> =
    OnceLock::new();
struct BackgroundOwner {
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}
static BACKGROUND: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<BackgroundOwner>>>> =
    OnceLock::new();
struct PendingMemory {
    scope: String,
    id: String,
    ready_at: std::time::Instant,
    attempt: u8,
}
static MEMORY_PENDING: OnceLock<Mutex<HashMap<std::path::PathBuf, Vec<PendingMemory>>>> =
    OnceLock::new();
fn schedule_memory(root: &Path, document: &Value) {
    let Some(scope) = document["scope"].as_str() else {
        return;
    };
    let Some(id) = document["scopeId"].as_str() else {
        return;
    };
    let mut pending = MEMORY_PENDING.get_or_init(Default::default).lock().unwrap();
    let scopes = pending.entry(root.to_path_buf()).or_default();
    if let Some(entry) = scopes
        .iter_mut()
        .find(|entry| entry.scope == scope && entry.id == id)
    {
        entry.ready_at = std::time::Instant::now();
        entry.attempt = 0;
        return;
    }
    if scopes.len() < 128 {
        scopes.push(PendingMemory {
            scope: scope.to_string(),
            id: id.to_string(),
            ready_at: std::time::Instant::now(),
            attempt: 0,
        });
    }
}
fn temporary_index_failure(code: &str) -> bool {
    matches!(
        code,
        "RESOURCE_WAIT_TIMEOUT"
            | "RESOURCE_RUNTIME_UNAVAILABLE"
            | "INFERENCE_RUNTIME_UNAVAILABLE"
            | "INFERENCE_RUNTIME_NOT_INITIALIZED"
            | "INFERENCE_RESOURCE_BUSY"
            | "INFERENCE_SESSION_BUDGET_FULL"
            | "INFERENCE_TOKENIZER_BUDGET_FULL"
            | "INFERENCE_SESSION_RETIRED"
            | "INFERENCE_GPU_AUDIT_RESOURCE_REQUIRED"
            | "INFERENCE_RESERVATION_FAILED"
            | "RETRIEVAL_JOB_CAPACITY"
            | "RETRIEVAL_INDEX_BUSY"
            | "SQLITE_BUSY"
            | "INDEX_JOB_INTERRUPTED"
            | "EMBEDDING_UNAVAILABLE"
    )
}
fn retry_delay(attempt: u8) -> std::time::Duration {
    std::time::Duration::from_secs((30u64 << attempt.min(5)).min(900))
}
fn enqueue_startup_memory(root: &Path, pending_scopes: &mut VecDeque<Value>) {
    let mut owners = MEMORY_PENDING.get_or_init(Default::default).lock().unwrap();
    let pending = owners.entry(root.to_path_buf()).or_default();
    while pending.len() < 128 {
        let Some(value) = pending_scopes.pop_front() else {
            break;
        };
        let Some(scope) = value["scope"].as_str() else {
            continue;
        };
        let Some(id) = value["scopeId"].as_str() else {
            continue;
        };
        if !pending
            .iter()
            .any(|entry| entry.scope == scope && entry.id == id)
        {
            pending.push(PendingMemory {
                scope: scope.into(),
                id: id.into(),
                ready_at: std::time::Instant::now(),
                attempt: 0,
            });
        }
    }
}
struct MaintenanceAttempt {
    signature: String,
    attempt: u8,
    semantic_only: bool,
    coverage: Value,
}
fn maintenance_attempt(
    jobs: &[Value],
    project: Option<&str>,
    signature: &str,
    changed: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<MaintenanceAttempt> {
    if jobs.iter().any(|job| {
        job["projectId"] == json!(project)
            && (job["automaticRebuildBlocked"] == true
                || ["queued", "running", "paused"].contains(&job["status"].as_str().unwrap_or("")))
    }) {
        return None;
    }
    let previous = jobs
        .iter()
        .rev()
        .find(|job| job["projectId"] == json!(project));
    let retry = previous.filter(|job| {
        job["automaticRebuildScope"] == signature
            && matches!(job["status"].as_str(), Some("partial" | "failed"))
            && (job["error"].as_str().is_some_and(temporary_index_failure)
                || job["semantic"]["diagnosticCodes"]
                    .as_array()
                    .is_some_and(|codes| {
                        codes
                            .iter()
                            .filter_map(Value::as_str)
                            .any(temporary_index_failure)
                    }))
    });
    if let Some(previous) = retry {
        let attempt = previous["automaticRetryAttempt"]
            .as_u64()
            .unwrap_or(0)
            .min(10) as u8;
        let finished =
            chrono::DateTime::parse_from_rfc3339(previous["finishedAt"].as_str()?).ok()?;
        if now
            < finished.with_timezone(&chrono::Utc)
                + chrono::Duration::from_std(retry_delay(attempt)).ok()?
        {
            return None;
        }
        return Some(MaintenanceAttempt {
            signature: signature.into(),
            attempt: attempt.saturating_add(1).min(10),
            semantic_only: previous["coverage"]["complete"] == true
                && previous["semantic"]["skippedSources"].as_u64().unwrap_or(0) > 0,
            coverage: previous["coverage"].clone(),
        });
    }
    if !changed || previous.is_some_and(|job| job["automaticRebuildScope"] == signature) {
        return None;
    }
    Some(MaintenanceAttempt {
        signature: signature.into(),
        attempt: 0,
        semantic_only: false,
        coverage: Value::Null,
    })
}
fn maintain_memory(config: &GatewayConfig, cancel: &Arc<AtomicBool>) -> Result<(), ContractError> {
    let pending = {
        let mut owners = MEMORY_PENDING.get_or_init(Default::default).lock().unwrap();
        let queue = owners.entry(config.data_root.clone()).or_default();
        let mut ready = Vec::new();
        let now = std::time::Instant::now();
        let mut index = 0;
        while index < queue.len() {
            if queue[index].ready_at <= now {
                ready.push(queue.remove(index));
            } else {
                index += 1;
            }
        }
        ready
    };
    for mut item in pending {
        let result = if cancel.load(Ordering::Acquire) {
            Err(gateway::error("记忆索引已暂停。", "INDEX_CANCELLED", 409))
        } else {
            maintain_memory_scope(config, &item.scope, &item.id, cancel)
        };
        if let Err(error) = result {
            // Keep transient work bounded and retryable; deleting one scope does not discard other scopes.
            // 暂时性失败保留为有界待处理任务；单个范围删除或失败不会丢弃其他范围的更新。
            if temporary_index_failure(error.code()) && !cancel.load(Ordering::Acquire) {
                item.ready_at = std::time::Instant::now() + retry_delay(item.attempt);
                item.attempt = item.attempt.saturating_add(1).min(10);
                let mut owners = MEMORY_PENDING.get_or_init(Default::default).lock().unwrap();
                let queue = owners.entry(config.data_root.clone()).or_default();
                if queue.len() < 128
                    && !queue
                        .iter()
                        .any(|other| other.scope == item.scope && other.id == item.id)
                {
                    queue.push(item);
                }
            }
        }
    }
    Ok(())
}
fn maintain_deleted_chats(
    config: &GatewayConfig,
    cancel: &Arc<AtomicBool>,
    cursor: &mut usize,
) -> Result<(), ContractError> {
    let catalog = gateway::load_catalog(&config.data_root)?;
    let deleted = catalog["Tombstones"].as_array().unwrap();
    if deleted.is_empty() {
        *cursor = 0;
        return Ok(());
    }
    // Rotate bounded batches so a long deletion history cannot starve later tombstones or foreground work.
    // 有界轮转删除清理，避免长删除历史遗漏后面的聊天或阻塞前台；不扫描项目日志或共享来源正文。
    let count = deleted.len().min(16);
    let start = *cursor % deleted.len();
    let mut failure = None;
    for offset in 0..count {
        if cancel.load(Ordering::Acquire) {
            return Err(gateway::error("聊天清理已暂停。", "INDEX_CANCELLED", 409));
        }
        match gateway_index::retire_deleted_chat(config, &deleted[(start + offset) % deleted.len()])
        {
            Ok(_) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    *cursor = (start + count) % deleted.len();
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}
pub(crate) fn retire_chat_index_work(root: &Path, conversation: &str) {
    if let Some(pending) = MEMORY_PENDING.get() {
        if let Some(queue) = pending.lock().unwrap().get_mut(root) {
            queue
                .retain(|item| item.scope != "chat" || !item.id.eq_ignore_ascii_case(conversation));
        }
    }
}
fn maintain_memory_scope(
    config: &GatewayConfig,
    scope: &str,
    id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<(), ContractError> {
    let document = storage::gateway_memory::read_index_document(&config.data_root, &scope, &id)?;
    let key = if scope == "user" {
        "user".to_string()
    } else {
        format!("{scope}:{id}")
    };
    let mut sources = Vec::new();
    for entry in document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"] == "confirmed" && entry["active"] == true)
    {
        sources.push(json!({"sourceId":gateway_index::hash(&json!(["memory",key,entry["id"]]).to_string()),"scopeKey":key,"sourceType":"memory","text":entry["content"],"sourceRevision":entry["revision"],"title":entry["kind"],"locator":{"memoryId":entry["id"]}}));
    }
    for batch in sources.chunks(100) {
        gateway_index::publish(config, batch)?;
    }
    let keep = sources
        .iter()
        .filter_map(|source| source["sourceId"].as_str())
        .collect::<HashSet<_>>();
    let mut after = Value::Null;
    loop {
        let page = storage::sqlite_queries::apply(
            "storage.index.listSources",
            &gateway_index::arguments(
                config,
                json!({"scopes":[key],"sourceType":"memory","afterSourceId":after}),
            ),
        )?;
        let rows = page["rows"].as_array().unwrap();
        if rows.is_empty() {
            break;
        }
        for row in rows {
            if !keep.contains(row["sourceId"].as_str().unwrap()) {
                gateway_index::remove(
                    config,
                    row["sourceId"].as_str().unwrap(),
                    &json!([key]),
                    false,
                )?;
            }
        }
        after = page["nextSourceId"].clone();
        if rows.len() < 256 {
            break;
        }
    }
    let settings = effective(config, if scope == "project" { Some(id) } else { None })?;
    if settings["local"]["semantic"] != "off" {
        let mut retry = None;
        for source in sources {
            if cancel.load(Ordering::Acquire) {
                return Err(gateway::error("记忆索引已暂停。", "INDEX_CANCELLED", 409));
            }
            if let Err(error) =
                gateway_index::publish_semantic(config, &source, &settings["local"], cancel)
            {
                if temporary_index_failure(error.code()) {
                    retry.get_or_insert(error);
                }
            }
        }
        if let Some(error) = retry {
            return Err(error);
        }
    }
    Ok(())
}
pub fn start_background(config: &GatewayConfig) -> Result<Value, ContractError> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| gateway::error("后台运行时尚未就绪。", "RETRIEVAL_JOB_START_FAILED", 503))?;
    super::gateway_embedding::install_runtime(runtime.clone());
    let mut workers = BACKGROUND
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| gateway::error("后台作业服务暂时不可用。", "RETRIEVAL_BUSY", 503))?;
    if workers
        .get(&config.data_root)
        .is_some_and(|owner| !owner.stop.load(Ordering::Acquire))
    {
        return Ok(json!({"started":true,"reused":true}));
    }
    job_plan(config, "recover", json!({"resumable":false}))?;
    let root = config.data_root.clone();
    storage::gateway_memory::set_indexer(
        &root,
        Arc::new({
            let root = root.clone();
            move |document| schedule_memory(&root, document)
        }),
    );
    let owner = Arc::new(BackgroundOwner {
        stop: Arc::new(AtomicBool::new(false)),
        worker: Mutex::new(None),
    });
    let config = config.clone();
    let data_root = config.data_root.clone();
    let shared = owner.clone();
    let worker = std::thread::Builder::new()
        .name("kynxa-work-maintenance".into())
        .spawn(move || {
            let _runtime = runtime.enter();
            let mut previous = HashMap::new();
            let mut deleted_cursor = 0;
            // Discovery runs inside the existing watcher, so startup never waits for full memory preparation.
            // 发现过程位于现有 watcher 内，启动不会等待全部记忆准备；待处理范围分批合并同一队列。
            let mut startup_memory =
                storage::gateway_memory::startup_index_scopes(&config.data_root, &shared.stop)
                    .ok()
                    .and_then(|value| value["scopes"].as_array().cloned())
                    .unwrap_or_default()
                    .into_iter()
                    .collect::<VecDeque<_>>();
            loop {
                if shared.stop.load(Ordering::Acquire) {
                    break;
                }
                let cycle = (|| -> Result<(), ContractError> {
                    enqueue_startup_memory(&config.data_root, &mut startup_memory);
                    maintain_memory(&config, &shared.stop)?;
                    let catalog = gateway::load_catalog(&config.data_root)?;
                    let jobs = job_plan(&config, "read", json!({}))?["document"]["jobs"]
                        .as_array()
                        .unwrap()
                        .clone();
                    let mut projects = vec![None];
                    projects.extend(
                        catalog["Projects"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|project| {
                                project["IsArchived"] != true
                                    && project["IsFolderlessWorkspace"] != true
                            })
                            .filter_map(|project| {
                                project["Id"].as_str().map(|id| Some(id.to_string()))
                            }),
                    );
                    let (knowledge, _) = registry(&config.data_root)?;
                    for project in projects {
                        if shared.stop.load(Ordering::Acquire) {
                            break;
                        }
                        let id = project.as_deref();
                        let key = id.unwrap_or("user").to_string();
                        if jobs.iter().any(|job| {
                            job["projectId"] == json!(id)
                                && (["queued", "running", "paused"]
                                    .contains(&job["status"].as_str().unwrap_or(""))
                                    || job["automaticRebuildBlocked"] == true)
                        }) {
                            continue;
                        }
                        let settings = effective(&config, id)?;
                        if settings["local"]["enabled"] == false {
                            continue;
                        }
                        let relation = id
                            .map(|id| {
                                storage::catalog::apply(
                                    "storage.catalog.project",
                                    &json!({"current":catalog,"id":id}),
                                )
                            })
                            .transpose()?;
                        let files = if settings["projectIndexing"]["mountedFolder"] == true {
                            inventory(
                                &config,
                                relation.as_ref().unwrap()["folderPath"].as_str().unwrap(),
                                &settings["local"]["indexing"],
                                &shared.stop,
                            )?
                        } else {
                            json!([])
                        };
                        let sources = knowledge["sources"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|source| {
                                source["status"] == "ready"
                                    && (source["scope"] == "user"
                                        || source["projectId"] == json!(id))
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        let signature = gateway_index::hash(
                            &json!([settings, relation, files, sources]).to_string(),
                        );
                        let Some(attempt) = maintenance_attempt(
                            &jobs,
                            id,
                            &signature,
                            previous.get(&key) != Some(&signature),
                            chrono::Utc::now(),
                        ) else {
                            continue;
                        };
                        if id.is_none() && sources.is_empty() {
                            previous.insert(key, signature);
                            continue;
                        }
                        if rebuild_internal(
                            &config,
                            &json!({"projectId":id,"automatic":true}),
                            Some(attempt),
                        )
                        .is_ok()
                        {
                            previous.insert(key, signature);
                        }
                    }
                    maintain_deleted_chats(&config, &shared.stop, &mut deleted_cursor)?;
                    Ok(())
                })();
                if let Err(error) = cycle {
                    let _ = error;
                }
                let _ = super::gateway_embedding::retire_after(
                    &config,
                    std::time::Duration::from_secs(30),
                );
                for _ in 0..300 {
                    if shared.stop.load(Ordering::Acquire) {
                        break;
                    }
                    if MEMORY_PENDING
                        .get()
                        .and_then(|pending| pending.lock().ok())
                        .is_some_and(|pending| {
                            pending.get(&config.data_root).is_some_and(|entries| {
                                entries
                                    .iter()
                                    .any(|entry| entry.ready_at <= std::time::Instant::now())
                            })
                        })
                    {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        })
        .map_err(|_| gateway::error("后台维护线程未启动。", "RETRIEVAL_JOB_START_FAILED", 503))?;
    *owner.worker.lock().unwrap() = Some(worker);
    workers.insert(data_root, owner);
    Ok(json!({"started":true,"reconcileIntervalMs":30000}))
}
pub fn stop_background(config: &GatewayConfig) -> Value {
    if let Some(owners) = BACKGROUND.get() {
        if let Some(owner) = owners
            .lock()
            .ok()
            .and_then(|owners| owners.get(&config.data_root).cloned())
        {
            owner.stop.store(true, Ordering::Release);
        }
    }
    if let Some(tokens) = CANCELLATIONS.get() {
        for token in tokens.lock().unwrap().values() {
            token.store(true, Ordering::Release);
        }
    }
    storage::gateway_memory::stop_candidates(&config.data_root);
    json!({"stopping":true,"cancelRequested":true})
}
fn inventory(
    config: &GatewayConfig,
    path: &str,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    let mut scanner = crate::tools::service_source_reading::SourceReadingState::default();
    let token = Some(cancel.clone());
    let mut options = limits.clone();
    options["root"] = json!(path);
    options["excludedRoots"] = json!([config.data_root, config.data_home, config.extension_root]);
    scanner
        .apply(
            "source.reading.scanOpen",
            &json!({"path":path,"options":options}),
            &token,
        )
        .map_err(|error| {
            gateway::error(
                &error.message,
                "RETRIEVAL_SOURCE_UNAVAILABLE",
                error.status_code,
            )
        })?;
    let mut files = Vec::new();
    loop {
        let next = scanner
            .apply("source.reading.scanNext", &json!({}), &token)
            .map_err(|error| {
                gateway::error(
                    &error.message,
                    "RETRIEVAL_SOURCE_UNAVAILABLE",
                    error.status_code,
                )
            })?;
        if next.is_null() {
            break;
        }
        files.push(json!([
            next["relativePath"],
            next["metadata"],
            next["failed"],
            next["missing"]
        ]));
    }
    let stats = scanner
        .apply("source.reading.scanStats", &json!({}), &token)
        .map_err(|error| {
            gateway::error(
                &error.message,
                "RETRIEVAL_SOURCE_UNAVAILABLE",
                error.status_code,
            )
        })?;
    files.sort_by_cached_key(Value::to_string);
    Ok(json!({"files":files,"coverage":stats}))
}
pub fn effective(config: &GatewayConfig, project_id: Option<&str>) -> Result<Value, ContractError> {
    gateway::with_catalog(&config.data_root, |catalog, _| {
        let (value, _) = gateway::read_json(
            &config.data_root,
            &config.data_root.join("Retrieval/settings.json"),
            8 * 1024 * 1024,
        )?;
        let mut global = match value {
            Some(value) => settings::apply(
                "storage.settings.document",
                &json!({"value":value,"projectId":null}),
            )?,
            None => settings::apply("storage.settings.defaults", &json!({}))?,
        };
        global["projectId"] = json!(project_id);
        global["projectRevision"] = Value::Null;
        global["projectIndexing"] =
            json!({"mountedFolder":false,"bindingRevision":0,"knowledgeIds":[]});
        if let Some(id) = project_id {
            let relationship = storage::catalog::apply(
                "storage.catalog.project",
                &json!({"current":catalog,"id":id}),
            )?;
            let (value, _) = gateway::read_json(
                &config.data_root,
                &config
                    .data_root
                    .join("Projects")
                    .join(id)
                    .join("retrieval.json"),
                8 * 1024 * 1024,
            )?;
            let project = match value {
                Some(value) => settings::apply(
                    "storage.settings.document",
                    &json!({"value":value,"projectId":id}),
                )?,
                None => {
                    settings::apply("storage.settings.defaultProject", &json!({"projectId":id}))?
                }
            };
            global = settings::apply(
                "storage.settings.merge",
                &json!({"base":global,"patch":project["overrides"]}),
            )?;
            global["projectId"] = json!(id);
            global["projectRevision"] = project["revision"].clone();
            global["projectIndexing"] = json!({"mountedFolder":settings::apply("storage.settings.mounted",&json!({"relationship":relationship}))?,
                "bindingRevision":project["indexingSources"]["mountedFolder"]["bindingRevision"],"knowledgeIds":project["indexingSources"]["knowledgeIds"]});
        }
        Ok(global)
    })
}
fn registry(root: &Path) -> Result<(Value, Option<String>), ContractError> {
    let (value, hash) =
        gateway::read_json(root, &root.join("Knowledge/catalog.json"), 16 * 1024 * 1024)?;
    let document = value.unwrap_or(json!({"schemaVersion":1,"revision":0,"sources":[]}));
    library::apply("storage.library.validate", &json!({"document":document}))?;
    Ok((document, hash))
}
fn duplicate(entry: &Value) -> String {
    json!([
        entry["originalPath"],
        entry["contentHash"],
        entry.get("extraction").map(Value::to_string),
        entry.get("fileWindow").map(Value::to_string),
        entry.get("archive").map(Value::to_string),
        entry.get("urlWindow"),
        entry["origin"]["responseHash"]
    ])
    .to_string()
}
pub fn import(config: &GatewayConfig, input: &Value) -> Result<Value, ContractError> {
    let scope = input["scope"].as_str().unwrap_or("user");
    if !["user", "project"].contains(&scope) {return Err(gateway::error("资料范围无效。", "INVALID_RETRIEVAL_SCOPE", 400));}
    let project = input["projectId"].as_str();
    let settings = effective(config, if scope == "project" { project } else { None })?;
    let limits = &settings["local"]["indexing"];
    let cancel = crate::service_cancellation::current_token().unwrap_or_else(||Arc::new(AtomicBool::new(false)));
    let urls = input.get("url").is_some() || input.get("urls").is_some();
    let path = if urls { None } else {
        let path=Path::new(input["path"].as_str().ok_or_else(||gateway::error("导入路径缺失。","INVALID_RETRIEVAL_SOURCE",400))?);
        if !path.is_absolute() {return Err(gateway::error("导入需要绝对路径。","INVALID_RETRIEVAL_SOURCE",400));}
        Some(path)
    };
    let mut staged = Vec::new();
    let mut entries = Vec::new();
    let result = (|| {
        let mut stage = |file: Value| -> Result<(), ContractError> {
            let text = file["text"].as_str().unwrap();
            let id = gateway::new_id();
            let receipt = library::apply(
                "storage.library.stage",
                &json!({"root":config.data_root,"id":id,"text":text}),
            )?;
            staged.push(json!({"id":id,"hash":receipt["hash"]}));
            let mut entry = json!({"id":id,"scope":scope,"projectId":if scope=="project"{input["projectId"].clone()}else{Value::Null},"title":file["title"],"originalPath":file["locator"]["path"],"revision":1,"storedBytes":text.len(),"contentHash":gateway_index::hash(text),"status":"ready","createdAt":gateway::now()});
            if file["locator"]["archive"].is_object() && !file["locator"]["origin"].is_object() {
                let members=file["locator"]["archive"]["chain"].as_array().into_iter().flatten().filter_map(|item|item["member"].as_str()).collect::<Vec<_>>().join("!/");
                entry["originalPath"]=json!(format!("{}!/{members}#{}",file["locator"]["path"].as_str().unwrap_or("archive"),file["locator"]["archive"]["partIndex"]));
            }
            for field in ["extraction", "fileWindow", "archive", "origin", "urlWindow"] {
                if let Some(value) = file["locator"].get(field) {
                    entry[field] = value.clone();
                }
            }
            entries.push(entry);
            Ok(())
        };
        let scan = if urls {
            super::gateway_url_sources::scan(config,input,limits,&cancel,&mut stage)?
        } else {
            scan_each(config,path.unwrap().to_str().unwrap(),limits,&cancel,&mut stage)?
        };
        if entries.is_empty() {
            return Err(gateway::error(
                &format!("没有成功读取的可导入资料：{}。",scan["failures"][0]["errorCode"].as_str().or_else(||scan["failures"][0]["code"].as_str()).unwrap_or("RETRIEVAL_IMPORT_EMPTY")),
                "RETRIEVAL_IMPORT_EMPTY",
                400,
            ));
        }
        let receipt = gateway::with_catalog(&config.data_root, |catalog, _| {
            let relation = project
                .map(|id| {
                    storage::catalog::apply(
                        "storage.catalog.project",
                        &json!({"current":catalog,"id":id}),
                    )
                })
                .transpose()?;
            let target = library::apply(
                "storage.library.target",
                &json!({"scope":scope,"projectId":project,"relationship":relation}),
            )?;
            let (mut document, digest) = registry(&config.data_root)?;
            let mut added = Vec::new();
            for entry in &entries {
                let keys = document["sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| {
                        (
                            entry["id"].as_str().unwrap().to_string(),
                            json!(duplicate(entry)),
                        )
                    })
                    .collect::<serde_json::Map<String, Value>>();
                let plan = gateway::normalize_transport_numbers(library::apply(
                    "storage.library.addition",
                    &json!({"document":document,"target":target,"entry":entry,"limits":limits,"existingKeys":keys,"duplicateKey":duplicate(entry)}),
                )?);
                document = plan["document"].clone();
                added.push(plan["entry"].clone());
            }
            document["revision"] = json!(document["revision"].as_u64().unwrap() + 1);
            library::apply(
                "storage.library.publish",
                &json!({"root":config.data_root,"raw":document.to_string(),"expectedHash":digest}),
            )?;
            Ok(json!({"revision":document["revision"],"sources":added,"scan":scan}))
        })?;
        Ok(receipt)
    })();
    // Discard only snapshots not published by this import; duplicate detection may retain an older source.
    // 仅清理本次尚未发布的快照；去重可能选择已有资料，不能留下新建但未登记的文件。
    let retained = result
        .as_ref()
        .ok()
        .and_then(|receipt| receipt["sources"].as_array());
    for owned in staged {
        if retained.is_none_or(|sources| !sources.iter().any(|entry| entry["id"] == owned["id"])) {
            let _ = library::apply(
                "storage.library.discard",
                &json!({"root":config.data_root,"id":owned["id"],"hash":owned["hash"]}),
            );
        }
    }
    let mut result = result?;
    result["job"]=rebuild(config,&json!({"projectId":if scope=="project"{input["projectId"].clone()}else{Value::Null},"automatic":true})).ok().unwrap_or(Value::Null);
    Ok(result)
}
pub fn remove_source(
    config: &GatewayConfig,
    id: &str,
    input: &Value,
) -> Result<Value, ContractError> {
    let removed = gateway::with_catalog(&config.data_root, |_, _| {
        let (document, digest) = registry(&config.data_root)?;
        let mut arguments = json!({"document":document,"id":id,"now":gateway::now()});
        if let Some(revision) = input
            .get("expectedRevision")
            .filter(|value| !value.is_null())
        {
            arguments["expectedRevision"] = revision.clone();
        }
        let plan = gateway::normalize_transport_numbers(library::apply(
            "storage.library.remove",
            &arguments,
        )?);
        if plan["changed"] == true {
            library::apply(
                "storage.library.publish",
                &json!({"root":config.data_root,"raw":plan["document"].to_string(),"expectedHash":digest}),
            )?;
        }
        Ok(plan["entry"].clone())
    })?;
    let scope = if removed["scope"] == "project" {
        format!("project:{}", removed["projectId"].as_str().unwrap())
    } else {
        "user".into()
    };
    gateway_index::remove(config, id, &json!([scope]), true)?;
    Ok(
        json!({"id":removed["id"],"title":removed["title"],"scope":removed["scope"],"projectId":removed["projectId"],"path":removed["originalPath"],"status":removed["status"],"revision":removed["revision"]}),
    )
}
pub fn bind_work(
    config: &GatewayConfig,
    context: &Value,
    input: &Value,
) -> Result<Value, ContractError> {
    let path = Path::new(input["path"].as_str().unwrap_or(""));
    if !path.is_absolute()
        || input["reason"]
            .as_str()
            .is_none_or(|reason| reason.trim().is_empty())
    {
        return Err(gateway::error(
            "关联工作目录需要绝对路径及长期用途。",
            "WORK_BINDING_PATH_REQUIRED",
            400,
        ));
    }
    let mut scanner = crate::tools::service_source_reading::SourceReadingState::default();
    let options = json!({"root":path,"maximumFiles":1,"maximumSourceBytes":32*1024*1024,"maximumTotalBytes":512*1024*1024,"maximumEntries":2048,"excludedRoots":[config.data_root,config.data_home,config.extension_root]});
    scanner
        .apply(
            "source.reading.scanOpen",
            &json!({"path":path,"options":options}),
            &None,
        )
        .map_err(|_| {
            gateway::error(
                "工作目录必须有效、无链接并且不属于应用内部。",
                "WORK_BINDING_NOT_DIRECTORY",
                400,
            )
        })?;
    scanner
        .apply("source.reading.scanClose", &json!({}), &None)
        .map_err(|_| gateway::error("工作目录核验失败。", "WORK_BINDING_NOT_DIRECTORY", 400))?;
    let id = context["conversationId"]
        .as_str()
        .ok_or_else(|| gateway::error("当前聊天不属于工作。", "WORK_REQUIRED", 409))?;
    let expected_project = context
        .get("expectedProjectId")
        .or_else(|| context.get("projectId"))
        .ok_or_else(|| {
            gateway::error("工作绑定缺少当前轮次归属快照。", "WORKSPACE_CHANGED", 409)
        })?;
    let expected_folder = context.get("expectedFolderPath").ok_or_else(|| {
        gateway::error("工作绑定缺少当前轮次目录快照。", "WORKSPACE_CHANGED", 409)
    })?;
    let folder = std::fs::canonicalize(path)
        .map_err(|_| gateway::error("工作目录不存在。", "WORK_BINDING_NOT_DIRECTORY", 400))?
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_string();
    let binding = gateway::with_catalog(&config.data_root, |catalog, digest| {
        let current = storage::catalog::apply(
            "storage.catalog.relationship",
            &json!({"current":catalog,"id":id}),
        )?;
        let relation = storage::catalog::apply(
            "storage.catalog.project",
            &json!({"current":catalog,"id":current["projectId"]}),
        )?;
        let plan = storage::catalog::apply(
            "storage.catalog.bindWork",
            &json!({"current":catalog,"id":id,"expectedProjectId":expected_project,"resolvedCurrent":relation["folderPath"],"resolvedExpected":expected_folder,"folderPath":folder}),
        )?;
        let mut next = catalog.clone();
        if plan["changed"] == true {
            next["Projects"][plan["projectIndex"].as_u64().unwrap() as usize]["FolderPath"] =
                json!(folder);
            next["Revision"] = json!(catalog["Revision"].as_u64().unwrap() + 1);
            gateway::commit_catalog(&config.data_root, &next, digest, &[], &[])?;
        }
        Ok(
            json!({"projectId":plan["projectId"],"folderPath":folder,"revision":next["Revision"],"changed":plan["changed"]}),
        )
    })?;
    let mut binding = binding;
    binding["preparation"] = rebuild(
        config,
        &json!({"projectId":binding["projectId"],"automatic":true}),
    )
    .map(|job| json!({"state":"queued","jobId":job["jobId"],"waitForSemantic":false}))
    .unwrap_or_else(|error| json!({"state":"failed","reason":error.code()}));
    Ok(binding)
}
fn job_plan(config: &GatewayConfig, operation: &str, input: Value) -> Result<Value, ContractError> {
    gateway::with_catalog(&config.data_root, |_, _| {
        let path = config.data_root.join("Retrieval/jobs.json");
        let (value, hash) = gateway::read_json(&config.data_root, &path, 16 * 1024 * 1024)?;
        let document = value.unwrap_or(json!({"schemaVersion":1,"jobs":[]}));
        if operation == "create" {
            let active = document["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|job| {
                    ["queued", "running", "paused"].contains(&job["status"].as_str().unwrap_or(""))
                })
                .collect::<Vec<_>>();
            if let Some(previous) = active
                .iter()
                .find(|job| job["projectId"] == input["job"]["projectId"])
            {
                return Ok(
                    json!({"document":document,"jobId":previous["jobId"],"changed":false,"reused":true}),
                );
            }
            if active.len() >= 128 {
                return Err(gateway::error(
                    "后台索引队列已满，前台搜索仍可使用。",
                    "RETRIEVAL_JOB_CAPACITY",
                    503,
                ));
            }
        }
        let mut arguments = input;
        arguments["document"] = document.clone();
        arguments["now"] = json!(gateway::now());
        arguments["timestamps"] = gateway::timestamps(&arguments);
        let result = jobs::apply(&format!("storage.jobs.{operation}"), &arguments)?;
        if result["changed"] == true {
            gateway::commit_json(
                &config.data_root,
                &path,
                &result["document"],
                hash.as_deref(),
                16 * 1024 * 1024,
            )?;
        }
        Ok(result)
    })
}
fn job_update(config: &GatewayConfig, id: &str, patch: Value) -> Result<Value, ContractError> {
    let result = job_plan(config, "update", json!({"jobId":id,"patch":patch}))?;
    Ok(result["document"]["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["jobId"] == id)
        .cloned()
        .unwrap())
}
pub fn rebuild(config: &GatewayConfig, input: &Value) -> Result<Value, ContractError> {
    rebuild_internal(config, input, None)
}
fn rebuild_internal(
    config: &GatewayConfig,
    input: &Value,
    maintenance: Option<MaintenanceAttempt>,
) -> Result<Value, ContractError> {
    let project_id = input["projectId"].as_str();
    let current = effective(config, project_id)?;
    if current["local"]["enabled"] == false {
        return Err(gateway::error(
            "本地检索已停用。",
            "RETRIEVAL_DISABLED",
            409,
        ));
    }
    let id = gateway::new_id();
    let mut job = json!({"jobId":id,"projectId":project_id,"status":"queued","completedSources":0,"totalSources":0,"createdAt":gateway::now()});
    if let Some(maintenance) = &maintenance {
        job["automaticRebuildScope"] = json!(maintenance.signature);
        job["automaticRetryAttempt"] = json!(maintenance.attempt);
        job["semanticRetry"] = json!(maintenance.semantic_only);
    }
    let plan = job_plan(
        config,
        "create",
        json!({"job":job,"resumeAutomatic":input["automatic"]!=true}),
    )?;
    let id = plan["jobId"].as_str().unwrap().to_string();
    let job = plan["document"]["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["jobId"] == id)
        .cloned()
        .unwrap();
    let cancellation = crate::service_cancellation::CancellationLease::acquire(&id)
        .map_err(|_| gateway::error("索引取消令牌无法创建。", "RETRIEVAL_BUSY", 503))?;
    let cancel = cancellation.token.clone();
    let mut tokens = CANCELLATIONS.get_or_init(Default::default).lock().unwrap();
    if tokens.contains_key(&id) {
        return Ok(job);
    }
    tokens.insert(id.clone(), cancel.clone());
    drop(tokens);
    let runtime = tokio::runtime::Handle::try_current().ok();
    if let Some(runtime) = &runtime {
        super::gateway_embedding::install_runtime(runtime.clone());
    }
    let config = config.clone();
    let settings = current;
    std::thread::Builder::new().name("kynxa-corpus-index".to_string()).spawn(move|| {
        let _runtime=runtime.as_ref().map(tokio::runtime::Handle::enter);
        let gate={let mut gates=BACKGROUND_GATES.get_or_init(Default::default).lock().unwrap();gates.entry(config.data_root.clone()).or_insert_with(||Arc::new(Mutex::new(()))).clone()};
        let _queue=gate.lock().unwrap();
        if cancel.load(Ordering::Acquire){CANCELLATIONS.get().unwrap().lock().unwrap().remove(&id);return;}
        let _=job_update(&config,&id,json!({"status":"running","startedAt":gateway::now()}));
        let result={let _scope=cancellation.enter();if let Some(maintenance)=maintenance.filter(|attempt|attempt.semantic_only){run_semantic_retry(&config,&settings,maintenance.coverage,&cancel)}else{run_index(&config,&id,&settings,&cancel)}};
        let patch=match result {
            Ok(report)=>json!({"status":if report["failed"].as_u64().unwrap_or(0)>0{"partial"}else{"completed"},"completedSources":report["completedSources"],"totalSources":report["totalSources"],"finishedAt":gateway::now(),"coverage":report["coverage"],"semantic":report["semantic"]}),
            Err(error)=>json!({"status":if cancel.load(Ordering::Acquire){"cancelled"}else{"failed"},"error":error.code(),"finishedAt":gateway::now(),
                "automaticRebuildBlocked":cancel.load(Ordering::Acquire)&&!BACKGROUND.get().and_then(|owners|owners.lock().ok()).and_then(|owners|owners.get(&config.data_root).cloned()).is_some_and(|owner|owner.stop.load(Ordering::Acquire))})
        };
        let _=job_update(&config,&id,patch);CANCELLATIONS.get().unwrap().lock().unwrap().remove(&id);
    }).map_err(|_|gateway::error("后台索引线程未启动。","RETRIEVAL_JOB_START_FAILED",503))?;
    Ok(job)
}
fn run_index(
    config: &GatewayConfig,
    id: &str,
    settings: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    let project_id = settings["projectId"].as_str();
    let scope = project_id
        .map(|id| format!("project:{id}"))
        .unwrap_or("user".to_string());
    let mut count = 0;
    let mut queued = Vec::new();
    let mut seen = HashSet::new();
    let mut failed = 0;
    let mut discovered = 0;
    let mut incomplete = false;
    let mut failures = Vec::new();
    let catalog = gateway::load_catalog(&config.data_root)?;
    if let Some(project_id) = project_id {
        let project = storage::catalog::apply(
            "storage.catalog.project",
            &json!({"current":catalog,"id":project_id}),
        )?;
        if settings["projectIndexing"]["mountedFolder"] == true {
            let root = project["folderPath"].as_str().unwrap_or("");
            let report = scan_each(
                config,
                root,
                &settings["local"]["indexing"],
                cancel,
                |mut source| {
                    let mut identity = vec![
                        json!("work-file"),
                        json!(project_id),
                        json!(root),
                        source["locator"]["relativePath"].clone(),
                    ];
                    if let Some(window) = source["locator"].get("fileWindow") {
                        identity.push(window["startOffset"].clone());
                    } else if source["locator"]["extraction"]["pageWindow"].is_object() {
                        identity.push(json!("pdf-page"));
                        identity.push(
                            source["locator"]["extraction"]["pageWindow"]["startPage"].clone(),
                        );
                    } else if source["locator"]["archive"].is_object() {
                        identity.push(json!("archive-part"));
                        identity.push(source["locator"]["archive"]["part"].clone());
                    }
                    source["sourceId"] = json!(gateway_index::hash(&json!(identity).to_string()));
                    source["scopeKey"] = json!(scope);
                    source["sourceType"] = json!("work-file");
                    source["bindingRevision"] =
                        settings["projectIndexing"]["bindingRevision"].clone();
                    seen.insert(source["sourceId"].as_str().unwrap().to_string());
                    gateway_index::publish(config, &[source.clone()])?;
                    count += 1;
                    source.as_object_mut().unwrap().remove("text");
                    queued.push(source);
                    if count % 16 == 0 {
                        job_update(
                            config,
                            id,
                            json!({"completedSources":count,"totalSources":count}),
                        )?;
                    }
                    Ok(())
                },
            )?;
            failed += report["failedFiles"].as_u64().unwrap_or(0);
            discovered += report["scannedFiles"].as_u64().unwrap_or(count);
            incomplete |= report["incomplete"] == true;
            failures.extend(report["failures"].as_array().into_iter().flatten().take(1000).cloned());
        }
    }
    let (document, _) = registry(&config.data_root)?;
    for entry in document["sources"].as_array().unwrap() {
        if cancel.load(Ordering::Acquire) {
            return Err(gateway::error("索引已取消。", "INDEX_CANCELLED", 409));
        }
        if entry["status"] == "deleted"
            || entry["scope"] == "project" && entry["projectId"] != settings["projectId"]
        {
            continue;
        }
        let snapshot = library::apply(
            "storage.library.readSnapshot",
            &json!({"root":config.data_root,"id":entry["id"]}),
        )?;
        if snapshot["exists"] != true {
            continue;
        }
        let text = snapshot["raw"].as_str().unwrap();
        if gateway_index::hash(text) != entry["contentHash"] {
            continue;
        }
        let mut source = json!({"sourceId":entry["id"],"scopeKey":if entry["scope"]=="user"{"user".to_string()}else{scope.clone()},
            "sourceType":"knowledge","title":entry["title"],"text":text,"sourceRevision":entry["revision"],"locator":{"knowledgeId":entry["id"],"relativePath":entry["title"]}});
        if let Some(extraction) = entry.get("extraction") {
            source["locator"]["extraction"] = extraction.clone();
        }
        if let Some(window) = entry.get("fileWindow") {
            source["locator"]["fileWindow"] = window.clone();
        }
        for field in ["archive", "origin", "urlWindow"] {
            if let Some(value)=entry.get(field) {source["locator"][field]=value.clone();}
        }
        gateway_index::publish(config, &[source.clone()])?;
        seen.insert(entry["id"].as_str().unwrap().to_string());
        source["contentHash"] = entry["contentHash"].clone();
        source.as_object_mut().unwrap().remove("text");
        queued.push(source);
        count += 1;
        discovered += 1;
    }
    job_update(
        config,
        id,
        json!({"completedSources":count,"totalSources":count}),
    )?;
    // Incomplete scans retain unobserved sources; complete scans revoke deletions before later embedding work.
    // 不完整扫描保留未观察来源；完整扫描在后续向量化之前撤销删除内容。
    if failed == 0 && !incomplete {
        let mut after = Value::Null;
        loop {
            let page = storage::sqlite_queries::apply(
                "storage.index.listSources",
                &gateway_index::arguments(config, json!({"scopes":[scope],"afterSourceId":after})),
            )?;
            for source in page["rows"].as_array().unwrap() {
                if ["knowledge", "work-file"].contains(&source["sourceType"].as_str().unwrap_or(""))
                    && !seen.contains(source["sourceId"].as_str().unwrap())
                {
                    gateway_index::remove(
                        config,
                        source["sourceId"].as_str().unwrap(),
                        &json!([scope]),
                        false,
                    )?;
                }
            }
            if page["rows"].as_array().unwrap().is_empty() {
                break;
            }
            after = page["nextSourceId"].clone();
        }
    }
    finish_semantic(
        config,
        settings,
        queued,
        json!({"complete":failed==0&&!incomplete,"discovered":discovered,"lexical":count,"semantic":0,"failed":failed,"skipped":0,"partial":u64::from(incomplete),"failures":failures,"reportTruncated":incomplete||failed>1000}),
        cancel,
    )
}
fn run_semantic_retry(
    config: &GatewayConfig,
    settings: &Value,
    mut coverage: Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    let scope = settings["projectId"]
        .as_str()
        .map(|id| format!("project:{id}"))
        .unwrap_or_else(|| "user".into());
    let mut queued = Vec::new();
    let mut after = Value::Null;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(gateway::error("索引已取消。", "INDEX_CANCELLED", 409));
        }
        let page = sqlite_sources(config, &scope, &after)?;
        for row in page["rows"].as_array().unwrap().iter().filter(|row| {
            ["knowledge", "work-file"].contains(&row["sourceType"].as_str().unwrap_or(""))
        }) {
            let mut source = row.clone();
            for field in ["locator", "sourceRevision"] {
                if let Some(raw) = source[field].as_str() {
                    source[field] = serde_json::from_str(raw).map_err(|_| {
                        gateway::error("索引来源结构无效。", "CORRUPT_RETRIEVAL_INDEX", 409)
                    })?;
                }
            }
            queued.push(source);
        }
        if page["rows"].as_array().unwrap().len() < 256 {
            break;
        }
        after = page["nextSourceId"].clone();
    }
    coverage["lexical"] = json!(queued.len());
    coverage["discovered"] = json!(queued.len());
    finish_semantic(config, settings, queued, coverage, cancel)
}
fn sqlite_sources(
    config: &GatewayConfig,
    scope: &str,
    after: &Value,
) -> Result<Value, ContractError> {
    storage::sqlite_queries::apply(
        "storage.index.listSources",
        &gateway_index::arguments(config, json!({"scopes":[scope],"afterSourceId":after})),
    )
}
fn finish_semantic(
    config: &GatewayConfig,
    settings: &Value,
    queued: Vec<Value>,
    mut coverage: Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    let count = coverage["lexical"].as_u64().unwrap_or(0);
    let failed = coverage["failed"].as_u64().unwrap_or(0);
    let incomplete = coverage["complete"] != true;
    let mut semantic = 0u64;
    let mut pending = 0u64;
    let mut total_chunks = 0u64;
    let mut vector_chunks = 0u64;
    let mut cached_chunks = 0u64;
    let mut codes = HashSet::new();
    let mut profile = Value::Null;
    for source in queued {
        if cancel.load(Ordering::Acquire) {
            return Err(gateway::error("索引已取消。", "INDEX_CANCELLED", 409));
        }
        if settings["local"]["semantic"] == "off" {
            continue;
        }
        let original = load_queued(config, &source, settings, cancel);
        match original.and_then(|source| {
            gateway_index::publish_semantic(config, &source, &settings["local"], cancel)
        }) {
            Ok(receipt) => {
                semantic += 1;
                total_chunks += receipt["chunks"].as_u64().unwrap_or(0);
                vector_chunks += receipt["vectorChunks"].as_u64().unwrap_or(0);
                cached_chunks += receipt["cachedChunks"].as_u64().unwrap_or(0);
                profile = receipt["profileId"].clone();
            }
            Err(error) => {
                if cancel.load(Ordering::Acquire) {
                    return Err(error);
                }
                pending += 1;
                codes.insert(error.code().to_string());
            }
        }
    }
    coverage["semantic"] = json!(semantic);
    Ok(
        json!({"completedSources":count,"totalSources":count,"failed":failed+pending+u64::from(incomplete),"semantic":{"requested":settings["local"]["semantic"]!="off","profileId":profile,"totalChunks":total_chunks,"vectorChunks":vector_chunks,"cachedChunks":cached_chunks,"skippedSources":pending,"skippedChunks":0,"diagnosticCodes":codes.into_iter().take(8).collect::<Vec<_>>(),"state":if settings["local"]["semantic"]=="off"{"disabled"}else if pending>0&&semantic==0{"unavailable"}else if pending>0{"partial"}else{"complete"}},
        "coverage":coverage}),
    )
}
fn load_queued(
    config: &GatewayConfig,
    source: &Value,
    settings: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    let observed = if source["sourceType"] == "work-file" {
        let receipt = crate::tools::service_source_reading::execute_io(
            "metadata",
            &json!({"path":source["locator"]["path"]}),
            &Some(cancel.clone()),
        )
        .map_err(|_| gateway::error("原文件已改变。", "STALE_RETRIEVAL_SOURCE", 409))?;
        if source
            .get("fileMetadata")
            .is_some_and(|metadata| *metadata != receipt["metadata"])
        {
            return Err(gateway::error(
                "原文件已改变。",
                "STALE_RETRIEVAL_SOURCE",
                409,
            ));
        }
        Some(receipt["metadata"].clone())
    } else {
        None
    };
    let value = if source["sourceType"] == "knowledge" {
        library::apply(
            "storage.library.readSnapshot",
            &json!({"root":config.data_root,"id":source["sourceId"]}),
        )?
    } else if source["locator"]["archive"].is_object() {
        super::gateway_archives::read_member(config,&source["locator"],&settings["local"]["indexing"],cancel)?
    } else if source["locator"]["fileWindow"].is_object() {
        crate::tools::service_source_reading::execute_io("readTextWindow",&json!({"path":source["locator"]["path"],"window":source["locator"]["fileWindow"],"maximumSourceBytes":settings["local"]["indexing"]["maximumSourceBytes"]}),&Some(cancel.clone())).map_err(|_|gateway::error("原文件已改变。","STALE_RETRIEVAL_SOURCE",409))?
    } else {
        let value = super::gateway_documents::read_with_config(
            config,
            Path::new(source["locator"]["path"].as_str().unwrap()),
            &settings["local"]["indexing"],
            cancel,
        )?;
        value["documentWindows"]
            .as_array()
            .and_then(|windows| {
                windows.iter().find(|window| {
                    window["extraction"]["pageWindow"]
                        == source["locator"]["extraction"]["pageWindow"]
                })
            })
            .cloned()
            .unwrap_or(value)
    };
    if source["sourceType"] == "work-file" {
        super::gateway_documents::verify_extraction(&source["locator"]["extraction"], &value["extraction"])?;
    }
    let text = value["text"]
        .as_str()
        .or_else(|| value["raw"].as_str())
        .ok_or_else(|| gateway::error("来源正文缺失。", "RETRIEVAL_SOURCE_UNAVAILABLE", 409))?;
    if gateway_index::hash(text) != source["contentHash"] {
        return Err(gateway::error(
            "来源已变化，旧向量不会发布。",
            "STALE_RETRIEVAL_SOURCE",
            409,
        ));
    }
    let mut loaded = source.clone();
    loaded["text"] = json!(text);
    if let Some(observed) = observed {
        let metadata = crate::tools::service_source_reading::execute_io(
            "metadata",
            &json!({"path":source["locator"]["path"]}),
            &Some(cancel.clone()),
        )
        .map_err(|_| gateway::error("原文件已改变。", "STALE_RETRIEVAL_SOURCE", 409))?;
        if metadata["metadata"] != observed {
            return Err(gateway::error(
                "读取期间原文件已改变。",
                "STALE_RETRIEVAL_SOURCE",
                409,
            ));
        }
        loaded["fileMetadata"] = observed;
    }
    Ok(loaded)
}
fn scan_each(
    config: &GatewayConfig,
    path: &str,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    mut consume: impl FnMut(Value) -> Result<(), ContractError>,
) -> Result<Value, ContractError> {
    use crate::tools::service_source_reading::{SourceReadingState, execute_io};
    let token = Some(cancel.clone());
    let mut scanner = SourceReadingState::default();
    let mut options = limits.clone();
    options["root"] = json!(path);
    options["maximumBytes"] = limits["maximumTotalBytes"].clone();
    options["allowPartial"] = json!(true);
    options["excludedRoots"] = json!([config.data_root, config.data_home, config.extension_root]);
    scanner
        .apply(
            "source.reading.scanOpen",
            &json!({"path":path,"options":options}),
            &token,
        )
        .map_err(|_| {
            gateway::error(
                "目录扫描未通过范围检查。",
                "RETRIEVAL_SOURCE_UNAVAILABLE",
                400,
            )
        })?;
    'files: loop {
        let next = scanner
            .apply("source.reading.scanNext", &json!({}), &token)
            .map_err(|_| {
                gateway::error(
                    "目录扫描受阻或已取消。",
                    "RETRIEVAL_SOURCE_UNAVAILABLE",
                    409,
                )
            })?;
        if next.is_null() {
            break;
        }
        if next["failed"] == true || next["missing"] == true {
            continue;
        }
        scanner
            .apply(
                "source.reading.scanObserved",
                &json!({"reused":false,"size":next["metadata"]["sizeBytes"]}),
                &token,
            )
            .map_err(|failure| gateway::error(&failure.message, "RETRIEVAL_SCAN_FAILED", 422))?;
        let mut input = limits.clone();
        input["path"] = next["path"].clone();
        input["decodeText"] = json!(true);
        let filename = Path::new(next["path"].as_str().unwrap());
        if super::gateway_archives::is_archive(filename) {
            let scanned=super::gateway_archives::scan(config,filename,limits,cancel,&mut |mut source| {
                let logical=source["locator"]["archive"]["chain"].as_array().into_iter().flatten().filter_map(|item|item["member"].as_str()).collect::<Vec<_>>().join("!/");
                source["locator"]["relativePath"]=json!(format!("{}!/{logical}",next["relativePath"].as_str().unwrap_or("archive")));
                source["locator"]["root"]=json!(path);
                let admitted=scanner.apply("source.reading.scanPublish",&json!({"textBytes":source["text"].as_str().unwrap_or("").len(),"size":next["metadata"]["sizeBytes"]}),&token)
                    .map_err(|failure|gateway::error(&failure.message,"RETRIEVAL_SCAN_LIMIT",422))?;
                if admitted==false {return Err(gateway::error("扫描资料达到配置额度。","RETRIEVAL_SCAN_LIMIT",413));}
                consume(source)
            });
            let report=match scanned {
                Ok(report)=>report,
                Err(error) if error.code()=="RETRIEVAL_SCAN_LIMIT"=>break 'files,
                Err(error) if error.code().starts_with("ARCHIVE_") || error.code().starts_with("DOCUMENT_") || error.code().starts_with("OCR_") || error.code()=="RETRIEVAL_SOURCE_UNAVAILABLE"=>{
                    scanner.apply("source.reading.scanFailure",&json!({"relativePath":next["relativePath"],"error":{"code":error.code()},"aborted":false}),&token)
                        .map_err(|_|gateway::error("压缩包覆盖记录未保存。","RETRIEVAL_SCAN_FAILED",409))?;
                    continue;
                }
                Err(error)=>return Err(error),
            };
            for failure in report["failures"].as_array().into_iter().flatten() {
                scanner.apply("source.reading.scanFailure",&json!({"relativePath":failure["relativePath"],"error":{"code":failure["errorCode"],"archive":report["archive"]},"aborted":false}),&token)
                    .map_err(|_|gateway::error("压缩包覆盖记录未保存。","RETRIEVAL_SCAN_FAILED",409))?;
            }
            continue;
        }
        let document = crate::tools::service_source_reading::document_format(filename).is_some();
        let loaded = if !document
            && next["metadata"]["sizeBytes"].as_u64().unwrap_or(0) > 2 * 1024 * 1024
        {
            execute_io("describeTextWindows",&input,&token).map_err(|_|gateway::error("大文件窗口读取失败。","RETRIEVAL_SOURCE_UNAVAILABLE",422)).and_then(|value|{
                let windows:Vec<Value>=serde_json::from_str(value["windowsWire"].as_str().unwrap()).map_err(|_|gateway::error("文件窗口描述无效。","INVALID_RETRIEVAL_WINDOW",422))?;
                let mut result=Vec::new();for window in windows{let text=execute_io("readTextWindow",&json!({"path":next["path"],"window":window,"maximumSourceBytes":limits["maximumSourceBytes"]}),&token)
                    .map_err(|_|gateway::error("文件窗口已改变。","STALE_RETRIEVAL_SOURCE",409))?;result.push(json!({"text":text["text"],"fileWindow":window,"metadata":value["metadata"]}));}Ok(result)
            })
        } else {
            super::gateway_documents::read_with_config(config, filename, limits, cancel).map(
                |value| {
                    value["documentWindows"]
                        .as_array()
                        .cloned()
                        .unwrap_or(vec![value])
                },
            )
        };
        match loaded {
            Ok(values) => {
                for value in values {
                    let text = value["text"].as_str().unwrap();
                    let admitted=scanner.apply("source.reading.scanPublish",&json!({"textBytes":text.len(),"size":next["metadata"]["sizeBytes"],"pdf":value["extraction"]["format"]=="pdf"}),&token)
                        .map_err(|failure|gateway::error(&failure.message,"RETRIEVAL_SCAN_LIMIT",422))?;
                    if admitted == false {
                        break 'files;
                    }
                    let digest = gateway_index::hash(text);
                    let mut locator = json!({"relativePath":next["relativePath"],"path":next["path"],"root":path});
                    if let Some(extraction) = value.get("extraction") {
                        locator["extraction"] = extraction.clone();
                    }
                    if let Some(window) = value.get("fileWindow") {
                        locator["fileWindow"] = window.clone();
                    }
                    let revision = source_revision(&digest, &locator)?;
                    consume(
                        json!({"title":filename.file_name().unwrap().to_string_lossy(),"text":text,"contentHash":digest,"sourceRevision":revision,"fileMetadata":value["metadata"],"locator":locator}),
                    )?;
                }
            }
            Err(error) => {
                let mut failure = json!({"code":error.code()});
                if document && (error.code().starts_with("OCR_") || error.code().starts_with("DOCUMENT_")) && !cancel.load(Ordering::Acquire) {
                    let encoding = crate::tools::service_source_reading::document_format(filename).unwrap();
                    let mut coverage = json!({"format":if ["pdf","docx"].contains(&encoding){encoding}else{"image"},
                        "complete":false,"reasonCode":error.code(),"processedPages":0,"publishedPages":0,
                        "uncoveredRanges":[],"unprocessedRanges":[],"readback":{"kind":"original-document"},
                        "effectiveLimits":{"maximumInputBytes":limits["maximumDocumentInputBytes"].as_u64().unwrap_or(32*1024*1024),
                            "maximumOutputBytes":limits["maximumDocumentOutputBytes"].as_u64().unwrap_or(2*1024*1024),
                            "maximumPages":if encoding=="pdf"{limits["maximumPdfPages"].as_u64().unwrap_or(100)}else{1}}});
                    if coverage["format"]=="image" {coverage["encoding"]=json!(encoding);}
                    if let Ok((bytes, metadata)) = crate::tools::service_source_reading::read_binary_stable(filename,
                        limits["maximumDocumentInputBytes"].as_u64().unwrap_or(32*1024*1024)
                            .min(limits["maximumSourceBytes"].as_u64().unwrap_or(32*1024*1024)).min(32*1024*1024), &token) {
                        if scanner.apply("source.reading.sameMetadata", &json!({"previous":next["metadata"],"current":metadata}), &token)
                            .map_err(|failure|gateway::error(&failure.message,"RETRIEVAL_SOURCE_UNAVAILABLE",failure.status_code))? == true {
                            coverage["rawContentHash"] = json!(gateway_index::hash_bytes(&bytes));
                        }
                    }
                    failure["documentCoverage"] = coverage;
                }
                scanner.apply("source.reading.scanFailure",&json!({"relativePath":next["relativePath"],"error":failure,"aborted":cancel.load(Ordering::Acquire)}),&token)
                .map_err(|_|gateway::error("来源读取受阻。","RETRIEVAL_SOURCE_UNAVAILABLE",409))?;
            }
        }
    }
    let stats = scanner
        .apply("source.reading.scanStats", &json!({}), &token)
        .map_err(|_| gateway::error("扫描状态无效。", "RETRIEVAL_SOURCE_UNAVAILABLE", 409))?;
    scanner
        .apply("source.reading.scanClose", &json!({}), &token)
        .map_err(|_| gateway::error("扫描关闭失败。", "RETRIEVAL_SOURCE_UNAVAILABLE", 409))?;
    Ok(stats)
}
fn source_revision(digest: &str, locator: &Value) -> Result<String, ContractError> {
    if let Some(window) = locator.get("fileWindow") {
        storage::retrieval::apply("storage.retrieval.fileWindow", &json!({"value":window}))?;
        return Ok(gateway_index::hash(
            &json!([
                window["rawContentHash"],
                window["startOffset"],
                window["endOffset"],
                window["contentHash"]
            ])
            .to_string(),
        ));
    }
    if let Some(extraction) = locator.get("extraction") {
        storage::retrieval::apply("storage.retrieval.extraction", &json!({"value":extraction}))?;
        let mut identity = vec![
            json!(digest),
            extraction["version"].clone(),
            extraction["rawContentHash"].clone(),
            extraction["pages"].clone(),
        ];
        if extraction.get("pageWindow").is_some() || extraction.get("ocr").is_some() {
            identity.push(extraction["pageWindow"].clone());
            identity.push(extraction["ocr"].clone());
        }
        if extraction["format"] == "image" {
            identity.push(extraction["image"].clone());
            identity.push(extraction["textRange"].clone());
        }
        return Ok(gateway_index::hash(&json!(identity).to_string()));
    }
    Ok(digest.to_string())
}
pub fn handle(
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
    config: &GatewayConfig,
) -> Option<GatewayResult> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let project_settings = parts.len() == 5
        && parts[0] == "api"
        && parts[1] == "projects"
        && parts[3] == "retrieval"
        && parts[4] == "settings";
    if path == "/api/retrieval/settings" || project_settings {
        let project = if project_settings {
            Some(parts[2])
        } else {
            None
        };
        return Some((|| {
            let (_, mut result) = gateway::with_catalog(&config.data_root, |catalog, _| {
                let filename = project
                    .map(|id| {
                        config
                            .data_root
                            .join("Projects")
                            .join(id)
                            .join("retrieval.json")
                    })
                    .unwrap_or(config.data_root.join("Retrieval/settings.json"));
                let relationship = project
                    .map(|id| {
                        storage::catalog::apply(
                            "storage.catalog.project",
                            &json!({"current":catalog,"id":id}),
                        )
                    })
                    .transpose()?;
                let (value, hash) =
                    gateway::read_json(&config.data_root, &filename, 8 * 1024 * 1024)?;
                let current = match value {
                    Some(value) => settings::apply(
                        "storage.settings.document",
                        &json!({"value":value,"projectId":project}),
                    )?,
                    None => settings::apply(
                        if project.is_some() {
                            "storage.settings.defaultProject"
                        } else {
                            "storage.settings.defaults"
                        },
                        &json!({"projectId":project}),
                    )?,
                };
                let value = if method == "GET" {
                    current
                } else if method == "PATCH" {
                    let mutation = gateway::normalize_transport_numbers(settings::apply(
                        "storage.settings.mutation",
                        &json!({"value":body,"project":project.is_some()}),
                    )?);
                    let next = settings::apply(
                        "storage.settings.next",
                        &json!({"current":current,"mutation":mutation,"project":project.is_some(),"folderless":relationship.as_ref().map(|value|value["isFolderlessWorkspace"]==true).unwrap_or(false)}),
                    )?;
                    gateway::commit_json(
                        &config.data_root,
                        &filename,
                        &next,
                        hash.as_deref(),
                        8 * 1024 * 1024,
                    )?;
                    next
                } else {
                    return Err(gateway::error("此方法不支持。", "METHOD_NOT_ALLOWED", 405));
                };
                Ok((200, value))
            })?;
            if project.is_some() {
                result["effective"] = effective(config, project)?;
            }
            Ok((200, result))
        })());
    }
    if path == "/api/retrieval/status" && method == "GET" {
        return Some((|| {
            let index = gateway_index::status(config)?;
            let current = effective(config, gateway::query_value(query, "projectId"))?;
            Ok((
                200,
                json!({"backend":"sqlite","sourceCount":index["sources"],"chunkCount":index["chunks"],"embedding":super::gateway_embedding::status(config,&current["local"]),
                "jobs":job_plan(config,"read",json!({}))?["document"]["jobs"],"configuredEmbeddingProfileId":current["local"]["embeddingProfileId"],
                "deployment":{"platform":"win32","nativeGateway":true,"database":"sqlite","ann":"usearch"}}),
            ))
        })());
    }
    if path == "/api/retrieval/models" && method == "GET" {
        return Some(Ok((200, super::gateway_embedding::profiles(config))));
    }
    if path == "/api/retrieval/index/rebuild" && method == "POST" {
        return Some(rebuild(config, body).map(|value| (200, value)));
    }
    if parts.len() >= 5 && parts[..4] == ["api", "retrieval", "index", "jobs"] {
        let id = parts[4];
        if parts.len() == 6 && parts[5] == "cancel" && method == "POST" {
            if let Some(token) = CANCELLATIONS
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .get(id)
            {
                token.store(true, Ordering::Release);
            }
            return Some(job_update(config,id,json!({"status":"cancelled","error":"INDEX_CANCELLED","automaticRebuildBlocked":true,"finishedAt":gateway::now()})).map(|value|(200,value)));
        }
        if parts.len() == 5 && method == "GET" {
            return Some(job_plan(config, "get", json!({"jobId":id})).map(|value| (200, value)));
        }
    }
    if path == "/api/retrieval/search" && method == "POST" {
        return Some(
            super::gateway_tools::handle_tool(
                "knowledge.search",
                body,
                &json!({"conversationId":body["conversationId"],"requestId":gateway::new_id()}),
                config,
            )
            .unwrap()
            .map(|value| (200, value)),
        );
    }
    if ["/api/retrieval/sources", "/api/retrieval/sources/import"].contains(&path)
        && method == "POST"
    {
        return Some(import(config, body).map(|value| {let first=&value["sources"][0];(200,json!({"id":first["id"],"title":first["title"],"scope":first["scope"],"projectId":first["projectId"],"path":first["originalPath"],"status":first["status"],"revision":first["revision"],"importedCount":value["sources"].as_array().unwrap().len(),"jobId":value["job"]["jobId"],"sourceCoverage":{"discovered":value["scan"]["scannedFiles"],"failed":value["scan"]["failedFiles"],"failures":value["scan"]["failures"],"complete":value["scan"]["incomplete"]!=true&&value["scan"]["failedFiles"].as_u64().unwrap_or(0)==0}}))}));
    }
    if parts.len() == 4 && parts[..3] == ["api", "retrieval", "sources"] && method == "DELETE" {
        return Some(remove_source(config, parts[3], body).map(|value| (200, value)));
    }
    if path == "/api/retrieval/sources" && method == "GET" {
        return Some(gateway::with_catalog(&config.data_root, |_, _| {
            let (document, _) = registry(&config.data_root)?;
            let value = library::apply(
                "storage.library.list",
                &json!({"document":document,"projectId":gateway::query_value(query,"projectId"),"errors":{}}),
            )?;
            Ok((
                200,
                json!({"revision":value["revision"],"sources":value["items"].as_array().unwrap().iter().map(|item|item["value"].clone()).collect::<Vec<_>>()}),
            ))
        }));
    }
    None
}

#[cfg(test)]
mod gateway_tests {
    use super::*;
    struct Workspace {
        root: std::path::PathBuf,
        config: GatewayConfig,
    }
    impl Workspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("kynxa-retrieval-{}", gateway::new_id()));
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
            storage::gateway_memory::stop_candidates(&self.config.data_root);
            let _ = storage::sqlite_queries::apply(
                "storage.index.closeConnections",
                &gateway_index::arguments(&self.config, json!({})),
            );
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn scanner_honors_partial_limits_and_publishes_actual_text() {
        let workspace = Workspace::new();
        let folder = workspace.root.join("工作 中文");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), "当前代码和文档 alpha").unwrap();
        std::fs::write(folder.join("b.txt"), "另一个有效来源 beta").unwrap();
        let mut limits = storage::settings::apply("storage.settings.defaults", &json!({})).unwrap()
            ["local"]["indexing"]
            .clone();
        limits["maximumFiles"] = json!(1);
        let mut found = Vec::new();
        let report = scan_each(
            &workspace.config,
            folder.to_str().unwrap(),
            &limits,
            &Arc::new(AtomicBool::new(false)),
            |source| {
                found.push(source);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["text"], "当前代码和文档 alpha");
        assert_eq!(report["incomplete"], true);
        assert_eq!(report["scannedFiles"], 2);
        assert!(found[0]["fileMetadata"].is_object());
    }
    #[test]
    fn deleted_chat_retires_only_its_derived_rows_and_undo_rebuilds_from_preserved_sources() {
        let workspace = Workspace::new();
        let root = &workspace.config.data_root;
        let chat = gateway::new_id();
        let retained = gateway::new_id();
        let project = gateway::new_id();
        let message = gateway::new_id();
        gateway::ensure_conversation(root, &chat, "删除夹具").unwrap();
        gateway::ensure_conversation(root, &retained, "保留夹具").unwrap();
        gateway::upsert_message(root,&chat,&json!({"Id":message,"Role":"user","Content":"DELETE_CHAT_SOURCE","CreatedAt":gateway::now(),"Status":"completed"})).unwrap();
        let work = workspace.root.join("user-work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("original.txt"), b"user-owned-source").unwrap();
        gateway::with_catalog(root, |catalog, digest| {
            let mut next = catalog.clone();
            next["Projects"]
                .as_array_mut()
                .unwrap()
                .push(json!({"Id":project,"Name":"共享项目","FolderPath":work,"Chats":[]}));
            next["Revision"] = json!(catalog["Revision"].as_u64().unwrap() + 1);
            gateway::commit_catalog(root, &next, digest, &[], &[]).map(|_| ())
        })
        .unwrap();
        let active = gateway::load_catalog(root).unwrap();
        let chat_path =
            storage::conversation_logs::directory(root, &gateway::locate(&active, &chat).unwrap())
                .unwrap();
        std::fs::write(chat_path.join("context.json"), b"{\"fixtureSummary\":true}").unwrap();
        std::fs::write(
            chat_path.join("attachments/fixture.json"),
            b"{\"fixtureToolArchive\":true}",
        )
        .unwrap();
        let now = gateway::now();
        for (scope, id, path) in [
            ("user", "user".to_string(), root.join("Memory/entries.json")),
            (
                "project",
                project.clone(),
                root.join("Projects")
                    .join(&project)
                    .join("Memory/entries.json"),
            ),
            ("chat", chat.clone(), chat_path.join("Memory/entries.json")),
        ] {
            gateway::commit_json(root,&path,&json!({"schemaVersion":1,"scope":scope,"scopeId":id,"revision":1,"entries":[{"id":gateway::new_id(),"scope":scope,"scopeId":id,"content":"已确认共享夹具事实","kind":"fact","status":"confirmed","revision":1,"createdAt":now,"updatedAt":now,"source":{"type":"manual","role":"user"}}],"dismissedSources":[]}),None,8*1024*1024).unwrap();
        }
        let source = json!({"sourceId":"deleted-message","scopeKey":format!("chat:{chat}"),"sourceType":"message","title":"same-title","locator":{"conversationId":chat,"messageId":message},"text":"DELETE_CHAT_SOURCE","embeddingProfileId":"fixture-profile","embeddingModelVersion":"fixture-v1","embeddingSpaceId":"fixture-space"});
        let prepared = gateway_index::prepare_source(&source, Some(&[vec![1.0, 0.0]])).unwrap();
        gateway_index::publish_prepared(&workspace.config, &[prepared.clone()]).unwrap();
        let keep = [
            json!({"sourceId":"user-source","scopeKey":"user","locator":{"conversationId":chat},"text":"shared imported fixture"}),
            json!({"sourceId":"project-source","scopeKey":format!("project:{project}"),"locator":{"conversationId":chat,"path":work.join("original.txt")},"text":"shared work fixture"}),
            json!({"sourceId":"retained-message","sourceType":"message","scopeKey":format!("chat:{retained}"),"text":"retained fixture"}),
        ];
        gateway_index::publish(&workspace.config, &keep).unwrap();
        let mut settings = settings::apply("storage.settings.defaults", &json!({})).unwrap();
        settings["local"]["semantic"] = json!("off");
        gateway::commit_json(
            root,
            &root.join("Retrieval/settings.json"),
            &settings,
            None,
            8 * 1024 * 1024,
        )
        .unwrap();
        maintain_memory_scope(
            &workspace.config,
            "chat",
            &chat,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let ann = root.join("Index/ann");
        std::fs::create_dir_all(&ann).unwrap();
        let deleted_key = "a".repeat(64);
        let shared_key = "b".repeat(64);
        for (key, scope) in [
            (&deleted_key, format!("chat:{chat}")),
            (&shared_key, "user".to_string()),
        ] {
            std::fs::write(
                ann.join(format!("{key}.json")),
                json!({"identity":{"scopeKey":scope}}).to_string(),
            )
            .unwrap();
            std::fs::write(
                ann.join(format!("{key}.usearch")),
                b"disposable-fixture-cache",
            )
            .unwrap();
        }
        let full = gateway::handle("GET", "/api/conversations/catalog", &[], &Value::Null, root)
            .unwrap()
            .unwrap()
            .1;
        let mut without = full.clone();
        without["Chats"]
            .as_array_mut()
            .unwrap()
            .retain(|entry| entry["Id"] != chat);
        gateway::handle("PUT", "/api/conversations/catalog", &[], &without, root)
            .unwrap()
            .unwrap();
        assert_eq!(
            gateway::read_model_messages(root, &chat)
                .unwrap_err()
                .status_code(),
            410
        );
        assert_eq!(
            gateway::read_model_summary(root, &chat)
                .unwrap_err()
                .status_code(),
            410
        );
        assert_eq!(
            gateway_index::publish_prepared(&workspace.config, &[prepared.clone()])
                .unwrap_err()
                .status_code(),
            410
        );
        let tombstone = gateway::load_catalog(root).unwrap()["Tombstones"][0].clone();
        let mut cursor = 0;
        maintain_deleted_chats(
            &workspace.config,
            &Arc::new(AtomicBool::new(false)),
            &mut cursor,
        )
        .unwrap();
        assert_eq!(
            gateway_index::status(&workspace.config).unwrap()["sources"],
            3
        );
        assert_eq!(
            gateway_index::status(&workspace.config).unwrap()["vectorChunks"],
            0
        );
        assert!(sqlite_sources(&workspace.config,&format!("chat:{chat}"),&Value::Null).unwrap()["rows"].as_array().unwrap().is_empty());
        let identities = gateway::read_json(
            root,
            &root.join("Retrieval/source-identities.json"),
            32 * 1024 * 1024,
        )
        .unwrap()
        .0
        .unwrap();
        assert!(
            !identities["sources"]
                .as_object()
                .unwrap()
                .values()
                .any(|entry| entry["scopeKey"] == format!("chat:{chat}"))
        );
        assert!(!ann.join(format!("{deleted_key}.usearch")).exists());
        assert!(ann.join(format!("{shared_key}.usearch")).exists());
        for suffix in [
            "events.jsonl",
            "context.json",
            "attachments/fixture.json",
            "Memory/entries.json",
        ] {
            assert!(
                root.join("Trash").join(&chat).join(suffix).exists(),
                "missing preserved {suffix}"
            );
        }
        assert_eq!(
            std::fs::read(work.join("original.txt")).unwrap(),
            b"user-owned-source"
        );
        assert_eq!(
            storage::gateway_memory::context(root, &retained).unwrap()["entries"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(storage::gateway_memory::read_index_document(root,"project",&project).unwrap()["entries"].as_array().unwrap().len(),1);
        let mut restored = full;
        restored["Revision"] = gateway::load_catalog(root).unwrap()["Revision"].clone();
        gateway::handle("PUT", "/api/conversations/catalog", &[], &restored, root)
            .unwrap()
            .unwrap();
        assert_eq!(
            gateway_index::retire_deleted_chat(&workspace.config, &tombstone).unwrap()["retired"],
            false
        );
        assert_eq!(
            gateway::read_model_messages(root, &chat).unwrap()[0]["Content"],
            "DELETE_CHAT_SOURCE"
        );
        gateway_index::publish_prepared(&workspace.config, &[prepared]).unwrap();
        assert_eq!(
            gateway_index::status(&workspace.config).unwrap()["vectorChunks"],
            1
        );
    }
    #[test]
    fn deleted_chat_database_cleanup_is_batched_and_writer_admission_and_sql_share_the_deadline() {
        let workspace = Workspace::new();
        let chat = gateway::new_id();
        gateway::ensure_conversation(&workspace.config.data_root, &chat, "有界删除夹具").unwrap();
        let scope = format!("chat:{chat}");
        for page in 0..3 {
            let sources = (0..100).map(|index|json!({"sourceId":format!("fixture-message-{}",page*100+index),"scopeKey":scope,"sourceType":"message","text":"BATCHED_CHAT_CLEANUP"})).collect::<Vec<_>>();
            gateway_index::publish(&workspace.config, &sources).unwrap();
        }
        let args = gateway_index::arguments(&workspace.config, json!({"scopeKey":scope}));
        let held = storage::sqlite_connections::acquire_write(
            &workspace.config.data_root,
            &gateway_index::database(&workspace.config),
            1000,
        )
        .unwrap();
        assert_eq!(
            storage::sqlite_index::apply("storage.index.retireChat", &args)
                .unwrap_err()
                .code(),
            "RETRIEVAL_SEARCH_TIMEOUT"
        );
        drop(held);
        let first = storage::sqlite_index::apply("storage.index.retireChat", &args).unwrap();
        assert_eq!(first["hasMore"], true);
        assert!(first["removedChunks"].as_u64().unwrap() <= 128);
        assert!(first["removed"].as_u64().unwrap() <= 16);
        let mut complete = false;
        for _ in 0..32 {
            if storage::sqlite_index::apply("storage.index.retireChat", &args).unwrap()["complete"]
                == true
            {
                complete = true;
                break;
            }
        }
        assert!(complete);
        let sql = storage::sqlite_connections::acquire_write(
            &workspace.config.data_root,
            &gateway_index::database(&workspace.config),
            100,
        )
        .unwrap();
        let error = sql.query_row("WITH RECURSIVE spin(x) AS (SELECT 0 UNION ALL SELECT x+1 FROM spin WHERE x<10000000) SELECT sum(x) FROM spin",[],|row|row.get::<_,i64>(0)).unwrap_err();
        assert!(
            matches!(error,rusqlite::Error::SqliteFailure(failure,_) if failure.code==rusqlite::ErrorCode::OperationInterrupted)
        );
    }
    #[test]
    fn fresh_database_removal_cleans_coverage_without_an_explicit_coverage_initializer() {
        let workspace = Workspace::new();
        let source = json!({"sourceId":"fresh-source","scopeKey":"user","sourceType":"document", "title":"Fresh source","locator":{"relativePath":"fresh.txt"},"text":"Fresh lexical source content"});
        gateway_index::publish(&workspace.config, &[source]).unwrap();
        let removed = storage::sqlite_index::apply(
            "storage.index.remove",
            &gateway_index::arguments(
                &workspace.config,
                json!({"sourceId":"fresh-source","scopeKey":"user"}),
            ),
        )
        .unwrap();
        assert_eq!(removed["removed"], true);
        assert_eq!(
            gateway_index::status(&workspace.config).unwrap()["sources"],
            0
        );
        let coverage = storage::source_coverage::apply(
            "storage.coverage.query",
            &gateway_index::arguments(&workspace.config, json!({"scopes":["user"],"limit":20})),
        )
        .unwrap();
        assert_eq!(coverage["counts"]["discovered"], 0);
        storage::sqlite_index::apply(
            "storage.index.invalidate",
            &gateway_index::arguments(&workspace.config, json!({"scopeKey":"user"})),
        )
        .unwrap();
    }
    #[test]
    fn project_settings_and_status_preserve_desktop_contract() {
        let workspace = Workspace::new();
        let project = gateway::new_id();
        gateway::with_catalog(&workspace.config.data_root,|document,hash|{let mut next=document.clone();next["Projects"].as_array_mut().unwrap().push(json!({"Id":project,"Name":"项目","FolderPath":null,"IsFolderlessWorkspace":true,"Chats":[]}));next["Revision"]=json!(1);gateway::commit_catalog(&workspace.config.data_root,&next,hash,&[],&[]).map(|_|())}).unwrap();
        let settings = handle(
            "GET",
            &format!("/api/projects/{project}/retrieval/settings"),
            &[],
            &json!({}),
            &workspace.config,
        )
        .unwrap()
        .unwrap()
        .1;
        assert!(settings["effective"]["local"].is_object());
        assert_eq!(settings["projectId"], project);
        for expected_revision in 0..2 {
            let result = handle(
                "PATCH",
                &format!("/api/projects/{project}/retrieval/settings"),
                &[],
                &json!({"expectedRevision":expected_revision as f64,"patch":{"overrides":{"web":{"language":if expected_revision==0{"zh-CN"}else{"en"}}}}}),
                &workspace.config,
            )
            .unwrap()
            .unwrap()
            .1;
            assert_eq!(result["revision"].as_u64(), Some(expected_revision + 1));
        }
        let stored = gateway::read_json(
            &workspace.config.data_root,
            &workspace
                .config
                .data_root
                .join("Projects")
                .join(&project)
                .join("retrieval.json"),
            8 * 1024 * 1024,
        )
        .unwrap()
        .0
        .unwrap();
        assert_eq!(stored["revision"].as_u64(), Some(2));
        let status = handle(
            "GET",
            "/api/retrieval/status",
            &[],
            &json!({}),
            &workspace.config,
        )
        .unwrap()
        .unwrap()
        .1;
        assert_eq!(status["backend"], "sqlite");
        assert_eq!(status["sourceCount"], 0);
        assert_eq!(status["chunkCount"], 0);
        assert_eq!(status["embedding"]["loaded"], false);
        assert!(status["jobs"].is_array());
    }
    #[test]
    fn unchanged_inventory_retries_only_temporary_semantic_failures_after_backoff() {
        let now = chrono::Utc::now();
        let signature = "a".repeat(64);
        let mut job = json!({"projectId":null,"status":"partial","automaticRebuildScope":signature,"automaticRetryAttempt":0,
            "finishedAt":(now-chrono::Duration::seconds(31)).to_rfc3339(),"coverage":{"complete":true},"semantic":{"skippedSources":2,"diagnosticCodes":["RESOURCE_WAIT_TIMEOUT"]}});
        let attempt = maintenance_attempt(&[job.clone()], None, &signature, false, now).unwrap();
        assert!(attempt.semantic_only);
        assert_eq!(attempt.attempt, 1);
        job["finishedAt"] = json!(now.to_rfc3339());
        assert!(maintenance_attempt(&[job.clone()], None, &signature, false, now).is_none());
        job["finishedAt"] = json!((now - chrono::Duration::hours(1)).to_rfc3339());
        job["semantic"]["diagnosticCodes"] = json!(["EMBEDDING_ASSET_INVALID"]);
        assert!(maintenance_attempt(&[job.clone()], None, &signature, false, now).is_none());
        job["semantic"]["diagnosticCodes"] = json!(["RESOURCE_WAIT_TIMEOUT"]);
        job["automaticRebuildBlocked"] = json!(true);
        assert!(maintenance_attempt(&[job.clone()], None, &signature, true, now).is_none());
        job["automaticRebuildBlocked"] = json!(false);
        job["status"] = json!("cancelled");
        assert!(maintenance_attempt(&[job], None, &signature, false, now).is_none());
        assert_eq!(retry_delay(10), std::time::Duration::from_secs(900));
    }
    #[test]
    fn startup_memory_joins_existing_queue_and_indexes_confirmed_scopes_without_cold_models() {
        let workspace = Workspace::new();
        let project = gateway::new_id();
        let chat = gateway::new_id();
        let message = gateway::new_id();
        let folder = workspace.root.join("work");
        std::fs::create_dir_all(&folder).unwrap();
        gateway::ensure_conversation(&workspace.config.data_root, &chat, "历史聊天").unwrap();
        gateway::upsert_message(
            &workspace.config.data_root,
            &chat,
            &json!({"Id":message,"Role":"user","Content":"这是一条未确认候选。"}),
        )
        .unwrap();
        gateway::with_catalog(&workspace.config.data_root,|catalog,digest|{let mut next=catalog.clone();next["Projects"].as_array_mut().unwrap().push(json!({"Id":project,"Name":"工作","FolderPath":folder,"IsFolderlessWorkspace":false,"Chats":[]}));next["Revision"]=json!(catalog["Revision"].as_u64().unwrap()+1);gateway::commit_catalog(&workspace.config.data_root,&next,digest,&[],&[]).map(|_|())}).unwrap();
        let now = gateway::now();
        let draft = json!({"id":gateway::new_id(),"scope":"chat","scopeId":chat,"content":"这是一条未确认候选。","kind":"fact","status":"draft","revision":1,"createdAt":now,"updatedAt":now,"source":{"type":"user-message","role":"user","conversationId":chat,"messageId":message},"candidate":{"algorithm":"conservative-extractive-v1","trigger":"remember","fingerprint":"a".repeat(64),"quotes":[{"messageId":message,"text":"这是一条未确认候选。"}]}});
        let location = gateway::with_catalog(&workspace.config.data_root, |catalog, _| {
            gateway::locate(catalog, &chat)
        })
        .unwrap();
        for (scope, id, path, entries) in [
            (
                "user",
                "user".to_string(),
                workspace.config.data_root.join("Memory/entries.json"),
                Value::Null,
            ),
            (
                "project",
                project.clone(),
                workspace
                    .config
                    .data_root
                    .join("Projects")
                    .join(&project)
                    .join("Memory/entries.json"),
                Value::Null,
            ),
            (
                "chat",
                chat.clone(),
                storage::conversation_logs::directory(&workspace.config.data_root, &location)
                    .unwrap()
                    .join("Memory/entries.json"),
                json!([draft]),
            ),
        ] {
            let entries = if entries.is_null() {
                json!([{"id":gateway::new_id(),"scope":scope,"scopeId":id,"content":format!("已确认 {scope} 记忆"),"kind":"fact","status":"confirmed","revision":1,"createdAt":now,"updatedAt":now,"source":{"type":"manual","role":"user"}}])
            } else {
                entries
            };
            gateway::commit_json(&workspace.config.data_root,&path,&json!({"schemaVersion":1,"scope":scope,"scopeId":id,"revision":1,"entries":entries,"dismissedSources":[]}),None,8*1024*1024).unwrap();
        }
        let mut settings = settings::apply("storage.settings.defaults", &json!({})).unwrap();
        settings["local"]["semantic"] = json!("off");
        gateway::commit_json(
            &workspace.config.data_root,
            &workspace.config.data_root.join("Retrieval/settings.json"),
            &settings,
            None,
            8 * 1024 * 1024,
        )
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let discovered =
            storage::gateway_memory::startup_index_scopes(&workspace.config.data_root, &cancel)
                .unwrap();
        assert_eq!(discovered["failed"], 0);
        assert_eq!(discovered["scopes"].as_array().unwrap().len(), 3);
        let mut queue = discovered["scopes"]
            .as_array()
            .unwrap()
            .clone()
            .into_iter()
            .collect();
        enqueue_startup_memory(&workspace.config.data_root, &mut queue);
        maintain_memory(&workspace.config, &cancel).unwrap();
        let status = gateway_index::status(&workspace.config).unwrap();
        assert_eq!(status["sources"], 2);
        assert_eq!(status["vectorChunks"], 0);
        let chat_sources =
            sqlite_sources(&workspace.config, &format!("chat:{chat}"), &Value::Null).unwrap();
        assert!(chat_sources["rows"].as_array().unwrap().is_empty());
        cancel.store(true, Ordering::Release);
        assert_eq!(
            storage::gateway_memory::startup_index_scopes(&workspace.config.data_root, &cancel)
                .unwrap_err()
                .code(),
            "INDEX_CANCELLED"
        );
    }
}
