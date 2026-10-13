use std::collections::HashMap;

use serde_json::{Value, json};

const MIB: u64 = 1024 * 1024;

fn limits(options: &Value) -> Result<Value, String> {
    let mut result = options.as_object().cloned().unwrap_or_default();
    for (name, default, minimum, maximum) in [
        ("maxBatchDocuments", 512, 1, 4096),
        ("maxPendingRequests", 128, 1, 1024),
        ("maxRequestBytes", 8 * MIB, 1024, 64 * MIB),
        ("maxQueuedBytes", 64 * MIB, 1024, 256 * MIB),
        ("maxQueuedEstimatedTokens", 8 * MIB, 512, 64 * MIB),
        ("maxBatchInputTokens", 262_144, 512, 2_097_152),
    ] {
        let value = match result.get(name) {
            None => default,
            Some(value) => value
                .as_u64()
                .ok_or_else(|| format!("INFERENCE_INVALID_LIMIT:{name}"))?,
        };
        if !(minimum..=maximum).contains(&value) {
            return Err(format!("INFERENCE_INVALID_LIMIT:{name}"));
        }
        result.insert(name.into(), json!(value));
    }
    if result["maxQueuedBytes"].as_u64() < result["maxRequestBytes"].as_u64() {
        return Err("INFERENCE_INVALID_QUEUE_CAPACITY".into());
    }
    Ok(Value::Object(result))
}

/// Tickets belong to native settlement, so cancelling a JS promise never frees input capacity.
/// 票据属于原生结算；取消 JS Promise 不释放输入容量，退出收据才可清空账本。
#[derive(Default)]
pub struct AdmissionState {
    limits: Value,
    effective_queued_bytes: u64,
    sequence: u64,
    tickets: HashMap<u64, (u64, u64)>,
    bytes: u64,
    estimated_tokens: u64,
}

impl AdmissionState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "resources.admission.init" => {
                if !self.limits.is_null() {
                    return Err("INFERENCE_ADMISSION_EXISTS".into());
                }
                self.limits = limits(&args["options"])?;
                self.effective_queued_bytes = self.limits["maxQueuedBytes"].as_u64().unwrap();
                Ok(self.limits.clone())
            }
            "resources.admission.budget" => {
                if let Some(memory) = args["memoryBytes"]
                    .as_f64()
                    .filter(|value| value.is_finite() && *value > 0.0)
                {
                    self.effective_queued_bytes = (memory / 8.0)
                        .floor()
                        .max(self.limits["maxRequestBytes"].as_u64().unwrap() as f64)
                        .min(self.limits["maxQueuedBytes"].as_u64().unwrap() as f64)
                        as u64;
                }
                Ok(Value::Null)
            }
            "resources.admission.reserve" => {
                let count = args["count"].as_u64().ok_or("INFERENCE_INVALID_INPUT")?;
                let input_bytes = args["inputBytes"]
                    .as_u64()
                    .ok_or("INFERENCE_INVALID_INPUT")?;
                let query_bytes = args["queryBytes"].as_u64().unwrap_or(0);
                let bytes = input_bytes
                    .checked_add(query_bytes)
                    .ok_or("INFERENCE_INPUT_BACKPRESSURE")?;
                let tokens = bytes
                    .checked_add(query_bytes.saturating_mul(count.saturating_sub(1)))
                    .ok_or("INFERENCE_INPUT_BACKPRESSURE")?;
                if count > self.limits["maxBatchDocuments"].as_u64().unwrap()
                    || bytes > self.limits["maxRequestBytes"].as_u64().unwrap()
                    || self.tickets.len() as u64
                        >= self.limits["maxPendingRequests"].as_u64().unwrap()
                    || self.bytes.saturating_add(bytes) > self.effective_queued_bytes
                    || self.estimated_tokens.saturating_add(tokens)
                        > self.limits["maxQueuedEstimatedTokens"].as_u64().unwrap()
                {
                    return Err("INFERENCE_INPUT_BACKPRESSURE".into());
                }
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or("INFERENCE_REQUEST_LIMIT")?;
                self.tickets.insert(self.sequence, (bytes, tokens));
                self.bytes += bytes;
                self.estimated_tokens += tokens;
                Ok(json!(self.sequence))
            }
            "resources.admission.release" => {
                if let Some((bytes, tokens)) = args["ticket"]
                    .as_u64()
                    .and_then(|ticket| self.tickets.remove(&ticket))
                {
                    self.bytes -= bytes;
                    self.estimated_tokens -= tokens;
                }
                Ok(Value::Null)
            }
            "resources.admission.clear" => {
                self.tickets.clear();
                self.bytes = 0;
                self.estimated_tokens = 0;
                Ok(Value::Null)
            }
            "resources.admission.status" => {
                let mut limits = self.limits.clone();
                limits["effectiveQueuedBytes"] = json!(self.effective_queued_bytes);
                Ok(
                    json!({"activeRequests": self.tickets.len(), "queuedInputBytes": self.bytes,
                    "queuedEstimatedTokens": self.estimated_tokens, "limits": limits}),
                )
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "resources.inferenceLimits" => limits(&args["options"]),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_tickets_keep_budget_until_settled_and_ignore_duplicate_receipts() {
        let mut state = AdmissionState::default();
        state
            .apply(
                "resources.admission.init",
                &json!({"options": {"maxPendingRequests": 1}}),
            )
            .unwrap();
        let ticket = state
            .apply(
                "resources.admission.reserve",
                &json!({"count": 2, "inputBytes": 1024, "queryBytes": 16}),
            )
            .unwrap();
        assert_eq!(
            state.apply(
                "resources.admission.reserve",
                &json!({"count": 1, "inputBytes": 1})
            ),
            Err("INFERENCE_INPUT_BACKPRESSURE".into())
        );
        assert_eq!(
            state
                .apply("resources.admission.status", &json!({}))
                .unwrap()["queuedEstimatedTokens"],
            1056
        );
        for _ in 0..2 {
            state
                .apply("resources.admission.release", &json!({"ticket": ticket}))
                .unwrap();
        }
        assert_eq!(
            state
                .apply("resources.admission.status", &json!({}))
                .unwrap()["activeRequests"],
            0
        );
    }
}
