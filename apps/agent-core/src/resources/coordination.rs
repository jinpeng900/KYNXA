use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_LEASES: usize = 128;
const MAX_QUEUE: usize = 32;
const MAX_CLAIM_MS: u64 = 60_000;
const YIELD_MS: u64 = 2_000;

fn number(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}
fn safe_integer(v: &Value, min: f64, max: f64) -> bool {
    v.as_f64()
        .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= min && n <= max)
}
fn key(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}
fn bounded_put(
    map: &mut BTreeMap<String, Value>,
    order: &mut VecDeque<String>,
    id: String,
    value: Value,
) {
    order.retain(|entry| entry != &id);
    order.push_back(id.clone());
    map.insert(id, value);
    while order.len() > MAX_LEASES {
        if let Some(old) = order.pop_front() {
            map.remove(&old);
        }
    }
}

/// Preserve reservations, feedback and admission order across restarts; callers supply OS/IPC receipts.
/// 监控重启后保留隔离预约、反馈和准入顺序；系统与 IPC 回执由调用方提供。
pub struct CoordinationState {
    leases: BTreeMap<String, Value>,
    claims: BTreeMap<String, Value>,
    queue: BTreeMap<String, Value>,
    attempted: BTreeSet<String>,
    queue_sequence: u64,
    foreground_streak: u64,
    fraction: f64,
    feedback: Value,
    histories: BTreeMap<String, Value>,
    history_order: VecDeque<String>,
    controls: BTreeMap<String, Value>,
    control_order: VecDeque<String>,
    recovery_attempts: u64,
    recovery_failures: u64,
    retry_at: f64,
    recovery_reason: Option<String>,
}
impl Default for CoordinationState {
    fn default() -> Self {
        Self {
            leases: BTreeMap::new(),
            claims: BTreeMap::new(),
            queue: BTreeMap::new(),
            attempted: BTreeSet::new(),
            queue_sequence: 0,
            foreground_streak: 0,
            fraction: 1.0,
            feedback: json!({"throughputPerSecond":null,"latencyMs":null,"queueDepth":null,"lastAdjustmentMs":0,"reports":0}),
            histories: BTreeMap::new(),
            history_order: VecDeque::new(),
            controls: BTreeMap::new(),
            control_order: VecDeque::new(),
            recovery_attempts: 0,
            recovery_failures: 0,
            retry_at: 0.0,
            recovery_reason: None,
        }
    }
}
impl CoordinationState {
    fn reserved(&self) -> Value {
        let mut cpu = 0.0;
        let mut memory = 0.0;
        let mut gpu = 0.0;
        for lease in self.leases.values() {
            cpu += number(&lease["cpuThreads"]);
            memory += number(&lease["memoryBytes"]);
            gpu += number(&lease["gpuMemoryBytes"]);
        }
        json!({"cpuThreads":cpu,"memoryBytes":memory,"gpuMemoryBytes":gpu})
    }
    fn ordered(&self, now: f64) -> Vec<String> {
        let priority = |v: &Value| {
            if v["request"]["kind"] == "background"
                && (now - number(&v["queuedAt"]) >= YIELD_MS as f64 || self.foreground_streak >= 3)
            {
                0
            } else if v["request"]["kind"] == "foreground" {
                1
            } else {
                2
            }
        };
        let mut entries: Vec<_> = self.queue.iter().collect();
        entries.sort_by(|(_, a), (_, b)| {
            priority(a)
                .cmp(&priority(b))
                .then_with(|| number(&a["queuedAt"]).total_cmp(&number(&b["queuedAt"])))
                .then_with(|| number(&a["ordinal"]).total_cmp(&number(&b["ordinal"])))
        });
        entries.into_iter().map(|(id, _)| id.clone()).collect()
    }
    pub fn apply(&mut self, op: &str, args: &Value) -> Result<Value, String> {
        let now = number(&args["now"]);
        Ok(match op.trim_start_matches("resources.coordination.") {
            "validateAllocation" => {
                let result = &args["result"];
                let request = &args["request"];
                json!(
                    result["leaseId"].is_string()
                        && safe_integer(&result["cpuThreads"], 0.0, number(&request["cpuThreads"]))
                        && result["memoryBytes"].as_f64() == request["memoryBytes"].as_f64()
                        && result["gpuMemoryBytes"].as_f64() == request["gpuMemoryBytes"].as_f64()
                        && safe_integer(
                            &result["expiresAt"],
                            -9_007_199_254_740_991.0,
                            9_007_199_254_740_991.0
                        )
                        && (number(&request["gpuMemoryBytes"]) <= 0.0
                            || (result["mode"] == "rust"
                                && result["device"] == "gpu"
                                && result["deviceId"].as_f64() == Some(0.0)
                                && (result.get("executionProvider").is_none()
                                    || (result["executionProvider"] == "dml"
                                        && safe_integer(
                                            &result["executionDeviceId"],
                                            0.0,
                                            15.0
                                        )))))
                )
            }
            "validateTtl" => json!(safe_integer(&args["ttlMs"], 1000.0, 300_000.0)),
            "validateExecutor" => json!(
                safe_integer(&args["processId"], 1.0, u32::MAX as f64)
                    && (args.get("startTimeMs").is_none()
                        || safe_integer(&args["startTimeMs"], 0.0, 9_007_199_254_740_991.0))
            ),
            "validateHealth" => json!(
                args["result"]["mode"] == "rust"
                    && safe_integer(
                        &args["result"]["cpu"]["logicalCores"],
                        -9_007_199_254_740_991.0,
                        9_007_199_254_740_991.0
                    )
                    && safe_integer(
                        &args["result"]["memory"]["availableBytes"],
                        -9_007_199_254_740_991.0,
                        9_007_199_254_740_991.0
                    )
            ),
            "leases" => json!(self.leases.values().collect::<Vec<_>>()),
            "lease" => self
                .leases
                .get(&key(&args["id"]))
                .cloned()
                .unwrap_or(Value::Null),
            "put" => {
                let lease = args["lease"].clone();
                let id = key(&lease["leaseId"]);
                if id.is_empty()
                    || !self.leases.contains_key(&id) && self.leases.len() >= MAX_LEASES
                {
                    return Err("RESOURCE_LEASE_LIMIT".into());
                }
                self.leases.insert(id, lease);
                Value::Null
            }
            "patch" => {
                if let Some(lease) = self.leases.get_mut(&key(&args["id"])) {
                    if let Some(fields) = args["changes"].as_object() {
                        for (k, v) in fields {
                            lease[k] = v.clone();
                        }
                    }
                }
                Value::Null
            }
            "drop" => json!(self.leases.remove(&key(&args["id"])).is_some()),
            "restored" => {
                for lease in self.leases.values_mut() {
                    lease["mode"] = json!("rust");
                }
                Value::Null
            }
            "reserved" => self.reserved(),
            "counts" => json!({"leases":self.leases.len(),"queue":self.queue.len()}),
            "claim" => {
                if self.claims.len() >= 4096 {
                    return Err("RESOURCE_QUEUE_FULL".into());
                }
                self.claims.insert(
                    key(&args["id"]),
                    json!({"reason":args["reason"],"startedAt":now}),
                );
                Value::Null
            }
            "releaseClaim" => {
                let removed = self.claims.remove(&key(&args["id"])).is_some();
                json!({"removed":removed,"resume":removed&&self.claims.is_empty()})
            }
            "priority" => {
                json!({"pendingForegroundAdmissions":self.claims.values().filter(|claim|claim["reason"]=="pending-admission").count(),
                "protectedTaskAdmissions":self.claims.values().filter(|claim|claim["reason"]=="task-admission").count(),"yieldingBackgroundRequests":args["waiters"],
                "maximumBackgroundYieldMs":YIELD_MS,"maximumClaimMs":MAX_CLAIM_MS,
                "oldestClaimStartedAt":self.claims.values().map(|claim|number(&claim["startedAt"])).min_by(f64::total_cmp)})
            }
            "yield" => {
                let request = &args["request"];
                if request["kind"] != "background" || self.claims.is_empty() {
                    json!({"waitMs":request["waitMs"],"yieldedMs":0})
                } else if number(&request["waitMs"]) == 0.0 {
                    json!({"denial":{"status":"denied","reason":"RESOURCE_PRESSURE","mode":args["mode"],"admissionPriority":{"reason":"foreground-admission-pending","retryable":true}}})
                } else if number(&args["waiters"]) >= MAX_QUEUE as f64 {
                    json!({"denial":{"status":"denied","reason":"RESOURCE_QUEUE_FULL","mode":args["mode"]}})
                } else {
                    json!({"wait":true,"timerMs":number(&request["waitMs"]).min(YIELD_MS as f64)})
                }
            }
            "yieldResume" => {
                let elapsed = number(&args["elapsed"]).ceil().max(0.0);
                let wait = number(&args["waitMs"]);
                if elapsed >= wait {
                    json!({"denial":{"status":"denied","reason":"RESOURCE_WAIT_TIMEOUT","mode":args["mode"],"foregroundYieldMs":elapsed}})
                } else {
                    json!({"waitMs":wait-elapsed,"yieldedMs":elapsed})
                }
            }
            "grant" => {
                let request = &args["request"];
                let control = self.controls.get(&(key(&request["taskId"]) + "|cpu"));
                let mut result = super::fallback::calculate(
                    "resources.fallbackGrant",
                    &json!({"request":request,"hardware":args["hardware"],"reserved":self.reserved(),
                    "leasesCount":self.leases.len(),"policyFraction":control.map(|v|number(&v["fraction"])).unwrap_or(self.fraction),"feedback":self.feedback}),
                )?;
                if result["status"] == "granted" {
                    result["leaseId"] = args["id"].clone();
                    result["expiresAt"] = json!(now + number(&request["ttlMs"]));
                    let mut lease = result.clone();
                    lease["taskId"] = request["taskId"].clone();
                    self.leases.insert(key(&args["id"]), lease);
                }
                result
            }
            "report" => {
                let lease = self
                    .leases
                    .get(&key(&args["id"]))
                    .ok_or("RESOURCE_LEASE_UNKNOWN")?;
                let identity = super::fallback::calculate(
                    "resources.feedbackIdentity",
                    &json!({"taskId":lease["taskId"],"feedback":args["feedback"]}),
                )?;
                let control_key = key(&identity["controlKey"]);
                let history_key = key(&identity["historyKey"]);
                let mut input = json!({"feedback":args["feedback"],"identity":identity,"now":now,"previous":self.feedback});
                if let Some(control) = self.controls.get(&control_key) {
                    input["control"] = control.clone();
                }
                if let Some(history) = self.histories.get(&history_key) {
                    input["history"] = history.clone();
                }
                let result = super::fallback::calculate("resources.feedbackUpdate", &input)?;
                if identity["backend"] == "legacy" {
                    self.fraction = number(&result["control"]["fraction"]);
                }
                self.feedback = result["feedback"].clone();
                bounded_put(
                    &mut self.controls,
                    &mut self.control_order,
                    control_key,
                    result["control"].clone(),
                );
                bounded_put(
                    &mut self.histories,
                    &mut self.history_order,
                    history_key,
                    result["history"].clone(),
                );
                let mut feedback = self.feedback.clone();
                feedback["backgroundFraction"] = result["control"]["fraction"].clone();
                json!({"status":"reported","feedback":feedback})
            }
            "queue" => {
                if self.queue.len() >= MAX_QUEUE {
                    json!(false)
                } else {
                    self.queue_sequence = self.queue_sequence.saturating_add(1);
                    self.queue.insert(key(&args["id"]),json!({"request":args["request"],"queuedAt":now,"ordinal":self.queue_sequence}));
                    json!(true)
                }
            }
            "unqueue" => {
                self.attempted.remove(&key(&args["id"]));
                json!(self.queue.remove(&key(&args["id"])).is_some())
            }
            "drainBegin" => {
                self.attempted.clear();
                Value::Null
            }
            "drainNext" => {
                let next = self
                    .ordered(now)
                    .into_iter()
                    .find(|id| !self.attempted.contains(id));
                if let Some(id) = next {
                    self.attempted.insert(id.clone());
                    json!(id)
                } else {
                    Value::Null
                }
            }
            "drainSettled" => {
                if args["granted"] == true {
                    if let Some(waiter) = self.queue.remove(&key(&args["id"])) {
                        self.foreground_streak = if waiter["request"]["kind"] == "foreground" {
                            self.foreground_streak.saturating_add(1)
                        } else {
                            0
                        };
                    }
                    self.attempted.clear();
                }
                Value::Null
            }
            "recoveredQueue" => {
                let plan=self.ordered(now).into_iter().filter_map(|id|self.queue.get(&id).map(|waiter|json!({"id":id,"waitMs":(number(&waiter["request"]["waitMs"])-(now-number(&waiter["queuedAt"]))).max(0.0)}))).collect::<Vec<_>>();
                self.queue.clear();
                self.attempted.clear();
                json!(plan)
            }
            "recovery" => {
                json!({"attempts":self.recovery_attempts,"consecutiveFailures":self.recovery_failures,"retryAt":self.retry_at,"reason":self.recovery_reason})
            }
            "recoveryAttempt" => {
                self.recovery_attempts = self.recovery_attempts.saturating_add(1);
                Value::Null
            }
            "recoveryFailed" => {
                self.recovery_failures = self.recovery_failures.saturating_add(1);
                self.retry_at = now
                    + (1000u64
                        .saturating_mul(1u64 << self.recovery_failures.saturating_sub(1).min(5)))
                    .min(30_000) as f64;
                self.recovery_reason = Some(
                    args["reason"]
                        .as_str()
                        .unwrap_or("RESOURCE_RESTORE_FAILED")
                        .to_owned(),
                );
                Value::Null
            }
            "retired" => {
                self.retry_at = self.retry_at.max(now + 1000.0);
                self.recovery_reason = Some(key(&args["code"]));
                Value::Null
            }
            "reconciled" => {
                self.recovery_failures = 0;
                self.retry_at = 0.0;
                self.recovery_reason = None;
                Value::Null
            }
            "snapshot" => {
                let hardware = &args["hardware"];
                let reserved = self.reserved();
                let capacity = super::fallback::calculate(
                    "resources.capacity",
                    &json!({"hardware":hardware,"workload":"compute"}),
                )?;
                let transport = super::fallback::calculate(
                    "resources.capacity",
                    &json!({"hardware":hardware,"workload":"model-transport"}),
                )?;
                let memory = number(&reserved["memoryBytes"]);
                let gpu = number(&reserved["gpuMemoryBytes"]);
                let active = self
                    .leases
                    .values()
                    .filter(|lease| number(&lease["expiresAt"]) > now && lease["mode"] != "rust")
                    .count();
                let mut budget = capacity.clone();
                budget["gpuMemoryBytes"] = Value::Null;
                budget["reservedCpuThreads"] = reserved["cpuThreads"].clone();
                budget["reservedMemoryBytes"] = reserved["memoryBytes"].clone();
                budget["reservedGpuMemoryBytes"] = reserved["gpuMemoryBytes"].clone();
                let mut feedback = self.feedback.clone();
                feedback["backgroundFraction"] = json!(self.fraction);
                json!({"budget":budget,"accounting":{"observedMaterializedMemoryBytes":0,"unmaterializedMemoryBytes":memory,"capacityMemoryBytes":capacity["memoryBytes"],
                    "maximumResidentReservationBytes":(number(&hardware["memory"]["totalBytes"])*0.4).floor(),"availableMemoryBytes":(number(&capacity["memoryBytes"])-memory).max(0.0),
                    "availableTransportMemoryBytes":(number(&transport["memoryBytes"])-memory).max(0.0),"gpuMemoryState":"unverified-reservation","gpuUnverifiedReservationBytes":gpu,
                    "observedMaterializedGpuMemoryBytes":0,"availableGpuMemoryBytes":null,"gpuAttributionExact":false,"state":"monitor-unavailable-reservations-quarantined"},
                    "activeLeases":active,"quarantinedLeases":self.leases.len()-active,"maxLeases":MAX_LEASES,"sampledAt":now,"queuedRequests":self.queue.len(),"feedback":feedback,
                    "executors":{"measurementState":"unknown","reason":"RESOURCE_NATIVE_MONITOR_UNAVAILABLE","registeredLeases":self.leases.values().filter(|lease|lease.get("executor").is_some()).count(),"processes":[]}})
            }
            "clear" => {
                self.leases.clear();
                self.claims.clear();
                self.queue.clear();
                Value::Null
            }
            _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn apply(state: &mut CoordinationState, op: &str, args: Value) -> Value {
        state
            .apply(&("resources.coordination.".to_owned() + op), &args)
            .unwrap()
    }
    #[test]
    fn expired_debt_stays_fenced_and_a_new_owner_cannot_inherit_it() {
        let mut state = CoordinationState::default();
        apply(
            &mut state,
            "put",
            json!({"lease":{"leaseId":"owned","mode":"fallback","cpuThreads":4,"memoryBytes":1024,"gpuMemoryBytes":0,"expiresAt":1}}),
        );
        let hardware = json!({"cpu":{"logicalCores":8,"usagePercent":10},"memory":{"totalBytes":17179869184u64,"availableBytes":8589934592u64}});
        let snapshot = apply(
            &mut state,
            "snapshot",
            json!({"hardware":hardware,"now":1000}),
        );
        assert_eq!(snapshot["activeLeases"], 0);
        assert_eq!(snapshot["quarantinedLeases"], 1);
        let request = json!({"taskId":"new","kind":"background","workload":"compute","cpuThreads":1,"memoryBytes":0,"gpuMemoryBytes":0,"ttlMs":1000});
        assert_eq!(
            apply(
                &mut state,
                "grant",
                json!({"request":request,"hardware":hardware,"now":1000,"id":"new"})
            )["reason"],
            "RESOURCE_PRESSURE"
        );
        assert_eq!(
            apply(&mut CoordinationState::default(), "counts", json!({}))["leases"],
            0
        );
        apply(&mut state, "drop", json!({"id":"owned"}));
        assert_eq!(
            apply(
                &mut state,
                "grant",
                json!({"request":request,"hardware":hardware,"now":1000,"id":"new"})
            )["status"],
            "granted"
        );
    }
    #[test]
    fn queue_fairness_preserves_fifo_and_foreground_claims_never_clear_held_leases() {
        let mut state = CoordinationState::default();
        for (id, kind) in [
            ("second-lexically", "background"),
            ("first-lexically", "background"),
            ("foreground", "foreground"),
        ] {
            apply(
                &mut state,
                "queue",
                json!({"id":id,"request":{"kind":kind,"waitMs":5000},"now":100}),
            );
        }
        apply(&mut state, "drainBegin", json!({}));
        assert_eq!(
            apply(&mut state, "drainNext", json!({"now":101})),
            "foreground"
        );
        apply(&mut state, "drainBegin", json!({}));
        assert_eq!(
            apply(&mut state, "drainNext", json!({"now":2100})),
            "second-lexically"
        );
        apply(
            &mut state,
            "claim",
            json!({"id":"task","reason":"task-admission","now":2000}),
        );
        assert_eq!(
            apply(
                &mut state,
                "yield",
                json!({"request":{"kind":"background","waitMs":0},"mode":"fallback","waiters":0})
            )["denial"]["reason"],
            "RESOURCE_PRESSURE"
        );
        assert_eq!(
            apply(&mut state, "releaseClaim", json!({"id":"task"}))["resume"],
            true
        );
        assert_eq!(
            apply(&mut state, "releaseClaim", json!({"id":"task"}))["resume"],
            false
        );
    }
}
