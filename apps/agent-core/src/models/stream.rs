use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::protocol::{
    array, characters, check_finish, decoder_error, error, estimate_tokens, output_limit, string,
    text_parts, truthy, validate_buffer, validate_count,
};

/// A single upstream stream owns bounded incremental state. Open never resets an existing stream.
/// 单次上游流拥有有界增量状态；重复 open 不能重置已接收内容与参数账目。
#[derive(Default)]
pub struct StreamState {
    plain: super::plain::PlainStreamState,
    opened: bool,
    settled: bool,
    protocol: String,
    allow_truncated_text: bool,
    content: String,
    reasoning: String,
    // Account only the new delta; rescanning an entire long reply per frame would be quadratic.
    // 只计算新增片段；每帧重扫完整长回复会形成二次方开销。
    content_characters: usize,
    reasoning_characters: usize,
    raw_reasoning: String,
    stop_reason: Value,
    calls: Vec<(u64, Value)>,
    blocks: BTreeMap<String, Value>,
    response_arguments: Vec<(String, String)>,
    argument_parts: Vec<String>,
    frame_events: Vec<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_or_finalizing_twice_cannot_reset_argument_usage() {
        let mut stream = StreamState::default();
        stream
            .apply(
                "models.stream.open",
                &json!({"protocol":"openai-responses"}),
            )
            .unwrap();
        let frame = json!({"frame":{"data":json!({"type":"response.function_call_arguments.delta","item_id":"first","delta":"{\"x\":1}"}).to_string()}});
        stream.apply("models.stream.frame", &frame).unwrap();
        assert_eq!(
            stream.apply("models.stream.open", &json!({})).unwrap_err(),
            "AGENT_CORE_STREAM_ALREADY_OPEN"
        );
        let usage =
            stream.apply("models.stream.state", &json!({})).unwrap()["estimatedGeneratedTokens"]
                .as_u64()
                .unwrap();
        assert!(usage > 0);
        stream.apply("models.stream.end", &json!({})).unwrap();
        assert_eq!(
            stream.apply("models.stream.frame", &frame).unwrap_err(),
            "AGENT_CORE_STREAM_SETTLED"
        );
        assert_eq!(
            stream.apply("models.stream.state", &json!({})).unwrap()["estimatedGeneratedTokens"],
            usage
        );
    }
}

impl StreamState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation.starts_with("models.stream.plain.") {
            return self.plain.apply(operation, args);
        }
        if operation == "models.stream.open" {
            if self.opened {
                return Err("AGENT_CORE_STREAM_ALREADY_OPEN".to_owned());
            }
            self.protocol = string(&args["protocol"]).to_owned();
            self.allow_truncated_text = args["allowTruncatedText"] == true;
            self.opened = true;
            return Ok(Value::Null);
        }
        if !self.opened {
            return Err("AGENT_CORE_STREAM_NOT_OPEN".to_owned());
        }
        if operation == "models.stream.state" {
            return Ok(self.summary());
        }
        if self.settled {
            return Err("AGENT_CORE_STREAM_SETTLED".to_owned());
        }
        if operation == "models.stream.frame" {
            self.frame_events.clear();
            let mut result = self.frame(&args["frame"]);
            if result.get("error").is_some() {
                result["events"] = json!(self.frame_events);
            }
            if result.get("result").is_some() || result.get("error").is_some() {
                self.settled = true;
            }
            return Ok(self.account_failure(result));
        }
        if operation == "models.stream.end" {
            self.settled = true;
            let failure = error(
                "模型连接已断开，未执行未完成的工具调用。",
                self.has_tool_calls()
                    .then_some("MODEL_TOOL_STREAM_INCOMPLETE"),
                "interrupted",
            );
            return Ok(self.account_failure(failure));
        }
        Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned())
    }

    fn has_tool_calls(&self) -> bool {
        !self.calls.is_empty()
            || self
                .blocks
                .values()
                .any(|block| block["type"] == "tool_use")
    }

    fn generated_tokens(&self) -> u64 {
        estimate_tokens(&self.content)
            + estimate_tokens(&self.reasoning)
            + estimate_tokens(&self.argument_parts.concat())
    }

    fn summary(&self) -> Value {
        json!({"estimatedGeneratedTokens":self.generated_tokens(),"hasPendingToolCalls":self.has_tool_calls(),"settled":self.settled})
    }

    fn account_failure(&self, mut result: Value) -> Value {
        if result.get("error").is_some() {
            result["error"]["estimatedGeneratedTokens"] = json!(self.generated_tokens());
        }
        result
    }

    fn send(&mut self, kind: &str, delta: &Value, events: &mut Vec<Value>) -> Option<Value> {
        let text = text_parts(delta);
        if text.is_empty() {
            return None;
        }
        if kind == "text_delta" {
            self.content.push_str(&text);
            self.content_characters += characters(&text);
        } else {
            self.reasoning.push_str(&text);
            self.reasoning_characters += characters(&text);
        }
        if self.content_characters + self.reasoning_characters > 2 * 1024 * 1024 {
            return Some(error("本次回复超过大小限制。", None, "error"));
        }
        let event = json!({"type":kind,"delta":text});
        self.frame_events.push(event.clone());
        events.push(event);
        None
    }

    fn completion_result(&self) -> Value {
        let mut message = json!({"role":"assistant","content":self.content});
        if !self.raw_reasoning.is_empty() {
            message["reasoning_content"] = json!(self.raw_reasoning);
        }
        if !self.calls.is_empty() {
            message["tool_calls"] =
                json!(self.calls.iter().map(|(_, call)| call).collect::<Vec<_>>());
        }
        json!({"choices":[{"finish_reason":if self.stop_reason.is_null() {json!("stop")} else {self.stop_reason.clone()},"message":message}]})
    }

    fn frame(&mut self, frame: &Value) -> Value {
        if frame["data"] == "[DONE]" {
            return if self.protocol == "openai-completions" {
                json!({"events":[],"result":self.completion_result()})
            } else {
                error("模型工具流缺少完成事件。", None, "error")
            };
        }
        let item: Value = match serde_json::from_str(string(&frame["data"])) {
            Ok(item) => item,
            Err(_) => {
                return if self.has_tool_calls() {
                    decoder_error(
                        "模型工具流返回了无法识别的数据。",
                        "MODEL_TOOL_STREAM_INVALID",
                    )
                } else {
                    error("模型流返回了无法识别的数据。", None, "error")
                };
            }
        };
        let kind = item
            .get("type")
            .filter(|value| !value.is_null())
            .map(string)
            .unwrap_or_else(|| string(&frame["event"]))
            .to_owned();
        if truthy(&item["error"]) || kind == "error" {
            return error(
                "模型服务返回错误，请检查连接配置或稍后重试。",
                None,
                "error",
            );
        }
        let mut events = Vec::new();
        if self.protocol == "openai-responses" {
            if kind == "response.output_text.delta" {
                if let Some(failure) = self.send("text_delta", &item["delta"], &mut events) {
                    return failure;
                }
            }
            if matches!(
                kind.as_str(),
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta"
            ) {
                if let Some(failure) = self.send("reasoning_delta", &item["delta"], &mut events) {
                    return failure;
                }
            }
            if matches!(
                kind.as_str(),
                "response.function_call_arguments.delta" | "response.function_call_arguments.done"
            ) {
                let identity = item
                    .get("item_id")
                    .filter(|value| !value.is_null())
                    .or_else(|| item.get("output_index").filter(|value| !value.is_null()));
                let key = identity
                    .map(|value| serde_json::to_string(value).unwrap())
                    .unwrap_or_else(|| "unbound".to_owned());
                let position = self
                    .response_arguments
                    .iter()
                    .position(|(identity, _)| identity == &key);
                let previous = position
                    .map(|position| self.response_arguments[position].1.clone())
                    .unwrap_or_default();
                let next = if kind.ends_with(".delta") {
                    format!("{previous}{}", string(&item["delta"]))
                } else if item["arguments"].is_string()
                    && characters(string(&item["arguments"])) >= characters(&previous)
                {
                    string(&item["arguments"]).to_owned()
                } else {
                    previous
                };
                if let Some(position) = position {
                    self.response_arguments[position].1 = next.clone();
                } else {
                    self.response_arguments.push((key.clone(), next.clone()));
                }
                self.argument_parts = self
                    .response_arguments
                    .iter()
                    .map(|(_, text)| text.clone())
                    .collect();
                if let Some(failure) = validate_count(self.response_arguments.len()) {
                    return failure;
                }
                if let Some(failure) = validate_buffer(
                    if key == "unbound" { "" } else { &next },
                    self.argument_parts
                        .iter()
                        .map(|text| characters(text))
                        .sum(),
                ) {
                    return failure;
                }
            }
            if kind == "response.completed" {
                return json!({"events":events,"result":item.get("response").filter(|value|!value.is_null()).unwrap_or(&item)});
            }
            if matches!(
                kind.as_str(),
                "response.incomplete" | "response.failed" | "response.cancelled"
            ) {
                if truthy(&item["response"]) {
                    let mut result = item["response"].clone();
                    result["status"] = json!(kind.trim_start_matches("response."));
                    return json!({"events":events,"result":result});
                }
                return error("模型未完整结束本次工具回复。", None, "interrupted");
            }
        } else if self.protocol == "anthropic-messages" {
            let key = serde_json::to_string(&item["index"]).unwrap();
            if kind == "content_block_start" {
                let valid_index = item["index"]
                    .as_u64()
                    .is_some_and(|index| index <= 9_007_199_254_740_991);
                if !valid_index || self.blocks.len() >= 64 || self.blocks.contains_key(&key) {
                    return if item["content_block"]["type"] == "tool_use" || self.has_tool_calls() {
                        decoder_error("模型返回了无效的工具内容块。", "MODEL_TOOL_INDEX_INVALID")
                    } else {
                        error("模型返回了无效的内容块。", None, "error")
                    };
                }
                let mut block = item["content_block"].clone();
                if block["type"] == "tool_use" {
                    if let Some(failure) = validate_count(
                        self.blocks
                            .values()
                            .filter(|value| value["type"] == "tool_use")
                            .count()
                            + 1,
                    ) {
                        return failure;
                    }
                    block["partialInput"] = json!("");
                }
                self.blocks.insert(key.clone(), block.clone());
                if block["type"] == "text" {
                    if let Some(failure) = self.send("text_delta", &block["text"], &mut events) {
                        return failure;
                    }
                }
                if block["type"] == "thinking" {
                    if let Some(failure) =
                        self.send("reasoning_delta", &block["thinking"], &mut events)
                    {
                        return failure;
                    }
                }
            }
            if kind == "content_block_delta" {
                let delta = &item["delta"];
                let delta_kind = string(&delta["type"]);
                if !self.blocks.contains_key(&key)
                    && matches!(delta_kind, "text_delta" | "thinking_delta")
                {
                    self.blocks.insert(
                        key.clone(),
                        if delta_kind == "text_delta" {
                            json!({"type":"text","text":""})
                        } else {
                            json!({"type":"thinking","thinking":""})
                        },
                    );
                }
                if !self.blocks.contains_key(&key) {
                    return if delta_kind == "signature_delta" {
                        json!({"events":events})
                    } else if delta_kind == "input_json_delta" {
                        decoder_error("模型工具流内容块顺序无效。", "MODEL_TOOL_IDENTITY_INVALID")
                    } else {
                        error("模型流内容块顺序无效。", None, "error")
                    };
                }
                let block = self.blocks.get_mut(&key).unwrap();
                let append = |block: &mut Value, field: &str, value: &Value| {
                    block[field] = json!(format!("{}{}", string(&block[field]), string(value)));
                };
                if delta_kind == "text_delta" {
                    append(block, "text", &delta["text"]);
                    if let Some(failure) = self.send("text_delta", &delta["text"], &mut events) {
                        return failure;
                    }
                } else if delta_kind == "thinking_delta" {
                    append(block, "thinking", &delta["thinking"]);
                    if let Some(failure) =
                        self.send("reasoning_delta", &delta["thinking"], &mut events)
                    {
                        return failure;
                    }
                } else if delta_kind == "signature_delta" {
                    append(block, "signature", &delta["signature"]);
                } else if delta_kind == "input_json_delta" {
                    self.argument_parts
                        .push(string(&delta["partial_json"]).to_owned());
                    append(block, "partialInput", &delta["partial_json"]);
                    let partial = string(&block["partialInput"]).to_owned();
                    let aggregate = self
                        .blocks
                        .values()
                        .map(|value| characters(string(&value["partialInput"])))
                        .sum();
                    if let Some(failure) = validate_buffer(&partial, aggregate) {
                        return failure;
                    }
                }
            }
            if kind == "message_delta" {
                self.stop_reason = item["delta"]["stop_reason"].clone();
            }
            if kind == "message_stop" {
                if !truthy(&self.stop_reason) {
                    return error("模型工具流缺少结束状态。", None, "error");
                }
                let text_limit = self.allow_truncated_text
                    && output_limit(string(&self.stop_reason))
                    && !self.has_tool_calls();
                if self.stop_reason != "tool_use" && !text_limit {
                    if let Some(mut failure) = check_finish(&self.stop_reason) {
                        if self.has_tool_calls() {
                            failure["error"]["code"] = json!("MODEL_TOOL_OUTPUT_TRUNCATED");
                        }
                        return failure;
                    }
                }
                let mut blocks: Vec<_> = self.blocks.iter().collect();
                blocks.sort_by_key(|(key, _)| serde_json::from_str::<u64>(key).unwrap_or(0));
                let mut raw = Vec::new();
                for (_, block) in blocks {
                    let mut block = block.clone();
                    if block["type"] == "tool_use" {
                        let partial = block
                            .as_object_mut()
                            .unwrap()
                            .remove("partialInput")
                            .unwrap_or(Value::Null);
                        if !string(&partial).is_empty() {
                            block["input"] = match serde_json::from_str(string(&partial)) {
                                Ok(input) => input,
                                Err(_) => {
                                    return decoder_error(
                                        "模型返回了不完整的工具参数。",
                                        "MODEL_TOOL_ARGUMENT_INVALID",
                                    );
                                }
                            };
                        }
                    }
                    raw.push(block);
                }
                return json!({"events":events,"result":{"content":raw,"stop_reason":self.stop_reason}});
            }
        } else {
            let choices = array(&item["choices"]);
            let choice = choices
                .iter()
                .find(|choice| choice["index"] == 0)
                .or_else(|| choices.first())
                .unwrap_or(&Value::Null);
            let delta = &choice["delta"];
            if let Some(failure) = self.send("text_delta", &delta["content"], &mut events) {
                return failure;
            }
            let thinking = delta
                .get("reasoning_content")
                .filter(|value| !value.is_null())
                .unwrap_or(&delta["reasoning"]);
            if truthy(thinking) {
                self.raw_reasoning.push_str(&text_parts(thinking));
                if let Some(failure) = self.send("reasoning_delta", thinking, &mut events) {
                    return failure;
                }
            }
            for part in array(&delta["tool_calls"]) {
                self.argument_parts
                    .push(string(&part["function"]["arguments"]).to_owned());
                let mut slot = part["index"]
                    .as_u64()
                    .filter(|index| *index <= 9_007_199_254_740_991);
                if let Some(text) = part["index"].as_str() {
                    if text.len() <= 6
                        && (text == "0"
                            || text.starts_with(|character: char| ('1'..='9').contains(&character)))
                        && text.bytes().all(|character| character.is_ascii_digit())
                    {
                        slot = text.parse().ok();
                    }
                }
                if part["index"].is_null() && part["id"].is_string() {
                    let matches: Vec<_> = self
                        .calls
                        .iter()
                        .filter(|(_, call)| call["id"] == part["id"])
                        .collect();
                    if matches.len() == 1 {
                        slot = Some(matches[0].0);
                    }
                }
                let Some(slot) = slot else {
                    return decoder_error("工具调用序号无效。", "MODEL_TOOL_INDEX_INVALID");
                };
                let position = if let Some(position) =
                    self.calls.iter().position(|(index, _)| *index == slot)
                {
                    position
                } else {
                    if let Some(failure) = validate_count(self.calls.len() + 1) {
                        return failure;
                    }
                    self.calls.push((
                        slot,
                        json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                    ));
                    self.calls.len() - 1
                };
                let call = &mut self.calls[position].1;
                if truthy(&part["id"]) {
                    if truthy(&call["id"]) && call["id"] != part["id"] {
                        return decoder_error(
                            "工具调用身份发生改变。",
                            "MODEL_TOOL_IDENTITY_INVALID",
                        );
                    }
                    call["id"] = part["id"].clone();
                }
                call["function"]["name"] = json!(format!(
                    "{}{}",
                    string(&call["function"]["name"]),
                    string(&part["function"]["name"])
                ));
                if characters(string(&call["function"]["name"])) > 256
                    || characters(string(&call["id"])) > 200
                {
                    return decoder_error(
                        "模型返回了无效的工具调用身份。",
                        "MODEL_TOOL_IDENTITY_INVALID",
                    );
                }
                call["function"]["arguments"] = json!(format!(
                    "{}{}",
                    string(&call["function"]["arguments"]),
                    string(&part["function"]["arguments"])
                ));
                let arguments = string(&call["function"]["arguments"]).to_owned();
                let aggregate = self
                    .calls
                    .iter()
                    .map(|(_, call)| characters(string(&call["function"]["arguments"])))
                    .sum();
                if let Some(failure) = validate_buffer(&arguments, aggregate) {
                    return failure;
                }
            }
            if truthy(&choice["finish_reason"]) {
                self.stop_reason = choice["finish_reason"].clone();
                return json!({"events":events,"result":self.completion_result()});
            }
        }
        json!({"events":events})
    }
}
