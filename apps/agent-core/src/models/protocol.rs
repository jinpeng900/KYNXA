use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub const MAX_TURN_CALLS: usize = 32;
pub const MAX_ARGUMENT_CHARACTERS: usize = 65_536;
pub const MAX_TURN_ARGUMENT_CHARACTERS: usize = 1_048_576;

pub fn characters(text: &str) -> usize {
    text.encode_utf16().count()
}

pub fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

pub fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

pub fn trim(text: &str) -> &str {
    text.trim_matches(|character: char| {
        character.is_whitespace() && character != '\u{0085}' || character == '\u{feff}'
    })
}

pub fn text_parts(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    array(value)
        .iter()
        .filter(|part| part["type"] == "text")
        .map(|part| string(&part["text"]))
        .collect()
}

pub use crate::text_metrics::estimate_tokens;

fn serialized(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn value_tokens(value: &Value, field: &str) -> u64 {
    // Encoded image bytes are transport data, not text tokens; their model-specific cost stays unobserved.
    // 图片编码字节属于传输数据，不是文本 token；其模型相关消耗仍须标记为未观测。
    if matches!(
        value["type"].as_str(),
        Some("image" | "image_url" | "input_image")
    ) {
        return 8
            + estimate_tokens(string(&value["type"]))
            + estimate_tokens(string(&value["detail"]));
    }
    if field == "images" && value.is_array() {
        return 8 * array(value).len() as u64;
    }
    match value {
        Value::String(text) => estimate_tokens(text),
        Value::Null => 1,
        Value::Bool(_) | Value::Number(_) => estimate_tokens(&serialized(value)),
        _ if matches!(field, "input" | "arguments") => estimate_tokens(&serialized(value)),
        Value::Array(items) => {
            2 + items
                .iter()
                .map(|item| 1 + value_tokens(item, ""))
                .sum::<u64>()
        }
        Value::Object(fields) => {
            2 + fields
                .iter()
                .map(|(key, item)| estimate_tokens(key) + 4 + value_tokens(item, key))
                .sum::<u64>()
        }
    }
}

pub fn image_inputs(value: &Value) -> u64 {
    if matches!(
        value["type"].as_str(),
        Some("image" | "image_url" | "input_image")
    ) {
        return 1;
    }
    match value {
        Value::Array(values) => values.iter().map(image_inputs).sum(),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                if key == "images" {
                    array(value).len() as u64
                } else {
                    image_inputs(value)
                }
            })
            .sum(),
        _ => 0,
    }
}

pub fn tool_message_tokens(messages: &[Value], system: &str) -> u64 {
    let initial = if system.is_empty() {
        0
    } else {
        8 + estimate_tokens(system)
    };
    initial
        + messages
            .iter()
            .map(|message| {
                let Some(fields) = message.as_object() else {
                    return 8 + value_tokens(message, "");
                };
                8 + fields
                    .get("content")
                    .map(|content| value_tokens(content, "content"))
                    .unwrap_or(0)
                    + fields
                        .iter()
                        .filter(|(key, _)| !matches!(key.as_str(), "role" | "content"))
                        .map(|(key, value)| estimate_tokens(key) + 4 + value_tokens(value, key))
                        .sum::<u64>()
            })
            .sum::<u64>()
}

pub fn error(message: &str, code: Option<&str>, kind: &str) -> Value {
    let mut result = json!({"message":message, "type":kind});
    if let Some(code) = code {
        result["code"] = json!(code);
    }
    json!({"error":result})
}

pub fn decoder_error(message: &str, code: &str) -> Value {
    error(message, Some(code), "error")
}

pub fn output_limit(reason: &str) -> bool {
    matches!(
        reason,
        "length" | "max_tokens" | "max_output_tokens" | "model_context_window_exceeded"
    )
}

pub fn check_finish(reason: &Value) -> Option<Value> {
    let reason = string(reason);
    if reason.is_empty() || matches!(reason, "stop" | "end_turn" | "stop_sequence") {
        return None;
    }
    Some(error(
        if output_limit(reason) {
            "回复达到模型输出上限，已保留生成的内容。"
        } else if matches!(reason, "tool_calls" | "tool_use") {
            "模型请求使用工具，当前聊天尚未执行该工具，已保留生成的内容。"
        } else {
            "模型未完整生成回复，已保留生成的内容。"
        },
        None,
        "interrupted",
    ))
}

pub fn validate_count(count: usize) -> Option<Value> {
    (count > MAX_TURN_CALLS)
        .then(|| decoder_error("模型单次请求的工具数量超过上限。", "MODEL_TOOL_CALL_LIMIT"))
}

pub fn validate_buffer(text: &str, aggregate: usize) -> Option<Value> {
    (characters(text) > MAX_ARGUMENT_CHARACTERS || aggregate > MAX_TURN_ARGUMENT_CHARACTERS)
        .then(|| decoder_error("工具参数超过大小限制。", "MODEL_TOOL_ARGUMENT_LIMIT"))
}

pub fn final_parts(protocol: &str, result: &Value) -> Value {
    if result.get("error").is_some_and(truthy) {
        return error(
            "模型服务返回错误，请检查连接配置或稍后重试。",
            None,
            "error",
        );
    }
    if protocol == "anthropic-messages" {
        let join = |kind: &str, field: &str| {
            array(&result["content"])
                .iter()
                .filter(|part| part["type"] == kind)
                .map(|part| string(&part[field]))
                .collect::<Vec<_>>()
                .join("\n")
        };
        return json!({"content":join("text", "text"), "reasoning":join("thinking", "thinking"), "finish":result["stop_reason"]});
    }
    if protocol == "openai-responses" {
        let output = result.get("output");
        let texts = |kind: &str, field: &str, part_kind: &str| {
            array(&result["output"])
                .iter()
                .filter(|item| item["type"] == kind)
                .flat_map(|item| array(&item[field]))
                .filter(|part| part["type"] == part_kind)
                .map(|part| string(&part["text"]))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let status = string(&result["status"]);
        let finish = if status == "incomplete" {
            if truthy(&result["incomplete_details"]["reason"]) {
                result["incomplete_details"]["reason"].clone()
            } else {
                json!("incomplete")
            }
        } else if !status.is_empty() && status != "completed" {
            json!(status)
        } else {
            Value::Null
        };
        return json!({"content":if output.is_some() { texts("message", "content", "output_text") } else { string(&result["output_text"]).to_owned() },
            "reasoning":texts("reasoning", "summary", "summary_text"), "finish":finish});
    }
    let choice = array(&result["choices"]).first().unwrap_or(&Value::Null);
    json!({"content":text_parts(&choice["message"]["content"]),
        "reasoning":text_parts(choice["message"].get("reasoning_content").filter(|value| !value.is_null()).unwrap_or(&choice["message"]["reasoning"])),
        "finish":choice["finish_reason"]})
}

pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}

pub fn wire_name(name: &str) -> String {
    let alias: String = name
        .encode_utf16()
        .map(|unit| {
            let character = char::from_u32(unit as u32).unwrap_or('_');
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    format!(
        "k_{alias}_{}",
        &format!("{:x}", Sha256::digest(name.as_bytes()))[..8]
    )
}

fn decode_turn(args: &Value) -> Value {
    let protocol = string(&args["protocol"]);
    let result = &args["result"];
    let mut parts = final_parts(protocol, result);
    if parts.get("error").is_some() {
        return parts;
    }
    let raw_calls: Vec<Value> = if protocol == "anthropic-messages" {
        parts["content"] = json!(
            array(&result["content"])
                .iter()
                .filter(|item| item["type"] == "text")
                .map(|item| string(&item["text"]))
                .collect::<String>()
        );
        parts["reasoning"] = json!(
            array(&result["content"])
                .iter()
                .filter(|item| item["type"] == "thinking")
                .map(|item| string(&item["thinking"]))
                .collect::<String>()
        );
        array(&result["content"])
            .iter()
            .filter(|item| item["type"] == "tool_use")
            .map(|item| json!({"id":item["id"],"name":item["name"],"arguments":item["input"]}))
            .collect()
    } else if protocol == "openai-responses" {
        array(&result["output"]).iter().filter(|item| item["type"] == "function_call").map(|item| json!({"id":item["call_id"],"name":item["name"],"arguments":item["arguments"]})).collect()
    } else {
        array(&result["choices"][0]["message"]["tool_calls"]).iter().map(|item| json!({"id":item["id"],"name":item["function"]["name"],"arguments":item["function"]["arguments"]})).collect()
    };
    let truncated = args["allowTruncatedText"] == true
        && raw_calls.is_empty()
        && output_limit(string(&parts["finish"]));
    let fail = |mut failure: Value| {
        failure["error"]["estimatedGeneratedTokens"] = json!(
            estimate_tokens(string(&parts["content"]))
                + estimate_tokens(string(&parts["reasoning"]))
                + estimate_tokens(&serialized(&json!(raw_calls)))
        );
        failure
    };
    if !truncated && !matches!(string(&parts["finish"]), "tool_calls" | "tool_use") {
        if let Some(mut failure) = check_finish(&parts["finish"]) {
            if !raw_calls.is_empty() {
                failure["error"]["code"] = json!("MODEL_TOOL_OUTPUT_TRUNCATED");
            }
            return fail(failure);
        }
    }
    if let Some(failure) = validate_count(raw_calls.len()) {
        return fail(failure);
    }
    let mut aggregate = 0;
    for call in &raw_calls {
        let text = call["arguments"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| serialized(&call["arguments"]));
        aggregate += characters(&text);
        if let Some(failure) = validate_buffer(&text, aggregate) {
            return fail(failure);
        }
    }
    let mut decoded = Vec::new();
    let mut ids = HashSet::new();
    for (index, call) in raw_calls.iter().enumerate() {
        let (id, name) = (string(&call["id"]), string(&call["name"]));
        if id.is_empty()
            || name.is_empty()
            || characters(id) > 200
            || characters(name) > 256
            || id.contains(['\0', '\r', '\n'])
            || name.contains(['\0', '\r', '\n'])
        {
            return fail(decoder_error(
                "模型返回了无效的工具调用身份。",
                "MODEL_TOOL_IDENTITY_INVALID",
            ));
        }
        let arguments = match call["arguments"].as_str() {
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(value) => value,
                Err(_) => {
                    return fail(decoder_error(
                        "模型返回了不完整的工具参数。",
                        "MODEL_TOOL_ARGUMENT_INVALID",
                    ));
                }
            },
            None => call["arguments"].clone(),
        };
        if !arguments.is_object() || characters(&serialized(&arguments)) > MAX_ARGUMENT_CHARACTERS {
            return fail(decoder_error(
                "工具参数必须为大小受限的 JSON 对象。",
                "MODEL_TOOL_ARGUMENT_INVALID",
            ));
        }
        if !ids.insert(id) {
            return fail(decoder_error(
                "模型重复了工具调用 ID。",
                "MODEL_TOOL_IDENTITY_INVALID",
            ));
        }
        let descriptor = array(&args["catalog"])
            .iter()
            .find(|tool| tool["wireName"] == name);
        let mut value = json!({"id":id,"name":descriptor.map(|tool| &tool["name"]).unwrap_or(&call["name"]),"rawIndex":index});
        if descriptor.is_none() {
            value["unavailable"] = json!(true);
        }
        decoded.push(value);
    }
    if !truncated
        && matches!(string(&parts["finish"]), "tool_calls" | "tool_use")
        && decoded.is_empty()
    {
        return fail(decoder_error(
            "模型结束了工具调用，但未返回完整参数。",
            "MODEL_TOOL_ARGUMENT_INVALID",
        ));
    }
    if !truncated
        && protocol == "openai-responses"
        && truthy(&result["status"])
        && result["status"] != "completed"
    {
        return fail(error("模型未完整结束本次工具回复。", None, "interrupted"));
    }
    let refusal = if protocol == "openai-completions" {
        truthy(&result["choices"][0]["message"]["refusal"])
    } else {
        array(
            result
                .get("output")
                .filter(|value| !value.is_null())
                .unwrap_or(&result["content"]),
        )
        .iter()
        .any(|item| {
            item["type"] == "refusal"
                || array(&item["content"])
                    .iter()
                    .any(|part| part["type"] == "refusal")
        })
    };
    if decoded.is_empty()
        && trim(string(&parts["content"])).is_empty()
        && !refusal
        && !(truncated && !trim(string(&parts["reasoning"])).is_empty())
    {
        return fail(decoder_error(
            "模型返回空回复，当前步骤未执行工具。",
            "MODEL_RESPONSE_EMPTY",
        ));
    }
    parts["calls"] = json!(decoded);
    if truncated {
        parts["outputTruncated"] = json!(true);
    }
    parts
}

pub fn complete_pairs(messages: &[Value]) -> bool {
    let (mut calls, mut results) = (Vec::new(), Vec::new());
    for message in messages {
        if message
            .get("tool_calls")
            .is_some_and(|value| !value.is_null() && !value.is_array())
        {
            return false;
        }
        calls.extend(
            array(&message["tool_calls"])
                .iter()
                .map(|call| call["id"].clone()),
        );
        if message["type"] == "function_call" {
            calls.push(message["call_id"].clone());
        }
        if message["type"] == "function_call_output" {
            results.push(message["call_id"].clone());
        }
        if message["role"] == "tool" {
            results.push(message["tool_call_id"].clone());
        }
        for block in array(&message["content"]) {
            if block["type"] == "tool_use" {
                calls.push(block["id"].clone());
            }
            if block["type"] == "tool_result" {
                results.push(block["tool_use_id"].clone());
            }
        }
    }
    let identifiers: HashSet<_> = calls
        .iter()
        .filter_map(Value::as_str)
        .filter(|id| !id.is_empty())
        .collect();
    identifiers.len() == calls.len()
        && results.len() == calls.len()
        && results
            .iter()
            .filter_map(Value::as_str)
            .collect::<HashSet<_>>()
            .len()
            == results.len()
        && results
            .iter()
            .all(|id| id.as_str().is_some_and(|id| identifiers.contains(id)))
}

struct SchemaProjection {
    visited: usize,
    omitted: usize,
    structural: bool,
}

impl SchemaProjection {
    fn project(&mut self, schema: &Value, depth: usize) -> Result<Value, String> {
        let Some(fields) = schema.as_object() else {
            return Ok(schema.clone());
        };
        self.visited += 1;
        if depth > 64 || self.visited > 32_768 {
            return Err("Local tool declaration schema exceeds its projection bound.".to_owned());
        }
        let mut projected = Map::new();
        for (key, value) in fields {
            let constraint = if self.structural {
                matches!(
                    key.as_str(),
                    "minLength"
                        | "maxLength"
                        | "minItems"
                        | "maxItems"
                        | "pattern"
                        | "format"
                        | "minimum"
                        | "maximum"
                        | "exclusiveMinimum"
                        | "exclusiveMaximum"
                        | "multipleOf"
                        | "minContains"
                        | "maxContains"
                        | "minProperties"
                        | "maxProperties"
                        | "uniqueItems"
                )
            } else {
                matches!(key.as_str(), "maxLength" | "maxItems")
                    || matches!(key.as_str(), "minLength" | "minItems")
                        && value.as_f64().is_some_and(|number| number > 32.0)
            };
            if constraint {
                self.omitted += 1;
                continue;
            }
            let child = if matches!(
                key.as_str(),
                "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
            ) && value.is_object()
            {
                let mut children = Map::new();
                for (name, value) in value.as_object().unwrap() {
                    children.insert(name.clone(), self.project(value, depth + 1)?);
                }
                Value::Object(children)
            } else if matches!(
                key.as_str(),
                "allOf" | "anyOf" | "oneOf" | "prefixItems" | "items"
            ) && value.is_array()
            {
                Value::Array(
                    array(value)
                        .iter()
                        .map(|child| self.project(child, depth + 1))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            } else if matches!(
                key.as_str(),
                "items"
                    | "additionalItems"
                    | "additionalProperties"
                    | "contains"
                    | "propertyNames"
                    | "not"
                    | "if"
                    | "then"
                    | "else"
                    | "unevaluatedProperties"
                    | "unevaluatedItems"
                    | "contentSchema"
            ) {
                self.project(value, depth + 1)?
            } else {
                value.clone()
            };
            projected.insert(key.clone(), child);
        }
        Ok(Value::Object(projected))
    }
}

fn project_schemas(args: &Value) -> Value {
    let mut projection = SchemaProjection {
        visited: 0,
        omitted: 0,
        structural: args["mode"] == "structural",
    };
    let schemas = match array(&args["schemas"])
        .iter()
        .map(|schema| projection.project(schema, 0))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(schemas) => schemas,
        Err(message) => return json!({"error":{"message":message}}),
    };
    json!({"schemas":schemas,"omittedConstraintCount":projection.omitted})
}

fn grammar_rejection(args: &Value) -> Value {
    if args["status"] != 400 {
        return Value::Null;
    }
    let body = &args["body"];
    let message = body["error"]
        .as_str()
        .or_else(|| body["error"]["message"].as_str())
        .or_else(|| body["message"].as_str())
        .unwrap_or("");
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| Regex::new(r"(?i)(?-u:\b)(?:failed to (?:parse|initialize)[^\n]{0,120}grammar|error parsing grammar|number of (?:rules|repetitions)[^\n]{0,240}exceeds sane defaults)(?-u:\b)").unwrap());
    if characters(message) > 65_536 || !pattern.is_match(message) {
        return Value::Null;
    }
    json!({"kind":"tool-grammar","code":"MODEL_LOCAL_TOOL_GRAMMAR_REJECTED"})
}

pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    Ok(match operation {
        "models.tokens.text" => json!(estimate_tokens(string(&args["text"]))),
        "models.tokens.messages" => json!(
            array(&args["messages"])
                .iter()
                .map(|item| 8 + estimate_tokens(string(&item["content"])))
                .sum::<u64>()
                + if string(&args["system"]).is_empty() {
                    0
                } else {
                    8 + estimate_tokens(string(&args["system"]))
                }
        ),
        "models.tokens.toolMessages" => json!(tool_message_tokens(
            array(&args["messages"]),
            string(&args["system"])
        )),
        "models.protocol.wireNames" => json!(
            array(&args["names"])
                .iter()
                .map(|name| wire_name(string(name)))
                .collect::<Vec<_>>()
        ),
        "models.protocol.textParts" => json!(text_parts(&args["value"])),
        "models.protocol.finalParts" => final_parts(string(&args["protocol"]), &args["result"]),
        "models.protocol.checkFinish" => check_finish(&args["reason"]).unwrap_or(Value::Null),
        "models.protocol.decodeTurn" => decode_turn(args),
        "models.protocol.completePairs" => json!(complete_pairs(array(&args["messages"]))),
        "models.protocol.projectLocalSchemas" => project_schemas(args),
        "models.protocol.grammarRejection" => grammar_rejection(args),
        "models.protocol.publicMessages" => json!(
            array(&args["messages"])
                .iter()
                .filter_map(|message| {
                    let kind = string(&message["type"]);
                    if kind == "reasoning" {
                        return None;
                    }
                    let mut projected = Map::new();
                    if matches!(kind, "function_call" | "function_call_output") {
                        for field in if kind == "function_call" {
                            &["type", "call_id", "name", "arguments"][..]
                        } else {
                            &["type", "call_id", "output"][..]
                        } {
                            if let Some(value) = message.get(*field) {
                                projected.insert((*field).to_owned(), value.clone());
                            }
                        }
                    } else {
                        for field in ["role", "content", "tool_calls", "tool_call_id"] {
                            if let Some(value) = message.get(field) {
                                projected.insert(field.to_owned(), value.clone());
                            }
                        }
                        if message["content"].is_array() {
                            projected.insert(
                                "content".to_owned(),
                                json!(
                                    array(&message["content"])
                                        .iter()
                                        .filter(|block| matches!(
                                            string(&block["type"]),
                                            "text" | "tool_use" | "tool_result"
                                        ))
                                        .cloned()
                                        .collect::<Vec<_>>()
                                ),
                            );
                        }
                    }
                    Some(Value::Object(projected))
                })
                .collect::<Vec<_>>()
        ),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    })
}

pub(crate) fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

/// UTF-16 offsets are the public archive contract; byte offsets never escape this module.
/// 公开归档坐标始终使用 UTF-16；Rust 字节偏移不传给调用者。
pub(crate) fn slice(text: &str, start: usize, end: usize) -> String {
    String::from_utf16_lossy(
        &text
            .encode_utf16()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn output_excerpt(text: &str, maximum: usize) -> String {
    let length = characters(text);
    if maximum == 0 {
        return String::new();
    }
    if length <= maximum {
        return text.to_owned();
    }
    let marker = "\n… [middle omitted] …\n";
    let marker_length = characters(marker);
    if maximum <= marker_length {
        return slice(marker, 0, maximum);
    }
    let remaining = maximum - marker_length;
    let mut head = remaining.div_ceil(2);
    let mut tail = length - remaining / 2;
    let units: Vec<_> = text.encode_utf16().collect();
    if head > 0 && (0xd800..=0xdbff).contains(&units[head - 1]) {
        head -= 1;
    }
    if tail < length && (0xdc00..=0xdfff).contains(&units[tail]) {
        tail += 1;
    }
    format!(
        "{}{marker}{}",
        slice(text, 0, head),
        slice(text, tail, length)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javascript_whitespace_and_astral_accounting_remain_distinct() {
        assert_eq!(estimate_tokens("\u{0085}"), 2);
        assert_eq!(estimate_tokens("\u{feff}"), 1);
        assert_eq!(estimate_tokens("😀"), 4);
        assert_eq!(trim("\u{0085}"), "\u{0085}");
        assert_eq!(trim("\u{feff}"), "");
    }

    #[test]
    fn local_schema_projection_never_mutates_authoritative_or_literal_values() {
        let schema = json!({"type":"object","properties":{"__proto__":{"type":"string","maxLength":5000}},"default":{"maxLength":5000},"required":["__proto__"]});
        let original = schema.clone();
        let projected = project_schemas(&json!({"schemas":[schema],"mode":"bounded"}));
        assert_eq!(
            projected["schemas"][0]["properties"]["__proto__"]["maxLength"],
            Value::Null
        );
        assert_eq!(projected["schemas"][0]["default"], original["default"]);
        assert_eq!(original["properties"]["__proto__"]["maxLength"], 5000);
    }
}
