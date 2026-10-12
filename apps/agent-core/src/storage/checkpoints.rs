use super::{
    ContractError, ContractResult, cancellation, hash, job_contracts, managed_files, raw_json,
};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

const MAX_CHECKPOINT_BYTES: u64 = 128 * 1024 * 1024;
static TRANSACTIONS: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    job_id: String,
    checkpoint_id: String,
    sources: Box<RawValue>,
    checksum: String,
}

fn failure(message: impl Into<String>) -> ContractError {
    ContractError::new(message, "INVALID_RETRIEVAL_CHECKPOINT", 409)
}
fn io_failure(error: std::io::Error) -> ContractError {
    ContractError::new(
        format!("Checkpoint storage failed. / 检查点存储失败：{error}"),
        "STORAGE_IO_FAILED",
        500,
    )
}
fn capacity() -> ContractError {
    ContractError::new(
        "索引恢复日志或来源超过本地预算。",
        "RETRIEVAL_CHECKPOINT_TOO_LARGE",
        413,
    )
}

fn check_cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        return Err(ContractError::new(
            "Checkpoint cancelled. / 检查点操作已取消。",
            "ABORT_ERR",
            499,
        ));
    }
    Ok(())
}

fn validate_job_id(value: &Value) -> Result<&str, ContractError> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .ok_or_else(|| {
            ContractError::new("索引任务身份无效。", "INVALID_RETRIEVAL_CHECKPOINT", 400)
        })
}

/// Verify complete records against their original JSON order before repairing only the uncommitted tail.
/// 完整回执先按原始 JSON 属性顺序校验，再只修复未提交尾部。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let _transaction = TRANSACTIONS
        .lock()
        .map_err(|_| failure("checkpoint lock failed"))?;
    check_cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let job_id = validate_job_id(&arguments["jobId"])?;
    let path = root
        .join("Retrieval")
        .join("checkpoints")
        .join(format!("{job_id}.jsonl"));
    if operation == "storage.checkpoints.remove" {
        if managed_files::inspect(root, &path, false, true, u64::MAX)? {
            fs::remove_file(path).map_err(io_failure)?;
        }
        return Ok(Value::Null);
    }
    if operation == "storage.checkpoints.append" {
        let payload = arguments["payload"]
            .as_str()
            .ok_or_else(|| failure("invalid receipt codec"))?;
        let projected: Value = serde_json::from_str(&raw_json::scalar_projection(payload))
            .map_err(|_| failure("索引批次回执格式无效。"))?;
        if projected["jobId"] != job_id || !hash(&projected["checkpointId"]) {
            return Err(failure("索引批次回执身份无效。"));
        }
        let sources = projected["sources"]
            .as_array()
            .filter(|sources| sources.len() <= 128)
            .ok_or_else(|| failure("索引批次回执过大。"))?;
        for source in sources {
            job_contracts::checkpoint_source(source)?;
        }
        let digest = managed_files::digest(payload.as_bytes());
        let line = format!(
            "{},\"checksum\":\"{digest}\"}}\n",
            payload
                .strip_suffix('}')
                .ok_or_else(|| failure("invalid receipt codec"))?
        );
        let parent = path.parent().unwrap();
        managed_files::inspect(root, parent, true, false, MAX_CHECKPOINT_BYTES)?;
        let exists = managed_files::inspect(root, &path, false, true, MAX_CHECKPOINT_BYTES)?;
        if (if exists {
            fs::metadata(&path).map_err(io_failure)?.len()
        } else {
            0
        }) + line.len() as u64
            > MAX_CHECKPOINT_BYTES
        {
            return Err(capacity());
        }
        check_cancelled()?;
        let mut handle = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io_failure)?;
        handle.write_all(line.as_bytes()).map_err(io_failure)?;
        handle.sync_all().map_err(io_failure)?;
        return Ok(json!({"bytes": line.len()}));
    }
    if operation != "storage.checkpoints.read" {
        return Err(failure("unknown checkpoint operation"));
    }
    job_contracts::checkpoint(&arguments["checkpoint"], &arguments["timestamps"])?;
    let bytes = managed_files::read(root, &path, MAX_CHECKPOINT_BYTES).map_err(|error| {
        if error.code == "CORRUPT_MEMORY" {
            capacity()
        } else {
            error
        }
    })?;
    let Some(bytes) = bytes else {
        return Ok(json!([]));
    };
    let boundary = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut source_order = Vec::new();
    let mut selected: HashMap<Vec<u16>, String> = HashMap::new();
    for line in bytes[..boundary].split(|byte| *byte == b'\n') {
        check_cancelled()?;
        if line.is_empty() {
            continue;
        }
        let raw = std::str::from_utf8(line).map_err(|_| failure("索引恢复日志损坏。"))?;
        let record: Record =
            serde_json::from_str(raw).map_err(|_| failure("索引恢复日志损坏。"))?;
        let payload = format!(
            "{{\"jobId\":{},\"checkpointId\":{},\"sources\":{}}}",
            serde_json::to_string(&record.job_id).unwrap(),
            serde_json::to_string(&record.checkpoint_id).unwrap(),
            record.sources.get()
        );
        if record.job_id != job_id
            || !hash(&json!(record.checkpoint_id))
            || record.checksum != managed_files::digest(payload.as_bytes())
        {
            return Err(failure("索引恢复日志损坏。"));
        }
        let sources: Vec<Box<RawValue>> = serde_json::from_str(record.sources.get())
            .map_err(|_| failure("索引恢复日志损坏。"))?;
        if sources.len() > 128 {
            return Err(failure("索引恢复日志损坏。"));
        }
        for raw_source in sources {
            let source: Value =
                serde_json::from_str(&raw_json::scalar_projection(raw_source.get()))
                    .map_err(|_| failure("索引恢复日志损坏。"))?;
            job_contracts::checkpoint_source(&source)?;
            if record.checkpoint_id == arguments["checkpoint"]["checkpointId"] {
                let raw_fields: HashMap<String, Box<RawValue>> =
                    serde_json::from_str(raw_source.get())
                        .map_err(|_| failure("索引恢复日志损坏。"))?;
                let id = raw_json::utf16_identity(raw_fields["sourceId"].get())
                    .ok_or_else(|| failure("索引恢复来源身份无效。"))?;
                if !selected.contains_key(&id) {
                    source_order.push(id.clone());
                }
                selected.insert(id, raw_source.get().to_owned());
                if selected.len() > 200000 {
                    return Err(capacity());
                }
            }
        }
    }
    if boundary < bytes.len() {
        check_cancelled()?;
        let handle = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(io_failure)?;
        handle.set_len(boundary as u64).map_err(io_failure)?;
        handle.sync_all().map_err(io_failure)?;
    }
    Ok(json!(
        source_order
            .into_iter()
            .filter_map(|id| selected.remove(&id))
            .collect::<Vec<_>>()
    ))
}
