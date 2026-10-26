use std::collections::HashSet;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::protocol::{MAX_TURN_CALLS, array, string};

pub(crate) const MAX_TRANSCRIPT_ROUNDS: usize = 512;
pub(crate) const MAX_TRANSCRIPT_CALLS: usize = 4096;

fn units(value: &Value) -> Vec<u64> {
    array(&value["units"])
        .iter()
        .filter_map(Value::as_u64)
        .collect()
}
fn text(value: &Value, maximum: u64) -> bool {
    value["string"] == true
        && value["length"]
            .as_u64()
            .is_some_and(|length| length <= maximum)
}
fn identity(value: &Value) -> bool {
    text(value, 256)
        && value["length"].as_u64().is_some_and(|length| length > 0)
        && !units(value).iter().any(|unit| [0, 10, 13].contains(unit))
}
fn object(value: &Value) -> bool {
    value["object"] == true
        && value["jsonLength"]
            .as_u64()
            .is_some_and(|length| length <= 65536)
}
fn hex(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// Runtime snapshots are bounded broker data, never replayed calls or another source of tool authority.
/// 运行快照只承载有界程序资料，不能夹带待执行调用或变成另一套工具授权。
pub(crate) fn valid_context_before(value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    let Some(entries) = value.as_array().filter(|entries| entries.len() <= 64) else {
        return false;
    };
    let mut characters = 0usize;
    for entry in entries {
        let Some(fields) = entry.as_object() else {
            return false;
        };
        if fields.keys().any(|name| !["message", "source"].contains(&name.as_str())) {
            return false;
        }
        let Some(message) = entry["message"].as_object() else {
            return false;
        };
        if message.keys().any(|name| !["role", "content"].contains(&name.as_str()))
            || !matches!(entry["message"]["role"].as_str(), Some("user" | "system" | "developer"))
        {
            return false;
        }
        let Some(content) = entry["message"]["content"].as_str().filter(|text| !text.trim().is_empty()) else {
            return false;
        };
        let Some(source) = entry["source"].as_object() else {
            return false;
        };
        if source.keys().any(|name| !["projectionOrigin", "runtimeContext", "snapshotHash", "authorityHash",
            "publicText", "instructionAuthority", "runtimeKind"].contains(&name.as_str()))
            || entry["source"]["projectionOrigin"] != "broker-control"
            || entry["source"]["runtimeContext"] != true
            || !hex(&entry["source"]["snapshotHash"])
            || !hex(&entry["source"]["authorityHash"])
            || entry["source"]["publicText"] != content
            || entry["source"]["instructionAuthority"] != false
            || entry["source"]["runtimeKind"] != "runtime-snapshot"
        {
            return false;
        }
        characters = characters.saturating_add(entry.to_string().encode_utf16().count());
        if characters > 1024 * 1024 {
            return false;
        }
    }
    true
}
fn invalid() -> String {
    "INVALID_MODEL_TRANSCRIPT".into()
}
/// Live decoding and durable rounds share the same bounded call contract before dispatch.
/// 实时解码与持久轮次在派发前共用同一个有界调用合同，不能执行后才发现无法保存。
pub(crate) fn validate_calls(calls: &[Value]) -> Result<(), String> {
    if calls.len() > MAX_TURN_CALLS {
        return Err(invalid());
    }
    let mut unique = HashSet::new();
    for call in calls {
        if call["object"] != true
            || !identity(&call["id"])
            || call["id"]["length"].as_u64().unwrap_or(0) > 200
            || !identity(&call["name"])
            || !object(&call["arguments"])
            || !unique.insert(units(&call["id"]))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn validate(args: &Value) -> Result<Value, String> {
    if args["object"] != true || args["version"] != 1 {
        return Err("UNSUPPORTED_MODEL_TRANSCRIPT_VERSION".into());
    }
    let origin = &args["origin"];
    let rounds = args["rounds"].as_array().ok_or_else(invalid)?;
    if origin["object"] != true
        || !identity(&origin["providerId"])
        || !identity(&origin["model"])
        || ![
            "openai-completions",
            "openai-responses",
            "anthropic-messages",
            "ollama",
        ]
        .contains(&string(&origin["protocol"]))
        || !hex(&origin["connectionFingerprint"])
        || rounds.len() > MAX_TRANSCRIPT_ROUNDS
    {
        return Err(invalid());
    }
    let mut previous = 0;
    let mut total_calls = 0;
    for round in rounds {
        let number = round["round"]
            .as_u64()
            .filter(|number| *number > previous && *number <= MAX_TRANSCRIPT_ROUNDS as u64)
            .ok_or_else(invalid)?;
        let calls = round["calls"].as_array().ok_or_else(invalid)?;
        if round["object"] != true || !text(&round["text"], 2 * 1024 * 1024)
            || !valid_context_before(&round["contextBefore"])
        {
            return Err(invalid());
        }
        validate_calls(calls)?;
        previous = number;
        total_calls += calls.len();
        if total_calls > MAX_TRANSCRIPT_CALLS {
            return Err(invalid());
        }
        if round["hasReference"] == true {
            let reference = &round["reference"];
            if reference["object"] != true
                || !regex::Regex::new(
                    "(?i)^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$",
                )
                .unwrap()
                .is_match(string(&reference["id"]))
                || !hex(&reference["sha256"])
                || reference["bytes"]
                    .as_u64()
                    .is_none_or(|bytes| bytes > 9_007_199_254_740_991)
                || !hex(&round["prefixFingerprint"])
            {
                return Err(invalid());
            }
        }
    }
    Ok(json!(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_shape(text: &str) -> Value {
        json!({"string":true,"length":text.encode_utf16().count(),"units":text.encode_utf16().collect::<Vec<_>>()})
    }

    fn transcript(call_count: usize) -> Value {
        let calls: Vec<_> = (0..call_count)
            .map(|index| json!({"object":true,"id":text_shape(&format!("call-{index}")),"name":text_shape("filesystem.read"),"arguments":{"object":true,"jsonLength":2}}))
            .collect();
        json!({"object":true,"version":1,"origin":{"object":true,"providerId":text_shape("fixture"),"model":text_shape("fixture-model"),"protocol":"openai-completions","connectionFingerprint":"0".repeat(64)},"rounds":[{"object":true,"round":1,"text":text_shape("reading"),"calls":calls}]})
    }

    #[test]
    fn durable_round_accepts_the_decoder_limit_without_relaxing_transcript_total() {
        let value = transcript(MAX_TURN_CALLS);
        assert_eq!(
            calculate("models.transcript.validate", &value).unwrap(),
            true
        );
        let appended = calculate("models.transcript.append", &json!({"previousOriginJson":"fixture","originJson":"fixture","rounds":[],"round":value["rounds"][0],"validation":value})).unwrap();
        assert_eq!(appended, json!([0]));
        assert_eq!(
            calculate(
                "models.transcript.validate",
                &transcript(MAX_TURN_CALLS + 1)
            )
            .unwrap_err(),
            "INVALID_MODEL_TRANSCRIPT"
        );
        let mut value = value;
        let round = value["rounds"][0].clone();
        // 129 full rounds exceed the 4096-call transcript total while staying under the 512-round limit.
        // 129 个满调用轮次超过 4096 个调用的总上限，但仍低于 512 轮上限。
        value["rounds"] = json!(
            (1..=129)
                .map(|number| {
                    let mut round = round.clone();
                    round["round"] = json!(number);
                    round
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(
            calculate("models.transcript.validate", &value).unwrap_err(),
            "INVALID_MODEL_TRANSCRIPT"
        );
    }

    #[test]
    fn shared_call_validation_keeps_identity_uniqueness_and_argument_limits() {
        let value = transcript(MAX_TURN_CALLS);
        for mutation in ["duplicate", "identity", "arguments"] {
            let mut value = value.clone();
            match mutation {
                "duplicate" => {
                    value["rounds"][0]["calls"][1]["id"] =
                        value["rounds"][0]["calls"][0]["id"].clone()
                }
                "identity" => value["rounds"][0]["calls"][0]["id"] = text_shape("bad\nidentity"),
                _ => value["rounds"][0]["calls"][0]["arguments"]["jsonLength"] = json!(65537),
            }
            assert_eq!(
                calculate("models.transcript.validate", &value).unwrap_err(),
                "INVALID_MODEL_TRANSCRIPT"
            );
        }
    }

    #[test]
    fn runtime_context_preserves_legacy_rounds_and_rejects_calls_or_forged_authority() {
        let snapshot = json!({"message":{"role":"user","content":"Current runtime data"},
            "source":{"projectionOrigin":"broker-control","runtimeContext":true,"snapshotHash":"a".repeat(64),
                "authorityHash":"b".repeat(64),"publicText":"Current runtime data","instructionAuthority":false,
                "runtimeKind":"runtime-snapshot"}});
        assert!(valid_context_before(&Value::Null));
        assert!(valid_context_before(&json!([])));
        for role in ["user", "system", "developer"] {
            let mut accepted = snapshot.clone();
            accepted["message"]["role"] = json!(role);
            let mut round = transcript(0);
            round["rounds"][0]["contextBefore"] = json!([accepted]);
            assert_eq!(calculate("models.transcript.validate", &round).unwrap(), true);
        }
        let mut invalid_entries = Vec::new();
        for role in ["tool", "function", "assistant"] {
            let mut invalid = snapshot.clone();
            invalid["message"]["role"] = json!(role);
            invalid_entries.push(invalid);
        }
        for (path, value) in [
            (["message", "tool_calls"], json!([])),
            (["source", "runtimeContext"], json!(false)),
            (["source", "instructionAuthority"], json!(true)),
            (["source", "executed"], json!(true)),
            (["source", "snapshotHash"], json!("invalid")),
            (["source", "authorityHash"], json!("B".repeat(64))),
            (["source", "publicText"], json!("Another text")),
        ] {
            let mut invalid = snapshot.clone();
            invalid[path[0]][path[1]] = value;
            invalid_entries.push(invalid);
        }
        for invalid in invalid_entries {
            let mut round = transcript(0);
            round["rounds"][0]["contextBefore"] = json!([invalid]);
            assert_eq!(calculate("models.transcript.validate", &round).unwrap_err(), "INVALID_MODEL_TRANSCRIPT");
        }
        assert!(!valid_context_before(&json!(vec![snapshot.clone(); 65])));
        let mut oversized = snapshot;
        oversized["message"]["content"] = json!("x".repeat(1024 * 1024));
        oversized["source"]["publicText"] = oversized["message"]["content"].clone();
        assert!(!valid_context_before(&json!([oversized])));
    }
}

/// Validate UTF-16 metadata and plan projections; keep private continuation in its archive adapter and preserve hashes.
/// 校验 UTF-16 元信息并规划投影；私有续传留在归档适配层，保留原哈希表示。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "models.transcript.hash" => Ok(json!(format!(
            "{:x}",
            Sha256::digest(string(&args["wire"]).as_bytes())
        ))),
        "models.transcript.validate" => validate(args),
        "models.transcript.append" => {
            if args["previousOriginJson"] != args["originJson"] {
                return Err("MODEL_TRANSCRIPT_ORIGIN_CHANGED".into());
            }
            let mut indexes: Vec<usize> = array(&args["rounds"])
                .iter()
                .enumerate()
                .filter(|(_, round)| round["round"] != args["round"]["round"])
                .map(|(index, _)| index)
                .collect();
            indexes.push(array(&args["rounds"]).len());
            let mut rounds = array(&args["rounds"]).to_vec();
            rounds.push(args["round"].clone());
            indexes.sort_by(|left, right| {
                rounds[*left]["round"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&rounds[*right]["round"].as_f64().unwrap_or(0.0))
            });
            let mut validation = args["validation"].clone();
            validation["rounds"] = json!(
                indexes
                    .iter()
                    .map(|index| rounds[*index].clone())
                    .collect::<Vec<_>>()
            );
            validate(&validation)?;
            Ok(json!(indexes))
        }
        "models.transcript.continuation" => {
            let items = array(&args["items"]);
            let protocol = string(&args["protocol"]);
            if protocol == "anthropic-messages" {
                if !items.iter().any(|block| {
                    block["type"] == "thinking" && text(&block["signature"], 2 * 1024 * 1024)
                        || block["type"] == "redacted_thinking"
                }) {
                    return Ok(Value::Null);
                }
                let recipes: Vec<Value> = items.iter().enumerate().filter_map(|(index, block)| {
                    let fields = match string(&block["type"]) {
                        "text" if text(&block["text"], 2 * 1024 * 1024) => vec!["type", "text"],
                        "thinking" if text(&block["thinking"], 2 * 1024 * 1024) && text(&block["signature"], 2 * 1024 * 1024) => vec!["type", "thinking", "signature"],
                        "redacted_thinking" if text(&block["data"], 2 * 1024 * 1024) => vec!["type", "data"],
                        "tool_use" if identity(&block["id"]) && identity(&block["name"]) && object(&block["input"]) => vec!["type", "id", "name", "input"],
                        _ => return None,
                    }; Some(json!({"index": index, "fields": fields, "cloneInput": block["type"] == "tool_use"}))
                }).collect();
                Ok(json!({"style": "anthropic", "items": recipes}))
            } else if protocol == "openai-responses" {
                if !items.iter().any(|item| {
                    item["type"] == "reasoning" && text(&item["encrypted_content"], 2 * 1024 * 1024)
                }) {
                    return Ok(Value::Null);
                }
                let recipes: Vec<Value> = items.iter().enumerate().filter_map(|(index, item)| {
                    match string(&item["type"]) {
                        "reasoning" if text(&item["encrypted_content"], 2 * 1024 * 1024) => Some(json!({"index": index, "type": "reasoning", "fields": if identity(&item["id"]) { vec!["type", "id", "encrypted_content"] } else { vec!["type", "encrypted_content"] }})),
                        "function_call" if identity(&item["call_id"]) && identity(&item["name"]) && text(&item["arguments"], 65536) => Some(json!({"index": index, "type": "function_call", "fields": ["type", "call_id", "name", "arguments"]})),
                        "message" => { let blocks: Vec<usize> = array(&item["content"]).iter().enumerate().filter(|(_, block)| block["type"] == "output_text" && text(&block["text"], 2 * 1024 * 1024)).map(|(index, _)| index).collect();
                            if blocks.is_empty() { None } else { Some(json!({"index": index, "type": "message", "blocks": blocks})) } },
                        _ => None,
                    }
                }).collect();
                Ok(json!({"style": "responses", "items": recipes}))
            } else {
                let message = &args["message"];
                if array(&args["calls"]).is_empty()
                    || !text(&message["reasoning_content"], 2 * 1024 * 1024)
                    || message["reasoning_content"]["length"] == 0
                {
                    return Ok(Value::Null);
                }
                let native = array(&message["tool_calls"]);
                let mut calls = Vec::new();
                for call in array(&args["calls"]) {
                    let index = native
                        .iter()
                        .position(|item| units(&item["id"]) == units(&call["id"]))
                        .ok_or_else(invalid)?;
                    let item = &native[index];
                    if !identity(&item["name"]) || !text(&item["arguments"], 65536) {
                        return Err(invalid());
                    }
                    calls.push(index);
                }
                Ok(
                    json!({"style": "completions", "calls": calls, "content": text(&message["content"], 2 * 1024 * 1024)}),
                )
            }
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
