use serde_json::{Value, json};

#[derive(Default)]
pub struct BatchState {
    lengths: Vec<u64>,
    order: Vec<usize>,
    offset: usize,
    active_rows: usize,
    token_budget: u64,
    activation_bytes: f64,
    hidden_size: f64,
    attention_heads: f64,
    cpu_threads: Option<u64>,
    retries: u64,
    measured_maximum_rows: usize,
    current: Vec<usize>,
}

impl BatchState {
    fn activation_cost(&self, count: usize) -> f64 {
        let sequence = self.lengths[self.order[self.offset + count - 1]] as f64;
        count as f64
            * (sequence * self.hidden_size * 4.0 * 8.0
                + sequence * sequence * self.attention_heads * 4.0)
    }
    fn has_activation_budget(&self) -> bool {
        self.activation_bytes != 0.0 && self.hidden_size != 0.0 && self.attention_heads != 0.0
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "models.lifecycle.batchInit" => {
                let invalid = || "INFERENCE_INVALID_BATCH".to_owned();
                self.lengths = args["tokenLengths"]
                    .as_array()
                    .ok_or_else(invalid)?
                    .iter()
                    .map(|value| {
                        value
                            .as_u64()
                            .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991)
                            .ok_or_else(invalid)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if Some(self.lengths.len() as u64) != args["count"].as_u64() {
                    return Err(invalid());
                }
                self.active_rows = args["batchSize"]
                    .as_u64()
                    .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991)
                    .ok_or_else(invalid)?
                    .min(128) as usize;
                self.token_budget = args["batchTokenBudget"]
                    .as_u64()
                    .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991)
                    .ok_or_else(invalid)?;
                self.order = (0..self.lengths.len()).collect();
                self.order
                    .sort_by_key(|index| (self.lengths[*index], *index));
                self.activation_bytes = args["activationMemoryBytes"].as_f64().unwrap_or(0.0);
                self.hidden_size = args["hiddenSize"].as_f64().unwrap_or(0.0);
                self.attention_heads = args["attentionHeads"].as_f64().unwrap_or(0.0);
                self.cpu_threads = args["cpuThreads"].as_u64();
                self.offset = 0;
                self.retries = 0;
                self.current.clear();
                Ok(Value::Null)
            }
            "models.lifecycle.batchNext" => {
                if self.offset == self.order.len() {
                    return Ok(Value::Null);
                }
                let mut count = self.active_rows.min(self.order.len() - self.offset);
                while count > 1
                    && (self.lengths[self.order[self.offset + count - 1]]
                        > self.lengths[self.order[self.offset]].saturating_mul(2)
                        || self.lengths[self.order[self.offset + count - 1]]
                            .saturating_mul(count as u64)
                            > self.token_budget
                        || self.has_activation_budget()
                            && self.activation_cost(count) > self.activation_bytes)
                {
                    count -= 1;
                }
                if self.lengths[self.order[self.offset]] > self.token_budget
                    || self.has_activation_budget()
                        && self.activation_cost(count) > self.activation_bytes
                {
                    return Err("INFERENCE_RESOURCE_BUSY".into());
                }
                self.current = self.order[self.offset..self.offset + count].to_vec();
                Ok(json!(self.current))
            }
            "models.lifecycle.batchSuccess" => {
                if self.current.is_empty() {
                    return Err("INFERENCE_BATCH_NOT_PENDING".into());
                }
                let count = self.current.len();
                self.offset += count;
                self.measured_maximum_rows = self.measured_maximum_rows.max(count);
                let sequence = self.lengths[*self.current.last().unwrap()];
                let tokens: u64 = self.current.iter().map(|index| self.lengths[*index]).sum();
                let padded = sequence * count as u64;
                let latency = args["latencyMs"].as_f64().unwrap_or(0.01).max(0.01);
                let mut measurement = json!({"latencyMs": latency, "throughputPerSecond": tokens as f64 * 1000.0 / latency,
                    "phase": "hot-inference", "unit": "tokens", "sequenceTokens": sequence, "inputTokens": tokens, "paddedTokens": padded,
                    "batchSize": count, "measuredMaximumBatchSize": self.measured_maximum_rows, "bucketMinimumTokens": self.lengths[self.current[0]], "paddingTokens": padded - tokens,
                    "paddingRatio": (padded - tokens) as f64 / padded as f64, "retries": self.retries,
                    "queueDepth": self.order.len() - self.offset, "progress": self.offset as f64 / self.order.len().max(1) as f64});
                if let Some(cpu) = self.cpu_threads {
                    measurement["cpuThreads"] = json!(cpu);
                }
                self.current.clear();
                Ok(measurement)
            }
            "models.lifecycle.batchPressure" => {
                if self.current.len() <= 1 || self.retries >= 5 {
                    return Ok(
                        json!({"batchSize": 1, "retries": self.retries, "code": "INFERENCE_ALLOCATION_FAILED", "terminal": true}),
                    );
                }
                self.active_rows = (self.current.len() / 2).max(1);
                self.retries += 1;
                self.current.clear();
                Ok(
                    json!({"batchSize": self.active_rows, "retries": self.retries, "code": "INFERENCE_BATCH_REDUCED"}),
                )
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_attention_and_failed_allocation_reduce_batches_without_advancing_results() {
        let mut batch = BatchState::default();
        batch.apply("models.lifecycle.batchInit", &json!({"count": 4, "tokenLengths": [512, 32, 512, 32], "batchSize": 64,
            "batchTokenBudget": 65536, "activationMemoryBytes": 20 * 1024 * 1024, "hiddenSize": 384, "attentionHeads": 12})).unwrap();
        assert_eq!(
            batch
                .apply("models.lifecycle.batchNext", &json!({}))
                .unwrap(),
            json!([1, 3])
        );
        batch
            .apply("models.lifecycle.batchSuccess", &json!({"latencyMs": 10}))
            .unwrap();
        assert_eq!(
            batch
                .apply("models.lifecycle.batchNext", &json!({}))
                .unwrap(),
            json!([0])
        );
        assert_eq!(
            batch
                .apply("models.lifecycle.batchPressure", &json!({}))
                .unwrap()["terminal"],
            true
        );
        assert_eq!(batch.offset, 2);
    }
}
