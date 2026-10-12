use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

/// Context rules operate on bounded metadata; provider bodies, archives and history remain in Node.
/// 上下文规则处理受限元信息；供应商正文、归档和正式历史仍由 Node 持有。
pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "context.outputDefaults" | "context.outputValidate" | "context.outputBudget" => {
            super::output::calculate(operation, args)
        }
        "context.completeTurns" => Ok(complete_turns(args)),
        "context.observationReadTool" => Ok(json!(read_tool(args["name"].as_str().unwrap_or("")))),
        "context.observationDescribe" => Ok(describe_observation(args)),
        "context.observationPlan" => Ok(observation_plan(args)),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    }
}

fn complete_turns(args: &Value) -> Value {
    let Some(messages) = args["messages"].as_array() else {
        return json!([]);
    };
    let before_id = args["beforeUserId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_lowercase);
    let mut users: HashMap<String, usize> = HashMap::new();
    let mut consumed = HashSet::new();
    let mut adjacent_user = None;
    let mut turns = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if before_id.as_ref().is_some_and(|before| {
            message["id"]
                .as_str()
                .is_some_and(|id| id.to_lowercase() == *before)
        }) {
            break;
        }
        let key = message["key"].as_str().unwrap_or("");
        if message["role"] == "user" {
            users.insert(key.to_owned(), index);
            adjacent_user = Some(index);
        } else if message["role"] == "assistant"
            && message["status"] == "completed"
            && message["hasContent"] == true
        {
            let user = match message["replyKey"].as_str() {
                Some(reply) => users.get(reply).copied(),
                None => adjacent_user,
            };
            let Some(user) = user else {
                continue;
            };
            let user_key = messages[user]["key"].as_str().unwrap_or("");
            if messages[user]["validUserContent"] != true || consumed.contains(user_key) {
                continue;
            }
            let completion = super::history::task_completion(&message["taskCompletion"]);
            turns.push(if completion.is_null() {
                json!([user, index])
            } else {
                json!([user, index, {"taskCompletion": completion,
                    "notice": super::history::task_completion_notice(&message["id"], &completion)}])
            });
            consumed.insert(user_key.to_owned());
            if adjacent_user == Some(user) {
                adjacent_user = None;
            }
        }
    }
    json!(turns)
}

fn read_tool(name: &str) -> bool {
    ["filesystem.read", "filesystem.stat", "knowledge.read"].contains(&name)
}

fn digest(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn safe_offset(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|value| *value <= 9_007_199_254_740_991)
}

fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.is_empty())
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}

fn describe_observation(args: &Value) -> Value {
    let name = args["name"].as_str().unwrap_or("");
    let value = &args["metadata"];
    if !read_tool(name)
        || args["status"] != "completed"
        || !nonempty(&args["scopeKey"])
        || args["canonicalObject"] != true
        || args["valueObject"] != true
        || args["canonicalError"] == true
        || truthy(&value["storageError"])
        || truthy(&value["contextCompacted"])
        || truthy(&value["resultUnavailable"])
        || value["truncated"] == true && value["previewIsString"] == true
    {
        return Value::Null;
    }
    let (target, version, target_json) = if name.starts_with("filesystem.") {
        if args["pathIsAbsolute"] != true
            || !digest(&value["sha256"])
            || !args["normalizedPath"].is_string()
        {
            return Value::Null;
        }
        let target = json!({"path": args["normalizedPath"]});
        let serialized = target.to_string();
        (target, json!({"sha256": value["sha256"]}), serialized)
    } else {
        if !nonempty(&value["sourceId"])
            || !nonempty(&value["scopeKey"])
            || !digest(&value["contentHash"])
            || !(nonempty(&value["sourceRevision"])
                || safe_offset(&value["sourceRevision"]).is_some())
        {
            return Value::Null;
        }
        let target = json!({"sourceId": value["sourceId"], "scopeKey": value["scopeKey"]});
        // Preserve the legacy JSON field order in the comparison key, without copying source text.
        // 比较键保持既有 JSON 字段顺序，同时不复制来源正文。
        let serialized = format!(
            "{{\"sourceId\":{},\"scopeKey\":{}}}",
            value["sourceId"], value["scopeKey"]
        );
        (
            target,
            json!({"contentHash": value["contentHash"], "sourceRevision": value["sourceRevision"]}),
            serialized,
        )
    };
    let mut page = "stat".to_owned();
    if name != "filesystem.stat" {
        let (Some(offset), Some(next), Some(length)) = (
            safe_offset(&value["offset"]),
            safe_offset(&value["nextOffset"]),
            safe_offset(&value["textLength"]),
        ) else {
            return Value::Null;
        };
        if value["textIsString"] != true
            || offset.checked_add(length) != Some(next)
            || !value["hasMore"].is_boolean()
        {
            return Value::Null;
        }
        page = if value["hasMore"] == false
            && offset == 0
            && safe_offset(&value["totalCharacters"]) == Some(next)
        {
            "whole".to_owned()
        } else {
            json!([offset, next]).to_string()
        };
    }
    let target_key = format!(
        "[{},{},{},{}]",
        args["scopeKey"],
        json!(name),
        target_json,
        json!(page)
    );
    json!({"target": target, "version": version, "page": page,
        "archiveVerified": args["archiveVerified"], "targetKey": target_key,
        "ignoredFields": if name == "knowledge.read" { json!(["sourceRef", "modelSourceRef", "canonicalSourceRef", "evidenceDecision"]) } else { json!([]) }})
}

fn observation_plan(args: &Value) -> Value {
    let Some(observations) = args["observations"].as_array() else {
        return json!([]);
    };
    let mut latest = HashMap::new();
    for (index, source) in observations.iter().enumerate() {
        if source["archiveVerified"] == true && source["hasResultRef"] == true {
            latest.insert(source["targetKey"].to_string(), index);
        }
    }
    let mut plans = Vec::new();
    for (index, source) in observations.iter().enumerate() {
        if source["archiveVerified"] != true || source["hasResultRef"] != true {
            continue;
        }
        let Some(replacement) = latest
            .get(&source["targetKey"].to_string())
            .copied()
            .filter(|replacement| *replacement != index)
        else {
            continue;
        };
        if source
            .get("objectIdentity")
            .is_some_and(|identity| *identity == observations[replacement]["objectIdentity"])
        {
            continue;
        }
        let duplicate =
            source["contentFingerprint"] == observations[replacement]["contentFingerprint"];
        let changed = source["versionJson"] != observations[replacement]["versionJson"];
        if duplicate || changed {
            plans.push(
                json!({"sourceIndex": index, "replacementIndex": replacement,
            "reason": if duplicate {"duplicate-observation"} else {"superseded-version"}}),
            );
        }
    }
    json!(plans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_window_and_provider_caps_keep_the_exact_input_boundary() {
        let exact = apply("context.outputBudget", &json!({"contextWindowTokens": 2048, "requestedOutputTokens": 1024, "requiredInputTokens": 1536})).unwrap();
        assert_eq!(exact["maxOutputTokens"], 256);
        assert_eq!(exact["inputBudgetTokens"], 1536);
        assert_eq!(
            apply(
                "context.outputBudget",
                &json!({"contextWindowTokens": 2048, "requiredInputTokens": 1537})
            )
            .unwrap_err(),
            "CONTEXT_INPUT_TOO_LARGE"
        );
        let cap = apply(
            "context.outputBudget",
            &json!({"contextWindowTokens": 1_000_000, "providerMaxOutputTokens": 128000}),
        )
        .unwrap();
        assert_eq!(cap["maxOutputTokens"], 128000);
        assert_eq!(cap["outputBudgetReductionReason"], "provider_limit");
        assert!(
            apply(
                "context.outputBudget",
                &json!({"contextWindowTokens": 1000000, "providerMaxInputTokens": null})
            )
            .is_err()
        );
    }

    #[test]
    fn reply_links_survive_retries_without_promoting_failed_or_duplicate_turns() {
        let pairs = complete_turns(&json!({"messages": [
            {"id": "u1", "key": "u1", "role": "user", "validUserContent": true},
            {"id": "fail", "key": "fail", "role": "assistant", "status": "interrupted", "hasContent": true, "replyKey": "u1"},
            {"id": "u2", "key": "u2", "role": "user", "validUserContent": true},
            {"id": "retry", "key": "retry", "role": "assistant", "status": "completed", "hasContent": true, "replyKey": "u1"},
            {"id": "a2", "key": "a2", "role": "assistant", "status": "completed", "hasContent": true, "replyKey": "u2"},
            {"id": "duplicate", "key": "duplicate", "role": "assistant", "status": "completed", "hasContent": true, "replyKey": "u2"}]}));
        assert_eq!(pairs, json!([[0, 3], [2, 4]]));
    }

    #[test]
    fn unverified_or_partial_reads_do_not_replace_a_complete_source() {
        let base = json!({"name": "knowledge.read", "status": "completed", "scopeKey": "current-chat", "archiveVerified": true,
            "canonicalObject": true, "valueObject": true, "metadata": {"sourceId": "source", "scopeKey": "global",
                "contentHash": "a".repeat(64), "sourceRevision": 1, "textIsString": true, "textLength": 19,
                "offset": 0, "nextOffset": 19, "hasMore": false, "totalCharacters": 200}});
        let partial = describe_observation(&base);
        assert_eq!(partial["page"], "[0,19]");
        assert_eq!(
            partial["targetKey"],
            "[\"current-chat\",\"knowledge.read\",{\"sourceId\":\"source\",\"scopeKey\":\"global\"},\"[0,19]\"]"
        );
        let mut whole = base.clone();
        whole["metadata"]["totalCharacters"] = json!(19);
        assert_eq!(describe_observation(&whole)["page"], "whole");
        let plan = observation_plan(&json!({"observations": [
            {"archiveVerified": true, "hasResultRef": true, "targetKey": "first", "versionJson": "v1", "contentFingerprint": "body1"},
            {"archiveVerified": false, "hasResultRef": true, "targetKey": "first", "versionJson": "v2", "contentFingerprint": "body2"}]}));
        assert_eq!(plan, json!([]));
    }
}
