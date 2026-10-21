use serde_json::{Value, json};

pub(crate) const MAX_MESSAGE_SEGMENTS: usize = 512;
pub(crate) const MAX_SEGMENT_ORDER: u64 = 8192;

fn invalid() -> String {
    "INVALID_CONVERSATION_DATA".into()
}
fn integer(value: &Value, minimum: u64, maximum: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|value| value >= minimum && value <= maximum)
}

/// Public bodies are transported as original UTF-16 by the adapter; native metadata owns ordering and timing.
/// 公开正文由适配器按原始 UTF-16 传输；原生元信息拥有顺序和计时状态。
#[derive(Default)]
pub struct SegmentState {
    items: Vec<Value>,
    order: u64,
    thinking_started: Option<f64>,
}
impl SegmentState {
    fn stop_thinking(&mut self, now: f64) {
        if let Some(started) = self.thinking_started.take() {
            if let Some(current) = self.items.last_mut() {
                current["reasoningDurationMs"] =
                    json!(current["reasoningDurationMs"].as_f64().unwrap_or(0.0) + now - started);
            }
        }
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let now = args["now"].as_f64().unwrap_or(0.0);
        match operation {
            "order" => {
                if let Some(order) = args["value"].as_u64() {
                    self.order = order;
                }
                Ok(json!(self.order))
            }
            "start" => {
                self.items.push(json!({"id":args["id"],"round":args["round"],"order":self.order,"phase":"commentary","status":"streaming","reasoningDurationMs":0}));
                self.order += 1;
                self.thinking_started = None;
                Ok(self.items.last().cloned().unwrap())
            }
            "receive" => {
                let kind = args["type"].as_str().unwrap_or("");
                if kind == "text_delta" {
                    self.stop_thinking(now);
                }
                if kind == "reasoning_delta" && self.thinking_started.is_none() {
                    self.thinking_started = Some(now);
                }
                Ok(
                    json!({"id":self.items.last().ok_or("INVALID_ASSISTANT_SEGMENT")?["id"],"action":match kind{"text_delta"=>"appendContent","reasoning_delta"=>"appendReasoning","content_snapshot"=>"replace",_=>"none"}}),
                )
            }
            "finish" => {
                self.stop_thinking(now);
                let current = self.items.last_mut().ok_or("INVALID_ASSISTANT_SEGMENT")?;
                current["phase"] = json!(if args["final"]
                    .as_bool()
                    .unwrap_or(args["callCount"].as_u64().unwrap_or(0) == 0)
                {
                    "final_answer"
                } else {
                    "commentary"
                });
                current["status"] = json!("completed");
                Ok(current.clone())
            }
            "interrupt" => {
                if self
                    .items
                    .last()
                    .is_some_and(|value| value["status"] == "streaming")
                {
                    self.stop_thinking(now);
                    self.items.last_mut().unwrap()["status"] = json!("interrupted");
                    Ok(json!(true))
                } else {
                    Ok(json!(false))
                }
            }
            "stopThinking" => {
                self.stop_thinking(now);
                Ok(Value::Null)
            }
            "current" => Ok(self.items.last().cloned().unwrap_or(Value::Null)),
            "snapshot" => Ok(json!(self.items)),
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "orchestration.segment.validate" => {
            let items = args["items"]
                .as_array()
                .filter(|items| items.len() <= MAX_MESSAGE_SEGMENTS)
                .ok_or_else(invalid)?;
            let mut ids = std::collections::HashSet::new();
            let mut orders = std::collections::HashSet::new();
            let mut previous_order = None;
            let mut previous_round = 0;
            let mut final_seen = false;
            for item in items {
                let id = item["id"]
                    .as_str()
                    .filter(|id| {
                        !id.is_empty()
                            && id.len() <= 128
                            && id
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                    })
                    .ok_or_else(invalid)?;
                let round = item["round"].as_u64().unwrap_or(0);
                let order = item["order"].as_u64().unwrap_or(u64::MAX);
                if !item["object"].as_bool().unwrap_or(false)
                    || !ids.insert(id)
                    || !orders.insert(order)
                    || !integer(&item["round"], 1, crate::models::transcript::MAX_TRANSCRIPT_ROUNDS as u64)
                    || round <= previous_round
                    || !integer(&item["order"], 0, MAX_SEGMENT_ORDER)
                    || previous_order.is_some_and(|previous| order <= previous)
                    || final_seen
                    || !matches!(item["phase"].as_str(), Some("commentary" | "final_answer"))
                    || !matches!(
                        item["status"].as_str(),
                        Some("streaming" | "completed" | "interrupted")
                    )
                    || item["content"]["string"] != true
                    || item["reasoning"]["string"] != true
                    || item["content"]["length"]
                        .as_u64()
                        .unwrap_or(u64::MAX)
                        .saturating_add(item["reasoning"]["length"].as_u64().unwrap_or(u64::MAX))
                        > 2 * 1024 * 1024
                    || !integer(&item["reasoningDurationMs"], 0, 9_007_199_254_740_991)
                {
                    return Err(invalid());
                }
                previous_order = Some(order);
                previous_round = round;
                final_seen = item["phase"] == "final_answer";
            }
            Ok(Value::Null)
        }
        "orchestration.segment.event" => {
            let items = args["items"].as_array().cloned().unwrap_or_default();
            let event = &args["event"];
            if event["type"] == "assistant_segment" {
                let mut selected = items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item["id"] != event["id"])
                    .map(|(index, item)| (index as i64, item["order"].as_i64().unwrap_or(0)))
                    .collect::<Vec<_>>();
                selected.push((-1, event["order"].as_i64().unwrap_or(0)));
                selected.sort_by_key(|(_, order)| *order);
                return Ok(
                    json!({"replace":selected.iter().map(|(index,_)|*index).collect::<Vec<_>>()}),
                );
            }
            if event["segmentId"].is_null()
                || event["segmentId"] == false
                || event["segmentId"] == 0
                || event["segmentId"] == ""
            {
                return Ok(json!({"handled":false}));
            }
            let index = items
                .iter()
                .position(|item| item["id"] == event["segmentId"])
                .ok_or("INVALID_ASSISTANT_SEGMENT")?;
            Ok(
                json!({"index":index,"action":match event["type"].as_str(){Some("text_delta")=>"appendContent",Some("reasoning_delta")=>"appendReasoning",Some("content_snapshot")=>"replace",_=>"none"}}),
            )
        }
        "orchestration.segment.duration" => {
            if args["hasDuration"] == true {
                if !integer(&args["duration"], 0, 9_007_199_254_740_991) {
                    return Err(invalid());
                }
                return Ok(args["duration"].clone());
            }
            if args["role"] != "assistant"
                || !matches!(
                    args["status"].as_str(),
                    Some("completed" | "error" | "interrupted")
                )
                || args["runVersion"] != 1
                || args["phase"] != args["status"]
                || args["validDates"] != true
            {
                return Ok(json!(0));
            }
            let duration = args["end"].as_f64().unwrap_or(f64::NAN)
                - args["start"].as_f64().unwrap_or(f64::NAN);
            Ok(json!(if duration.is_finite()
                && duration.fract() == 0.0
                && duration >= 0.0
                && duration <= 9_007_199_254_740_991.0
            {
                duration as u64
            } else {
                0
            }))
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
