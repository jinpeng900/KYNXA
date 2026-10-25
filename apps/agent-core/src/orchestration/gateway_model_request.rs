//! Shared model admission, provider dispatch and durable usage settlement.
//! 共享模型准入、供应商派发和正式用量结算。

use super::gateway_runtime::{GatewayRuntime, RequestRun};
use crate::{
    gateway_contract::{GatewayError, new_id},
    models::{gateway as model, gateway_config, gateway_context::SummaryExecutor, gateway_protocol as protocol},
    tools::gateway as tools,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::{Duration, Instant}};
use tokio::sync::{Mutex, mpsc};

pub(crate) fn descriptor(tool: &Value, name: &str) -> Value {
    let wire = crate::models::protocol::wire_name(name);
    json!({"name":name,"wireName":wire,"description":tool["description"],"inputSchema":tool["inputSchema"],
        "annotations":tool["annotations"],"execution":tool["execution"]})
}
/// Project loaded tools into provider schemas without changing IDs or parameters.
/// 将已加载工具投影为供应商声明，保留身份和参数合同。
pub(crate) fn model_tool_declarations(protocol_name: &str, catalog: &[Value]) -> Vec<Value> {
    model_tool_declarations_with_order(protocol_name, catalog,
        std::env::var("KYNXA_BENCHMARK_TOOL_ORDER").as_deref() != Ok("relevance"))
}

fn model_tool_declarations_with_order(protocol_name: &str, catalog: &[Value], stable: bool) -> Vec<Value> {
    // Rank to select and trim tools; stabilize only their wire order.
    // 按相关性选择和裁剪工具，仅稳定发送顺序。
    let mut ordered: Vec<_> = catalog.iter().collect();
    // Only explicit development benchmarks alter wire order, not loaded capabilities.
    // 仅显式开发评测改变发送顺序，已加载能力不变。
    if stable {
        ordered.sort_by(|left, right| left["wireName"].as_str().unwrap_or("")
            .cmp(right["wireName"].as_str().unwrap_or("")));
    }
    ordered
        .iter()
        .map(|tool| {
            let fields = json!({
                "name": tool["wireName"],
                "description": tool["description"],
                "parameters": tool["inputSchema"]
            });
            match protocol_name {
                "anthropic-messages" => json!({
                    "name": tool["wireName"],
                    "description": tool["description"],
                    "input_schema": tool["inputSchema"]
                }),
                "openai-responses" => {
                    let mut fields = fields;
                    fields["type"] = json!("function");
                    fields
                }
                _ => json!({"type": "function", "function": fields}),
            }
        })
        .collect()
}

/// Preserve dispatch and elapsed-time facts even when the provider request fails.
/// 供应商请求失败时也保留真实派发状态和耗时。
pub(crate) struct ModelRequestOutcome {
    pub(crate) result: Result<protocol::ModelResponse, model::ModelError>,
    pub(crate) dispatched: bool,
    pub(crate) duration_ms: u64,
}

/// Read dispatched summary timings without duplicating formal usage settlement.
/// 读取已派发摘要请求的耗时，不另建正式用量结算。
pub(crate) struct SummaryDispatches {
    durations_ms: Arc<Mutex<Vec<u64>>>,
}

impl SummaryDispatches {
    pub(crate) async fn take_durations_ms(&self) -> Vec<u64> {
        std::mem::take(&mut *self.durations_ms.lock().await)
    }
}

/// Root and child summaries retain the same admission, cancellation and usage lifecycle.
/// 根、子任务摘要沿用同一套准入、取消和用量生命周期。
pub(crate) fn summary_executor(
    runtime: &Arc<GatewayRuntime>,
    run: &Arc<RequestRun>,
    ctx: &tools::GatewayToolContext,
    connection: &Value,
    model_name: &str,
    context_window_tokens: u64,
) -> (SummaryExecutor, SummaryDispatches) {
    let runtime = runtime.clone();
    let run = run.clone();
    let ctx = ctx.clone();
    let connection = connection.clone();
    let model_name = model_name.to_owned();
    let durations_ms = Arc::new(Mutex::new(Vec::new()));
    let observations = durations_ms.clone();
    let executor: SummaryExecutor = Arc::new(move |body, cancellation| {
        let runtime = runtime.clone();
        let run = run.clone();
        let ctx = ctx.clone();
        let connection = connection.clone();
        let model_name = model_name.clone();
        let observations = observations.clone();
        Box::pin(async move {
            let ModelRequestOutcome { result, dispatched, duration_ms } = request_model(
                &runtime, &run, &ctx, &connection, &model_name, context_window_tokens,
                body, &[], cancellation, None, None, false,
            ).await;
            if dispatched {
                observations.lock().await.push(duration_ms);
            }
            result
        })
    });
    (executor, SummaryDispatches { durations_ms })
}

/// Meter admission waits separately, including cancellation and early returns.
/// 准入等待单独计时，取消或提前返回也不遗漏。
struct ModelAdmissionWait<'a> {
    run: &'a RequestRun,
    started: Instant,
}

impl Drop for ModelAdmissionWait<'_> {
    fn drop(&mut self) {
        self.run.resource_wait_ms.fetch_add(
            self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// Bind model capabilities to observed runtime files and templates across chat, recovery and children.
/// 聊天、恢复及子任务均按实际运行时文件和模板判定能力。
pub(crate) async fn prepare_managed_connection(
    runtime: &GatewayRuntime,
    connection: &mut Value,
    model_name: &str,
    cancellation: tokio::sync::watch::Receiver<bool>,
    foreground: bool,
) -> Result<(), model::ModelError> {
    if !crate::models::gateway_local_engine::is_managed(connection) { return Ok(()); }
    let local_id = connection["providerId"].as_str().unwrap_or("")
        .trim_start_matches("kynxa-local-").to_owned();
    let metadata = crate::models::gateway_local_models::enabled_model(&runtime.config.data_home, &local_id).await?;
    crate::models::gateway_local_engine::ensure_with_priority(&runtime.config.data_home, connection, model_name, cancellation, foreground).await?;
    let observed = crate::models::gateway_local_engine::status(&runtime.config.data_home, &local_id);
    connection["capabilities"] = crate::models::gateway_local_engine::request_capabilities(&metadata, &observed);
    Ok(())
}

/// Manage owned background residency; forecast external growth without unloading user-managed models.
/// 协调自有后台驻留，仅预测外部增长，不卸载用户自管模型。
pub(crate) async fn request_model(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    ctx: &tools::GatewayToolContext,
    connection: &Value,
    model_name: &str,
    context_window_tokens: u64,
    mut body: Value,
    catalog: &[Value],
    cancellation: tokio::sync::watch::Receiver<bool>,
    events: Option<mpsc::Sender<Value>>,
    cache_policy: Option<Value>,
    is_retry: bool,
) -> ModelRequestOutcome {
    // Local preparation and dispatch must reserve the same negotiated window, including summaries and children.
    // 本地准备与实际派发使用同一协商窗口，摘要和子任务同样适用；不改持久模型配置。
    if protocol::protocol(connection) == "ollama" {
        body["options"]["num_ctx"] = json!(context_window_tokens);
    }
    let cache_policy = Some(cache_policy.unwrap_or_else(|| {
        let scope = json!([runtime.config.data_root.to_string_lossy(), ctx.project_id, run.conversation_id]).to_string();
        protocol::apply_summary_prompt_cache(connection, model_name, &mut body, &scope)
    }));
    // Resource and model-slot queues count as waiting, not execution or provider calls.
    // 资源及模型槽排队计入等待，不计执行时长或供应商调用。
    let admission_wait = ModelAdmissionWait { run, started: Instant::now() };
    let steering_enabled = events.is_some();
    let mut steering = run.steering.subscribe();
    let steering_sequence = ctx.config["activeSteeringSequence"].as_u64()
        .unwrap_or_else(|| *steering.borrow_and_update());
    let managed = match ensure_model_ready(runtime, run, connection, model_name, &body, cancellation.clone()).await {
        Ok(managed) => managed,
        Err(error) => return ModelRequestOutcome { result: Err(error), dispatched: false, duration_ms: 0 },
    };
    // Hold model ownership through dispatch and settlement; phase helpers must not shorten these guards.
    // 模型使用权贯穿派发及结算，分阶段处理不缩短保护范围。
    let _managed_generation = match crate::models::gateway_local_engine::request_guard(
        &runtime.config.data_home, connection, model_name,
    ) {
        Ok(guard) => guard,
        Err(error) => return ModelRequestOutcome { result: Err(error), dispatched: false, duration_ms: 0 },
    };
    let slot = match runtime.model_slots.acquire_with_priority(
        connection, model_name, run.parent_task_id.is_none(),
        !run.is_background_recovery.load(std::sync::atomic::Ordering::Acquire), cancellation.clone(),
    ).await {
        Ok(slot) => slot,
        Err(error) => return ModelRequestOutcome { result: Err(error), dispatched: false, duration_ms: 0 },
    };
    let leases = match acquire_model_resources(
        runtime, run, ctx, connection, model_name, context_window_tokens, managed, cancellation.clone(),
    ).await {
        Ok(leases) => leases,
        Err(error) => return ModelRequestOutcome { result: Err(error), dispatched: false, duration_ms: 0 },
    };
    let generation = match gateway_config::begin_local_generation(connection, model_name) {
        Ok(guard) => guard,
        Err(error) => {
            for lease in leases { lease.release().await; }
            return ModelRequestOutcome { result: Err(error), dispatched: false, duration_ms: 0 };
        }
    };
    let reservation_id = new_id();
    if let Some(journal) = &run.journal {
        if let Err(error) = renew_operational_budget(runtime, run, ctx) {
            return ModelRequestOutcome {
                result: Err(model::ModelError::new(&error.code, &error.message, error.status)),
                dispatched: false, duration_ms: 0,
            };
        }
        if let Err(error) = journal.reserve_model_request(&reservation_id) {
            drop(generation);
            for lease in leases { lease.release().await; }
            return ModelRequestOutcome {
                result: Err(model::ModelError::new(&error.code, &error.message, error.status)),
                dispatched: false, duration_ms: 0,
            };
        }
    }
    drop(admission_wait);
    let mut outcome = dispatch_model(connection, body, catalog, cancellation, events,
        steering, steering_sequence, steering_enabled).await;
    let result = &outcome.result;
    slot.observe(
        result.is_ok() || result.as_ref().is_err_and(|error|error.code=="TASK_USER_STEERED"),
        result
            .as_ref()
            .err()
            .is_some_and(|error| error.status_code == 429 || error.code.contains("RATE_LIMIT")),
    );
    settle_model_usage(run, connection, &reservation_id, &mut outcome, cache_policy, is_retry);
    drop(generation);
    for lease in leases { lease.release().await; }
    outcome
}

async fn ensure_model_ready(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    connection: &Value,
    model_name: &str,
    body: &Value,
    cancellation: tokio::sync::watch::Receiver<bool>,
) -> Result<bool, model::ModelError> {
    // Join this model's preload only; other model preparation cannot block the accepted chat.
    // 仅等待当前模型预载，其他模型准备不阻塞已接受聊天。
    if !runtime
        .model_preparation
        .wait_model(
            connection["providerId"].as_str().unwrap_or(""),
            model_name,
            cancellation.clone(),
        )
        .await
    {
        let error = if *cancellation.borrow() {
            model::ModelError::cancelled()
        } else {
            model::ModelError::new(
                "MODEL_PREPARATION_WAIT_TIMEOUT",
                "当前模型准备尚未结束，已保留任务状态。",
                504,
            )
        };
        return Err(error);
    }
    let managed = crate::models::gateway_local_engine::is_managed(connection);
    if managed {
        if let Err(error) = crate::models::gateway_local_engine::ensure_with_priority(
            &runtime.config.data_home, connection, model_name, cancellation.clone(),
            !run.is_background_recovery.load(std::sync::atomic::Ordering::Acquire),
        ).await {
            return Err(error);
        }
        let local_id = connection["providerId"].as_str().unwrap_or("").trim_start_matches("kynxa-local-");
        let observed = crate::models::gateway_local_engine::status(&runtime.config.data_home, local_id);
        // Recheck capabilities before dispatch after runtime or template changes, without extra HTTP probes.
        // 运行时或模板变化后，派发前重核能力，不增加 HTTP 探测。
        if body["tools"].as_array().is_some_and(|tools| !tools.is_empty())
            && (observed["modelIdentityVerified"] != true || observed["supportsTools"] != true)
        {
            let (code, message) = if observed["supportsTools"] == false {
                ("LOCAL_MODEL_TOOLS_UNSUPPORTED", "当前本地模型模板不支持工具调用，尚未派发本次请求。")
            } else {
                ("LOCAL_MODEL_TOOLS_UNCONFIRMED", "当前本地模型工具能力尚未确认，尚未派发本次请求。")
            };
            return Err(model::ModelError::new(code, message, 409));
        }
    }
    Ok(managed)
}

fn transport_resource_request(request_id: &str, workspace: &str) -> Value {
    // Model-slot admission owns priority; transport uses the lightweight I/O contract.
    // 模型槽负责前后台优先级；传输使用轻量 I/O 准入合同。
    json!({"taskId":request_id,"workspaceId":workspace,"kind":"foreground","workload":"model-transport",
        "cpuThreads":1,"memoryBytes":16777216,"gpuMemoryBytes":0,"waitMs":30000,"ttlMs":30000})
}

async fn acquire_model_resources(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    ctx: &tools::GatewayToolContext,
    connection: &Value,
    model_name: &str,
    context_window_tokens: u64,
    managed: bool,
    cancellation: tokio::sync::watch::Receiver<bool>,
) -> Result<Vec<crate::resources::gateway::ResourceLease>, model::ModelError> {
    let mut leases = Vec::new();
    if let Some(service) = crate::resources::gateway::global() {
        let workspace = ctx.project_id.as_deref().unwrap_or(&run.conversation_id);
        let mut transport = Box::pin(service.acquire_waiting(
            transport_resource_request(&run.request_id, workspace),
            cancellation.clone(),
        ));
        let admitted = tokio::select! {
            result = &mut transport => result,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                let config = runtime.config.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    crate::retrieval::gateway_embedding::retire_idle_models(&config)
                }).await;
                transport.await
            }
        };
        match admitted {
            Ok(lease) => leases.push(lease),
            Err(error) => {
                return Err(model::ModelError::new(&error.code, &error.message, error.status));
            }
        }
        let observed = if managed { None } else { gateway_config::observe_local(
            connection,
            model_name,
            Some(context_window_tokens),
            cancellation.clone(),
        )
        .await.ok() };
        if let Some(snapshot) = observed {
            let hardware = service
                .request("snapshot", json!({}), Duration::from_secs(5))
                .await
                .unwrap_or(Value::Null);
            if let Ok(demand) = crate::resources::calculate(
                "resources.externalDemand",
                &json!({
                    "snapshot": snapshot,
                    "contextTokens": context_window_tokens,
                    "hardware": hardware
                }),
            ) {
                if demand["requiresAdmission"] == true {
                    let config = runtime.config.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        crate::retrieval::gateway_embedding::retire_idle_models(&config)
                    })
                    .await;
                    match service
                        .acquire_waiting(
                            json!({
                                "taskId": run.request_id,
                                "workspaceId": workspace,
                                "kind": "foreground",
                                "workload": "compute",
                                "cpuThreads": 0,
                                "memoryBytes": demand["memoryBytes"],
                                "gpuMemoryBytes": demand["gpuMemoryBytes"],
                                "waitMs": 30000,
                                "ttlMs": 30000
                            }),
                            cancellation.clone(),
                        )
                        .await
                    {
                        Ok(lease) => leases.push(lease),
                        Err(error) => {
                            for lease in leases {
                                lease.release().await;
                            }
                            return Err(model::ModelError::new(&error.code, &error.message, error.status));
                        }
                    }
                }
            }
        }
    }
    Ok(leases)
}

/// Renew implicit operational segments before reserving the next dispatch.
/// 下一次派发预约前，仅续接隐式执行预算段。
fn renew_operational_budget(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    ctx: &tools::GatewayToolContext,
) -> Result<(), GatewayError> {
    for owner in runtime.budget_journals(run)? {
        let Some(mut policy) = owner.operational_budget_policy() else { continue; };
        policy.renew_execution_time &= ctx.config["renewOperationalExecutionBudget"] == true;
        let task = owner.load_current()?;
        let elapsed = if run.journal.as_ref().is_some_and(|journal| owner.task_id() == journal.task_id()) { run.effective_active_ms() }
            else { task.usage.active_duration_ms.unwrap_or(0) };
        let (model_floor, execution_floor) = super::budget::operational_budget_floor(
            policy, task.usage.model_requests, elapsed,
            task.budget.max_model_requests, task.budget.max_execution_ms,
        );
        if task.budget.max_model_requests < model_floor
            || task.budget.max_execution_ms.is_some_and(|limit|execution_floor > limit) {
            owner.repository().extend_operational_budget(owner.token(), model_floor.min(u32::MAX as u64) as u32, execution_floor, "continue-authorized-unfinished-work")?;
        }
    }
    Ok(())
}

async fn dispatch_model(
    connection: &Value,
    body: Value,
    catalog: &[Value],
    cancellation: tokio::sync::watch::Receiver<bool>,
    events: Option<mpsc::Sender<Value>>,
    mut steering: tokio::sync::watch::Receiver<u64>,
    steering_sequence: u64,
    steering_enabled: bool,
) -> ModelRequestOutcome {
    let model_dispatched_at = Instant::now();
    let (model_cancel, model_cancellation) = tokio::sync::watch::channel(false);
    let mut parent_cancellation = cancellation;
    let operation = model::request_with_catalog(connection, body, model_cancellation, events, catalog);
    tokio::pin!(operation);
    let result = tokio::select! {
        biased;
        _ = tools::wait_for_cancellation(&mut parent_cancellation) => {
            model_cancel.send_replace(true);
            operation.await
        }
        _ = async {
            if !steering_enabled { std::future::pending::<()>().await; }
            while *steering.borrow_and_update() == steering_sequence {
                if steering.changed().await.is_err() { std::future::pending::<()>().await; }
            }
        } => {
            // Cancel the model socket only and report dispatch; retain tasks/tools and reject partial arguments.
            // 仅取消模型连接并报告派发状态，保留任务和工具，不执行残缺参数。
            model_cancel.send_replace(true);
            let cancelled = operation.await;
            match cancelled {
                Err(mut error) => {
                    error.code = "TASK_USER_STEERED".into();
                    error.message = "当前生成已按用户引导接续。".into();
                    Err(error)
                }
                response => response,
            }
        }
        result = &mut operation => result,
    };
    let dispatched = match &result {
        Ok(_) => true,
        Err(error) => error.details["requestDispatched"] == true,
    };
    let model_duration_ms = if dispatched {
        model_dispatched_at
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64
    } else {
        0
    };
    ModelRequestOutcome { result, dispatched, duration_ms: model_duration_ms }
}

/// Settle only reported provider usage; a persistence failure retains dispatch uncertainty.
/// 仅结算供应商实际报告的用量，持久化失败仍保留派发未知状态。
fn settle_model_usage(
    run: &RequestRun,
    connection: &Value,
    reservation_id: &str,
    outcome: &mut ModelRequestOutcome,
    cache_policy: Option<Value>,
    is_retry: bool,
) {
    let dispatched = outcome.dispatched;
    let model_duration_ms = outcome.duration_ms;
    let result = &mut outcome.result;
    if let Some(journal) = &run.journal {
        let reported_usage = result.as_ref().ok().map(|response| &response.usage)
            .or_else(|| result.as_ref().err().and_then(|error| error.details.get("reportedUsage")));
        let reported = reported_usage.map(|usage| {
            crate::models::usage::token_counts(
                connection["protocol"].as_str().unwrap_or("openai-completions"),
                usage,
            )
        }).unwrap_or_default();
        let input = reported.input_tokens;
        let output = reported.output_tokens;
        let usage = crate::storage::task_repository::TaskUsage {
            model_requests: u64::from(dispatched),
            failed_requests: u64::from(dispatched && result.as_ref().is_err_and(|error|error.code!="TASK_USER_STEERED")),
            retries: u64::from(dispatched && is_retry && !result.as_ref().is_err_and(|error|error.code=="TASK_USER_STEERED")),
            model_duration_ms,
            input_tokens: input.unwrap_or(0),
            output_tokens: output.unwrap_or(0),
            unknown_token_requests: u64::from(dispatched && (input.is_none() || output.is_none())),
            ..Default::default()
        };
        let provider_usage = dispatched.then(|| {
            let raw = reported_usage.unwrap_or(&Value::Null);
            let cache = crate::models::usage::cache_breakdown(
                connection["protocol"].as_str().unwrap_or("openai-completions"), raw);
            crate::storage::task_repository::ProviderTokenUsage {
                input_tokens: input, output_tokens: output, cache_read_tokens:cache.read_tokens,
                cache_creation_tokens:cache.creation_tokens, uncached_input_tokens:cache.uncached_input_tokens,
                client_metrics: Some(crate::storage::task_repository::ModelRequestClientMetrics {
                    first_visible_text_ms: raw["_kynxaFirstVisibleTextMs"].as_u64(),
                    cache_policy,
                }),
            }
        });
        let settlement = match provider_usage {
            Some(provider) => journal.settle_model_request_with_provider_usage(reservation_id, dispatched, &usage, Some(provider)),
            None => journal.settle_model_request(reservation_id, dispatched, &usage),
        };
        if let Err(error) = settlement {
            // Transmission may have occurred; keep the unknown reservation and do not redispatch.
            // 请求可能已发出，保留未知预约，不重新派发。
            let mut uncertain = model::ModelError::new(&error.code, &error.message, error.status);
            uncertain.details = json!({"requestDispatched":dispatched,"reservationId":reservation_id,"settlementUnconfirmed":true});
            *result = Err(uncertain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_transport_payload_matches_the_live_resource_contract() {
        let request = transport_resource_request("saved-request", "saved-workspace");
        assert_eq!(crate::resources::calculate("resources.validRequest", &json!({"request":request})).unwrap(), true);
        assert_eq!(request["gpuMemoryBytes"], 0);
        assert_eq!(request["kind"], "foreground");
    }

    #[test]
    fn wire_tools_keep_the_same_prefix_without_changing_relevance_selection() {
        let first = json!({"wireName":"k_z","description":"read","inputSchema":{"type":"object"}});
        let second = json!({"wireName":"k_a","description":"find","inputSchema":{"type":"object"}});
        let ranked = vec![first.clone(), second.clone()];
        for protocol in ["openai-responses", "anthropic-messages", "openai-completions"] {
            assert_eq!(model_tool_declarations(protocol, &ranked),
                model_tool_declarations(protocol, &[second.clone(), first.clone()]));
            let relevance = model_tool_declarations_with_order(protocol, &ranked, false);
            let stable = model_tool_declarations_with_order(protocol, &ranked, true);
            assert_eq!(relevance[0], stable[1]);
            assert_eq!(relevance[1], stable[0]);
        }
        assert_eq!(ranked[0], first, "wire canonicalization must not reorder admission/trimming priorities");
    }
}
