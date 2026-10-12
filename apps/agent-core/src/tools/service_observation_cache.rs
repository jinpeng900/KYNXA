use crate::models::protocol::truthy;
use serde_json::{Value, json};

use super::service_error::{ServiceError, ServiceResult};

struct ObservationEntry {
    key: String,
    result_text: String,
    time: f64,
    captured_at: String,
    scope: Value,
}

/// Request-local observation receipts are bounded and may only reuse known stateless MCP reads.
/// 请求级观察回执具有容量上限，仅可复用已知无状态 MCP 读取操作。
pub struct ObservationCache {
    entries: Vec<ObservationEntry>,
    bytes: usize,
    ttl_ms: Option<f64>,
}

impl Default for ObservationCache {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
            ttl_ms: Some(15_000.0),
        }
    }
}

fn eligible(arguments: &Value) -> bool {
    arguments["call"]["name"]
        .as_str()
        .is_some_and(|name| name.starts_with("mcp."))
        && arguments["parallel"] == true
}

fn key(arguments: &Value) -> Option<String> {
    super::observations::calculate(
        "observations.fingerprint",
        &json!({"value":[arguments["call"]["name"], arguments["call"]["arguments"]["arguments"]]}),
    )
    .ok()?
    .as_str()
    .map(str::to_owned)
}

impl ObservationCache {
    fn remove(&mut self, index: usize) {
        self.bytes = self
            .bytes
            .saturating_sub(self.entries.remove(index).result_text.len());
    }

    fn expired(&self, entry: &ObservationEntry, now: f64) -> bool {
        self.ttl_ms.is_some_and(|ttl| now - entry.time >= ttl)
    }

    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "observationCache.init" => {
                self.ttl_ms = arguments["ttlMs"].as_f64();
                Ok(Value::Null)
            }
            "observationCache.eligible" => Ok(json!(eligible(arguments))),
            "observationCache.clear" => {
                self.entries.clear();
                self.bytes = 0;
                Ok(Value::Null)
            }
            "observationCache.get" => {
                if !eligible(arguments) {
                    return Ok(Value::Null);
                }
                let Some(key) = key(arguments) else {
                    return Ok(Value::Null);
                };
                let Some(index) = self.entries.iter().position(|entry| entry.key == key) else {
                    return Ok(Value::Null);
                };
                let now = arguments["now"].as_f64().unwrap_or(0.0);
                if self.expired(&self.entries[index], now)
                    || self.entries[index].scope != arguments["scope"]
                {
                    self.remove(index);
                    return Ok(Value::Null);
                }
                Ok(
                    json!({"resultText":self.entries[index].result_text,"capturedAt":self.entries[index].captured_at}),
                )
            }
            "observationCache.remember" => {
                if !eligible(arguments) {
                    return Ok(Value::Null);
                }
                let Some(result_text) = arguments["resultText"].as_str() else {
                    return Ok(Value::Null);
                };
                if result_text.len() > 65_536 {
                    return Ok(Value::Null);
                }
                let Ok(result) = serde_json::from_str::<Value>(result_text) else {
                    return Ok(Value::Null);
                };
                let Some(content) = result["content"].as_str() else {
                    return Ok(Value::Null);
                };
                if truthy(&result["isError"])
                    || truthy(&result["code"])
                    || !truthy(&result["canonical"])
                    || truthy(&result["canonical"]["isError"])
                    || content.encode_utf16().count() > 60_000
                    || result["canonical"]["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|block| {
                            !["text", "resource_link"]
                                .contains(&block["type"].as_str().unwrap_or(""))
                        })
                {
                    return Ok(Value::Null);
                }
                let Some(key) = key(arguments) else {
                    return Ok(Value::Null);
                };
                let now = arguments["now"].as_f64().unwrap_or(0.0);
                let mut index = 0;
                while index < self.entries.len() {
                    if self.expired(&self.entries[index], now) || self.entries[index].key == key {
                        self.remove(index);
                    } else {
                        index += 1;
                    }
                }
                while !self.entries.is_empty()
                    && (self.entries.len() >= 32 || self.bytes + result_text.len() > 262_144)
                {
                    self.remove(0);
                }
                self.bytes += result_text.len();
                self.entries.push(ObservationEntry {
                    key,
                    result_text: result_text.into(),
                    time: now,
                    captured_at: arguments["capturedAt"].as_str().unwrap_or("").into(),
                    scope: arguments["scope"].clone(),
                });
                Ok(Value::Null)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown observation cache operation.",
                400,
            )),
        }
    }
}
