use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

const RETRY_WINDOW_MS: f64 = 60000.0;
const RUNTIME_BYTES: f64 = 96.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

type AnnResult = Result<Value, Value>;
fn error(code: &str, message: &str, status: u16) -> Value {
    json!({"code":code,"message":message,"statusCode":status})
}
fn number(value: &Value, fallback: f64) -> f64 {
    value.as_f64().unwrap_or(fallback)
}
fn vector_bytes(descriptor: &Value, connectivity: f64) -> f64 {
    number(&descriptor["count"], 0.0)
        * (number(&descriptor["dimensions"], 0.0) * 4.0 + connectivity * 16.0 + 256.0)
}

fn retry(previous: Option<&Value>, now: f64, code: &str, descriptor: Option<&Value>) -> Value {
    let window = previous
        .filter(|previous| now - number(&previous["windowStartedAt"], 0.0) < RETRY_WINDOW_MS)
        .map(|previous| number(&previous["windowStartedAt"], now))
        .unwrap_or(now);
    let attempts = previous
        .filter(|previous| number(&previous["windowStartedAt"], f64::NAN) == window)
        .map(|previous| number(&previous["attempts"], 0.0) + 1.0)
        .unwrap_or(1.0);
    let retry_at = if attempts >= 3.0 {
        window + RETRY_WINDOW_MS
    } else {
        now + 1000.0 * 2_f64.powf(attempts - 1.0)
    };
    let mut result =
        json!({"attempts":attempts,"windowStartedAt":window,"retryAt":retry_at,"code":code});
    if let Some(descriptor) = descriptor {
        result["descriptor"] = descriptor.clone();
    }
    result
}

fn same_build(left: &Value, right: &Value) -> bool {
    ["generation", "count", "buildBudgetKey"]
        .iter()
        .all(|field| left[*field] == right[*field])
}

struct BuildJob {
    key: String,
    descriptor: Value,
    state: &'static str,
    cancelled: bool,
    completed_vectors: f64,
}

impl BuildJob {
    fn value(&self, id: u64) -> Value {
        json!({"id":id,"key":self.key,"descriptor":self.descriptor,"scopeKey":self.descriptor["scope_key"],"generation":self.descriptor["generation"],"totalVectors":self.descriptor["count"],"completedVectors":self.completed_vectors,"state":self.state,"cancelled":self.cancelled})
    }
}

/// Background scheduling owns generations, capacity, retries and completion records independently of runtime promises.
/// 后台调度独立于运行时 Promise，拥有版本、容量、重试与完成记录。
#[derive(Default)]
struct BuildQueue {
    records: HashMap<u64, BuildJob>,
    pending: HashMap<String, u64>,
    order: VecDeque<String>,
    active: HashMap<String, u64>,
    active_order: VecDeque<u64>,
    failures: HashMap<String, Value>,
    failure_order: VecDeque<String>,
    sequence: u64,
    concurrency: usize,
    closed: bool,
    draining: bool,
    completed: usize,
    failed: usize,
    last_error: Option<String>,
}

impl BuildQueue {
    fn failure(&self, key: &str, descriptor: &Value) -> Option<&Value> {
        self.failures
            .get(key)
            .filter(|failure| same_build(&failure["descriptor"], descriptor))
    }
    fn should_schedule(&self) -> bool {
        !self.closed
            && !self.draining
            && self.active.len() < self.concurrency
            && !self.pending.is_empty()
    }
    fn ids(&self, active: bool) -> Vec<u64> {
        if active {
            self.active_order
                .iter()
                .filter(|id| self.records.contains_key(id))
                .copied()
                .collect()
        } else {
            self.order
                .iter()
                .filter_map(|key| self.pending.get(key).copied())
                .collect()
        }
    }
    fn remove_pending(&mut self, key: &str) {
        self.pending.remove(key);
        self.order.retain(|entry| entry != key);
    }
    fn cancel_scopes(&mut self, scopes: &[Value]) -> Vec<u64> {
        let mut ids = Vec::new();
        for id in self.ids(true).into_iter().chain(self.ids(false)) {
            if !ids.contains(&id)
                && self
                    .records
                    .get(&id)
                    .is_some_and(|job| scopes.contains(&job.descriptor["scope_key"]))
            {
                ids.push(id);
            }
        }
        for id in &ids {
            let Some(job) = self.records.get_mut(id) else {
                continue;
            };
            job.cancelled = true;
            let key = job.key.clone();
            let queued = job.state == "queued";
            if queued && self.pending.get(&key) == Some(id) {
                self.remove_pending(&key);
                self.records.remove(id);
            }
        }
        ids
    }
    fn status(&self, now: f64) -> Value {
        let mut ids = self.ids(false);
        for id in self.ids(true) {
            if !ids.contains(&id) {
                ids.insert(0, id);
            }
        }
        json!({"pendingBuilds":ids.len(),"activeBuilds":self.active.len(),"buildConcurrency":self.concurrency,"maximumQueuedBuilds":64,
            "completedBuilds":self.completed,"failedBuilds":self.failed,"retryingBuilds":self.failures.values().filter(|failure|number(&failure["retryAt"],0.0)>now).count(),
            "buildErrorCode":self.last_error,"buildProgress":ids.iter().filter_map(|id|self.records.get(id)).map(|job|json!({"scopeKey":job.descriptor["scope_key"],"generation":job.descriptor["generation"],"state":if job.cancelled{"cancelling"}else{job.state},"totalVectors":job.descriptor["count"],"completedVectors":job.completed_vectors})).collect::<Vec<_>>()})
    }
    fn apply(&mut self, operation: &str, arguments: &Value) -> AnnResult {
        match operation {
            "queue.init" => {
                self.concurrency = arguments["concurrency"].as_u64().unwrap_or(1) as usize;
                Ok(Value::Null)
            }
            "queue.configure" => {
                self.concurrency = arguments["concurrency"]
                    .as_u64()
                    .filter(|value| *value <= 9_007_199_254_740_991)
                    .map(|value| value.clamp(1, 4) as usize)
                    .unwrap_or(1);
                Ok(Value::Null)
            }
            "queue.shouldSchedule" => Ok(json!(self.should_schedule())),
            "queue.pending" => Ok(json!(self.ids(false))),
            "queue.active" => Ok(json!(self.ids(true))),
            "queue.get" => Ok(arguments["id"]
                .as_u64()
                .and_then(|id| self.records.get(&id).map(|job| job.value(id)))
                .unwrap_or(Value::Null)),
            "queue.failure" => Ok(self
                .failure(string(&arguments["key"]), &arguments["descriptor"])
                .cloned()
                .unwrap_or(Value::Null)),
            "queue.failureDiagnostics" => Ok(json!(self.failure_order.iter().filter_map(|key| {
                self.failures.get(key).map(|failure| json!({"key":key,"code":failure["code"],"attempts":failure["attempts"],"retryAt":failure["retryAt"]}))
            }).collect::<Vec<_>>())),
            "queue.enqueue" => {
                if self.closed {
                    return Ok(json!({"accepted":false}));
                }
                let key = string(&arguments["key"]);
                let descriptor = &arguments["descriptor"];
                let now = number(&arguments["now"], 0.0);
                let previous = self.pending.get(key).copied();
                if previous
                    .and_then(|id| self.records.get(&id))
                    .is_some_and(|job| !job.cancelled && same_build(&job.descriptor, descriptor))
                {
                    return Ok(json!({"accepted":true}));
                }
                if self
                    .failure(key, descriptor)
                    .is_some_and(|failure| now < number(&failure["retryAt"], 0.0))
                {
                    return Ok(json!({"accepted":false}));
                }
                if previous.is_none() && self.pending.len() >= 64 {
                    return Err(error(
                        "RETRIEVAL_ANN_QUEUE_FULL",
                        "ANN build queue is full. / 向量建图队列已满。",
                        400,
                    ));
                }
                let mut cancel = Vec::new();
                if let Some(id) = previous {
                    cancel.push(id);
                    if let Some(job) = self.records.get_mut(&id) {
                        job.cancelled = true;
                    }
                    if !self.active.values().any(|active| *active == id) {
                        self.records.remove(&id);
                    }
                }
                self.sequence += 1;
                let id = self.sequence;
                let job = BuildJob {
                    key: key.into(),
                    descriptor: descriptor.clone(),
                    state: "queued",
                    cancelled: false,
                    completed_vectors: 0.0,
                };
                let value = job.value(id);
                self.records.insert(id, job);
                if previous.is_none() {
                    self.order.push_back(key.into());
                }
                self.pending.insert(key.into(), id);
                Ok(json!({"accepted":true,"job":value,"cancel":cancel}))
            }
            "queue.start" => {
                if !self.should_schedule() {
                    return Ok(Value::Null);
                }
                let id = self
                    .order
                    .iter()
                    .filter_map(|key| self.pending.get(key))
                    .find(|id| {
                        self.records.get(id).is_some_and(|job| {
                            job.state == "queued" && !self.active.contains_key(&job.key)
                        })
                    })
                    .copied();
                let Some(id) = id else {
                    return Ok(Value::Null);
                };
                let job = self.records.get_mut(&id).unwrap();
                job.state = "building";
                self.active.insert(job.key.clone(), id);
                self.active_order.push_back(id);
                Ok(job.value(id))
            }
            "queue.patch" => {
                if let Some(job) = arguments["id"]
                    .as_u64()
                    .and_then(|id| self.records.get_mut(&id))
                {
                    if let Some(changes) = arguments["descriptor"].as_object() {
                        for (key, value) in changes {
                            job.descriptor[key] = value.clone();
                        }
                    }
                    if arguments.get("completedVectors").is_some() {
                        job.completed_vectors = number(&arguments["completedVectors"], 0.0);
                    }
                    if arguments["cancelled"] == true {
                        job.cancelled = true;
                    }
                }
                Ok(Value::Null)
            }
            "queue.finish" => {
                let Some(id) = arguments["id"].as_u64() else {
                    return Ok(Value::Null);
                };
                let Some(job) = self.records.remove(&id) else {
                    return Ok(Value::Null);
                };
                if !job.cancelled && arguments["cancelled"] != true {
                    if arguments["succeeded"] == true {
                        self.completed += 1;
                        self.last_error = None;
                        self.failures.remove(&job.key);
                        self.failure_order.retain(|key| *key != job.key);
                    } else if arguments["aborted"] != true {
                        self.failed += 1;
                        let code = arguments["code"]
                            .as_str()
                            .unwrap_or("RETRIEVAL_ANN_BUILD_FAILED");
                        self.last_error = Some(code.into());
                        let failure = retry(
                            self.failure(&job.key, &job.descriptor),
                            number(&arguments["now"], 0.0),
                            code,
                            Some(&job.descriptor),
                        );
                        self.failures.insert(job.key.clone(), failure);
                        self.failure_order.retain(|key| *key != job.key);
                        self.failure_order.push_back(job.key.clone());
                        while self.failures.len() > 64 {
                            if let Some(key) = self.failure_order.pop_front() {
                                self.failures.remove(&key);
                            }
                        }
                    }
                }
                if self.pending.get(&job.key) == Some(&id) {
                    self.remove_pending(&job.key);
                }
                if self.active.get(&job.key) == Some(&id) {
                    self.active.remove(&job.key);
                }
                self.active_order.retain(|active| *active != id);
                Ok(Value::Null)
            }
            "queue.cancelScopes" => Ok(json!(self.cancel_scopes(array(&arguments["scopes"])))),
            "queue.drain" | "queue.close" => {
                self.draining = true;
                if operation == "queue.close" {
                    self.closed = true;
                }
                let scopes = self
                    .records
                    .values()
                    .map(|job| job.descriptor["scope_key"].clone())
                    .collect::<Vec<_>>();
                Ok(json!(self.cancel_scopes(&scopes)))
            }
            "queue.drained" => {
                self.draining = false;
                Ok(Value::Null)
            }
            "queue.status" => Ok(self.status(number(&arguments["now"], 0.0))),
            _ => Err(error(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown ANN queue operation.",
                400,
            )),
        }
    }
}

#[derive(Default)]
struct BackgroundBatch {
    pending: VecDeque<(u64, u64)>,
    current: Option<(u64, u64)>,
    next_id: u64,
    completed: u64,
}

#[derive(Default)]
struct AnnStore {
    fields: HashMap<String, Value>,
    shards: HashMap<String, Value>,
    shard_order: VecDeque<String>,
    prepared: HashMap<String, Value>,
    prepared_order: VecDeque<String>,
    sequence: u64,
    batches: HashMap<u64, BackgroundBatch>,
}

/// The shared native session exposes only operation receipts; runtime objects and vector buffers stay outside state.
/// 共用原生会话仅暴露操作回执；运行时对象与向量缓冲不进入控制状态。
#[derive(Default)]
pub struct AnnSession {
    queue: BuildQueue,
    store: AnnStore,
    vector_costs: Vec<(String, String, f64)>,
}

impl AnnSession {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> Result<Value, String> {
        let operation = operation
            .strip_prefix("retrieval.ann.")
            .ok_or("AGENT_CORE_UNKNOWN_OPERATION")?;
        if operation == "batch" {
            let operations = array(&arguments["operations"]);
            if operations.len() > 64 {
                return Err("AGENT_CORE_INVALID_INPUT".to_owned());
            }
            let mut results = Vec::with_capacity(operations.len());
            for item in operations {
                let name = string(&item["operation"]);
                if name == "batch"
                    || !(name.starts_with("store.")
                        || name.starts_with("queue.")
                        || name.starts_with("vector."))
                {
                    return Err("AGENT_CORE_INVALID_INPUT".to_owned());
                }
                let result = self.apply(&format!("retrieval.ann.{name}"), &item["arguments"])?;
                if result.get("error").is_some() {
                    return Ok(result);
                }
                results.push(result["value"].clone());
            }
            return Ok(json!({"value":results}));
        }
        if operation == "runtime.snapshot" {
            let mut result = self.store.status(arguments);
            let queue = self
                .queue
                .apply("queue.status", &json!({"now":arguments["now"]}))
                .map_err(|_| "AGENT_CORE_INVALID_INPUT")?;
            if let (Some(result), Some(queue)) = (result.as_object_mut(), queue.as_object()) {
                result.extend(queue.clone());
            }
            return Ok(json!({"value":result}));
        }
        if operation == "query.prepare" {
            let jobs: Vec<Value> = self
                .queue
                .ids(false)
                .iter()
                .filter_map(|id| self.queue.records.get(id))
                .map(|job| json!({"descriptor":job.descriptor,"cancelled":job.cancelled}))
                .collect();
            let budget = self.store.apply("store.budget", &json!({"options":arguments["options"],"jobs":jobs,"hasChild":arguments["hasChild"]}));
            let result = budget.and_then(|budget| {
                let entry = self.store.apply("store.entry", arguments)?;
                let plan = self.store.apply(
                    "store.searchPlan",
                    &json!({"entry":entry,"hasBuilds":!self.queue.active.is_empty()}),
                )?;
                Ok(json!({"budget":budget,"entry":entry,"plan":plan}))
            });
            return Ok(match result {
                Ok(value) => json!({"value":value}),
                Err(error) => json!({"error":error}),
            });
        }
        let result = if operation.starts_with("queue.") {
            self.queue.apply(operation, arguments)
        } else if operation.starts_with("vector.") {
            self.vector(operation, arguments)
        } else {
            self.store.apply(operation, arguments)
        };
        Ok(match result {
            Ok(value) => json!({"value":value}),
            Err(error) => json!({"error":error}),
        })
    }
    fn vector(&mut self, operation: &str, arguments: &Value) -> AnnResult {
        let cost_key = |descriptor: &Value| {
            serde_json::to_string(&json!([
                arguments["epoch"],
                descriptor["scope_key"],
                descriptor["embedding_profile_id"],
                descriptor["dimensions"],
                descriptor["embedding_model_version"],
                descriptor["embedding_space_id"],
                descriptor["domain"],
                descriptor["generation"]
            ]))
            .unwrap()
        };
        match operation {
            "vector.recordExactCost" => {
                let descriptor = &arguments["descriptor"];
                let count = number(&descriptor["count"], 0.0);
                let elapsed = number(&arguments["elapsedMs"], 0.0);
                if count == 0.0 || elapsed <= 0.0 {
                    return Ok(Value::Null);
                }
                let key = cost_key(descriptor);
                let previous = self
                    .vector_costs
                    .iter()
                    .find(|(existing, _, _)| existing == &key)
                    .map(|(_, _, value)| *value);
                self.vector_costs
                    .retain(|(existing, _, _)| existing != &key);
                self.vector_costs.push((
                    key,
                    string(&descriptor["scope_key"]).into(),
                    previous
                        .map(|cost| cost * 0.8 + elapsed / count * 0.2)
                        .unwrap_or(elapsed / count),
                ));
                while self.vector_costs.len() > 64 {
                    self.vector_costs.remove(0);
                }
                Ok(Value::Null)
            }
            "vector.invalidate" => {
                self.vector_costs
                    .retain(|(_, scope, _)| !array(&arguments["scopes"]).contains(&json!(scope)));
                Ok(Value::Null)
            }
            "vector.costCount" => Ok(json!(self.vector_costs.len())),
            "vector.clear" => {
                self.vector_costs.clear();
                Ok(Value::Null)
            }
            "vector.planning" => {
                let routing = self.vector("vector.routing", arguments)?;
                let options = &routing["options"];
                let descriptors = array(&arguments["descriptors"]);
                let admission = options["mode"] == "ann"
                    || options["mode"] == "auto"
                        && descriptors.iter().any(|descriptor| {
                            number(&descriptor["count"], 0.0)
                                > number(&options["threshold"], 50000.0)
                        });
                let configured = arguments
                    .get("configuredOptions")
                    .unwrap_or(&arguments["options"]);
                let parts = array(&arguments["parts"]);
                let audit = if arguments["hasAdmission"] == true {
                    let mut audit = arguments["admissionAudit"]
                        .as_object()
                        .cloned()
                        .map(Value::Object)
                        .unwrap_or(json!({}));
                    audit["configured"] = json!({"maxShardBytes":configured["maxShardBytes"],"maxCachedShards":configured["maxCachedShards"],"threshold":configured["threshold"]});
                    audit["approved"] = json!({"maxShardBytes":options["maxShardBytes"],"maxCachedShards":options["maxCachedShards"],"threshold":options["threshold"]});
                    audit["originalDescriptors"] = json!(descriptors.len());
                    audit["plannedShards"] = json!(parts.len());
                    audit["largestShardBytes"] = json!(
                        parts
                            .iter()
                            .map(|descriptor| vector_bytes(
                                descriptor,
                                number(&options["connectivity"], 0.0)
                            ))
                            .fold(0.0, f64::max)
                    );
                    audit
                } else {
                    json!({"configured":{"maxShardBytes":configured["maxShardBytes"],"threshold":configured["threshold"]},"approved":null,"reason":"exact-route-no-ann-admission"})
                };
                Ok(
                    json!({"needsAnnAdmission":admission,"options":options,"routingAudit":routing["audit"],"audit":audit}),
                )
            }
            "vector.routing" => {
                let options = &arguments["options"];
                let configured = number(&options["threshold"], 50000.0);
                let mut threshold = configured;
                let mut reason = "configured-threshold";
                let descriptors = array(&arguments["descriptors"]);
                let costs = descriptors
                    .iter()
                    .filter_map(|descriptor| {
                        let key = cost_key(descriptor);
                        self.vector_costs
                            .iter()
                            .find(|(existing, _, _)| existing == &key)
                            .map(|(_, _, cost)| *cost)
                            .filter(|cost| *cost > 0.0)
                    })
                    .collect::<Vec<_>>();
                if options["mode"] == "auto" && options["adaptive"] == true && configured == 50000.0
                {
                    if let Some(cost) = costs.iter().copied().reduce(f64::max) {
                        threshold = (25.0 / cost).floor().clamp(1024.0, 200000.0);
                        reason = "observed-exact-cost";
                    }
                    if arguments["approvedCapacity"] == true && !descriptors.is_empty() {
                        let vector_bytes = descriptors
                            .iter()
                            .map(|descriptor| {
                                number(&descriptor["dimensions"], 0.0) * 4.0
                                    + number(&options["connectivity"], 0.0) * 16.0
                                    + 256.0
                            })
                            .reduce(f64::max)
                            .unwrap_or(1.0);
                        let capacity =
                            (number(&options["maxShardBytes"], 0.0) / vector_bytes / 2.0)
                                .floor()
                                .max(1024.0);
                        if capacity < threshold {
                            threshold = capacity;
                            reason = "approved-shard-capacity";
                        }
                    }
                }
                let mut effective = options.clone();
                effective["threshold"] = json!(threshold);
                Ok(
                    json!({"options":effective,"audit":{"configuredThreshold":options["threshold"],"effectiveThreshold":threshold,"reason":reason,"exactTargetLatencyMs":25,"measuredCorpora":costs.len(),"thresholdBounds":{"minimum":1024,"maximum":200000},"explicitThresholdPreserved":configured!=50000.0}}),
                )
            }
            _ => Err(error(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown vector routing operation.",
                400,
            )),
        }
    }
}

impl AnnStore {
    fn field(&self, name: &str) -> Value {
        self.fields.get(name).cloned().unwrap_or(Value::Null)
    }
    fn set(&mut self, name: &str, value: Value) {
        self.fields.insert(name.into(), value);
    }
    fn counter(&mut self, name: &str, count: f64) {
        let mut counters = self.field("counters");
        counters[name] = json!(number(&counters[name], 0.0) + count);
        self.set("counters", counters);
    }
    fn worker_guard(&self, now: f64) -> AnnResult {
        let failure = self.field("nativeFailure");
        let retry = self.field("nativeRetry");
        if !failure.is_null() && now < number(&retry["retryAt"], 0.0) {
            Err(failure)
        } else {
            Ok(Value::Null)
        }
    }
    fn entries(&self) -> Vec<&Value> {
        self.shard_order
            .iter()
            .filter_map(|key| self.shards.get(key))
            .collect()
    }
    fn bytes(entry: &Value) -> f64 {
        number(&entry["count"], 0.0)
            * (number(&entry["identity"]["dimensions"], 0.0) * 4.0
                + number(&entry["identity"]["connectivity"], 0.0) * 16.0
                + 256.0)
    }
    fn identity(&mut self, arguments: &Value) -> Value {
        let descriptor = &arguments["descriptor"];
        let options = &arguments["options"];
        let mut fields = vec![
            ("schemaVersion", json!(1)),
            ("packageVersion", json!("2.26.4")),
            ("epoch", self.field("epoch")),
            ("scopeKey", descriptor["scope_key"].clone()),
            ("profileId", descriptor["embedding_profile_id"].clone()),
            ("dimensions", descriptor["dimensions"].clone()),
            (
                "modelVersion",
                descriptor["embedding_model_version"].clone(),
            ),
            ("spaceId", descriptor["embedding_space_id"].clone()),
            ("domain", descriptor["domain"].clone()),
        ];
        if descriptor["partitioned"] == true {
            for (name, field) in [("minimumId", "minimumId"), ("maximumId", "maximumId")] {
                if let Some(value) = descriptor.get(field) {
                    fields.push((name, value.clone()));
                }
            }
        }
        fields.extend([
            ("metric", json!("cos")),
            ("quantization", json!("f32")),
            ("connectivity", options["connectivity"].clone()),
            ("expansionAdd", options["expansionAdd"].clone()),
            ("expansionSearch", options["expansionSearch"].clone()),
        ]);
        let text = format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(key, value)| format!("{}:{}", serde_json::to_string(key).unwrap(), value))
                .collect::<Vec<_>>()
                .join(",")
        );
        let identity: Value = serde_json::from_str(&text).unwrap();
        let key = format!("{:x}", Sha256::digest(text.as_bytes()));
        self.sequence += 1;
        json!({"entryId":self.sequence,"key":key,"identity":identity,"identityOrder":fields.iter().map(|(key,_)|*key).collect::<Vec<_>>(),"descriptor":descriptor,"options":options,"generation":descriptor["generation"],"count":descriptor["count"],"dirty":false})
    }
    fn effective_options(&self, options: &Value) -> Value {
        let adaptive = self.field("adaptiveOptions");
        let defaults = self.field("defaults");
        let mut options = options.clone();
        if options["adaptive"] == true && !adaptive.is_null() {
            for name in ["maxShardBytes", "maxCachedShards"] {
                options[name] = if options[name] == defaults[name] {
                    adaptive[name].clone()
                } else {
                    json!(number(&options[name], 0.0).min(number(&adaptive[name], 0.0)))
                };
            }
        }
        options
    }
    fn resource_admission(&mut self, arguments: &Value) -> AnnResult {
        let snapshot = &arguments["snapshot"];
        let options = &arguments["options"];
        let descriptor = &arguments["descriptor"];
        let isolated = self.field("isolatedBuilder") == true;
        let available = [
            &snapshot["hardware"]["memory"]["availableBytes"],
            &snapshot["memory"]["availableBytes"],
            &snapshot["budget"]["memoryBytes"],
        ]
        .into_iter()
        .find_map(Value::as_f64)
        .unwrap_or(512.0 * MIB);
        let estimate = vector_bytes(descriptor, number(&options["connectivity"], 0.0));
        let capacity = [
            &snapshot["accounting"]["availableMemoryBytes"],
            &snapshot["budget"]["memoryBytes"],
        ]
        .into_iter()
        .find_map(Value::as_f64)
        .unwrap_or(available);
        let target = if isolated {
            estimate + RUNTIME_BYTES
        } else {
            (estimate * 2.0).max(available / 8.0)
        };
        let memory = (2.0 * 1024.0 * MIB)
            .min(available / 3.0)
            .min(capacity / if isolated { 1.0 } else { 2.0 })
            .min((128.0 * MIB).max(target))
            .floor();
        self.set("resourceAdmissionAudit",json!({"requestedMemoryBytes":memory,"availableCapacityBytes":capacity,"memoryAdmission":"full-request-or-denial","configured":{"maxShardBytes":options["maxShardBytes"],"maxCachedShards":options["maxCachedShards"]},"reason":"awaiting-resource-admission"}));
        if memory < RUNTIME_BYTES + MIB {
            return Err(error(
                "RETRIEVAL_ANN_RESOURCE_LIMIT",
                "ANN runtime headroom is unavailable. / 向量运行时余量不足。",
                400,
            ));
        }
        Ok(json!(memory))
    }
    fn resource_options(&mut self, arguments: &Value) -> AnnResult {
        let lease = &arguments["lease"];
        if arguments["validateGranted"] == true && lease["status"] != "granted" {
            return Err(error(
                "RETRIEVAL_ANN_RESOURCE_LIMIT",
                "ANN is waiting for resource capacity. / 向量图等待可用资源。",
                400,
            ));
        }
        let options = &arguments["options"];
        if options["adaptive"] != true {
            return Ok(options.clone());
        }
        let descriptor = &arguments["descriptor"];
        let defaults = self.field("defaults");
        let memory = number(&lease["memoryBytes"], 0.0);
        let cache = MIB.max(memory - RUNTIME_BYTES);
        let shard = MIB.max((1024.0 * MIB).min(
            cache
                / if self.field("isolatedBuilder") == true {
                    1.0
                } else {
                    2.0
                },
        ));
        let max_shard = if options["maxShardBytes"] == defaults["maxShardBytes"] {
            shard.floor()
        } else {
            number(&options["maxShardBytes"], 0.0).min(cache.floor())
        };
        let estimated = MIB.max(vector_bytes(
            descriptor,
            number(&options["connectivity"], 0.0),
        ));
        let max_cached = if options["maxCachedShards"] == defaults["maxCachedShards"] {
            (cache / estimated).floor().clamp(1.0, 16.0)
        } else {
            number(&options["maxCachedShards"], 0.0).min((cache / max_shard).floor().max(1.0))
        };
        let mut result = options.clone();
        result["maxShardBytes"] = json!(max_shard);
        result["maxCachedShards"] = json!(max_cached);
        self.set("adaptiveOptions", result.clone());
        let mut audit = self.field("resourceAdmissionAudit");
        if !audit.is_object() {
            audit = json!({});
        }
        audit["approvedMemoryBytes"] = json!(memory);
        audit["cacheBytes"] = json!(cache);
        audit["approved"] = json!({"maxShardBytes":max_shard,"maxCachedShards":max_cached});
        audit["reason"] = json!("approved-resident-capacity");
        self.set("resourceAdmissionAudit", audit);
        Ok(result)
    }
    fn status(&self, arguments: &Value) -> Value {
        let failure = self.field("nativeFailure");
        let counters = self.field("counters");
        let cached_vectors = self
            .shards
            .values()
            .map(|entry| number(&entry["count"], 0.0))
            .fold(0.0, |total, count| total + count);
        let mut result = json!({"backend":"usearch","packageVersion":"2.26.4","state":if !failure.is_null(){"unavailable"}else if arguments["hasChild"]==true{"ready"}else{"idle"},
            "cachedShards":self.shards.len(),"cachedVectors":cached_vectors,"helperPid":arguments["helperPid"],"helperRssBytes":self.field("helperMemory")["rssBytes"],"helperObservedPeakRssBytes":self.field("helperMemory")["observedPeakRssBytes"],
            "backgroundObservedPeakRssBytes":self.field("backgroundObservedPeakRssBytes"),"adaptiveOptions":self.field("adaptiveOptions"),"planningAudit":self.field("planningAudit"),"resourceAdmissionAudit":self.field("resourceAdmissionAudit"),
            "nativeRetryAt":if failure.is_null(){Value::Null}else{self.field("nativeRetry")["retryAt"].clone()},"errorCode":if failure.is_null(){self.field("lastError")}else{failure["code"].clone()}});
        if let Some(counters) = counters.as_object() {
            for (key, value) in counters {
                result[key] = value.clone();
            }
        }
        result
    }
    fn apply(&mut self, operation: &str, arguments: &Value) -> AnnResult {
        match operation {
            "store.batchStart" => {
                let batch_id = arguments["batchId"].as_u64().ok_or_else(|| {
                    error(
                        "INVALID_RETRIEVAL_ANN",
                        "Invalid ANN batch. / 向量批次身份无效。",
                        400,
                    )
                })?;
                let count = arguments["count"]
                    .as_u64()
                    .filter(|count| *count > 0)
                    .ok_or_else(|| {
                        error(
                            "INVALID_RETRIEVAL_ANN",
                            "Invalid ANN count. / 向量批次数量无效。",
                            400,
                        )
                    })?;
                if self.batches.len() >= 64 || self.batches.contains_key(&batch_id) {
                    return Err(error(
                        "RETRIEVAL_ANN_BUILD_QUEUE_FULL",
                        "ANN batch already exists or queue is full. / 向量批次重复或队列已满。",
                        503,
                    ));
                }
                self.batches.insert(
                    batch_id,
                    BackgroundBatch {
                        pending: VecDeque::from([(1, count)]),
                        next_id: 2,
                        ..BackgroundBatch::default()
                    },
                );
                Ok(json!(1))
            }
            "store.batchEnd" => {
                if let Some(id) = arguments["batchId"].as_u64() {
                    self.batches.remove(&id);
                }
                Ok(Value::Null)
            }
            "store.batchNext" | "store.batchPartition" | "store.batchComplete" => {
                let batch = arguments["batchId"]
                    .as_u64()
                    .and_then(|id| self.batches.get_mut(&id))
                    .ok_or_else(|| {
                        error(
                            "INVALID_RETRIEVAL_ANN",
                            "ANN batch is closed. / 向量批次已关闭。",
                            409,
                        )
                    })?;
                if operation == "store.batchNext" {
                    if batch.current.is_some() {
                        return Err(error(
                            "INVALID_RETRIEVAL_ANN",
                            "ANN batch has unfinished work. / 向量批次仍有未结算分片。",
                            409,
                        ));
                    }
                    let next = batch.pending.pop_front();
                    batch.current = next;
                    return Ok(next
                        .map(
                            |(id, _)| json!({"descriptorId":id,"completedVectors":batch.completed}),
                        )
                        .unwrap_or(Value::Null));
                }
                let (_, count) = batch.current.ok_or_else(|| {
                    error(
                        "INVALID_RETRIEVAL_ANN",
                        "ANN batch has no active shard. / 向量批次缺少当前分片。",
                        409,
                    )
                })?;
                if operation == "store.batchComplete" {
                    batch.completed = batch.completed.checked_add(count).ok_or_else(|| {
                        error(
                            "INVALID_RETRIEVAL_ANN",
                            "ANN count overflow. / 向量数量超限。",
                            400,
                        )
                    })?;
                    batch.current = None;
                    return Ok(json!(batch.completed));
                }
                let parts = arguments["parts"]
                    .as_array()
                    .filter(|parts| !parts.is_empty())
                    .ok_or_else(|| {
                        error(
                            "INVALID_RETRIEVAL_ANN",
                            "Invalid ANN partitions. / 向量分片无效。",
                            400,
                        )
                    })?;
                let counts: Vec<u64> = parts
                    .iter()
                    .map(|part| {
                        part["count"]
                            .as_u64()
                            .filter(|count| *count > 0)
                            .ok_or_else(|| {
                                error(
                                    "INVALID_RETRIEVAL_ANN",
                                    "Invalid ANN partition count. / 向量分片数量无效。",
                                    400,
                                )
                            })
                    })
                    .collect::<Result<_, _>>()?;
                let total = counts
                    .iter()
                    .try_fold(0u64, |total, count| total.checked_add(*count));
                if total != Some(count) {
                    return Err(error(
                        "RETRIEVAL_ANN_STALE_BUILD",
                        "ANN partition counts changed. / 向量分片数量已变化。",
                        409,
                    ));
                }
                let mut ids = Vec::new();
                if counts.len() == 1 {
                    return Ok(Value::Null);
                }
                for count in counts {
                    let id = batch.next_id;
                    batch.next_id = batch.next_id.checked_add(1).ok_or_else(|| {
                        error(
                            "INVALID_RETRIEVAL_ANN",
                            "ANN partition identities overflow. / 向量分片身份超限。",
                            400,
                        )
                    })?;
                    ids.push((id, count));
                }
                // The current shard is replaced in order; Node keeps only the opaque-ID-to-SDK-object mapping.
                // 按顺序替换当前分片；Node 仅保留不透明身份到 SDK 对象的映射，不保存队列或进度。
                for part in ids.iter().rev() {
                    batch.pending.push_front(*part);
                }
                batch.current = None;
                Ok(json!(ids.iter().map(|(id, _)| *id).collect::<Vec<_>>()))
            }
            "store.init" => {
                if !["chunks", "retrieval_vector_rows"].contains(&string(&arguments["vectorTable"]))
                {
                    return Err(error(
                        "INVALID_RETRIEVAL_ARGUMENTS",
                        "Invalid vector view. / 向量视图无效。",
                        400,
                    ));
                }
                if arguments["requestTimeoutMs"]
                    .as_u64()
                    .is_none_or(|value| !(1..=30000).contains(&value))
                {
                    return Err(error(
                        "INVALID_RETRIEVAL_ANN",
                        "Invalid ANN timeout. / 向量计算期限无效。",
                        400,
                    ));
                }
                self.set("epoch", arguments["epoch"].clone());
                self.set("isolatedBuilder", arguments["isolatedBuilder"].clone());
                self.set("defaults", arguments["defaults"].clone());
                self.set("closed", json!(false));
                self.set(
                    "helperMemory",
                    json!({"rssBytes":0,"observedPeakRssBytes":0}),
                );
                self.set("backgroundObservedPeakRssBytes", json!(0));
                self.set(
                    "nativeRetry",
                    json!({"attempts":0,"windowStartedAt":0,"retryAt":0}),
                );
                self.set("counters",json!({"built":0,"loaded":0,"updated":0,"invalidated":0,"cacheMisses":0,"recycled":0}));
                Ok(Value::Null)
            }
            "store.get" => Ok(if let Some(field) = arguments["counter"].as_str() {
                self.field("counters")[field].clone()
            } else {
                self.field(string(&arguments["field"]))
            }),
            "store.set" => {
                if let Some(counter) = arguments["counter"].as_str() {
                    let mut counters = self.field("counters");
                    counters[counter] = arguments["value"].clone();
                    self.set("counters", counters);
                } else {
                    self.set(string(&arguments["field"]), arguments["value"].clone());
                }
                Ok(arguments["value"].clone())
            }
            "store.counterAdd" => {
                self.counter(
                    string(&arguments["counter"]),
                    number(&arguments["count"], 1.0),
                );
                Ok(Value::Null)
            }
            "store.entry" | "store.identity" => Ok(self.identity(arguments)),
            "store.cacheSet" => {
                let entry = &arguments["entry"];
                let key = string(&arguments["key"]);
                if !self.shards.contains_key(key) {
                    self.shard_order.push_back(key.into());
                }
                self.shards.insert(key.into(), entry.clone());
                Ok(Value::Null)
            }
            "store.cacheUpdate" => {
                if let Some(entry) = self.shards.get_mut(string(&arguments["key"])) {
                    if entry["entryId"] == arguments["entryId"] {
                        if let Some(fields) = arguments["fields"].as_object() {
                            for (key, value) in fields {
                                entry[key] = value.clone();
                            }
                        }
                    }
                }
                Ok(Value::Null)
            }
            "store.cacheDelete" => {
                let key = string(&arguments["key"]);
                let removed = self.shards.remove(key).is_some();
                self.shard_order.retain(|entry| entry != key);
                Ok(json!(removed))
            }
            "store.cacheClear" => {
                self.shards.clear();
                self.shard_order.clear();
                Ok(Value::Null)
            }
            "store.preparedSet" => {
                let key = string(&arguments["key"]);
                if !self.prepared.contains_key(key) {
                    self.prepared_order.push_back(key.into());
                }
                self.prepared
                    .insert(key.into(), arguments["generation"].clone());
                while self.prepared.len() > 128 {
                    if let Some(key) = self.prepared_order.pop_front() {
                        self.prepared.remove(&key);
                    }
                }
                Ok(Value::Null)
            }
            "store.preparedGet" => Ok(self
                .prepared
                .get(string(&arguments["key"]))
                .cloned()
                .unwrap_or(Value::Null)),
            "store.preparedDelete" => {
                let key = string(&arguments["key"]);
                let removed = self.prepared.remove(key).is_some();
                self.prepared_order.retain(|entry| entry != key);
                Ok(json!(removed))
            }
            "store.preparedClear" => {
                self.prepared.clear();
                self.prepared_order.clear();
                Ok(Value::Null)
            }
            "store.preparedEntries" => Ok(json!(
                self.prepared_order
                    .iter()
                    .filter_map(|key| self
                        .prepared
                        .get(key)
                        .map(|generation| json!([key, generation])))
                    .collect::<Vec<_>>()
            )),
            "store.workerFailed" => {
                let mut failure = arguments["failure"].clone();
                if !failure.is_object() {
                    failure = json!({});
                }
                if failure["code"].is_null() {
                    failure["code"] = json!("RETRIEVAL_ANN_UNAVAILABLE");
                }
                let previous = self.field("nativeRetry");
                self.set(
                    "nativeRetry",
                    retry(
                        (number(&previous["attempts"], 0.0) > 0.0).then_some(&previous),
                        number(&arguments["now"], 0.0),
                        string(&failure["code"]),
                        None,
                    ),
                );
                self.set("nativeFailure", failure.clone());
                self.shards.clear();
                self.shard_order.clear();
                Ok(failure)
            }
            "store.workerRecovered" => {
                self.set("nativeFailure", Value::Null);
                Ok(Value::Null)
            }
            "store.workerGuard" => self.worker_guard(number(&arguments["now"], 0.0)),
            "store.workerAdmission" => {
                if arguments["hasChild"] == true {
                    return Ok(Value::Null);
                }
                if arguments["retiring"] == true {
                    return Err(error(
                        "RETRIEVAL_ANN_RETIREMENT_PENDING",
                        "ANN helper is retiring. / 向量进程正在退出。",
                        400,
                    ));
                }
                if self.field("closed") == true {
                    return Err(error(
                        "RETRIEVAL_INDEX_CLOSED",
                        "ANN store is closed. / 向量索引已关闭。",
                        409,
                    ));
                }
                if arguments["hasResources"] == true && arguments["hasLease"] != true {
                    return Err(error(
                        "RETRIEVAL_ANN_RESOURCE_LIMIT",
                        "ANN memory has not been reserved. / 尚未取得向量进程内存预约。",
                        400,
                    ));
                }
                self.worker_guard(number(&arguments["now"], 0.0))
            }
            "store.observeMemory" => {
                if let (Some(rss), Some(peak)) = (
                    arguments["memory"]["rssBytes"].as_u64(),
                    arguments["memory"]["observedPeakRssBytes"].as_u64(),
                ) {
                    if rss <= 9_007_199_254_740_991 && peak <= 9_007_199_254_740_991 {
                        let previous = self.field("helperMemory");
                        self.set("helperMemory",json!({"rssBytes":rss,"observedPeakRssBytes":number(&previous["observedPeakRssBytes"],0.0).max(peak as f64)}));
                    }
                }
                Ok(Value::Null)
            }
            "store.retiredMemory" => {
                let mut memory = self.field("helperMemory");
                memory["rssBytes"] = json!(0);
                self.set("helperMemory", memory);
                Ok(Value::Null)
            }
            "store.requestAdmission" => {
                if number(&arguments["pending"], 0.0) >= 32.0 {
                    return Err(error(
                        "RETRIEVAL_ANN_QUEUE_FULL",
                        "ANN helper queue is full. / 向量计算队列已满。",
                        400,
                    ));
                }
                Ok(Value::Null)
            }
            "store.assertSave" => {
                if arguments["ownerMatches"] != true
                    || arguments["entry"]["generation"] != arguments["generation"]
                    || arguments["entry"]["count"] != arguments["count"]
                    || arguments["authoritativeGeneration"] != arguments["generation"]
                {
                    return Err(error(
                        "RETRIEVAL_ANN_STALE_BUILD",
                        "ANN cache changed during save. / 向量缓存在保存期间已变化。",
                        400,
                    ));
                }
                Ok(Value::Null)
            }
            "store.loadedMatches" => Ok(json!(
                arguments["loaded"]["count"] == arguments["count"]
                    && arguments["loaded"]["dimensions"] == arguments["dimensions"]
            )),
            "store.checkCount" => {
                if arguments["completedVectors"] != arguments["count"] {
                    return Err(error(
                        "RETRIEVAL_ANN_STALE_BUILD",
                        "ANN build changed during publication. / 向量建图发布时数量已变化。",
                        400,
                    ));
                }
                Ok(Value::Null)
            }
            "store.planningPrepare" => {
                let options = &arguments["options"];
                let descriptors = array(&arguments["descriptors"]);
                if arguments["hasResources"] != true
                    || options["adaptive"] != true
                    || descriptors.is_empty()
                {
                    return Ok(Value::Null);
                }
                let connectivity = number(&options["connectivity"], 0.0);
                let mut largest = &descriptors[0];
                for descriptor in &descriptors[1..] {
                    if vector_bytes(descriptor, connectivity) > vector_bytes(largest, connectivity)
                    {
                        largest = descriptor;
                    }
                }
                let per_vector =
                    number(&largest["dimensions"], 0.0) * 4.0 + connectivity * 16.0 + 256.0;
                let mut descriptor = largest.clone();
                descriptor["count"] = json!(
                    number(&largest["count"], 0.0).min(
                        (number(&options["maxShardBytes"], 0.0) / per_vector)
                            .floor()
                            .max(1.0)
                    )
                );
                Ok(descriptor)
            }
            "store.planningComplete" => {
                let mut options = arguments["options"].clone();
                let limit = self.field("buildShardLimit");
                if number(&arguments["now"], 0.0) < number(&limit["expiresAt"], 0.0) {
                    options["maxShardBytes"] = json!(
                        number(&options["maxShardBytes"], 0.0).min(number(&limit["bytes"], 0.0))
                    );
                }
                self.set("adaptiveOptions", options.clone());
                Ok(options)
            }
            "store.resourcePrepare" => {
                self.worker_guard(number(&arguments["now"], 0.0))?;
                self.set("resourceContext",json!({"options":arguments["options"],"descriptor":arguments["descriptor"],"kind":arguments["kind"]}));
                Ok(json!(
                    arguments["planning"] != true
                        && arguments["hasLease"] == true
                        && vector_bytes(
                            &arguments["descriptor"],
                            number(&arguments["options"]["connectivity"], 0.0)
                        ) + RUNTIME_BYTES
                            > number(&arguments["leaseMemoryBytes"], 0.0)
                ))
            }
            "store.resourceAdmission" => self.resource_admission(arguments),
            "store.resourceOptions" => self.resource_options(arguments),
            "store.partition" => {
                let options = &arguments["options"];
                let descriptor = &arguments["descriptor"];
                let per_vector = number(&descriptor["dimensions"], 0.0) * 4.0
                    + number(&options["connectivity"], 0.0) * 16.0
                    + 256.0;
                let per_shard = (number(&options["maxShardBytes"], 0.0) / per_vector)
                    .floor()
                    .max(1.0);
                let count = number(&descriptor["count"], 0.0);
                Ok(if options["adaptive"] == true && count > per_shard {
                    json!((count / per_shard).ceil())
                } else {
                    Value::Null
                })
            }
            "store.partitionResult" => Ok(json!(
                array(&arguments["parts"])
                    .iter()
                    .map(|part| {
                        let mut descriptor = arguments["descriptor"].clone();
                        if let Some(fields) = part.as_object() {
                            for (key, value) in fields {
                                descriptor[key] = value.clone();
                            }
                        }
                        descriptor["partitioned"] = json!(true);
                        descriptor
                    })
                    .collect::<Vec<_>>()
            )),
            "store.budget" => {
                let options = self.effective_options(&arguments["options"]);
                let maximum = number(&options["maxShardBytes"], 0.0);
                let count = number(&options["maxCachedShards"], 0.0);
                let incompatible = array(&arguments["jobs"]).iter().any(|job| {
                    let descriptor = &job["descriptor"];
                    let largest = descriptor["buildLargestShardBytes"]
                        .as_f64()
                        .unwrap_or_else(|| {
                            vector_bytes(
                                descriptor,
                                number(
                                    &descriptor["buildConnectivity"],
                                    number(&options["connectivity"], 0.0),
                                ),
                            )
                        });
                    largest > maximum
                        || number(&descriptor["buildMaxShardBytes"], 0.0) > maximum
                        || number(&descriptor["buildMaxCachedShards"], 0.0) > count
                });
                let unused = ["off", "exact"].contains(&string(&options["mode"]))
                    && (!self.shards.is_empty()
                        || !array(&arguments["jobs"]).is_empty()
                        || arguments["hasChild"] == true);
                let enforce = unused
                    || incompatible
                    || self.shards.len() as f64 > count
                    || self
                        .shards
                        .values()
                        .any(|entry| Self::bytes(entry) > maximum);
                Ok(json!({"options":options,"enforce":enforce}))
            }
            "store.preserve" => Ok(json!(
                self.entries()
                    .into_iter()
                    .filter(|entry| Self::bytes(entry)
                        <= number(&arguments["options"]["maxShardBytes"], 0.0))
                    .rev()
                    .take(number(&arguments["options"]["maxCachedShards"], 0.0) as usize)
                    .map(|entry| entry["key"].clone())
                    .collect::<Vec<_>>()
            )),
            "store.evict" => Ok(json!(
                self.shards.len() + usize::from(arguments["incoming"] != false)
                    > number(&arguments["options"]["maxCachedShards"], 0.0) as usize
            )),
            "store.residentBudget" => {
                let options = &arguments["options"];
                let budget = arguments["leaseMemoryBytes"].as_f64().unwrap_or(
                    128.0 * MIB
                        + number(&options["maxCachedShards"], 0.0)
                            * number(&options["maxShardBytes"], 0.0),
                );
                Ok(json!(
                    number(&self.field("helperMemory")["rssBytes"], 0.0) > budget
                ))
            }
            "store.warm" => {
                let entry = &arguments["entry"];
                let options = &entry["options"];
                let descriptor = &entry["descriptor"];
                Ok(json!(
                    self.field("closed") != true
                        && !["off", "exact"].contains(&string(&options["mode"]))
                        && number(&descriptor["count"], 0.0)
                            > 1024_f64.max(number(&options["threshold"], 0.0))
                        && (arguments["hasResources"] == true
                            || vector_bytes(descriptor, number(&options["connectivity"], 0.0))
                                <= number(&options["maxShardBytes"], 0.0))
                        && self.prepared.get(string(&entry["key"])) != Some(&entry["generation"])
                        && self
                            .shards
                            .get(string(&entry["key"]))
                            .is_none_or(|resident| resident["generation"] != entry["generation"])
                ))
            }
            "store.enqueue" => {
                self.worker_guard(number(&arguments["now"], 0.0))?;
                let entry = &arguments["entry"];
                let descriptor = &entry["descriptor"];
                let options = &entry["options"];
                let covering = array(&arguments["jobs"]).iter().any(|job| {
                    let candidate = &job["descriptor"];
                    job["cancelled"] != true
                        && candidate["buildConnectivity"] == options["connectivity"]
                        && candidate["buildExpansionAdd"] == options["expansionAdd"]
                        && candidate["buildExpansionSearch"] == options["expansionSearch"]
                        && [
                            "scope_key",
                            "embedding_profile_id",
                            "dimensions",
                            "embedding_model_version",
                            "embedding_space_id",
                            "domain",
                            "generation",
                        ]
                        .iter()
                        .all(|field| candidate[*field] == descriptor[*field])
                        && number(&candidate["minimumId"], 0.0)
                            <= number(&descriptor["minimumId"], 0.0)
                        && number(&candidate["maximumId"], 9_007_199_254_740_991.0)
                            >= number(&descriptor["maximumId"], 9_007_199_254_740_991.0)
                });
                if covering {
                    return Ok(Value::Null);
                }
                let mut build = descriptor.clone();
                let budget_key = format!(
                    "[{},{}]",
                    options["maxShardBytes"], options["maxCachedShards"]
                );
                build["buildBudgetKey"] = json!(budget_key);
                for (target, source) in [
                    ("buildMaxShardBytes", "maxShardBytes"),
                    ("buildMaxCachedShards", "maxCachedShards"),
                    ("buildConnectivity", "connectivity"),
                    ("buildExpansionAdd", "expansionAdd"),
                    ("buildExpansionSearch", "expansionSearch"),
                ] {
                    build[target] = options[source].clone();
                }
                Ok(build)
            }
            "store.reducedBuild" => {
                let options = &arguments["options"];
                self.set("buildShardLimit",json!({"bytes":options["maxShardBytes"],"expiresAt":number(&arguments["now"],0.0)+30000.0}));
                self.set("adaptiveOptions", options.clone());
                let largest = array(&arguments["parts"])
                    .iter()
                    .map(|part| vector_bytes(part, number(&options["connectivity"], 0.0)))
                    .fold(0.0, f64::max);
                Ok(
                    json!({"buildMaxShardBytes":options["maxShardBytes"],"buildMaxCachedShards":options["maxCachedShards"],"buildLargestShardBytes":largest}),
                )
            }
            "store.backgroundCompleted" => {
                self.counter("built", number(&arguments["built"], 0.0));
                self.set(
                    "backgroundObservedPeakRssBytes",
                    json!(
                        number(&self.field("backgroundObservedPeakRssBytes"), 0.0)
                            .max(number(&arguments["peak"], 0.0))
                    ),
                );
                Ok(Value::Null)
            }
            "store.searchPlan" => {
                let entry = &arguments["entry"];
                let descriptor = &entry["descriptor"];
                let options = &entry["options"];
                if vector_bytes(descriptor, number(&options["connectivity"], 0.0))
                    > number(&options["maxShardBytes"], 0.0)
                {
                    return Err(error(
                        "RETRIEVAL_ANN_RESOURCE_LIMIT",
                        "ANN shard exceeds its memory budget. / 向量分片超过内存预算。",
                        400,
                    ));
                }
                let key = string(&entry["key"]);
                if let Some(resident) = self.shards.get(key) {
                    return Ok(
                        json!({"kind":if resident["generation"]==descriptor["generation"]{"ready"}else{"stale"}}),
                    );
                }
                self.counter("cacheMisses", 1.0);
                let background = (number(&descriptor["count"], 0.0)
                    > 1024_f64.max(number(&options["threshold"], 0.0))
                    || arguments["hasBuilds"] == true)
                    && self.prepared.get(key) != Some(&descriptor["generation"]);
                Ok(json!({"kind":if background{"background"}else{"prepare"}}))
            }
            "store.searchCurrent" => {
                let entry = &arguments["entry"];
                if self
                    .shards
                    .get(string(&entry["key"]))
                    .is_none_or(|resident| resident["entryId"] != entry["entryId"])
                {
                    return Err(error(
                        "RETRIEVAL_ANN_BUILD_PENDING",
                        "ANN graph was replaced. / 向量图已被替换。",
                        400,
                    ));
                }
                Ok(json!(
                    number(&arguments["limit"], 0.0).min(number(&entry["count"], 0.0))
                ))
            }
            "store.searchDispatch" => {
                // Validate and update one request-scoped metadata projection; no cross-query JS ledger is cached.
                // 核验并更新本次请求的元信息投影，不在 JS 缓存跨查询业务账本。
                self.apply("store.assertBuild", arguments)?;
                let limit = self.apply("store.searchCurrent", arguments)?;
                let mut entry = arguments["entry"].clone();
                entry["options"] = arguments["options"].clone();
                let key = string(&entry["key"]).to_owned();
                self.shards.insert(key.clone(), entry);
                self.shard_order.retain(|existing| existing != &key);
                self.shard_order.push_back(key);
                let evict = self.apply(
                    "store.evict",
                    &json!({"options":arguments["options"],"incoming":false}),
                )?;
                Ok(json!({"limit":limit,"evict":evict}))
            }
            "store.assertBuild" => {
                let entry = &arguments["entry"];
                let replaced = arguments["hasOwner"] == true && arguments["ownerMatches"] != true;
                let failure = self.field("nativeFailure");
                if replaced && !failure.is_null() {
                    return Err(failure);
                }
                if self.field("closed") == true
                    || replaced
                    || arguments["generation"] != entry["generation"]
                {
                    return Err(if arguments["hasSignal"] == true {
                        json!({"name":"AbortError","code":"ABORT_ERR","message":"ANN build superseded. / 向量建图版本已被替换。"})
                    } else {
                        error(
                            "RETRIEVAL_ANN_STALE_BUILD",
                            "ANN graph was replaced. / 向量图已被替换。",
                            409,
                        )
                    });
                }
                Ok(Value::Null)
            }
            "store.updateGuard" | "store.updateCommit" => {
                let key = string(&arguments["key"]);
                let valid = self.shards.get(key).is_some_and(|entry| {
                    entry["entryId"] == arguments["entryId"]
                        && entry["generation"] == arguments["beforeGeneration"]
                }) && arguments["ownerMatches"] == true
                    && arguments["afterGeneration"].as_u64().is_some()
                    && arguments["currentGeneration"] == arguments["afterGeneration"];
                if !valid {
                    return Err(error(
                        "RETRIEVAL_ANN_STALE_BUILD",
                        "ANN delta no longer matches its committed source version. / ANN 增量已不匹配本次提交的来源版本。",
                        409,
                    ));
                }
                if operation == "store.updateGuard" {
                    return Ok(Value::Null);
                }
                let fields = json!({"count":arguments["count"],"generation":arguments["afterGeneration"],"dirty":true});
                let entry = self
                    .shards
                    .get_mut(key)
                    .expect("validated native ANN entry");
                for (field, value) in fields.as_object().expect("update fields") {
                    entry[field] = value.clone();
                }
                Ok(fields)
            }
            "store.updatePlan" => {
                let old = array(&arguments["oldRows"]);
                let new = array(&arguments["newRows"]);
                let mut result = Vec::new();
                for entry in self.entries() {
                    if !array(&arguments["scopes"]).contains(&entry["identity"]["scopeKey"]) {
                        continue;
                    }
                    let belongs = |row: &Value| {
                        [
                            ("scope_key", "scopeKey"),
                            ("embedding_profile_id", "profileId"),
                            ("dimensions", "dimensions"),
                            ("embedding_model_version", "modelVersion"),
                            ("embedding_space_id", "spaceId"),
                            ("domain", "domain"),
                        ]
                        .iter()
                        .all(|(row_field, identity_field)| {
                            row[*row_field] == entry["identity"][*identity_field]
                        })
                    };
                    let removed = old
                        .iter()
                        .filter(|row| belongs(row))
                        .map(|row| row["id"].clone())
                        .collect::<Vec<_>>();
                    let added = new
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| belongs(row))
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    result.push(json!({"key":entry["key"],"partitioned":entry["descriptor"]["partitioned"]==true,"removed":removed,"added":added,"count":number(&entry["count"],0.0)-removed.len()as f64+added.len()as f64}));
                }
                Ok(json!(result))
            }
            "store.invalidatePlan" => Ok(json!(
                self.entries()
                    .iter()
                    .filter(|entry| array(&arguments["scopes"])
                        .contains(&entry["identity"]["scopeKey"]))
                    .map(|entry| entry["key"].clone())
                    .collect::<Vec<_>>()
            )),
            "store.status" => Ok(self.status(arguments)),
            "store.close" => {
                self.batches.clear();
                self.set("closed", json!(true));
                Ok(Value::Null)
            }
            _ => Err(error(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown ANN store operation.",
                400,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AnnSession;
    use serde_json::json;

    #[test]
    fn empty_cache_status_uses_positive_zero() {
        let mut session = AnnSession::default();
        let receipt = session
            .apply("retrieval.ann.store.status", &json!({}))
            .unwrap();
        let cached_vectors = receipt["value"]["cachedVectors"].as_f64().unwrap();
        assert_eq!(cached_vectors, 0.0);
        assert!(cached_vectors.is_sign_positive());
    }

    #[test]
    fn graph_delta_requires_the_committed_baseline_and_rejects_later_generations() {
        let mut session = AnnSession::default();
        session.apply("retrieval.ann.store.cacheSet", &json!({"key":"graph","entry":{"entryId":7,"key":"graph","generation":1,"count":1,"dirty":false}})).unwrap();
        let guard = json!({"key":"graph","entryId":7,"beforeGeneration":1,"afterGeneration":2,
            "currentGeneration":2,"ownerMatches":true,"count":2});
        let mut wrong_baseline = guard.clone();
        wrong_baseline["beforeGeneration"] = json!(0);
        assert_eq!(
            session
                .apply("retrieval.ann.store.updateGuard", &wrong_baseline)
                .unwrap()["error"]["code"],
            "RETRIEVAL_ANN_STALE_BUILD"
        );
        let mut later_source = guard.clone();
        later_source["currentGeneration"] = json!(3);
        assert_eq!(
            session
                .apply("retrieval.ann.store.updateCommit", &later_source)
                .unwrap()["error"]["code"],
            "RETRIEVAL_ANN_STALE_BUILD"
        );
        assert_eq!(
            session
                .apply("retrieval.ann.store.updateCommit", &guard)
                .unwrap()["value"],
            json!({"count":2,"generation":2,"dirty":true})
        );
        assert_eq!(
            session
                .apply("retrieval.ann.store.updateCommit", &guard)
                .unwrap()["error"]["code"],
            "RETRIEVAL_ANN_STALE_BUILD"
        );
    }

    #[test]
    fn background_batch_partitions_preserve_fifo_and_completed_counts() {
        let mut session = AnnSession::default();
        let call = |session: &mut AnnSession, operation: &str, arguments| {
            session
                .apply(&format!("retrieval.ann.store.{operation}"), &arguments)
                .unwrap()
        };
        assert_eq!(
            call(&mut session, "batchStart", json!({"batchId":7,"count":6}))["value"],
            1
        );
        assert_eq!(
            call(&mut session, "batchNext", json!({"batchId":7}))["value"]["descriptorId"],
            1
        );
        assert!(call(&mut session, "batchNext", json!({"batchId":7}))["error"].is_object());
        assert!(
            call(
                &mut session,
                "batchPartition",
                json!({"batchId":7,"parts":[{"count":6}]})
            )["value"]
                .is_null()
        );
        assert!(
            call(
                &mut session,
                "batchPartition",
                json!({"batchId":7,"parts":[{"count":5}]})
            )["error"]
                .is_object()
        );
        assert_eq!(
            call(
                &mut session,
                "batchPartition",
                json!({"batchId":7,"parts":[{"count":2},{"count":4}]})
            )["value"],
            json!([2, 3])
        );
        assert_eq!(
            call(&mut session, "batchNext", json!({"batchId":7}))["value"],
            json!({"descriptorId":2,"completedVectors":0})
        );
        assert_eq!(
            call(&mut session, "batchComplete", json!({"batchId":7}))["value"],
            2
        );
        assert_eq!(
            call(&mut session, "batchNext", json!({"batchId":7}))["value"],
            json!({"descriptorId":3,"completedVectors":2})
        );
        assert_eq!(
            call(&mut session, "batchComplete", json!({"batchId":7}))["value"],
            6
        );
        assert!(call(&mut session, "batchNext", json!({"batchId":7}))["value"].is_null());
        call(&mut session, "batchEnd", json!({"batchId":7}));
        assert!(call(&mut session, "batchNext", json!({"batchId":7}))["error"].is_object());
    }
}
