use serde_json::{Value, json};

use super::protocol::{
    array, characters, check_finish, error, final_parts, string, text_parts, truthy,
};

/// Plain model streams have their own 8 Mi UTF-16 limit, distinct from the 2 Mi tool-stream contract.
/// 普通模型流独立保留 8 Mi UTF-16 上限，不与工具流 2 Mi 合同混用。
#[derive(Default)]
pub struct PlainStreamState {
    opened: bool,
    settled: bool,
    protocol: String,
    content: String,
    reasoning: String,
    // Account only the new delta; rescanning an entire long reply per frame would be quadratic.
    // 只计算新增片段；每帧重扫完整长回复会形成二次方开销。
    content_characters: usize,
    reasoning_characters: usize,
    frame_events: Vec<Value>,
}

impl PlainStreamState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation == "models.stream.plain.open" {
            if self.opened {
                return Err("AGENT_CORE_STREAM_ALREADY_OPEN".to_owned());
            }
            self.opened = true;
            self.protocol = string(&args["protocol"]).to_owned();
            return Ok(Value::Null);
        }
        if !self.opened {
            return Err("AGENT_CORE_STREAM_NOT_OPEN".to_owned());
        }
        if operation == "models.stream.plain.state" {
            return Ok(
                json!({"settled":self.settled,"contentCharacters":self.content_characters,"reasoningCharacters":self.reasoning_characters}),
            );
        }
        if self.settled {
            return Err("AGENT_CORE_STREAM_SETTLED".to_owned());
        }
        self.frame_events.clear();
        let mut result = match operation {
            "models.stream.plain.frame" => self.frame(&args["frame"]),
            "models.stream.plain.json" => self.json(&args["result"]),
            "models.stream.plain.end" => {
                error("连接提前结束，已保留生成的内容。", None, "interrupted")
            }
            _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
        };
        if result.get("result").is_some() || result.get("error").is_some() {
            self.settled = true;
        }
        let is_json = operation == "models.stream.plain.json";
        let has_snapshot = self
            .frame_events
            .iter()
            .any(|event| event["type"] == "content_snapshot");
        if is_json || has_snapshot {
            // The final frame may reference 8 Mi UTF-16 of reasoning; send its snapshot once to stay within transport limits.
            // 最终帧可能引用已累积的 8 Mi UTF-16 思考内容；快照只传一次，避免重复转义超限。
            if result.get("result").is_none() {
                result["result"] = self.completed()["result"].clone();
            }
            result["events"] = json!(self.frame_events.iter().map(|event| {
                if event["type"] == "content_snapshot" {
                    json!({"type":event["type"],"field":"snapshot"})
                } else if is_json {
                    json!({"type":event["type"],"field":if event["type"] == "text_delta" { "content" } else { "reasoning" }})
                } else {
                    event.clone()
                }
            }).collect::<Vec<_>>());
            self.frame_events.clear();
        } else {
            result["events"] = json!(self.frame_events);
        }
        Ok(result)
    }

    fn append(&mut self, kind: &str, delta: &Value) -> Option<Value> {
        let Some(text) = delta.as_str().filter(|text| !text.is_empty()) else {
            return None;
        };
        if kind == "text_delta" {
            self.content.push_str(text);
            self.content_characters += characters(text);
        } else {
            self.reasoning.push_str(text);
            self.reasoning_characters += characters(text);
        }
        if self.content_characters + self.reasoning_characters > 8 * 1024 * 1024 {
            return Some(error("模型回复过长，已停止接收。", None, "interrupted"));
        }
        self.frame_events.push(json!({"type":kind,"delta":text}));
        None
    }

    fn completed(&self) -> Value {
        json!({"result":{"content":self.content,"reasoning":self.reasoning}})
    }

    fn json(&mut self, result: &Value) -> Value {
        let parts = final_parts(&self.protocol, result);
        if parts.get("error").is_some() {
            return parts;
        }
        if let Some(failure) = self.append("reasoning_delta", &parts["reasoning"]) {
            return failure;
        }
        if let Some(failure) = self.append("text_delta", &parts["content"]) {
            return failure;
        }
        if let Some(failure) = check_finish(&parts["finish"]) {
            return failure;
        }
        self.completed()
    }

    fn frame(&mut self, frame: &Value) -> Value {
        if frame["data"] == "[DONE]" {
            return if self.protocol.is_empty() || self.protocol == "openai-completions" {
                self.completed()
            } else {
                json!({})
            };
        }
        let item: Value = match serde_json::from_str(string(&frame["data"])) {
            Ok(item) => item,
            Err(_) => return error("模型接口返回了无效的流式事件。", None, "error"),
        };
        if truthy(&item["error"]) || item["type"] == "error" || frame["event"] == "error" {
            return error("模型服务返回错误，已保留生成的内容。", None, "error");
        }
        let kind = item
            .get("type")
            .filter(|value| !value.is_null())
            .map(string)
            .unwrap_or_else(|| string(&frame["event"]));
        if self.protocol == "anthropic-messages" {
            if kind == "content_block_start" {
                if let Some(failure) = self.append("text_delta", &item["content_block"]["text"]) {
                    return failure;
                }
                if let Some(failure) =
                    self.append("reasoning_delta", &item["content_block"]["thinking"])
                {
                    return failure;
                }
            } else if kind == "content_block_delta" {
                let delta = &item["delta"];
                if delta["type"] == "text_delta" {
                    if let Some(failure) = self.append("text_delta", &delta["text"]) {
                        return failure;
                    }
                }
                if delta["type"] == "thinking_delta" {
                    if let Some(failure) = self.append("reasoning_delta", &delta["thinking"]) {
                        return failure;
                    }
                }
            } else if kind == "message_delta" {
                if let Some(failure) = check_finish(&item["delta"]["stop_reason"]) {
                    return failure;
                }
            } else if kind == "message_stop" {
                return self.completed();
            }
        } else if self.protocol == "openai-responses" {
            if kind == "response.output_text.delta" {
                if let Some(failure) = self.append("text_delta", &item["delta"]) {
                    return failure;
                }
            } else if kind == "response.reasoning_summary_text.delta" {
                if let Some(failure) = self.append("reasoning_delta", &item["delta"]) {
                    return failure;
                }
            } else if kind == "response.completed" {
                if truthy(&item["response"]["output"]) {
                    let parts = final_parts(&self.protocol, &item["response"]);
                    if parts.get("error").is_some() {
                        return parts;
                    }
                    self.content = string(&parts["content"]).to_owned();
                    self.content_characters = characters(&self.content);
                    if !string(&parts["reasoning"]).is_empty() {
                        self.reasoning = string(&parts["reasoning"]).to_owned();
                        self.reasoning_characters = characters(&self.reasoning);
                    }
                    self.frame_events.push(json!({"type":"content_snapshot"}));
                    if let Some(failure) = check_finish(&parts["finish"]) {
                        return failure;
                    }
                }
                return self.completed();
            } else if matches!(
                kind,
                "response.incomplete" | "response.failed" | "response.cancelled"
            ) {
                if truthy(&item["response"]["output"]) {
                    let parts = final_parts(&self.protocol, &item["response"]);
                    if parts.get("error").is_some() {
                        return parts;
                    }
                    if !string(&parts["content"]).is_empty() {
                        self.content = string(&parts["content"]).to_owned();
                        self.content_characters = characters(&self.content);
                    }
                    if !string(&parts["reasoning"]).is_empty() {
                        self.reasoning = string(&parts["reasoning"]).to_owned();
                        self.reasoning_characters = characters(&self.reasoning);
                    }
                    self.frame_events.push(json!({"type":"content_snapshot"}));
                    if let Some(failure) = check_finish(&parts["finish"]) {
                        return failure;
                    }
                }
                return error(
                    "模型未完整生成回复，已保留生成的内容。",
                    None,
                    "interrupted",
                );
            }
        } else {
            let choices = array(&item["choices"]);
            let choice = choices
                .iter()
                .find(|choice| choice["index"] == 0)
                .or_else(|| choices.iter().find(|choice| choice["index"].is_null()))
                .unwrap_or(&Value::Null);
            let thinking = choice["delta"]
                .get("reasoning_content")
                .filter(|value| !value.is_null())
                .unwrap_or(&choice["delta"]["reasoning"]);
            if let Some(failure) = self.append("reasoning_delta", &json!(text_parts(thinking))) {
                return failure;
            }
            if let Some(failure) = self.append(
                "text_delta",
                &json!(text_parts(&choice["delta"]["content"])),
            ) {
                return failure;
            }
            if truthy(&choice["finish_reason"]) {
                if let Some(failure) = check_finish(&choice["finish_reason"]) {
                    return failure;
                }
                return self.completed();
            }
        }
        json!({})
    }
}
