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
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct InferenceOwner {
    policy: ReservationState,
    leases: Vec<ResourceLease>,
    root: PathBuf,
    device: String,
    last_used: Instant,
    ready: bool,
    retired: bool,
    backend_status: Value,
}
static OWNERS: OnceLock<Mutex<HashMap<String, Arc<Mutex<InferenceOwner>>>>> = OnceLock::new();
static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
pub fn install_runtime(runtime: tokio::runtime::Handle) {
    let _ = RUNTIME.set(runtime);
}
pub fn reserve_graph(
    memory_bytes: u64,
    cancel: &Arc<AtomicBool>,
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
        let result=service.acquire(json!({"taskId":gateway::new_id(),"workspaceId":"local","kind":"foreground","cpuThreads":0,"memoryBytes":memory_bytes,"gpuMemoryBytes":0,"ttlMs":30000,"waitMs":5000}),receiver).await;monitor.abort();
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
    RUNTIME
        .get()
        .cloned()
        .or_else(|| tokio::runtime::Handle::try_current().ok())
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
    owner: &mut InferenceOwner,
    operation: &str,
    args: Value,
) -> Result<Value, ContractError> {
    owner
        .policy
        .apply(operation, &args)
        .map_err(|message| gateway::error(&message, "INFERENCE_RESERVATION_FAILED", 503))
}
fn release(owner: &mut InferenceOwner, index: usize) -> Result<(), ContractError> {
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
    owner: &mut InferenceOwner,
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
) -> Result<Option<Arc<Mutex<InferenceOwner>>>, ContractError> {
    owner_kind(config, local, cold, false)
}
fn owner_kind(
    config: &GatewayConfig,
    local: &Value,
    cold: bool,
    rerank: bool,
) -> Result<Option<Arc<Mutex<InferenceOwner>>>, ContractError> {
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
        if !cold && owner.try_lock().map(|owner| !owner.ready).unwrap_or(true) {
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
    let owner = Arc::new(Mutex::new(InferenceOwner {
        policy,
        leases: Vec::new(),
        root,
        device: "cpu".into(),
        last_used: Instant::now(),
        ready: false,
        retired: false,
        backend_status: Value::Null,
    }));
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
    let owner = owner_kind(config, local, true, true)?.unwrap();
    let mut owner = owner.lock().map_err(|_| {
        gateway::error("重排会话暂时不可用。", "INFERENCE_RUNTIME_UNAVAILABLE", 503)
    })?;
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
        owner.ready = true;
        owner.backend_status = batch.inference_status.clone();
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
    let mut owner = owner.lock().map_err(|_| {
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
    let mut owner = owner.lock().map_err(|_| {
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
            let requested = gateway_inference::qualification_request(false, options.cpu_threads)
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
        owner.ready = true;
        owner.backend_status = batch.inference_status.clone();
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
                if let Ok(response) =
                    runtime()?.block_on(service.request("report", report, Duration::from_secs(5)))
                {
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
}
pub fn retire_idle_models(config: &GatewayConfig) -> Result<Value, ContractError> {
    retire_after(config, Duration::ZERO)
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
        let Ok(mut guard) = owner.try_lock() else {
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
    let available = profiles["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|profile| profile["id"] == configured)
        .is_some_and(|profile| profile["available"] == true);
    let key = owner_key(config, &model_root(config), device_preference(local, false));
    let observed = OWNERS
        .get()
        .and_then(|owners| owners.lock().ok())
        .and_then(|owners| owners.get(&key).cloned());
    let backend = observed.and_then(|owner| {
        owner
            .try_lock()
            .ok()
            .map(|owner| (owner.ready, owner.backend_status.clone()))
    });
    json!({"state":if backend.as_ref().is_some_and(|value|value.0){"ready"}else if available{"unloaded"}else{"unavailable"},"profileId":configured,"dimensions":384,"available":available,"loaded":backend.as_ref().is_some_and(|value|value.0),"inferenceBackend":backend.map(|value|value.1)})
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
            Arc::new(Mutex::new(InferenceOwner {
                policy: ReservationState::default(),
                leases: Vec::new(),
                root,
                device: "cpu".into(),
                last_used: Instant::now(),
                ready: true,
                retired: false,
                backend_status: json!({"device":"cpu","gpuValidated":false,"model":model}),
            }))
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
}
