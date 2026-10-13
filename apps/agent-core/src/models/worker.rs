use std::collections::HashSet;

use serde_json::{Value, json};

use super::protocol::{array, string};

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        value.exp() / (1.0 + value.exp())
    }
}
fn cpu_budget(budget: &Value, pressure: bool) -> Value {
    let mut budget = budget.clone();
    for field in [
        "activationMemoryBytes",
        "hiddenSize",
        "attentionHeads",
        "deviceId",
    ] {
        budget.as_object_mut().unwrap().remove(field);
    }
    let fallback = budget["cpuFallbackBatchBudget"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    budget.as_object_mut().unwrap().extend(fallback);
    budget["device"] = json!("cpu");
    budget["gpuMemoryBytes"] = json!(0);
    if pressure {
        budget["diagnostic"] = json!({"code": "GPU_MEMORY_INSUFFICIENT"});
    }
    budget
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_keeps_work_owned_until_settlement_and_close_never_claims_disposal() {
        let mut state = WorkerState::default();
        state.apply("models.lifecycle.worker.init", &json!({"kind": "embedding", "profile": {"requiredDevice": "dml"}, "budget": {"device": "dml"}})).unwrap();
        state
            .apply("models.lifecycle.worker.receive", &json!({"id": 7}))
            .unwrap();
        state
            .apply("models.lifecycle.worker.accepted", &json!({"id": 7}))
            .unwrap();
        state
            .apply("models.lifecycle.worker.cancel", &json!({"id": 7}))
            .unwrap();
        assert_eq!(state.active.len(), 1);
        assert_eq!(
            state
                .apply(
                    "models.lifecycle.worker.recover",
                    &json!({"allowed": true, "memoryPressure": true})
                )
                .unwrap(),
            Value::Null
        );
        state
            .apply("models.lifecycle.worker.close", &json!({}))
            .unwrap();
        assert_eq!(state.active.len(), 1);
        let settled = state
            .apply("models.lifecycle.worker.settled", &json!({"id": 7}))
            .unwrap();
        assert_eq!(settled["idle"], false);
        assert_eq!(settled["throughId"], 7);
    }
}

/// The isolated SDK process reports facts; native state owns cancellation, execution shape and qualification.
/// 隔离 SDK 进程报告真实事实；原生状态拥有取消、执行形状和资格规则，不能以关闭请求冒充 SDK 释放。
#[derive(Default)]
pub struct WorkerState {
    kind: String,
    profile: Value,
    preference: String,
    limits: Value,
    closed: bool,
    active: HashSet<u64>,
    cancelled: HashSet<u64>,
    latest: u64,
    baseline: u64,
    peak: u64,
    budget: Value,
    loaded: Value,
    backend: Value,
    measurement_at: f64,
    sample_started: bool,
    tokens: Vec<u64>,
    truncated: u64,
    audit_key: Option<String>,
    pending_audit_key: String,
    canonical: Value,
    qualified_batch: u64,
    audited_batch: u64,
}
impl WorkerState {
    fn code(&self, suffix: &str) -> String {
        format!(
            "{}_{suffix}",
            if self.kind == "embedding" {
                "EMBEDDING"
            } else {
                "RERANK"
            }
        )
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "models.lifecycle.worker.init" => {
                self.kind = string(&args["kind"]).into();
                self.profile = args["profile"].clone();
                self.preference = string(&args["preference"]).into();
                self.limits = args["limits"].clone();
                self.baseline = args["baseline"].as_u64().unwrap_or(0);
                self.budget = args["budget"].clone();
                self.qualified_batch = 1;
                self.audited_batch = 1;
                Ok(Value::Null)
            }
            "models.lifecycle.worker.status" => {
                Ok(json!({"closed": self.closed, "budget": self.budget}))
            }
            "models.lifecycle.worker.cancelled" => Ok(json!(
                self.closed || self.cancelled.contains(&args["id"].as_u64().unwrap_or(0))
            )),
            "models.lifecycle.worker.cancel" => {
                let id = args["id"].as_u64().unwrap_or(0);
                if self.active.contains(&id) {
                    self.cancelled.insert(id);
                }
                Ok(Value::Null)
            }
            "models.lifecycle.worker.close" => {
                self.closed = true;
                self.cancelled.extend(self.active.iter().copied());
                Ok(Value::Null)
            }
            "models.lifecycle.worker.receive" => {
                let id = args["id"].as_u64().unwrap_or(0);
                self.latest = if self.kind == "embedding" {
                    id
                } else {
                    self.latest.max(id)
                };
                Ok(json!({"closed": self.closed}))
            }
            "models.lifecycle.worker.accepted" => {
                self.active.insert(args["id"].as_u64().unwrap_or(0));
                Ok(Value::Null)
            }
            "models.lifecycle.worker.settled" => {
                let id = args["id"].as_u64().unwrap_or(0);
                self.active.remove(&id);
                self.cancelled.remove(&id);
                Ok(
                    json!({"idle": !self.closed && self.active.is_empty(), "throughId": self.latest}),
                )
            }
            "models.lifecycle.worker.measurement" => {
                self.peak = self.peak.max(
                    args["rss"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_sub(self.baseline),
                );
                Ok(
                    json!({"backend": args["backend"], "processId": args["processId"], "workloadMemoryDeltaBytes": self.peak, "memoryMeasurementSource": "isolated-native-process-delta"}),
                )
            }
            "models.lifecycle.worker.begin" => {
                let requested = args
                    .get("budget")
                    .filter(|budget| !budget.is_null())
                    .unwrap_or(&self.budget);
                if self.preference == "cpu" && requested["device"] != "cpu" {
                    return Err(self.code("DEVICE_PREFERENCE_CHANGED"));
                }
                let changed = args["sessionLoaded"] == true
                    && ["cpuThreads", "device", "deviceId"]
                        .iter()
                        .any(|key| requested[*key] != self.loaded[*key]);
                let adjustment = if changed {
                    json!({"boundary": "completed-request", "requiresSessionRebuild": true,
                    "reason": if ["device", "deviceId"].iter().any(|key| requested[*key] != self.loaded[*key]) { "approved-backend-changed" } else { "approved-thread-count-changed" },
                    "from": {"device": self.loaded["device"], "cpuThreads": self.loaded["cpuThreads"]},
                    "to": {"device": requested["device"], "cpuThreads": requested["cpuThreads"]}})
                } else {
                    Value::Null
                };
                self.budget = requested.clone();
                self.sample_started = false;
                self.tokens.clear();
                self.truncated = 0;
                Ok(json!({"budget": self.budget, "rebuild": changed, "adjustment": adjustment}))
            }
            "models.lifecycle.worker.loaded" => {
                let mut backend = args["backend"].clone();
                if backend["device"] == "cpu" {
                    self.budget = cpu_budget(&self.budget, false);
                    if backend["diagnostic"].is_null() && !self.budget["diagnostic"].is_null() {
                        backend["diagnostic"] = self.budget["diagnostic"].clone();
                    }
                }
                self.budget["device"] = backend["device"].clone();
                self.loaded = self.budget.clone();
                self.backend = backend.clone();
                self.audit_key = None;
                Ok(json!({"budget": self.budget, "backend": backend}))
            }
            "models.lifecycle.worker.token" => {
                let count = args["count"]
                    .as_u64()
                    .ok_or_else(|| self.code("INVALID_INPUT"))?;
                let maximum = self.profile["maxInputTokens"].as_u64().unwrap_or(512);
                if self.kind == "embedding" && count > maximum {
                    return Ok(
                        json!({"error": {"code": self.code("INPUT_TOO_LONG"), "details": {"index": self.tokens.len(), "tokenCount": count, "maxInputTokens": maximum}}}),
                    );
                }
                if count > maximum {
                    self.truncated += 1;
                }
                let count = if self.kind == "reranker" {
                    count.min(maximum)
                } else {
                    count
                };
                self.tokens.push(count);
                Ok(json!(count))
            }
            "models.lifecycle.worker.tokens" => {
                let total: u64 = self.tokens.iter().sum();
                let maximum = self.limits["maxBatchInputTokens"]
                    .as_u64()
                    .unwrap_or(131072);
                if total > maximum {
                    return Ok(
                        json!({"error": {"code": self.code("INPUT_TOO_LONG"), "details": {"totalInputTokens": total, "maxBatchInputTokens": maximum}}}),
                    );
                }
                Ok(json!({"lengths": self.tokens, "truncatedInputsCount": self.truncated}))
            }
            "models.lifecycle.worker.sample" => {
                let now = args["now"].as_f64().unwrap_or(0.0);
                let report = !self.sample_started
                    || args["progress"] == 1
                    || now - self.measurement_at >= 250.0;
                if report {
                    self.sample_started = true;
                    self.measurement_at = now;
                }
                Ok(json!(report))
            }
            "models.lifecycle.worker.vectors" => {
                let vectors = args["vectors"]
                    .as_array()
                    .ok_or_else(|| self.code("INVALID_VECTOR"))?;
                let dimensions = self.profile["dimensions"].as_u64().unwrap_or(0) as usize;
                if vectors.iter().any(|vector| {
                    vector.as_array().is_none_or(|vector| {
                        vector.len() != dimensions
                            || vector
                                .iter()
                                .any(|value| value.as_f64().is_none_or(|value| !value.is_finite()))
                    })
                }) {
                    return Err(self.code("INVALID_VECTOR"));
                }
                Ok(json!(true))
            }
            "models.lifecycle.worker.scores" => {
                let logits = args["logits"]
                    .as_array()
                    .ok_or_else(|| self.code("INVALID_RESULT"))?;
                if args["count"].as_u64() != Some(logits.len() as u64)
                    || logits
                        .iter()
                        .any(|value| value.as_f64().is_none_or(|value| !value.is_finite()))
                {
                    return Err(self.code("INVALID_RESULT"));
                }
                Ok(json!(
                    logits
                        .iter()
                        .map(|value| sigmoid(value.as_f64().unwrap()))
                        .collect::<Vec<_>>()
                ))
            }
            "models.lifecycle.worker.probe" => {
                let left = array(&args["reference"]);
                let right = array(&args["actual"]);
                if left.len() != 3
                    || right.len() != 3
                    || left
                        .iter()
                        .chain(right)
                        .any(|value| value.as_f64().is_none_or(|value| !value.is_finite()))
                {
                    return Ok(Value::Null);
                }
                let maximum = left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| {
                        (sigmoid(left.as_f64().unwrap()) - sigmoid(right.as_f64().unwrap())).abs()
                    })
                    .fold(0.0_f64, f64::max);
                if maximum > 0.05
                    || left[0].as_f64().unwrap() <= left[1].as_f64().unwrap()
                    || right[0].as_f64().unwrap() <= right[1].as_f64().unwrap()
                    || left[2].as_f64().unwrap() <= left[1].as_f64().unwrap()
                    || right[2].as_f64().unwrap() <= right[1].as_f64().unwrap()
                {
                    return Ok(Value::Null);
                }
                Ok(
                    json!({"maxProbabilityDifference": maximum, "probeRankAgreement": true, "separateProfile": true}),
                )
            }
            "models.lifecycle.worker.recover" => Ok(
                if !self.profile["requiredDevice"].is_null()
                    || args["allowed"] != true
                    || self.budget["device"] == "cpu"
                    || args["memoryPressure"] != true
                {
                    Value::Null
                } else {
                    cpu_budget(&self.budget, true)
                },
            ),
            "models.lifecycle.worker.auditBegin" => {
                let audit_key = format!(
                    "{}:{}:{}",
                    string(&self.budget["device"]),
                    self.budget["cpuThreads"],
                    self.budget["batchSize"]
                );
                self.pending_audit_key = audit_key.clone();
                let cached = self.audit_key.as_ref() == Some(&audit_key);
                if cached {
                    self.budget["batchSize"] = json!(
                        self.budget["batchSize"]
                            .as_u64()
                            .unwrap_or(1)
                            .min(self.audited_batch)
                    );
                }
                Ok(json!({"cached": cached, "budget": self.budget, "reference": self.canonical}))
            }
            "models.lifecycle.worker.auditResult" => {
                if self.canonical.is_null() {
                    self.canonical = args["reference"].clone();
                }
                if args["qualified"] == true {
                    self.qualified_batch = self.budget["batchSize"].as_u64().unwrap_or(1);
                    return Ok(Value::Null);
                }
                let current = self.budget["batchSize"].as_u64().unwrap_or(1);
                self.budget["batchSize"] = json!(if self.qualified_batch <= current {
                    self.qualified_batch
                } else {
                    1
                });
                Ok(json!(self.budget))
            }
            "models.lifecycle.worker.auditRetry" => {
                if args["qualified"] == true {
                    return Ok(Value::Null);
                }
                if self.budget["batchSize"].as_u64().unwrap_or(1) <= 1 {
                    return Err(self.code("SPACE_INCOMPATIBLE"));
                }
                self.budget["batchSize"] = json!(1);
                Ok(json!(self.budget))
            }
            "models.lifecycle.worker.auditFinish" => {
                self.audit_key = Some(self.pending_audit_key.clone());
                self.audited_batch = self.budget["batchSize"].as_u64().unwrap_or(1);
                let mut status = args["status"].clone();
                status["appliedBatchSize"] = self.budget["batchSize"].clone();
                status["embeddingSpaceId"] = self.profile["embeddingSpaceId"].clone();
                self.backend["batchCompatibility"] = status;
                Ok(json!(self.backend))
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
