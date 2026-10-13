use super::{ContractError, ContractResult, hash, utf16_length};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

const MAX_AGE_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;

fn failure(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_RETRIEVAL_INPUT", 400)
}
fn text(value: &Value, maximum: usize) -> bool {
    value.as_str().is_some_and(|value| {
        !value.is_empty() && utf16_length(value) <= maximum && !value.contains('\0')
    })
}
fn source_refs(value: &Value) -> bool {
    value.as_array().is_some_and(|values| {
        values.len() <= 64
            && values
                .iter()
                .all(|value| text(value, 16384) && value.as_str().unwrap().starts_with("rag1:"))
    })
}
fn navigation(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.len() <= 32 && values.iter().all(|value| text(value, 1000)))
}

/// Experience remains expiring, scope-bound navigation; no saved conclusion grants permission or correctness.
/// 经验只是过期和范围绑定的导航线索，保存结论不能授予权限或认证正确性。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    if operation == "storage.experiences.validate" {
        let input = &arguments["input"];
        if !text(&input["query"], 2000)
            || !source_refs(&input["sourceRefs"])
            || !text(&input["conclusionId"], 128)
            || !text(&input["state"], 128)
            || !navigation(&input["navigation"])
            || !input["complete"].is_boolean()
        {
            return Err(failure("Invalid experience receipt. / 检索经验回执无效。"));
        }
        return Ok(Value::Null);
    }
    if operation == "storage.experiences.read" {
        let document = &arguments["document"];
        if document["version"] != 1
            || document["scopeKey"] != arguments["scopeKey"]
            || !document["entries"].is_array()
        {
            return Err(failure("Invalid experience version. / 经验版本无效。"));
        }
        let now = arguments["nowMs"]
            .as_f64()
            .ok_or_else(|| failure("Invalid experience clock. / 经验时钟无效。"))?;
        let records = document["entries"].as_array().unwrap();
        let omitted = records.len().saturating_sub(64);
        let mut invalid = 0;
        let mut expired = 0;
        let mut entries = Vec::new();
        for (index, entry) in records.iter().enumerate().skip(omitted) {
            let created_at = arguments["dateReceipts"][index]["createdAtMs"].as_f64();
            let expires_at = arguments["dateReceipts"][index]["expiresAtMs"]
                .as_f64()
                .or_else(|| {
                    if entry.get("expiresAt").is_none() {
                        created_at.map(|created| created + MAX_AGE_MS)
                    } else {
                        None
                    }
                });
            if !hash(&entry["id"])
                || !text(&entry["query"], 2000)
                || !text(&entry["conclusionId"], 128)
                || !text(&entry["state"], 128)
                || !source_refs(&entry["sourceRefs"])
                || !navigation(entry.get("navigation").unwrap_or(&json!([])))
                || created_at.is_none()
                || expires_at.is_none()
                || expires_at <= created_at
                || created_at.is_some_and(|created| created > now + 60000.0)
            {
                invalid += 1;
                continue;
            }
            let expires_at = expires_at.unwrap();
            if expires_at <= now {
                expired += 1;
                continue;
            }
            let partial = entry["complete"] != true
                || entry["sourceRefs"].as_array().unwrap().is_empty()
                || entry["state"] != "ready-to-answer";
            entries.push(json!({"index": index, "expiresAtMs": expires_at, "recordState": if partial { "partial" } else { "navigation-only" }}));
        }
        return Ok(
            json!({"entries": entries, "diagnostics": {"invalid": invalid, "expired": expired, "omitted": omitted}}),
        );
    }
    if operation == "storage.experiences.selectSave" {
        let id = &arguments["id"];
        let entries = arguments["entries"]
            .as_array()
            .ok_or_else(|| failure("Invalid experience entries. / 经验记录无效。"))?;
        let mut selected: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry["id"] != *id)
            .map(|(index, _)| index)
            .collect();
        if selected.len() > 63 {
            selected.drain(..selected.len() - 63);
        }
        let entry_bytes = arguments["entryBytes"].as_u64().unwrap_or(u64::MAX);
        if entry_bytes > 1024 * 1024 {
            return Err(failure(
                "Experience receipt is too large. / 检索经验回执过大。",
            ));
        }
        // The codec supplies exact compact JSON byte counts; array commas and brackets belong to the service budget.
        // 编码边界提供精确紧凑 JSON 字节数，数组逗号和括号由服务预算计算。
        let mut size = entry_bytes
            + 2
            + selected
                .iter()
                .map(|index| {
                    arguments["entrySizes"][*index]
                        .as_u64()
                        .unwrap_or(u64::MAX)
                        .saturating_add(1)
                })
                .sum::<u64>();
        while size > 1024 * 1024 && !selected.is_empty() {
            let index = selected.remove(0);
            size = size.saturating_sub(arguments["entrySizes"][index].as_u64().unwrap_or(0) + 1);
        }
        let include_new = size <= 1024 * 1024;
        return Ok(json!({"indices": selected, "includeNew": include_new}));
    }
    if operation == "storage.experiences.find" {
        let entries = arguments["entries"]
            .as_array()
            .ok_or_else(|| failure("Invalid experience entries. / 经验记录无效。"))?;
        let terms: Vec<&str> = arguments["terms"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut order = Vec::new();
        let mut selected = HashMap::new();
        let mut seen = HashSet::new();
        for (index, entry) in entries.iter().enumerate() {
            let lower_query = arguments["lowerQueries"][index].as_str().unwrap_or("");
            if !terms.is_empty() && !terms.iter().any(|term| lower_query.contains(term)) {
                continue;
            }
            let id = entry["id"].as_str().unwrap_or("");
            if seen.insert(id) {
                order.push(id);
            }
            selected.insert(id, index);
        }
        let limit = if arguments["isNaNLimit"] == true {
            order.len()
        } else if arguments["isPositiveInfinityLimit"] == true {
            20
        } else if arguments["isNegativeInfinityLimit"] == true {
            1
        } else {
            arguments["limit"]
                .as_f64()
                .unwrap_or(6.0)
                .clamp(1.0, 20.0)
                .floor() as usize
        };
        Ok(json!(
            order
                .into_iter()
                .rev()
                .take(limit)
                .map(|id| selected[id])
                .collect::<Vec<_>>()
        ))
    } else {
        Err(failure("Unknown experience operation. / 未知经验操作。"))
    }
}
