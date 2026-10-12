use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::batches::BatchState;
use super::discovery::ModelDiscoveryState;
use super::document_fit::DocumentFitState;
use super::history_service::HistoryProjectionState;
use super::semantic::SummaryRuntimeState;
use super::tool_context::ToolContextState;
use super::worker::WorkerState;

struct NativeRequest {
    ticket: Value,
    kind: String,
    inputs: Value,
    waiting: bool,
}

/// Worker state and accepted native work outlive caller promises; only native receipts drain ownership.
/// Worker 状态与已接纳原生工作独立于调用方 Promise；只有原生回执才能排空所有权。
#[derive(Default)]
pub struct InferenceLifecycleState {
    kind: String,
    profile: Value,
    preference: String,
    limits: Value,
    state: String,
    phase: String,
    loaded: bool,
    closed: bool,
    permanent_close: bool,
    retiring: bool,
    worker_owned: bool,
    worker_exit_confirmed: bool,
    restarts: u64,
    preparing: u64,
    sequence: u64,
    idle_since: f64,
    cpu_threads: Value,
    timeout_ms: f64,
    close_timeout_ms: f64,
    error_code: Option<String>,
    asset_verification: String,
    backend: Value,
    resource_diagnostic: Value,
    requests: BTreeMap<u64, NativeRequest>,
    batches: BatchState,
    fitting: DocumentFitState,
    history: HistoryProjectionState,
    discovery: ModelDiscoveryState,
    tool_context: ToolContextState,
    worker: WorkerState,
    summary_runtime: SummaryRuntimeState,
}

fn number(value: &Value, default: f64) -> f64 {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}
fn text_valid(value: &Value, maximum: usize) -> bool {
    value.as_str().is_some_and(|text| {
        !super::protocol::trim(text).is_empty() && text.encode_utf16().count() <= maximum
    })
}

impl InferenceLifecycleState {
    fn prefix(&self) -> &str {
        if self.kind == "embedding" {
            "EMBEDDING"
        } else {
            "RERANK"
        }
    }
    fn code(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.prefix())
    }
    fn pending(&self) -> usize {
        self.requests
            .values()
            .filter(|request| request.waiting)
            .count()
    }
    fn outstanding_native(&self) -> usize {
        self.requests
            .values()
            .filter(|request| !request.ticket.is_null())
            .count()
    }
    fn safe_to_retire(&self, admission_count: u64, yielding: bool) -> bool {
        self.retiring
            && !self.closed
            && !yielding
            && self.pending() == 0
            && self.preparing == 0
            && self.outstanding_native() == 0
            && admission_count == 0
            && (!self.worker_owned || ["idle", "stopped"].contains(&self.phase.as_str()))
    }
    fn fail(&mut self, code: &str) -> Vec<u64> {
        if !self.closed {
            self.state = if code == self.code("ASSET_MISSING") {
                "unavailable"
            } else {
                "error"
            }
            .into();
            self.error_code = Some(code.into());
            self.loaded = false;
            self.backend = Value::Null;
            if [self.code("ASSET_INVALID"), self.code("ASSET_MISSING")].contains(&code.to_owned()) {
                self.asset_verification = "failed".into();
            }
        }
        let pending: Vec<u64> = self
            .requests
            .iter()
            .filter(|(_, request)| request.waiting)
            .map(|(id, _)| *id)
            .collect();
        for request in self.requests.values_mut() {
            request.waiting = false;
        }
        pending
    }
    fn assert_open(&self) -> Result<(), String> {
        if self.closed || self.retiring {
            Err(self.code("CLOSED"))
        } else {
            Ok(())
        }
    }
    fn validate(&self, args: &Value) -> Result<Value, String> {
        let requested = args["devicePreference"]
            .as_str()
            .unwrap_or(&self.preference);
        if self.kind == "embedding" {
            self.assert_open()?;
        }
        if requested != self.preference {
            return Err(self.code("DEVICE_PREFERENCE_CHANGED"));
        }
        if self.kind == "reranker" {
            self.assert_open()?;
        }
        if args["aborted"] == true {
            return Err(self.code("CANCELLED"));
        }
        let method = args["method"].as_str().unwrap_or("");
        let maximum = self.limits["maxBatchDocuments"].as_u64().unwrap_or(512) as usize;
        if self.kind == "reranker" {
            let query = &args["query"];
            let candidates = args["candidates"]
                .as_array()
                .ok_or_else(|| self.code("INVALID_INPUT"))?;
            let limit = args
                .get("limit")
                .map_or(Some(20), Value::as_u64)
                .ok_or_else(|| self.code("INVALID_INPUT"))? as usize;
            if !text_valid(query, 2048)
                || candidates.len() > maximum
                || limit == 0
                || limit > self.profile["maxCandidates"].as_u64().unwrap_or(20) as usize
            {
                return Err(self.code("INVALID_INPUT"));
            }
            let texts: Vec<Value> = candidates
                .iter()
                .take(limit)
                .map(|item| {
                    item.get("excerpt")
                        .filter(|value| !value.is_null())
                        .unwrap_or(&item["text"])
                        .clone()
                })
                .collect();
            if texts.iter().any(|text| !text_valid(text, 16384)) {
                return Err(self.code("INVALID_INPUT"));
            }
            return Ok(
                json!({"kind": "rerank", "inputs": texts, "query": query, "limit": limit, "empty": texts.is_empty()}),
            );
        }
        let inputs = if method == "query" {
            vec![args["text"].clone()]
        } else {
            args["inputs"]
                .as_array()
                .ok_or_else(|| self.code("INVALID_INPUT"))?
                .clone()
        };
        if inputs.len() > maximum {
            return Err(self.code("INVALID_INPUT"));
        }
        let mut normalized = Vec::new();
        for input in inputs {
            let fitting = method == "fit-documents";
            let text = if fitting && !input.is_string() {
                &input["text"]
            } else {
                &input
            };
            let text = text
                .as_str()
                .filter(|text| !super::protocol::trim(text).is_empty())
                .ok_or_else(|| self.code("INVALID_INPUT"))?;
            if text.encode_utf16().count() > 16384 {
                return Err(self.code("INPUT_TOO_LONG"));
            }
            if fitting {
                let context = if input.is_string() {
                    ""
                } else {
                    input
                        .get("context")
                        .filter(|context| !context.is_null())
                        .map_or(Some(""), Value::as_str)
                        .ok_or_else(|| self.code("INVALID_INPUT"))?
                };
                if context.encode_utf16().count() > 16384 {
                    return Err(self.code("INVALID_INPUT"));
                }
                normalized.push(json!({"text": text, "context": context}));
            } else {
                normalized.push(json!(text));
            }
        }
        Ok(json!({"kind": method, "inputs": normalized, "empty": normalized.is_empty()}))
    }
    fn valid_fitting(&self, documents: &Value, inputs: &Value) -> bool {
        let (Some(documents), Some(inputs)) = (documents.as_array(), inputs.as_array()) else {
            return false;
        };
        if documents.len() != inputs.len() {
            return false;
        }
        documents.iter().zip(inputs).all(|(document, input)| {
            let Some(text) = input["text"].as_str() else {
                return false;
            };
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let context_len = input["context"]
                .as_str()
                .unwrap_or("")
                .encode_utf16()
                .count();
            let Some(segments) = document["segments"]
                .as_array()
                .filter(|segments| !segments.is_empty() && segments.len() <= utf16.len())
            else {
                return false;
            };
            if document["tokenCount"]
                .as_u64()
                .is_none_or(|tokens| tokens < 1)
            {
                return false;
            }
            let mut cursor = 0;
            for segment in segments {
                let (Some(start), Some(end), Some(tokens)) = (
                    segment["start"].as_u64(),
                    segment["end"].as_u64(),
                    segment["tokenCount"].as_u64(),
                ) else {
                    return false;
                };
                let (start, end) = (start as usize, end as usize);
                if start != cursor
                    || end <= cursor
                    || end > utf16.len()
                    || tokens < 1
                    || tokens > self.profile["maxInputTokens"].as_u64().unwrap_or(512)
                    || context_len + end - start > 16384
                    || end < utf16.len()
                        && (0xd800..=0xdbff).contains(&utf16[end - 1])
                        && (0xdc00..=0xdfff).contains(&utf16[end])
                {
                    return false;
                }
                cursor = end;
            }
            cursor == utf16.len()
        })
    }
    fn result(&self, message: &Value, request: &NativeRequest) -> Result<Value, String> {
        let count = request.inputs.as_array().map_or(0, Vec::len);
        if request.kind == "fit-documents" {
            if !self.valid_fitting(&message["documents"], &request.inputs) {
                return Err(self.code("INVALID_FIT_RESULT"));
            }
            return Ok(message["documents"].clone());
        }
        if self.kind == "embedding" {
            let vectors = message["vectors"]
                .as_array()
                .ok_or_else(|| self.code("INVALID_VECTOR"))?;
            let dimensions = self.profile["dimensions"].as_u64().unwrap_or(0) as usize;
            if vectors.len() != count
                || vectors.iter().any(|vector| {
                    vector.as_array().is_none_or(|vector| {
                        vector.len() != dimensions
                            || vector.iter().any(|number| {
                                number.as_f64().is_none_or(|value| !value.is_finite())
                            })
                    })
                })
            {
                return Err(self.code("INVALID_VECTOR"));
            }
            return Ok(message["vectors"].clone());
        }
        let scores = message["scores"]
            .as_array()
            .ok_or_else(|| self.code("INVALID_RESULT"))?;
        let truncated = message["truncatedInputsCount"]
            .as_u64()
            .ok_or_else(|| self.code("INVALID_RESULT"))?;
        if scores.len() != count
            || truncated > count as u64
            || scores.iter().any(|score| {
                score
                    .as_f64()
                    .is_none_or(|score| !score.is_finite() || !(0.0..=1.0).contains(&score))
            })
        {
            return Err(self.code("INVALID_RESULT"));
        }
        let mut indexes: Vec<usize> = (0..count).collect();
        indexes.sort_by(|left, right| {
            scores[*right]
                .as_f64()
                .unwrap()
                .total_cmp(&scores[*left].as_f64().unwrap())
                .then(left.cmp(right))
        });
        Ok(
            json!({"ranked": indexes.into_iter().enumerate().map(|(rank, index)| json!({"index": index, "rerankScore": scores[index], "rerankRank": rank + 1})).collect::<Vec<_>>(),
            "truncatedInputsCount": truncated}),
        )
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation.starts_with("models.lifecycle.summary.") {
            return self.summary_runtime.apply(operation, args);
        }
        if operation.starts_with("models.lifecycle.batch") {
            return self.batches.apply(operation, args);
        }
        if operation.starts_with("models.lifecycle.fit.") {
            return self.fitting.apply(operation, args);
        }
        if operation.starts_with("models.lifecycle.history.") {
            return self.history.apply(operation, args);
        }
        if operation.starts_with("models.lifecycle.discovery.") {
            return self.discovery.apply(operation, args);
        }
        if operation.starts_with("models.lifecycle.toolContext.") {
            return self.tool_context.apply(operation, args);
        }
        if operation.starts_with("models.lifecycle.worker.") {
            return self.worker.apply(operation, args);
        }
        let now = number(&args["now"], 0.0);
        match operation {
            "models.lifecycle.init" => {
                self.kind = args["kind"]
                    .as_str()
                    .ok_or("RETRIEVAL_INVALID_MODEL_KIND")?
                    .into();
                self.profile = args["profile"].clone();
                self.preference = args["devicePreference"].as_str().unwrap_or("auto").into();
                self.limits = args["limits"].clone();
                if self.profile["requiredDevice"].as_str().is_some() && self.preference == "cpu" {
                    return Err(self.code("GPU_REQUIRED"));
                }
                self.timeout_ms = number(
                    &args["timeoutMs"],
                    if self.kind == "embedding" {
                        120000.0
                    } else {
                        60000.0
                    },
                )
                .clamp(1000.0, 300000.0);
                self.close_timeout_ms =
                    number(&args["closeTimeoutMs"], 30000.0).clamp(1.0, 300000.0);
                self.state = if args["assetsExist"] == true {
                    "ready"
                } else {
                    "unavailable"
                }
                .into();
                if self.state == "unavailable" {
                    self.error_code = Some(self.code("ASSET_MISSING"));
                }
                self.phase = "stopped".into();
                self.asset_verification = "pending".into();
                self.cpu_threads = json!(0);
                Ok(json!({"timeoutMs": self.timeout_ms, "closeTimeoutMs": self.close_timeout_ms}))
            }
            "models.lifecycle.validate" => self.validate(args),
            "models.lifecycle.admit" => {
                self.assert_open()?;
                if args["aborted"] == true {
                    return Err(self.code("CANCELLED"));
                }
                if self.state == "unavailable" {
                    return Err(self
                        .error_code
                        .clone()
                        .unwrap_or_else(|| self.code("ASSET_MISSING")));
                }
                let retry = self.state == "error"
                    && !self.worker_owned
                    && self.error_code.as_deref() == Some(&self.code("WORKER_FAILED"))
                    && self.restarts < 1;
                if retry {
                    self.restarts += 1;
                    self.state = "ready".into();
                    self.error_code = None;
                }
                if self.state == "error" {
                    return Err(self
                        .error_code
                        .clone()
                        .unwrap_or_else(|| self.code("WORKER_FAILED")));
                }
                Ok(json!({"retryGpu": retry && self.profile["requiredDevice"].as_str().is_some()}))
            }
            "models.lifecycle.prepare" => {
                self.preparing += 1;
                if self.kind == "embedding" && !self.worker_owned {
                    self.state = "loading".into();
                }
                Ok(Value::Null)
            }
            "models.lifecycle.prepared" => {
                self.preparing = self.preparing.saturating_sub(1);
                if let Some(error) = args.get("error") {
                    self.resource_diagnostic = json!({"code": "INFERENCE_RESOURCE_BUSY", "reason": error["reason"].as_str().unwrap_or("RESOURCE_UNAVAILABLE")});
                    if !self.worker_owned && self.state == "loading" {
                        self.state = "ready".into();
                    }
                }
                Ok(json!({"idleResources": self.pending() == 0 && self.preparing == 0}))
            }
            "models.lifecycle.budget" => {
                if self.closed {
                    return Err(self.code("CLOSED"));
                }
                if args["aborted"] == true {
                    return Err(self.code("CANCELLED"));
                }
                self.cpu_threads = args["budget"]["cpuThreads"].clone();
                if args["kind"] != "fit-documents"
                    && self.profile["requiredDevice"].as_str().is_some()
                    && args["budget"]["device"] != self.profile["requiredDevice"]
                {
                    if !self.worker_owned {
                        self.state = "ready".into();
                    }
                    return Err(self.code("GPU_REQUIRED"));
                }
                Ok(Value::Null)
            }
            "models.lifecycle.workerStart" => {
                self.worker_owned = true;
                self.worker_exit_confirmed = false;
                self.state = "loading".into();
                self.phase = "starting".into();
                Ok(Value::Null)
            }
            "models.lifecycle.dispatch" => {
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or("INFERENCE_REQUEST_LIMIT")?;
                self.requests.insert(
                    self.sequence,
                    NativeRequest {
                        ticket: args["ticket"].clone(),
                        kind: args["kind"].as_str().unwrap_or("").into(),
                        inputs: args["inputs"].clone(),
                        waiting: true,
                    },
                );
                if self.kind == "embedding" {
                    self.phase = "queued".into();
                    self.idle_since = 0.0;
                }
                Ok(json!(self.sequence))
            }
            "models.lifecycle.cancel" => {
                let id = args["id"].as_u64().unwrap_or(0);
                let pending = self.requests.get_mut(&id).is_some_and(|request| {
                    let waiting = request.waiting;
                    request.waiting = false;
                    waiting
                });
                let ticket = if args["notDispatched"] == true {
                    self.requests
                        .remove(&id)
                        .map(|request| request.ticket)
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                Ok(json!({"pending": pending, "releaseTicket": ticket}))
            }
            "models.lifecycle.message" => {
                let message = &args["message"];
                let message_type = message["type"].as_str().unwrap_or("");
                match message_type {
                    "phase" => {
                        if !self.closed {
                            self.phase = message["phase"].as_str().unwrap_or("").into();
                            if self.phase == "loading-model"
                                && !self.loaded
                                && ["ready", "loading"].contains(&self.state.as_str())
                            {
                                self.state = "loading".into();
                            }
                        }
                        Ok(json!({}))
                    }
                    "settled" => {
                        let id = message["id"].as_u64().unwrap_or(0);
                        let ticket = self.requests.get_mut(&id).and_then(|request| {
                            let ticket = std::mem::take(&mut request.ticket);
                            (!ticket.is_null()).then_some(ticket)
                        });
                        self.requests
                            .retain(|_, request| request.waiting || !request.ticket.is_null());
                        Ok(json!({"releaseTickets": ticket.into_iter().collect::<Vec<_>>()}))
                    }
                    "idle" => {
                        let through = message["throughId"].as_u64().unwrap_or(0);
                        let ids: Vec<u64> = self
                            .requests
                            .keys()
                            .filter(|id| **id <= through)
                            .copied()
                            .collect();
                        let tickets: Vec<Value> = ids
                            .iter()
                            .filter_map(|id| {
                                self.requests.get_mut(id).and_then(|request| {
                                    let ticket = std::mem::take(&mut request.ticket);
                                    (!ticket.is_null()).then_some(ticket)
                                })
                            })
                            .collect();
                        self.requests
                            .retain(|_, request| request.waiting || !request.ticket.is_null());
                        let current = if self.kind == "embedding" {
                            through == self.sequence
                        } else {
                            through >= self.sequence
                        };
                        let idle =
                            !self.closed && current && self.pending() == 0 && self.preparing == 0;
                        if idle {
                            self.phase = "idle".into();
                            if self.idle_since == 0.0 {
                                self.idle_since = now;
                            }
                            if !self.loaded && self.state == "loading" {
                                self.state = "ready".into();
                            }
                        }
                        Ok(json!({"releaseTickets": tickets, "idleResources": idle}))
                    }
                    "ready" | "tokenizer-ready" => {
                        if self.closed || ["error", "unavailable"].contains(&self.state.as_str()) {
                            return Ok(json!({}));
                        }
                        self.state = "ready".into();
                        self.asset_verification = "verified".into();
                        self.error_code = None;
                        if message_type == "ready" {
                            self.loaded = true;
                            if let Some(backend) = message.get("inferenceBackend") {
                                self.backend = backend.clone();
                                return Ok(json!({"backend": backend}));
                            }
                        }
                        Ok(json!({}))
                    }
                    "resource-pressure" => {
                        self.resource_diagnostic = message["diagnostic"].clone();
                        Ok(json!({"resourcePressure": true}))
                    }
                    "result" | "error" => {
                        let id = message["id"].as_u64().unwrap_or(0);
                        let Some(request) =
                            self.requests.get(&id).filter(|request| request.waiting)
                        else {
                            return Ok(json!({}));
                        };
                        let result = if message_type == "result" {
                            self.result(message, request)
                        } else {
                            Err(message["code"]
                                .as_str()
                                .unwrap_or_else(|| {
                                    if self.kind == "embedding" {
                                        "EMBEDDING_FAILED"
                                    } else {
                                        "RERANK_FAILED"
                                    }
                                })
                                .into())
                        };
                        self.requests.get_mut(&id).unwrap().waiting = false;
                        self.requests
                            .retain(|_, request| request.waiting || !request.ticket.is_null());
                        if message_type != "result"
                            && self.kind == "reranker"
                            && message["code"] != "RERANK_BUSY"
                        {
                            self.state = "error".into();
                            self.error_code = result.as_ref().err().cloned();
                        }
                        Ok(match result {
                            Ok(result) => json!({"id": id, "result": result}),
                            Err(code) => json!({"id": id, "errorCode": code}),
                        })
                    }
                    _ => Ok(json!({})),
                }
            }
            "models.lifecycle.fail" => {
                let code = args["code"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| self.code("WORKER_FAILED"));
                Ok(json!({"rejectIds": self.fail(&code)}))
            }
            "models.lifecycle.workerExit" => {
                self.worker_owned = false;
                self.worker_exit_confirmed = true;
                self.phase = "stopped".into();
                self.idle_since = 0.0;
                let tickets: Vec<Value> = self
                    .requests
                    .values()
                    .map(|request| request.ticket.clone())
                    .collect();
                let failure = !self.closed
                    && (self.pending() > 0
                        || !["error", "unavailable"].contains(&self.state.as_str()));
                let reject = if failure {
                    self.fail(&self.code("WORKER_FAILED"))
                } else {
                    vec![]
                };
                self.requests.clear();
                Ok(
                    json!({"releaseTickets": tickets, "rejectIds": reject, "idleResources": true,
                    "gpuFailed": !self.closed && args["acknowledged"] != true && number(&args["gpuMemoryBytes"], 0.0) > 0.0}),
                )
            }
            "models.lifecycle.diagnostic" => {
                self.resource_diagnostic = args["diagnostic"].clone();
                Ok(Value::Null)
            }
            "models.lifecycle.claimIdle" => {
                let reason = if self.closed {
                    Some("closed")
                } else if self.pending() > 0
                    || self.preparing > 0
                    || self.outstanding_native() > 0
                    || args["activeRequests"].as_u64().unwrap_or(0) > 0
                {
                    Some("native-work-outstanding")
                } else if args["gpuOnly"] == true && number(&args["gpuMemoryBytes"], 0.0) == 0.0 {
                    Some("no-gpu-residency")
                } else if !self.worker_owned
                    && self.worker_exit_confirmed
                    && self.phase == "stopped"
                    && number(&args["minimumIdleMs"], 0.0) == 0.0
                    && (number(&args["gpuMemoryBytes"], 0.0) > 0.0
                        || number(&args["residentMemoryBytes"], 0.0) > 0.0)
                {
                    None
                } else if !self.worker_owned || self.phase != "idle" {
                    Some("worker-not-idle")
                } else if number(&args["minimumIdleMs"], 0.0) > 0.0
                    && (self.idle_since == 0.0
                        || now - self.idle_since < number(&args["minimumIdleMs"], 0.0))
                {
                    Some("idle-grace-period")
                } else {
                    None
                };
                Ok(match reason {
                    Some(reason) => json!({"retiring": false, "reason": reason}),
                    None => json!({"retiring": true,
                    "gpuMemoryBytes": args["gpuMemoryBytes"], "residentMemoryBytes": args["residentMemoryBytes"]}),
                })
            }
            "models.lifecycle.retire" => {
                self.retiring = true;
                Ok(json!({"closed": self.closed}))
            }
            "models.lifecycle.shouldRetire" => Ok(json!(self.safe_to_retire(
                args["activeRequests"].as_u64().unwrap_or(0),
                args["yielding"] == true
            ))),
            "models.lifecycle.close" => {
                if args["permanent"] == true {
                    self.permanent_close = true;
                }
                if self.closed {
                    return Ok(json!({"rejectIds": []}));
                }
                self.closed = true;
                self.loaded = false;
                self.state = "closing".into();
                self.phase = if self.worker_owned {
                    "closing"
                } else {
                    "stopped"
                }
                .into();
                self.error_code = Some(self.code("CLOSED"));
                Ok(json!({"rejectIds": self.fail(&self.code("CLOSED"))}))
            }
            "models.lifecycle.drained" => {
                if args["noWorker"] != true
                    && (args["exitCode"] != 0 || args["acknowledged"] != true)
                {
                    return Err(self.code("CLOSE_FAILED"));
                }
                self.requests.clear();
                self.state = "unavailable".into();
                Ok(Value::Null)
            }
            "models.lifecycle.closeFailed" => {
                self.state = "error".into();
                self.error_code = Some(
                    args["code"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| self.code("CLOSE_FAILED")),
                );
                Ok(Value::Null)
            }
            "models.lifecycle.reopen" => {
                if self.permanent_close {
                    return Ok(json!(false));
                }
                self.closed = false;
                self.loaded = false;
                self.phase = "stopped".into();
                self.error_code = None;
                self.backend = Value::Null;
                self.resource_diagnostic = Value::Null;
                self.restarts = 0;
                self.asset_verification = "pending".into();
                self.cpu_threads = json!(0);
                self.state = if args["assetsExist"] == true {
                    "ready"
                } else {
                    "unavailable"
                }
                .into();
                if self.state == "unavailable" {
                    self.error_code = Some(self.code("ASSET_MISSING"));
                }
                Ok(json!(true))
            }
            "models.lifecycle.status" => {
                let mut status = json!({"state": self.state, "loaded": self.loaded, "supported": true, "local": true, "network": false,
                    "workerPhase": self.phase, "assetVerification": self.asset_verification, "devicePreference": self.preference,
                    "retiring": self.retiring, "maxInputTokens": self.profile["maxInputTokens"], "pendingRequests": self.pending(), "cpuThreads": self.cpu_threads});
                if self.kind == "embedding" {
                    status["idleSince"] = if self.idle_since == 0.0 {
                        Value::Null
                    } else {
                        json!(self.idle_since)
                    };
                }
                if let Some(code) = &self.error_code {
                    status["errorCode"] = json!(code);
                }
                if !self.backend.is_null() {
                    status["inferenceBackend"] = self.backend.clone();
                }
                if !self.resource_diagnostic.is_null() {
                    status["resourceDiagnostic"] = self.resource_diagnostic.clone();
                }
                Ok(status)
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
