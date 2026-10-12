use super::{ContractError, ContractResult, MAX_SAFE_INTEGER, hash, safe_integer};
use serde_json::{Value, json};

fn conflict(message: &str, code: &'static str) -> ContractError {
    ContractError::new(message, code, 409)
}
fn corpus_source(value: &Value) -> bool {
    matches!(value.as_str(), Some("knowledge" | "work-file"))
}

/// The persistent identity ledger owns scope binding and revocation before disposable SQLite publication.
/// 持久身份账本在可重建 SQLite 发布之前拥有范围绑定与永久撤销状态。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let mut registry = arguments["registry"].clone();
    if registry["schemaVersion"] != 1
        || !safe_integer(&registry["revision"], 0, MAX_SAFE_INTEGER)
        || !registry["sources"].is_object()
    {
        return Err(conflict(
            "Unsupported source identity registry. / 资料身份登记版本无效，原文件已保留。",
            "CORRUPT_RETRIEVAL_IDENTITIES",
        ));
    }
    if operation == "storage.registry.validate" {
        return Ok(registry);
    }
    if operation == "storage.registry.retireChat" {
        let id = super::identity(&arguments["conversationId"])?;
        let scope = format!("chat:{}", id.as_str().unwrap().to_ascii_lowercase());
        let sources = registry["sources"].as_object_mut().unwrap();
        let before = sources.len();
        // Only derived identities owned by this chat retire; their creation origin is not shared-data ownership.
        // 仅移除归属此聊天的派生身份；创建来源不等于共享资料归属，撤销恢复仍允许按原日志重建。
        sources.retain(|_, source| source["scopeKey"] != scope);
        let removed = before - sources.len();
        if removed > 0 {
            registry["revision"] = json!(registry["revision"].as_u64().unwrap() + 1);
        }
        return Ok(json!({"registry":registry,"scopeKey":scope,"removed":removed}));
    }
    if operation == "storage.registry.publish" {
        for prepared in arguments["sources"].as_array().into_iter().flatten() {
            let source = &prepared["source"];
            let key = prepared["key"]
                .as_str()
                .filter(|_| hash(&prepared["key"]))
                .ok_or_else(|| {
                    conflict(
                        "Invalid source identity hash. / 资料身份哈希无效。",
                        "CORRUPT_RETRIEVAL_IDENTITIES",
                    )
                })?;
            let previous = &registry["sources"][key];
            if !previous.is_null() && previous["scopeKey"] != source["scopeKey"] {
                return Err(conflict(
                    "Source identity belongs to another scope. / 资料身份已属于其他范围。",
                    "RETRIEVAL_SCOPE_MISMATCH",
                ));
            }
            if previous["tombstone"] == true {
                return Err(conflict(
                    "Source identity was permanently revoked. / 此资料身份已永久撤销，不能由迟到任务恢复。",
                    "RETRIEVAL_SOURCE_REVOKED",
                ));
            }
            let current = &prepared["current"];
            if !current.is_null() {
                if current["scope_key"] != source["scopeKey"] {
                    return Err(conflict(
                        "Source identity belongs to another scope. / 资料身份已属于其他范围。",
                        "RETRIEVAL_SCOPE_MISMATCH",
                    ));
                }
                let revision_raw = current["source_revision"].as_str().unwrap_or("null");
                let revision: Value =
                    serde_json::from_str(&super::raw_json::scalar_projection(revision_raw))
                        .map_err(|_| {
                            conflict(
                                "Invalid persisted revision. / 已保存版本格式无效。",
                                "CORRUPT_RETRIEVAL_IDENTITIES",
                            )
                        })?;
                if source["sourceRevision"].is_number()
                    && revision.is_number()
                    && source["sourceRevision"].as_f64() < revision.as_f64()
                    || source["bindingRevision"].as_u64().unwrap_or(0)
                        < current["binding_revision"].as_u64().unwrap_or(0)
                {
                    return Err(conflict(
                        "An outdated source update was rejected. / 已拒绝过期资料更新。",
                        "STALE_RETRIEVAL_SOURCE",
                    ));
                }
                let same_revision = if revision.is_string() {
                    prepared["revisionRaw"].as_str() == Some(revision_raw)
                } else {
                    source["sourceRevision"] == revision
                };
                if same_revision && source["contentHash"] != current["content_hash"] {
                    return Err(conflict(
                        "Source changed without a new revision. / 资料内容已改变但版本未更新。",
                        "RETRIEVAL_SOURCE_CHANGED",
                    ));
                }
            }
            registry["sources"][key] = prepared["record"].clone();
        }
        registry["revision"] = json!(registry["revision"].as_u64().unwrap() + 1);
        return Ok(json!({"registry": registry}));
    }
    let scopes: Vec<&str> = arguments["scopes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if operation == "storage.registry.remove" {
        let current = &arguments["current"];
        let key = arguments["key"].as_str().unwrap_or("");
        let previous = &registry["sources"][key];
        if !current.is_null() && !scopes.contains(&current["scope_key"].as_str().unwrap_or(""))
            || current.is_null()
                && !previous.is_null()
                && !scopes.contains(&previous["scopeKey"].as_str().unwrap_or(""))
        {
            return Ok(json!({"allowed": false, "registry": registry}));
        }
        if current.is_null() && arguments["permanent"] == false {
            return Ok(json!({"allowed": true, "coverageOnly": true, "registry": registry}));
        }
        let scope = current["scope_key"]
            .as_str()
            .or_else(|| previous["scopeKey"].as_str())
            .or_else(|| scopes.first().copied())
            .unwrap_or("")
            .to_owned();
        let corpus_changed = corpus_source(
            current
                .get("source_type")
                .unwrap_or(&previous["sourceType"]),
        );
        let mut record = previous
            .as_object()
            .cloned()
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));
        record["sourceId"] = arguments["sourceId"].clone();
        record["scopeKey"] = json!(scope);
        record["active"] = json!(false);
        if arguments["permanent"] != false || previous["tombstone"] == true {
            record["tombstone"] = json!(true);
        }
        registry["sources"][key] = record;
        registry["revision"] = json!(registry["revision"].as_u64().unwrap() + 1);
        return Ok(
            json!({"allowed": true, "coverageOnly": false, "scopeKey": scope, "corpusChanged": corpus_changed, "registry": registry}),
        );
    }
    if operation == "storage.registry.invalidate" {
        let scope = &arguments["scopeKey"];
        let mut corpus_changed = arguments["hasCorpusRows"] == true;
        for source in registry["sources"].as_object_mut().unwrap().values_mut() {
            if source["scopeKey"] == *scope {
                corpus_changed |= source["active"] == true && corpus_source(&source["sourceType"]);
                source["active"] = json!(false);
            }
        }
        registry["revision"] = json!(registry["revision"].as_u64().unwrap() + 1);
        return Ok(json!({"registry": registry, "corpusChanged": corpus_changed}));
    }
    Err(conflict(
        "Unknown identity ledger operation. / 未知身份账本操作。",
        "INVALID_RETRIEVAL_INPUT",
    ))
}
