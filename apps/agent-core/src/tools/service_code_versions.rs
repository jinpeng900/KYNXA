use std::io::Read;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde::Serialize;
use serde_json::{Value, json};

use super::local_files::{absolute_path, file_version, handle_version, inspect, open_read, sha256};
use super::service_error::{ServiceError, ServiceResult};

const MAX_TARGETS: usize = 64;
const MAX_HASH_BYTES: u64 = 1024 * 1024;

/// The signature covers only authorized observed targets, not the whole repository.
/// 签名只覆盖已授权且已观察的目标，不代表扫描了整个仓库。
#[derive(Default)]
pub struct CodeVersions {
    targets: Vec<Value>,
    overflow: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionRecord {
    path_id: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

fn record(path: &str, state: &str, digest: Option<String>, code: Option<String>) -> Value {
    serde_json::to_value(VersionRecord {
        path_id: sha256(path.as_bytes()),
        state: state.into(),
        sha256: digest,
        code,
    })
    .unwrap()
}

fn cancellation_check(cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<()> {
    if cancellation
        .as_ref()
        .is_some_and(|token| token.load(Ordering::Acquire))
    {
        return Err(ServiceError::new(
            "TOOL_CANCELLED",
            "代码版本读取已取消。",
            409,
        ));
    }
    Ok(())
}

fn unavailable(arguments: &Value) -> Value {
    let code = arguments["code"]
        .as_str()
        .filter(|code| super::intent::matches(r"^[A-Z][A-Z0-9_]{0,79}$", code))
        .unwrap_or("CODE_VERSION_UNAVAILABLE");
    record(
        arguments["binding"]["path"].as_str().unwrap_or(""),
        "unavailable",
        None,
        Some(code.into()),
    )
}

fn snapshot(arguments: &Value, cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<Value> {
    let binding = &arguments["binding"];
    let path_text = binding["path"].as_str().unwrap_or("");
    let changed = || {
        ServiceError::new(
            "CODE_VERSION_CHANGED",
            "Target changed during hashing.",
            409,
        )
    };
    let result = (|| {
        cancellation_check(cancellation)?;
        super::service_filesystem::revalidate(binding)?;
        let path = absolute_path(path_text);
        let Some(metadata) = inspect(&path, true, binding["allowHardLinks"] == true)? else {
            return Ok(record(path_text, "missing", None, None));
        };
        if metadata.is_dir() {
            return Ok(record(path_text, "directory", None, None));
        }
        if !metadata.is_file() || metadata.len() > MAX_HASH_BYTES {
            return Err(ServiceError::new(
                "CODE_VERSION_TARGET_LIMIT",
                "Unbounded target.",
                400,
            ));
        }
        let original = file_version(&path, &metadata)?;
        let mut file = open_read(&path)?;
        let opened = file.metadata().map_err(ServiceError::io)?;
        if !opened.is_file() || handle_version(&file, &opened)? != original {
            return Err(changed());
        }
        let mut bytes = Vec::with_capacity(opened.len() as usize + 1);
        let mut buffer = [0_u8; 64 * 1024];
        while bytes.len() as u64 <= opened.len() {
            cancellation_check(cancellation)?;
            let remaining =
                (opened.len() + 1 - bytes.len() as u64).min(buffer.len() as u64) as usize;
            let length = file
                .read(&mut buffer[..remaining])
                .map_err(ServiceError::io)?;
            if length == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..length]);
        }
        super::service_filesystem::revalidate(binding)?;
        let after =
            inspect(&path, false, binding["allowHardLinks"] == true)?.ok_or_else(changed)?;
        if bytes.len() as u64 != opened.len()
            || bytes.len() as u64 > MAX_HASH_BYTES
            || original != handle_version(&file, &file.metadata().map_err(ServiceError::io)?)?
            || original != file_version(&path, &after)?
        {
            return Err(changed());
        }
        Ok(record(path_text, "present", Some(sha256(&bytes)), None))
    })();
    cancellation_check(cancellation)?;
    Ok(result.unwrap_or_else(|error: ServiceError| {
        unavailable(&json!({"binding":binding,"code":error.code}))
    }))
}

impl CodeVersions {
    pub fn apply(
        &mut self,
        operation: &str,
        arguments: &Value,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<Value> {
        match operation {
            "versions.observe" => {
                let binding = &arguments["binding"];
                if binding.is_null() {
                    return Ok(Value::Null);
                }
                let index = self
                    .targets
                    .iter()
                    .position(|target| target["path"] == binding["path"]);
                if index.is_none() && self.targets.len() >= MAX_TARGETS {
                    self.overflow = true;
                    return Ok(Value::Null);
                }
                let mut binding = binding.clone();
                binding["allowMissing"] = json!(true);
                if let Some(index) = index {
                    self.targets[index] = binding;
                } else {
                    self.targets.push(binding);
                }
                Ok(Value::Null)
            }
            "versions.targets" => Ok(json!({"targets":self.targets,"overflow":self.overflow})),
            "versions.snapshot" => snapshot(arguments, cancellation),
            "versions.unavailable" => Ok(unavailable(arguments)),
            "versions.finish" => {
                let mut records = arguments["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|file| VersionRecord {
                        path_id: file["pathId"].as_str().unwrap_or("").into(),
                        state: file["state"].as_str().unwrap_or("").into(),
                        sha256: file["sha256"].as_str().map(str::to_owned),
                        code: file["code"].as_str().map(str::to_owned),
                    })
                    .collect::<Vec<_>>();
                records.sort_by(|left, right| left.path_id.cmp(&right.path_id));
                // Field order is part of the historical signature codec, independently of JSON map ordering.
                // 字段顺序属于历史签名编码合同，不依赖 JSON 映射自身的键排序。
                let signature = sha256(serde_json::to_string(&records).unwrap().as_bytes());
                let overflow = arguments["overflow"] == true;
                let complete = !overflow
                    && records
                        .iter()
                        .any(|file| ["present", "missing"].contains(&file.state.as_str()))
                    && records.iter().all(|file| file.state != "unavailable");
                Ok(
                    json!({"signature":signature,"complete":complete,"files":records,"overflow":overflow,
                    "coverage":"observed-filesystem-targets","exhaustive":false}),
                )
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown code version service operation.",
                400,
            )),
        }
    }
}
