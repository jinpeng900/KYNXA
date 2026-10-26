use serde_json::{Value, json};

use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string};

struct HistoryMessage {
    metadata: Value,
    units: Vec<u16>,
}

/// Check ownership before and after I/O; native history handles filtering, matching and Unicode pagination.
/// I/O 前后核验归属；原生历史服务负责过滤、匹配和 Unicode 分页。
#[derive(Default)]
pub struct HistoryState {
    messages: Vec<HistoryMessage>,
}

fn same(left: &Value, right: &Value) -> bool {
    left.is_null() && right.is_null()
        || left
            .as_str()
            .zip(right.as_str())
            .is_some_and(|(left, right)| left.to_lowercase() == right.to_lowercase())
}
fn invalid(message: &str) -> ServiceError {
    ServiceError::new("INVALID_TOOL_ARGUMENTS", message, 400)
}
fn bounded(value: &Value, fallback: u64, maximum: u64, minimum: u64) -> ServiceResult<usize> {
    if value.is_null() {
        return Ok(fallback as usize);
    }
    value
        .as_u64()
        .filter(|value| {
            (*value >= minimum) && (*value <= maximum) && (*value <= 9_007_199_254_740_991)
        })
        .map(|value| value as usize)
        .ok_or_else(|| invalid(&format!("整数参数须在 {minimum}–{maximum} 范围内。")))
}
fn high(value: u16) -> bool {
    (0xd800..=0xdbff).contains(&value)
}
fn low(value: u16) -> bool {
    (0xdc00..=0xdfff).contains(&value)
}

impl HistoryState {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "history.ownership" => {
                let relationship = &arguments["relationship"];
                let context = &arguments["context"];
                if !same(&relationship["conversationId"], &context["conversationId"])
                    || !same(&relationship["projectId"], &context["projectId"])
                {
                    return Err(ServiceError::new(
                        "WORKSPACE_CHANGED",
                        "聊天工作范围已变化。",
                        409,
                    ));
                }
                if relationship["isArchived"] == true || relationship["projectArchived"] == true {
                    return Err(ServiceError::new(
                        "CONVERSATION_HISTORY_ARCHIVED",
                        "已归档聊天不能用于模型历史查阅。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "history.message" => {
                let message = &arguments["message"];
                if !["user", "assistant"].contains(&string(&message["Role"]))
                    || message["Status"] == "streaming"
                    || arguments["contentIsString"] != true
                {
                    return Ok(Value::Null);
                }
                let mut metadata = json!({"messageId":message["Id"],"role":message["Role"],"status":message["Status"]});
                if crate::models::protocol::truthy(&message["CreatedAt"]) {
                    metadata["createdAt"] = message["CreatedAt"].clone();
                }
                self.messages.push(HistoryMessage {
                    metadata,
                    units: Vec::new(),
                });
                Ok(json!({"messageIndex":self.messages.len()-1,"chunkCharacters":16384}))
            }
            "history.append" => {
                let index = arguments["messageIndex"].as_u64().unwrap_or(u64::MAX) as usize;
                let message = self
                    .messages
                    .get_mut(index)
                    .ok_or_else(|| invalid("历史消息上传身份无效。"))?;
                message.units.extend(
                    array(&arguments["units"])
                        .iter()
                        .filter_map(Value::as_u64)
                        .map(|value| value as u16),
                );
                Ok(Value::Null)
            }
            "history.search" => {
                let input = &arguments["input"];
                let query = string(&input["query"]).to_lowercase();
                if query.contains('\0') {
                    return Err(invalid("历史查询不能包含空字符。"));
                }
                let offset = bounded(&input["offset"], 0, 1_000_000, 0)?;
                let limit = bounded(&input["limit"], 10, 20, 1)?;
                let mut matching = Vec::new();
                for message in &self.messages {
                    let text = String::from_utf16_lossy(&message.units);
                    let folded = text.to_lowercase();
                    let match_offset = if query.is_empty() {
                        Some(0)
                    } else {
                        folded.find(&query).map(|byte| {
                            let folded_offset = folded[..byte].encode_utf16().count();
                            let mut original = 0;
                            let mut folded = 0;
                            for character in text.chars() {
                                let next = folded
                                    + character.to_lowercase().map(char::len_utf16).sum::<usize>();
                                if folded_offset < next {
                                    break;
                                }
                                folded = next;
                                original += character.len_utf16();
                            }
                            original
                        })
                    };
                    if let Some(match_offset) = match_offset {
                        matching.push((message, match_offset));
                    }
                }
                if offset > matching.len() {
                    return Err(invalid("历史查询分页位置超出范围。"));
                }
                let mut selected = Vec::new();
                let mut serialized_characters = 2;
                for (message, match_offset) in matching.iter().skip(offset).take(limit) {
                    let mut start = match_offset.saturating_sub(120);
                    let mut end = (start + 512).min(message.units.len());
                    if start > 0
                        && message.units.get(start).is_some_and(|value| low(*value))
                        && high(message.units[start - 1])
                    {
                        start -= 1;
                    }
                    if end < message.units.len() && end > 0 && high(message.units[end - 1]) {
                        end -= 1;
                    }
                    let mut value = message.metadata.clone();
                    value["matchOffset"] = json!(match_offset);
                    value["excerpt"] = json!(String::from_utf16_lossy(&message.units[start..end]));
                    value["excerptOffset"] = json!(start);
                    value["totalCharacters"] = json!(message.units.len());
                    let size = serde_json::to_string(&value)
                        .unwrap()
                        .encode_utf16()
                        .count()
                        + usize::from(!selected.is_empty());
                    if serialized_characters + size > 60_000 {
                        break;
                    }
                    serialized_characters += size;
                    selected.push(value);
                }
                Ok(
                    json!({"conversationId":arguments["conversationId"],"messages":selected,"offset":offset,"nextOffset":matching.len().min(offset+selected.len()),"total":matching.len(),"hasMore":offset+selected.len()<matching.len()}),
                )
            }
            "history.read" => {
                let input = &arguments["input"];
                let message = self
                    .messages
                    .iter()
                    .find(|message| same(&message.metadata["messageId"], &arguments["messageId"]))
                    .ok_or_else(|| {
                        ServiceError::new(
                            "CONVERSATION_HISTORY_MESSAGE_NOT_FOUND",
                            "当前聊天不存在此公开消息。",
                            404,
                        )
                    })?;
                let mut offset = bounded(&input["offset"], 0, 16_000_000, 0)?;
                let limit = bounded(&input["limit"], 4096, 16000, 1)?;
                if offset > message.units.len() {
                    return Err(invalid("历史消息分页位置超出范围。"));
                }
                if offset > 0
                    && message.units.get(offset).is_some_and(|value| low(*value))
                    && high(message.units[offset - 1])
                {
                    offset -= 1;
                }
                let mut end = (offset + limit).min(message.units.len());
                if end < message.units.len() && end > 0 && high(message.units[end - 1]) {
                    end -= 1;
                }
                if end == offset && end < message.units.len() {
                    end = (end + 2).min(message.units.len());
                }
                let mut result = message.metadata.clone();
                result["conversationId"] = arguments["conversationId"].clone();
                result["units"] = json!(&message.units[offset..end]);
                result["offset"] = json!(offset);
                result["nextOffset"] = json!(end);
                result["totalCharacters"] = json!(message.units.len());
                result["truncated"] = json!(end < message.units.len());
                Ok(result)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown history service operation.",
                400,
            )),
        }
    }
}
