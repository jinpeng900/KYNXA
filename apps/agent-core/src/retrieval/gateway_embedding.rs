//! Native inference effects consume the existing reservation policy, without a second resource ledger.
//! 原生推理执行复用既有预约策略，不建立第二套资源账本。

use crate::{
    gateway_contract::GatewayConfig,
    models::gateway_inference::{self, EmbedKind, EmbeddingBatch, InferenceOptions},
    resources::{
        gateway::{self as hardware, ResourceLease},
        reservation::ReservationState,
    },
    storage::{ContractError, gateway},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};
pub(crate) use super::gateway_cache::cache_memory_limit;
use super::gateway_cache::{FlightCache, Lookup, memory_grant};

struct InferenceSession {
    policy: ReservationState,
    leases: Vec<ResourceLease>,
    root: PathBuf,
    device: String,
    last_used: Instant,
    retired: bool,
}
struct InferenceObservation {
    state: &'static str,
    phase: &'static str,
    reason: &'static str,
    changed: Instant,
    loaded: bool,
    backend: Value,
    error_code: Option<String>,
    message: Option<String>,
    retry_at: Option<Instant>,
}
impl Default for InferenceObservation {
    fn default() -> Self {
        Self {
            state: "unloaded",
            phase: "idle",
            reason: "not-loaded",
            changed: Instant::now(),
            loaded: false,
            backend: Value::Null,
            error_code: None,
            message: None,
            retry_at: None,
        }
    }
}
struct InferenceOwner {
    session: Mutex<InferenceSession>,
    // Observation shares this owner's lifecycle but never waits for ORT execution or resource admission.
    // 观察状态属于同一模型所有者，查询状态不等待 ORT 执行或资源准入锁。
    observation: Mutex<InferenceObservation>,
}
impl InferenceOwner {
    fn phase(&self, state: &'static str, phase: &'static str, reason: &'static str) {
        if let Ok(mut observation) = self.observation.lock() {
            if observation.loaded {
                return;
            }
            if observation.state != state || observation.phase != phase {
                observation.changed = Instant::now();
            }
            observation.state = state;
            observation.phase = phase;
            observation.reason = reason;
            observation.error_code = None;
            observation.message = None;
            observation.retry_at = None;
        }
    }
    fn ready(&self, backend: Value) {
        if let Ok(mut observation) = self.observation.lock() {
            observation.state = "ready";
            observation.phase = "idle";
            observation.reason = "";
            observation.loaded = true;
            observation.backend = backend;
            observation.error_code = None;
            observation.message = None;
            observation.retry_at = None;
        }
    }
    fn failure(&self, error: &ContractError) {
        if let Ok(mut observation) = self.observation.lock() {
            if observation.loaded {
                return;
            }
            observation.state = if super::gateway::temporary_index_failure(error.code()) {
                "waiting"
            } else {
                "error"
            };
            observation.phase = if observation.state == "waiting" {
                "queued"
            } else {
                "idle"
            };
            observation.reason = if observation.state == "waiting" {
                "resource-busy"
            } else {
                "load-failed"
            };
            observation.error_code = Some(error.code().into());
            observation.message = Some(error.message().into());
            observation.changed = Instant::now();
        }
    }
}
static OWNERS: OnceLock<Mutex<HashMap<String, Arc<InferenceOwner>>>> = OnceLock::new();
static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
pub fn install_runtime(runtime: tokio::runtime::Handle) {
    let _ = RUNTIME.set(runtime);
}
pub fn reserve_graph(
    memory_bytes: u64,
    cancel: &Arc<AtomicBool>,
    background: bool,
) -> Result<ResourceLease, ContractError> {
    let service = hardware::global().ok_or_else(|| {
        gateway::error(
            "图资源服务尚未就绪，精确向量查询仍可用。",
            "RESOURCE_RUNTIME_UNAVAILABLE",
            503,
        )
    })?;
    let token = cancel.clone();
    runtime()?.block_on(async move{
        let(sender,receiver)=tokio::sync::watch::channel(false);let monitor=tokio::spawn(async move{while !token.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(25)).await;}let _=sender.send(true);});
        let result=service.acquire(json!({"taskId":gateway::new_id(),"workspaceId":"local","kind":if background{"background"}else{"foreground"},"cpuThreads":0,"memoryBytes":memory_bytes,"gpuMemoryBytes":0,"ttlMs":30000,"waitMs":if background{20000}else{5000}}),receiver).await;monitor.abort();
        result.map_err(|error|gateway::error(&error.message,"RETRIEVAL_ANN_RESOURCE_LIMIT",error.status))
    })
}
pub fn model_root(config: &GatewayConfig) -> PathBuf {
    let installed = config
        .application_root
        .join("runtime/embedding/builtin-multilingual");
    if installed.is_dir() {
        installed
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../artifacts/runtime/embedding/builtin-multilingual")
    }
}
pub(crate) fn runtime() -> Result<tokio::runtime::Handle, ContractError> {
    tokio::runtime::Handle::try_current()
        .ok()
        .or_else(|| RUNTIME.get().cloned())
        .ok_or_else(|| {
            gateway::error(
                "推理资源运行时尚未就绪。",
                "INFERENCE_RUNTIME_UNAVAILABLE",
                503,
            )
        })
}
fn failure(error: crate::models::gateway::ModelError) -> ContractError {
    let code = match error.code.as_str() {
        "INFERENCE_RESOURCE_BUSY" => "INFERENCE_RESOURCE_BUSY",
        "INFERENCE_SESSION_BUDGET_FULL" => "INFERENCE_SESSION_BUDGET_FULL",
        "INFERENCE_GPU_AUDIT_RESOURCE_REQUIRED" => "INFERENCE_GPU_AUDIT_RESOURCE_REQUIRED",
        "INFERENCE_TOKENIZER_BUDGET_FULL" => "INFERENCE_TOKENIZER_BUDGET_FULL",
        "INFERENCE_RUNTIME_UNAVAILABLE" => "INFERENCE_RUNTIME_UNAVAILABLE",
        "INFERENCE_RUNTIME_NOT_INITIALIZED" => "INFERENCE_RUNTIME_NOT_INITIALIZED",
        "EMBEDDING_ASSET_MISSING" => "EMBEDDING_ASSET_MISSING",
        "EMBEDDING_ASSET_INVALID" => "EMBEDDING_ASSET_INVALID",
        "INFERENCE_CANCELLED" => "INFERENCE_CANCELLED",
        _ => "RETRIEVAL_INFERENCE_FAILED",
    };
    gateway::error(&error.message, code, error.status_code)
}
fn native(
    owner: &mut InferenceSession,
    operation: &str,
    args: Value,
) -> Result<Value, ContractError> {
    owner
        .policy
        .apply(operation, &args)
        .map_err(|message| gateway::error(&message, "INFERENCE_RESERVATION_FAILED", 503))
}
fn release(owner: &mut InferenceSession, index: usize) -> Result<(), ContractError> {
    let receipt = runtime()?
        .block_on(owner.leases[index].release_confirmed())
        .map_err(|error| gateway::error(&error.message, "INFERENCE_RELEASE_UNCONFIRMED", 503))?;
    let lease_id = owner.leases[index].allocation["leaseId"].clone();
    native(
        owner,
        "resources.reservation.releaseReceipt",
        json!({"leaseId":lease_id,"result":receipt}),
    )?;
    owner.leases.remove(index);
    Ok(())
}
fn acquire(
    owner: &mut InferenceSession,
    kind: &str,
    mut request: Value,
    cancel: &Arc<AtomicBool>,
) -> Result<(), ContractError> {
    if request.is_null() {
        return Ok(());
    }
    // Policy numbers retain JavaScript-number compatibility; the native resource DTO requires integers.
    // 策略数值保留历史 Number 合同，原生资源 DTO 使用经校验的整数传输。
    for field in [
        "cpuThreads",
        "memoryBytes",
        "gpuMemoryBytes",
        "ttlMs",
        "waitMs",
    ] {
        if let Some(value) = request.get(field) {
            let number = value
                .as_f64()
                .filter(|number| {
                    number.is_finite()
                        && *number >= 0.0
                        && number.fract() == 0.0
                        && *number <= 9007199254740991.0
                })
                .ok_or_else(|| {
                    gateway::error("资源需求数值无效。", "INFERENCE_INVALID_BUDGET", 400)
                })?;
            request[field] = json!(number as u64);
        }
    }
    request["waitMs"] = json!(if kind == "gpu" { 0 } else { 5000 });
    let service = hardware::global().ok_or_else(|| {
        gateway::error(
            "原生资源服务尚未就绪，词法检索仍可用。",
            "RESOURCE_RUNTIME_UNAVAILABLE",
            503,
        )
    })?;
    let runtime = runtime()?;
    let token = cancel.clone();
    let result = runtime.block_on(async move {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let monitor = tokio::spawn(async move {
            loop {
                if token.load(Ordering::Acquire) {
                    let _ = sender.send(true);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        let result = service.acquire(request, receiver).await;
        monitor.abort();
        result
    });
    match result {
        Ok(lease) => {
            let receipt = native(
                owner,
                "resources.reservation.lease",
                json!({"kind":kind,"lease":lease.allocation}),
            )?;
            owner.leases.push(lease);
            if receipt["release"] == true {
                release(owner, owner.leases.len() - 1)?;
            } else if receipt["registerProcessId"].as_u64().is_some() {
                if let Some(service) = hardware::global() {
                    let last = owner.leases.len() - 1;
                    let registered=runtime.block_on(service.request("registerExecutor",json!({"leaseId":owner.leases[last].allocation["leaseId"],"processId":receipt["registerProcessId"]}),Duration::from_secs(5)))
                    .map_err(|error|gateway::error(&error.message,"INFERENCE_EXECUTOR_UNVERIFIED",error.status))?;
                    if registered["status"] != "registered" {
                        return Err(gateway::error(
                            "推理驻留归属尚未确认。",
                            "INFERENCE_EXECUTOR_UNVERIFIED",
                            503,
                        ));
                    }
                }
            }
            Ok(())
        }
        Err(error) if kind == "gpu" => {
            native(
                owner,
                "resources.reservation.lease",
                json!({"kind":kind,"lease":{"status":"denied","reason":error.code}}),
            )?;
            Ok(())
        }
        Err(error) => Err(gateway::error(
            &error.message,
            "RESOURCE_WAIT_TIMEOUT",
            error.status,
        )),
    }
}
fn owner(
    config: &GatewayConfig,
    local: &Value,
    cold: bool,
) -> Result<Option<Arc<InferenceOwner>>, ContractError> {
    owner_kind(config, local, cold, false)
}
fn owner_kind(
    config: &GatewayConfig,
    local: &Value,
    cold: bool,
    rerank: bool,
) -> Result<Option<Arc<InferenceOwner>>, ContractError> {
    let root = if rerank {
        rerank_root(config)
    } else {
        model_root(config)
    };
    let preference = device_preference(local, rerank);
    let key = owner_key(config, &root, preference);
    let mut owners = OWNERS.get_or_init(Default::default).lock().map_err(|_| {
        gateway::error("推理会话暂时不可用。", "INFERENCE_RUNTIME_UNAVAILABLE", 503)
    })?;
    if let Some(owner) = owners.get(&key) {
        if !cold
            && (owner
                .observation
                .lock()
                .map(|owner| !owner.loaded)
                .unwrap_or(true)
                || owner.session.try_lock().is_err())
        {
            return Ok(None);
        }
        return Ok(Some(owner.clone()));
    }
    if !cold {
        return Ok(None);
    }
    if owners.len() >= 8 {
        return Err(gateway::error(
            "推理会话已达到有界额度。",
            "INFERENCE_SESSION_BUDGET_FULL",
            503,
        ));
    }
    let cores = std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1);
    let mut policy = ReservationState::default();
    policy.apply("resources.reservation.init",&json!({"profile":{"id":if rerank{"builtin-multilingual-reranker"}else{"builtin-multilingual"},"dtype":"q8","dimensions":if rerank{768}else{384},"hiddenSize":if rerank{768}else{384},"attentionHeads":12,"maxInputTokens":512,"files":[{"path":"onnx/model_quantized.onnx","bytes":if rerank{279301077}else{118308185}},{"path":"tokenizer.json","bytes":if rerank{17098079}else{17082730}}]},
        "taskId":gateway::new_id(),"kind":"background","platform":"win32","arch":"x64","devicePreference":preference,"logicalCores":cores,"cpuThreads":local["cpuThreads"].as_u64().unwrap_or((cores/2).clamp(1,32) as u64),"gatewayProcessId":std::process::id()}))
        .map_err(|message|gateway::error(&message,"INFERENCE_RESERVATION_FAILED",503))?;
    policy
        .apply(
            "resources.reservation.executor",
            &json!({"processId":std::process::id()}),
        )
        .map_err(|message| gateway::error(&message, "INFERENCE_EXECUTOR_UNVERIFIED", 503))?;
    let owner = Arc::new(InferenceOwner {
        session: Mutex::new(InferenceSession {
            policy,
            leases: Vec::new(),
            root,
            device: "cpu".into(),
            last_used: Instant::now(),
            retired: false,
        }),
        observation: Mutex::new(InferenceObservation::default()),
    });
    owners.insert(key, owner.clone());
    Ok(Some(owner))
}
fn device_preference(local: &Value, rerank: bool) -> &str {
    local[if rerank {
        "rerankDevicePolicy"
    } else {
        "embeddingDevicePolicy"
    }]
    .as_str()
    .or_else(|| local["devicePreference"].as_str())
    .or_else(|| local["device"].as_str())
    .unwrap_or("auto")
}
/// Owner identity keeps the requested policy even when automatic admission chooses CPU or GPU.
/// 所有者身份保留请求策略，自动准入实际选择 CPU 或 GPU 时仍使用同一键。
fn owner_key(config: &GatewayConfig, root: &Path, preference: &str) -> String {
    format!(
        "{}:{}:{preference}",
        config.data_root.display(),
        root.display()
    )
}
fn rerank_root(config: &GatewayConfig) -> PathBuf {
    let installed = config
        .application_root
        .join("runtime/rerank/builtin-multilingual-reranker");
    if installed.is_dir() {
        installed
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../artifacts/runtime/rerank/builtin-multilingual-reranker")
    }
}

pub fn rerank(
    config: &GatewayConfig,
    query: &str,
    texts: &[String],
    local: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<crate::models::gateway_inference::RerankBatch, ContractError> {
    let observed = owner_kind(config, local, true, true)?.unwrap();
    let mut owner = observed.session.lock().map_err(|_| {
        gateway::error("重排会话暂时不可用。", "INFERENCE_RUNTIME_UNAVAILABLE", 503)
    })?;
    if cancel.load(Ordering::Acquire) {
        return Err(gateway::error("推理已取消。", "INFERENCE_CANCELLED", 499));
    }
    if owner.retired {
        return Err(gateway::error(
            "重排会话已回收。",
            "INFERENCE_SESSION_RETIRED",
            409,
        ));
    }
    native(&mut owner, "resources.reservation.admit", json!({}))?;
    native(&mut owner, "resources.reservation.plan", json!({}))?;
    let plan = native(&mut owner, "resources.reservation.residentPlan", json!({}))?;
    acquire(&mut owner, "resident", plan, &cancel)?;
    let plan = native(&mut owner, "resources.reservation.executionPlan", json!({}))?;
    acquire(&mut owner, "execution", plan, &cancel)?;
    let result = (|| {
        let plan = native(&mut owner, "resources.reservation.gpuPlan", json!({}))?;
        acquire(&mut owner, "gpu", plan, &cancel)?;
        let budget = native(&mut owner, "resources.reservation.grant", json!({}))?;
        let mut options = InferenceOptions {
            device: budget["device"].as_str().unwrap_or("cpu").into(),
            device_id: budget["deviceId"].as_u64().map(|id| id as usize),
            cpu_threads: budget["cpuThreads"].as_u64().unwrap_or(1).max(1) as usize,
            batch_size: budget["batchSize"]
                .as_f64()
                .unwrap_or(20.0)
                .clamp(1.0, 128.0) as usize,
            batch_token_budget: budget["batchTokenBudget"]
                .as_f64()
                .unwrap_or(16384.0)
                .max(512.0) as usize,
            activation_memory_bytes: budget["activationMemoryBytes"]
                .as_f64()
                .unwrap_or(0.0)
                .ceil() as u64,
            gpu_audit_memory_bytes: 0,
        };
        let audit = if options.device == "dml" {
            let requested = gateway_inference::qualification_request(true, options.cpu_threads)
                .map_err(failure)?;
            acquire(
                &mut owner,
                "resident",
                json!({"taskId":gateway::new_id(),"workspaceId":"local","kind":"background","cpuThreads":0,"memoryBytes":requested["memoryBytes"].as_f64().unwrap_or(0.0).ceil()as u64,"gpuMemoryBytes":0,"ttlMs":30000}),
                &cancel,
            )?;
            let last = owner.leases.len() - 1;
            options.gpu_audit_memory_bytes = owner.leases[last].allocation["memoryBytes"]
                .as_u64()
                .unwrap_or(0);
            Some(owner.leases[last].allocation["leaseId"].clone())
        } else {
            None
        };
        gateway_inference::initialize_runtime(&config.inference_library).map_err(failure)?;
        owner.device = options.device.clone();
        let generated =
            gateway_inference::rerank_blocking(&owner.root, query, texts, &options, cancel)
                .map_err(failure);
        if let Some(id) = audit {
            if let Some(index) = owner
                .leases
                .iter()
                .position(|lease| lease.allocation["leaseId"] == id)
            {
                release(&mut owner, index)?;
            }
        }
        let batch = generated?;
        observed.ready(batch.inference_status.clone());
        native(
            &mut owner,
            "resources.reservation.backend",
            json!({"status":batch.inference_status}),
        )?;
        Ok(batch)
    })();
    let idle = native(&mut owner, "resources.reservation.idlePlan", json!({}))?;
    for item in idle.as_array().into_iter().flatten() {
        if let Some(index) = owner
            .leases
            .iter()
            .position(|lease| lease.allocation["leaseId"] == item["leaseId"])
        {
            release(&mut owner, index)?;
        }
    }
    owner.last_used = Instant::now();
    result
}
pub fn fit(
    config: &GatewayConfig,
    documents: &[Value],
    local: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<Value>, ContractError> {
    let owner = owner(config, local, true)?.unwrap();
    let mut owner = owner.session.lock().map_err(|_| {
        gateway::error("推理会话暂时不可用。", "INFERENCE_RUNTIME_UNAVAILABLE", 503)
    })?;
    if owner.retired {
        return Err(gateway::error(
            "推理会话已回收，可在下一步重新准入。",
            "INFERENCE_SESSION_RETIRED",
            409,
        ));
    }
    native(&mut owner, "resources.reservation.admit", json!({}))?;
    let plan = native(
        &mut owner,
        "resources.reservation.residentPlan",
        json!({"tokenizerOnly":true}),
    )?;
    acquire(&mut owner, "resident", plan, &cancel)?;
    let plan = native(&mut owner, "resources.reservation.executionPlan", json!({}))?;
    acquire(&mut owner, "execution", plan, &cancel)?;
    let result = gateway_inference::fit_documents(&owner.root, documents, cancel).map_err(failure);
    let idle = native(&mut owner, "resources.reservation.idlePlan", json!({}))?;
    for item in idle.as_array().into_iter().flatten() {
        if let Some(index) = owner
            .leases
            .iter()
            .position(|lease| lease.allocation["leaseId"] == item["leaseId"])
        {
            release(&mut owner, index)?;
        }
    }
    owner.last_used = Instant::now();
    result
}
const QUERY_CACHE_NAME: &str = "query-vectors";
const SCORE_CACHE_NAME: &str = "rerank-scores";
static QUERY_CACHE: OnceLock<Arc<FlightCache<EmbeddingBatch>>> = OnceLock::new();
static SCORE_CACHE: OnceLock<Arc<FlightCache<RerankValue>>> = OnceLock::new();

enum RerankValue {
    Score { score:f64, profile:String, version:String, device:String, computed_micros:u64 },
    Batch(gateway_inference::RerankBatch),
}
fn query_cache() -> &'static Arc<FlightCache<EmbeddingBatch>> { QUERY_CACHE.get_or_init(FlightCache::new) }
fn score_cache() -> &'static Arc<FlightCache<RerankValue>> { SCORE_CACHE.get_or_init(FlightCache::new) }
fn reclaim_query_cache() { if let Some(cache)=QUERY_CACHE.get() { cache.clear(); } }
fn reclaim_score_cache() { if let Some(cache)=SCORE_CACHE.get() { cache.clear(); } }
pub(crate) fn resize_query_cache(bytes:usize) {query_cache().capacity(bytes);}
pub(crate) fn resize_score_cache(bytes:usize) {score_cache().capacity(bytes);}
pub(crate) fn query_cache_feedback() -> super::cache_budget::Feedback {QUERY_CACHE.get().map_or_else(Default::default,|cache|cache.feedback())}
pub(crate) fn score_cache_feedback() -> super::cache_budget::Feedback {SCORE_CACHE.get().map_or_else(Default::default,|cache|cache.feedback())}

fn cache_cap(local: &Value, name: &str, environment: &str) -> Option<usize> {
    let mib = local[name].as_u64().and_then(|n|usize::try_from(n).ok())
        .or_else(||std::env::var(environment).ok().and_then(|value|value.parse::<usize>().ok()));
    mib.map(|mib|mib.saturating_mul(1024*1024))
}
fn inference_micros(measurements:&[Value]) -> u64 {
    measurements.iter().filter(|sample|sample["phase"]=="hot-inference").filter_map(|sample|sample["latencyMs"].as_f64())
        .filter(|latency|latency.is_finite() && *latency>0.0).fold(0u64,|total,latency|total.saturating_add((latency*1000.0).min(u64::MAX as f64) as u64))
}
fn run_query_inference(config:&GatewayConfig,texts:&[String],local:&Value,cancel:Arc<AtomicBool>,cold:bool) -> Result<Option<EmbeddingBatch>,ContractError> {
    let batch=embed(config,texts,EmbedKind::Query,local,cancel,cold)?;
    if let Some(batch)=&batch {query_cache().measured_compute(inference_micros(&batch.batch_measurements));}
    Ok(batch)
}
fn run_rerank_inference(config:&GatewayConfig,query:&str,texts:&[String],local:&Value,cancel:Arc<AtomicBool>) -> Result<gateway_inference::RerankBatch,ContractError> {
    let batch=rerank(config,query,texts,local,cancel)?;score_cache().measured_compute(inference_micros(&batch.batch_measurements));Ok(batch)
}
fn hash_parts(parts: impl IntoIterator<Item=impl AsRef<[u8]>>) -> String {
    let mut hash=Sha256::new();
    for part in parts { let bytes=part.as_ref(); hash.update((bytes.len() as u64).to_le_bytes()); hash.update(bytes); }
    format!("{:x}",hash.finalize())
}
fn text_key(identity: &str, tag: &str, texts: &[String]) -> String {
    hash_parts(std::iter::once(identity.as_bytes()).chain(std::iter::once(tag.as_bytes())).chain(texts.iter().map(|text|text.as_bytes())))
}

/// Pinned SHA manifests describe the verified resident graph; cheap file stamps reject changed assets.
/// 固定 SHA 清单描述原生加载时已核验的模型；文件时间与长度戳额外隔离磁盘资源变更，无需每次重读大权重。
fn cache_identity(config: &GatewayConfig, scope:&Value, local:&Value, rerank:bool) -> Option<String> {
    if !scope["scopeKeys"].is_array() || scope["scopeKeys"].as_array()?.is_empty() { return None; }
    let root=if rerank { rerank_root(config) } else { model_root(config) };
    let mut stamps=Vec::new();
    for relative in ["config.json","tokenizer_config.json","special_tokens_map.json","tokenizer.json","onnx/model_quantized.onnx"] {
        let metadata=std::fs::symlink_metadata(root.join(relative)).ok()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() { return None; }
        stamps.push(json!([relative,metadata.len(),metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos().to_string()]));
    }
    let metadata=if rerank { gateway_inference::reranker_metadata("cpu").ok()? } else { gateway_inference::embedding_metadata("cpu").ok()? };
    let gpu_metadata=if rerank { gateway_inference::reranker_metadata("dml").ok()? } else { gateway_inference::embedding_metadata("dml").ok()? };
    let mut scopes=scope["scopeKeys"].as_array()?.clone();
    if scopes.iter().any(|key|!key.is_string()) { return None; }
    scopes.sort_by(|a,b|a.as_str().cmp(&b.as_str())); scopes.dedup();
    let scope=json!({"scopeKeys":scopes,"conversationId":scope["conversationId"],"projectId":scope["projectId"],
        "permissionMode":scope["permissionMode"],"permissionRevision":scope["permissionRevision"],
        "workspaceBindingRevision":scope["workspaceBindingRevision"],"workspaceRoot":scope["workspaceRoot"]});
    let runtime=std::fs::symlink_metadata(&config.inference_library).ok()?;
    if !runtime.is_file() || runtime.file_type().is_symlink() { return None; }
    let runtime_stamp=json!([config.inference_library,runtime.len(),runtime.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos().to_string()]);
    let identity=json!({"version":"native-numeric-cache-v1","dataRoot":config.data_root,"root":root,
        "scope":scope,"cpu":metadata,"dml":gpu_metadata,"files":stamps,"policy":device_preference(local,rerank),
        "runtime":runtime_stamp,"projection":if rerank {"text-pair-longest-first-512-sigmoid"} else {"query-prefix-special-tokens-512-reject-overflow-mean-l2-384"}});
    Some(hash_parts([serde_json::to_vec(&identity).ok()?]))
}
fn resident_device(config:&GatewayConfig, local:&Value, rerank:bool) -> Option<String> {
    let root=if rerank { rerank_root(config) } else { model_root(config) };
    let key=owner_key(config,&root,device_preference(local,rerank));
    let observed=OWNERS.get()?.try_lock().ok()?.get(&key)?.clone();
    let observation=observed.observation.try_lock().ok()?;
    if !observation.loaded { return None; }
    observation.backend["device"].as_str().map(str::to_owned)
}
fn cache_receipt(batch:&mut EmbeddingBatch, status:&str) {
    if status=="hit" || status=="shared" {query_cache().saved_compute(inference_micros(&batch.batch_measurements));}
    if !batch.inference_status.is_object() { batch.inference_status=json!({}); }
    batch.inference_status["queryVectorCache"]=json!({"status":status,"memory":query_cache().stats()});
    if status=="hit" || status=="shared" { batch.batch_measurements.clear(); }
}
fn embedding_bytes(batch:&EmbeddingBatch) -> usize {
    batch.vectors.iter().map(|v|v.capacity().saturating_mul(std::mem::size_of::<f32>())).sum::<usize>()
        .saturating_add(batch.vectors.capacity().saturating_mul(std::mem::size_of::<Vec<f32>>()))
        .saturating_add(super::cache::retained_json_bytes(&batch.inference_status))
        .saturating_add(batch.batch_measurements.capacity()*std::mem::size_of::<Value>())
        .saturating_add(batch.batch_measurements.iter().map(super::cache::retained_json_bytes).sum::<usize>())
        .saturating_add(batch.profile_id.capacity()+batch.model_version.capacity()+batch.space_id.capacity()+batch.device.capacity()+std::mem::size_of::<EmbeddingBatch>())
}
fn rerank_bytes(batch:&gateway_inference::RerankBatch) -> usize {
    std::mem::size_of::<RerankValue>()+batch.scores.capacity()*std::mem::size_of::<f64>()
        +batch.profile_id.capacity()+batch.model_version.capacity()+batch.device.capacity()
        +super::cache::retained_json_bytes(&batch.inference_status)
        +batch.batch_measurements.capacity()*std::mem::size_of::<Value>()
        +batch.batch_measurements.iter().map(super::cache::retained_json_bytes).sum::<usize>()
}
fn valid_query_batch(batch:&EmbeddingBatch, count:usize) -> bool {
    batch.vectors.len()==count && batch.dimensions==384
        && batch.vectors.iter().all(|vector|vector.len()==batch.dimensions && vector.iter().all(|value|value.is_finite()))
}
fn retain_scores(identity:&str, query:&str, texts:&[String], batch:&gateway_inference::RerankBatch, generation:u64) {
    if batch.truncated_inputs_count!=0 || batch.scores.len()!=texts.len() || batch.batch_measurements.is_empty() { return; }
    for (text,score) in texts.iter().zip(&batch.scores) {
        if !score.is_finite() { continue; }
        let value=RerankValue::Score {score:*score,profile:batch.profile_id.clone(),version:batch.model_version.clone(),device:batch.device.clone(),
            computed_micros:inference_micros(&batch.batch_measurements)/(texts.len().max(1) as u64)};
        let bytes=std::mem::size_of::<RerankValue>()+batch.profile_id.len()+batch.model_version.len()+batch.device.len();
        let key=text_key(identity,&format!("pair:{}",batch.device),&[query.to_owned(),text.clone()]);
        score_cache().insert_generation(key,Arc::new(value),bytes,generation);
    }
}

/// Only query vectors enter this cache; document batches and warmup keep their original resource path.
/// 仅缓存查询向量；文档批处理与预热继续走原有路径，键不依赖索引代次或聊天请求 ID。
pub fn embed_query_cached(config:&GatewayConfig, texts:&[String], scope:&Value, local:&Value, cancel:Arc<AtomicBool>, cold:bool) -> Result<Option<EmbeddingBatch>,ContractError> {
    let lookup_started=Instant::now();
    let cap=cache_cap(local,"queryVectorCacheMiB","KYNXA_QUERY_VECTOR_CACHE_MIB");
    super::gateway_cache::configure_cache_limit(QUERY_CACHE_NAME,cap);
    if cap==Some(0) {return run_query_inference(config,texts,local,cancel,cold);}
    let identity=cache_identity(config,scope,local,false);
    let device=resident_device(config,local,false);
    if identity.is_none() || (!cold && device.is_none()) { return run_query_inference(config,texts,local,cancel,cold); }
    let identity=identity.unwrap();
    let bytes=32*1024*1024;
    let approved=cache_memory_limit(QUERY_CACHE_NAME,bytes,reclaim_query_cache);
    let generation=query_cache().generation();
    if cancel.load(Ordering::Acquire) { return Err(gateway::error("检索推理已取消。","INFERENCE_CANCELLED",499)); }
    if let Some(device)=device {
        if let Some(batch)=query_cache().get(&text_key(&identity,&format!("ready:{device}"),texts)) {
            let mut batch=batch.as_ref().clone(); cache_receipt(&mut batch,"hit");
            super::gateway_cache::record_local_foreground_latency(lookup_started.elapsed()); return Ok(Some(batch));
        }
    }
    super::gateway_cache::record_local_foreground_latency(lookup_started.elapsed());
    let permit=memory_grant(QUERY_CACHE_NAME);
    let batch = if approved > 0 && permit.is_some() {
        let owned_config=config.clone(); let owned_texts=texts.to_vec(); let owned_local=local.clone();
        let owned_scope=scope.clone(); let owned_identity=identity.clone();
        let input_bytes=owned_texts.iter().map(|text|text.capacity()+std::mem::size_of::<String>()).sum::<usize>()
            .saturating_add(super::cache::retained_json_bytes(local)).saturating_add(8192);
        let computed=query_cache().compute(text_key(&identity,"flight",texts),input_bytes,cancel.clone(),permit,true,move |token| {
            let batch=run_query_inference(&owned_config,&owned_texts,&owned_local,token.clone(),cold)?;
            // Publish the actual-device entry before retiring its flight, closing the completion race.
            // 必须先发布实际设备的就绪条目，再结束共享计算，避免完成瞬间的新请求重复推理。
            if let Some(batch)=batch.as_ref().filter(|batch|valid_query_batch(batch,owned_texts.len()) && !token.load(Ordering::Acquire)) {
                if cache_identity(&owned_config,&owned_scope,&owned_local,false).as_ref()==Some(&owned_identity) {
                    let key=text_key(&owned_identity,&format!("ready:{}",batch.device),&owned_texts);
                    query_cache().insert_generation(key,Arc::new(batch.clone()),embedding_bytes(batch),generation);
                }
            }
            Ok(batch.map(|batch|{let bytes=embedding_bytes(&batch);(batch,bytes)}))
        });
        match computed {
            Ok(Lookup::Value {value:Some(batch),reused}) if !reused || resident_device(config,local,false).as_deref()==Some(batch.device.as_str()) => { let mut batch=batch.as_ref().clone(); cache_receipt(&mut batch,if reused {"shared"}else{"miss"}); Some(batch) },
            Ok(Lookup::Value {value:Some(_),..}) => run_query_inference(config,texts,local,cancel.clone(),cold)?,
            Ok(Lookup::Value {value:None,..}) => None,
            Ok(Lookup::Bypass) => run_query_inference(config,texts,local,cancel.clone(),cold)?,
            Err(error) if error.code()=="CACHE_WORKER_UNAVAILABLE" => run_query_inference(config,texts,local,cancel.clone(),cold)?,
            Err(error) => return Err(error),
        }
    } else { run_query_inference(config,texts,local,cancel.clone(),cold)? };
    if cancel.load(Ordering::Acquire) { return Err(gateway::error("检索推理已取消。","INFERENCE_CANCELLED",499)); }
    if let Some(batch)=batch.as_ref().filter(|batch|valid_query_batch(batch,texts.len()) && !batch.batch_measurements.is_empty()
        && cache_identity(config,scope,local,false).as_ref()==Some(&identity)) {
        let _=cache_memory_limit(QUERY_CACHE_NAME,bytes,reclaim_query_cache);
        let key=text_key(&identity,&format!("ready:{}",batch.device),texts);
        let mut retained=batch.clone();
        if let Some(status)=retained.inference_status.as_object_mut() { status.remove("queryVectorCache"); }
        let owned_bytes=embedding_bytes(&retained); query_cache().insert_generation(key,Arc::new(retained),owned_bytes,generation);
    }
    Ok(batch)
}

/// Numeric pair scores are reusable; candidate selection, fusion, source readback and authorization are not.
/// 仅复用查询与实际片段窗口的数字得分；候选选择、融合、原文读回与授权仍由原链路逐次处理。
pub fn rerank_cached(config:&GatewayConfig, query:&str, texts:&[String], scope:&Value, local:&Value, cancel:Arc<AtomicBool>) -> Result<gateway_inference::RerankBatch,ContractError> {
    let lookup_started=Instant::now();
    let cap=cache_cap(local,"rerankScoreCacheMiB","KYNXA_RERANK_SCORE_CACHE_MIB");
    super::gateway_cache::configure_cache_limit(SCORE_CACHE_NAME,cap);
    if cap==Some(0) {return run_rerank_inference(config,query,texts,local,cancel);}
    let Some(identity)=cache_identity(config,scope,local,true) else { return run_rerank_inference(config,query,texts,local,cancel); };
    let budget=16*1024*1024;
    let _=cache_memory_limit(SCORE_CACHE_NAME,budget,reclaim_score_cache);
    let generation=score_cache().generation();
    let mut scores=vec![None;texts.len()]; let mut descriptor=None;
    if let Some(device)=resident_device(config,local,true) {
        for (index,text) in texts.iter().enumerate() {
            let key=text_key(&identity,&format!("pair:{device}"),&[query.to_owned(),text.clone()]);
            if let Some(value)=score_cache().get(&key) {
                if let RerankValue::Score {score,profile,version,device,computed_micros}=value.as_ref() {
                    score_cache().saved_compute(*computed_micros);
                    scores[index]=Some(*score); descriptor=Some((profile.clone(),version.clone(),device.clone()));
                }
            }
        }
    }
    let missing=scores.iter().enumerate().filter_map(|(index,score)|score.is_none().then_some(index)).collect::<Vec<_>>();
    let hits=texts.len()-missing.len();
    super::gateway_cache::record_local_foreground_latency(lookup_started.elapsed());
    if cancel.load(Ordering::Acquire) { return Err(gateway::error("检索推理已取消。","INFERENCE_CANCELLED",499)); }
    let mut batch=if missing.is_empty() && !texts.is_empty() {
        let (profile_id,model_version,device)=descriptor.unwrap();
        gateway_inference::RerankBatch {scores:Vec::new(),truncated_inputs_count:0,profile_id,model_version,device:device.clone(),
            inference_status:json!({"device":device,"cached":true}),batch_measurements:Vec::new()}
    } else {
        let pending=missing.iter().map(|index|texts[*index].clone()).collect::<Vec<_>>();
        let owned_config=config.clone(); let owned_query=query.to_owned(); let owned_texts=pending.clone(); let owned_local=local.clone();
        let owned_scope=scope.clone(); let owned_identity=identity.clone();
        let input_bytes=query.len()+pending.iter().map(|text|text.capacity()+std::mem::size_of::<String>()).sum::<usize>()
            +super::cache::retained_json_bytes(local)+8192;
        let mut flight_texts=Vec::with_capacity(pending.len()+1); flight_texts.push(query.to_owned()); flight_texts.extend(pending.iter().cloned());
        let flight_key=text_key(&identity,"batch-flight",&flight_texts); drop(flight_texts);
        let permit=memory_grant(SCORE_CACHE_NAME);
        let computed=if permit.is_some() {score_cache().compute(flight_key,input_bytes,cancel.clone(),permit,true,move |token| {
            let batch=run_rerank_inference(&owned_config,&owned_query,&owned_texts,&owned_local,token.clone())?;
            if !token.load(Ordering::Acquire) && cache_identity(&owned_config,&owned_scope,&owned_local,true).as_ref()==Some(&owned_identity) {
                retain_scores(&owned_identity,&owned_query,&owned_texts,&batch,generation);
            }
            let bytes=rerank_bytes(&batch); Ok(Some((RerankValue::Batch(batch),bytes)))
        })} else {Ok(Lookup::Bypass)};
        let mut generated=match computed {
            Ok(Lookup::Value {value:Some(value),reused}) => match value.as_ref() {
                RerankValue::Batch(batch) if !reused || resident_device(config,local,true).as_deref()==Some(batch.device.as_str()) => {
                    let mut batch=batch.clone(); if reused { score_cache().saved_compute(inference_micros(&batch.batch_measurements)); batch.batch_measurements.clear(); } batch
                },
                _=>run_rerank_inference(config,query,&pending,local,cancel.clone())?
            },
            Ok(_) => run_rerank_inference(config,query,&pending,local,cancel.clone())?,
            Err(error) if error.code()=="CACHE_WORKER_UNAVAILABLE" => run_rerank_inference(config,query,&pending,local,cancel.clone())?,
            Err(error) => return Err(error),
        };
        if generated.scores.len()!=missing.len() || descriptor.as_ref().is_some_and(|(profile,version,device)|profile!=&generated.profile_id || version!=&generated.model_version || device!=&generated.device) {
            return run_rerank_inference(config,query,texts,local,cancel);
        }
        let unchanged=cache_identity(config,scope,local,true).as_ref()==Some(&identity);
        let _=cache_memory_limit(SCORE_CACHE_NAME,budget,reclaim_score_cache);
        for (index,score) in missing.iter().zip(generated.scores.iter()) {
            scores[*index]=Some(*score);
        }
        // Truncated-pair counts are never guessed; only confirmed non-truncated pairs are retained.
        // 不猜测批内截断归属，仅缓存确认未截断批次的得分，保留精确截断回执。
        if unchanged { retain_scores(&identity,query,&pending,&generated,generation); }
        generated.scores.clear(); generated
    };
    if cancel.load(Ordering::Acquire) { return Err(gateway::error("检索推理已取消。","INFERENCE_CANCELLED",499)); }
    batch.scores=scores.into_iter().collect::<Option<Vec<_>>>().unwrap_or_default();
    if !batch.inference_status.is_object() { batch.inference_status=json!({}); }
    batch.inference_status["scoreCache"]=json!({"hits":hits,"misses":missing.len(),"memory":score_cache().stats()});
    Ok(batch)
}

pub fn embed(
    config: &GatewayConfig,
    texts: &[String],
    kind: EmbedKind,
    local: &Value,
    cancel: Arc<AtomicBool>,
    cold: bool,
) -> Result<Option<EmbeddingBatch>, ContractError> {
    let Some(owner) = owner(config, local, cold)? else {
        return Ok(None);
    };
    embed_owned(&owner, config, texts, kind, cancel, false)
}
fn embed_owned(
    observed: &InferenceOwner,
    config: &GatewayConfig,
    texts: &[String],
    kind: EmbedKind,
    cancel: Arc<AtomicBool>,
    warm_only: bool,
) -> Result<Option<EmbeddingBatch>, ContractError> {
    let mut owner = observed.session.lock().map_err(|_| {
        gateway::error("推理会话暂时不可用。", "INFERENCE_RUNTIME_UNAVAILABLE", 503)
    })?;
    if cancel.load(Ordering::Acquire) {
        return Err(gateway::error(
            "检索预热已取消。",
            "INFERENCE_CANCELLED",
            499,
        ));
    }
    if warm_only
        && observed
            .observation
            .lock()
            .map(|state| state.loaded)
            .unwrap_or(false)
    {
        return Ok(None);
    }
    let result = (|| {
        if owner.retired {
            return Err(gateway::error(
                "推理会话已回收，可在下一步重新准入。",
                "INFERENCE_SESSION_RETIRED",
                409,
            ));
        }
        observed.phase("waiting", "resource-admission", "resource-admission");
        native(&mut owner, "resources.reservation.admit", json!({}))?;
        native(&mut owner, "resources.reservation.plan", json!({}))?;
        let plan = native(&mut owner, "resources.reservation.residentPlan", json!({}))?;
        acquire(&mut owner, "resident", plan, &cancel)?;
        let plan = native(&mut owner, "resources.reservation.executionPlan", json!({}))?;
        acquire(&mut owner, "execution", plan, &cancel)?;
        let result = (|| {
            let plan = native(&mut owner, "resources.reservation.gpuPlan", json!({}))?;
            acquire(&mut owner, "gpu", plan, &cancel)?;
            let budget = native(&mut owner, "resources.reservation.grant", json!({}))?;
            let mut options = InferenceOptions {
                device: budget["device"].as_str().unwrap_or("cpu").into(),
                device_id: budget["deviceId"].as_u64().map(|id| id as usize),
                cpu_threads: budget["cpuThreads"].as_u64().unwrap_or(1).max(1) as usize,
                batch_size: budget["batchSize"]
                    .as_f64()
                    .unwrap_or(32.0)
                    .clamp(1.0, 128.0) as usize,
                batch_token_budget: budget["batchTokenBudget"]
                    .as_f64()
                    .unwrap_or(16384.0)
                    .max(512.0) as usize,
                gpu_audit_memory_bytes: 0,
                activation_memory_bytes: budget["activationMemoryBytes"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .ceil() as u64,
            };
            let audit = if options.device == "dml" {
                let requested =
                    gateway_inference::qualification_request(false, options.cpu_threads)
                        .map_err(failure)?;
                let memory = requested["memoryBytes"].as_f64().unwrap_or(0.0).ceil() as u64;
                acquire(
                    &mut owner,
                    "resident",
                    json!({"taskId":gateway::new_id(),"workspaceId":"local","kind":"background","cpuThreads":0,"memoryBytes":memory,"gpuMemoryBytes":0,"ttlMs":30000}),
                    &cancel,
                )?;
                let index = owner.leases.len() - 1;
                options.gpu_audit_memory_bytes = owner.leases[index].allocation["memoryBytes"]
                    .as_u64()
                    .unwrap_or(0);
                Some(owner.leases[index].allocation["leaseId"].clone())
            } else {
                None
            };
            observed.phase("loading", "model-loading", "model-loading");
            gateway_inference::initialize_runtime(&config.inference_library).map_err(failure)?;
            owner.device = options.device.clone();
            let generated =
                gateway_inference::embed_with_options(&owner.root, texts, kind, &options, cancel)
                    .map_err(failure);
            if let Some(id) = audit {
                if let Some(index) = owner
                    .leases
                    .iter()
                    .position(|lease| lease.allocation["leaseId"] == id)
                {
                    release(&mut owner, index)?;
                }
            }
            let batch = generated?;
            observed.ready(batch.inference_status.clone());
            native(
                &mut owner,
                "resources.reservation.backend",
                json!({"status":batch.inference_status}),
            )?;
            let mut feedback = batch
                .batch_measurements
                .iter()
                .rev()
                .find(|measurement| measurement["phase"] == "hot-inference")
                .cloned()
                .unwrap_or_else(|| json!({"phase":"cold-load","unit":"tokens","progress":0}));
            feedback["backend"] = json!(batch.device);
            feedback["cpuThreads"] = json!(options.cpu_threads);
            let report = native(
                &mut owner,
                "resources.reservation.report",
                json!({"feedback":feedback}),
            )?;
            if !report.is_null() {
                if let Some(service) = hardware::global() {
                    if let Ok(response) = runtime()?.block_on(service.request(
                        "report",
                        report,
                        Duration::from_secs(5),
                    )) {
                        native(
                            &mut owner,
                            "resources.reservation.feedback",
                            json!({"result":response}),
                        )?;
                    }
                }
            }
            Ok(batch)
        })();
        let idle = native(&mut owner, "resources.reservation.idlePlan", json!({}))?;
        for item in idle.as_array().into_iter().flatten() {
            if let Some(index) = owner
                .leases
                .iter()
                .position(|lease| lease.allocation["leaseId"] == item["leaseId"])
            {
                release(&mut owner, index)?;
            }
        }
        owner.last_used = Instant::now();
        result.map(Some)
    })();
    if let Err(error) = &result {
        observed.failure(error);
    }
    result
}
pub fn retire_idle_models(config: &GatewayConfig) -> Result<Value, ContractError> {
    super::gateway_cache::reclaim_memory("model-resource-pressure");
    retire_after(config, Duration::ZERO)
}

/// Warm the selected embedding owner under existing background leases; never load a reranker here.
/// 仅用既有后台租约预热选定嵌入模型；执行锁内再次核验就绪状态，不重复预热或加载重排模型。
pub fn warm(
    config: &GatewayConfig,
    local: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<bool, ContractError> {
    let current = status(config, local);
    if current["loaded"] == true {
        return Ok(true);
    }
    if current["available"] != true {
        return Ok(false);
    }
    let observed = owner(config, local, true)?.unwrap();
    embed_owned(
        &observed,
        config,
        &["Local project search readiness".into()],
        EmbedKind::Query,
        cancel,
        true,
    )
    .map(|_| true)
}
pub(crate) fn waiting_retry(config: &GatewayConfig, local: &Value, delay: Duration) {
    if let Some(owner) = OWNERS
        .get()
        .and_then(|owners| owners.lock().ok())
        .and_then(|owners| {
            owners
                .get(&owner_key(
                    config,
                    &model_root(config),
                    device_preference(local, false),
                ))
                .cloned()
        })
    {
        if let Ok(mut observation) = owner.observation.lock() {
            observation.retry_at = Some(Instant::now() + delay);
        }
    }
}
pub fn retire_after(
    config: &GatewayConfig,
    minimum_idle: Duration,
) -> Result<Value, ContractError> {
    let owners = OWNERS.get_or_init(Default::default);
    let entries = owners
        .lock()
        .map_err(|_| gateway::error("推理会话暂时不可用。", "INFERENCE_RUNTIME_UNAVAILABLE", 503))?
        .iter()
        .filter(|(key, _)| key.starts_with(&format!("{}:", config.data_root.display())))
        .map(|(key, owner)| (key.clone(), owner.clone()))
        .collect::<Vec<_>>();
    let mut retired = 0;
    for (key, owner) in entries {
        let Ok(mut guard) = owner.session.try_lock() else {
            continue;
        };
        if guard.last_used.elapsed() < minimum_idle {
            continue;
        }
        if !gateway_inference::retire_idle(&guard.root, &guard.device).map_err(failure)? {
            continue;
        }
        while !guard.leases.is_empty() {
            let index = guard.leases.len() - 1;
            release(&mut guard, index)?;
        }
        guard.retired = true;
        owners
            .lock()
            .map_err(|_| {
                gateway::error("推理会话暂时不可用。", "INFERENCE_RUNTIME_UNAVAILABLE", 503)
            })?
            .remove(&key);
        retired += 1;
    }
    Ok(json!({"retired":retired,"confirmedUnload":true}))
}

/// Status observes an existing resident owner; reading the settings screen never loads a model.
/// 状态仅观察已有驻留所有者，打开配置界面不会因此冷加载模型。
pub fn status(config: &GatewayConfig, local: &Value) -> Value {
    let profiles = profiles(config);
    let configured = local["embeddingProfileId"]
        .as_str()
        .unwrap_or("builtin-multilingual");
    let selected = profiles["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|profile| profile["id"] == configured && profile["kind"] == "embedding");
    let available = selected.is_some_and(|profile| profile["available"] == true);
    let key = owner_key(config, &model_root(config), device_preference(local, false));
    let observed = OWNERS
        .get()
        .and_then(|owners| owners.lock().ok())
        .and_then(|owners| owners.get(&key).cloned());
    let mut value = json!({"state":if available {"unloaded"} else {"unavailable"},"phase":"idle",
        "reason":if selected.is_none() {"unsupported-profile"} else if !available {"assets-missing"} else {"not-loaded"},
        "profileId":configured,"dimensions":384,"available":available,"loaded":false,
        "elapsedMs":0,"inferenceBackend":null,"errorCode":null,"message":null,"retryAfterMs":null});
    if selected.is_some() {
        if let Some(owner) = observed {
            if let Ok(observation) = owner.observation.lock() {
                value["state"] = json!(observation.state);
                value["phase"] = json!(observation.phase);
                value["reason"] = json!(observation.reason);
                value["loaded"] = json!(observation.loaded);
                value["elapsedMs"] = json!(
                    observation
                        .changed
                        .elapsed()
                        .as_millis()
                        .min(u64::MAX as u128) as u64
                );
                value["inferenceBackend"] = observation.backend.clone();
                value["errorCode"] = json!(observation.error_code);
                value["message"] = json!(observation.message);
                value["retryAfterMs"] = observation
                    .retry_at
                    .map(|until| {
                        json!(
                            until
                                .saturating_duration_since(Instant::now())
                                .as_millis()
                                .min(u64::MAX as u128) as u64
                        )
                    })
                    .unwrap_or(Value::Null);
            }
        }
    }
    if local["enabled"] == false || local["semantic"] == "off" {
        value["reason"] = json!(if local["enabled"] == false {
            "disabled"
        } else {
            "semantic-off"
        });
    }
    value["caches"]=json!({"queryVectors":QUERY_CACHE.get().map(|cache|cache.stats()),"rerankScores":SCORE_CACHE.get().map(|cache|cache.stats()),
        "candidates":super::cache_candidates::status(),"automaticBudget":super::gateway_cache::budget_status()});
    value
}
pub fn profiles(config: &GatewayConfig) -> Value {
    let root = model_root(config);
    let available = root.join("onnx/model_quantized.onnx").is_file()
        && root.join("tokenizer.json").is_file()
        && config.inference_library.is_file();
    let mut values = ["cpu", "dml"]
        .iter()
        .filter_map(|device| gateway_inference::embedding_metadata(device).ok())
        .map(|mut metadata| {
            metadata["id"] = metadata["profileId"].clone();
            metadata["kind"] = json!("embedding");
            metadata["available"] = json!(available);
            metadata
        })
        .collect::<Vec<_>>();
    let root = rerank_root(config);
    let available = root.join("onnx/model_quantized.onnx").is_file()
        && root.join("tokenizer.json").is_file()
        && config.inference_library.is_file();
    values.extend(
        ["cpu", "dml"]
            .iter()
            .filter_map(|device| gateway_inference::reranker_metadata(device).ok())
            .map(|mut metadata| {
                metadata["id"] = metadata["profileId"].clone();
                metadata["kind"] = json!("reranker");
                metadata["available"] = json!(available);
                metadata
            }),
    );
    json!({"profiles":values})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_cache_identity_separates_authority_assets_and_queries_but_not_index_generation() {
        let workspace=PrewarmWorkspace::new();
        let root=workspace.config.application_root.join("runtime/embedding/builtin-multilingual");
        std::fs::create_dir_all(root.join("onnx")).unwrap();
        for path in ["config.json","tokenizer_config.json","special_tokens_map.json","tokenizer.json","onnx/model_quantized.onnx"] {
            std::fs::write(root.join(path),b"fixture").unwrap();
        }
        std::fs::write(&workspace.config.inference_library,b"fixture-runtime").unwrap();
        let scope=json!({"scopeKeys":["project:a","chat:a"],"conversationId":"a","permissionMode":"approval","indexGeneration":1});
        let local=json!({"embeddingDevicePolicy":"auto"});
        let first=cache_identity(&workspace.config,&scope,&local,false).unwrap();
        let mut reordered=scope.clone(); reordered["scopeKeys"]=json!(["chat:a","project:a"]); reordered["indexGeneration"]=json!(99);
        assert_eq!(cache_identity(&workspace.config,&reordered,&local,false).unwrap(),first);
        let mut other=scope.clone(); other["scopeKeys"]=json!(["chat:a","project:b"]);
        assert_ne!(cache_identity(&workspace.config,&other,&local,false).unwrap(),first);
        other=scope.clone(); other["permissionMode"]=json!("read-only");
        assert_ne!(cache_identity(&workspace.config,&other,&local,false).unwrap(),first);
        let mut other_root=workspace.config.clone(); other_root.data_root=workspace.root.join("other-data");
        assert_ne!(cache_identity(&other_root,&scope,&local,false).unwrap(),first);
        assert_ne!(text_key(&first,"ready:cpu",&["query one".into()]),text_key(&first,"ready:cpu",&["query two".into()]));
        assert_ne!(text_key(&first,"ready:cpu",&["query one".into()]),text_key(&first,"ready:dml",&["query one".into()]));
        let pair_a=text_key(&first,"pair:cpu",&["query".into(),"actual window A".into()]);
        assert_ne!(pair_a,text_key(&first,"pair:cpu",&["query".into(),"actual window B".into()]));
        std::fs::write(root.join("tokenizer.json"),b"a different tokenizer fixture").unwrap();
        assert_ne!(cache_identity(&workspace.config,&scope,&local,false).unwrap(),first);
        assert!(!first.contains("query")); assert_eq!(first.len(),64);
    }

    struct PrewarmWorkspace {
        root: PathBuf,
        config: GatewayConfig,
    }

    impl PrewarmWorkspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("kynxa-retrieval-{}", gateway::new_id()));
            std::fs::create_dir_all(&root).unwrap();
            let config = GatewayConfig {
                data_root: root.join("Data"),
                data_home: root.join("Models"),
                extension_root: root.join("Extensions"),
                application_root: root.join("App"),
                port: 0,
                toolhost_exe: root.join("toolhost.exe"),
                vector_extension: root.join("vec0.dll"),
                inference_library: root.join("onnxruntime.dll"),
            };
            Self { root, config }
        }
    }

    impl Drop for PrewarmWorkspace {
        fn drop(&mut self) {
            crate::storage::gateway_memory::stop_candidates(&self.config.data_root);
            let _ = crate::storage::sqlite_queries::apply(
                "storage.index.closeConnections",
                &super::super::gateway_index::arguments(&self.config, json!({})),
            );
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A real installed model is loaded once without projects, user data, downloads or a cloud API.
    /// 使用已有离线模型验证无项目启动预热与驻留复用；不读取用户数据、不下载或调用云端。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires the pinned offline ORT/model and native resource-service assets"]
    async fn native_background_prewarm_without_projects_reuses_owner() {
        let mut workspace = PrewarmWorkspace::new();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let bundle = repository.join("artifacts/runtime/native-desktop/win-x64");
        workspace.config.inference_library = bundle.join("runtime/native/onnxruntime.dll");
        let service = crate::resources::gateway::ResourceService::start(
            &bundle.join("runtime/resource/kynxa-resource-service.exe"),
        )
        .await
        .unwrap();
        crate::resources::gateway::install(service.clone());
        install_runtime(tokio::runtime::Handle::current());
        let mut settings = crate::storage::settings::apply("storage.settings.defaults", &json!({})).unwrap();
        settings["local"]["embeddingDevicePolicy"] = json!("cpu");
        gateway::commit_json(
            &workspace.config.data_root,
            &workspace.config.data_root.join("Retrieval/settings.json"),
            &settings,
            None,
            8 * 1024 * 1024,
        )
        .unwrap();
        let started = std::time::Instant::now();
        super::super::gateway::start_background(&workspace.config).unwrap();
        let mut observed =
            status(&workspace.config, &settings["local"]);
        while observed["loaded"] != true && started.elapsed() < std::time::Duration::from_secs(45) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            observed = status(&workspace.config, &settings["local"]);
            if observed["state"] == "error" {
                break;
            }
        }
        let reused = super::super::gateway::start_background(&workspace.config).unwrap();
        let cleanup_config = workspace.config.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            super::super::gateway::drain_background(&cleanup_config)?;
            retire_idle_models(&cleanup_config)
        })
        .await
        .unwrap();
        service.close().await;
        assert!(cleanup.is_ok(), "cleanup: {cleanup:?}");
        assert_eq!(
            observed["state"], "ready",
            "actual prewarm status: {observed}"
        );
        assert_eq!(observed["loaded"], true);
        assert_eq!(reused["reused"], true);
        assert_eq!(
            status(&workspace.config, &settings["local"])["loaded"],
            false
        );
        eprintln!(
            "native background embedding ready/reused/retired; elapsedMs={}",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn embedding_status_excludes_reranker_and_keeps_auto_cpu_owner_identity() {
        let fixture = std::env::temp_dir().join(format!(
            "kynxa-embedding-status-{}",
            crate::gateway_contract::new_id()
        ));
        let config = GatewayConfig {
            data_home: fixture.clone(),
            data_root: fixture.join("Data"),
            extension_root: fixture.join("Extensions"),
            application_root: fixture.join("Package"),
            port: 0,
            toolhost_exe: fixture.join("ToolHost.exe"),
            vector_extension: fixture.join("vec0.dll"),
            inference_library: fixture.join("onnxruntime.dll"),
        };
        let local = json!({"embeddingDevicePolicy":"auto","rerankDevicePolicy":"auto"});
        let embedding_root = model_root(&config);
        let reranker_root = rerank_root(&config);
        let embedding_key = owner_key(&config, &embedding_root, "auto");
        let reranker_key = owner_key(&config, &reranker_root, "auto");
        let observed = |root: PathBuf, model: &str| {
            let owner = Arc::new(InferenceOwner {
                session: Mutex::new(InferenceSession {
                    policy: ReservationState::default(),
                    leases: Vec::new(),
                    root,
                    device: "cpu".into(),
                    last_used: Instant::now(),
                    retired: false,
                }),
                observation: Mutex::new(InferenceObservation::default()),
            });
            owner.ready(json!({"device":"cpu","gpuValidated":false,"model":model}));
            owner
        };
        OWNERS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(reranker_key.clone(), observed(reranker_root, "BGE"));
        let unloaded = status(&config, &local);
        assert_eq!(unloaded["loaded"], false);
        assert!(unloaded["inferenceBackend"].is_null());

        // An auto owner may execute on CPU; observed device must not replace its lookup policy.
        // auto 所有者可能实际使用 CPU；状态查询不能用实际设备替换所有者的请求策略。
        OWNERS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .insert(embedding_key.clone(), observed(embedding_root, "E5"));
        let loaded = status(&config, &local);
        assert_eq!(loaded["loaded"], true);
        assert_eq!(loaded["inferenceBackend"]["model"], "E5");
        assert_eq!(loaded["inferenceBackend"]["device"], "cpu");
        let observed_owner = OWNERS.get().unwrap().lock().unwrap()[&embedding_key].clone();
        let execution = observed_owner.session.lock().unwrap();
        assert_eq!(status(&config, &local)["state"], "ready");
        assert!(owner(&config, &local, false).unwrap().is_none());
        drop(execution);
        for fallback in [json!({"devicePreference":"auto"}), json!({"device":"auto"})] {
            assert_eq!(status(&config, &fallback)["loaded"], true);
        }
        assert_eq!(
            status(&config, &json!({"embeddingDevicePolicy":"cpu"}))["loaded"],
            false
        );
        let mut owners = OWNERS.get().unwrap().lock().unwrap();
        owners.remove(&embedding_key);
        owners.remove(&reranker_key);
    }

    #[test]
    fn embedding_loading_and_resource_wait_status_never_wait_for_execution_lock() {
        let root = std::env::temp_dir().join(format!(
            "kynxa-embedding-observation-{}",
            crate::gateway_contract::new_id()
        ));
        let config = GatewayConfig {
            data_home: root.clone(),
            data_root: root.join("Data"),
            extension_root: root.join("Extensions"),
            application_root: root.join("App"),
            port: 0,
            toolhost_exe: root.join("toolhost.exe"),
            vector_extension: root.join("vec0.dll"),
            inference_library: root.join("ort.dll"),
        };
        let local = json!({"embeddingDevicePolicy":"cpu"});
        let observed = owner(&config, &local, true).unwrap().unwrap();
        let execution = observed.session.lock().unwrap();
        observed.phase("waiting", "resource-admission", "resource-admission");
        assert_eq!(status(&config, &local)["state"], "waiting");
        observed.phase("loading", "model-loading", "model-loading");
        assert_eq!(status(&config, &local)["state"], "loading");
        observed.failure(&gateway::error(
            "Resource pressure",
            "RESOURCE_WAIT_TIMEOUT",
            503,
        ));
        waiting_retry(&config, &local, Duration::from_secs(5));
        let waiting = status(&config, &local);
        assert_eq!(waiting["state"], "waiting");
        assert_eq!(waiting["errorCode"], "RESOURCE_WAIT_TIMEOUT");
        assert!(waiting["retryAfterMs"].as_u64().unwrap() <= 5000);
        observed.failure(&gateway::error(
            "Invalid embedding asset",
            "EMBEDDING_ASSET_INVALID",
            503,
        ));
        assert_eq!(status(&config, &local)["state"], "error");
        drop(execution);
        let cancelled = Arc::new(AtomicBool::new(true));
        assert_eq!(warm(&config, &local, cancelled).unwrap_or(false), false);
        OWNERS.get().unwrap().lock().unwrap().remove(&owner_key(
            &config,
            &model_root(&config),
            "cpu",
        ));
    }

    #[test]
    fn unavailable_embedding_assets_do_not_create_or_load_an_owner() {
        let fixture = std::env::temp_dir().join(format!(
            "kynxa-embedding-missing-{}",
            crate::gateway_contract::new_id()
        ));
        let config = GatewayConfig {
            data_home: fixture.clone(),
            data_root: fixture.join("Data"),
            extension_root: fixture.join("Extensions"),
            application_root: fixture.join("App"),
            port: 0,
            toolhost_exe: fixture.join("toolhost.exe"),
            vector_extension: fixture.join("vec0.dll"),
            inference_library: fixture.join("ort.dll"),
        };
        let installed = config
            .application_root
            .join("runtime/embedding/builtin-multilingual");
        std::fs::create_dir_all(&installed).unwrap();
        let local = json!({"embeddingDevicePolicy":"cpu"});
        assert!(!warm(&config, &local, Arc::new(AtomicBool::new(false))).unwrap());
        assert_eq!(status(&config, &local)["reason"], "assets-missing");
        assert!(owner(&config, &local, false).unwrap().is_none());
        std::fs::remove_dir_all(&fixture).unwrap();
    }
}
