use super::{ContractError, ContractResult, memory, raw_json, utf16_length};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_MEMORY", 400)
}
fn expose(entry: &Value, id: &Value) -> bool {
    if entry["source"].get("conversationId").is_none() {
        entry["scope"] == "chat" && entry["scopeId"] == *id
    } else {
        entry["source"]["conversationId"] == *id
    }
}
fn same_scope(entry: &Value, source: &Value) -> bool {
    match entry["scope"].as_str() {
        Some("project") => {
            source["projectId"].as_str().is_some_and(|id| {
                !id.is_empty() && id.to_ascii_lowercase() == entry["scopeId"].as_str().unwrap_or("")
            }) && source["isFolderlessWorkspace"] != true
        }
        Some("chat") => {
            source["conversationId"]
                .as_str()
                .map(str::to_ascii_lowercase)
                == entry["scopeId"].as_str().map(str::to_owned)
        }
        _ => true,
    }
}
fn quotes_available(arguments: &Value) -> bool {
    let users = &arguments["usersRaw"];
    let entry = &arguments["entry"];
    let message_id = entry["source"]["messageId"].as_str().unwrap_or("");
    users.get(message_id).is_some()
        && arguments["quotesRaw"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|quote| {
                users.get(quote["messageId"].as_str().unwrap_or("")) == Some(&quote["textRaw"])
            })
}
fn contains(content: &[u16], query: &[u16]) -> bool {
    query.is_empty()
        || content
            .windows(query.len())
            .any(|candidate| candidate == query)
}
fn units(value: &Value) -> Result<Vec<u16>, ContractError> {
    raw_json::utf16_identity(value.as_str().unwrap_or(""))
        .ok_or_else(|| invalid("记忆字符串编码无效。"))
}

/// Memory source authority, private provenance, lexical selection and projected freshness share one policy.
/// 记忆来源权威、私人出处、词法选择与投影新鲜度共用同一策略，确认和权限不能由相似度替代。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    match operation {
        "storage.memoryService.queryInput" => {
            let input = &arguments["input"];
            if !input.as_object().is_some_and(|input| {
                input
                    .keys()
                    .all(|key| ["scopes", "query", "limit", "offset"].contains(&key.as_str()))
            }) {
                return Err(invalid("模型记忆查询无效。"));
            }
            if let Some(scopes) = input.get("scopes") {
                let scopes = scopes
                    .as_array()
                    .filter(|scopes| !scopes.is_empty() && scopes.len() <= 3)
                    .ok_or_else(|| invalid("请选择有效且不重复的记忆范围。"))?;
                let mut seen = HashSet::new();
                for scope in scopes {
                    memory::apply("storage.memory.scope", &json!({"value": scope}))?;
                    if !seen.insert(scope.as_str().unwrap()) {
                        return Err(invalid("请选择有效且不重复的记忆范围。"));
                    }
                }
            }
            let limit = input
                .get("limit")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or(json!(20));
            if arguments["limitNumberValid"] == false || !super::safe_integer(&limit, 1, 50) {
                return Err(invalid("模型记忆查询最多返回 50 条。"));
            }
            let offset = input.get("offset").cloned().unwrap_or(json!(0));
            if arguments["offsetNumberValid"] == false || !super::safe_integer(&offset, 0, 3000) {
                return Err(invalid("模型记忆查询 offset 须为 0–3000 的整数。"));
            }
            let query = input.get("query").cloned().unwrap_or(json!(""));
            if !query
                .as_str()
                .is_some_and(|query| utf16_length(query) <= 4000 && !query.contains('\0'))
            {
                return Err(invalid("记忆查询须为最多 4000 个字符的字符串。"));
            }
            Ok(json!({"limit": limit, "offset": offset, "scopes": input["scopes"]}))
        }
        "storage.memoryService.visible" => Ok(json!(
            arguments["entries"]
                .as_array()
                .ok_or_else(|| invalid("记忆条目列表无效。"))?
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry["status"] == "confirmed"
                    || expose(entry, &arguments["conversationId"]))
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        )),
        "storage.memoryService.sourceStatus" => {
            let entry = &arguments["entry"];
            let source = &arguments["source"];
            if entry["source"]["type"] == "manual" {
                return Ok(
                    json!({"active": entry["status"] == "confirmed", "sourceAvailable": true, "sourceArchived": false}),
                );
            }
            if source.is_null() {
                return Ok(
                    json!({"active": false, "sourceAvailable": false, "sourceArchived": false}),
                );
            }
            let available = quotes_available(arguments);
            Ok(
                json!({"active": entry["status"] == "confirmed" && available && same_scope(entry, source), "sourceAvailable": available, "sourceArchived": source["isArchived"] == true}),
            )
        }
        "storage.memoryService.validateSource" => {
            if arguments["entry"]["source"]["type"] == "manual" {
                return Ok(Value::Null);
            }
            if !same_scope(&arguments["entry"], &arguments["source"])
                || !quotes_available(arguments)
            {
                return Err(ContractError::new(
                    "候选原话已变化、删除或移出此范围，不能保存或确认。",
                    "MEMORY_SOURCE_UNAVAILABLE",
                    409,
                ));
            }
            Ok(Value::Null)
        }
        "storage.memoryService.scoreBatch" => {
            let query = units(&arguments["queryRaw"])?;
            let terms: Vec<Vec<u16>> = arguments["termsRaw"]
                .as_array()
                .into_iter()
                .flatten()
                .map(units)
                .collect::<Result<_, _>>()?;
            let records = arguments["records"]
                .as_array()
                .filter(|records| records.len() <= 32)
                .ok_or_else(|| invalid("记忆查询批次无效。"))?;
            let scores = records
                .iter()
                .map(|record| {
                    let content = units(record)?;
                    let score = if query.is_empty() {
                        1.0
                    } else if contains(&content, &query) {
                        2.0
                    } else {
                        terms.iter().filter(|term| contains(&content, term)).count() as f64
                            / terms.len().max(1) as f64
                    };
                    Ok(score)
                })
                .collect::<Result<Vec<_>, ContractError>>()?;
            Ok(json!(scores))
        }
        "storage.memoryService.query" => {
            let records = arguments["records"]
                .as_array()
                .ok_or_else(|| invalid("记忆条目列表无效。"))?;
            let scores = arguments["scores"]
                .as_array()
                .filter(|scores| scores.len() == records.len())
                .ok_or_else(|| invalid("记忆查询评分回执无效。"))?;
            let mut ranked = Vec::new();
            for (index, record) in records.iter().enumerate() {
                let score = scores[index]
                    .as_f64()
                    .filter(|score| score.is_finite() && (0.0..=2.0).contains(score))
                    .ok_or_else(|| invalid("记忆查询评分回执无效。"))?;
                if score > 0.0 {
                    ranked.push((index, score, record["localeRank"].as_u64().unwrap_or(0)));
                }
            }
            ranked.sort_by(|left, right| {
                right
                    .1
                    .total_cmp(&left.1)
                    .then_with(|| left.2.cmp(&right.2))
                    .then_with(|| left.0.cmp(&right.0))
            });
            let offset = arguments["offset"].as_u64().unwrap_or(0) as usize;
            let limit = arguments["limit"].as_u64().unwrap_or(20) as usize;
            let selected = ranked.iter().skip(offset).take(limit).map(|(index, _, _)| json!({"index": index, "expose": expose(&records[*index]["entry"], &arguments["conversationId"])})).collect::<Vec<_>>();
            let has_more = offset + selected.len() < ranked.len();
            Ok(
                json!({"selected": selected, "hasMore": has_more, "nextOffset": if has_more { Some(offset + selected.len()) } else { None }, "matched": ranked.len(), "truncated": ranked.len() > selected.len()}),
            )
        }
        "storage.memoryService.snapshot" => {
            let snapshot = &arguments["snapshot"];
            let current = &arguments["current"];
            let wrong_conversation = arguments["conversationId"] != snapshot["conversationId"];
            let scope_changed = current["projectId"] != snapshot["projectId"]
                || current["isFolderlessWorkspace"] != snapshot["isFolderlessWorkspace"];
            let entries: HashMap<&str, &Value> = arguments["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| Some((entry["id"].as_str()?, entry)))
                .collect();
            let mut invalid_sources = Vec::new();
            for record in snapshot["entries"].as_array().into_iter().flatten() {
                let entry = entries.get(record["memoryId"].as_str().unwrap_or(""));
                if !wrong_conversation
                    && (record["scope"] != "project" || !scope_changed)
                    && entry.is_some_and(|entry| entry["identity"] == record["identity"])
                {
                    continue;
                }
                let code = if wrong_conversation || scope_changed && record["scope"] == "project" {
                    "MEMORY_SCOPE_CHANGED"
                } else if entry.is_some() {
                    "MEMORY_SOURCE_CHANGED"
                } else {
                    "MEMORY_SOURCE_UNAVAILABLE"
                };
                invalid_sources.push(json!({"memoryId": record["memoryId"], "sourceType": "memory", "scope": record["scope"], "code": code, "next": "read-current-confirmed-memory-before-relying-on-this-record"}));
            }
            Ok(
                json!({"current": invalid_sources.is_empty(), "checked": snapshot["entries"].as_array().map_or(0, Vec::len), "invalidSources": invalid_sources, "freshnessOnly": true, "correctnessCertified": false}),
            )
        }
        _ => Err(invalid("未知记忆服务操作。")),
    }
}
