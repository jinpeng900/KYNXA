use std::collections::HashSet;

use serde_json::{Value, json};

use super::{ContractError, ContractResult, MAX_SAFE_INTEGER, member, object, safe_integer};

fn failure(message: impl Into<String>) -> ContractError {
    ContractError::new(message, "INVALID_RETRIEVAL_INPUT", 400)
}
fn require_keys(value: &Value, allowed: &[&str], label: &str) -> Result<(), ContractError> {
    let fields = object(
        value,
        &format!("{label} must be an object. / 检索配置必须是对象。"),
        "INVALID_RETRIEVAL_INPUT",
        400,
    )?;
    for name in fields.keys() {
        if !allowed.contains(&name.as_str()) {
            return Err(failure(format!(
                "Unknown {label} field: {name}. / 存在不支持的配置字段。"
            )));
        }
    }
    Ok(())
}
fn integer(value: &Value, minimum: u64, maximum: u64, name: &str) -> ContractResult {
    if !safe_integer(value, minimum, maximum) {
        return Err(failure(format!(
            "Invalid {name} range. / 数值配置超出允许范围。"
        )));
    }
    Ok(value.clone())
}
fn boolean(value: &Value, name: &str) -> ContractResult {
    if !value.is_boolean() {
        return Err(failure(format!(
            "{name} must be boolean. / 开关配置必须为布尔值。"
        )));
    }
    Ok(value.clone())
}
fn choice(value: &Value, choices: &[&str], name: &str) -> ContractResult {
    if !member(value, choices) {
        return Err(failure(format!("Invalid {name}. / 配置选项无效。")));
    }
    Ok(value.clone())
}
fn reference(value: &Value, name: &str, nullable: bool) -> ContractResult {
    if nullable && value.is_null() {
        return Ok(Value::Null);
    }
    if !value.as_str().is_some_and(|value| {
        !value.is_empty()
            && value.len() <= 256
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
            })
    }) {
        return Err(failure(format!(
            "Invalid {name} reference. / 能力引用格式无效。"
        )));
    }
    Ok(value.clone())
}
fn indexing_defaults() -> Value {
    json!({"maximumFiles": 20000, "maximumSourceBytes": 32 * 1024 * 1024, "maximumTotalBytes": 512 * 1024 * 1024,
        "maximumEntries": 200000, "batchSize": 32, "maximumDocumentInputBytes": 32 * 1024 * 1024,
        "maximumDocumentOutputBytes": 2 * 1024 * 1024, "maximumPdfPages": 100})
}
fn ann_defaults() -> Value {
    json!({"mode": "auto", "adaptive": true, "threshold": 50000, "maxCachedShards": 4, "maxShardBytes": 256 * 1024 * 1024,
        "connectivity": 16, "expansionAdd": 128, "expansionSearch": 128, "exactScanLimit": 1000000})
}
fn defaults() -> Value {
    json!({"schemaVersion": 1, "revision": 0, "local": {"enabled": true, "semantic": "auto", "vectorBackend": "sqlite",
        "embeddingProfileId": "builtin-multilingual", "embeddingDevicePolicy": "auto",
        "rrfK": 60, "indexing": indexing_defaults(), "ann": ann_defaults()},
        "web": {"mode": "auto", "providerId": "auto", "depth": "standard", "language": "auto", "browserRead": "auto"},
        "cache": {"memoryLimitBytes": 64 * 1024 * 1024, "diskLimitBytes": 512 * 1024 * 1024}})
}
fn indexing(value: &Value, partial: bool) -> ContractResult {
    require_keys(
        value,
        &[
            "maximumFiles",
            "maximumSourceBytes",
            "maximumTotalBytes",
            "maximumEntries",
            "batchSize",
            "maximumDocumentInputBytes",
            "maximumDocumentOutputBytes",
            "maximumPdfPages",
        ],
        "indexing limits",
    )?;
    let mut result = if partial {
        json!({})
    } else {
        indexing_defaults()
    };
    for (name, value) in value.as_object().unwrap() {
        let maximum = match name.as_str() {
            "maximumFiles" => 100000,
            "maximumSourceBytes" => 256 * 1024 * 1024,
            "maximumTotalBytes" => 8 * 1024 * 1024 * 1024,
            "maximumEntries" => 2000000,
            "batchSize" => 128,
            "maximumDocumentInputBytes" => 32 * 1024 * 1024,
            "maximumDocumentOutputBytes" => 2 * 1024 * 1024,
            _ => 100,
        };
        result[name] = integer(value, 1, maximum, name)?;
    }
    Ok(result)
}
fn ann(value: &Value) -> ContractResult {
    let failure = |message| ContractError::new(message, "INVALID_RETRIEVAL_ANN", 400);
    let Some(fields) = value.as_object() else {
        return Err(failure("Invalid ANN options. / 本地向量索引配置无效。"));
    };
    let mut result = ann_defaults();
    for (name, value) in fields {
        match name.as_str() {
            "adaptive" if !value.is_boolean() => {
                return Err(failure(
                    "Invalid adaptive ANN setting. / 自适应向量预算无效。",
                ));
            }
            "mode" if !member(value, &["auto", "off", "ann", "exact"]) => {
                return Err(failure("Invalid ANN mode. / 本地向量索引模式无效。"));
            }
            "adaptive" | "mode" => {}
            _ => {
                let bounds = match name.as_str() {
                    "threshold" | "exactScanLimit" => Some((1, 1000000)),
                    "maxCachedShards" => Some((1, 16)),
                    "maxShardBytes" => Some((1024 * 1024, 1024 * 1024 * 1024)),
                    "connectivity" => Some((4, 64)),
                    "expansionAdd" | "expansionSearch" => Some((16, 2048)),
                    _ => None,
                };
                if !bounds.is_some_and(|(minimum, maximum)| safe_integer(value, minimum, maximum)) {
                    return Err(failure(
                        "Invalid ANN resource budget. / 本地向量索引资源预算无效。",
                    ));
                }
            }
        }
        result[name] = value.clone();
    }
    Ok(result)
}
fn merge_values(base: &Value, patch: &Value) -> Value {
    let mut result = base.as_object().cloned().unwrap_or_default();
    if let Some(patch) = patch.as_object() {
        result.extend(patch.clone());
    }
    Value::Object(result)
}
fn merge_local(base: &Value, patch: &Value) -> Value {
    let mut result = merge_values(base, patch);
    for name in ["indexing", "ann"] {
        if base.get(name).is_some() || patch.get(name).is_some() {
            result[name] = merge_values(&base[name], &patch[name]);
        }
    }
    // Ignore retired cross-encoder settings while accepting older persisted/client schemas.
    // 接受旧持久设置与客户端字段，但不能重新启用已移除的交叉编码器。
    if let Some(fields) = result.as_object_mut() {
        for name in ["rerankProfileId", "rerankDevicePolicy", "rerankCandidates"] { fields.remove(name); }
    }
    if result.get("rrfK").is_some() {
        result["rrfK"] = json!(result["rrfK"].as_u64().unwrap_or(60).clamp(20, 100));
    }
    result
}
fn merge_settings(base: &Value, patch: &Value) -> Value {
    let mut result = base.clone();
    result["local"] = merge_local(&base["local"], &patch["local"]);
    for name in ["web", "cache"] {
        result[name] = merge_values(&base[name], &patch[name]);
    }
    result
}
fn patch(value: &Value) -> ContractResult {
    require_keys(value, &["local", "web", "cache"], "retrieval settings")?;
    let mut result = json!({});
    if let Some(local) = value.get("local") {
        require_keys(
            local,
            &[
                "enabled",
                "semantic",
                "vectorBackend",
                "embeddingProfileId",
                "embeddingDevicePolicy",
                "rerankProfileId",
                "rerankCandidates",
                "rerankDevicePolicy",
                "rrfK",
                "indexing",
                "ann",
            ],
            "local",
        )?;
        let mut clean = json!({});
        for (name, value) in local.as_object().unwrap() {
            clean[name] = match name.as_str() {
                "enabled" => boolean(value, name)?,
                "semantic" => choice(value, &["auto", "off"], name)?,
                "embeddingDevicePolicy" => {
                    choice(value, &["auto", "cpu", "gpu"], name)?
                }
                "vectorBackend" => choice(value, &["sqlite"], name)?,
                "rerankProfileId" | "rerankCandidates" | "rerankDevicePolicy" => Value::Null,
                "rrfK" => json!(integer(value, 20, 100, name)?),
                "indexing" => indexing(value, true)?,
                "ann" => {
                    ann(value).map_err(|error| failure(error.message))?;
                    value.clone()
                }
                _ => reference(value, name, true)?,
            };
        }
        result["local"] = clean;
    }
    if let Some(web) = value.get("web") {
        require_keys(
            web,
            &["mode", "providerId", "depth", "language", "browserRead"],
            "web",
        )?;
        let mut clean = json!({});
        for (name, value) in web.as_object().unwrap() {
            clean[name] = match name.as_str() {
                "mode" | "browserRead" => choice(value, &["auto", "off"], name)?,
                "depth" => choice(value, &["standard", "deep"], name)?,
                "language" => choice(value, &["auto", "zh-CN", "en", "en-US"], name)?,
                _ => reference(value, name, false)?,
            };
        }
        result["web"] = clean;
    }
    if let Some(cache) = value.get("cache") {
        require_keys(cache, &["memoryLimitBytes", "diskLimitBytes"], "cache")?;
        let mut clean = json!({});
        for (name, value) in cache.as_object().unwrap() {
            clean[name] = integer(
                value,
                0,
                if name == "memoryLimitBytes" {
                    512 * 1024 * 1024
                } else {
                    8 * 1024 * 1024 * 1024
                },
                name,
            )?;
        }
        result["cache"] = clean;
    }
    Ok(result)
}
fn project_patch(value: &Value) -> ContractResult {
    require_keys(
        value,
        &["overrides", "indexingSources"],
        "project retrieval settings",
    )?;
    let mut result = json!({});
    if let Some(overrides) = value.get("overrides") {
        require_keys(overrides, &["local", "web", "cache"], "project overrides")?;
        let updates = overrides
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut clean = patch(&Value::Object(updates))?;
        for (name, value) in overrides.as_object().unwrap() {
            if value.is_null() {
                clean[name] = Value::Null;
            }
        }
        result["overrides"] = clean;
    }
    if let Some(sources) = value.get("indexingSources") {
        require_keys(
            sources,
            &["mountedFolder", "knowledgeIds"],
            "indexing sources",
        )?;
        let mut clean = json!({});
        if let Some(folder) = sources.get("mountedFolder") {
            require_keys(folder, &["enabled"], "mounted folder")?;
            clean["mountedFolder"] =
                json!({"enabled": boolean(&folder["enabled"], "mountedFolder.enabled")?});
        }
        if let Some(ids) = sources.get("knowledgeIds") {
            let Some(ids) = ids.as_array().filter(|ids| ids.len() <= 100000) else {
                return Err(failure("Invalid knowledge selection. / 资料选择列表无效。"));
            };
            // Retain first-selection order with linear duplicate detection.
            // 以线性去重保留首次选择顺序，避免大型资料列表触发平方级比较。
            let mut seen = HashSet::with_capacity(ids.len());
            let mut unique = Vec::with_capacity(ids.len());
            for id in ids {
                reference(id, "knowledgeId", false)?;
                if seen.insert(id.as_str().unwrap()) {
                    unique.push(id.clone());
                }
            }
            clean["knowledgeIds"] = json!(unique);
        }
        result["indexingSources"] = clean;
    }
    Ok(result)
}
fn default_project(project_id: &Value) -> Value {
    json!({"schemaVersion": 1, "projectId": project_id, "revision": 0, "overrides": {},
        "indexingSources": {"mountedFolder": {"enabled": true, "bindingRevision": 0}, "knowledgeIds": []}})
}
fn document(value: &Value, project_id: &Value) -> ContractResult {
    object(
        value,
        "Stored retrieval settings must be an object. / 检索配置必须是对象。",
        "INVALID_RETRIEVAL_INPUT",
        400,
    )?;
    if value["schemaVersion"] != 1 {
        return Err(ContractError::new(
            "Unsupported retrieval settings version. / 检索配置版本不受支持，原文件已保留。",
            "UNSUPPORTED_RETRIEVAL_SETTINGS_VERSION",
            409,
        ));
    }
    integer(&value["revision"], 0, MAX_SAFE_INTEGER, "revision")?;
    if project_id.is_null() {
        require_keys(
            value,
            &["schemaVersion", "revision", "local", "web", "cache"],
            "stored global settings",
        )?;
        let mut input = json!({});
        for name in ["local", "web", "cache"] {
            if let Some(value) = value.get(name) {
                input[name] = value.clone();
            }
        }
        let mut result = merge_settings(&defaults(), &patch(&input)?);
        result["revision"] = value["revision"].clone();
        return Ok(result);
    }
    require_keys(
        value,
        &[
            "schemaVersion",
            "projectId",
            "revision",
            "overrides",
            "indexingSources",
        ],
        "stored project settings",
    )?;
    if &value["projectId"] != project_id {
        return Err(ContractError::new(
            "Project settings identity mismatch. / 工作配置身份不匹配。",
            "CORRUPT_RETRIEVAL_SETTINGS",
            409,
        ));
    }
    let mut result = default_project(project_id);
    result["revision"] = value["revision"].clone();
    result["overrides"] = patch(
        value
            .get("overrides")
            .filter(|value| !value.is_null())
            .unwrap_or(&json!({})),
    )?;
    let sources = value
        .get("indexingSources")
        .filter(|value| !value.is_null())
        .unwrap_or(&result["indexingSources"]);
    require_keys(
        sources,
        &["mountedFolder", "knowledgeIds"],
        "stored indexing sources",
    )?;
    require_keys(
        &sources["mountedFolder"],
        &["enabled", "bindingRevision"],
        "stored folder settings",
    )?;
    let mut clean = project_patch(&json!({"indexingSources": {"mountedFolder": {"enabled": sources["mountedFolder"]["enabled"]}, "knowledgeIds": sources.get("knowledgeIds").filter(|value| !value.is_null()).unwrap_or(&json!([]))}}))?["indexingSources"].clone();
    clean["mountedFolder"]["bindingRevision"] = integer(
        &sources["mountedFolder"]["bindingRevision"],
        0,
        MAX_SAFE_INTEGER,
        "bindingRevision",
    )?;
    // Work preparation is automatic; old opt-in documents cannot disable the active binding.
    // 工作资料自动准备；旧版选择加入配置不能关闭当前目录绑定。
    clean["mountedFolder"]["enabled"] = json!(true);
    result["indexingSources"] = clean;
    Ok(result)
}
fn project_next(args: &Value) -> ContractResult {
    let current = &args["current"];
    let patch = &args["patch"];
    let mut result = current.clone();
    result["revision"] = json!(current["revision"].as_u64().unwrap_or(0) + 1);
    let mut overrides = json!({});
    for name in ["local", "web", "cache"] {
        if patch["overrides"].get(name) == Some(&Value::Null)
            || current["overrides"].get(name).is_none() && patch["overrides"].get(name).is_none()
        {
            continue;
        }
        overrides[name] = if name == "local" {
            merge_local(&current["overrides"][name], &patch["overrides"][name])
        } else {
            merge_values(&current["overrides"][name], &patch["overrides"][name])
        };
    }
    result["overrides"] = overrides;
    result["indexingSources"] =
        merge_values(&current["indexingSources"], &patch["indexingSources"]);
    let mut folder = merge_values(
        &current["indexingSources"]["mountedFolder"],
        &patch["indexingSources"]["mountedFolder"],
    );
    folder["enabled"] = json!(true);
    if current["indexingSources"]["mountedFolder"]["enabled"] != true {
        folder["bindingRevision"] = json!(
            current["indexingSources"]["mountedFolder"]["bindingRevision"]
                .as_u64()
                .unwrap_or(0)
                + 1
        );
    }
    result["indexingSources"]["mountedFolder"] = folder;
    Ok(result)
}

/// Settings remain CAS documents owned by the gateway; this module computes policy and validation only.
/// 配置仍由网关独占执行 CAS 持久化；此模块只计算策略和校验结果。
pub fn apply(operation: &str, args: &Value) -> ContractResult {
    match operation {
        "storage.settings.storageBudget" => {
            if !args["bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes <= 8 * 1024 * 1024)
            {
                return Err(ContractError::new(
                    "Retrieval settings exceed the storage limit. / 检索配置超过存储上限，原文件已保留。",
                    "RETRIEVAL_SETTINGS_LIMIT",
                    413,
                ));
            }
            Ok(Value::Null)
        }
        "storage.settings.ioFailure" => {
            let (code, message, status) = match args["code"].as_str() {
                Some("CORRUPT_MEMORY") => (
                    "CORRUPT_RETRIEVAL_SETTINGS",
                    "Settings file is too large. / 检索配置文件过大，原文件已保留。",
                    409,
                ),
                Some("MEMORY_CAPACITY_EXCEEDED") => (
                    "RETRIEVAL_SETTINGS_LIMIT",
                    "Retrieval settings exceed the storage limit. / 检索配置超过存储上限，原文件已保留。",
                    413,
                ),
                Some("MEMORY_CONFLICT") => (
                    "RETRIEVAL_SETTINGS_CONFLICT",
                    "Retrieval settings changed. / 检索配置已被修改，请刷新后重试。",
                    409,
                ),
                Some("INVALID_MEMORY_PATH") => (
                    "UNSAFE_RETRIEVAL_PATH",
                    "Unsafe settings path. / 检索配置包含链接或异常文件结构。",
                    409,
                ),
                _ => return Ok(Value::Null),
            };
            Ok(json!({"code":code,"message":message,"statusCode":status}))
        }
        "storage.settings.next" => {
            let current = &args["current"];
            let mutation = &args["mutation"];
            if current["revision"] != mutation["expectedRevision"] {
                return Err(ContractError::new(
                    "Retrieval settings changed. / 检索配置已被修改，请刷新后重试。",
                    "RETRIEVAL_SETTINGS_CONFLICT",
                    409,
                ));
            }
            if args["project"] == true {
                if args["folderless"] == true
                    && mutation["patch"]["indexingSources"]["mountedFolder"]["enabled"] == true
                {
                    return Err(ContractError::new(
                        "No mounted folder is available. / 此工作没有挂载文件夹。",
                        "RETRIEVAL_FOLDER_UNAVAILABLE",
                        409,
                    ));
                }
                project_next(&json!({"current":current,"patch":mutation["patch"]}))
            } else {
                let mut next = merge_settings(current, &mutation["patch"]);
                next["revision"] = json!(current["revision"].as_u64().unwrap_or(0) + 1);
                Ok(next)
            }
        }
        "storage.settings.mounted" => Ok(json!(
            args["relationship"]["folderPath"]
                .as_str()
                .is_some_and(|path| !path.is_empty())
                && args["relationship"]["isFolderlessWorkspace"] != true
                && args["relationship"]["isArchived"] != true
        )),
        "storage.settings.defaults" => Ok(defaults()),
        "storage.settings.indexing" => {
            indexing(&args["value"], args["partial"].as_bool().unwrap_or(false))
        }
        "storage.settings.ann" => ann(&args["value"]),
        "storage.settings.patch" => patch(&args["value"]),
        "storage.settings.projectPatch" => project_patch(&args["value"]),
        "storage.settings.defaultProject" => Ok(default_project(&args["projectId"])),
        "storage.settings.document" => document(&args["value"], &args["projectId"]),
        "storage.settings.mergeLocal" => Ok(merge_local(&args["base"], &args["patch"])),
        "storage.settings.merge" => Ok(merge_settings(&args["base"], &args["patch"])),
        "storage.settings.projectNext" => project_next(args),
        "storage.settings.mutation" => {
            require_keys(
                &args["value"],
                &["expectedRevision", "patch"],
                "settings mutation",
            )?;
            let revision = integer(
                &args["value"]["expectedRevision"],
                0,
                MAX_SAFE_INTEGER - 1,
                "expectedRevision",
            )?;
            let patch = if args["project"] == true {
                project_patch(&args["value"]["patch"])?
            } else {
                patch(&args["value"]["patch"])?
            };
            Ok(json!({"expectedRevision": revision, "patch": patch}))
        }
        _ => Err(ContractError::new(
            "Unknown settings contract.",
            "AGENT_CORE_UNKNOWN_OPERATION",
            400,
        )),
    }
}

#[cfg(test)]
mod lightweight_settings_tests {
    use super::*;
    #[test]
    fn retired_settings_cannot_reload_a_model_and_project_edits_preserve_global_rrf_k() {
        let legacy=merge_local(&defaults()["local"],&json!({"rerankProfileId":"retired-profile","rerankDevicePolicy":"gpu","rerankCandidates":60}));
        for field in ["rerankProfileId","rerankDevicePolicy","rerankCandidates"] {assert!(legacy.get(field).is_none());}
        assert_eq!(legacy["rrfK"],60);
        let local_patch=merge_local(&json!({}),&json!({"enabled":true}));
        assert!(local_patch.get("rrfK").is_none());
        assert_eq!(merge_local(&json!({"rrfK":35}),&local_patch)["rrfK"],35);
    }
}
