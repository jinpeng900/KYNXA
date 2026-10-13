use serde_json::{Value, json};

use super::{gateway::ModelError, protocol, stream::StreamState};

const MAXIMUM_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_FRAME_CHARACTERS: usize = 2 * 1024 * 1024;

/// Frame bytes until a complete UTF-8 line exists; CR/LF split boundaries never repair provider data.
/// 只有完整 UTF-8 行才解码；跨包 CR/LF 不得修补供应商数据或派发末尾未闭合的 SSE 帧。
#[derive(Default)]
pub struct FrameDecoder {
    pending: Vec<u8>,
    data: Vec<String>,
    event: String,
    frame_characters: usize,
}
impl FrameDecoder {
    pub fn push(
        &mut self,
        bytes: &[u8],
        ndjson: bool,
        end: bool,
    ) -> Result<Vec<Value>, ModelError> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() + self.data.iter().map(String::len).sum::<usize>()
            > MAXIMUM_FRAME_BYTES
        {
            return Err(ModelError::new(
                "MODEL_STREAM_FRAME_LIMIT",
                "模型返回的流式事件过大。",
                502,
            ));
        }
        let mut frames = Vec::new();
        let mut offset = 0;
        while let Some(position) = self.pending[offset..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
        {
            let position = offset + position;
            if self.pending[position] == b'\r' && position + 1 == self.pending.len() && !end {
                break;
            }
            let width = if self.pending[position] == b'\r'
                && self.pending.get(position + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            let line = std::str::from_utf8(&self.pending[offset..position])
                .map_err(|_| {
                    ModelError::new("MODEL_STREAM_INVALID_UTF8", "模型流包含无效 UTF-8。", 502)
                })?
                .to_owned();
            offset = position + width;
            self.line(&line, ndjson, &mut frames)?;
        }
        self.pending.drain(..offset);
        if end && ndjson && !self.pending.is_empty() {
            let line = String::from_utf8(std::mem::take(&mut self.pending)).map_err(|_| {
                ModelError::new("MODEL_STREAM_INVALID_UTF8", "模型流包含无效 UTF-8。", 502)
            })?;
            self.line(&line, true, &mut frames)?;
        }
        Ok(frames)
    }
    fn line(
        &mut self,
        line: &str,
        ndjson: bool,
        frames: &mut Vec<Value>,
    ) -> Result<(), ModelError> {
        if ndjson {
            if !line.trim().is_empty() {
                frames.push(json!({"data":line}));
            }
        } else if line.is_empty() {
            if !self.data.is_empty() {
                frames.push(json!({"event":self.event,"data":self.data.join("\n")}));
            }
            self.data.clear();
            self.event.clear();
            self.frame_characters = 0;
        } else if !line.starts_with(':') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            if field == "data" {
                self.frame_characters += value.encode_utf16().count();
                if self.frame_characters > MAXIMUM_FRAME_CHARACTERS {
                    return Err(ModelError::new(
                        "MODEL_STREAM_FRAME_LIMIT",
                        "模型返回的流式事件过大。",
                        502,
                    ));
                }
                self.data.push(value.into());
            } else if field == "event" {
                self.event = value.into();
            }
        }
        Ok(())
    }
}

pub struct ProviderStream {
    state: StreamState,
    protocol: String,
    pub usage: Value,
    pub identity: Value,
    ollama_content: String,
    ollama_reasoning: String,
    ollama_calls: Vec<Value>,
    completion: Option<Value>,
}
impl ProviderStream {
    pub fn new(protocol: &str) -> Result<Self, ModelError> {
        let mut state = StreamState::default();
        state
            .apply(
                "models.stream.open",
                &json!({"protocol":protocol,"allowTruncatedText":true}),
            )
            .map_err(ModelError::native)?;
        Ok(Self {
            state,
            protocol: protocol.into(),
            usage: Value::Null,
            identity: Value::Null,
            ollama_content: String::new(),
            ollama_reasoning: String::new(),
            ollama_calls: Vec::new(),
            completion: None,
        })
    }
    pub fn frame(&mut self, frame: &Value) -> Result<Value, ModelError> {
        let item: Value =
            serde_json::from_str(frame["data"].as_str().unwrap_or("")).unwrap_or(Value::Null);
        if !item["id"].is_null() {
            self.identity = item["id"].clone();
        }
        if !item["message"]["id"].is_null() {
            self.identity = item["message"]["id"].clone();
        }
        for usage in [
            &item["usage"],
            &item["message"]["usage"],
            &item["response"]["usage"],
        ] {
            if let Some(fields) = usage.as_object() {
                if !self.usage.is_object() {
                    self.usage = json!({});
                }
                self.usage.as_object_mut().unwrap().extend(fields.clone());
            }
        }
        if let Some(completion) = &self.completion {
            // Chat may report usage after finish_reason; never reopen its settled native decoder.
            // Chat 可以在 finish_reason 后补 usage；不得重新打开已经收口的原生解码器。
            if frame["data"] == "[DONE]" {
                let mut completion = completion.clone();
                if !self.usage.is_null() {
                    completion["usage"] = self.usage.clone();
                }
                return Ok(json!({"events":[],"result":completion}));
            }
            if item["choices"].as_array().is_some_and(Vec::is_empty)
                && item["usage"].is_object()
                && item.get("error").is_none()
            {
                return Ok(json!({"events":[]}));
            }
            return Err(ModelError::new(
                "MODEL_STREAM_INVALID",
                "模型终态之后返回了新增内容。",
                502,
            ));
        }
        if self.protocol == "ollama" {
            if item.is_null() || item.get("error").is_some() {
                return Err(ModelError::new(
                    "MODEL_STREAM_INVALID",
                    "本地模型返回无效流事件。",
                    502,
                ));
            }
            let text = item["message"]["content"].as_str().unwrap_or("");
            let thinking = item["message"]["thinking"].as_str().unwrap_or("");
            self.ollama_content.push_str(text);
            self.ollama_reasoning.push_str(thinking);
            if self.ollama_content.encode_utf16().count()
                + self.ollama_reasoning.encode_utf16().count()
                > 2 * 1024 * 1024
            {
                return Err(ModelError::new(
                    "MODEL_RESPONSE_SIZE_LIMIT",
                    "本地模型回复超过容量。",
                    502,
                ));
            }
            if let Some(calls) = item["message"]["tool_calls"].as_array() {
                self.ollama_calls.extend(calls.clone());
            }
            if let Some(failure) = protocol::validate_count(self.ollama_calls.len()) {
                return Err(ModelError::decoded(&failure["error"]));
            }
            let mut events = Vec::new();
            if !text.is_empty() {
                events.push(json!({"type":"text_delta","delta":text}));
            }
            if !thinking.is_empty() {
                events.push(json!({"type":"reasoning_delta","delta":thinking}));
            }
            if item["done"] == true {
                self.usage = json!({"prompt_tokens":item["prompt_eval_count"],"completion_tokens":item["eval_count"]});
                let mut result = item;
                result["message"] = json!({"role":"assistant","content":self.ollama_content,
                    "reasoning_content":self.ollama_reasoning,"tool_calls":self.ollama_calls});
                return Ok(json!({"events":events,"result":result}));
            }
            return Ok(json!({"events":events}));
        }
        let mut decoded = self
            .state
            .apply("models.stream.frame", &json!({"frame":frame}))
            .map_err(ModelError::native)?;
        if decoded["result"].is_object() {
            if !self.usage.is_null() {
                decoded["result"]["usage"] = self.usage.clone();
            }
            if !self.identity.is_null() {
                decoded["result"]["id"] = self.identity.clone();
            }
            if self.protocol == "openai-completions" {
                self.completion = Some(decoded["result"].clone());
            }
        }
        Ok(decoded)
    }
    pub fn end(&mut self) -> ModelError {
        if self.protocol == "ollama" {
            return ModelError::new("MODEL_STREAM_INCOMPLETE", "本地模型流缺少终态。", 502);
        }
        match self.state.apply("models.stream.end", &json!({})) {
            Ok(value) => ModelError::decoded(&value["error"]),
            Err(code) => ModelError::native(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_and_crlf_boundaries_dispatch_only_closed_frames() {
        let source = "data: 中文\r\n\r\ndata: unfinished".as_bytes();
        for boundary in 0..source.len() {
            let mut decoder = FrameDecoder::default();
            let mut frames = decoder.push(&source[..boundary], false, false).unwrap();
            frames.extend(decoder.push(&source[boundary..], false, true).unwrap());
            assert_eq!(frames, vec![json!({"event":"","data":"中文"})]);
        }
    }
}
