//! Canonical data APIs for the independent Rust gateway; transport code owns HTTP framing.
//! 独立 Rust 网关的正式数据 API；HTTP 解码与流传输由传输层负责。

use super::{
    ContractError, ContractResult, catalog, catalog_transactions, conversation_history,
    conversation_logs, layout, managed_files, raw_json,
};
use serde_json::{Value, json};
use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub type GatewayResult = Result<(u16, Value), ContractError>;
static OWNERS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
const MAXIMUM_CATALOG_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_INITIALIZED_ROOTS: usize = 128;
struct CatalogSnapshot {
    root: PathBuf,
    version: String,
    hash: String,
    document: Value,
    bytes: usize,
}
struct LayoutSnapshot {
    root: PathBuf,
    plan: String,
    manifests: Vec<Option<String>>,
}
static CATALOG_SNAPSHOTS: Mutex<Vec<CatalogSnapshot>> = Mutex::new(Vec::new());
static LAYOUT_SNAPSHOTS: Mutex<Vec<LayoutSnapshot>> = Mutex::new(Vec::new());
static IMAGE_CLEANUP_CONVERSATIONS: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());
#[cfg(test)]
static MODEL_READ_OBSERVERS: Mutex<Vec<(PathBuf, Arc<dyn Fn(&str) + Send + Sync>)>> = Mutex::new(Vec::new());

pub fn now() -> String {
    crate::gateway_contract::now_iso8601()
}
pub fn new_id() -> String {
    crate::gateway_contract::new_id()
}
pub(crate) fn error(message: &str, code: &'static str, status: u16) -> ContractError {
    ContractError::new(message, code, status)
}
pub fn query_value<'a>(query: &'a [(String, String)], name: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}
/// The former Number transport serialized integral safe floats as integers; preserve that public contract.
/// 历史 Number 传输将安全范围的整数浮点数编码成整数；保留公开协议而不改写私有原始日志。
pub fn normalize_transport_numbers(mut value: Value) -> Value {
    fn visit(value: &mut Value) {
        match value {
            Value::Number(number) => {
                if number.is_f64() {
                    if let Some(number) = number.as_f64().filter(|number| {
                        number.is_finite()
                            && number.fract() == 0.0
                            && number.abs() <= 9_007_199_254_740_991.0
                    }) {
                        *value = json!(number as i64);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item);
                }
            }
            Value::Object(fields) => {
                for value in fields.values_mut() {
                    visit(value);
                }
            }
            _ => {}
        }
    }
    visit(&mut value);
    value
}

pub(crate) fn read_json(
    root: &Path,
    path: &Path,
    maximum: u64,
) -> Result<(Option<Value>, Option<String>), ContractError> {
    let bytes = managed_files::read(root, path, maximum)?;
    match bytes {
        None => Ok((None, None)),
        Some(bytes) => {
            let raw = String::from_utf8(bytes.clone())
                .map_err(|_| error("正式 JSON 编码无效，原文件已保留。", "CORRUPT_STORAGE", 409))?;
            let value = serde_json::from_str(&raw_json::scalar_projection(
                raw.trim_start_matches('\u{feff}'),
            ))
            .map_err(|_| error("正式 JSON 格式无效，原文件已保留。", "CORRUPT_STORAGE", 409))?;
            Ok((
                Some(normalize_transport_numbers(value)),
                Some(managed_files::digest(&bytes)),
            ))
        }
    }
}
pub(crate) fn commit_json(
    root: &Path,
    path: &Path,
    value: &Value,
    hash: Option<&str>,
    maximum: u64,
) -> ContractResult {
    // Preserve untouched formal UTF-16 fields; the atomic write still compares the caller's original digest.
    // 保留正式文件未修改字段的原始 UTF-16 编码；原子写入仍核对调用方原快照哈希。
    let original = managed_files::read(root, path, maximum)?
        .map(String::from_utf8)
        .transpose()
        .map_err(|_| error("正式 JSON 编码无效，原文件已保留。", "CORRUPT_STORAGE", 409))?;
    let originals = original
        .as_deref()
        .map(|raw| raw.trim_start_matches('\u{feff}'))
        .into_iter()
        .collect::<Vec<_>>();
    let encoded = encode_preserving(&normalize_transport_numbers(value.clone()), &originals);
    managed_files::apply(
        "storage.files.commit",
        &json!({"root":root,"path":path,
        "raw":encoded,"expectedHash":hash,"maximumBytes":maximum}),
    )
}
pub(crate) fn timestamps(value: &Value) -> Value {
    fn visit(value: &Value, dates: &mut serde_json::Map<String, Value>) {
        match value {
            Value::String(text) => {
                dates.insert(
                    text.clone(),
                    json!(chrono::DateTime::parse_from_rfc3339(text).is_ok()),
                );
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, dates);
                }
            }
            Value::Object(fields) => {
                for item in fields.values() {
                    visit(item, dates);
                }
            }
            _ => {}
        }
    }
    let mut dates = serde_json::Map::new();
    visit(value, &mut dates);
    Value::Object(dates)
}

pub(crate) fn locations(document: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    for chat in document["Chats"].as_array().into_iter().flatten() {
        result.push(json!({"Id":chat["Id"],"ProjectId":null,"Chat":chat}));
    }
    for project in document["Projects"].as_array().into_iter().flatten() {
        for chat in project["Chats"].as_array().into_iter().flatten() {
            result.push(json!({"Id":chat["Id"],"ProjectId":project["Id"],"Chat":chat}));
        }
    }
    result
}
pub(crate) fn locate(document: &Value, id: &str) -> ContractResult {
    catalog::apply(
        "storage.catalog.checkAlive",
        &json!({"current":document,"id":id}),
    )?;
    locations(document)
        .into_iter()
        .find(|item| {
            item["Id"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(id))
        })
        .ok_or_else(|| error("聊天不存在。", "CONVERSATION_NOT_FOUND", 404))
}
pub fn relationship(root: &Path, id: &str) -> ContractResult {
    with_catalog(root, |document, _| relationship_from_catalog(document, id))
}

fn relationship_from_catalog(document: &Value, id: &str) -> ContractResult {
    let mut relation = catalog::apply(
        "storage.catalog.relationship",
        &json!({"current":document,"id":id}),
    )?;
    // Resolve the work folder from the same authoritative catalog snapshot as its scope identity.
    // 从范围身份对应的同一正式目录快照读取工作路径；不能因缺少路径字段误用聊天沙箱。
    let project = document["Projects"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|project| project["Id"] == relation["projectId"]);
    let mounts = super::workspace_mounts::relationship(project.unwrap_or(&Value::Null))?;
    for key in ["folderPath", "mounts", "defaultMountId"] { relation[key] = mounts[key].clone(); }
    Ok(relation)
}

/// Recheck an already-open native session against the current checked catalog without preparing the whole layout.
/// 已打开的原生会话只复核当前受检目录快照，不逐次准备整个布局；未完成的正式事务仍走恢复路径。
pub(crate) fn current_relationship(root: &Path, id: &str) -> ContractResult {
    let document = read_current_catalog(root)?
        .ok_or_else(|| error("聊天不存在。", "CONVERSATION_NOT_FOUND", 404))?;
    relationship_from_catalog(&document, id)
}

/// Read a checked catalog snapshot without preparing unrelated chat paths; pending transactions retain full recovery.
/// 读取受检目录快照时不准备无关聊天路径；存在待完成事务时仍执行完整正式恢复。
fn read_current_catalog(root: &Path) -> Result<Option<Value>, ContractError> {
    if managed_files::inspect(
        root,
        &root.join(".catalog-transaction.json"),
        false,
        true,
        64 * 1024 * 1024,
    )? {
        return load_catalog(root).map(Some);
    }
    let snapshot = match read_catalog_snapshot(root) {
        Err(error) if error.code() == "CATALOG_CONFLICT" => read_catalog_snapshot(root),
        result => result,
    }?;
    Ok(snapshot.0)
}
pub(crate) fn normalize_message(input: &Value) -> ContractResult {
    if !input.is_object() {
        return Err(error("消息格式无效。", "INVALID_CONVERSATION_DATA", 400));
    }
    let mut message = input.clone();
    let created = input["CreatedAt"]
        .as_str()
        .unwrap_or("1970-01-01T00:00:00.000Z");
    let date = chrono::DateTime::parse_from_rfc3339(created)
        .map_err(|_| error("消息时间无效。", "INVALID_CONVERSATION_DATA", 400))?;
    let fields = [
        "Role",
        "CreatedAt",
        "ReasoningDurationMs",
        "Content",
        "Reasoning",
        "Status",
        "Error",
        "Provider",
        "Model",
    ];
    let types: serde_json::Map<String, Value> = fields
        .iter()
        .map(|field| {
            (
                (*field).to_string(),
                json!(match &input[*field] {
                    Value::String(_) => "string",
                    Value::Number(_) => "number",
                    Value::Null => "undefined",
                    _ => "object",
                }),
            )
        })
        .collect();
    let plan = catalog::apply(
        "storage.catalog.message",
        &json!({"value":input,"id":input["Id"],"rawTypes":types,
        "hasTranscript":input.get("ModelTranscript").is_some(),"parsedDate":date.timestamp_millis()}),
    )?;
    message["Id"] = plan["id"].clone();
    message["CreatedAt"] = json!(created);
    for field in ["Content", "Reasoning", "Error", "Provider", "Model"] {
        if message[field].is_null() {
            message[field] = json!("");
        }
    }
    if message["Status"].is_null() {
        message["Status"] = json!("completed");
    }
    message["ReasoningDurationMs"] = plan["reasoningDurationMs"].clone();
    super::message_attachments::normalize_descriptors(&mut message)?;
    validate_message_metadata(&message)?;
    if message.get("DurationMs").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_f64()
                .is_some_and(|value| value.is_finite() && value >= 0.0)
    }) {
        return Err(error("回复耗时无效。", "INVALID_CONVERSATION_DATA", 400));
    }
    Ok(message)
}
fn validate_message_metadata(message: &Value) -> Result<(), ContractError> {
    let text = |value: &Value| json!({"string":value.is_string(),"length":value.as_str().map(|text|text.encode_utf16().count()),"units":value.as_str().unwrap_or("").encode_utf16().take(256).collect::<Vec<_>>()});
    if let Some(transcript) = message
        .get("ModelTranscript")
        .filter(|value| !value.is_null())
    {
        let origin = &transcript["origin"];
        let rounds=transcript["rounds"].as_array().ok_or_else(||error("模型续接记录无效。","INVALID_MODEL_TRANSCRIPT",400))?.iter().map(|round|{
            let calls=round["calls"].as_array().map(|calls|calls.iter().map(|call|json!({"object":call.is_object(),"id":text(&call["id"]),"name":text(&call["name"]),"arguments":{"object":call["arguments"].is_object(),"jsonLength":call["arguments"].to_string().encode_utf16().count()}})).collect::<Vec<_>>());
            let reference=&round["nativeContinuationRef"];
            json!({"object":round.is_object(),"round":round["round"],"text":text(&round["text"]),"calls":calls,"contextBefore":round["contextBefore"],"hasReference":round.get("nativeContinuationRef").is_some(),"reference":{"object":reference.is_object(),"id":reference["id"],"sha256":reference["sha256"],"bytes":reference["bytes"]},"prefixFingerprint":round["prefixFingerprint"]})
        }).collect::<Vec<_>>();
        crate::models::transcript::calculate("models.transcript.validate",&json!({"object":transcript.is_object(),"version":transcript["version"],"origin":{"object":origin.is_object(),"providerId":text(&origin["providerId"]),"model":text(&origin["model"]),"protocol":origin["protocol"],"connectionFingerprint":origin["connectionFingerprint"]},"rounds":rounds}))
            .map_err(|_|error("模型续接记录无效。","INVALID_MODEL_TRANSCRIPT",400))?;
    }
    if let Some(segments) = message
        .get("AssistantSegments")
        .filter(|value| !value.is_null())
    {
        let items=segments.as_array().ok_or_else(||error("回复分段格式无效。","INVALID_CONVERSATION_DATA",400))?.iter().map(|segment|json!({"object":segment.is_object(),"id":segment["id"],"round":segment["round"],"order":segment["order"],"phase":segment["phase"],"status":segment["status"],"content":text(&segment["content"]),"reasoning":text(&segment["reasoning"]),"reasoningDurationMs":segment.get("reasoningDurationMs").unwrap_or(&json!(0))})).collect::<Vec<_>>();
        crate::orchestration::segments::calculate(
            "orchestration.segment.validate",
            &json!({"items":items}),
        )
        .map_err(|_| error("回复分段格式无效。", "INVALID_CONVERSATION_DATA", 400))?;
    }
    Ok(())
}
fn public_message(mut message: Value) -> Value {
    if let Some(fields) = message.as_object_mut() {
        fields.remove("ModelTranscript");
    }
    message
}

/// Omit private history at the raw field boundary without decoding its entire transcript twice.
/// 在原始字段边界移除私有历史，避免重复解码整份模型转录。
fn public_message_raw(raw: &str) -> Result<String, ContractError> {
    if let Ok(mut fields) = serde_json::from_str::<std::collections::BTreeMap<String, &serde_json::value::RawValue>>(raw) {
        fields.remove("ModelTranscript");
        return Ok(format!("{{{}}}", fields.iter().map(|(key, value)|
            format!("{}:{}", json!(key), value.get())).collect::<Vec<_>>().join(",")));
    }
    // Retain scalar-repair compatibility for legacy keys containing isolated UTF-16 escapes.
    // 旧键名含孤立 UTF-16 转义时保留原有标量修复兼容。
    let value = serde_json::from_str::<Value>(&raw_json::scalar_projection(raw))
        .map_err(|_| error("历史消息损坏。", "CORRUPT_CONVERSATION", 500))?;
    Ok(encode_preserving(&public_message(value), &[raw]))
}

pub(crate) fn read_messages_at(root: &Path, location: &Value) -> Result<Vec<Value>, ContractError> {
    let mut read = read_raw_snapshot(root, location)?;
    let messages = read
        .messages
        .iter()
        .map(|raw| {
            let parsed = serde_json::from_str::<Value>(&raw_json::scalar_projection(raw))
                .map_err(|_| error("消息日志无效。", "INVALID_CONVERSATION_DATA", 400))?;
            normalize_message(&parsed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    read.verify()?;
    Ok(messages)
}
pub(crate) fn read_raw_snapshot(
    root: &Path,
    location: &Value,
) -> Result<conversation_history::HistoryRead, ContractError> {
    match conversation_history::read_latest(root, location, None, false) {
        Ok(read) => Ok(read),
        Err(error) if error.code() == "CONVERSATION_HISTORY_TAIL_INCOMPLETE" => {
            read_messages_recovering(root, location)?;
            conversation_history::read_latest(root, location, None, false)
        }
        Err(error) => Err(error),
    }
}
fn read_log_identities(root: &Path, location: &Value) -> Result<Vec<Value>, ContractError> {
    let mut read = match conversation_history::read_metadata(root, location) {
        Err(error) if error.code() == "CONVERSATION_HISTORY_TAIL_INCOMPLETE" => {
            read_messages_recovering(root, location)?;
            conversation_history::read_metadata(root, location)?
        }
        result => result?,
    };
    let messages = read
        .message_ids()
        .into_iter()
        .map(|id| json!({"Id":id}))
        .collect();
    read.verify()?;
    Ok(messages)
}
fn read_messages_recovering(root: &Path, location: &Value) -> Result<Vec<Value>, ContractError> {
    let snapshot = conversation_logs::apply(
        "storage.logs.read",
        &json!({"root":root,"location":location}),
    )?;
    let lines = snapshot["lines"].as_array().unwrap();
    let mut decoded = Vec::new();
    let mut invalid = None;
    for (number, line) in lines.iter().enumerate() {
        let line = line.as_str().unwrap();
        let parsed = serde_json::from_str::<Value>(&raw_json::scalar_projection(line));
        let message = parsed
            .map_err(|_| error("消息日志无效。", "INVALID_CONVERSATION_DATA", 400))
            .and_then(|event| {
                catalog::apply(
                    "storage.catalog.event",
                    &json!({"value":event,"blank":line.trim().is_empty()}),
                )?;
                normalize_message(&event["message"])
            });
        match message {
            Ok(message) => decoded.push(message),
            Err(_) => {
                invalid = Some(number);
                break;
            }
        }
    }
    let receipt = conversation_logs::apply(
        "storage.logs.finalize",
        &json!({"root":root,"location":location,
        "expectedHash":snapshot["hash"],"exists":snapshot["exists"],"invalidLine":invalid}),
    )?;
    Ok(receipt["selected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|index| decoded[index.as_u64().unwrap() as usize].clone())
        .collect())
}
fn history_page_recovering(root: &Path, location: &Value, arguments: &Value) -> ContractResult {
    match conversation_history::apply("storage.history.page", arguments) {
        Err(error) if error.code() == "CONVERSATION_HISTORY_TAIL_INCOMPLETE" => {
            // Reuse the formal byte-preserving recovery transaction before creating a page snapshot.
            // 生成分页快照前复用正式日志的保字节恢复事务；旧游标仍按新快照正常拒绝。
            read_messages_recovering(root, location)?;
            conversation_history::apply("storage.history.page", arguments)
        }
        result => result,
    }
}
pub fn read_messages(root: &Path, id: &str, public: bool) -> Result<Vec<Value>, ContractError> {
    let catalog = load_catalog(root)?;
    let location = locate(&catalog, id)?;
    let mut read = read_raw_snapshot(root, &location)?;
    let messages = read
        .messages
        .iter()
        .map(|raw| {
            let value = serde_json::from_str::<Value>(&raw_json::scalar_projection(raw))
                .map_err(|_| error("历史消息无效。", "CORRUPT_CONVERSATION", 500))?;
            normalize_message(&value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    with_catalog(root, |current, _| {
        verify_catalog_sources(&catalog, current, &[id.to_string()])?;
        read.verify()?;
        Ok(messages
            .into_iter()
            .map(|message| {
                if public {
                    public_message(message)
                } else {
                    message
                }
            })
            .collect())
    })
}

/// Recheck used chat ownership after long reads; reject moves, archives or deletion, not unrelated edits.
/// 长读取后重核所用聊天归属；移动、归档或删除拒绝旧结果，无关修改不影响。
pub(crate) fn verify_catalog_sources(
    before: &Value,
    after: &Value,
    ids: &[String],
) -> Result<(), ContractError> {
    for id in ids {
        let old = catalog::apply(
            "storage.catalog.relationship",
            &json!({"current":before,"id":id}),
        )?;
        let new = catalog::apply(
            "storage.catalog.relationship",
            &json!({"current":after,"id":id}),
        )
        .map_err(|_| error("聊天归属已改变，请重新读取。", "WORKSPACE_CHANGED", 409))?;
        for field in [
            "conversationId",
            "projectId",
            "isArchived",
            "projectArchived",
            "isFolderlessWorkspace",
        ] {
            if old[field] != new[field] {
                return Err(error(
                    "聊天归属已改变，请重新读取。",
                    "WORKSPACE_CHANGED",
                    409,
                ));
            }
        }
    }
    Ok(())
}

fn validate_catalog(mut document: Value) -> ContractResult {
    catalog::apply("storage.catalog.document", &json!({"value":document}))?;
    for project in document["Projects"].as_array_mut().unwrap() {
        catalog::apply(
            "storage.catalog.projectValue",
            &json!({"value":project,"nameType":if project["Name"].is_string(){"string"}else{"object"}}),
        )?;
        project["Id"] = super::identity(&project["Id"])?;
        *project = super::workspace_mounts::project(project)?;
        for field in ["IsPinned", "IsArchived", "IsFolderlessWorkspace"] {
            if project[field].is_null() {
                project[field] = json!(false);
            }
        }
    }
    for location in locations(&document) {
        catalog::apply(
            "storage.catalog.chat",
            &json!({"value":location["Chat"],"titleType":if location["Chat"]["Title"].is_string(){"string"}else{"object"},"hasMessages":location["Chat"].get("Messages").is_some()}),
        )?;
    }
    for chat in document["Chats"].as_array_mut().unwrap() {
        chat["Id"] = super::identity(&chat["Id"])?;
        chat.as_object_mut().unwrap().remove("Messages");
    }
    for project in document["Projects"].as_array_mut().unwrap() {
        for chat in project["Chats"].as_array_mut().unwrap() {
            chat["Id"] = super::identity(&chat["Id"])?;
            chat.as_object_mut().unwrap().remove("Messages");
        }
    }
    Ok(document)
}
fn prepare_layout_plan(
    document: &Value,
    current: &Value,
) -> Result<(Value, Value, String), ContractError> {
    let settings = current["raw"]
        .as_str()
        .map(|raw| serde_json::from_str::<Value>(&raw_json::scalar_projection(raw)))
        .transpose()
        .map_err(|_| error("存储布局配置无效。", "INVALID_DATA_LAYOUT", 409))?
        .unwrap_or(json!({}));
    let plan = layout::apply(
        "storage.layout.plan",
        &json!({"document":document,"current":settings,"storeId":new_id(),"now":now()}),
    )?;
    let plan_hash = managed_files::digest(
        &serde_json::to_vec(&json!({"plan":plan,"chats":document["Chats"].as_array().into_iter().flatten().map(|chat|&chat["Id"]).collect::<Vec<_>>() })).unwrap(),
    );
    Ok((settings, plan, plan_hash))
}
fn ensure_layout(root: &Path, document: &Value) -> ContractResult {
    let current = layout::apply(
        "storage.layout.inspect",
        &json!({"root":root,"document":document}),
    )?;
    let (settings, plan, plan_hash) = prepare_layout_plan(document, &current)?;
    // Full catalog operations still inspect every path; this cache only avoids repeated scaffolding.
    // 完整目录操作仍检查全部路径；缓存仅避免重复创建目录框架。
    let manifest_versions = layout_manifest_versions(root, &plan)?;
    if LAYOUT_SNAPSHOTS
        .lock()
        .map_err(|_| error("布局缓存锁失效。", "STORAGE_IO_FAILED", 500))?
        .iter()
        .any(|cached| {
            cached.root == root && cached.plan == plan_hash && cached.manifests == manifest_versions
        })
    {
        return Ok(Value::Null);
    }
    if plan["settings"] != settings {
        commit_json(
            root,
            &root.join("settings.json"),
            &plan["settings"],
            current["hash"].as_str(),
            8 * 1024 * 1024,
        )?;
    }
    let manifests: Vec<Value> = plan["manifests"].as_array().unwrap().iter().map(|item|
        json!({"id":item["id"],"raw":serde_json::to_string_pretty(&item["document"]).unwrap()})).collect();
    layout::apply(
        "storage.layout.ensure",
        &json!({"root":root,"document":document,"rawSettings":serde_json::to_string_pretty(&plan["settings"]).unwrap(),
        "settingsHash":current["hash"],"manifests":manifests}),
    )?;
    let manifest_versions = layout_manifest_versions(root, &plan)?;
    let mut cache = LAYOUT_SNAPSHOTS
        .lock()
        .map_err(|_| error("布局缓存锁失效。", "STORAGE_IO_FAILED", 500))?;
    cache.retain(|cached| cached.root != root);
    cache.push(LayoutSnapshot {
        root: root.to_path_buf(),
        plan: plan_hash,
        manifests: manifest_versions,
    });
    while cache.len() > MAXIMUM_INITIALIZED_ROOTS {
        cache.remove(0);
    }
    Ok(Value::Null)
}
fn layout_manifest_versions(
    root: &Path,
    plan: &Value,
) -> Result<Vec<Option<String>>, ContractError> {
    plan["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|manifest| layout_manifest_version(root, manifest["id"].as_str().unwrap()))
        .collect()
}
fn layout_manifest_version(root: &Path, project_id: &str) -> Result<Option<String>, ContractError> {
    use std::io::Read;
    let path = root.join("Projects").join(project_id).join("project.json");
    let Some(mut reader) = managed_files::open_reader(root, &path, 8 * 1024 * 1024)? else {
        return Ok(None);
    };
    let token = reader.version_token()?;
    let version = match &token {
        Some(version) => version.clone(),
        None => {
            let mut bytes = Vec::new();
            Read::by_ref(&mut reader.file)
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(conversation_logs::io_error)?;
            if bytes.len() > 8 * 1024 * 1024 {
                return Err(error("项目清单超过容量，原文件已保留。", "INVALID_DATA_LAYOUT", 409));
            }
            managed_files::digest(&bytes)
        }
    };
    reader.verify()?;
    if reader.version_token()? != token {
        return Err(error("项目清单读取期间已改变。", "INVALID_DATA_LAYOUT", 409));
    }
    Ok(Some(version))
}
pub(crate) fn commit_catalog(
    root: &Path,
    next: &Value,
    hash: Option<&str>,
    moves: &[Value],
    writes: &[Value],
) -> ContractResult {
    let mut next = validate_catalog(next.clone())?;
    layout::apply(
        "storage.layout.inspect",
        &json!({"root":root,"document":next}),
    )?;
    let original = managed_files::read(root, &root.join("catalog.json"), 64 * 1024 * 1024)?
        .map(|bytes| {
            String::from_utf8(bytes)
                .map_err(|_| error("正式目录编码无效。", "CORRUPT_STORAGE", 409))
        })
        .transpose()?;
    if let Some(previous) = original.as_ref().and_then(|raw|serde_json::from_str::<Value>(&raw_json::scalar_projection(raw)).ok()) {
        preserve_mount_versions(&mut next, &previous)?;
    }
    let encoded = encode_preserving(&next, &original.as_deref().into_iter().collect::<Vec<_>>());
    let transaction = format!(
        "{{\"Version\":1,\"NextCatalog\":{encoded},\"Moves\":{},\"Writes\":{}}}",
        json!(moves),
        json!(writes)
    );
    let mut receipt = catalog_transactions::apply(
        "storage.conversations.commit",
        &json!({"root":root,"expectedHash":hash,
        "rawTransaction":transaction,"rawCatalog":encoded}),
    )?;
    ensure_layout(root, &next)?;
    // Return the canonical bindings that were actually committed, not the caller's pre-normalized copy.
    // 回传真实提交后的正式挂载版本，不能让调用方继续持有提交前的旧版本。
    receipt["catalog"] = next;
    Ok(receipt)
}

/// Reconcile mount versions with the catalog for both HTTP and internal writes.
/// HTTP 与内部写入均按正式目录校正挂载版本。
fn preserve_mount_versions(next: &mut Value, previous: &Value) -> Result<(), ContractError> {
    for project in next["Projects"].as_array_mut().unwrap() {
        if let Some(old) = previous["Projects"].as_array().into_iter().flatten()
            .find(|old| old["Id"] == project["Id"]) {
            *project = super::workspace_mounts::preserve_versions(project, old)?;
        }
    }
    Ok(())
}

/// Re-encode changed fields only; preserve original JSON strings and isolated UTF-16 units.
/// 仅重编码变化字段，保留原 JSON 字符串及孤立 UTF-16 单元。
pub fn encode_preserving(value: &Value, originals: &[&str]) -> String {
    use serde_json::value::RawValue;
    for raw in originals {
        if serde_json::from_str::<Value>(&raw_json::scalar_projection(raw))
            .ok()
            .as_ref()
            == Some(value)
        {
            return raw.to_string();
        }
    }
    match value {
        Value::Object(fields) => {
            let objects: Vec<HashMap<String, &RawValue>> = originals
                .iter()
                .filter_map(|raw| serde_json::from_str(raw).ok())
                .collect();
            let fields = fields
                .iter()
                .map(|(key, child)| {
                    let raw: Vec<&str> = objects
                        .iter()
                        .filter_map(|fields| fields.get(key).map(|raw| raw.get()))
                        .collect();
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        encode_preserving(child, &raw)
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            let arrays: Vec<Vec<&RawValue>> = originals
                .iter()
                .filter_map(|raw| serde_json::from_str(raw).ok())
                .collect();
            let fields = items
                .iter()
                .enumerate()
                .map(|(number, child)| {
                    let raw: Vec<&str> =
                        arrays
                            .iter()
                            .filter_map(|items| {
                                if let Some(id) = child.get("Id").or_else(|| child.get("id")) {
                                    items
                                        .iter()
                                        .find(|raw| {
                                            serde_json::from_str::<Value>(
                                                &raw_json::scalar_projection(raw.get()),
                                            )
                                            .ok()
                                            .is_some_and(|original| {
                                                let previous = original
                                                    .get("Id")
                                                    .or_else(|| original.get("id"));
                                                previous == Some(id)
                                                    || previous
                                                        .and_then(Value::as_str)
                                                        .zip(id.as_str())
                                                        .is_some_and(|(left, right)| {
                                                            // Canonical UUID casing can change; arbitrary string IDs remain case-sensitive.
                                                            // 正式 UUID 可规范化大小写；任意字符串标识仍须精确匹配，避免串用原文。
                                                            left.len() == 36
                                                                && right.len() == 36
                                                                && uuid::Uuid::parse_str(left)
                                                                    .is_ok()
                                                                && uuid::Uuid::parse_str(right)
                                                                    .is_ok()
                                                                && left.eq_ignore_ascii_case(right)
                                                        })
                                            })
                                        })
                                        .map(|raw| raw.get())
                                } else {
                                    items.get(number).map(|raw| raw.get())
                                }
                            })
                            .collect();
                    encode_preserving(child, &raw)
                })
                .collect::<Vec<_>>();
            format!("[{}]", fields.join(","))
        }
        _ => value.to_string(),
    }
}

pub fn handle_raw(
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
    raw: &str,
    root: &Path,
) -> Option<GatewayResult> {
    if method != "PUT" || path != "/api/conversations/catalog" {
        return handle(method, path, query, body, root);
    }
    Some(with_catalog(root, |document, hash| {
        let include_messages = query_value(query, "includeMessages") != Some("false");
        let mut next = document.clone();
        for field in ["Projects", "Chats"] {
            if let Some(value) = body.get(field) {
                next[field] = value.clone();
            }
        }
        let submitted = locations(&next);
        super::message_attachments::validate_catalog_submission(root, document, &submitted)?;
        let requests = catalog::apply(
            "storage.catalog.logRequests",
            &json!({"current":document,"next":next,"expectedRevision":body["Revision"]}),
        )?;
        let mut logs = Vec::new();
        for request in requests.as_array().unwrap() {
            let messages = read_log_identities(root, &request["source"])?;
            logs.push(json!({"id":request["id"],"messages":messages}));
        }
        let plan = catalog::apply(
            "storage.catalog.savePlan",
            &json!({"current":document,"next":next,"logs":logs,"expectedRevision":body["Revision"],"now":now()}),
        )?;
        let current =
            managed_files::read(root, &root.join("catalog.json"), 64 * 1024 * 1024)?.unwrap();
        let current = std::str::from_utf8(&current)
            .map_err(|_| error("目录编码无效。", "CORRUPT_STORAGE", 409))?;
        let mut validated = validate_catalog(plan["next"].clone())?;
        preserve_mount_versions(&mut validated, document)?;
        let catalog_raw = encode_preserving(&validated, &[raw, current]);
        let mut writes = Vec::new();
        for write in plan["writes"].as_array().unwrap() {
            let location = submitted
                .iter()
                .find(|item| item["Id"] == write["conversationId"])
                .unwrap();
            let index = write["messageIndex"].as_u64().unwrap() as usize;
            let message = normalize_message(&location["Chat"]["Messages"][index])?;
            let raw_chats = raw_message_sources(raw, location["Id"].as_str().unwrap(), index)?;
            writes.push(format!(
                "{{\"Location\":{},\"Message\":{}}}",
                write["Location"],
                encode_preserving(
                    &message,
                    &raw_chats.iter().map(String::as_str).collect::<Vec<_>>()
                )
            ));
        }
        let transaction = format!(
            "{{\"Version\":1,\"NextCatalog\":{},\"Moves\":{},\"Writes\":[{}]}}",
            catalog_raw,
            plan["moves"],
            writes.join(",")
        );
        catalog_transactions::apply(
            "storage.conversations.commit",
            &json!({"root":root,"expectedHash":hash,"rawTransaction":transaction,"rawCatalog":catalog_raw}),
        )?;
        ensure_layout(root, &validated)?;
        catalog_response(root, &validated, include_messages).map(|value| (200, value))
    }))
}
fn raw_message_sources(raw: &str, id: &str, index: usize) -> Result<Vec<String>, ContractError> {
    use serde_json::value::RawValue;
    let object: HashMap<String, &RawValue> = serde_json::from_str(raw)
        .map_err(|_| error("目录格式无效。", "INVALID_CONVERSATION_DATA", 400))?;
    let mut chats: Vec<&RawValue> = object
        .get("Chats")
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or_default();
    let projects: Vec<HashMap<String, &RawValue>> = object
        .get("Projects")
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or_default();
    for project in projects {
        let list: Vec<&RawValue> = project
            .get("Chats")
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .unwrap_or_default();
        chats.extend(list);
    }
    for chat in chats {
        let fields: HashMap<String, &RawValue> = serde_json::from_str(chat.get()).unwrap();
        if fields
            .get("Id")
            .and_then(|raw| serde_json::from_str::<String>(raw.get()).ok())
            .is_some_and(|value| value.eq_ignore_ascii_case(id))
        {
            let messages: Vec<&RawValue> = fields
                .get("Messages")
                .and_then(|raw| serde_json::from_str(raw.get()).ok())
                .unwrap_or_default();
            return messages
                .get(index)
                .map(|raw| vec![raw.get().to_string()])
                .ok_or_else(|| error("消息索引无效。", "INVALID_CONVERSATION_DATA", 400));
        }
    }
    Err(error(
        "消息所属聊天不存在。",
        "INVALID_CONVERSATION_DATA",
        400,
    ))
}
fn initialize(root: &Path) -> Result<(Value, Option<String>), ContractError> {
    managed_files::inspect(root, root, true, false, u64::MAX)?;
    let (journal, journal_hash) = read_json(
        root,
        &root.join(".catalog-transaction.json"),
        64 * 1024 * 1024,
    )?;
    if journal.is_some() {
        // Recover the original raw journal without encoding repaired scalar projections back into messages.
        // 恢复使用原始日志编码，不将修复后的标量投影重新写入消息。
        let raw = managed_files::read(
            root,
            &root.join(".catalog-transaction.json"),
            64 * 1024 * 1024,
        )?
        .unwrap();
        let raw = std::str::from_utf8(&raw)
            .map_err(|_| error("恢复日志编码无效，原文已保留。", "CORRUPT_STORAGE", 409))?;
        let fields: HashMap<String, &serde_json::value::RawValue> = serde_json::from_str(raw)
            .map_err(|_| error("恢复日志格式无效，原文已保留。", "CORRUPT_STORAGE", 409))?;
        let next = fields
            .get("NextCatalog")
            .ok_or_else(|| error("恢复目录缺失。", "CORRUPT_STORAGE", 409))?
            .get();
        catalog_transactions::apply(
            "storage.conversations.apply",
            &json!({"root":root,"expectedJournalHash":journal_hash,
            "rawTransaction":raw,"rawCatalog":next}),
        )?;
    }
    let (value, hash) = read_catalog_snapshot(root)?;
    let document = match value {
        Some(value) => value,
        None => migrate_legacy(root)?,
    };
    ensure_layout(root, &document)?;
    let hash = if hash.is_some() {
        hash
    } else {
        read_json(root, &root.join("catalog.json"), 64 * 1024 * 1024)?.1
    };
    Ok((document, hash))
}

/// Cache checked file identities/change stamps, not catalog authority; reread changed files.
/// 缓存文件身份及变更时间，不缓存目录权威；文件变化后重读。
pub(crate) fn read_catalog_snapshot(root: &Path) -> Result<(Option<Value>, Option<String>), ContractError> {
    use std::io::Read;
    let path = root.join("catalog.json");
    let Some(mut reader) = managed_files::open_reader(root, &path, 64 * 1024 * 1024)? else {
        return Ok((None, None));
    };
    let token = reader.version_token()?;
    if let Some(version) = &token {
        let cached = CATALOG_SNAPSHOTS
            .lock()
            .map_err(|_| error("目录缓存锁失效。", "STORAGE_IO_FAILED", 500))?
            .iter()
            .find(|cached| cached.root == root && &cached.version == version)
            .map(|cached| (cached.document.clone(), cached.hash.clone()));
        if let Some((document, hash)) = cached {
            reader.verify()?;
            if reader.version_token()? != token {
                return Err(error("正式目录读取期间已改变。", "CATALOG_CONFLICT", 409));
            }
            return Ok((Some(document), Some(hash)));
        }
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut reader.file)
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(conversation_logs::io_error)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(error(
            "正式目录超过容量，原文件已保留。",
            "CORRUPT_STORAGE",
            409,
        ));
    }
    reader.verify()?;
    if reader.version_token()? != token {
        return Err(error("正式目录读取期间已改变。", "CATALOG_CONFLICT", 409));
    }
    let raw = std::str::from_utf8(&bytes)
        .map_err(|_| error("正式目录编码无效，原文件已保留。", "CORRUPT_STORAGE", 409))?;
    let value = serde_json::from_str(&raw_json::scalar_projection(
        raw.trim_start_matches('\u{feff}'),
    ))
    .map_err(|_| error("正式目录格式无效，原文件已保留。", "CORRUPT_STORAGE", 409))?;
    let document = validate_catalog(normalize_transport_numbers(value))?;
    let hash = managed_files::digest(&bytes);
    if let Some(version) = token {
        let mut cache = CATALOG_SNAPSHOTS
            .lock()
            .map_err(|_| error("目录缓存锁失效。", "STORAGE_IO_FAILED", 500))?;
        cache.retain(|cached| cached.root != root);
        if bytes.len() <= MAXIMUM_CATALOG_CACHE_BYTES {
            cache.push(CatalogSnapshot {
                root: root.to_path_buf(),
                version,
                hash: hash.clone(),
                document: document.clone(),
                bytes: bytes.len(),
            });
            while cache.len() > MAXIMUM_INITIALIZED_ROOTS
                || cache.iter().map(|cached| cached.bytes).sum::<usize>()
                    > MAXIMUM_CATALOG_CACHE_BYTES
            {
                cache.remove(0);
            }
        }
    }
    Ok((Some(document), Some(hash)))
}
fn migrate_legacy(root: &Path) -> ContractResult {
    let legacy = std::env::var_os("KYNXA_LEGACY_DESKTOP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("Desktop"));
    let models = std::env::var_os("KYNXA_MODEL_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Conversations"))
            {
                root.parent().unwrap().to_path_buf()
            } else {
                root.join("Models")
            }
        });
    migrate_legacy_at(root, &legacy, &models)
}
fn migrate_legacy_at(root: &Path, legacy: &Path, models: &Path) -> ContractResult {
    let read = |name: &str| -> Result<String, ContractError> {
        Ok(
            managed_files::read(&legacy, &legacy.join(name), 64 * 1024 * 1024)?
                .map(|bytes| {
                    String::from_utf8(bytes).map_err(|_| {
                        error(
                            "旧版目录编码无效，迁移未开始。",
                            "INVALID_CONVERSATION_DATA",
                            409,
                        )
                    })
                })
                .transpose()?
                .unwrap_or("[]".into()),
        )
    };
    let projects_raw = read("projects.json")?;
    let chats_raw = read("chats.json")?;
    let original = format!("{{\"Projects\":{projects_raw},\"Chats\":{chats_raw}}}");
    let old: Value =
        serde_json::from_str(&raw_json::scalar_projection(&original)).map_err(|_| {
            error(
                "旧版目录格式无效，迁移未开始。",
                "INVALID_CONVERSATION_DATA",
                409,
            )
        })?;
    if !old["Projects"].is_array() || !old["Chats"].is_array() {
        return Err(error(
            "旧版目录格式无效，迁移未开始。",
            "INVALID_CONVERSATION_DATA",
            409,
        ));
    }
    let mut document = json!({"Version":1,"Revision":0,"Projects":old["Projects"],"Chats":old["Chats"],"Tombstones":[]});
    for project in document["Projects"].as_array_mut().unwrap() {
        catalog::apply(
            "storage.catalog.projectValue",
            &json!({"value":project,"nameType":if project["Name"].is_string(){"string"}else{"object"}}),
        )?;
        for field in ["IsPinned", "IsArchived", "IsFolderlessWorkspace"] {
            if project[field].is_null() {
                project[field] = json!(false);
            }
        }
    }
    let mut writes = Vec::new();
    for location in locations(&document) {
        let chat = &location["Chat"];
        catalog::apply(
            "storage.catalog.chat",
            &json!({"value":chat,"titleType":if chat["Title"].is_string(){"string"}else{"object"},"hasMessages":chat.get("Messages").is_some()}),
        )?;
        let mut ids = Vec::new();
        for (number, old_message) in chat["Messages"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let mut input = if old_message.is_string() {
                json!({"Role":"user","Content":old_message})
            } else {
                old_message.clone()
            };
            if input["Id"].is_null() {
                input["Id"] = json!(
                    format!(
                        "{:x}",
                        sha2::Sha256::digest(
                            format!("{}:{number}", chat["Id"].as_str().unwrap()).as_bytes()
                        )
                    )[..32]
                        .to_string()
                );
            }
            let message = normalize_message(&input)?;
            ids.push(message["Id"].clone());
            let originals = raw_message_sources(&original, chat["Id"].as_str().unwrap(), number)?;
            let wrapped = originals
                .iter()
                .map(|raw| {
                    if old_message.is_string() {
                        format!("{{\"Role\":\"user\",\"Content\":{raw}}}")
                    } else {
                        raw.clone()
                    }
                })
                .collect::<Vec<_>>();
            writes.push(format!(
                "{{\"Location\":{{\"Id\":{},\"ProjectId\":{}}},\"Message\":{}}}",
                location["Id"],
                location["ProjectId"],
                encode_preserving(
                    &message,
                    &wrapped.iter().map(String::as_str).collect::<Vec<_>>()
                )
            ))
        }
        catalog::apply("storage.catalog.uniqueMessages", &json!({"ids":ids}))?;
    }
    for chat in document["Chats"].as_array_mut().unwrap() {
        if chat["Draft"].is_null() {
            chat["Draft"] = json!("");
        }
        for field in ["IsSample", "IsPinned", "IsArchived"] {
            if chat[field].is_null() {
                chat[field] = json!(false);
            }
        }
    }
    document["Chats"].as_array_mut().unwrap().retain(|chat| {
        chat["IsSample"] == true
            || chat["Messages"]
                .as_array()
                .is_some_and(|messages| !messages.is_empty())
    });
    for project in document["Projects"].as_array_mut().unwrap() {
        for chat in project["Chats"].as_array_mut().unwrap() {
            if chat["Draft"].is_null() {
                chat["Draft"] = json!("");
            }
            for field in ["IsSample", "IsPinned", "IsArchived"] {
                if chat[field].is_null() {
                    chat[field] = json!(false);
                }
            }
        }
        project["Chats"].as_array_mut().unwrap().retain(|chat| {
            chat["IsSample"] == true
                || chat["Messages"]
                    .as_array()
                    .is_some_and(|messages| !messages.is_empty())
        });
    }
    let document = validate_catalog(document)?;
    let backup = root.join("Backups/conversations-v1");
    for name in ["projects.json", "chats.json"] {
        super::administration::apply(
            "storage.administration.backup",
            &json!({"source":legacy.join(name),"target":backup.join("Desktop").join(name)}),
        )?;
    }
    super::administration::apply(
        "storage.administration.backup",
        &json!({"source":models.join("sessions"),"target":backup.join("Models/sessions")}),
    )?;
    layout::apply(
        "storage.layout.inspect",
        &json!({"root":root,"document":document}),
    )?;
    let encoded = encode_preserving(&document, &[&original]);
    let transaction = format!(
        "{{\"Version\":1,\"NextCatalog\":{encoded},\"Moves\":[],\"Writes\":[{}]}}",
        writes.join(",")
    );
    catalog_transactions::apply(
        "storage.conversations.commit",
        &json!({"root":root,"expectedHash":null,"rawTransaction":transaction,"rawCatalog":encoded}),
    )?;
    ensure_layout(root, &document)?;
    Ok(document)
}
fn catalog_owner(root: &Path) -> Result<Arc<Mutex<()>>, ContractError> {
    let mut owners = OWNERS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| error("数据所有者锁失效。", "STORAGE_IO_FAILED", 500))?;
    Ok(owners
        .entry(root.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}
pub(crate) fn with_catalog<T>(
    root: &Path,
    operation: impl FnOnce(&Value, Option<&str>) -> Result<T, ContractError>,
) -> Result<T, ContractError> {
    let owner = catalog_owner(root)?;
    let _guard = owner
        .lock()
        .map_err(|_| error("数据所有者锁失效。", "STORAGE_IO_FAILED", 500))?;
    let (document, hash) = initialize(root)?;
    operation(&document, hash.as_deref())
}

/// Reuse completed layout preparation only; recheck scope and file permissions each time.
/// 仅复用未变布局的已完成准备，每次重核范围和文件权限。
fn prepared_conversation_catalog(
    root: &Path,
    conversation_id: &str,
) -> Result<(Value, Option<String>), ContractError> {
    let pending = managed_files::inspect(
        root,
        &root.join(".catalog-transaction.json"),
        false,
        true,
        64 * 1024 * 1024,
    )?;
    if !pending {
        if let (Some(document), hash) = read_catalog_snapshot(root)? {
            // Root settings/version and shared layout paths remain live checks, independent of the cache.
            // 根设置、结构版本及共享布局路径仍即时检查，不以缓存代替。
            let current = layout::apply("storage.layout.inspect", &json!({"root":root}))?;
            let (_, _, plan_hash) = prepare_layout_plan(&document, &current)?;
            let manifests = LAYOUT_SNAPSHOTS
                .lock()
                .map_err(|_| error("布局缓存锁失效。", "STORAGE_IO_FAILED", 500))?
                .iter()
                .find(|cached| cached.root == root && cached.plan == plan_hash)
                .map(|cached| cached.manifests.clone());
            if let Some(manifests) = manifests {
                let location = locate(&document, conversation_id)?;
                layout::inspect_conversation(root, &location)?;
                let project_matches = if let Some(project_id) = location["ProjectId"].as_str() {
                    let position = document["Projects"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .position(|project| project["Id"].as_str() == Some(project_id));
                    match position {
                        Some(position) => {
                            manifests.get(position) == Some(&layout_manifest_version(root, project_id)?)
                        }
                        None => false,
                    }
                } else {
                    true
                };
                if project_matches {
                    return Ok((document, hash));
                }
            }
        }
    }
    // Cold starts, pending canonical transactions and changed layout inputs retain the full recovery path.
    // 首次使用、未完成正式事务及布局输入变化仍走完整初始化与恢复流程。
    let (document, hash) = initialize(root)?;
    layout::inspect_conversation(root, &locate(&document, conversation_id)?)?;
    Ok((document, hash))
}

fn with_conversation_catalog<T>(
    root: &Path,
    conversation_id: &str,
    operation: impl FnOnce(&Value, Option<&str>) -> Result<T, ContractError>,
) -> Result<T, ContractError> {
    let owner = catalog_owner(root)?;
    let _guard = owner
        .lock()
        .map_err(|_| error("数据所有者锁失效。", "STORAGE_IO_FAILED", 500))?;
    let (document, hash) = prepared_conversation_catalog(root, conversation_id)?;
    operation(&document, hash.as_deref())
}
pub fn load_catalog(root: &Path) -> ContractResult {
    with_catalog(root, |document, _| Ok(document.clone()))
}

pub fn ensure_conversation(root: &Path, id: &str, title: &str) -> ContractResult {
    with_catalog(root, |document, hash| {
        let id = super::identity(&json!(id))?;
        catalog::apply(
            "storage.catalog.checkAlive",
            &json!({"current":document,"id":id}),
        )?;
        if locations(document).iter().any(|item| item["Id"] == id) {
            return Ok(Value::Null);
        }
        let mut next = document.clone();
        next["Chats"].as_array_mut().unwrap().push(json!({"Id":id,"Title":if title.trim().is_empty(){"新聊天"}else{title},"Draft":"","IsPinned":false,"IsArchived":false,"Messages":[]}));
        next["Revision"] = json!(document["Revision"].as_u64().unwrap() + 1);
        commit_catalog(root, &next, hash, &[], &[])?;
        Ok(Value::Null)
    })
}
pub fn read_model_messages(root: &Path, id: &str) -> Result<Vec<Value>, ContractError> {
    // Share the formal writer owner while projecting history; external file checks still apply.
    // 历史投影与正式写入共用所有者锁；外部文件身份与版本核验仍然生效。
    with_conversation_catalog(root, id, |catalog, _| {
        #[cfg(test)]
        let observer = MODEL_READ_OBSERVERS.lock().unwrap().iter()
            .find(|(path, _)| path == root).map(|(_, observer)| observer.clone());
        #[cfg(test)]
        if let Some(observer) = observer.as_ref() { observer("before"); }
        let messages = read_messages_at(root, &locate(catalog, id)?)?;
        #[cfg(test)]
        if let Some(observer) = observer.as_ref() { observer("after"); }
        let (current, _) = prepared_conversation_catalog(root, id)?;
        verify_catalog_sources(catalog, &current, &[id.to_string()])?;
        Ok(messages)
    })
}
pub fn read_conversation(root: &Path, id: &str) -> ContractResult {
    let document = load_catalog(root)?;
    let location = locate(&document, id)?;
    let mut read = read_raw_snapshot(root, &location)?;
    let messages = read
        .messages
        .iter()
        .map(|raw| {
            let value: Value = serde_json::from_str(&raw_json::scalar_projection(raw))
                .map_err(|_| error("历史消息损坏。", "CORRUPT_CONVERSATION", 500))?;
            normalize_message(&value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    with_catalog(root, |current, _| {
        verify_catalog_sources(&document, current, &[id.to_string()])?;
        read.verify()?;
        let mut relationship = catalog::apply(
            "storage.catalog.relationship",
            &json!({"current":current,"id":id}),
        )?;
        relationship["sessionDirectory"] = json!(conversation_logs::directory(root, &location)?);
        Ok(
            json!({"conversationId":relationship["conversationId"],"projectId":relationship["projectId"],
            "relationship":relationship,"revision":current["Revision"],"messages":messages}),
        )
    })
}
pub fn context_scopes(root: &Path, id: &str) -> ContractResult {
    with_catalog(root, |document, _| {
        let relationship = catalog::apply(
            "storage.catalog.relationship",
            &json!({"current":document,"id":id}),
        )?;
        let mut scopes = vec![
            json!("user"),
            json!(format!(
                "chat:{}",
                relationship["conversationId"].as_str().unwrap()
            )),
        ];
        if relationship["projectArchived"] != true && relationship["isFolderlessWorkspace"] != true
        {
            if let Some(project) = relationship["projectId"].as_str() {
                scopes.push(json!(format!("project:{project}")));
            }
        }
        Ok(json!({"relationship":relationship,"scopeKeys":scopes}))
    })
}

pub fn upsert_message(root: &Path, id: &str, input: &Value) -> ContractResult {
    upsert_message_internal(root, id, input, None)
}
pub fn upsert_message_content_raw(
    root: &Path,
    id: &str,
    input: &Value,
    original_content_raw: &str,
) -> ContractResult {
    let value: Value = serde_json::from_str(&raw_json::scalar_projection(original_content_raw))
        .map_err(|_| error("消息原文编码无效。", "INVALID_CONVERSATION_DATA", 400))?;
    if !value.is_string() || input["Content"] != value {
        return Err(error(
            "消息原文与当前请求不一致。",
            "INVALID_CONVERSATION_DATA",
            400,
        ));
    }
    upsert_message_internal(root, id, input, Some(original_content_raw))
}
fn upsert_message_internal(
    root: &Path,
    id: &str,
    input: &Value,
    original_content_raw: Option<&str>,
) -> ContractResult {
    let mut result = with_conversation_catalog(root, id, |document, _| {
        let location = locate(document, id)?;
        let message_id = super::identity(&input["Id"])?;
        let ids =
            std::collections::HashSet::from([message_id.as_str().unwrap().to_ascii_lowercase()]);
        // Upserts need one latest message, not all histories or a second decoded log snapshot.
        // 更新只需目标消息的最新原文，不读取全部历史，也不构造第二份全日志解码快照。
        let mut read = match conversation_history::read_latest(root, &location, Some(&ids), false) {
            Err(error) if error.code() == "CONVERSATION_HISTORY_TAIL_INCOMPLETE" => {
                read_messages_recovering(root, &location)?;
                conversation_history::read_latest(root, &location, Some(&ids), false)?
            }
            result => result?,
        };
        let previous_raw = read.messages.first();
        let mut merged = previous_raw
            .map(|raw| {
                let value: Value = serde_json::from_str(&raw_json::scalar_projection(raw))
                    .map_err(|_| error("消息日志无效。", "INVALID_CONVERSATION_DATA", 400))?;
                normalize_message(&value)
            })
            .transpose()?
            .unwrap_or(json!({}));
        let previous_attachments = json!({"Role":merged["Role"],"Attachments":merged["Attachments"]});
        merged
            .as_object_mut()
            .ok_or_else(|| error("消息格式无效。", "INVALID_CONVERSATION_DATA", 400))?
            .extend(
                input
                    .as_object()
                    .ok_or_else(|| error("消息格式无效。", "INVALID_CONVERSATION_DATA", 400))?
                    .clone(),
            );
        let message = normalize_message(&merged)?;
        if previous_raw.is_some() {
            super::message_attachments::validate_repeated_descriptors(&previous_attachments, &message)?;
        }
        let prior = previous_raw
            .map(String::as_str)
            .into_iter()
            .collect::<Vec<_>>();
        let raw_message = if let Some(content) = original_content_raw {
            let mut metadata = message.clone();
            metadata.as_object_mut().unwrap().remove("Content");
            let raw = encode_preserving(&metadata, &prior);
            format!("{},\"Content\":{content}}}", raw.trim_end_matches('}'))
        } else {
            encode_preserving(&message, &prior)
        };
        let unchanged = previous_raw.is_some_and(|previous| previous == &raw_message);
        read.append_message(&raw_message, unchanged)
    })?;
    if input["Role"] == "user" {
        // Preflight grants no authority; final checks reread the saved user source and catalog.
        // 预筛不授予权限，最终仍重核已保存用户原文和目录。
        let instruction = super::memory::apply(
            "storage.memory.instruction",
            &json!({"message":input["Content"],"hasProject":true}),
        );
        if !matches!(instruction, Ok(Value::Null)) {
            let receipt = super::gateway_memory::capture_explicit_instruction(
                root,
                id,
                input["Id"].as_str().unwrap_or(""),
            );
            // Memory failure cannot undo a saved message or falsely claim memory took effect.
            // 记忆失败不影响已保存消息，也不能声称记忆已生效。
            result["memoryInstruction"] = match receipt {
                Ok(receipt) => receipt,
                Err(error) => {
                    json!({"created":false,"reason":"save-failed","error":{"code":error.code(),"message":error.message()}})
                }
            };
        }
    }
    if input["Role"] == "user" || input["Role"] == "assistant" && input["Status"] == "completed" {
        let _ = super::gateway_memory::schedule_candidates(
            root,
            id,
            json!({"phase":if input["Role"]=="user"{"user"}else{"completed"},"userMessageId":if input["Role"]=="user"{input["Id"].clone()}else{Value::Null},"taskCompleted":input["TaskCompletion"]["status"]=="completed"}),
        );
    }
    Ok(result)
}
fn catalog_response(root: &Path, document: &Value, include_messages: bool) -> ContractResult {
    let mut output = json!({"Revision":document["Revision"],"Projects":document["Projects"],"Chats":document["Chats"]});
    for chat in output["Chats"].as_array_mut().unwrap() {
        chat["Messages"] = json!(if include_messages {
            read_messages_at(root, &json!({"Id":chat["Id"],"ProjectId":null}))?
                .into_iter()
                .map(public_message)
                .collect()
        } else {
            Vec::<Value>::new()
        });
    }
    for project in output["Projects"].as_array_mut().unwrap() {
        let project_id = project["Id"].clone();
        for chat in project["Chats"].as_array_mut().unwrap() {
            chat["Messages"] = json!(if include_messages {
                read_messages_at(root, &json!({"Id":chat["Id"],"ProjectId":project_id}))?
                    .into_iter()
                    .map(public_message)
                    .collect()
            } else {
                Vec::<Value>::new()
            });
        }
    }
    Ok(output)
}

fn scoped_result_directory(
    root: &Path,
    document: &Value,
    id: &str,
    allow_archived: bool,
) -> Result<(Value, PathBuf), ContractError> {
    let location = locate(document, id)?;
    let mut relationship = catalog::apply(
        "storage.catalog.relationship",
        &json!({"current":document,"id":id}),
    )?;
    relationship["sessionDirectory"] = json!(conversation_logs::directory(root, &location)?);
    super::tool_results::apply(
        "storage.toolResults.scope",
        &json!({"root":root,"context":{"conversationId":id},
        "relationship":relationship,"checkProject":false,"allowArchived":allow_archived}),
    )?;
    let directory =
        PathBuf::from(relationship["sessionDirectory"].as_str().unwrap()).join("tool-results");
    Ok((relationship, directory))
}
fn archive_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn normalize_environment(value: Option<&Value>) -> ContractResult {
    crate::tools::calculate(
        "tools.normalizeEnvironment",
        &json!({"input":value.unwrap_or(&Value::Null)}),
    )
    .map_err(|_| error("工具执行环境格式无效。", "INVALID_TOOL_RESULT", 400))
}

/// Archives use the owner lock, scope checks and create-only writes; private continuations stay private.
/// 归档沿用所属目录锁、范围校验及仅创建写入，私有续传不公开。
pub fn save_tool_result(
    root: &Path,
    conversation_id: &str,
    request_id: &str,
    call: &Value,
    canonical: &Value,
    environment: Option<&Value>,
) -> ContractResult {
    if call["arguments"]["retention"] == "permanent" && call["arguments"]["retentionReason"].as_str()
        .is_none_or(|reason| reason.trim().is_empty() || reason.chars().count() > 2000) {
        return Err(error("Permanent image retention requires a reason of 1–2000 characters. / 永久保留图像须提供1–2000字符的原因。",
            "INVALID_IMAGE_RETENTION", 400));
    }
    let request_id = super::identity(&json!(request_id))?;
    super::tool_results::validate_result_shape(call, canonical)?;
    let environment = normalize_environment(environment)?;
    let version = if environment.is_null() { 1 } else { 2 };
    let id = new_id();
    with_conversation_catalog(root, conversation_id, |document, _| {
        let (relationship, directory) =
            scoped_result_directory(root, document, conversation_id, true)?;
        // Externalize before assigning the immutable archive reference; existing hashes never change.
        // 先外置临时图像再生成不可变归档引用，不改写旧引用的受保护字节。
        let media_directory = directory.parent().unwrap().join("model-images");
        let canonical = super::model_images::externalize(root, &media_directory, conversation_id,
            request_id.as_str().unwrap(), canonical, chrono::Utc::now().timestamp_millis())?;
        if call["arguments"]["retention"] == "permanent" {
            if let Some(reason) = call["arguments"]["retentionReason"].as_str().filter(|reason|
                !reason.trim().is_empty() && reason.chars().count() <= 2000) {
                super::model_images::retain(root, &media_directory, conversation_id, &canonical,
                    Some(reason), 0, chrono::Utc::now().timestamp_millis())?;
            }
        }
        let canonical_raw = serde_json::to_string(&canonical).map_err(|_| error("工具结果无法编码。", "INVALID_TOOL_RESULT", 400))?;
        super::tool_results::apply("storage.toolResults.prepare",
            &json!({"identity":call,"canonical":canonical,"bytes":canonical_raw.len()}))?;
        let integrity = if version == 2 {
            serde_json::to_string(&json!({"canonical":canonical,"executionEnvironment":environment})).unwrap()
        } else { canonical_raw.clone() };
        let digest = archive_hash(integrity.as_bytes());
        let mut archived = json!({"version":version,"id":id,"conversationId":relationship["conversationId"],"requestId":request_id,
            "toolCallId":call["id"],"toolName":call["name"],"originalProjectId":relationship["projectId"],"savedProjectId":relationship["projectId"],
            "createdAt":now(),"bytes":canonical_raw.len(),"sha256":digest,"canonical":canonical});
        if version == 2 {
            archived["executionEnvironment"] = environment;
        }
        super::tool_results::apply(
            "storage.toolResults.save",
            &json!({"root":root,"directory":directory,"id":id,"raw":archived.to_string()}),
        )?;
        Ok(json!({"id":id,"bytes":canonical_raw.len(),"sha256":digest}))
    })
}

fn load_result_at(
    root: &Path,
    document: &Value,
    conversation_id: &str,
    id: &str,
    allow_archived: bool,
) -> ContractResult {
    let id = super::tool_results::apply("storage.toolResults.id", &json!({"value":id}))?;
    let (relationship, directory) =
        scoped_result_directory(root, document, conversation_id, allow_archived)?;
    let receipt = super::tool_results::apply(
        "storage.toolResults.read",
        &json!({"root":root,"directory":directory,"id":id}),
    )?;
    let raw = receipt["raw"].as_str().unwrap();
    let document: Value = serde_json::from_str(&raw_json::scalar_projection(raw))
        .map_err(|_| error("工具结果归档已损坏。", "CORRUPT_TOOL_RESULT", 500))?;
    // Raw fields preserve legacy hashes even when their Unicode scalar projection is lossy.
    // 原始字段保留旧归档哈希；校验不依赖可能修复孤立 UTF-16 单元的标量投影。
    #[derive(serde::Deserialize)]
    struct Original<'a> {
        #[serde(borrow)]
        canonical: &'a serde_json::value::RawValue,
        #[serde(borrow, rename = "executionEnvironment")]
        environment: Option<&'a serde_json::value::RawValue>,
    }
    let original: Original<'_> = serde_json::from_str(raw)
        .map_err(|_| error("工具结果归档已损坏。", "CORRUPT_TOOL_RESULT", 500))?;
    let environment = normalize_environment(document.get("executionEnvironment"))?;
    let integrity = if document["version"] == 2 {
        format!(
            "{{\"canonical\":{},\"executionEnvironment\":{}}}",
            original.canonical.get(),
            original
                .environment
                .map(|field| field.get())
                .unwrap_or("null")
        )
    } else {
        original.canonical.get().to_string()
    };
    super::tool_results::apply(
        "storage.toolResults.validate",
        &json!({"document":document,"id":id,"conversationId":relationship["conversationId"],
        "verifyDigest":true,"canonicalBytes":original.canonical.get().len(),"integrityHash":archive_hash(integrity.as_bytes()),
        "environmentMatches":!environment.is_null()&&environment==document["executionEnvironment"]}),
    )?;
    Ok(document)
}
pub(crate) fn public_archive(document: &Value) -> Result<(), ContractError> {
    let name = document["toolName"].as_str().unwrap_or("");
    if name.starts_with("model.") || name.starts_with("context.private") {
        return Err(error(
            "此附件仅供模型协议内部续传。",
            "PRIVATE_MODEL_ATTACHMENT",
            403,
        ));
    }
    Ok(())
}

/// Private lookup remains internal and always verifies the owning session and archive digest.
/// 私有归档读取仅供内部使用，始终核对所属会话及完整性，HTTP 接口不得直接返回此结构。
pub fn load_private_tool_result(root: &Path, conversation_id: &str, id: &str) -> ContractResult {
    with_catalog(root, |document, _| {
        load_result_at(root, document, conversation_id, id, false)
    })
}

/// Recover from unique original receipts after dispatch, not current workspace files.
/// 根据派发后唯一原回执恢复，不凭工作文件现状推断执行。
pub(crate) fn find_recovery_tool_result(
    root: &Path,
    conversation_id: &str,
    request_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    dispatched_at_ms: i64,
) -> ContractResult {
    with_catalog(root, |document, _| {
        let (_, directory) = scoped_result_directory(root, document, conversation_id, false)?;
        if !managed_files::inspect(root, &directory, false, false, 0)? { return Ok(Value::Null); }
        let mut matched = Value::Null;
        for entry in std::fs::read_dir(&directory).map_err(|_| error("恢复回执目录不可读。", "TOOL_RESULT_READ_FAILED", 503))? {
            let entry = entry.map_err(|_| error("恢复回执目录读取失败。", "TOOL_RESULT_READ_FAILED", 503))?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") { continue; }
            let Some(bytes) = managed_files::read(root, &path, 8 * 1024 * 1024 + 8192)? else { continue; };
            let archived: Value = serde_json::from_slice(&bytes)
                .map_err(|_| error("恢复回执归档损坏。", "CORRUPT_TOOL_RESULT", 500))?;
            if archived["requestId"] != request_id || archived["toolCallId"] != tool_call_id
                || archived["toolName"] != tool_name || archived["conversationId"] != conversation_id
            { continue; }
            let created = archived["createdAt"].as_str()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|date| date.timestamp_millis());
            if created.is_none_or(|created| created < dispatched_at_ms) { continue; }
            if !matched.is_null() {
                return Err(error("同一派发存在多个原始回执，须明确原件引用后核验。", "TASK_RECOVERY_RECEIPT_AMBIGUOUS", 409));
            }
            matched = load_result_at(root, document, conversation_id, archived["id"].as_str().unwrap_or(""), false)?;
        }
        Ok(matched)
    })
}
fn object_at_mut<'a>(value: &'a mut Value, path: &[Value]) -> Option<&'a mut Value> {
    let mut value = value;
    for part in path {
        let key = part.as_str()?;
        value = if value.is_array() {
            value.get_mut(key.parse::<usize>().ok()?)?
        } else {
            value.get_mut(key)?
        };
    }
    Some(value)
}
pub fn public_tool_result(
    canonical: &Value,
    reference: Option<&Value>,
    environment: Option<&Value>,
    include_media: bool,
) -> ContractResult {
    use base64::Engine;
    let plan = super::tool_results::apply(
        "storage.toolResults.projection",
        &json!({"value":canonical,"includeMedia":include_media}),
    )?;
    let mut projected = canonical.clone();
    for path in plan["removals"].as_array().unwrap() {
        let path = path.as_array().unwrap();
        if let Some(owner) =
            object_at_mut(&mut projected, &path[..path.len() - 1]).and_then(Value::as_object_mut)
        {
            owner.remove(path.last().unwrap().as_str().unwrap());
        }
    }
    for item in plan["media"].as_array().unwrap() {
        let path = item["path"].as_array().unwrap();
        let payload = canonical.pointer(item["pointer"].as_str().unwrap());
        let bytes = payload.and_then(|value| value["kynxaImageRef"]["imageBytes"].as_u64()).unwrap_or_else(||
            payload.and_then(Value::as_str).and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
                .map_or(0, |bytes| bytes.len() as u64));
        let mut media =
            json!({"encoding":"base64","bytes":bytes,"availableInLocalView":reference.is_some()});
        if let Some(reference) = reference {
            let mut media_ref = reference.clone();
            media_ref["pointer"] = item["pointer"].clone();
            media["resultRef"] = media_ref;
        }
        if let Some(block) = object_at_mut(&mut projected, path) {
            block["media"] = media;
        }
    }
    let environment = normalize_environment(environment)?;
    if !environment.is_null() {
        if projected.is_object() {
            projected["executionEnvironment"] = environment;
        } else {
            projected = json!({"output":projected,"executionEnvironment":environment});
        }
    }
    Ok(projected)
}
pub fn read_tool_result(
    root: &Path,
    conversation_id: &str,
    id: &str,
    offset: u64,
    limit: u64,
) -> ContractResult {
    with_catalog(root, |catalog, _| {
        let document = load_result_at(root, catalog, conversation_id, id, false)?;
        public_archive(&document)?;
        let reference =
            json!({"id":document["id"],"bytes":document["bytes"],"sha256":document["sha256"]});
        let projected = public_tool_result(
            &document["canonical"],
            Some(&reference),
            document.get("executionEnvironment"),
            false,
        )?;
        let text = projected.to_string();
        let page = super::tool_results::apply(
            "storage.toolResults.page",
            &json!({"sourceRaw":serde_json::to_string(&text).unwrap(),"offset":offset,"limit":limit,"projection":"public"}),
        )?;
        let units: Vec<u16> = text.encode_utf16().collect();
        let start = page["offset"].as_u64().unwrap() as usize;
        let end = page["end"].as_u64().unwrap() as usize;
        let mut value = json!({"id":document["id"],"text":String::from_utf16_lossy(&units[start..end]),"offset":start,"nextOffset":end,
            "totalCharacters":page["totalCharacters"],"hasMore":page["hasMore"],"truncated":page["truncated"],"resultRef":reference,
            "projection":"public","sourceTool":document["toolName"],"offsetUnit":"utf16-code-units","resultRefBasis":"canonical-archive"});
        if !document["executionEnvironment"].is_null() {
            value["executionEnvironment"] = document["executionEnvironment"].clone();
        }
        Ok(value)
    })
}
pub fn model_media(
    root: &Path,
    conversation_id: &str,
    reference: &Value,
    owner: &Value,
) -> ContractResult {
    super::tool_results::apply(
        "storage.toolResults.reference",
        &json!({"reference":reference,"owner":owner}),
    )?;
    let document = load_private_tool_result(
        root,
        conversation_id,
        reference["id"].as_str().unwrap_or(""),
    )?;
    public_archive(&document)?;
    super::tool_results::apply(
        "storage.toolResults.reference",
        &json!({"document":document,"reference":reference,"owner":owner}),
    )?;
    let hydrated = hydrate_model_images(root, conversation_id, &document["canonical"])?;
    let mut result = public_tool_result(
        &hydrated["value"],
        Some(reference),
        document.get("executionEnvironment"),
        true,
    )?;
    if hydrated["expiredMedia"].as_array().is_some_and(|media| !media.is_empty()) {
        result["mediaRetention"] = json!({"expiredMedia":hydrated["expiredMedia"],"availableMedia":hydrated["availableMedia"]});
    }
    Ok(result)
}

/// Only trusted stored context uses image sidecars; original user files are never managed here.
/// 图像旁车仅用于可信已保存上下文，不接管或清理用户原文件。
pub fn externalize_model_images(root: &Path, conversation_id: &str, request_id: &str, value: &Value) -> ContractResult {
    if !super::model_images::has_image_payload(value) { return Ok(value.clone()); }
    super::identity(&json!(request_id))?;
    super::tool_results::apply("storage.toolResults.depth", &json!({"value":value}))?;
    with_conversation_catalog(root, conversation_id, |catalog, _| {
        let (_, directory) = scoped_result_directory(root, catalog, conversation_id, true)?;
        super::model_images::externalize(root, &directory.parent().unwrap().join("model-images"),
            conversation_id, request_id, value, chrono::Utc::now().timestamp_millis())
    })
}

pub fn hydrate_model_images(root: &Path, conversation_id: &str, value: &Value) -> ContractResult {
    if !super::model_images::has_references(value) {
        return Ok(json!({"value":value,"expiredMedia":[],"availableMedia":0}));
    }
    super::tool_results::apply("storage.toolResults.depth", &json!({"value":value}))?;
    with_conversation_catalog(root, conversation_id, |catalog, _| {
        let (_, directory) = scoped_result_directory(root, catalog, conversation_id, true)?;
        super::model_images::hydrate(root, &directory.parent().unwrap().join("model-images"),
            conversation_id, value, chrono::Utc::now().timestamp_millis())
    })
}

pub fn pin_model_images(root: &Path, conversation_id: &str, value: &Value, reason: &str) -> ContractResult {
    if reason.trim().is_empty() || reason.chars().count() > 2000 {
        return Err(error("Permanent image retention requires a reason of 1–2000 characters. / 永久保留图像须提供1–2000字符的原因。",
            "INVALID_IMAGE_RETENTION", 400));
    }
    if !super::model_images::has_references(value) { return Ok(json!({"retained":0,"expired":0})); }
    with_conversation_catalog(root, conversation_id, |catalog, _| {
        let (_, directory) = scoped_result_directory(root, catalog, conversation_id, true)?;
        super::model_images::retain(root, &directory.parent().unwrap().join("model-images"),
            conversation_id, value, Some(reason), 0, chrono::Utc::now().timestamp_millis())
    })
}

pub fn protect_model_images(root: &Path, conversation_id: &str, value: &Value, lease_ms: u64) -> ContractResult {
    if !super::model_images::has_references(value) { return Ok(json!({"retained":0,"expired":0})); }
    with_conversation_catalog(root, conversation_id, |catalog, _| {
        let (_, directory) = scoped_result_directory(root, catalog, conversation_id, true)?;
        super::model_images::retain(root, &directory.parent().unwrap().join("model-images"),
            conversation_id, value, None, lease_ms.min(30 * 60 * 1000) as i64, chrono::Utc::now().timestamp_millis())
    })
}

/// Inspect bounded batches per session; expiry removes payloads, never receipts or hash metadata.
/// 每个会话分批检查；过期只删除载荷，不删回执或哈希元数据。
pub fn cleanup_expired_model_images(root: &Path, now_ms: i64, limit: usize) -> ContractResult {
    let empty = || json!({"scanned":0,"removed":0,"removedBytes":0});
    if limit == 0 { return Ok(empty()); }
    let owner = catalog_owner(root)?;
    let _guard = owner.lock().map_err(|_| error("数据所有者锁失效。", "STORAGE_IO_FAILED", 500))?;
    if managed_files::inspect(root, &root.join(".catalog-transaction.json"), false, true, 64 * 1024 * 1024)? {
        return Ok(empty());
    }
    let Some(catalog) = read_catalog_snapshot(root)?.0 else { return Ok(empty()); };
        let chats = catalog["Chats"].as_array().into_iter().flatten().chain(
            catalog["Projects"].as_array().into_iter().flatten().flat_map(|project|
                project["Chats"].as_array().into_iter().flatten())).collect::<Vec<_>>();
        if chats.is_empty() { return Ok(empty()); }
        let mut cursor = IMAGE_CLEANUP_CONVERSATIONS.lock().map_err(|_| error("图像清理游标不可用。", "STORAGE_IO_FAILED", 500))?;
        let start = cursor.iter().find(|(path, _)| path == root).map_or(0, |(_, index)| *index) % chats.len();
        let mut next = start;
        let mut remaining = limit.min(4096);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        let (mut scanned, mut removed, mut bytes) = (0_u64, 0_u64, 0_u64);
        for offset in 0..chats.len() {
            let index = (start + offset) % chats.len();
            next = (index + 1) % chats.len();
            let chat = chats[index];
            let Some(id) = chat["Id"].as_str() else { continue; };
            let directory = conversation_logs::directory(root, &locate(&catalog, id)?)?.join("model-images");
            let result = super::model_images::cleanup(root, &directory, id, now_ms, remaining)?;
            remaining = remaining.saturating_sub(result["visited"].as_u64().unwrap_or(0) as usize);
            scanned = scanned.saturating_add(result["scanned"].as_u64().unwrap_or(0));
            removed = removed.saturating_add(result["removed"].as_u64().unwrap_or(0));
            bytes = bytes.saturating_add(result["removedBytes"].as_u64().unwrap_or(0));
            if remaining == 0 || std::time::Instant::now() >= deadline { break; }
        }
        if let Some((_, index)) = cursor.iter_mut().find(|(path, _)| path == root) { *index = next; }
        else {
            if cursor.len() >= 128 { cursor.remove(0); }
            cursor.push((root.to_path_buf(), next));
        }
        Ok(json!({"scanned":scanned,"removed":removed,"removedBytes":bytes}))
}

/// Commit semantic checkpoints only when formal turn and receipt versions match.
/// 正式轮次及回执版本匹配时才提交语义检查点。
pub fn save_model_summary(
    root: &Path,
    conversation_id: &str,
    protocol: &str,
    available_tools: &[String],
    candidate: &Value,
) -> ContractResult {
    with_catalog(root, |document, _| {
        let location = locate(document, conversation_id)?;
        let messages = read_messages_at(root, &location)?;
        let projection = crate::models::gateway_protocol::history_projection(
            protocol,
            &messages,
            available_tools,
        )
        .map_err(|_| {
            error(
                "摘要来源投影失败，原文保留。",
                "SUMMARY_SOURCE_CHANGED",
                409,
            )
        })?;
        let sources = projection.summary_sources;
        let fields = sources
            .iter()
            .map(|source| {
                source["rounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|round| {
                        round["tools"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|tool| {
                                let mut fields = serde_json::Map::new();
                                for name in ["arguments", "resultRef", "executionEnvironment"] {
                                    if let Some(value) = tool.get(name) {
                                        fields.insert(name.into(), json!(value.to_string()));
                                    }
                                }
                                Value::Object(fields)
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let selection=crate::models::semantic::calculate("models.semantic.select",&json!({"summary":candidate,"conversationId":conversation_id,
            "sourceFacts":sources,"sourcesJson":sources.iter().map(Value::to_string).collect::<Vec<_>>(),"toolFieldsJson":fields,
            "maximumCoveredTurnCount":candidate["coveredTurnCount"],"budgetTokens":candidate["summaryBudgetTokens"],
            "createdAtValid":candidate["createdAt"].as_str().is_some_and(|date|chrono::DateTime::parse_from_rfc3339(date).is_ok())}))
            .map_err(|_|error("摘要校验失败。","INVALID_CONTEXT_SUMMARY",400))?;
        if selection.get("value").is_none() {
            return Err(error(
                "摘要来源已变化，请重新取证。",
                "SUMMARY_SOURCE_CHANGED",
                409,
            ));
        }
        let path = conversation_logs::directory(root, &location)?.join("context.json");
        let (_, digest) = read_json(root, &path, 8 * 1024 * 1024)?;
        let receipt = commit_json(root, &path, candidate, digest.as_deref(), 8 * 1024 * 1024)?;
        Ok(
            json!({"saved":true,"hash":receipt["hash"],"sourceHash":candidate["sourceHash"],"contentHash":candidate["contentHash"],"coveredTurnCount":candidate["coveredTurnCount"]}),
        )
    })
}
pub fn read_model_summary(root: &Path, conversation_id: &str) -> ContractResult {
    with_catalog(root, |catalog, _| {
        let location = locate(catalog, conversation_id)?;
        let path = conversation_logs::directory(root, &location)?.join("context.json");
        let (value, _) = read_json(root, &path, 8 * 1024 * 1024)?;
        Ok(value.unwrap_or(Value::Null))
    })
}

/// HTTP history preserves raw UTF-16 encodings while removing private transcript fields.
/// HTTP 历史保留原始 UTF-16 编码，同时移除私有模型续传字段。
pub fn handle_text(
    method: &str,
    path: &str,
    query: &[(String, String)],
    root: &Path,
) -> Option<Result<(u16, String), ContractError>> {
    if method != "GET" {
        return None;
    }
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if parts.len() == 4 && parts[..2] == ["api", "conversations"] && parts[3] == "messages" {
        return Some((|| {
            let catalog = match read_current_catalog(root)? {
                Some(catalog) => catalog,
                None => load_catalog(root)?,
            };
            let location = locate(&catalog, parts[2])?;
            let mut args = json!({"root":root,"location":location});
            for field in ["limit", "maxBytes"] {
                if let Some(value) = query_value(query, field) {
                    args[field] = json!(value.parse::<u64>().map_err(|_| error(
                        "分页数值无效。",
                        "INVALID_CONVERSATION_DATA",
                        400
                    ))?);
                }
            }
            if let Some(cursor) = query_value(query, "cursor") {
                args["cursor"] = json!(cursor);
            }
            let mut page = history_page_recovering(root, &location, &args)?;
            let messages = page["rawMessages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|raw| public_message_raw(raw.as_str().unwrap()))
                .collect::<Result<Vec<_>, ContractError>>()?;
            page.as_object_mut().unwrap().remove("rawMessages");
            page["conversationId"] = location["Id"].clone();
            let mut fields = page
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| format!("{}:{}", json!(key), value))
                .collect::<Vec<_>>();
            fields.push(format!("\"messages\":[{}]", messages.join(",")));
            let current = read_current_catalog(root)?
                .ok_or_else(|| error("聊天归属已改变，请重新读取。", "WORKSPACE_CHANGED", 409))?;
            verify_catalog_sources(&catalog, &current, &[parts[2].to_string()])?;
            conversation_history::verify_snapshot(root, &location, &page["snapshot"])?;
            Ok((200, format!("{{{}}}", fields.join(","))))
        })());
    }
    if path == "/api/conversations/catalog" {
        return Some(with_catalog(root, |catalog, _| {
            let include = query_value(query, "includeMessages") != Some("false");
            let projected = catalog_response(root, catalog, include)?;
            let original =
                managed_files::read(root, &root.join("catalog.json"), 64 * 1024 * 1024)?.unwrap();
            let original = std::str::from_utf8(&original)
                .map_err(|_| error("目录编码无效。", "CORRUPT_STORAGE", 409))?;
            if !include {
                return Ok((200, encode_preserving(&projected, &[original])));
            }
            let mut raw_chats = HashMap::new();
            for location in locations(catalog) {
                let messages = raw_messages_at(root, &location)?;
                let chat = &projected["Chats"];
                let value = chat
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|chat| chat["Id"] == location["Id"])
                    .or_else(|| {
                        projected["Projects"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .flat_map(|project| project["Chats"].as_array().into_iter().flatten())
                            .find(|chat| chat["Id"] == location["Id"])
                    })
                    .unwrap();
                let mut metadata = value.clone();
                metadata.as_object_mut().unwrap().remove("Messages");
                let original_chat =
                    raw_catalog_record(original, location["Id"].as_str().unwrap(), false)?;
                let encoded = encode_preserving(&metadata, &[&original_chat]);
                raw_chats.insert(
                    location["Id"].as_str().unwrap().to_string(),
                    format!(
                        "{},\"Messages\":[{}]}}",
                        encoded.trim_end_matches('}'),
                        messages.join(",")
                    ),
                );
            }
            let mut response = projected.clone();
            for chat in response["Chats"].as_array_mut().unwrap() {
                chat.as_object_mut().unwrap().remove("Messages");
            }
            for project in response["Projects"].as_array_mut().unwrap() {
                for chat in project["Chats"].as_array_mut().unwrap() {
                    chat.as_object_mut().unwrap().remove("Messages");
                }
            }
            let mut objects = response["Chats"]
                .as_array()
                .unwrap()
                .iter()
                .map(|chat| raw_chats[chat["Id"].as_str().unwrap()].clone())
                .collect::<Vec<_>>();
            let top_chats = format!("[{}]", objects.join(","));
            objects.clear();
            for project in response["Projects"].as_array().unwrap() {
                let mut metadata = project.clone();
                metadata.as_object_mut().unwrap().remove("Chats");
                let original_project =
                    raw_catalog_record(original, project["Id"].as_str().unwrap(), true)?;
                let encoded = encode_preserving(&metadata, &[&original_project]);
                let chats = project["Chats"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|chat| raw_chats[chat["Id"].as_str().unwrap()].as_str())
                    .collect::<Vec<_>>();
                objects.push(format!(
                    "{},\"Chats\":[{}]}}",
                    encoded.trim_end_matches('}'),
                    chats.join(",")
                ));
            }
            let fields = response
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| !["Chats", "Projects"].contains(&key.as_str()))
                .map(|(key, value)| format!("{}:{}", json!(key), value))
                .collect::<Vec<_>>();
            Ok((
                200,
                format!(
                    "{{{},\"Chats\":{},\"Projects\":[{}]}}",
                    fields.join(","),
                    top_chats,
                    objects.join(",")
                ),
            ))
        }));
    }
    None
}

#[cfg(test)]
mod gateway_tests {
    use super::*;
    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("kynxa-storage-{}", new_id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            super::super::gateway_memory::stop_candidates(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn model_history_projection_serializes_with_formal_steering_without_losing_messages() {
        let workspace = Workspace::new();
        let root = workspace.0.clone();
        let chat = new_id();
        ensure_conversation(&root, &chat, "Owned model projection").unwrap();
        upsert_message(&root, &chat, &json!({"Id":new_id(),"Role":"user","Content":"original"})).unwrap();
        let (start, started) = std::sync::mpsc::sync_channel(1);
        let (ready, waiting) = std::sync::mpsc::sync_channel(1);
        let waiting = Mutex::new(waiting);
        let writer_root = root.clone();
        let writer_chat = chat.clone();
        let owner = catalog_owner(&root).unwrap();
        let writer = std::thread::spawn(move || {
            started.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
            ready.send(matches!(owner.try_lock(), Err(std::sync::TryLockError::WouldBlock))).unwrap();
            upsert_message(&writer_root, &writer_chat,
                &json!({"Id":new_id(),"Role":"user","Content":"new steering"})).unwrap();
        });
        MODEL_READ_OBSERVERS.lock().unwrap().push((root.clone(), Arc::new(move |phase| {
            if phase != "before" { return; }
            start.send(()).unwrap();
            assert!(waiting.lock().unwrap().recv_timeout(std::time::Duration::from_secs(3)).unwrap(),
                "The formal writer must observe the model projection holding its existing owner.");
        })));
        let projected = read_model_messages(&root, &chat);
        MODEL_READ_OBSERVERS.lock().unwrap().retain(|(path, _)| path != &root);
        writer.join().unwrap();
        let projected = projected.unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0]["Content"], "original");
        let latest = read_model_messages(&root, &chat).unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0]["Content"], "original");
        assert_eq!(latest[1]["Content"], "new steering");
    }
    #[test]
    fn model_history_projection_rechecks_external_catalog_archival_before_returning() {
        let workspace = Workspace::new();
        let root = workspace.0.clone();
        let chat = new_id();
        ensure_conversation(&root, &chat, "External scope change").unwrap();
        upsert_message(&root, &chat, &json!({"Id":new_id(),"Role":"user","Content":"saved"})).unwrap();
        let changed_root = root.clone();
        MODEL_READ_OBSERVERS.lock().unwrap().push((root.clone(), Arc::new(move |phase| {
            if phase != "after" { return; }
            let path = changed_root.join("catalog.json");
            let mut catalog: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            catalog["Revision"] = json!(catalog["Revision"].as_u64().unwrap() + 1);
            catalog["Chats"][0]["IsArchived"] = json!(true);
            std::fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        })));
        let projected = read_model_messages(&root, &chat);
        MODEL_READ_OBSERVERS.lock().unwrap().retain(|(path, _)| path != &root);
        assert_eq!(projected.unwrap_err().code(), "WORKSPACE_CHANGED");
    }
    #[test]
    fn public_numbers_preserve_integer_compatibility_without_changing_raw_logs() {
        let value = normalize_transport_numbers(
            json!({"revision":2.0,"score":0.25,"negativeZero":-0.0,"oversize":9_007_199_254_740_992.0,"nested":[1.0,3.5]}),
        );
        assert_eq!(value["revision"].as_u64(), Some(2));
        assert_eq!(value["negativeZero"].as_i64(), Some(0));
        assert_eq!(value["nested"][0].as_u64(), Some(1));
        assert_eq!(value["score"], json!(0.25));
        assert_eq!(value["nested"][1], json!(3.5));
        assert!(value["oversize"].is_f64());
        let workspace = Workspace::new();
        let path = workspace.0.join("numbers.json");
        std::fs::write(&path, r#"{"revision":1.0,"text":"original\ud800"}"#).unwrap();
        let original = std::fs::read(&path).unwrap();
        let (parsed, digest) = read_json(&workspace.0, &path, 4096).unwrap();
        let mut parsed = parsed.unwrap();
        assert_eq!(parsed["revision"].as_u64(), Some(1));
        assert_eq!(digest, Some(managed_files::digest(&original)));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        parsed["revision"] = json!(2.0);
        parsed["other"] = json!("new field");
        commit_json(&workspace.0, &path, &parsed, digest.as_deref(), 4096).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("original\\ud800"));
        assert!(saved.contains("\"revision\":2"));
        assert!(!saved.contains("\"revision\":2.0"));
        assert!(commit_json(&workspace.0, &path, &parsed, digest.as_deref(), 4096).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
    }
    #[test]
    fn legacy_migration_preserves_raw_messages_and_backups() {
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        let legacy = workspace.0.join("Desktop");
        let models = workspace.0.join("Models");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(models.join("sessions")).unwrap();
        let chat = new_id().to_ascii_uppercase();
        let project = new_id().to_ascii_uppercase();
        let standalone = new_id().to_ascii_uppercase();
        let projects = format!(
            r#"[{{"Id":"{project}","Name":"项目\ud800","FolderPath":null,"IsFolderlessWorkspace":true,"Chats":[{{"Id":"{chat}","Title":"旧聊天\ud801","Messages":["原文\ud802",{{"Role":"assistant","Content":"已有回答"}}]}}]}}]"#
        );
        let chats = format!(
            r#"[{{"Id":"{standalone}","Title":"独立聊天","Messages":[{{"Role":"user","Content":"独立原文"}}]}}]"#
        );
        std::fs::write(legacy.join("projects.json"), &projects).unwrap();
        std::fs::write(legacy.join("chats.json"), &chats).unwrap();
        std::fs::write(
            models.join("sessions/old.jsonl"),
            "original legacy model log\n",
        )
        .unwrap();
        managed_files::inspect(&root, &root, true, false, u64::MAX).unwrap();
        let migrated = migrate_legacy_at(&root, &legacy, &models).unwrap();
        assert_eq!(
            migrated["Projects"][0]["Chats"].as_array().unwrap().len(),
            1
        );
        assert_eq!(read_model_messages(&root, &chat).unwrap().len(), 2);
        assert_eq!(read_model_messages(&root, &standalone).unwrap().len(), 1);
        let raw = handle_text("GET", "/api/conversations/catalog", &[], &root)
            .unwrap()
            .unwrap()
            .1;
        assert!(raw.contains("\\ud800"));
        assert!(raw.contains("\\ud801"));
        assert!(raw.contains("\\ud802"));
        assert_eq!(
            std::fs::read_to_string(root.join("Backups/conversations-v1/Desktop/projects.json"))
                .unwrap(),
            projects
        );
        assert_eq!(
            std::fs::read_to_string(
                root.join("Backups/conversations-v1/Models/sessions/old.jsonl")
            )
            .unwrap(),
            "original legacy model log\n"
        );
        std::fs::write(legacy.join("projects.json"), "broken legacy snapshot").unwrap();
        assert_eq!(
            load_catalog(&root).unwrap()["Projects"][0]["Id"],
            project.to_ascii_lowercase()
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("chats.json")).unwrap(),
            chats
        );
    }
    #[test]
    fn invalid_legacy_data_cannot_create_empty_catalog() {
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        let legacy = workspace.0.join("Desktop");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("projects.json"), "broken original").unwrap();
        assert!(migrate_legacy_at(&root, &legacy, &workspace.0.join("Models")).is_err());
        assert!(!root.join("catalog.json").exists());
        assert_eq!(
            std::fs::read_to_string(legacy.join("projects.json")).unwrap(),
            "broken original"
        );
    }
    #[test]
    fn formal_upsert_retains_utf16_content_and_rejects_bad_transcript() {
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        managed_files::inspect(&root, &root, true, false, u64::MAX).unwrap();
        migrate_legacy_at(
            &root,
            &workspace.0.join("empty"),
            &workspace.0.join("Models"),
        )
        .unwrap();
        let chat = new_id();
        ensure_conversation(&root, &chat, "原文测试").unwrap();
        let raw = r#""内容\ud800""#;
        let content: Value = serde_json::from_str(&raw_json::scalar_projection(raw)).unwrap();
        let message = json!({"Id":new_id(),"Role":"user","Content":content});
        upsert_message_content_raw(&root, &chat, &message, raw).unwrap();
        upsert_message(
            &root,
            &chat,
            &json!({"Id":message["Id"],"Role":"user","Status":"completed"}),
        )
        .unwrap();
        let page = handle_text(
            "GET",
            &format!("/api/conversations/{chat}/messages"),
            &[],
            &root,
        )
        .unwrap()
        .unwrap()
        .1;
        assert!(page.contains("\\ud800"));
        let original = r#"{"Content":"正文\ud800","Reasoning":"公开推理\ud801","AssistantSegments":[{"content":"分段\ud802"}],"ToolActivities":[{"arguments":{"text":"参数\ud803"},"result":"完整回执\ud804"}],"ModelTranscript":{"rounds":[{"text":"PRIVATE_HISTORY_MUST_NOT_LEAK"}]}}"#;
        let projected = public_message_raw(original).unwrap();
        assert!(!projected.contains("ModelTranscript") && !projected.contains("PRIVATE_HISTORY_MUST_NOT_LEAK"));
        for preserved in ["正文\\ud800", "公开推理\\ud801", "分段\\ud802", "参数\\ud803", "完整回执\\ud804"] {
            assert!(projected.contains(preserved), "public history changed raw UTF-16 content: {preserved}");
        }
        assert_eq!(serde_json::from_str::<Value>(&raw_json::scalar_projection(&projected)).unwrap(),
            public_message(serde_json::from_str::<Value>(&raw_json::scalar_projection(original)).unwrap()));
        assert!(upsert_message(&root,&chat,&json!({"Id":new_id(),"Role":"assistant","Content":"bad","ModelTranscript":{"version":1}})).is_err());
        assert_eq!(read_model_messages(&root, &chat).unwrap().len(), 1);
    }
    #[test]
    fn warm_catalog_observes_external_changes_and_keeps_layout_validation() {
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        let chat = new_id();
        ensure_conversation(&root, &chat, "before").unwrap();
        let mut catalog = load_catalog(&root).unwrap();
        catalog["Chats"][0]["Title"] = json!("after");
        catalog["Revision"] = json!(catalog["Revision"].as_u64().unwrap() + 1);
        std::fs::write(
            root.join("catalog.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        assert_eq!(load_catalog(&root).unwrap()["Chats"][0]["Title"], "after");
        let settings_path = root.join("settings.json");
        let original_settings = std::fs::read(&settings_path).unwrap();
        let mut settings: Value = serde_json::from_slice(&original_settings).unwrap();
        settings["Storage"]["LayoutVersion"] = json!(2);
        std::fs::write(&settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(
            load_catalog(&root).unwrap_err().code(),
            "INVALID_DATA_LAYOUT"
        );
        std::fs::write(&settings_path, &original_settings).unwrap();
        load_catalog(&root).unwrap();
        let attachments = root.join("Chats").join(&chat).join("attachments");
        std::fs::remove_dir(&attachments).unwrap();
        std::fs::write(&attachments, b"wrong directory object").unwrap();
        assert_eq!(
            load_catalog(&root).unwrap_err().code(),
            "INVALID_DATA_LAYOUT"
        );
        std::fs::remove_file(&attachments).unwrap();
        std::fs::create_dir(&attachments).unwrap();
        load_catalog(&root).unwrap();
        let catalog_path = root.join("catalog.json");
        std::fs::write(&catalog_path, b"corrupt external catalog").unwrap();
        assert_eq!(load_catalog(&root).unwrap_err().code(), "CORRUPT_STORAGE");
        assert_eq!(
            std::fs::read(&catalog_path).unwrap(),
            b"corrupt external catalog"
        );
    }
    #[test]
    fn scoped_writes_skip_unrelated_layout_and_keep_target_path_and_version_checks() {
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        let target = new_id();
        let unrelated = new_id();
        ensure_conversation(&root, &target, "active task").unwrap();
        ensure_conversation(&root, &unrelated, "unrelated task").unwrap();
        let mut message = json!({"Id":new_id(),"Role":"assistant","Status":"streaming","Content":"before"});
        upsert_message(&root, &target, &message).unwrap();
        let unrelated_attachments = root.join("Chats").join(&unrelated).join("attachments");
        std::fs::remove_dir(&unrelated_attachments).unwrap();
        std::fs::write(&unrelated_attachments, b"unrelated invalid directory").unwrap();

        // Only the current scope is touched, while explicit full catalog reads retain their old validation.
        // 仅访问当前范围；显式读取完整目录仍保留原来的全布局校验。
        message["Content"] = json!("saved while another chat needs repair");
        assert_eq!(upsert_message(&root, &target, &message).unwrap()["appended"], true);
        let request = new_id();
        let call = json!({"id":"read-1","name":"filesystem.read"});
        let canonical = json!({"success":true,"text":"original result"});
        let receipt = save_tool_result(&root, &target, &request, &call, &canonical, None).unwrap();
        let catalog = read_current_catalog(&root).unwrap().unwrap();
        let archived = load_result_at(&root, &catalog, &target, receipt["id"].as_str().unwrap(), false).unwrap();
        assert_eq!(archived["canonical"], canonical);
        assert_eq!(archived["requestId"], request);
        assert_eq!(load_catalog(&root).unwrap_err().code(), "INVALID_DATA_LAYOUT");

        let target_directory = root.join("Chats").join(&target);
        let attachments = target_directory.join("attachments");
        std::fs::remove_dir(&attachments).unwrap();
        std::fs::write(&attachments, b"invalid current directory").unwrap();
        assert_eq!(upsert_message(&root, &target, &message).unwrap_err().code(), "INVALID_DATA_LAYOUT");
        assert_eq!(save_tool_result(&root, &target, &request, &call, &canonical, None).unwrap_err().code(), "INVALID_DATA_LAYOUT");
        std::fs::remove_file(&attachments).unwrap();
        std::fs::create_dir(&attachments).unwrap();

        // An externally substituted hard-linked source must still fail the actual opened-file checks.
        // 外部替换为硬链接的原件仍须被实际打开文件检查拒绝。
        let events = target_directory.join("events.jsonl");
        let held_events = workspace.0.join("held-events.jsonl");
        let original = std::fs::read(&events).unwrap();
        std::fs::rename(&events, &held_events).unwrap();
        std::fs::hard_link(&held_events, &events).unwrap();
        assert_eq!(upsert_message(&root, &target, &message).unwrap_err().code(), "INVALID_DATA_LAYOUT");
        assert_eq!(save_tool_result(&root, &target, &request, &call, &canonical, None).unwrap_err().code(), "INVALID_DATA_LAYOUT");
        assert_eq!(std::fs::read(&held_events).unwrap(), original);
        std::fs::remove_file(&events).unwrap();
        std::fs::rename(&held_events, &events).unwrap();

        let settings_path = root.join("settings.json");
        let original_settings = std::fs::read(&settings_path).unwrap();
        let mut settings: Value = serde_json::from_slice(&original_settings).unwrap();
        settings["Storage"]["LayoutVersion"] = json!(2);
        std::fs::write(&settings_path, settings.to_string()).unwrap();
        assert_eq!(upsert_message(&root, &target, &message).unwrap_err().code(), "INVALID_DATA_LAYOUT");
        assert_eq!(save_tool_result(&root, &target, &request, &call, &canonical, None).unwrap_err().code(), "INVALID_DATA_LAYOUT");
        std::fs::write(&settings_path, original_settings).unwrap();
        std::fs::remove_file(&unrelated_attachments).unwrap();
        std::fs::create_dir(&unrelated_attachments).unwrap();
        load_catalog(&root).unwrap();
    }

    #[test]
    fn scoped_writes_initialize_cold_layout_and_reconcile_changed_project_inputs() {
        let workspace = Workspace::new();
        let seed = workspace.0.join("SeedData");
        let root = workspace.0.join("ColdData");
        let target = new_id();
        ensure_conversation(&seed, &target, "cold task").unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::copy(seed.join("catalog.json"), root.join("catalog.json")).unwrap();
        let message = json!({"Id":new_id(),"Role":"assistant","Status":"streaming","Content":"cold append"});
        upsert_message(&root, &target, &message).unwrap();
        assert!(root.join("settings.json").is_file());
        assert!(root.join("Knowledge").is_dir());
        assert!(root.join("Chats").join(&target).join("attachments").is_dir());

        // New catalog layout inputs must prepare their new paths before the hot path can be reused.
        // 正式目录出现新的布局输入时，先准备新增路径，再允许复用热路径。
        let project = new_id();
        let project_chat = new_id();
        let mut catalog = load_catalog(&root).unwrap();
        catalog["Projects"].as_array_mut().unwrap().push(json!({
            "Id":project,"Name":"new project","FolderPath":null,"IsFolderlessWorkspace":true,
            "Chats":[{"Id":project_chat,"Title":"project task"}]
        }));
        catalog["Revision"] = json!(catalog["Revision"].as_u64().unwrap() + 1);
        std::fs::write(root.join("catalog.json"), catalog.to_string()).unwrap();
        upsert_message(&root, &target, &message).unwrap();
        let project_directory = root.join("Projects").join(&project);
        let manifest = project_directory.join("project.json");
        assert!(manifest.is_file());
        assert!(project_directory.join("Sessions").join(&project_chat).join("attachments").is_dir());
        let project_message = json!({"Id":new_id(),"Role":"assistant","Status":"streaming","Content":"project append"});
        upsert_message(&root, &project_chat, &project_message).unwrap();
        std::fs::write(&manifest, b"changed derived projection").unwrap();
        upsert_message(&root, &project_chat, &project_message).unwrap();
        let repaired: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(repaired["Id"], project);
        assert_eq!(repaired["Sessions"][0], project_chat);
        let memory = project_directory.join("Memory");
        std::fs::remove_dir(&memory).unwrap();
        std::fs::write(&memory, b"invalid current project directory").unwrap();
        assert_eq!(upsert_message(&root, &project_chat, &project_message).unwrap_err().code(), "INVALID_DATA_LAYOUT");
    }

    #[test]
    fn scoped_writes_recover_pending_transactions_before_archiving_and_appending() {
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        let target = new_id();
        ensure_conversation(&root, &target, "pending task").unwrap();
        let mut next = load_catalog(&root).unwrap();
        next["Revision"] = json!(next["Revision"].as_u64().unwrap() + 1);
        next["Chats"][0]["Title"] = json!("recovered task");
        let recovered = json!({"Id":new_id(),"Role":"assistant","Status":"streaming","CreatedAt":now(),"Content":"durable pending message"});
        let journal = root.join(".catalog-transaction.json");
        std::fs::write(&journal, json!({
            "Version":1,"NextCatalog":next,"Moves":[],
            "Writes":[{"Location":locate(&next,&target).unwrap(),"Message":recovered}]
        }).to_string()).unwrap();
        let request = new_id();
        let canonical = json!({"success":true,"text":"after recovery"});
        let receipt = save_tool_result(&root, &target, &request,
            &json!({"id":"read-recovered","name":"filesystem.read"}), &canonical, None).unwrap();
        assert!(!journal.exists());
        let archived = load_private_tool_result(&root, &target, receipt["id"].as_str().unwrap()).unwrap();
        assert_eq!(archived["canonical"], canonical);
        assert_eq!(archived["requestId"], request);
        upsert_message(&root, &target, &json!({"Id":new_id(),"Role":"assistant","Status":"streaming","Content":"continued"})).unwrap();
        let messages = read_model_messages(&root, &target).unwrap();
        assert_eq!(messages.iter().filter(|message| message["Id"] == recovered["Id"]).count(), 1);
        assert_eq!(load_catalog(&root).unwrap()["Chats"][0]["Title"], "recovered task");
    }

    #[test]
    fn history_pages_skip_unrelated_layout_and_recheck_target_scope_and_path() {
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        let target = new_id();
        let unrelated = new_id();
        ensure_conversation(&root, &target, "target history").unwrap();
        ensure_conversation(&root, &unrelated, "unrelated history").unwrap();
        let message = json!({"Id":new_id(),"Role":"user","Content":"preserved target"});
        upsert_message(&root, &target, &message).unwrap();
        let catalog = load_catalog(&root).unwrap();
        let target_location = locate(&catalog, &target).unwrap();
        let target_log = conversation_logs::directory(&root, &target_location)
            .unwrap()
            .join("events.jsonl");
        let original_log = std::fs::read(&target_log).unwrap();
        let unrelated_attachments = root.join("Chats").join(&unrelated).join("attachments");
        std::fs::remove_dir(&unrelated_attachments).unwrap();
        std::fs::write(&unrelated_attachments, b"invalid unrelated directory").unwrap();
        let path = format!("/api/conversations/{target}/messages");
        let query = [("limit".into(), "4".into())];

        // A history page validates its own source; full catalog operations still validate the complete layout.
        // 历史页核验自身来源；完整目录操作仍检查全部布局，无关聊天异常不能阻塞目标聊天显示。
        let page = handle_text("GET", &path, &query, &root).unwrap().unwrap();
        let page: Value = serde_json::from_str(&page.1).unwrap();
        assert_eq!(page["messages"][0]["Id"], message["Id"]);
        assert_eq!(
            load_catalog(&root).unwrap_err().code(),
            "INVALID_DATA_LAYOUT"
        );

        // External catalog replacement must invalidate a target removed or moved after the first scope snapshot.
        // 外部替换目录后，目标删除或移动必须使先前范围快照失效，不能沿用缓存归属。
        let mut removed = catalog.clone();
        removed["Revision"] = json!(catalog["Revision"].as_u64().unwrap() + 1);
        removed["Chats"]
            .as_array_mut()
            .unwrap()
            .retain(|chat| chat["Id"] != target);
        std::fs::write(root.join("catalog.json"), removed.to_string()).unwrap();
        let current = read_current_catalog(&root).unwrap().unwrap();
        assert_eq!(
            verify_catalog_sources(&catalog, &current, &[target.clone()])
                .unwrap_err()
                .code(),
            "WORKSPACE_CHANGED"
        );
        assert_eq!(
            handle_text("GET", &path, &query, &root)
                .unwrap()
                .unwrap_err()
                .code(),
            "CONVERSATION_NOT_FOUND"
        );
        let mut moved = removed;
        moved["Projects"] = json!([{
            "Id":new_id(),"Name":"changed scope","FolderPath":null,"IsFolderlessWorkspace":true,
            "Chats":[target_location["Chat"]]
        }]);
        std::fs::write(root.join("catalog.json"), moved.to_string()).unwrap();
        let current = read_current_catalog(&root).unwrap().unwrap();
        assert_eq!(
            verify_catalog_sources(&catalog, &current, &[target.clone()])
                .unwrap_err()
                .code(),
            "WORKSPACE_CHANGED"
        );
        std::fs::write(root.join("catalog.json"), catalog.to_string()).unwrap();

        std::fs::remove_file(&target_log).unwrap();
        std::fs::create_dir(&target_log).unwrap();
        assert_eq!(
            handle_text("GET", &path, &query, &root)
                .unwrap()
                .unwrap_err()
                .code(),
            "INVALID_MEMORY_PATH"
        );
        std::fs::remove_dir(&target_log).unwrap();
        std::fs::write(&target_log, &original_log).unwrap();
        assert!(handle_text("GET", &path, &query, &root).unwrap().is_ok());
    }
    #[test]
    fn cold_catalog_submission_does_not_read_unrelated_history() {
        for raw_transport in [false, true] {
            let workspace = Workspace::new();
            let root = workspace.0.join("Data");
            let old_chat = new_id();
            let active_chat = new_id();
            ensure_conversation(&root, &old_chat, "old history").unwrap();
            ensure_conversation(&root, &active_chat, "current task").unwrap();
            let mut body = load_catalog(&root).unwrap();
            let old_location = locate(&body, &old_chat).unwrap();
            let old_log = conversation_logs::directory(&root, &old_location)
                .unwrap()
                .join("events.jsonl");
            // An unrelated unreadable history must not be parsed while saving a different chat.
            // 保存其他聊天时不解析无关的不可读历史；显式读取该历史仍必须报告损坏。
            let original = b"{\"version\":1,\"type\":\"message.upsert\",\"message\":broken}\n";
            std::fs::write(&old_log, original).unwrap();
            let user = json!({"Id":new_id(),"Role":"user","Status":"completed","CreatedAt":now(),"Content":"hello"});
            for chat in body["Chats"].as_array_mut().unwrap() {
                chat["Messages"] = if chat["Id"] == active_chat {
                    json!([user])
                } else {
                    json!([])
                };
            }
            let query = [("includeMessages".into(), "false".into())];
            let result = if raw_transport {
                handle_raw(
                    "PUT",
                    "/api/conversations/catalog",
                    &query,
                    &body,
                    &body.to_string(),
                    &root,
                )
            } else {
                handle("PUT", "/api/conversations/catalog", &query, &body, &root)
            }
            .unwrap()
            .unwrap();
            assert_eq!(result.0, 200);
            assert_eq!(result.1["Chats"].as_array().unwrap().len(), 2);
            assert_eq!(std::fs::read(&old_log).unwrap(), original);
            let current = read_model_messages(&root, &active_chat).unwrap();
            assert_eq!(current.len(), 1);
            assert_eq!(current[0]["Id"], user["Id"]);
            assert!(read_model_messages(&root, &old_chat).is_err());
        }
    }
    #[test]
    fn metadata_catalog_updates_reuse_identities_without_reencoding_large_history() {
        use std::io::Write;
        let workspace = Workspace::new();
        let root = workspace.0.join("Data");
        let chat = new_id();
        ensure_conversation(&root, &chat, "before").unwrap();
        let catalog = load_catalog(&root).unwrap();
        let location = locate(&catalog, &chat).unwrap();
        let path = conversation_logs::directory(&root, &location)
            .unwrap()
            .join("events.jsonl");
        let assistant = new_id();
        let user = new_id();
        let mut writer = std::fs::File::create(&path).unwrap();
        let large = "x".repeat(2 * 1024 * 1024);
        for _ in 0..4 {
            writeln!(writer,"{}",json!({"version":1,"type":"message.upsert","message":{"Id":assistant,"Role":"assistant","Status":"completed","Content":large}})).unwrap();
        }
        writeln!(writer,"{{\"version\":1,\"type\":\"message.upsert\",\"message\":{{\"Id\":\"{user}\",\"Role\":\"user\",\"Content\":\"raw\\ud800\"}}}}").unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        let original = std::fs::read(&path).unwrap();
        let metadata = conversation_history::read_metadata(&root, &location).unwrap();
        assert!(metadata.messages.is_empty());
        assert_eq!(metadata.message_ids(), vec![assistant, user]);
        drop(metadata);
        let mut body = catalog.clone();
        body["Chats"][0]["Title"] = json!("metadata changed");
        let raw = serde_json::to_string(&body).unwrap();
        let result = handle_raw(
            "PUT",
            "/api/conversations/catalog",
            &[("includeMessages".into(), "false".into())],
            &body,
            &raw,
            &root,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.0, 200);
        assert_eq!(result.1["Chats"][0]["Title"], "metadata changed");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            writer,
            "{{\"version\":1,\"type\":\"message.upsert\",\"message\":broken}}"
        )
        .unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        let body = load_catalog(&root).unwrap();
        assert!(
            handle(
                "PUT",
                "/api/conversations/catalog",
                &[("includeMessages".into(), "false".into())],
                &body,
                &root
            )
            .unwrap()
            .is_ok()
        );
        assert!(read_model_messages(&root, &chat).is_err());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with("broken}\n")
        );
    }
}
fn raw_catalog_record(original: &str, id: &str, project: bool) -> Result<String, ContractError> {
    use serde_json::value::RawValue;
    let fields: HashMap<String, Box<RawValue>> = serde_json::from_str(original)
        .map_err(|_| error("目录原始字段无效。", "CORRUPT_STORAGE", 409))?;
    let projects = serde_json::from_str::<Vec<Box<RawValue>>>(fields["Projects"].get())
        .map_err(|_| error("工作原始字段无效。", "CORRUPT_STORAGE", 409))?;
    let mut candidates = if project {
        Vec::new()
    } else {
        serde_json::from_str::<Vec<Box<RawValue>>>(fields["Chats"].get())
            .map_err(|_| error("聊天原始字段无效。", "CORRUPT_STORAGE", 409))?
    };
    for record in projects {
        if project {
            candidates.push(record);
        } else {
            let fields: HashMap<String, Box<RawValue>> = serde_json::from_str(record.get())
                .map_err(|_| error("工作原始字段无效。", "CORRUPT_STORAGE", 409))?;
            candidates.extend(
                serde_json::from_str::<Vec<Box<RawValue>>>(fields["Chats"].get())
                    .map_err(|_| error("工作聊天原始字段无效。", "CORRUPT_STORAGE", 409))?,
            );
        }
    }
    candidates
        .iter()
        .find(|candidate| {
            serde_json::from_str::<Value>(&raw_json::scalar_projection(candidate.get()))
                .ok()
                .is_some_and(|value| {
                    super::identity(&value["Id"]).ok() == super::identity(&json!(id)).ok()
                })
        })
        .map(|candidate| candidate.get().to_string())
        .ok_or_else(|| error("原始目录归属已改变。", "WORKSPACE_CHANGED", 409))
}
fn raw_messages_at(root: &Path, location: &Value) -> Result<Vec<String>, ContractError> {
    let mut read = read_raw_snapshot(root, location)?;
    let mut messages = Vec::with_capacity(read.messages.len());
    for raw in &read.messages {
        let value = serde_json::from_str::<Value>(&raw_json::scalar_projection(raw))
            .map_err(|_| error("历史消息损坏。", "CORRUPT_CONVERSATION", 500))?;
        let value = public_message(normalize_message(&value)?);
        messages.push(encode_preserving(&value, &[raw]));
    }
    read.verify()?;
    Ok(messages)
}
/// Dispatch formal data operations; storage helpers own locks, versions, and commits.
/// 分派正式数据操作；锁、版本核验与提交边界由对应存储辅助函数负责。
pub fn handle(
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
    root: &Path,
) -> Option<GatewayResult> {
    if let Some(result) = super::gateway_memory::handle(method, path, body, root) {
        return Some(result);
    }
    if path == "/api/conversations/catalog" {
        return Some(with_catalog(root, |document, hash| {
            let include_messages = query_value(query, "includeMessages") != Some("false");
            if method == "GET" {
                return catalog_response(root, document, include_messages)
                    .map(|value| (200, value));
            }
            if method != "PUT" {
                return Err(error("此方法不支持。", "METHOD_NOT_ALLOWED", 405));
            }
            let mut next = document.clone();
            for field in ["Projects", "Chats"] {
                if let Some(value) = body.get(field) {
                    next[field] = value.clone();
                }
            }
            let submitted = locations(&next);
            super::message_attachments::validate_catalog_submission(root, document, &submitted)?;
            let requests = catalog::apply(
                "storage.catalog.logRequests",
                &json!({"current":document,"next":next,"expectedRevision":body["Revision"]}),
            )?;
            let mut logs = Vec::new();
            for request in requests.as_array().unwrap() {
                let messages = read_log_identities(root, &request["source"])?;
                logs.push(json!({"id":request["id"],"messages":messages}));
            }
            let plan = catalog::apply(
                "storage.catalog.savePlan",
                &json!({"current":document,"next":next,"logs":logs,"expectedRevision":body["Revision"],"now":now()}),
            )?;
            let mut writes = Vec::new();
            for write in plan["writes"].as_array().unwrap() {
                let source = submitted
                    .iter()
                    .find(|item| item["Id"] == write["conversationId"])
                    .unwrap();
                let message = normalize_message(
                    &source["Chat"]["Messages"][write["messageIndex"].as_u64().unwrap() as usize],
                )?;
                writes.push(json!({"Location":write["Location"],"Message":message}));
            }
            let committed = commit_catalog(
                root,
                &plan["next"],
                hash,
                plan["moves"].as_array().unwrap(),
                &writes,
            )?;
            catalog_response(root, &committed["catalog"], include_messages).map(|value| (200, value))
        }));
    }
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.len() >= 4 && parts[..2] == ["api", "conversations"] {
        let id = parts[2];
        if parts.len() == 5 && parts[3] == "tool-results" && method == "GET" {
            if query_value(query, "offset").is_some() || query_value(query, "limit").is_some() {
                // The existing page reader owns catalog locking; never enter it while holding the same owner lock.
                // 既有分页读取器拥有目录锁；不得在已经持有同一所有者锁时再次进入。
                return Some((|| {
                    let read_unsigned_page_argument =
                        |field: &str, default: u64| -> Result<u64, ContractError> {
                            let Some(value) = query_value(query, field) else {
                                return Ok(default);
                            };
                            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
                            {
                                return Err(error(
                                    "工具结果分页数值无效。",
                                    "INVALID_TOOL_RESULT",
                                    400,
                                ));
                            }
                            value.parse::<u64>().map_err(|_| {
                                error("工具结果分页数值无效。", "INVALID_TOOL_RESULT", 400)
                            })
                        };
                    let offset = read_unsigned_page_argument("offset", 0)?;
                    let limit = read_unsigned_page_argument("limit", 16000)?;
                    if !(1..=16000).contains(&limit) {
                        return Err(error(
                            "工具结果分页额度须在 1–16000 范围内。",
                            "INVALID_TOOL_RESULT",
                            400,
                        ));
                    }
                    read_tool_result(root, id, parts[4], offset, limit).map(|value| (200, value))
                })());
            }
            return Some(with_catalog(root, |document, _| {
                let archive = load_result_at(root, document, id, parts[4], true)?;
                public_archive(&archive)?;
                let canonical = if super::model_images::has_references(&archive["canonical"]) {
                    let (_, directory) = scoped_result_directory(root, document, id, true)?;
                    super::model_images::hydrate(root, &directory.parent().unwrap().join("model-images"), id,
                        &archive["canonical"], chrono::Utc::now().timestamp_millis())?["value"].take()
                } else { archive["canonical"].clone() };
                public_tool_result(
                    &canonical,
                    None,
                    archive.get("executionEnvironment"),
                    true,
                )
                .map(|value| (200, json!({"result":value})))
            }));
        }
        if parts.len() == 4 && parts[3] == "relationships" && method == "GET" {
            return Some(with_catalog(root, |document, _| {
                catalog::apply(
                    "storage.catalog.related",
                    &json!({"current":document,"id":id}),
                )
                .map(|value| (200, value))
            }));
        }
        if parts.len() == 4 && parts[3] == "messages" && method == "GET" {
            return Some((|| {
                let document = load_catalog(root)?;
                let location = locate(&document, id)?;
                let mut args = json!({"root":root,"location":location});
                for field in ["limit", "maxBytes"] {
                    if let Some(value) = query_value(query, field) {
                        args[field] = json!(value.parse::<u64>().map_err(|_| error(
                            "分页数值无效。",
                            "INVALID_CONVERSATION_DATA",
                            400
                        ))?);
                    }
                }
                if let Some(cursor) = query_value(query, "cursor") {
                    args["cursor"] = json!(cursor);
                }
                let mut page = history_page_recovering(root, &location, &args)?;
                let messages = page["rawMessages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|raw| {
                        serde_json::from_str::<Value>(&raw_json::scalar_projection(
                            raw.as_str().unwrap(),
                        ))
                        .map(public_message)
                        .map_err(|_| error("历史消息无效。", "CORRUPT_CONVERSATION", 500))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                page.as_object_mut().unwrap().remove("rawMessages");
                page["messages"] = json!(messages);
                page["conversationId"] = location["Id"].clone();
                with_catalog(root, |current, _| {
                    verify_catalog_sources(&document, current, &[id.to_string()])?;
                    conversation_history::verify_snapshot(root, &location, &page["snapshot"])
                })?;
                Ok((200, page))
            })());
        }
        if parts.len() == 5 && parts[3] == "runs" && method == "GET" {
            return Some(read_messages(root,id,true).and_then(|messages| {
                let message=messages.into_iter().rev().find(|item|item["Role"]=="assistant"&&["RequestId","Id"].iter().any(|field|item[*field].as_str().is_some_and(|value|value.eq_ignore_ascii_case(parts[4]))))
                    .filter(|item|!item["ToolRun"].is_null()).ok_or_else(||error("此请求没有连续执行记录。","TOOL_RUN_NOT_FOUND",404))?;
                Ok((200,json!({"conversationId":id,"requestId":message.get("RequestId").unwrap_or(&message["Id"]),"status":message["Status"],"run":message["ToolRun"]})))
            }));
        }
    }
    None
}
