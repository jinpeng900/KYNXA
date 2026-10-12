use super::{ContractError, ContractResult, memory};
use serde_json::{Value, json};
use std::collections::HashSet;

fn failure(message: &str, code: &'static str, status: u16) -> ContractError {
    ContractError::new(message, code, status)
}

fn entry_index(document: &Value, id: &Value) -> Result<usize, ContractError> {
    document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entry| entry["id"] == *id)
        .ok_or_else(|| failure("记忆不存在。", "MEMORY_NOT_FOUND", 404))
}

fn target_index(
    document: &Value,
    target: &Value,
    identities: &Value,
) -> Result<usize, ContractError> {
    if target["scope"] != document["scope"] || target["scopeId"] != document["scopeId"] {
        return Err(failure(
            "目标不能跨越记忆作用域。",
            "MEMORY_SCOPE_CHANGED",
            409,
        ));
    }
    let index = entry_index(document, &target["id"]).map_err(|_| {
        failure(
            "目标记忆的内容、来源或版本已变化，请重新读取。",
            "MEMORY_TARGET_CHANGED",
            409,
        )
    })?;
    let entry = &document["entries"][index];
    if entry["status"] != "confirmed"
        || entry["revision"] != target["entryRevision"]
        || identities[entry["id"].as_str().unwrap()] != target["identity"]
    {
        return Err(failure(
            "目标记忆的内容、来源或版本已变化，请重新读取。",
            "MEMORY_TARGET_CHANGED",
            409,
        ));
    }
    Ok(index)
}

fn quotes(entry: &Value) -> Vec<Value> {
    entry["candidate"]["quotes"]
        .as_array()
        .or_else(|| entry["proposal"]["quotes"].as_array())
        .cloned()
        .unwrap_or_default()
}

fn dismissal(document: &mut Value, entry: &Value, now: &Value) {
    if entry["source"]["type"] != "user-message" {
        return;
    }
    let mut message_ids = vec![entry["source"]["messageId"].clone()];
    message_ids.extend(quotes(entry).iter().map(|quote| quote["messageId"].clone()));
    let mut seen = HashSet::new();
    for message_id in message_ids {
        if !seen.insert(message_id.to_string()) {
            continue;
        }
        let dismissed = document["dismissedSources"].as_array_mut().unwrap();
        if dismissed.iter().any(|source| {
            source["conversationId"] == entry["source"]["conversationId"]
                && source["messageId"] == message_id
                && source["candidateFingerprint"] == entry["candidate"]["fingerprint"]
                && source["proposalFingerprint"] == entry["proposal"]["fingerprint"]
        }) {
            continue;
        }
        let mut source = json!({"conversationId": entry["source"]["conversationId"], "messageId": message_id, "deletedAt": now});
        if entry.get("candidate").is_some() {
            source["candidateFingerprint"] = entry["candidate"]["fingerprint"].clone();
        }
        if entry.get("proposal").is_some() {
            source["proposalFingerprint"] = entry["proposal"]["fingerprint"].clone();
        }
        if let Some(value) = entry["candidate"]
            .get("batchThroughMessageId")
            .filter(|value| !value.is_null())
        {
            source["candidateBatchThroughMessageId"] = value.clone();
        }
        dismissed.push(source);
    }
}

fn capacity(document: &Value) -> Result<(), ContractError> {
    if document["entries"].as_array().unwrap().len() >= 1000 {
        return Err(failure(
            "此作用域的记忆已达上限，请先整理或删除。",
            "INVALID_MEMORY",
            400,
        ));
    }
    Ok(())
}

/// The repository owns deduplication, dismissals, proposal targets and confirmation transitions.
/// 仓储服务拥有去重、撤销来源、提案目标与确认状态迁移；引用验证回调仍由受保护调用端提供。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let mut document = arguments["document"].clone();
    let input = &arguments["input"];
    let now = &arguments["now"];
    let mut changed = true;
    let mut reason = "created";
    let mut selected_id = arguments["id"].clone();
    let mut origins = json!({});
    match operation {
        "storage.repository.updatePreflight" => {
            let index = entry_index(&document, &arguments["id"])?;
            let entry = &document["entries"][index];
            if input["status"] == "draft" && entry["status"] == "confirmed" {
                return Err(failure(
                    "已确认记忆不能退回自动草稿。",
                    "INVALID_MEMORY",
                    400,
                ));
            }
            let requires_source_validation =
                input["status"] == "confirmed" && entry["status"] == "draft";
            if requires_source_validation {
                memory::apply(
                    "storage.memory.confirmation",
                    &json!({"entry": entry, "proposalAction": input["proposalAction"]}),
                )?;
            }
            return Ok(
                json!({"entry": entry, "requiresSourceValidation": requires_source_validation}),
            );
        }
        "storage.repository.create"
        | "storage.repository.candidate"
        | "storage.repository.proposal" => {
            let scope = &document["scope"];
            let source = &input["source"];
            if operation != "storage.repository.create" && document["scopeId"] != input["scopeId"] {
                return Err(failure(
                    "候选或建议来源范围已改变。",
                    "MEMORY_SCOPE_CHANGED",
                    409,
                ));
            }
            let is_candidate = operation == "storage.repository.candidate";
            let is_proposal = operation == "storage.repository.proposal";
            let descriptor = if is_candidate {
                &input["candidate"]
            } else {
                &input["proposal"]
            };
            let message_ids: HashSet<&str> = descriptor["quotes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|quote| quote["messageId"].as_str())
                .collect();
            let dismissed = document["dismissedSources"].as_array().unwrap();
            if is_candidate {
                changed = !dismissed.iter().any(|item| {
                    item["candidateFingerprint"] == descriptor["fingerprint"]
                        || item["conversationId"] == source["conversationId"]
                            && message_ids.contains(item["messageId"].as_str().unwrap_or(""))
                }) && !document["entries"].as_array().unwrap().iter().any(|entry| {
                    arguments["candidateFingerprints"][entry["id"].as_str().unwrap()]
                        == descriptor["fingerprint"]
                        || entry["source"]["type"] == "user-message"
                            && entry["source"]["conversationId"] == source["conversationId"]
                            && (message_ids
                                .contains(entry["source"]["messageId"].as_str().unwrap_or(""))
                                || quotes(entry).iter().any(|quote| {
                                    message_ids.contains(quote["messageId"].as_str().unwrap_or(""))
                                }))
                });
            } else if is_proposal {
                if dismissed.iter().any(|item| {
                    item["proposalFingerprint"] == descriptor["fingerprint"]
                        || item["conversationId"] == source["conversationId"]
                            && message_ids.contains(item["messageId"].as_str().unwrap_or(""))
                }) {
                    changed = false;
                    reason = "dismissed";
                } else if let Some(entry) = document["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["proposal"]["fingerprint"] == descriptor["fingerprint"])
                {
                    changed = false;
                    reason = "deduplicated";
                    selected_id = entry["id"].clone();
                }
                if changed {
                    if let Some(target) = descriptor.get("target") {
                        if document["revision"] != target["expectedRevision"] {
                            return Err(failure(
                                "目标范围版本已变化，请重新读取。",
                                "MEMORY_CONFLICT",
                                409,
                            ));
                        }
                        target_index(&document, target, &arguments["entryIdentities"])?;
                    }
                }
            } else if source["type"] == "user-message" {
                changed = !dismissed.iter().any(|item| {
                    item["conversationId"] == source["conversationId"]
                        && item["messageId"] == source["messageId"]
                }) && !document["entries"].as_array().unwrap().iter().any(|entry| {
                    entry["source"]["type"] == "user-message"
                        && entry["source"]["conversationId"] == source["conversationId"]
                        && entry["source"]["messageId"] == source["messageId"]
                });
            }
            if changed {
                capacity(&document)?;
                let mut entry = json!({"id": arguments["id"], "scope": scope, "scopeId": document["scopeId"], "content": input["content"], "kind": input["kind"], "status": if is_candidate || is_proposal { "draft" } else { "confirmed" }, "source": source, "revision": 1, "createdAt": now, "updatedAt": now});
                if is_candidate {
                    entry["candidate"] = descriptor.clone();
                }
                if is_proposal {
                    let mut proposal = descriptor.clone();
                    let mut conflicts: Vec<Value> = document["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|entry| {
                            entry["status"] == "confirmed"
                                && entry["id"] != descriptor["target"]["id"]
                                && arguments["candidateFingerprints"][entry["id"].as_str().unwrap()]
                                    == arguments["contentFingerprint"]
                        })
                        .map(|entry| {
                            json!(format!("exact-content:{}", entry["id"].as_str().unwrap()))
                        })
                        .collect();
                    conflicts.extend(descriptor["conflicts"].as_array().unwrap().iter().cloned());
                    let mut seen = HashSet::new();
                    conflicts.retain(|value| seen.insert(value.to_string()));
                    conflicts.truncate(8);
                    proposal["conflicts"] = json!(conflicts);
                    entry["proposal"] = proposal;
                }
                origins[entry["id"].as_str().unwrap()] = json!("$input");
                document["entries"].as_array_mut().unwrap().push(entry);
            }
        }
        "storage.repository.update" => {
            let index = entry_index(&document, &arguments["id"])?;
            let entry = document["entries"][index].clone();
            if input["status"] == "confirmed"
                && entry["status"] == "draft"
                && entry["proposal"].get("target").is_some()
            {
                let target_index = target_index(
                    &document,
                    &entry["proposal"]["target"],
                    &arguments["entryIdentities"],
                )?;
                let target = document["entries"][target_index].clone();
                dismissal(&mut document, &entry, now);
                dismissal(&mut document, &target, now);
                if entry["proposal"]["action"] == "update" {
                    let mut updated = target.clone();
                    updated["content"] = input.get("content").unwrap_or(&entry["content"]).clone();
                    updated["kind"] = input.get("kind").unwrap_or(&entry["kind"]).clone();
                    updated["source"] = entry["source"].clone();
                    updated.as_object_mut().unwrap().remove("candidate");
                    updated["proposal"] = entry["proposal"].clone();
                    updated["proposal"]["appliedAt"] = now.clone();
                    updated["revision"] = json!(target["revision"].as_u64().unwrap() + 1);
                    updated["updatedAt"] = now.clone();
                    origins[target["id"].as_str().unwrap()] = entry["id"].clone();
                    document["entries"][target_index] = updated;
                } else {
                    document["entries"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|item| item["id"] != target["id"]);
                }
                document["entries"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|item| item["id"] != entry["id"]);
            } else {
                if input.get("content").is_none()
                    && input.get("kind").is_none()
                    && input["status"] == entry["status"]
                {
                    changed = false;
                }
                if changed {
                    let entry = &mut document["entries"][index];
                    for field in ["content", "kind", "status"] {
                        if let Some(value) = input.get(field) {
                            entry[field] = value.clone();
                        }
                    }
                    if entry.get("proposal").is_some() && entry["status"] == "draft" {
                        entry["proposal"]["fingerprint"] =
                            arguments["updatedProposalFingerprint"].clone();
                    }
                    entry["revision"] = json!(entry["revision"].as_u64().unwrap() + 1);
                    entry["updatedAt"] = now.clone();
                }
            }
        }
        "storage.repository.delete" => {
            let index = entry_index(&document, &arguments["id"])?;
            let entry = document["entries"][index].clone();
            dismissal(&mut document, &entry, now);
            document["entries"].as_array_mut().unwrap().remove(index);
        }
        _ => return Err(failure("未知记忆仓储操作。", "INVALID_MEMORY", 400)),
    }
    Ok(
        json!({"document": document, "changed": changed, "reason": reason, "entryId": selected_id, "origins": origins}),
    )
}
