use std::collections::HashSet;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::protocol::{MAX_TURN_CALLS, array, string};

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
        || rounds.len() > 128
    {
        return Err(invalid());
    }
    let mut previous = 0;
    let mut total_calls = 0;
    for round in rounds {
        let number = round["round"]
            .as_u64()
            .filter(|number| *number > previous && *number <= 128)
            .ok_or_else(invalid)?;
        let calls = round["calls"].as_array().ok_or_else(invalid)?;
        if round["object"] != true || !text(&round["text"], 2 * 1024 * 1024) {
            return Err(invalid());
        }
        validate_calls(calls)?;
        previous = number;
        total_calls += calls.len();
        if total_calls > 512 {
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
        value["rounds"] = json!(
            (1..=17)
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
}

/// Validate exact UTF-16 metadata and return projection recipes; private continuation stays in its SDK archive adapter.
/// 校验精确 UTF-16 元信息并返回投影配方；私有续传原文留在 SDK 附件适配层，不改变旧哈希表示。
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
