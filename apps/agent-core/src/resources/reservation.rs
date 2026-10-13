use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use super::inference;

const MIB: f64 = 1024.0 * 1024.0;
const LEASE_TTL_MS: u64 = 30_000;

fn number(value: &Value, default: f64) -> f64 {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}
fn integer(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|value| *value <= 9_007_199_254_740_991)
}
fn present(value: &Value) -> bool {
    !value.is_null()
}

type CostHistory = VecDeque<(String, VecDeque<Value>)>;
static COST_HISTORY: OnceLock<Mutex<CostHistory>> = OnceLock::new();

fn cost_identity(profile: &Value, backend: &str) -> String {
    format!(
        "{}:{backend}:{}",
        profile["modelVersion"]
            .as_str()
            .or_else(|| profile["id"].as_str())
            .unwrap_or(""),
        profile["dtype"].as_str().unwrap_or("fp32")
    )
}

fn records(profile: &Value, backend: &str) -> Result<Vec<Value>, String> {
    let costs = COST_HISTORY
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .map_err(|_| "INFERENCE_COST_LEDGER_UNAVAILABLE")?;
    Ok(costs
        .iter()
        .find(|(identity, _)| identity == &cost_identity(profile, backend))
        .map(|(_, entries)| entries.iter().cloned().collect())
        .unwrap_or_default())
}

/// Samples are accepted only from this reservation's owned isolated process, never gateway RSS.
/// 仅采纳当前预约拥有的隔离进程采样；网关 RSS 和其他进程不能冒充模型占用。
fn remember_cost(
    profile: &Value,
    feedback: &Value,
    executor: Option<u64>,
    gateway: u64,
) -> Result<(), String> {
    let process_id = integer(&feedback["processId"]);
    let backend = feedback["backend"].as_str().unwrap_or("");
    let bytes = integer(&feedback["workloadMemoryDeltaBytes"]).unwrap_or(0);
    if feedback["memoryMeasurementSource"] != "isolated-native-process-delta"
        || process_id.is_none()
        || process_id != executor
        || process_id == Some(gateway)
        || !["cpu", "dml"].contains(&backend)
        || bytes == 0
        || bytes > 32 * 1024_u64.pow(3)
    {
        return Ok(());
    }
    let identity = cost_identity(profile, backend);
    let mut costs = COST_HISTORY
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .map_err(|_| "INFERENCE_COST_LEDGER_UNAVAILABLE")?;
    let mut entries = costs
        .iter()
        .position(|(key, _)| key == &identity)
        .and_then(|index| costs.remove(index))
        .map(|(_, entries)| entries)
        .unwrap_or_default();
    entries.push_back(json!({"bytes": bytes, "phase": feedback["phase"],
        "sequenceTokens": feedback.get("sequenceTokens").unwrap_or(&json!(0)),
        "batchSize": feedback.get("batchSize").unwrap_or(&json!(0))}));
    while entries.len() > 16 {
        entries.pop_front();
    }
    costs.push_back((identity, entries));
    while costs.len() > 64 {
        costs.pop_front();
    }
    Ok(())
}

pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    if operation != "resources.inferenceMeasuredRequest" {
        return Err("AGENT_CORE_UNKNOWN_OPERATION".into());
    }
    let mut args = args.clone();
    let backend = args["backend"].as_str().unwrap_or("cpu");
    args["records"] = json!(records(&args["profile"], backend)?);
    inference::calculate("resources.inferenceRequest", &args)
}

/// Rust owns approved lease facts and all model demand/feedback rules; adapters execute effects.
/// Rust 拥有已批准租约事实及完整模型需求/反馈规则；适配器执行申请、续期和释放 I/O。
#[derive(Default)]
pub struct ReservationState {
    profile: Value,
    request: Value,
    previous_request: Value,
    task_id: String,
    kind: String,
    preference: String,
    platform: String,
    arch: String,
    cuda_visible_devices: String,
    logical_cores: f64,
    gateway_process_id: u64,
    executor_process_id: Option<u64>,
    base_cpu_threads: f64,
    explicit_cpu_threads: bool,
    batch_multiplier: f64,
    resident: Vec<Value>,
    gpu: Vec<Value>,
    execution: Value,
    closed: bool,
    renewal_failed: bool,
    diagnostic: Value,
    feedback: Value,
    applied_backend: Value,
    last_batch_measurement: Value,
    planning_suggestions: Value,
    last_grant: Value,
    last_gpu_snapshot_at: f64,
    adjustments: VecDeque<Value>,
}

impl ReservationState {
    fn resident_bytes(&self) -> f64 {
        self.resident
            .iter()
            .map(|lease| number(&lease["memoryBytes"], 0.0))
            .fold(0.0, |total, bytes| total + bytes)
    }
    fn gpu_bytes(&self) -> f64 {
        self.gpu
            .iter()
            .map(|lease| number(&lease["gpuMemoryBytes"], 0.0))
            .fold(0.0, |total, bytes| total + bytes)
    }
    fn suggestions_multiplier(&self) -> f64 {
        let suggestion = number(&self.planning_suggestions["batchMultiplier"], 1.0);
        let mut fraction = number(&self.feedback["backgroundFraction"], suggestion);
        if suggestion < 1.0 {
            fraction = fraction.min(suggestion);
        }
        if fraction >= 1.0 {
            fraction = fraction.max(number(
                &self.planning_suggestions["batchProbeMultiplier"],
                1.0,
            ));
        }
        fraction.clamp(0.125, 4.0)
    }
    fn demand(
        &self,
        cpu_threads: f64,
        backend: &str,
        sequence_tokens: f64,
        batch_size: f64,
    ) -> Result<Value, String> {
        inference::calculate(
            "resources.inferenceRequest",
            &json!({"profile": self.profile, "requestedCpuThreads": cpu_threads,
            "backend": backend, "sequenceTokens": sequence_tokens, "batchSize": batch_size, "logicalCores": self.logical_cores,
            "records": records(&self.profile, backend)?}),
        )
    }
    fn lease_request(
        &self,
        suffix: &str,
        kind: &str,
        cpu_threads: f64,
        memory_bytes: f64,
        gpu_bytes: f64,
        wait: bool,
    ) -> Value {
        let mut request = json!({"taskId": format!("{}-{suffix}", self.task_id), "workspaceId": "local", "kind": kind,
            "cpuThreads": cpu_threads, "memoryBytes": memory_bytes, "gpuMemoryBytes": gpu_bytes, "ttlMs": LEASE_TTL_MS});
        if wait {
            request["waitMs"] = json!(5000);
        }
        request
    }
    fn adjustment(&mut self, value: &Value, now: f64) {
        let Some(reason) = value["reason"]
            .as_str()
            .filter(|reason| reason.encode_utf16().count() <= 96)
        else {
            return;
        };
        let mut record = json!({"appliedAt": now, "phase": "applied", "reason": reason,
            "requiresSessionRebuild": value["requiresSessionRebuild"] == true});
        for key in ["boundary", "from", "to"] {
            if let Some(value) = value.get(key) {
                record[key] = value.clone();
            }
        }
        self.push_adjustment(record);
    }
    fn push_adjustment(&mut self, value: Value) {
        self.adjustments.push_back(value);
        while self.adjustments.len() > 16 {
            self.adjustments.pop_front();
        }
    }
    fn all_leases(&self) -> Vec<Value> {
        self.resident
            .iter()
            .chain(self.gpu.iter())
            .cloned()
            .chain(present(&self.execution).then(|| self.execution.clone()))
            .collect()
    }
    fn release_receipt(&mut self, lease_id: &str) {
        self.resident.retain(|lease| lease["leaseId"] != lease_id);
        self.gpu.retain(|lease| lease["leaseId"] != lease_id);
        if self.execution["leaseId"] == lease_id {
            self.execution = Value::Null;
        }
    }
    fn grant(&mut self, now: f64) -> Result<Value, String> {
        let shape = inference::calculate(
            "resources.inferenceShape",
            &json!({"profile": self.profile}),
        )?;
        let memory = self.resident_bytes();
        let gpu_memory = self.gpu_bytes();
        let maximum = number(&self.profile["maxInputTokens"], 512.0);
        let activation = (memory
            - number(
                &self.request["memoryEstimate"]["residentBaselineBytes"],
                0.0,
            ))
        .max(1.0);
        let scratch = json!({"activationMemoryBytes": activation, "hiddenSize": shape["hiddenSize"], "attentionHeads": shape["heads"],
            "sequenceTokens": self.request["memoryEstimate"]["sequenceTokens"]});
        let mut cpu_budget = json!({"cpuThreads": self.execution["cpuThreads"], "memoryBytes": memory, "device": "cpu", "batchMultiplier": self.batch_multiplier});
        cpu_budget
            .as_object_mut()
            .unwrap()
            .extend(scratch.as_object().unwrap().clone());
        let mut cpu_batch = inference::calculate(
            "resources.inferenceBatch",
            &json!({"budget": cpu_budget, "maxInputTokens": maximum}),
        )?;
        for key in ["activationMemoryBytes", "hiddenSize", "attentionHeads"] {
            cpu_batch[key] = scratch[key].clone();
        }
        let device = if self.gpu.is_empty() {
            "cpu"
        } else if self.platform == "win32" {
            "dml"
        } else {
            "cuda"
        };
        let mut grant = json!({"cpuThreads": self.execution["cpuThreads"], "memoryBytes": memory, "gpuMemoryBytes": gpu_memory,
            "device": device, "resourceMode": self.execution["mode"], "memoryEstimate": self.request["memoryEstimate"],
            "batchMultiplier": self.batch_multiplier, "batchSuggestions": self.execution.get("suggestions").unwrap_or(&json!({})),
            "cpuFallbackBatchBudget": cpu_batch, "hiddenSize": shape["hiddenSize"], "attentionHeads": shape["heads"],
            "activationMemoryBytes": if self.gpu.is_empty() { activation } else { (gpu_memory - number(&self.request["memoryEstimate"]["runtimeWeightBytes"], 0.0) - 128.0 * MIB).max(1.0) }});
        if let Some(gpu) = self.gpu.first() {
            grant["deviceId"] = gpu
                .get("executionDeviceId")
                .unwrap_or(&gpu["deviceId"])
                .clone();
        }
        let mut batch_budget = cpu_budget;
        batch_budget["gpuMemoryBytes"] = json!(gpu_memory);
        batch_budget["device"] = json!(if self.gpu.is_empty() { "cpu" } else { "dml" });
        let batch = inference::calculate(
            "resources.inferenceBatch",
            &json!({"budget": batch_budget, "maxInputTokens": maximum}),
        )?;
        grant
            .as_object_mut()
            .unwrap()
            .extend(batch.as_object().unwrap().clone());
        if present(&self.diagnostic) {
            grant["diagnostic"] = self.diagnostic.clone();
        }
        if present(&self.last_grant)
            && [
                "device",
                "deviceId",
                "cpuThreads",
                "batchSize",
                "batchTokenBudget",
            ]
            .iter()
            .any(|key| grant[*key] != self.last_grant[*key])
        {
            let reason = if grant["device"] != self.last_grant["device"] {
                "backend-admission-changed"
            } else if grant["cpuThreads"] != self.last_grant["cpuThreads"] {
                "approved-thread-budget-changed"
            } else {
                "steady-state-batch-feedback"
            };
            self.push_adjustment(json!({"appliedAt": now, "boundary": "next-request", "phase": "granted", "reason": reason,
                "from": {"device": self.last_grant["device"], "cpuThreads": self.last_grant["cpuThreads"], "batchSize": self.last_grant["batchSize"]},
                "to": {"device": grant["device"], "cpuThreads": grant["cpuThreads"], "batchSize": grant["batchSize"]}}));
        }
        self.last_grant = grant.clone();
        self.planning_suggestions = self.execution["suggestions"].clone();
        Ok(grant)
    }
    fn status(&self) -> Value {
        let batch_size = (32.0 * self.suggestions_multiplier())
            .floor()
            .clamp(1.0, 128.0);
        let maximum = number(&self.profile["maxInputTokens"], 512.0);
        let mut status = json!({"devicePreference": self.preference, "cpuThreads": number(&self.execution["cpuThreads"], 0.0),
            "residentMemoryBytes": self.resident_bytes(), "gpuMemoryBytes": self.gpu_bytes(), "leaseRenewalFailed": self.renewal_failed,
            "batchSuggestions": {"batchSize": batch_size, "batchTokenBudget": (batch_size * 384.0).max(maximum).min(65536.0),
                "maxInputTokens": maximum, "estimatedMemoryBytes": self.request["memoryBytes"], "source": "resource-demand-estimate", "requiresApproval": true},
            "memoryEstimate": self.request["memoryEstimate"], "adjustments": self.adjustments,
            "cpuThreadAudit": {"requested": self.request["cpuThreads"], "granted": number(&self.execution["cpuThreads"], 0.0),
                "configuredInSession": self.applied_backend["cpuThreads"], "source": self.applied_backend["source"].as_str().unwrap_or("not-reported"),
                "physicalActiveThreadsMeasured": false}});
        for (key, value) in [
            ("feedback", &self.feedback),
            ("appliedBackend", &self.applied_backend),
            ("lastBatchMeasurement", &self.last_batch_measurement),
            ("lastGrant", &self.last_grant),
            ("diagnostic", &self.diagnostic),
        ] {
            if present(value) {
                status[key] = value.clone();
            }
        }
        status
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let now = number(&args["now"], 0.0);
        match operation {
            "resources.reservation.init" => {
                if present(&self.profile) {
                    return Err("INFERENCE_RESERVATION_EXISTS".into());
                }
                let preference = args["devicePreference"].as_str().unwrap_or("auto");
                if !["auto", "cpu"].contains(&preference) {
                    return Err("INFERENCE_INVALID_DEVICE_PREFERENCE".into());
                }
                self.profile = args["profile"].clone();
                self.preference = preference.into();
                self.kind = args["kind"].as_str().unwrap_or("background").into();
                self.task_id = args["taskId"]
                    .as_str()
                    .ok_or("INFERENCE_INVALID_TASK_ID")?
                    .into();
                self.platform = args["platform"].as_str().unwrap_or("").into();
                self.arch = args["arch"].as_str().unwrap_or("").into();
                self.cuda_visible_devices =
                    args["cudaVisibleDevices"].as_str().unwrap_or("").into();
                self.gateway_process_id = integer(&args["gatewayProcessId"]).unwrap_or(0);
                self.logical_cores = number(&args["logicalCores"], 1.0);
                self.explicit_cpu_threads = args["cpuThreads"]
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value.fract() == 0.0);
                self.request = calculate(
                    "resources.inferenceMeasuredRequest",
                    &json!({"profile": self.profile,
                    "requestedCpuThreads": args["cpuThreads"], "backend": self.profile["requiredDevice"].as_str().unwrap_or("cpu"), "logicalCores": self.logical_cores}),
                )?;
                self.base_cpu_threads = number(&self.request["cpuThreads"], 1.0);
                self.batch_multiplier = 1.0;
                Ok(Value::Null)
            }
            "resources.reservation.admit" => {
                if self.closed {
                    return Err("INFERENCE_RESERVATION_CLOSED".into());
                }
                if self.renewal_failed {
                    return Err("INFERENCE_LEASE_RENEWAL_FAILED".into());
                }
                Ok(Value::Null)
            }
            "resources.reservation.plan" => {
                self.previous_request = self.request.clone();
                self.batch_multiplier = self.suggestions_multiplier();
                let cpu = if self.explicit_cpu_threads {
                    self.base_cpu_threads
                } else {
                    (self.base_cpu_threads * self.batch_multiplier)
                        .ceil()
                        .min(self.logical_cores)
                        .clamp(1.0, 32.0)
                };
                self.request = self.demand(
                    cpu,
                    self.applied_backend["device"]
                        .as_str()
                        .or_else(|| self.profile["requiredDevice"].as_str())
                        .unwrap_or("cpu"),
                    number(&self.last_batch_measurement["sequenceTokens"], 128.0),
                    (32.0 * self.batch_multiplier).floor().clamp(1.0, 128.0),
                )?;
                Ok(Value::Null)
            }
            "resources.reservation.residentPlan" => {
                let desired = if args["tokenizerOnly"] == true {
                    number(&self.request["tokenizerMemoryBytes"], 0.0)
                } else {
                    (number(&self.request["memoryBytes"], 0.0) / (64.0 * MIB)).ceil() * 64.0 * MIB
                };
                Ok(if self.resident_bytes() >= desired {
                    Value::Null
                } else {
                    self.lease_request(
                        "resident",
                        &self.kind,
                        0.0,
                        desired - self.resident_bytes(),
                        0.0,
                        true,
                    )
                })
            }
            "resources.reservation.residentDenied" => {
                let observed = number(&self.request["memoryEstimate"]["observedPeakBytes"], 0.0);
                let fallback = !self.closed
                    && args["aborted"] != true
                    && args["tokenizerOnly"] != true
                    && present(&self.last_grant)
                    && !(observed > 0.0 && observed * 1.125 + 32.0 * MIB > self.resident_bytes());
                if fallback {
                    self.request = self.previous_request.clone();
                    self.request["memoryBytes"] = json!(self.resident_bytes());
                    self.batch_multiplier = self
                        .batch_multiplier
                        .min(number(&self.last_grant["batchMultiplier"], 1.0));
                    self.adjustment(
                        &json!({"reason": "resident-expansion-denied", "boundary": "next-request"}),
                        now,
                    );
                }
                Ok(json!(fallback))
            }
            "resources.reservation.executionPlan" => Ok(if present(&self.execution) {
                Value::Null
            } else {
                self.lease_request(
                    "execution",
                    args["kind"].as_str().unwrap_or(&self.kind),
                    number(&self.request["cpuThreads"], 1.0),
                    0.0,
                    0.0,
                    true,
                )
            }),
            "resources.reservation.gpuUpgradePlan" => Ok(
                if !self.gpu.is_empty()
                    && number(&self.request["gpuMemoryBytes"], 0.0) > self.gpu_bytes()
                {
                    self.lease_request(
                        "gpu",
                        &self.kind,
                        0.0,
                        0.0,
                        number(&self.request["gpuMemoryBytes"], 0.0) - self.gpu_bytes(),
                        false,
                    )
                } else {
                    Value::Null
                },
            ),
            "resources.reservation.gpuSnapshotPlan" => {
                let inspect = self.diagnostic["code"] == "GPU_RESOURCE_UNAVAILABLE"
                    && now - self.last_gpu_snapshot_at >= 1000.0;
                if inspect {
                    self.last_gpu_snapshot_at = now;
                }
                Ok(json!(inspect))
            }
            "resources.reservation.gpuSnapshot" => {
                if args["snapshot"]["gpu"]["state"] == "available"
                    && number(&args["snapshot"]["gpu"]["availableMemoryBytes"], 0.0)
                        >= number(&self.request["gpuMemoryBytes"], 0.0)
                {
                    self.diagnostic = Value::Null;
                }
                Ok(Value::Null)
            }
            "resources.reservation.gpuPlan" => Ok(
                if self.preference == "auto" && !present(&self.diagnostic) && self.gpu.is_empty() {
                    self.lease_request(
                        "gpu",
                        &self.kind,
                        0.0,
                        0.0,
                        number(&self.request["gpuMemoryBytes"], 0.0),
                        false,
                    )
                } else {
                    Value::Null
                },
            ),
            "resources.reservation.lease" => {
                let lease = &args["lease"];
                let kind = args["kind"]
                    .as_str()
                    .ok_or("INFERENCE_INVALID_LEASE_KIND")?;
                if lease["status"] != "granted" {
                    if kind == "gpu" {
                        self.last_gpu_snapshot_at = now;
                        self.diagnostic = json!({"code": "GPU_RESOURCE_UNAVAILABLE", "reason": lease["reason"].as_str().unwrap_or("gpu-budget-unavailable")});
                    }
                    if kind == "gpuUpgrade" {
                        self.adjustment(
                            &json!({"reason": "gpu-expansion-denied", "boundary": "next-request"}),
                            now,
                        );
                    }
                    return Ok(
                        json!({"accepted": false, "denied": true, "reason": lease["reason"]}),
                    );
                }
                let accepted = match kind {
                    "resident" => {
                        self.resident.push(lease.clone());
                        true
                    }
                    "execution" => {
                        self.execution = lease.clone();
                        if number(&lease["suggestions"]["batchMultiplier"], 1.0) < 1.0 {
                            self.batch_multiplier = self
                                .batch_multiplier
                                .min(number(&lease["suggestions"]["batchMultiplier"], 1.0));
                        }
                        true
                    }
                    "gpuUpgrade" => self.gpu.first().is_some_and(|base| {
                        ["executionProvider", "executionDeviceId", "deviceId"]
                            .iter()
                            .all(|key| lease[*key] == base[*key])
                    }),
                    "gpu" => {
                        let direct_ml = self.platform == "win32"
                            && lease["executionProvider"] == "dml"
                            && integer(&lease["executionDeviceId"]).is_some();
                        let cuda = self.platform == "linux"
                            && self.arch == "x64"
                            && lease["deviceId"] == 0
                            && ["", "0"].contains(&self.cuda_visible_devices.as_str());
                        if !direct_ml && !cuda {
                            self.diagnostic = json!({"code": "GPU_DEVICE_MAPPING_UNVERIFIED"});
                        }
                        direct_ml || cuda
                    }
                    _ => return Err("INFERENCE_INVALID_LEASE_KIND".into()),
                };
                if accepted && ["gpu", "gpuUpgrade"].contains(&kind) {
                    self.gpu.push(lease.clone());
                }
                // Retain even an unusable grant until release is acknowledged; uncertainty never permits replacement.
                // 不可用的批准额度也须保留到释放确认；未知清理状态不能允许替代驻留。
                if !accepted {
                    self.gpu.push(lease.clone());
                }
                Ok(
                    json!({"accepted": accepted, "release": !accepted, "leaseId": lease["leaseId"],
                    "registerProcessId": if kind == "execution" { None } else { self.executor_process_id }}),
                )
            }
            "resources.reservation.grant" => {
                if self.closed {
                    return Err("INFERENCE_RESERVATION_CLOSED".into());
                }
                self.grant(now)
            }
            "resources.reservation.leases" => Ok(json!(self.all_leases())),
            "resources.reservation.idlePlan" => Ok(if present(&self.execution) {
                json!([self.execution])
            } else {
                json!([])
            }),
            "resources.reservation.releaseReceipt" => {
                let lease_id = args["leaseId"]
                    .as_str()
                    .ok_or("INFERENCE_INVALID_LEASE_ID")?;
                if args
                    .get("result")
                    .is_some_and(|result| result["status"] != "released")
                {
                    return Err("INFERENCE_RELEASE_UNCONFIRMED".into());
                }
                self.release_receipt(lease_id);
                Ok(Value::Null)
            }
            "resources.reservation.close" => {
                self.closed = true;
                Ok(Value::Null)
            }
            "resources.reservation.renewal" => {
                if args["failed"] == true
                    || args["results"].as_array().is_some_and(|results| {
                        results.iter().any(|result| result["status"] == "denied")
                    })
                {
                    self.renewal_failed = true;
                }
                Ok(Value::Null)
            }
            "resources.reservation.backend" => {
                let status = &args["status"];
                let device = status["device"].as_str().unwrap_or("");
                if ["cpu", "dml", "cuda"].contains(&device) {
                    let threads = integer(&status["cpuThreads"]).filter(|threads| *threads > 0);
                    self.applied_backend = json!({"device": device, "cpuThreads": threads,
                        "source": if threads.is_some() { "worker-ready-session-options" } else { "backend-retirement" },
                        "sessionInitialized": threads.is_some(), "gpuValidated": status["gpuValidated"] == true,
                        "cpuOperatorFallback": status["cpuOperatorFallback"] == true});
                    for key in ["deviceId", "executionMode"] {
                        if let Some(value) = status.get(key) {
                            self.applied_backend[key] = value.clone();
                        }
                    }
                    let diagnostic = status.get("diagnostic").unwrap_or(&self.diagnostic);
                    if present(diagnostic) {
                        self.applied_backend["diagnostic"] = diagnostic.clone();
                    }
                }
                if device != "cpu" || self.gpu.is_empty() {
                    return Ok(json!([]));
                }
                self.diagnostic = status
                    .get("diagnostic")
                    .cloned()
                    .unwrap_or(json!({"code": "GPU_BACKEND_UNAVAILABLE"}));
                if present(&self.last_grant) {
                    let fallback = self.last_grant["cpuFallbackBatchBudget"]
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                    self.last_grant.as_object_mut().unwrap().remove("deviceId");
                    self.last_grant.as_object_mut().unwrap().extend(fallback);
                    self.last_grant["device"] = json!("cpu");
                    self.last_grant["gpuMemoryBytes"] = json!(0);
                    self.last_grant["diagnostic"] = self.diagnostic.clone();
                }
                Ok(json!(self.gpu))
            }
            "resources.reservation.retryGpu" => {
                if self.gpu.is_empty() && self.diagnostic["code"] == "GPU_WORKER_FAILED" {
                    self.diagnostic = Value::Null;
                }
                Ok(Value::Null)
            }
            "resources.reservation.executor" => {
                let Some(process_id) = integer(&args["processId"]).filter(|value| *value > 0)
                else {
                    return Ok(json!([]));
                };
                self.executor_process_id = Some(process_id);
                Ok(json!(
                    self.resident
                        .iter()
                        .chain(self.gpu.iter())
                        .cloned()
                        .collect::<Vec<_>>()
                ))
            }
            "resources.reservation.report" => {
                let feedback = &args["feedback"];
                remember_cost(
                    &self.profile,
                    feedback,
                    self.executor_process_id,
                    self.gateway_process_id,
                )?;
                let sequence = integer(&feedback["sequenceTokens"]).unwrap_or(0);
                let batch = integer(&feedback["batchSize"]).unwrap_or(0);
                let tokens = integer(&feedback["inputTokens"]).unwrap_or(0);
                let padded = integer(&feedback["paddedTokens"]).unwrap_or(0);
                if feedback["phase"] == "hot-inference"
                    && self.executor_process_id.is_some()
                    && integer(&feedback["processId"]) == self.executor_process_id
                    && sequence > 0
                    && (1..=128).contains(&batch)
                    && tokens > 0
                    && padded >= tokens
                {
                    self.last_batch_measurement = json!({"processId": feedback["processId"], "backend": feedback["backend"],
                        "sequenceTokens": sequence, "batchSize": batch, "inputTokens": tokens, "paddedTokens": padded,
                        "paddingTokens": padded - tokens, "source": "owned-worker-hot-inference"});
                    if integer(&feedback["bucketMinimumTokens"]).is_some() {
                        self.last_batch_measurement["bucketMinimumTokens"] =
                            feedback["bucketMinimumTokens"].clone();
                    }
                    if feedback["latencyMs"].as_f64().is_some_and(f64::is_finite) {
                        self.last_batch_measurement["latencyMs"] = feedback["latencyMs"].clone();
                    }
                }
                self.request = self.demand(
                    number(&self.request["cpuThreads"], 1.0),
                    feedback["backend"].as_str().unwrap_or("cpu"),
                    number(&feedback["sequenceTokens"], 128.0),
                    number(&self.request["memoryEstimate"]["batchSize"], 32.0),
                )?;
                if feedback["phase"]
                    .as_str()
                    .is_some_and(|phase| !phase.is_empty() && phase != "hot-inference")
                    && feedback["allocationFailure"] != true
                {
                    return Ok(Value::Null);
                }
                let lease = self.gpu.first().unwrap_or(&self.execution);
                if !present(lease) {
                    return Ok(Value::Null);
                }
                let mut filtered = serde_json::Map::new();
                for key in [
                    "throughputPerSecond",
                    "latencyMs",
                    "queueDepth",
                    "allocationFailure",
                    "foregroundLatencyMs",
                    "progress",
                    "phase",
                    "unit",
                    "backend",
                    "inputTokens",
                    "sequenceTokens",
                    "batchSize",
                    "cpuThreads",
                ] {
                    if let Some(value) = feedback.get(key) {
                        filtered.insert(key.into(), value.clone());
                    }
                }
                Ok(json!({"leaseId": lease["leaseId"], "feedback": filtered}))
            }
            "resources.reservation.feedback" => {
                if args["result"]["status"] == "reported" {
                    self.feedback = args["result"]["feedback"].clone();
                }
                Ok(Value::Null)
            }
            "resources.reservation.adjustment" => {
                self.adjustment(&args["value"], now);
                Ok(Value::Null)
            }
            "resources.reservation.status" => Ok(self.status()),
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_residency_requires_release_receipt_and_unknown_mapping_is_never_relabelled() {
        let mut state = ReservationState::default();
        state
            .apply(
                "resources.reservation.init",
                &json!({"profile": {"id": "test", "maxInputTokens": 512},
            "taskId": "test", "platform": "win32", "logicalCores": 8}),
            )
            .unwrap();
        state.apply("resources.reservation.lease", &json!({"kind": "resident", "lease": {"status": "granted", "leaseId": "resident", "memoryBytes": 1024}})).unwrap();
        state
            .apply("resources.reservation.close", &json!({}))
            .unwrap();
        assert_eq!(state.status()["residentMemoryBytes"], 1024.0);
        state
            .apply(
                "resources.reservation.releaseReceipt",
                &json!({"leaseId": "resident"}),
            )
            .unwrap();
        assert_eq!(state.status()["residentMemoryBytes"], 0.0);
        let reply = state.apply("resources.reservation.lease", &json!({"kind": "gpu", "lease": {"status": "granted", "leaseId": "gpu", "deviceId": 0, "gpuMemoryBytes": 512}})).unwrap();
        assert_eq!(reply["release"], true);
        assert_eq!(
            state.status()["diagnostic"]["code"],
            "GPU_DEVICE_MAPPING_UNVERIFIED"
        );
        assert_eq!(state.status()["gpuMemoryBytes"], 512.0);
    }
}
