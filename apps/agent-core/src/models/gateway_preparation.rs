//! Bounded startup preparation reuses provider transport and native resource admission without generation.
//! 有界启动准备复用供应商通信及原生资源准入，不发送计费生成，不读取桌面偏好或启动外部模型进程。

use super::{
    connections,
    gateway::{self, ModelError},
    gateway_config::{self, ModelConfigStore},
};
use crate::gateway_contract::GatewayConfig;
use futures_util::StreamExt;
use reqwest::Method;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Notify, watch};

const MAX_PROVIDERS: usize = 32;
const DEADLINE: Duration = Duration::from_secs(90);
const RESULT_LIFETIME: Duration = Duration::from_secs(30);

struct PreparationState {
    generation: u64,
    signature: String,
    selected: Value,
    entries: Vec<Value>,
    preparing: bool,
    phase: &'static str,
    started: tokio::time::Instant,
    cancellation: watch::Sender<bool>,
    omitted: usize,
}

pub struct PreparationService {
    config: GatewayConfig,
    state: Mutex<PreparationState>,
    changed: Notify,
    pending_loads: Arc<std::sync::Mutex<HashMap<String, String>>>,
}

impl PreparationService {
    pub fn new(config: GatewayConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: Mutex::new(PreparationState {
                generation: 0,
                signature: String::new(),
                selected: Value::Null,
                entries: vec![],
                preparing: false,
                phase: "idle",
                started: tokio::time::Instant::now(),
                cancellation: watch::channel(false).0,
                omitted: 0,
            }),
            changed: Notify::new(),
            pending_loads: Arc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Accept a queue quickly; request completion never means that a model is already loaded.
    /// 快速接受准备队列；请求接受不等于模型已装载，空选择保留当前明确选择，配置和选择变更使旧结果失效。
    pub async fn schedule(
        self: &Arc<Self>,
        store: ModelConfigStore,
        selection: Value,
        cancellation: watch::Receiver<bool>,
    ) -> Result<Value, ModelError> {
        if *cancellation.borrow() {
            return Err(ModelError::cancelled());
        }
        let document = store.document().await?;
        let providers = document["providers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut state = self.state.lock().await;
        let explicit = !selection["providerId"].is_null() || !selection["modelId"].is_null();
        let selected = if explicit {
            selection.clone()
        } else {
            state.selected.clone()
        };
        if explicit && !valid_selection(&providers, &selected) {
            return Err(ModelError::new(
                "MODEL_SELECTION_UNAVAILABLE",
                "选中的模型不在当前正式连接中。",
                409,
            ));
        }
        let selected = if valid_selection(&providers, &selected) {
            json!({"providerId":selected["providerId"],"modelId":selected["modelId"]})
        } else {
            Value::Null
        };
        let signature = format!(
            "{:x}",
            Sha256::digest(json!([providers, selected]).to_string().as_bytes())
        );
        if state.signature == signature
            && (state.preparing || state.started.elapsed() < RESULT_LIFETIME)
        {
            return Ok(snapshot(&state, false));
        }
        state.cancellation.send_replace(true);
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let mut planned = providers;
        planned.sort_by_key(|provider| provider["providerId"] != selected["providerId"]);
        state.omitted = planned.len().saturating_sub(MAX_PROVIDERS);
        planned.truncate(MAX_PROVIDERS);
        state.entries = planned
            .iter()
            .map(|provider| row(provider, &selected))
            .collect();
        state.signature = signature;
        state.selected = selected.clone();
        state.preparing = !planned.is_empty();
        state.phase = if state.preparing { "preparing" } else { "idle" };
        state.started = tokio::time::Instant::now();
        let (owner, token) = watch::channel(false);
        state.cancellation = owner;
        let response = snapshot(&state, state.preparing);
        if !state.preparing {
            self.changed.notify_waiters();
            return Ok(response);
        }
        drop(state);
        let service = self.clone();
        tokio::spawn(async move {
            service
                .run(generation, planned, selected, token, cancellation)
                .await;
        });
        Ok(response)
    }

    pub async fn snapshot(&self) -> Value {
        let state = self.state.lock().await;
        snapshot(&state, false)
    }

    pub async fn wait_idle(
        &self,
        mut cancellation: watch::Receiver<bool>,
        timeout: Duration,
    ) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if !self.state.lock().await.preparing { return true; }
                tokio::select! { _ = &mut changed => {}, _ = explicit_cancelled(&mut cancellation) => return false }
            }
        }).await.unwrap_or(false)
    }

    /// Join only the selected local preload; completed or unknown preparation still permits a real request.
    /// 仅等待当前选择的本地预载，避免同一权重重复准入；结束或未知回执仍交正式请求核验，不等待其他连接。
    pub async fn wait_model(
        &self,
        provider_id: &str,
        model_id: &str,
        mut cancellation: watch::Receiver<bool>,
    ) -> bool {
        tokio::time::timeout(DEADLINE, async {
            loop {
                if *cancellation.borrow() {
                    return false;
                }
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let waiting = {
                    let state = self.state.lock().await;
                    state.preparing
                        && state.entries.iter().any(|entry| {
                            entry["selected"] == true
                                && entry["local"] == true
                                && entry["providerId"] == provider_id
                                && entry["modelId"] == model_id
                                && matches!(entry["state"].as_str(), Some("queued" | "preparing"))
                        })
                };
                if !waiting {
                    return true;
                }
                tokio::select! {
                    _ = &mut changed => {},
                    _ = explicit_cancelled(&mut cancellation) => return false,
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        state.cancellation.send_replace(true);
        state.generation = state.generation.wrapping_add(1);
        state.preparing = false;
        state.phase = "stopped";
        for entry in &mut state.entries {
            if matches!(entry["state"].as_str(), Some("queued" | "preparing")) {
                entry["state"] =
                    json!(if entry["local"] == true && entry["state"] == "preparing" {
                        "unknown"
                    } else {
                        "cancelled"
                    });
                entry["modelReady"] = Value::Null;
                entry["code"] = json!("MODEL_PREPARATION_CANCELLED");
            }
        }
        self.changed.notify_waiters();
    }

    async fn publish(&self, generation: u64, index: usize, entry: Value) {
        let mut state = self.state.lock().await;
        if state.generation == generation && state.preparing {
            state.entries[index] = entry;
            self.changed.notify_waiters();
        }
    }

    async fn run(
        self: Arc<Self>,
        generation: u64,
        providers: Vec<Value>,
        selected: Value,
        token: watch::Receiver<bool>,
        mut external: watch::Receiver<bool>,
    ) {
        let mut lifetime = token.clone();
        let worker = self.clone();
        let work = async move {
            let first_selected = providers
                .first()
                .is_some_and(|provider| provider["providerId"] == selected["providerId"]);
            if first_selected {
                worker.prepare_one(generation, 0, &providers[0], &selected, token.clone())
                    .await;
            }
            let mut pending = futures_util::stream::iter(
                providers
                    .into_iter()
                    .enumerate()
                    .skip(usize::from(first_selected))
                    .map(|(index, provider)| {
                        let worker = worker.clone();
                        let selected = selected.clone();
                        let token = token.clone();
                        async move {
                            worker.prepare_one(generation, index, &provider, &selected, token).await
                        }
                    }),
            )
            .buffer_unordered(2);
            while pending.next().await.is_some() {}
        };
        let code = tokio::select! {
            _ = work => None,
            _ = explicit_cancelled(&mut lifetime) => Some("MODEL_PREPARATION_CANCELLED"),
            _ = explicit_cancelled(&mut external) => Some("MODEL_PREPARATION_CANCELLED"),
            _ = tokio::time::sleep(DEADLINE) => Some("MODEL_PREPARATION_TIMEOUT"),
        };
        let mut state = self.state.lock().await;
        if state.generation != generation {
            return;
        }
        state.cancellation.send_replace(true);
        state.preparing = false;
        state.phase = "idle";
        if let Some(code) = code {
            for entry in &mut state.entries {
                if matches!(entry["state"].as_str(), Some("queued" | "preparing")) {
                    entry["state"] =
                        json!(if entry["local"] == true && entry["state"] == "preparing" {
                            "unknown"
                        } else {
                            "cancelled"
                        });
                    entry["modelReady"] = Value::Null;
                    entry["code"] = json!(code);
                }
            }
        }
        self.changed.notify_waiters();
    }

    async fn prepare_one(
        &self,
        generation: u64,
        index: usize,
        provider: &Value,
        selected: &Value,
        token: watch::Receiver<bool>,
    ) {
        let mut entry = row(provider, selected);
        entry["state"] = json!("preparing");
        self.publish(generation, index, entry.clone()).await;
        let started = tokio::time::Instant::now();
        let preparation = if local(provider) {
            self.prepare_local(
                provider,
                entry["modelId"].as_str().unwrap_or(""),
                entry["selected"] == true,
                token,
            )
        } else {
            self.prepare_api(provider, entry["modelId"].as_str().unwrap_or(""), token)
        };
        // Both branches use boxed futures so their shared timeout owns the complete network/resource attempt.
        // 两类准备共用整个网络和准入尝试的截止，不把初始观测成功冒充模型装载完成。
        let result = tokio::time::timeout(Duration::from_secs(60), preparation).await;
        match result {
            Ok(Ok(observed)) => {
                for (key, value) in observed.as_object().unwrap() {
                    entry[key] = value.clone();
                }
            }
            Ok(Err(error)) => {
                entry["state"] = json!(if error.code == "ABORT_ERR" || local(provider) {
                    "unknown"
                } else {
                    "failed"
                });
                entry["code"] = json!(error.code);
                entry["upstreamStatus"] = error.details["upstreamStatus"].clone();
            }
            Err(_) => {
                entry["state"] = json!("unknown");
                entry["code"] = json!("MODEL_PREPARATION_TIMEOUT");
            }
        }
        entry["checkedAt"] = json!(chrono::Utc::now().to_rfc3339());
        entry["durationMs"] = json!(started.elapsed().as_millis() as u64);
        self.publish(generation, index, entry).await;
    }

    fn prepare_api<'a>(
        &'a self,
        connection: &'a Value,
        model: &'a str,
        token: watch::Receiver<bool>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, ModelError>> + Send + 'a>>
    {
        Box::pin(async move {
            match gateway_config::discover_models(connection, token).await {
                Ok(models) => {
                    let available = if model.is_empty() {
                        !models.as_array().is_none_or(Vec::is_empty)
                    } else {
                        models
                            .as_array()
                            .is_some_and(|models| models.iter().any(|value| value == model))
                    };
                    Ok(
                        json!({"state":if available{"ready"}else{"unavailable"},"connectionReady":true,"modelReady":null,
                        "available":available,"preparationKind":"api-connection","inferenceVerified":false,
                        "availableModelCount":models.as_array().map(Vec::len),"code":if available{Value::Null}else{json!("MODEL_NOT_AVAILABLE")}}),
                    )
                }
                Err(error)
                    if matches!(error.details["upstreamStatus"].as_u64(), Some(404 | 405)) =>
                {
                    Ok(
                        json!({"state":"unknown","connectionReady":if error.details["connectionWarmed"]==true{json!(true)}else{Value::Null},"modelReady":null,"available":null,
                        "preparationKind":"api-connection","inferenceVerified":false,"code":"MODEL_DISCOVERY_UNSUPPORTED"}),
                    )
                }
                Err(error) => Err(error),
            }
        })
    }

    fn prepare_local<'a>(
        &'a self,
        connection: &'a Value,
        model: &'a str,
        selected: bool,
        token: watch::Receiver<bool>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, ModelError>> + Send + 'a>>
    {
        Box::pin(async move {
            let observed =
                gateway_config::observe_local_fresh(connection, model, None, token.clone()).await?;
            if observed["backend"] == "ollama" && observed["loaded"] == true {
                reconcile_preload(&self.pending_loads, &preload_key(connection, model)).await;
                return Ok(local_result(
                    "ready",
                    Some(true),
                    Some(true),
                    Some(true),
                    None,
                ));
            }
            if observed["backend"] == "llama.cpp" {
                let models = gateway_config::discover_models(connection, token.clone()).await?;
                if !models
                    .as_array()
                    .is_some_and(|models| models.iter().any(|value| value == model))
                {
                    return Ok(local_result(
                        "unavailable",
                        Some(true),
                        Some(false),
                        Some(false),
                        Some("MODEL_NOT_AVAILABLE"),
                    ));
                }
                if observed["loaded"] == true {
                    return Ok(local_result(
                        "ready",
                        Some(true),
                        Some(true),
                        Some(true),
                        None,
                    ));
                }
                // Discovery proves availability; a healthy server alone does not prove this model's residency.
                // 目录匹配证明模型可用；服务健康不能证明该模型已驻留，只有新鲜loaded观测允许就绪。
                return Ok(local_result(
                    "unknown",
                    Some(true),
                    observed["loaded"].as_bool(),
                    Some(true),
                    Some("LOCAL_MODEL_RESIDENCY_UNCONFIRMED"),
                ));
            }
            if observed["backend"] != "ollama" || observed["diagnostic"]["metadataPartial"] == true
            {
                let models = gateway_config::discover_models(connection, token).await?;
                let available = models
                    .as_array()
                    .is_some_and(|models| models.iter().any(|value| value == model));
                return Ok(local_result(
                    "unknown",
                    Some(true),
                    None,
                    Some(available),
                    Some("LOCAL_MODEL_PRELOAD_UNSUPPORTED"),
                ));
            }
            if !selected {
                return Ok(local_result(
                    "available",
                    Some(true),
                    observed["loaded"].as_bool(),
                    Some(true),
                    Some("LOCAL_MODEL_NOT_SELECTED"),
                ));
            }
            if !observed["modelFileBytes"]
                .as_f64()
                .is_some_and(|bytes| bytes > 0.0)
            {
                return Ok(local_result(
                    "unavailable",
                    Some(true),
                    Some(false),
                    Some(false),
                    Some("LOCAL_MODEL_NOT_INSTALLED"),
                ));
            }
            let load_key = preload_key(connection, model);
            if self.pending_loads.lock().unwrap().contains_key(&load_key) {
                return Ok(local_result(
                    "unknown", Some(true), observed["loaded"].as_bool(), Some(true),
                    Some("LOCAL_MODEL_PRELOAD_PENDING"),
                ));
            }
            let Some(service) = crate::resources::gateway::global() else {
                return Ok(local_result(
                    "unknown",
                    Some(true),
                    Some(false),
                    Some(true),
                    Some("RESOURCE_RUNTIME_UNAVAILABLE"),
                ));
            };
            let context =
                gateway_config::automatic_context(connection, model, &observed)?["effectiveTokens"]
                    .as_u64()
                    .ok_or_else(|| {
                        ModelError::new("INVALID_CONTEXT_WINDOW", "模型准备未得到有效窗口。", 502)
                    })?;
            let hardware = service
                .request("snapshot", json!({}), Duration::from_secs(5))
                .await
                .map_err(|error| ModelError::new(&error.code, &error.message, error.status))?;
            let demand = crate::resources::calculate(
                "resources.externalDemand",
                &json!({"snapshot":observed,"contextTokens":context,"hardware":hardware}),
            )
            .map_err(ModelError::native)?;
            if demand["state"] == "unknown" || demand["requiresAdmission"] != true {
                return Ok(local_result(
                    "unknown",
                    Some(true),
                    Some(false),
                    Some(true),
                    Some("LOCAL_MODEL_ADMISSION_UNOBSERVED"),
                ));
            }
            let config = self.config.clone();
            let _ = tokio::task::spawn_blocking(move || {
                crate::retrieval::gateway_embedding::retire_idle_models(&config)
            })
            .await;
            let lease = service.acquire(json!({"taskId":format!("model-preparation-{}",uuid::Uuid::new_v4()),
                "workspaceId":"model-preparation","kind":"foreground","workload":"compute","cpuThreads":1,
                "memoryBytes":demand["memoryBytes"],"gpuMemoryBytes":demand["gpuMemoryBytes"],"waitMs":30000,"ttlMs":30000}),token.clone()).await
                .map_err(|error|ModelError::new(&error.code,&error.message,error.status))?;
            if *token.borrow() {
                lease.release().await;
                return Err(ModelError::cancelled());
            }
            let activity = gateway_config::begin_local_generation(connection, model)?;
            let mut preload = PendingPreload::new(
                lease, self.pending_loads.clone(), load_key, connection.clone(), model.into(), context,
            )?;
            // Empty preload has no business prompt; never pull, unload, or request permanent residency.
            // 空预载不含业务提示；不拉取、不卸载、不要求永久驻留，只加载已确认安装的当前选择。
            let result = json_request(
                connection,
                "/api/generate",
                Some(json!({"model":model,"stream":false,"options":{"num_ctx":context}})),
                token.clone(),
            )
            .await;
            let loaded = match result {
                Ok(result) if result["done"] == true => {
                    // A completed load receipt settles the increment even if the following telemetry fails.
                    // 装载完成回执已确定该增量操作结束，后续遥测失败不意味着装载仍在进行。
                    preload.complete().await;
                    gateway_config::observe_local_fresh(connection, model, Some(context), token)
                        .await
                }
                Ok(_) => Err(ModelError::new(
                    "LOCAL_MODEL_PRELOAD_INCOMPLETE",
                    "本地模型未返回装载完成回执。",
                    502,
                )),
                Err(error) => {
                    if error.details["upstreamStatus"].as_u64()
                        .is_some_and(|status| (400..500).contains(&status) && status != 408)
                    {
                        preload.complete().await;
                    }
                    Err(error)
                }
            };
            drop(activity);
            let loaded = loaded?;
            Ok(local_result(
                if loaded["loaded"] == true {
                    "ready"
                } else {
                    "unknown"
                },
                Some(true),
                loaded["loaded"].as_bool(),
                Some(true),
                None,
            ))
        })
    }
}

fn preload_key(connection: &Value, model: &str) -> String {
    let origin = url::Url::parse(connection["baseUrl"].as_str().unwrap_or(""))
        .map(|url| url.origin().ascii_serialization()).unwrap_or_default();
    format!("{:x}", Sha256::digest(json!([origin, model]).to_string().as_bytes()))
}

fn remove_pending(pending: &std::sync::Mutex<HashMap<String, String>>, key: &str, id: &str) {
    let mut pending = pending.lock().unwrap();
    if pending.get(key).is_some_and(|current| current == id) { pending.remove(key); }
}

async fn reconcile_preload(pending: &std::sync::Mutex<HashMap<String, String>>, key: &str) {
    let id = pending.lock().unwrap().get(key).cloned();
    if let (Some(id), Some(service)) = (id, crate::resources::gateway::global()) {
        if service.request("release", json!({"leaseId":id}), Duration::from_secs(5))
            .await.is_ok_and(|receipt| receipt["status"] == "released")
        {
            remove_pending(pending, key, &id);
        }
    }
}

/// Own a dispatched external load until its receipt or fresh residency settles the reservation.
/// 外部装载派发后，以完成回执或新鲜驻留观测核销预约；调用方取消不能证明外部服务停止。
struct PendingPreload {
    lease: Option<crate::resources::gateway::ResourceLease>,
    pending: Arc<std::sync::Mutex<HashMap<String, String>>>,
    key: String,
    connection: Value,
    model: String,
    context: u64,
}
impl PendingPreload {
    fn new(
        mut lease: crate::resources::gateway::ResourceLease,
        pending: Arc<std::sync::Mutex<HashMap<String, String>>>,
        key: String, connection: Value, model: String, context: u64,
    ) -> Result<Self, ModelError> {
        {
            let mut loads = pending.lock().unwrap();
            if loads.contains_key(&key) || loads.len() >= MAX_PROVIDERS {
                return Err(ModelError::new("LOCAL_MODEL_PRELOAD_PENDING", "已有装载尚未核验，暂不重复派发。", 409));
            }
            loads.insert(key.clone(), lease.allocation["leaseId"].as_str().unwrap().into());
        }
        lease.retain_unknown_on_drop();
        Ok(Self { lease: Some(lease), pending, key, connection, model, context })
    }

    async fn complete(&mut self) {
        let Some(lease) = self.lease.as_mut() else { return; };
        if lease.release_confirmed().await.is_ok() {
            remove_pending(&self.pending, &self.key, lease.allocation["leaseId"].as_str().unwrap());
            self.lease.take();
        }
    }
}
impl Drop for PendingPreload {
    fn drop(&mut self) {
        let Some(mut lease) = self.lease.take() else { return; };
        let pending = self.pending.clone();
        let key = self.key.clone();
        let connection = self.connection.clone();
        let model = self.model.clone();
        let context = self.context;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let (_owner, token) = watch::channel(false);
                let verify = async {
                    loop {
                        if gateway_config::observe_local_fresh(&connection, &model, Some(context), token.clone())
                            .await.is_ok_and(|observed| observed["loaded"] == true)
                            && lease.release_confirmed().await.is_ok()
                        {
                            remove_pending(&pending, &key, lease.allocation["leaseId"].as_str().unwrap());
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                };
                // Verification is bounded; an unsettled lease expires into the existing quarantine ledger.
                // 核验有界；仍未知的租约停止续租后进入既有隔离账本，后续新鲜驻留观测可受控核销。
                let _ = tokio::time::timeout(Duration::from_secs(60), verify).await;
            });
        }
    }
}

fn valid_selection(providers: &[Value], selected: &Value) -> bool {
    selected["providerId"]
        .as_str()
        .is_some_and(|id| !id.is_empty())
        && connections::valid_model(&selected["modelId"])
        && providers.iter().any(|provider| {
            provider["providerId"] == selected["providerId"]
                && provider["models"]
                    .as_array()
                    .is_some_and(|models| models.contains(&selected["modelId"]))
        })
}
fn local(connection: &Value) -> bool {
    url::Url::parse(connection["baseUrl"].as_str().unwrap_or(""))
        .ok()
        .and_then(|url| url.host_str().map(connections::local_host))
        .unwrap_or(false)
}
fn row(provider: &Value, selected: &Value) -> Value {
    let current = provider["providerId"] == selected["providerId"] && !selected.is_null();
    let model = if current {
        selected["modelId"].as_str()
    } else {
        provider["models"]
            .as_array()
            .and_then(|models| models.first())
            .and_then(Value::as_str)
    };
    json!({"providerId":provider["providerId"].as_str().unwrap_or(""),"modelId":model,"selected":current,"local":local(provider),
        "state":"queued","connectionReady":null,"modelReady":null,"available":null,"inferenceVerified":false,
        "code":null,"checkedAt":null,"durationMs":0})
}
fn snapshot(state: &PreparationState, scheduled: bool) -> Value {
    json!({"scheduled":scheduled,"isPreparing":state.preparing,"phase":state.phase,"selected":state.selected,
        "models":state.entries,"omittedProviders":state.omitted,"maximumConcurrentChecks":2,"deadlineMs":90000,
        "generationVerified":false})
}
fn local_result(
    state: &str,
    connection: Option<bool>,
    loaded: Option<bool>,
    available: Option<bool>,
    code: Option<&str>,
) -> Value {
    json!({"state":state,"connectionReady":connection,"modelReady":loaded,"available":available,
        "preparationKind":"local-model","inferenceVerified":false,"code":code})
}

async fn explicit_cancelled(cancellation: &mut watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow_and_update() {
            return;
        }
        if cancellation.changed().await.is_err() {
            // A completed 202 caller does not cancel its accepted background preparation.
            // 202调用者正常结束不取消已接受的后台准备；显式取消、服务关闭和截止仍负责回收。
            std::future::pending::<()>().await;
        }
    }
}
async fn json_request(
    connection: &Value,
    path: &str,
    body: Option<Value>,
    mut token: watch::Receiver<bool>,
) -> Result<Value, ModelError> {
    let base = url::Url::parse(connection["baseUrl"].as_str().unwrap_or(""))
        .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "模型地址无效。", 400))?;
    let url = base
        .join(path)
        .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "模型准备地址无效。", 400))?;
    let mut request = gateway::authenticated(
        &gateway::client()?,
        connection,
        if body.is_some() {
            Method::POST
        } else {
            Method::GET
        },
        url.as_str(),
    );
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = tokio::select! { _=gateway::cancelled(&mut token)=>return Err(ModelError::cancelled()),
    response=request.send()=>response.map_err(|_|ModelError::new("MODEL_PREPARATION_NETWORK_FAILED","模型准备接口无法连接。",502))? };
    if !response.status().is_success() {
        let mut error = ModelError::new(
            "MODEL_PREPARATION_HTTP_ERROR",
            "模型准备接口未成功响应。",
            502,
        );
        error.details["upstreamStatus"] = json!(response.status().as_u16());
        return Err(error);
    }
    gateway::bounded_json(response, 1024 * 1024, &mut token).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Request, State},
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn config(root: &std::path::Path) -> GatewayConfig {
        GatewayConfig {
            data_home: root.to_path_buf(),
            data_root: root.join("Data"),
            extension_root: root.join("Tools"),
            application_root: root.to_path_buf(),
            port: 0,
            toolhost_exe: root.join("unused-host.exe"),
            vector_extension: root.join("unused-vector.dll"),
            inference_library: root.join("unused-inference.dll"),
        }
    }
    #[derive(Clone)]
    struct Fixture {
        generations: Arc<AtomicUsize>,
        missing: bool,
        discovery_status: StatusCode,
    }
    async fn handler(State(fixture): State<Fixture>, request: Request) -> Response {
        let path = request.uri().path().to_string();
        if matches!(
            path.as_str(),
            "/api/generate" | "/chat/completions" | "/responses" | "/messages"
        ) {
            fixture.generations.fetch_add(1, Ordering::SeqCst);
            return Json(json!({"done":true,"done_reason":"load"})).into_response();
        }
        match path.as_str() {
            "/oversized/models" => (fixture.discovery_status, "x".repeat(64 * 1024 + 1)).into_response(),
            "/models"=> { tokio::time::sleep(Duration::from_millis(40)).await;
                (fixture.discovery_status,Json(json!({"data":[{"id":"fixture-model"}]}))).into_response() },
            "/props"=> (StatusCode::NOT_FOUND,Json(json!({}))).into_response(),
            "/api/ps"=>Json(json!({"models":[]})).into_response(),
            "/api/show"=>Json(json!({"model_info":{},"details":{}})).into_response(),
            "/api/tags"=>Json(json!({"models":[{"name":"fixture-model","size":if fixture.missing{0}else{1_000_000}}]})).into_response(),
            _=>(StatusCode::NOT_FOUND,Json(json!({}))).into_response(),
        }
    }
    async fn fixture(
        missing: bool,
        status: StatusCode,
    ) -> (Value, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let generations = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback(handler).with_state(Fixture {
            generations: generations.clone(),
            missing,
            discovery_status: status,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection = json!({"providerId":"fixture","protocol":"openai-completions","baseUrl":format!("http://{}",listener.local_addr().unwrap()),"models":["fixture-model"],"apiKey":"synthetic-preparation-secret"});
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (connection, generations, task)
    }
    #[tokio::test]
    async fn api_preparation_checks_models_without_generation_and_reports_unsupported_discovery() {
        let service =
            PreparationService::new(config(std::path::Path::new("unused-preparation-fixture")));
        for status in [StatusCode::OK, StatusCode::NOT_FOUND, StatusCode::METHOD_NOT_ALLOWED] {
            let (connection, generations, server) = fixture(false, status).await;
            let (_owner, token) = watch::channel(false);
            let result = service
                .prepare_api(&connection, "fixture-model", token)
                .await
                .unwrap();
            assert_eq!(
                result["state"],
                if status == StatusCode::OK {
                    "ready"
                } else {
                    "unknown"
                }
            );
            assert!(result["modelReady"].is_null());
            assert_eq!(result["connectionReady"], true);
            if status != StatusCode::OK {
                assert_eq!(result["code"], "MODEL_DISCOVERY_UNSUPPORTED");
            }
            assert_eq!(result["inferenceVerified"], false);
            assert!(!result.to_string().contains("synthetic-preparation-secret"));
            if status != StatusCode::OK {
                let mut oversized = connection.clone();
                oversized["baseUrl"] = json!(format!("{}/oversized", connection["baseUrl"].as_str().unwrap()));
                let (_owner, token) = watch::channel(false);
                let not_warmed = service.prepare_api(&oversized, "fixture-model", token).await.unwrap();
                assert!(not_warmed["connectionReady"].is_null());
                assert_eq!(not_warmed["code"], "MODEL_DISCOVERY_UNSUPPORTED");
            }
            assert_eq!(generations.load(Ordering::SeqCst), 0);
            server.abort();
        }
    }
    #[tokio::test]
    async fn llama_model_readiness_requires_matching_discovery_and_explicit_fresh_residency() {
        async fn llama_handler(State(props): State<Value>, request: Request) -> Response {
            match request.uri().path() {
                "/props" => Json(props).into_response(),
                "/models" => Json(json!({"data":[{"id":"fixture-model"}]})).into_response(),
                "/health" => Json(json!({"status":"ok"})).into_response(),
                _ => (StatusCode::NOT_FOUND, Json(json!({}))).into_response(),
            }
        }
        let service =
            PreparationService::new(config(std::path::Path::new("unused-preparation-fixture")));
        for (sleeping, model, ready) in [
            (json!(true), "fixture-model", json!(false)),
            (Value::Null, "fixture-model", Value::Null),
            (json!(false), "fixture-model", json!(true)),
            (json!(false), "stale-configured-model", json!(false)),
        ] {
            let props = json!({"default_generation_settings":{"n_ctx":8192,"params":{}},
                "total_slots":1,"chat_template":"fixture","is_sleeping":sleeping});
            let app = Router::new().fallback(llama_handler).with_state(props);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let connection = json!({"providerId":"fixture","protocol":"openai-completions",
                "baseUrl":format!("http://{}",listener.local_addr().unwrap()),"models":[model]});
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let (_owner, token) = watch::channel(false);
            let result = service
                .prepare_local(&connection, model, true, token)
                .await
                .unwrap();
            assert_eq!(result["connectionReady"], true);
            assert_eq!(result["modelReady"], ready);
            assert_eq!(result["inferenceVerified"], false);
            if ready != true {
                assert_ne!(result["state"], "ready");
            }
            server.abort();
        }
    }
    #[tokio::test]
    async fn ollama_missing_asset_is_not_downloaded_and_no_selection_never_loads_weights() {
        let service =
            PreparationService::new(config(std::path::Path::new("unused-preparation-fixture")));
        let (mut connection, generations, server) = fixture(true, StatusCode::OK).await;
        connection["protocol"] = json!("ollama");
        let (_owner, token) = watch::channel(false);
        let result = service
            .prepare_local(&connection, "fixture-model", true, token.clone())
            .await
            .unwrap();
        assert_eq!(result["state"], "unavailable");
        assert_eq!(result["code"], "LOCAL_MODEL_NOT_INSTALLED");
        assert_eq!(generations.load(Ordering::SeqCst), 0);
        let result = service
            .prepare_local(&connection, "fixture-model", false, token)
            .await
            .unwrap();
        assert_ne!(result["modelReady"], true);
        assert_eq!(generations.load(Ordering::SeqCst), 0);
        server.abort();
    }
    #[tokio::test]
    async fn unconfirmed_preload_is_not_repeated_after_connection_metadata_changes() {
        let service = PreparationService::new(config(std::path::Path::new("unused-preparation-fixture")));
        let (mut connection, generations, server) = fixture(false, StatusCode::OK).await;
        connection["protocol"] = json!("ollama");
        let key = preload_key(&connection, "fixture-model");
        service.pending_loads.lock().unwrap().insert(key.clone(), "isolated-held-lease".into());
        connection["apiKey"] = json!("changed-synthetic-preparation-secret");
        connection["displayName"] = json!("renamed provider");
        assert_eq!(preload_key(&connection, "fixture-model"), key);
        let (_owner, token) = watch::channel(false);
        let result = service.prepare_local(&connection, "fixture-model", true, token).await.unwrap();
        assert_eq!(result["state"], "unknown");
        assert_eq!(result["code"], "LOCAL_MODEL_PRELOAD_PENDING");
        assert_eq!(generations.load(Ordering::SeqCst), 0);
        remove_pending(&service.pending_loads, &key, "stale-lease");
        assert!(service.pending_loads.lock().unwrap().contains_key(&key));
        remove_pending(&service.pending_loads, &key, "isolated-held-lease");
        assert!(service.pending_loads.lock().unwrap().is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn request_waits_only_for_matching_local_preparation_and_unknown_does_not_block_chat() {
        let service =
            PreparationService::new(config(std::path::Path::new("unused-preparation-fixture")));
        {
            let mut state = service.state.lock().await;
            state.preparing = true;
            state.phase = "preparing";
            state.entries = vec![
                json!({"providerId":"fixture","modelId":"fixture-model","selected":true,"local":true,"state":"queued"}),
            ];
        }
        let (_owner, token) = watch::channel(false);
        assert!(
            service
                .wait_model("other", "fixture-model", token.clone())
                .await
        );
        let (_owner, cancelled) = watch::channel(true);
        assert!(
            !service
                .wait_model("fixture", "fixture-model", cancelled)
                .await
        );
        let waiting = {
            let service = service.clone();
            tokio::spawn(async move { service.wait_model("fixture", "fixture-model", token).await })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        {
            let mut state = service.state.lock().await;
            state.entries[0]["state"] = json!("unknown");
        }
        service.changed.notify_waiters();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap()
        );
        assert!(service.state.lock().await.preparing);
    }
    #[tokio::test]
    async fn accepted_preparation_is_singleflight_keeps_explicit_selection_and_shutdown_invalidates_results()
     {
        let root = std::env::temp_dir().join(format!("kynxa-preparation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let (connection, generations, server) = fixture(false, StatusCode::OK).await;
        std::fs::write(
            root.join("connections.json"),
            json!({"version":1,"providers":[connection]}).to_string(),
        )
        .unwrap();
        let service = PreparationService::new(config(&root));
        let (_owner, token) = watch::channel(false);
        let first = service
            .schedule(
                ModelConfigStore::new(&root),
                json!({"providerId":"fixture","modelId":"fixture-model"}),
                token.clone(),
            )
            .await
            .unwrap();
        assert_eq!(first["scheduled"], true);
        let second = service
            .schedule(ModelConfigStore::new(&root), Value::Null, token.clone())
            .await
            .unwrap();
        assert_eq!(second["scheduled"], false);
        assert_eq!(second["selected"]["modelId"], "fixture-model");
        assert!(service.wait_idle(token, Duration::from_secs(5)).await);
        let state = service.snapshot().await;
        assert_eq!(state["phase"], "idle");
        assert_eq!(state["models"][0]["connectionReady"], true);
        assert_eq!(state["models"][0]["inferenceVerified"], false);
        assert!(!state.to_string().contains("synthetic-preparation-secret"));
        assert!(!state.to_string().contains("baseUrl"));
        assert_eq!(generations.load(Ordering::SeqCst), 0);
        let mut changed = connection.clone();
        changed["apiKey"] = json!("changed-synthetic-preparation-secret");
        std::fs::write(
            root.join("connections.json"),
            json!({"version":1,"providers":[changed]}).to_string(),
        )
        .unwrap();
        let (_refresh_owner, refresh_token) = watch::channel(false);
        assert_eq!(
            service
                .schedule(ModelConfigStore::new(&root), Value::Null, refresh_token)
                .await
                .unwrap()["scheduled"],
            true
        );
        service.shutdown().await;
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(service.snapshot().await["phase"], "stopped");
        assert_ne!(service.snapshot().await["models"][0]["state"], "ready");
        let (_owner, token) = watch::channel(true);
        assert_eq!(
            service
                .schedule(ModelConfigStore::new(&root), Value::Null, token)
                .await
                .unwrap_err()
                .code,
            "ABORT_ERR"
        );
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
}
