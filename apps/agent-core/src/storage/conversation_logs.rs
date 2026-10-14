use super::{ContractError, ContractResult, cancellation, identity, managed_files, raw_json};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

pub(crate) static CONVERSATION_TRANSACTIONS: Mutex<()> = Mutex::new(());
static RECOVERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn error(message: &str, code: &'static str, status: u16) -> ContractError {
    ContractError::new(message, code, status)
}

pub(crate) fn io_error(error: std::io::Error) -> ContractError {
    ContractError::new(
        format!("Conversation storage failed. / 会话存储失败：{error}"),
        "CONVERSATION_STORAGE_FAILED",
        500,
    )
}

pub(crate) fn check_cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        return Err(error(
            "Conversation operation cancelled. / 会话操作已取消。",
            "ABORT_ERR",
            499,
        ));
    }
    Ok(())
}

pub(crate) fn directory(root: &Path, location: &Value) -> Result<PathBuf, ContractError> {
    let id = identity(&location["Id"])?;
    let id = id.as_str().unwrap().to_ascii_lowercase();
    if location["Trash"] == true {
        return Ok(root.join("Trash").join(id));
    }
    if location["ProjectId"].is_null() {
        return Ok(root.join("Chats").join(id));
    }
    let project_id = identity(&location["ProjectId"])?;
    Ok(root
        .join("Projects")
        .join(project_id.as_str().unwrap().to_ascii_lowercase())
        .join("Sessions")
        .join(id))
}

fn lines(bytes: &[u8]) -> Vec<String> {
    let source = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
    if source.ends_with('\n') || source.is_empty() {
        lines.pop();
    }
    lines
}

#[derive(Deserialize)]
struct Event<'a> {
    version: u64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(borrow)]
    message: &'a RawValue,
}

fn projected_event_identity(raw: &str) -> Result<(String, String), ContractError> {
    let projection = raw_json::scalar_projection(raw);
    let event: Value = serde_json::from_str(&projection)
        .map_err(|_| error("会话事件格式无效。", "CORRUPT_CONVERSATION", 500))?;
    if event["version"] != 1 || event["type"] != "message.upsert" {
        return Err(error("会话事件格式无效。", "CORRUPT_CONVERSATION", 500));
    }
    let id = identity(&event["message"]["Id"])?;
    let role = event["message"]["Role"]
        .as_str()
        .filter(|role| ["user", "assistant", "system", "tool"].contains(role))
        .ok_or_else(|| error("消息角色格式无效。", "INVALID_CONVERSATION_DATA", 400))?;
    Ok((id.as_str().unwrap().to_ascii_lowercase(), role.to_owned()))
}

pub(crate) fn event_identity(raw: &str) -> Result<(String, String), ContractError> {
    // Borrow full bodies without decoding them; only ID/role scalars participate in ordering.
    // 借用完整正文而不解码，仅身份和角色标量参与事件排序，原始 UTF-16 字节继续保留。
    let event: HashMap<String, &RawValue> = match serde_json::from_str(raw) {
        Ok(event) => event,
        // Legacy scalar projection also accepts isolated UTF-16 in unused object keys.
        // 旧标量投影也接受未使用对象键中的孤立 UTF-16；罕见兼容路径保留原验证行为。
        Err(_) => return projected_event_identity(raw),
    };
    let scalar = |value: Option<&&RawValue>| -> Result<Value, ContractError> {
        value.map_or(Ok(Value::Null), |value| {
            serde_json::from_str(&raw_json::scalar_projection(value.get()))
                .map_err(|_| error("会话事件格式无效。", "CORRUPT_CONVERSATION", 500))
        })
    };
    if scalar(event.get("version"))? != 1 || scalar(event.get("type"))? != "message.upsert" {
        return Err(error("会话事件格式无效。", "CORRUPT_CONVERSATION", 500));
    }
    let message: HashMap<String, &RawValue> = match event.get("message") {
        Some(message) => match serde_json::from_str(message.get()) {
            Ok(message) => message,
            Err(_) => return projected_event_identity(raw),
        },
        None => HashMap::new(),
    };
    let id = identity(&scalar(message.get("Id"))?)?;
    let role = scalar(message.get("Role"))?;
    let role = role
        .as_str()
        .filter(|role| ["user", "assistant", "system", "tool"].contains(role))
        .ok_or_else(|| error("消息角色格式无效。", "INVALID_CONVERSATION_DATA", 400))?;
    Ok((id.as_str().unwrap().to_ascii_lowercase(), role.to_owned()))
}

fn expected(bytes: Option<&[u8]>, arguments: &Value) -> Result<(), ContractError> {
    let actual = bytes.map(managed_files::digest);
    if actual.as_deref() != arguments["expectedHash"].as_str() {
        return Err(error(
            "会话日志在操作期间已变化，请重新读取。",
            "CONVERSATION_LOG_CHANGED",
            409,
        ));
    }
    Ok(())
}

/// The native writer checks the latest stable ID and role before appending exact codec bytes.
/// 原生写入器在追加原始编码字节前检查稳定 ID 和角色，恢复重放保持幂等。
pub(crate) fn append_unlocked(
    root: &Path,
    location: &Value,
    raw_message: &str,
    unchanged: bool,
) -> Result<Value, ContractError> {
    let folder = directory(root, location)?;
    let path = folder.join("events.jsonl");
    managed_files::inspect(root, &folder, true, false, u64::MAX)?;
    let bytes = managed_files::read(root, &path, u64::MAX)?.unwrap_or_default();
    let incoming_raw =
        format!("{{\"version\":1,\"type\":\"message.upsert\",\"message\":{raw_message}}}");
    let (id, role) = event_identity(&incoming_raw)?;
    let mut previous = None;
    for line in String::from_utf8_lossy(&bytes).split_terminator('\n') {
        check_cancelled()?;
        let (saved_id, saved_role) = event_identity(line)?;
        if saved_id == id {
            let event: Event<'_> = serde_json::from_str(line)
                .map_err(|_| error("会话事件格式无效。", "CORRUPT_CONVERSATION", 500))?;
            if event.version != 1 || event.kind != "message.upsert" {
                return Err(error("会话事件格式无效。", "CORRUPT_CONVERSATION", 500));
            }
            previous = Some((saved_role, event.message.get().to_owned()));
        }
    }
    if let Some((saved_role, saved_raw)) = previous {
        if saved_role != role {
            return Err(error(
                "消息 ID 已用于其他角色。",
                "INVALID_CONVERSATION_DATA",
                400,
            ));
        }
        if unchanged || saved_raw == raw_message {
            return Ok(json!({"appended": false, "hash": managed_files::digest(&bytes)}));
        }
    }
    check_cancelled()?;
    managed_files::inspect(root, &path, false, true, u64::MAX)?;
    let mut handle = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(io_error)?;
    handle
        .write_all(incoming_raw.as_bytes())
        .map_err(io_error)?;
    handle.write_all(b"\n").map_err(io_error)?;
    handle.sync_all().map_err(io_error)?;
    let mut committed = bytes;
    committed.extend_from_slice(incoming_raw.as_bytes());
    committed.push(b'\n');
    Ok(json!({"appended": true, "hash": managed_files::digest(&committed)}))
}

/// Byte-compatible parsing remains at the gateway boundary; native code owns snapshot verification,
/// stable ordering and the only permitted tail-repair/write effects.
/// 网关保留字节兼容解析；原生代码拥有快照复验、稳定顺序及唯一允许的尾部恢复与写入效果。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let _guard = CONVERSATION_TRANSACTIONS
        .lock()
        .map_err(|_| error("会话存储锁失效。", "CONVERSATION_STORAGE_FAILED", 500))?;
    check_cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let folder = directory(root, &arguments["location"])?;
    let path = folder.join("events.jsonl");
    let bytes = managed_files::read(root, &path, u64::MAX)?;
    match operation {
        "storage.logs.read" => {
            if let Some(bytes) = bytes {
                if let Err(encoding) = std::str::from_utf8(&bytes) {
                    let tail_start = bytes
                        .iter()
                        .rposition(|byte| *byte == b'\n')
                        .map_or(0, |index| index + 1);
                    // Lossy projection is permitted only to classify an incomplete final scalar;
                    // the recovery receipt still backs up the original bytes before truncation.
                    // 仅为识别末尾截断字符允许有损投影；恢复回执在截断前备份完整原始残尾字节。
                    if bytes.ends_with(b"\n")
                        || encoding.error_len().is_some()
                        || encoding.valid_up_to() < tail_start
                    {
                        return Err(error(
                            "历史包含无效 UTF-8 编码。",
                            "INVALID_CONVERSATION_HISTORY",
                            400,
                        ));
                    }
                }
                let hash = managed_files::digest(&bytes);
                check_cancelled()?;
                // A cache hit still verifies all authoritative bytes, but never copies the body across the bridge.
                // 缓存命中仍复验全部正式字节，但不再将正文重复复制并传过原生桥接。
                if arguments["knownHash"].as_str() == Some(hash.as_str()) {
                    Ok(json!({"exists": true, "hash": hash, "unchanged": true}))
                } else {
                    Ok(json!({"exists": true, "hash": hash, "lines": lines(&bytes)}))
                }
            } else {
                Ok(json!({"exists": false, "hash": null, "lines": []}))
            }
        }
        "storage.logs.append" => {
            expected(bytes.as_deref(), arguments)?;
            append_unlocked(
                root,
                &arguments["location"],
                arguments["rawMessage"]
                    .as_str()
                    .ok_or_else(|| error("消息编码无效。", "INVALID_CONVERSATION_DATA", 400))?,
                arguments["unchanged"] == true,
            )
        }
        "storage.logs.finalize" => {
            expected(bytes.as_deref(), arguments)?;
            let mut bytes = bytes.unwrap_or_default();
            // Counting line delimiters does not require another complete decoded copy of the log.
            // 统计行分隔符不必再次完整解码并复制日志，尾部恢复判定仍使用原始字节。
            let record_count = bytes.iter().filter(|byte| **byte == b'\n').count()
                + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
            let invalid = arguments["invalidLine"]
                .as_u64()
                .map(|value| value as usize);
            if let Some(index) = invalid {
                if index + 1 != record_count || bytes.ends_with(b"\n") {
                    return Err(error(
                        "会话日志中间记录损坏，已停止读取并保留原文件。",
                        "CORRUPT_CONVERSATION",
                        500,
                    ));
                }
                let valid_length = bytes
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(0, |index| index + 1);
                let recovery = folder.join(format!(
                    "events.jsonl.recovered-tail-{}-{}",
                    std::process::id(),
                    RECOVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
                managed_files::inspect(root, &recovery, false, true, u64::MAX)?;
                let mut backup = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(recovery)
                    .map_err(io_error)?;
                backup.write_all(&bytes[valid_length..]).map_err(io_error)?;
                backup.sync_all().map_err(io_error)?;
                check_cancelled()?;
                let repaired = OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(io_error)?;
                repaired.set_len(valid_length as u64).map_err(io_error)?;
                repaired.sync_all().map_err(io_error)?;
                bytes.truncate(valid_length);
            } else if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                check_cancelled()?;
                let mut handle = OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .map_err(io_error)?;
                handle.write_all(b"\n").map_err(io_error)?;
                handle.sync_all().map_err(io_error)?;
                bytes.push(b'\n');
            }
            let mut positions = HashMap::new();
            let mut selected = Vec::new();
            for (index, line) in String::from_utf8_lossy(&bytes)
                .split_terminator('\n')
                .enumerate()
            {
                check_cancelled()?;
                let (id, _) = event_identity(line)?;
                if let Some(position) = positions.get(&id) {
                    selected[*position] = index;
                } else {
                    positions.insert(id, selected.len());
                    selected.push(index);
                }
            }
            Ok(
                json!({"selected": selected, "hash": if arguments["exists"] == true || !bytes.is_empty() { Some(managed_files::digest(&bytes)) } else { None }}),
            )
        }
        _ => Err(error(
            "未知会话日志操作。",
            "INVALID_CONVERSATION_DATA",
            400,
        )),
    }
}
