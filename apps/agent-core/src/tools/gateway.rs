//! Actual asynchronous tool execution for the native Windows gateway.
//! 原生 Windows 网关的实际异步工具执行；沿用原生规则，不再经过 Node。

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

pub(crate) use super::gateway_mcp::{explicit_tool_error, foreground, reusable_catalog as reusable_mcp_catalog};
pub(crate) use super::gateway_tool_memory::rank_tool_hints;
use super::service::ToolSession;
use crate::gateway_contract::GatewayError;

pub type ExternalTool = Arc<
    dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = Result<Value, GatewayError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct CachedSkillHeader {
    version: super::local_files::FileVersion,
    metadata: Option<Value>,
}

pub struct ToolRuntime {
    pub(crate) native: Arc<Mutex<ToolSession>>,
    pub(crate) mcp:
        tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<super::gateway_mcp::McpClient>>>>,
    pub(crate) browsers: tokio::sync::Mutex<
        HashMap<String, Arc<tokio::sync::Mutex<super::gateway_browser::BrowserSession>>>,
    >,
    pub(crate) workspace_browser:
        tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<super::gateway_browser::WorkspaceBrowser>>>>,
    pub(crate) cdp: tokio::sync::Mutex<HashMap<String, Arc<super::gateway_browser::CdpClient>>>,
    pub(crate) jobs: tokio::sync::Mutex<HashMap<String, super::gateway_process::HostJob>>,
    pub(crate) schemas: tokio::sync::RwLock<Vec<Value>>,
    pub(crate) tool_memory: super::gateway_tool_memory::ToolMemoryRuntime,
    pub(crate) semantic: super::gateway_semantic::SemanticRuntime,
    pub(crate) experience_repository: std::sync::OnceLock<crate::storage::task_repository::TaskRepository>,
    searches: Mutex<HashMap<String, Arc<Mutex<ToolSession>>>>,
    code_versions: Mutex<HashMap<String, Arc<Mutex<super::service_code_versions::CodeVersions>>>>,
    skill_headers: Mutex<HashMap<PathBuf, CachedSkillHeader>>,
    pub(crate) mcp_errors: tokio::sync::Mutex<HashMap<String, Value>>,
    pub(crate) mcp_aborters: Mutex<HashMap<String, watch::Sender<bool>>>,
    pub(crate) mcp_lifecycle: super::gateway_mcp::McpLifecycle,
    pub(crate) web_search: super::gateway_search::SearchState,
    /// Model connection home for built-in sources that reuse a configured provider key.
    /// 模型连接目录，供复用已配置供应商密钥的内置来源按需读取。
    pub(crate) model_home: std::sync::OnceLock<PathBuf>,
    pub(crate) web_fetch: super::gateway_web_snapshot::FetchState,
    pub(crate) preview_grants: Mutex<HashMap<String, super::gateway_workspace_preview::PreviewGrant>>,
    pub(crate) terminals: tokio::sync::Mutex<HashMap<String, Arc<super::gateway_workspace_terminal::TerminalSession>>>,
    configuration_updates: tokio::sync::RwLock<()>,
    configuration_generation: AtomicU64,
}

impl Default for ToolRuntime {
    fn default() -> Self {
        let mut session = ToolSession::default();
        let _ = session.apply("tools.service.host.init", &json!({"maximumRunning":4}));
        Self {
            native: Arc::new(Mutex::new(session)),
            mcp: Default::default(),
            browsers: Default::default(),
            workspace_browser: Default::default(),
            cdp: Default::default(),
            jobs: Default::default(),
            schemas: Default::default(),
            tool_memory: Default::default(),
            semantic: Default::default(),
            experience_repository: Default::default(),
            searches: Default::default(),
            code_versions: Default::default(),
            skill_headers: Default::default(),
            mcp_errors: Default::default(),
            mcp_aborters: Default::default(),
            mcp_lifecycle: Default::default(),
            web_search: Default::default(),
            model_home: Default::default(),
            web_fetch: Default::default(),
            preview_grants: Default::default(),
            terminals: Default::default(),
            configuration_updates: Default::default(),
            configuration_generation: AtomicU64::new(0),
        }
    }
}

impl ToolRuntime {
    pub(crate) fn with_gateway_config(config: &crate::gateway_contract::GatewayConfig) -> Self {
        let runtime = Self::default();
        runtime.semantic.install(config);
        let _ = runtime.model_home.set(config.data_home.clone());
        runtime
    }
}

pub(crate) fn prewarm(context: &GatewayToolContext) -> Value {
    super::gateway_semantic::prewarm(context);
    super::gateway_mcp::prewarm(context)
}

pub(crate) fn experience_identities(context: &GatewayToolContext, names: &[String]) -> Result<Value,GatewayError> {
    super::gateway_tool_memory::experience_identities(context,names)
}

pub(crate) async fn verified_experience_hint(context: &GatewayToolContext, query: &str) -> Value {
    super::gateway_experience::hint(query,context).await
        .unwrap_or_else(|_|json!({"entries":[],"navigationOnly":true}))
}

/// Approval belongs to this invocation, never to a model-supplied explanation.
/// 审批仅属于本次调用；模型生成的 reason、policy 等说明不能授予执行权限。
#[derive(Clone)]
pub struct GatewayToolContext {
    pub data_home: PathBuf,
    pub extension_root: PathBuf,
    pub workdir: PathBuf,
    pub toolhost_exe: PathBuf,
    pub permission_mode: String,
    pub conversation_id: String,
    pub project_id: Option<String>,
    pub authorized_roots: Vec<PathBuf>,
    pub cancellation: watch::Receiver<bool>,
    pub runtime: Arc<ToolRuntime>,
    pub config: Value,
    pub approved: bool,
    pub events: Option<mpsc::Sender<Value>>,
    pub external_execute: Option<ExternalTool>,
}

pub(crate) async fn wait_for_cancellation(cancellation: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow_and_update() {
            break;
        }
        if cancellation.changed().await.is_err() {
            // A closed signal channel is not an explicit cancellation request.
            // 信号通道关闭不等于用户明确取消，继续等待外层执行或超时结束。
            std::future::pending::<()>().await;
        }
    }
}

/// Cancel parsing and release the blocking reader's monitor when its caller drops.
/// 调用方退出时取消解析并释放阻塞读取的监听任务。
pub(super) struct WorkerCancellation {
    pub signal: Arc<AtomicBool>,
    monitor: tokio::task::JoinHandle<()>,
}

impl WorkerCancellation {
    pub fn new(context: &GatewayToolContext) -> Self {
        let signal = Arc::new(AtomicBool::new(*context.cancellation.borrow()));
        let worker_signal = signal.clone();
        let mut receiver = context.cancellation.clone();
        let monitor = tokio::spawn(async move {
            wait_for_cancellation(&mut receiver).await;
            worker_signal.store(true, Ordering::Release);
        });
        Self { signal, monitor }
    }
}

impl Drop for WorkerCancellation {
    fn drop(&mut self) {
        self.signal.store(true, Ordering::Release);
        self.monitor.abort();
    }
}

pub(crate) fn failure(code: &str, message: &str, status: u16) -> GatewayError {
    GatewayError::new(code, message, status)
}

pub(crate) fn decode_native(result: Result<Value, String>) -> Result<Value, GatewayError> {
    let value = result.map_err(|message| failure("TOOL_NATIVE_FAILED", &message, 500))?;
    if let Some(error) = value.get("error") {
        let mut mapped = failure(
            error["code"].as_str().unwrap_or("TOOL_FAILED"),
            error["message"].as_str().unwrap_or("工具执行失败。"),
            error["statusCode"].as_u64().unwrap_or(500) as u16,
        );
        // Only copy the safe pagination contract, never arbitrary native error bodies.
        // 仅保留安全的分页纠错合同，不将任意原生错误正文带入模型上下文。
        if ["INVALID_SKILL_LIST_PAGINATION", "INVALID_SKILL_RESOURCE_PAGINATION"].contains(&mapped.code.as_str()) {
            let mut details = serde_json::Map::new();
            for field in ["tool", "parameter", "type", "minimum", "maximum", "executed", "requestDispatched", "recoverable"] {
                if let Some(value) = error.get(field) {
                    details.insert(field.into(), value.clone());
                }
            }
            mapped.details = Value::Object(details);
        }
        return Err(mapped);
    }
    Ok(value.get("value").cloned().unwrap_or(value))
}

pub(crate) async fn native(
    ctx: &GatewayToolContext,
    operation: &str,
    args: Value,
) -> Result<Value, GatewayError> {
    let session = Arc::clone(&ctx.runtime.native);
    let operation = if operation.starts_with("tools.service.") {
        operation.to_owned()
    } else {
        format!("tools.service.{operation}")
    };
    let lease = cancellation_lease(ctx)?;
    let cancellation = lease.token.clone();
    let signal = cancellation.clone();
    let mut receiver = ctx.cancellation.clone();
    let monitor = tokio::spawn(async move {
        while receiver.changed().await.is_ok() {
            if *receiver.borrow_and_update() {
                signal.store(true, Ordering::Release);
                break;
            }
        }
    });
    let result = tokio::task::spawn_blocking(move || {
        let _entered = lease.enter();
        let mut session = session
            .lock()
            .map_err(|_| failure("TOOL_STATE_POISONED", "工具状态锁不可用。", 500))?;
        session.cancellation = Some(cancellation);
        decode_native(session.apply(&operation, &args))
    })
    .await
    .map_err(|_| failure("TOOL_WORKER_FAILED", "原生工具工作线程结束。", 500));
    monitor.abort();
    result?
}

fn cancellation_lease(
    ctx: &GatewayToolContext,
) -> Result<crate::service_cancellation::CancellationLease, GatewayError> {
    // Set cancellation TLS only on the blocking worker; Tokio threads can switch requests at await.
    // 取消 TLS 仅设在阻塞 worker 内；Tokio 线程在 await 时可能切换请求。
    let id = ctx.config["nativeCancellationId"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let lease = crate::service_cancellation::CancellationLease::acquire(&id)
        .map_err(|message| failure("TOOL_CANCEL_REGISTRY_FAILED", &message, 503))?;
    if *ctx.cancellation.borrow() {
        lease.token.store(true, Ordering::Release);
    }
    Ok(lease)
}

pub(crate) async fn emit(ctx: &GatewayToolContext, event: Value) {
    if let Some(sender) = &ctx.events {
        let _ = sender.send(event).await;
    }
}

pub(crate) fn check_cancelled(ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    if *ctx.cancellation.borrow() {
        Err(failure("TOOL_CANCELLED", "操作已取消。", 409))
    } else {
        Ok(())
    }
}

fn version_request_key(ctx: &GatewayToolContext) -> Option<String> {
    ctx.config["activeRequestId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(|request| format!("{}:{request}", ctx.conversation_id))
}

fn version_state(
    ctx: &GatewayToolContext,
) -> Result<Option<Arc<Mutex<super::service_code_versions::CodeVersions>>>, GatewayError> {
    let Some(key) = version_request_key(ctx) else {
        return Ok(None);
    };
    let mut states = ctx
        .runtime
        .code_versions
        .lock()
        .map_err(|_| failure("TOOL_STATE_POISONED", "代码版本状态锁不可用。", 500))?;
    Ok(Some(states.entry(key).or_default().clone()))
}

fn observe_code_target(ctx: &GatewayToolContext, binding: &Value) -> Result<(), GatewayError> {
    if let Some(state) = version_state(ctx)? {
        state
            .lock()
            .map_err(|_| failure("TOOL_STATE_POISONED", "代码版本状态锁不可用。", 500))?
            .apply("versions.observe", &json!({"binding":binding}), &None)
            .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    }
    Ok(())
}

/// Hash only the current request's authorized observed targets, never all users' files.
/// 只核对当前请求实际获准访问的目标；不扫描全仓，也不共享其他请求的文件账。
pub async fn capture_code_version(ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    let state = version_state(ctx)?.unwrap_or_default();
    let cancellation = Arc::new(AtomicBool::new(*ctx.cancellation.borrow()));
    let signal = cancellation.clone();
    let mut receiver = ctx.cancellation.clone();
    let monitor = tokio::spawn(async move {
        while receiver.changed().await.is_ok() {
            if *receiver.borrow_and_update() {
                signal.store(true, Ordering::Release);
                break;
            }
        }
    });
    let result = tokio::task::spawn_blocking(move || {
        let mut state = state
            .lock()
            .map_err(|_| failure("TOOL_STATE_POISONED", "代码版本状态锁不可用。", 500))?;
        let cancellation = Some(cancellation);
        let apply_error = |error: super::service_error::ServiceError| {
            failure(&error.code, &error.message, error.status_code)
        };
        let plan = state
            .apply("versions.targets", &json!({}), &cancellation)
            .map_err(apply_error)?;
        let mut files = Vec::new();
        for binding in plan["targets"].as_array().into_iter().flatten() {
            files.push(
                state
                    .apply(
                        "versions.snapshot",
                        &json!({"binding":binding}),
                        &cancellation,
                    )
                    .map_err(apply_error)?,
            );
        }
        state
            .apply(
                "versions.finish",
                &json!({"files":files,"overflow":plan["overflow"]}),
                &cancellation,
            )
            .map_err(apply_error)
    })
    .await
    .map_err(|_| failure("TOOL_WORKER_FAILED", "代码版本工作线程结束。", 500));
    monitor.abort();
    result?
}

/// Release after the final snapshot, including failed and cancelled requests.
/// 最终核对后在请求 finally 释放，失败和取消也不能留下无界目标账。
pub fn release_request(ctx: &GatewayToolContext) {
    if let Some(key) = version_request_key(ctx) {
        if let Ok(mut states) = ctx.runtime.code_versions.lock() {
            states.remove(&key);
        }
    }
}

/// A later observation can confirm an owned job's dispatch, never generic MCP/file task success.
/// 后续真实观察仅能确认自有任务已派发；不能泛化为 MCP/文件操作成功，更不能自动重放。
pub async fn verify_unknown_effects(
    ctx: &GatewayToolContext,
    records: &[Value],
    observation: &Value,
) -> Result<Vec<Value>, GatewayError> {
    let mut proofs = Vec::new();
    if observation["call"]["name"] == "mcp.task.result" {
        let result = &observation["result"]["content"];
        let proof = &result["_mcpTaskProof"];
        if result["isError"] != true && proof["terminalStatus"] == "completed" {
            for record in records {
                let handle = record["result"].get("recoveryHandle").unwrap_or(&record["result"]["content"]["recoveryHandle"]);
                if handle["kind"] == "mcp-task" && handle["taskId"] == proof["taskId"]
                    && handle["serverId"] == proof["serverId"] && handle["configurationFingerprint"] == proof["configurationFingerprint"]
                    && record["call"]["id"] == proof["toolCallId"] {
                    proofs.push(json!({"toolCallId":record["call"]["id"],"observationToolCallId":observation["call"]["id"],
                        "outcome":"dispatch-confirmed","result":{"value":result,"isError":false,"status":"completed"}}));
                }
            }
        }
    }
    let indexes = native(
        ctx,
        "broker.completion.recoveryCandidates",
        json!({"records":records,"observation":observation}),
    )
    .await?;
    for index in indexes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
    {
        let Some(record) = records.get(index as usize) else {
            continue;
        };
        let handle = &record["result"]["recoveryHandle"];
        let Some(id) = handle["jobId"].as_str() else {
            continue;
        };
        let owns = ctx
            .runtime
            .jobs
            .lock()
            .await
            .get(id)
            .is_some_and(|job| job.conversation_id == ctx.conversation_id);
        if !owns {
            continue;
        }
        let Ok(job) = native(ctx, "host.read", json!({"context":{"conversationId":ctx.conversation_id},"jobId":id,"offset":0,"limit":1})).await else { continue };
        let value = native(
            ctx,
            "broker.completion.jobProof",
            json!({"job":job,"observationId":observation["call"]["id"]}),
        )
        .await?;
        if value.is_null() {
            continue;
        }
        proofs.push(json!({"toolCallId":record["call"]["id"],"observationToolCallId":observation["call"]["id"],"outcome":"dispatch-confirmed",
            "result":{"value":value,"isError":false,"status":"completed"}}));
    }
    Ok(proofs)
}

pub fn builtin_catalog() -> Vec<Value> {
    let catalog: Value =
        serde_json::from_str(include_str!("gateway-catalog.json")).expect("checked tool catalog");
    let mut tools = catalog["tools"].as_array().cloned().unwrap_or_default();
    tools.extend(super::gateway_browser::descriptors());
    tools.extend(super::gateway_mcp::task_descriptors());
    tools.extend(super::gateway_skill_authoring::descriptors());
    tools.extend(super::gateway_extension_install::descriptors());
    tools
}

/// Discovery includes diagnostics; model declarations include only actual executable definitions.
/// 发现目录包含能力诊断；模型函数声明仅包含当前运行时确实可执行的定义。
pub fn executable_descriptor(tool: &Value) -> bool {
    tool["inputSchema"].is_object()
        && tool["enabled"] != false
        && tool["runtimeCompatible"] != false
        && tool["state"] != "unsupported"
}

pub async fn catalog(ctx: &GatewayToolContext) -> Result<Vec<Value>, GatewayError> {
    check_cancelled(ctx)?;
    let mut tools = builtin_catalog();
    // Discovery failures stay visible per service; one unavailable MCP must not hide all tools.
    // 发现失败按服务保留；一个 MCP 不可用不能把其余工具目录一起隐藏。
    tools.extend(super::gateway_mcp::catalog(ctx).await?);
    *ctx.runtime.schemas.write().await = tools.clone();
    Ok(tools)
}

pub(super) fn validate_schema(schema: &Value, value: &Value, label: &str) -> Result<(), GatewayError> {
    let invalid = || {
        let mut details = json!({"parameter":label,"executed":false,"requestDispatched":false,"recoverable":true});
        // Parameter rejection precedes dispatch; do not record it as an executed tool or retry a side effect.
        // 参数校验发生在派发前，不将其记作工具已执行，也不因此重试任何副作用。
        for field in ["type", "minimum", "maximum", "minLength", "maxLength", "maxItems", "required"] {
            if let Some(value) = schema.get(field) {
                details[field] = value.clone();
            }
        }
        failure(
            "INVALID_TOOL_ARGUMENTS",
            &format!("工具参数 {label} 不符合声明。"),
            400,
        )
        .with_details(details)
    };
    if let Some(options) = schema["enum"].as_array() {
        if !options.contains(value) {
            return Err(invalid());
        }
    }
    match schema["type"].as_str().unwrap_or("") {
        "object" => {
            let object = value.as_object().ok_or_else(invalid)?;
            for field in schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !object.contains_key(field) {
                    return Err(invalid());
                }
            }
            for (field, entry) in object {
                if let Some(child) = schema["properties"].get(field) {
                    validate_schema(child, entry, &format!("{label}.{field}"))?;
                } else if schema["additionalProperties"] == false {
                    return Err(invalid());
                }
            }
        }
        "array" => {
            let items = value.as_array().ok_or_else(invalid)?;
            if schema["maxItems"]
                .as_u64()
                .is_some_and(|max| items.len() as u64 > max)
            {
                return Err(invalid());
            }
            for item in items {
                validate_schema(&schema["items"], item, label)?;
            }
        }
        "string" => {
            let text = value.as_str().ok_or_else(invalid)?;
            let length = text.encode_utf16().count() as u64;
            if text.contains('\0')
                || schema["minLength"].as_u64().is_some_and(|min| length < min)
                || schema["maxLength"].as_u64().is_some_and(|max| length > max)
            {
                return Err(invalid());
            }
        }
        "integer" => {
            let number = value.as_i64().ok_or_else(invalid)?;
            if schema["minimum"].as_i64().is_some_and(|min| number < min)
                || schema["maximum"].as_i64().is_some_and(|max| number > max)
            {
                return Err(invalid());
            }
        }
        "boolean" if !value.is_boolean() => return Err(invalid()),
        "null" if !value.is_null() => return Err(invalid()),
        _ => {}
    }
    Ok(())
}

pub(crate) fn workspace_path_tool(name: &str, arguments: &Value) -> bool {
    name.starts_with("filesystem.") || ["document.read","document.create","image.read","terminal.run","terminal.workspace.open","terminal.workspace.write","terminal.workspace.run","terminal.host.run","terminal.host.start"].contains(&name)
        || name == "artifact.open" && !arguments["url"].is_string() && !arguments["resultId"].is_string()
}

pub(crate) fn operation_mount_binding(ctx: &GatewayToolContext, arguments: &Value) -> Result<Value, GatewayError> {
    let mounts = ctx.config["workspaceMounts"].as_array().or_else(||ctx.config["bindingSnapshot"]["mounts"].as_array());
    let Some(mounts) = mounts else { return Ok(Value::Null); };
    let requested_id = arguments["mountId"].as_str().or_else(||ctx.config["activeMountId"].as_str());
    if mounts.is_empty() && requested_id.is_none() {
        return Ok(Value::Null);
    }
    if let Some(id) = requested_id {
        return mounts.iter().find(|mount|mount["id"] == id).cloned()
            .ok_or_else(||failure("WORKSPACE_MOUNT_UNAVAILABLE", "该目录不在本次任务的已授权挂载中。", 409));
    }
    if let Some(path) = arguments["path"].as_str().or_else(||arguments["cwd"].as_str()).filter(|path|Path::new(path).is_absolute()) {
        return Ok(mounts.iter().filter(|mount|mount["path"].as_str().is_some_and(|root|super::local_files::within(Path::new(root),Path::new(path))))
            .max_by_key(|mount|mount["path"].as_str().unwrap_or("").len()).cloned()
            .unwrap_or(Value::Null));
    }
    Ok(mounts.iter().find(|mount|mount["id"] == ctx.config["defaultMountId"]).cloned().unwrap_or(Value::Null))
}

/// Freeze a resumed task's original mounts; catalog additions never silently enlarge its authority.
/// 固定续接任务原有的挂载授权；目录列表新增不能悄悄扩大该任务的访问范围。
pub(crate) fn restrict_workspace_context(ctx: &mut GatewayToolContext, snapshot: &Value) -> Result<(), GatewayError> {
    if !snapshot.is_object() { return Ok(()); }
    if let Some(mounts) = snapshot["mounts"].as_array() {
        ctx.config["workspaceMounts"] = json!(mounts);
        ctx.config["defaultMountId"] = snapshot["defaultMountId"].clone();
        ctx.config["bindingSnapshot"] = snapshot.clone();
        ctx.authorized_roots = mounts.iter().filter_map(|mount|mount["path"].as_str().map(PathBuf::from)).collect();
    } else if let Some(folder) = snapshot["folderPath"].as_str() {
        let mounts = ctx.config["workspaceMounts"].as_array().into_iter().flatten()
            .filter(|mount|mount["path"] == folder).cloned().collect::<Vec<_>>();
        ctx.config["defaultMountId"] = mounts.first().map(|mount|mount["id"].clone()).unwrap_or(Value::Null);
        ctx.config["workspaceMounts"] = json!(mounts);
        ctx.authorized_roots = vec![PathBuf::from(folder)];
    }
    if let Some(folder) = snapshot["folderPath"].as_str().filter(|folder|!folder.is_empty()) {
        ctx.workdir = PathBuf::from(folder);
        if !ctx.authorized_roots.contains(&ctx.workdir) { ctx.authorized_roots.push(ctx.workdir.clone()); }
    }
    Ok(())
}

pub(crate) async fn select_workspace_context(ctx: &GatewayToolContext, arguments: &Value) -> Result<GatewayToolContext, GatewayError> {
    let selected = if (arguments["input"].is_string() || arguments["command"].is_string()) && arguments["sessionId"].is_string() {
        super::gateway_workspace_terminal::session_mount(ctx,arguments["sessionId"].as_str().unwrap()).await?
    } else { operation_mount_binding(ctx, arguments)? };
    if selected.is_null() { return Ok(ctx.clone()); }
    let root = ctx.data_home.clone();
    let conversation = ctx.conversation_id.clone();
    let relation = tokio::task::spawn_blocking(move ||crate::storage::gateway::current_relationship(&root,&conversation)).await
        .map_err(|_|failure("WORKSPACE_SCOPE_FAILED", "工作挂载核验线程结束。", 503))??;
    if relation["projectId"].as_str() != ctx.project_id.as_deref()
        || !relation["mounts"].as_array().into_iter().flatten().any(|mount|crate::storage::workspace_mounts::same_mount(&selected,mount)) {
        return Err(failure("WORKSPACE_MOUNT_CHANGED", "该挂载已移除或改变；任务进度已保留，请核对后继续。", 409));
    }
    let mut selected_context = ctx.clone();
    selected_context.workdir = PathBuf::from(selected["path"].as_str().unwrap());
    selected_context.authorized_roots = vec![selected_context.workdir.clone()];
    selected_context.config["activeMountId"] = selected["id"].clone();
    selected_context.config["activeMount"] = selected;
    if let Some(path) = arguments["path"].as_str().or_else(||arguments["cwd"].as_str()) {
        let (_, outside) = canonical_authorized(&selected_context,path,true)?;
        if outside {
            if arguments["mountId"].is_string() { return Err(failure("WORKSPACE_PATH_OUTSIDE", "文件路径不属于指定挂载。", 403)); }
            return Ok(ctx.clone());
        }
    }
    Ok(selected_context)
}

pub(crate) async fn workspace_files(arguments: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let selected = select_workspace_context(ctx,arguments).await?;
    let path = arguments["path"].as_str().unwrap_or(".");
    let (absolute_path, outside) = canonical_authorized(&selected,path,false)?;
    if outside { return Err(failure("WORKSPACE_PATH_OUTSIDE", "文件列表只能浏览选定挂载。", 403)); }
    let mut value = filesystem("filesystem.list", json!({"path":path}), &selected).await?;
    value["mountId"] = selected.config["activeMountId"].clone();
    value["path"] = json!(absolute_path);
    Ok(value)
}

pub(crate) fn canonical_authorized(
    ctx: &GatewayToolContext,
    requested: &str,
    allow_missing: bool,
) -> Result<(PathBuf, bool), GatewayError> {
    let candidate = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        ctx.workdir.join(requested)
    };
    let path = super::service_filesystem::canonical(&candidate, allow_missing)
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    let roots = ctx
        .authorized_roots
        .iter()
        .chain(std::iter::once(&ctx.workdir))
        .filter_map(|root| std::fs::canonicalize(root).ok()).collect::<Vec<_>>();
    let originally_inside = roots.iter().any(|root|super::local_files::within(root,&path));
    let current_roots = if let Some(mounts) = ctx.config["workspaceMounts"].as_array() {
        let relation = crate::storage::gateway::current_relationship(&ctx.data_home,&ctx.conversation_id)?;
        if relation["projectId"].as_str() != ctx.project_id.as_deref() {
            return Err(failure("WORKSPACE_MOUNT_CHANGED","当前聊天的工作归属已变化。",409));
        }
        let mut allowed = mounts.iter().filter(|old|relation["mounts"].as_array().into_iter().flatten()
            .any(|new|crate::storage::workspace_mounts::same_mount(old,new)))
            .filter_map(|mount|mount["path"].as_str().and_then(|path|std::fs::canonicalize(path).ok())).collect::<Vec<_>>();
        if mounts.is_empty() {
            if let Ok(sandbox) = std::fs::canonicalize(ctx.data_home.join("Sandboxes").join(&ctx.conversation_id)) { allowed.push(sandbox); }
        }
        roots.iter().filter(|root|allowed.contains(root)).cloned().collect::<Vec<_>>()
    } else { roots };
    let inside = current_roots.iter()
        .any(|root| super::local_files::within(&root, &path));
    if originally_inside && !inside {
        return Err(failure("WORKSPACE_MOUNT_CHANGED","原授权挂载已移除或改变，不能继续读取或修改。",409));
    }
    Ok((path, !inside))
}

pub(crate) fn protected_path(ctx: &GatewayToolContext, path: &Path) -> bool {
    let sandbox = ctx.data_home.join("Sandboxes").join(&ctx.conversation_id);
    let own_sandbox = super::service_filesystem::canonical(&sandbox, false)
        .ok()
        .zip(super::service_filesystem::canonical(&ctx.workdir, false).ok())
        .is_some_and(|(sandbox, work)| {
            super::local_files::within(&sandbox, &work)
                && super::local_files::within(&work, &sandbox)
                && super::local_files::within(&sandbox, path)
        });
    [
        Some(ctx.data_home.clone()),
        Some(ctx.extension_root.clone()),
        ctx.config["officialToolsRoot"].as_str().map(PathBuf::from),
    ]
    .into_iter()
    .enumerate()
    .filter_map(|(index, root)| {
        root.and_then(|root| std::fs::canonicalize(root).ok())
            .map(|root| (index, root))
    })
    .any(|(index, root)| super::local_files::within(&root, path) && !(index == 0 && own_sandbox))
}

pub(super) async fn filesystem(
    name: &str,
    args: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let missing = matches!(name, "filesystem.write" | "filesystem.mkdir" | "document.create");
    let (path, outside) = canonical_authorized(ctx, args["path"].as_str().unwrap_or("."), missing)?;
    let requested = args["path"].as_str().unwrap_or(".");
    let requested = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        ctx.workdir.join(requested)
    };
    let mut managed = vec![ctx.data_home.clone(), ctx.extension_root.clone()];
    if let Some(official) = ctx.config["officialToolsRoot"].as_str() {
        managed.push(PathBuf::from(official));
    }
    let sandbox = ctx.data_home.join("Sandboxes").join(&ctx.conversation_id);
    let own_sandbox = super::service_filesystem::canonical(&sandbox, false)
        .ok()
        .zip(super::service_filesystem::canonical(&ctx.workdir, false).ok())
        .is_some_and(|(sandbox, work)| {
            super::local_files::within(&sandbox, &work)
                && super::local_files::within(&work, &sandbox)
                && super::local_files::within(&sandbox, &path)
        });
    if managed
        .iter()
        .enumerate()
        .filter_map(|(index, root)| std::fs::canonicalize(root).ok().map(|root| (index, root)))
        .any(|(index, root)| {
            super::local_files::within(&root, &path) && !(index == 0 && own_sandbox)
        })
    {
        return Err(failure(
            "PROTECTED_APP_DATA",
            "正式数据、凭据和扩展须通过专用服务访问。",
            403,
        ));
    }
    if outside && !ctx.approved && ctx.permission_mode != "full" {
        return Ok(approval(name, &args, ctx));
    }
    if super::service_skills::private_resource(&super::local_files::path_text(&path))
        && !ctx.approved
        && ctx.permission_mode != "full"
    {
        return Ok(approval(name, &args, ctx));
    }
    let needs=super::policy::calculate("tools.needsApproval",&json!({"name":name,"permissionMode":ctx.permission_mode,
        "options":{"outsideWorkspace":outside,"sensitiveRead":super::service_skills::private_resource(&super::local_files::path_text(&path))}}))
        .map_err(|message|failure("TOOL_POLICY_FAILED",&message,500))?;
    if needs == true && !ctx.approved {
        return Ok(approval(name, &args, ctx));
    }
    let binding = json!({"requestedPath":super::local_files::path_text(&requested),"path":super::local_files::path_text(&path),
        "allowMissing":missing,"allowHardLinks":name.starts_with("filesystem.read") || name == "filesystem.stat" || name == "document.read" || name == "image.read" || name == "artifact.open"});
    if name == "artifact.open" {
        return super::gateway_workspace_preview::open(args, path, binding, ctx).await;
    }
    if name == "image.read" {
        return super::gateway_image::read(args, path, binding, ctx).await;
    }
    if name == "document.read" {
        let result = super::gateway_document_read::read(args, path, binding.clone(), ctx).await?;
        observe_code_target(ctx, &binding)?;
        return Ok(result);
    }
    if name == "document.create" {
        // Only the authorized canonical binding reaches the binary writer; model fields cannot supply it.
        // 二进制写入只接收本层授权的规范路径绑定，模型参数不能自行提供该绑定。
        let mut input = args;
        input["path"] = json!(super::local_files::path_text(&path));
        input["_pathBinding"] = binding;
        return super::gateway_document_create::create(ctx, input).await;
    }
    if name == "filesystem.search" {
        let identity = json!({"conversationId":ctx.conversation_id,"projectId":ctx.project_id,"workspaceRoot":ctx.workdir,
            "permissionMode":ctx.permission_mode});
        // The exact conversation sandbox is already a bounded search root; all other formal data stays protected.
        // 本会话精确沙箱本身就是有界搜索根；其他正式数据依旧受保护，不能放开整个 Data。
        let search_protected = if own_sandbox {
            managed.iter().skip(1).cloned().collect::<Vec<_>>()
        } else {
            managed.clone()
        };
        let request = json!({"path":path,"input":args,"workspaceRoot":ctx.workdir,"protectedRoots":search_protected,
            "context":identity,"contextIdentity":ctx.conversation_id,"ownerIdentity":ctx.conversation_id});
        let cursor = args["cursor"].as_str();
        let session = {
            let mut searches = ctx
                .runtime
                .searches
                .lock()
                .map_err(|_| failure("TOOL_STATE_POISONED", "搜索状态锁不可用。", 500))?;
            if let Some(cursor) = cursor {
                searches.remove(cursor).ok_or_else(|| {
                    failure(
                        "INVALID_TOOL_SEARCH_CURSOR",
                        "搜索游标不属于当前调用或已消费。",
                        409,
                    )
                })?
            } else {
                if searches.len() >= 16 {
                    searches.clear();
                }
                Arc::new(Mutex::new(ToolSession::default()))
            }
        };
        let operation = if cursor.is_some() {
            "search.acquire"
        } else {
            "search.init"
        };
        let worker = session.clone();
        let sensitive_allowed = ctx.approved || ctx.permission_mode == "full";
        let maximum = args["maxMatches"].as_u64().unwrap_or(100);
        let lease = cancellation_lease(ctx)?;
        let cancellation = lease.token.clone();
        let signal = cancellation.clone();
        let mut receiver = ctx.cancellation.clone();
        let monitor = tokio::spawn(async move {
            while receiver.changed().await.is_ok() {
                if *receiver.borrow_and_update() {
                    signal.store(true, Ordering::Release);
                    break;
                }
            }
        });
        let page = tokio::task::spawn_blocking(move || {
            let _entered = lease.enter();
            let mut session = worker
                .lock()
                .map_err(|_| failure("TOOL_STATE_POISONED", "搜索状态锁不可用。", 500))?;
            session.cancellation = Some(cancellation);
            decode_native(session.apply(&format!("tools.service.{operation}"), &request))?;
            let mut phase = decode_native(session.apply(
                "tools.service.search.pageStart",
                &json!({"maxMatches":maximum,"monitorState":"unavailable"}),
            ))?;
            while !phase["authorization"].is_null() {
                let target = phase["authorization"]["path"].as_str().unwrap_or("");
                let allowed = !super::service_skills::private_resource(target) || sensitive_allowed;
                phase = decode_native(session.apply(
                    "tools.service.search.step",
                    &json!({"allowed":allowed,"monitorState":"unavailable"}),
                ))?;
            }
            decode_native(session.apply(
                "tools.service.search.pageFinish",
                &json!({"monitorState":"unavailable"}),
            ))
        })
        .await
        .map_err(|_| failure("TOOL_WORKER_FAILED", "搜索工作线程退出。", 500));
        monitor.abort();
        let page = page??;
        if let Some(cursor) = page["nextCursor"].as_str() {
            ctx.runtime
                .searches
                .lock()
                .map_err(|_| failure("TOOL_STATE_POISONED", "搜索状态锁不可用。", 500))?
                .insert(cursor.to_owned(), session);
        }
        return Ok(page);
    }
    let mut result = native(
        ctx,
        &name.replacen("filesystem.", "file.", 1),
        json!({"input":args,"path":path,"pathBinding":binding,"workspaceRoot":ctx.workdir}),
    )
    .await?;
    if name == "filesystem.list" {
        if let Some(entries) = result["entries"].as_array_mut() {
            entries.sort_by(|a, b| {
                a["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["name"].as_str().unwrap_or(""))
            });
        }
        result = native(ctx, "file.listFinish", json!({"result":result})).await?;
    }
    observe_code_target(ctx, &binding)?;
    Ok(result)
}

pub(crate) fn approval(name: &str, args: &Value, ctx: &GatewayToolContext) -> Value {
    json!({"requiresApproval":true,"status":"not-dispatched","invocation":{"id":uuid::Uuid::new_v4().to_string(),"name":name,
        "arguments":args,"conversationId":ctx.conversation_id,"projectId":ctx.project_id}})
}

async fn bind_work(args: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let requested = args["path"].as_str().unwrap_or("");
    native(ctx, "broker.workRequest", json!({"context":{"projectId":ctx.project_id},"input":args,"absolute":Path::new(requested).is_absolute()})).await?;
    let (path, outside) = canonical_authorized(ctx, requested, false)?;
    let owned = [
        ctx.data_home.clone(),
        ctx.extension_root.clone(),
        ctx.config["officialToolsRoot"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.extension_root.join("official-tools")),
    ]
    .iter()
    .filter_map(|root| std::fs::canonicalize(root).ok())
    .any(|root| super::local_files::within(&root, &path));
    native(
        ctx,
        "broker.workTarget",
        json!({"owned":owned,"readOnly":owned}),
    )
    .await?;
    let needs = super::policy::calculate("tools.needsApproval", &json!({"name":"work.folder.bind","permissionMode":ctx.permission_mode,"options":{"outsideWorkspace":outside}}))
        .map_err(|message|failure("TOOL_POLICY_FAILED",&message,500))?;
    if needs == true && !ctx.approved {
        return Ok(approval("work.folder.bind", &args, ctx));
    }
    let binding =
        json!({"requestedPath":requested,"path":path,"allowMissing":false,"allowHardLinks":false});
    let info=native(ctx,"file.stat",json!({"path":path,"pathBinding":binding,"input":{"path":path},"workspaceRoot":ctx.workdir})).await?;
    native(
        ctx,
        "broker.workDirectory",
        json!({"directory":info["type"]=="directory"}),
    )
    .await?;
    let Some(execute) = &ctx.external_execute else {
        return Err(failure("TOOL_NOT_IMPLEMENTED", "工作关联服务未接线。", 503));
    };
    // Binding changes the saved relationship, not this turn's authorized roots.
    // 关联只改变持久关系，不扩大本轮授权根目录。
    let mut normalized = args;
    normalized["path"] = json!(path);
    execute("work.folder.bind".into(), normalized).await
}

/// Correct a browser launch chosen for an in-app website request before any desktop process is started.
/// 在启动任何桌面进程前，修正软件内网页请求误选的浏览器启动；明确本机应用请求保持原路径。
fn workspace_launch_target(args: &Value, ctx: &GatewayToolContext) -> Result<Option<Value>, GatewayError> {
    let executable = args["appPath"].as_str().unwrap_or("")
        .rsplit(['/', '\\']).next().unwrap_or("");
    if !super::intent::is_browser_executable(&executable.to_ascii_lowercase()) {
        return Ok(None);
    }
    let intent = super::intent::calculate("tools.browserIntent", &json!({
        "message":ctx.config["trustedUserText"],"previousUserMessages":ctx.config["previousUserMessages"]
    })).map_err(|message| failure("TOOL_INTENT_FAILED", &message, 500))?;
    if intent["presentationTarget"] == "application" {
        return Ok(None);
    }
    let url = args["args"].as_array().into_iter().flatten().filter_map(Value::as_str)
        .map(|argument| argument.strip_prefix("--app=").unwrap_or(argument))
        .find(|argument| argument.starts_with("https://") || argument.starts_with("http://"))
        .ok_or_else(|| failure("WORKSPACE_BROWSER_REQUIRED",
            "网页默认在右侧工作区打开。请使用 artifact.open 的 url，或需要网页操作时使用 browser.open；只有明确本机浏览器请求才启动桌面应用。", 400))?;
    super::gateway_workspace_preview::web_target(&json!({"url":url})).map(Some)
}

async fn execute_inner(
    tool_name: &str,
    args: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    let schemas = ctx.runtime.schemas.read().await.clone();
    let descriptors = if schemas.is_empty() {
        builtin_catalog()
    } else {
        schemas
    };
    if let Some(descriptor) = descriptors
        .iter()
        .find(|descriptor| descriptor["name"] == tool_name && (!tool_name.starts_with("mcp.") || super::gateway_mcp::builtin_task(tool_name)))
    {
        validate_schema(&descriptor["inputSchema"], &args, tool_name)?;
    }
    if tool_name == "work.folder.bind" {
        return bind_work(args, ctx).await;
    }
    if tool_name == "work.folder.list" {
        let root = ctx.data_home.clone();
        let chat = ctx.conversation_id.clone();
        let relation = tokio::task::spawn_blocking(move ||crate::storage::gateway::current_relationship(&root,&chat)).await
            .map_err(|_|failure("WORKSPACE_SCOPE_FAILED", "工作挂载读取线程结束。", 503))??;
        let allowed = ctx.config["workspaceMounts"].as_array();
        let mounts = relation["mounts"].as_array().into_iter().flatten().filter(|mount|
            allowed.is_none_or(|allowed|allowed.iter().any(|old|crate::storage::workspace_mounts::same_mount(old,mount))))
            .cloned().collect::<Vec<_>>();
        return Ok(json!({"mounts":mounts,"defaultMountId":ctx.config.get("defaultMountId").unwrap_or(&relation["defaultMountId"]),"managedWorkspace":mounts.is_empty(),"workspaceRoot":ctx.workdir}));
    }
    if tool_name == "computer.launch" {
        if let Some(target) = workspace_launch_target(&args, ctx)? {
            let mut opened = Box::pin(super::gateway_browser::workspace_open(&target, ctx)).await?;
            for (field, value) in target.as_object().unwrap() {
                if !opened.get(field).is_some_and(|value| !value.is_null()) {
                    opened[field] = value.clone();
                }
            }
            opened["presentation"] = json!("workspace");
            opened["redirectedFrom"] = json!("computer.launch");
            return Ok(opened);
        }
    }
    if tool_name.starts_with("mcp.") && !super::gateway_mcp::builtin_task(tool_name) {
        super::gateway_mcp::authorized_server(tool_name, ctx)?;
    }
    let options = json!({"outsideWorkspace":(["terminal.host.run","terminal.host.start","terminal.host.stop",
        "terminal.workspace.open","terminal.workspace.write","terminal.workspace.run","terminal.workspace.close"].contains(&tool_name)
        || tool_name.starts_with("mcp.")),"verifiedSandbox":false});
    let needs = super::policy::calculate(
        "tools.needsApproval",
        &json!({"name":tool_name,"permissionMode":ctx.permission_mode,"options":options}),
    )
    .map_err(|message| failure("TOOL_POLICY_FAILED", &message, 500))?;
    let read_browser = [
        "browser.tabs",
        "browser.snapshot",
        "browser.screenshot",
        "browser.console",
        "browser.network",
    ]
    .contains(&tool_name);
    if needs == true
        && !ctx.approved
        && !read_browser
        && !tool_name.starts_with("filesystem.")
        && !["document.read", "document.create", "image.read", "artifact.open"].contains(&tool_name)
        && !tool_name.starts_with("skill.")
        && !["tool.search", "tool.load", "web.search"].contains(&tool_name)
    {
        return Ok(approval(tool_name, &args, ctx));
    }
    if tool_name == "artifact.open" {
        if ["path", "url", "resultId"].iter().filter(|key| args[*key].as_str().is_some_and(|value| !value.trim().is_empty())).count() != 1 {
            return Err(failure("INVALID_TOOL_ARGUMENTS", "请仅指定文件路径、网页地址或已有图片回执之一。", 400));
        }
        if args["resultId"].is_string() {
            let execute = ctx.external_execute.as_ref().ok_or_else(|| failure("TOOL_NOT_IMPLEMENTED", "图片归档展示服务未接线。", 503))?;
            return execute(tool_name.to_owned(), args).await;
        }
    }
    if tool_name == "artifact.open" && args["url"].is_string() {
        let target = super::gateway_workspace_preview::web_target(&args)?;
        let mut opened = Box::pin(super::gateway_browser::workspace_open(&target, ctx)).await?;
        opened["currentUrl"] = opened["url"].clone();
        for (field, value) in target.as_object().unwrap() {
            if matches!(field.as_str(), "url" | "path" | "title" | "kind" | "mimeType" | "version") {
                opened[field] = value.clone();
            }
        }
        opened["presentation"] = json!("workspace");
        return Ok(opened);
    }
    if tool_name == "attachment.read" {
        return super::gateway_attachment_read::read(args, ctx).await;
    }
    if tool_name == "artifact.open" {
        if !args["path"].as_str().is_some_and(|path| !path.trim().is_empty()) {
            return Err(failure("INVALID_TOOL_ARGUMENTS", "请选择真实文件路径或网页地址。", 400));
        }
        let mut target = args;
        target["_metadataOnly"] = json!(true);
        return filesystem(tool_name, target, ctx).await;
    }
    if tool_name == "knowledge.experience" {
        return super::gateway_experience::query(args,ctx).await;
    }
    if tool_name.starts_with("filesystem.") || ["document.read", "document.create", "image.read", "artifact.open"].contains(&tool_name) {
        return filesystem(tool_name, args, ctx).await;
    }
    if tool_name.starts_with("terminal.workspace.") {
        return Box::pin(super::gateway_workspace_terminal::execute(tool_name, &args, ctx)).await;
    }
    if tool_name.starts_with("terminal.") || tool_name.starts_with("computer.") {
        return super::gateway_process::execute(tool_name, args, ctx).await;
    }
    if tool_name.starts_with("browser.") {
        return Box::pin(super::gateway_browser::execute(tool_name, args, ctx)).await;
    }
    if super::gateway_extension_install::supports(tool_name) {
        return super::gateway_extension_install::execute(tool_name,args,ctx).await;
    }
    if super::gateway_mcp::builtin_task(tool_name) {
        return super::gateway_mcp::execute_task(tool_name,args,ctx).await;
    }
    if tool_name.starts_with("mcp.") {
        return super::gateway_mcp::execute(tool_name, args, ctx).await;
    }
    if tool_name == "web.fetch" {
        return super::gateway_web::fetch(args, ctx).await;
    }
    if tool_name == "web.search" {
        return super::gateway_search::search(args, ctx).await;
    }
    if super::gateway_skill_authoring::supports(tool_name) {
        return super::gateway_skill_authoring::execute(tool_name,args,ctx).await;
    }
    if tool_name.starts_with("skill.") {
        return skills(tool_name, args, ctx).await;
    }
    if tool_name == "tool.search" || tool_name == "tool.load" {
        let mut all = builtin_catalog();
        if tool_name == "tool.load" {
            let mut known = all.clone();
            known.extend(ctx.runtime.schemas.read().await.iter().cloned());
            known.extend(reusable_mcp_catalog(ctx));
            // Resolve exact known wire aliases only; model spelling or a guessed hash cannot invent a capability.
            // 只归一化已知工具的精确协议别名；模型拼写或猜出的哈希不能创造能力。
            let names: Vec<Value> = args["names"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|requested| {
                    known
                        .iter()
                        .find(|tool| {
                            tool["name"].as_str().is_some_and(|name| {
                                requested == name
                                    || requested.as_str()
                                        == Some(crate::models::protocol::wire_name(name).as_str())
                            })
                        })
                        .map(|tool| tool["name"].clone())
                        .unwrap_or_else(|| requested.clone())
                })
                .collect();
            let servers = super::gateway_mcp::servers(ctx)
                .into_iter()
                .filter(|server| {
                    names.iter().filter_map(Value::as_str).any(|name| {
                        name == format!("mcp.{}", server["id"].as_str().unwrap_or(""))
                            || name.starts_with(&format!(
                                "mcp.{}.",
                                server["id"].as_str().unwrap_or("")
                            ))
                    })
                })
                .collect();
            all.extend(super::gateway_mcp::catalog_selected(ctx, servers).await?);
            let mut previous = ctx.runtime.schemas.write().await;
            for tool in &all {
                if !previous.iter().any(|entry| entry["name"] == tool["name"]) {
                    previous.push(tool.clone());
                }
            }
            return Ok(
                json!({"tools":all.into_iter().filter(|tool| names.iter().filter_map(Value::as_str)
                .any(|name| tool["name"] == name || tool["name"].as_str().is_some_and(|value| value.starts_with(&format!("{name}."))))).map(|mut tool| {
                    tool["wireName"] = json!(crate::models::protocol::wire_name(tool["name"].as_str().unwrap_or("")));
                    tool
                }).collect::<Vec<_>>() }),
            );
        }
        all.extend(
            ctx.runtime
                .schemas
                .read()
                .await
                .iter()
                .filter(|tool| tool["serverId"].is_string())
                .cloned(),
        );
        for tool in reusable_mcp_catalog(ctx) {
            if !all.iter().any(|existing| existing["name"] == tool["name"]) {
                all.push(tool);
            }
        }
        all.extend(super::gateway_mcp::servers(ctx).iter().map(|server|json!({"name":format!("mcp.{}",server["id"].as_str().unwrap_or("")),"description":format!("MCP service {}. Load this group to discover its actual enabled tools; configured does not imply ready.",server["name"].as_str().unwrap_or("")),"source":format!("mcp:{}",server["id"].as_str().unwrap_or("")),"serverId":server["id"]})));
        // Keep native aliases and ranking authoritative; transport code must not reinterpret intent.
        // 别名与排序仍由既有原生规则负责；传输接线不重新实现另一套语义识别。
        let indexes = super::discovery::calculate(
            "tools.searchTools",
            &json!({"descriptors":all,"query":args["query"],"includeScores":true}),
        )
        .map_err(|message| failure("TOOL_DISCOVERY_FAILED", &message, 500))?;
        let mut indexes = indexes.as_array().cloned().unwrap_or_default();
        let semantic = super::gateway_semantic::supplement(ctx,&all,args["query"].as_str().unwrap_or(""),&mut indexes).await;
        rank_tool_hints(ctx, &all, &mut indexes);
        let matching = indexes.iter()
            .filter_map(|hit| hit.as_u64().or_else(|| hit["index"].as_u64()))
            .filter_map(|index| all.get(index as usize))
            .collect::<Vec<_>>();
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(20).min(20) as usize;
        return Ok(
            json!({"tools":matching.iter().skip(offset).take(limit).map(|tool|json!({"name":tool["name"],"description":tool["description"],"source":tool["source"],"state":tool["state"],"runtimeCompatible":tool["runtimeCompatible"],"code":tool["code"]})).collect::<Vec<_>>(),
            "total":matching.len(),"nextOffset":if offset+limit<matching.len(){json!(offset+limit)}else{Value::Null},"discovery":semantic}),
        );
    }
    if let Some(execute) = &ctx.external_execute {
        return execute(tool_name.to_owned(), args).await;
    }
    Err(failure(
        "TOOL_NOT_IMPLEMENTED",
        &format!("专用服务未接线：{tool_name}"),
        503,
    ))
}

fn execution_environment(
    tool_name: &str,
    result: &Value,
    server: &Value,
) -> Result<Value, GatewayError> {
    let descriptor =
        json!({"name":tool_name,"source":if tool_name.starts_with("mcp."){"mcp"}else{"builtin"}});
    // Trust provenance only from native browser/search adapters; MCP results cannot select a host.
    // 仅信任原生浏览器和搜索来源信息；MCP 结果不能选择执行主机。
    if (tool_name.starts_with("browser.") || tool_name == "web.search")
        && result["executionEnvironment"].is_object()
    {
        let normalized = super::contracts::calculate(
            "tools.normalizeEnvironment",
            &json!({"input":result["executionEnvironment"]}),
        )
        .map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500))?;
        if normalized.is_object() {
            return Ok(normalized);
        }
        return super::contracts::calculate(
            "tools.resolveEnvironment",
            &json!({"descriptor":{"source":"unknown"}}),
        )
        .map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500));
    }
    let sandbox = if tool_name == "terminal.run"
        && result["sandbox"] == "appcontainer"
        && result["tokenVerified"] == true
        && result["activeProcessesAfterExit"] == 0
    {
        // runner.sandboxReceipt checked these facts before returning; they are not model-supplied capabilities.
        // runner.sandboxReceipt 已在返回前核验这些事实；它们不是模型自报的能力。
        json!({"available":true,"sandbox":"appcontainer","failClosed":true,"checksChildToken":true,"network":result["network"]})
    } else {
        result.clone()
    };
    super::contracts::calculate(
        "tools.resolveEnvironment",
        &json!({"descriptor":descriptor,"server":server,"sandbox":sandbox}),
    )
    .map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500))
}

/// Saved request snapshots can lose capabilities, but configuration changes cannot grant them new ones.
/// 已保存的请求快照可以被撤销能力，配置更新不能给旧请求扩大能力。
pub(crate) async fn checked_configuration(
    ctx: &GatewayToolContext,
) -> Result<GatewayToolContext, GatewayError> {
    let mut checked = ctx.clone();
    let _snapshot = ctx.runtime.configuration_updates.read().await;
    let generation = ctx.runtime.configuration_generation.load(Ordering::Acquire);
    checked.config["configurationGeneration"] = json!(generation);
    checked.config["mcpConfigurationGeneration"] =
        json!(super::gateway_mcp::configuration_generation(ctx));
    // Unpersisted contexts are internal discovery/test inputs; formal requests always carry a saved revision.
    // 未持久化的上下文用于内部发现或夹具；正式请求始终带有已保存的配置修订号。
    if ctx.config["revision"].is_u64() {
        let current = get_config(ctx).await?;
        let original = super::gateway_mcp::servers(ctx);
        let mut latest = ctx.clone();
        latest.config = current.clone();
        let enabled = super::gateway_mcp::servers(&latest);
        let retained = original
            .into_iter()
            .filter_map(|mut server| {
                let active = enabled.iter().find(|active| active["id"] == server["id"])?;
                let mut previous_identity = server.clone();
                let mut current_identity = active.clone();
                for field in ["name", "disabledTools", "backgroundTask", "prewarmPriority"] {
                    previous_identity.as_object_mut()?.remove(field);
                    current_identity.as_object_mut()?.remove(field);
                }
                if previous_identity != current_identity {
                    return None;
                }
                let mut disabled = server["disabledTools"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for tool in active["disabledTools"].as_array().into_iter().flatten() {
                    if !disabled.contains(tool) {
                        disabled.push(tool.clone());
                    }
                }
                server["disabledTools"] = json!(disabled);
                Some(server)
            })
            .collect::<Vec<_>>();
        checked.config["mcpServers"] = json!(retained);
        let mut disabled = ctx.config["disabledSkills"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for skill in current["disabledSkills"].as_array().into_iter().flatten() {
            if !disabled.contains(skill) {
                disabled.push(skill.clone());
            }
        }
        checked.config["disabledSkills"] = json!(disabled);
        checked.config["skillDirectories"] = json!(
            ctx.config["skillDirectories"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|directory| current["skillDirectories"]
                    .as_array()
                    .is_some_and(|directories| directories.contains(directory)))
                .cloned()
                .collect::<Vec<_>>()
        );
    }
    Ok(checked)
}

pub(crate) fn check_configuration_generation(ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    if ctx.config["configurationGeneration"]
        .as_u64()
        .is_some_and(|generation| {
            generation != ctx.runtime.configuration_generation.load(Ordering::Acquire)
        })
    {
        return Err(failure(
            "TOOL_CONFIGURATION_CHANGED",
            "工具配置已更新，旧调用未派发，请重新加载能力。",
            409,
        ));
    }
    check_cancelled(ctx)
}

/// Build authority only from the selected native context, not caller-supplied approval fields.
/// 授权绑定只取已选择的原生上下文；动态调用ID不影响任务复用，权限/配置/挂载变化会失效。
async fn task_approval_binding(name: &str, args: &Value, ctx: &GatewayToolContext, invocation: &Value) -> Result<Value, GatewayError> {
    let target = if name == "filesystem.write" {
        let (path, _) = canonical_authorized(ctx, args["path"].as_str().unwrap_or(""), true)?;
        if protected_path(ctx, &path) {
            return Err(failure("PROTECTED_APP_DATA", "正式数据、凭据和扩展须通过专用服务访问。", 403));
        }
        Some(super::local_files::path_text(&path))
    } else { None };
    let mut configuration = ctx.config.clone();
    if let Some(fields) = configuration.as_object_mut() {
        for key in ["approvedInvocation", "activeToolCallId", "activeOperationId", "activeRequestId", "nativeCancellationId",
            "activeAssistantMessageId", "operationNamespace", "renewOperationalExecutionBudget", "attachmentContextChars", "activeSteeringSequence"] {
            fields.remove(key);
        }
    }
    let mut roots = ctx.authorized_roots.iter().map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
        .map(|root| super::local_files::path_text(&root)).collect::<Vec<_>>();
    roots.sort(); roots.dedup();
    let schemas = ctx.runtime.schemas.read().await;
    let schema = schemas.iter().find(|tool| tool["name"] == name).map(|tool| tool["inputSchema"].clone()).unwrap_or(Value::Null);
    drop(schemas);
    let mut proof = invocation.clone();
    if let Some(fields) = proof.as_object_mut() {
        for key in ["id", "arguments", "taskApproval"] { fields.remove(key); }
    }
    let host = std::fs::metadata(&ctx.toolhost_exe).ok().map(|metadata|
        json!({"bytes":metadata.len(),"modified":metadata.modified().ok().and_then(|time|time.duration_since(std::time::UNIX_EPOCH).ok()).map(|time|time.as_nanos().to_string())}));
    let scope = json!({"conversationId":ctx.conversation_id,"projectId":ctx.project_id,"permission":ctx.permission_mode,
        "dataRoot":ctx.data_home,"extensionRoot":ctx.extension_root,"workdir":std::fs::canonicalize(&ctx.workdir).unwrap_or_else(|_|ctx.workdir.clone()),"authorizedRoots":roots,
        "configuration":configuration,"configurationGeneration":ctx.runtime.configuration_generation.load(Ordering::Acquire),
        "mcpGeneration":super::gateway_mcp::configuration_generation(ctx),"schema":schema,"toolhost":ctx.toolhost_exe,"hostVersion":host});
    Ok(json!({"fingerprint":super::approval_grant::fingerprint(name,args,target.as_deref(),&scope,&proof),
        "kind":if target.is_some(){"same-file-write"}else{"exact-invocation"}}))
}

pub async fn execute(
    tool_name: &str,
    args: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    // Invalid read pages are correctable before configuration, discovery or schema execution.
    // 错误读取分页在配置、发现和通用 schema 校验前返回可纠正回执，不扫描目录或改变读取游标。
    let pagination = match tool_name {
        "skill.list" => super::service_skill_discovery::list_page(&args).map(|_| ()),
        "skill.resource.read" => super::service_skills::resource_page(&args).map(|_| ()),
        _ => Ok(()),
    };
    pagination.map_err(|error| {
        failure(&error.code, &error.message, error.status_code)
            .with_details(Value::Object(error.details))
    })?;
    let selected = if workspace_path_tool(tool_name,&args) {
        Some(select_workspace_context(ctx,&args).await?)
    } else { None };
    let ctx = selected.as_ref().unwrap_or(ctx);
    let checked = if tool_name.starts_with("skill.")
        || tool_name.starts_with("mcp.")
        || tool_name == "tool.load"
        || tool_name == "tool.search"
    {
        Some(checked_configuration(ctx).await?)
    } else {
        None
    };
    let ctx = checked.as_ref().unwrap_or(ctx);
    let approval_arguments = args.clone();
    // Invalidate approval if its target or configuration changes while waiting; execution checks again.
    // 等待时目标或配置变化使旧审批失效；执行前再次核验。
    if ctx.approved && ctx.config["approvedInvocation"]["taskApproval"].is_object() {
        let binding = task_approval_binding(tool_name, &approval_arguments, ctx, &ctx.config["approvedInvocation"]).await?;
        if binding["fingerprint"] != ctx.config["approvedInvocation"]["taskApproval"]["fingerprint"] {
            return Err(failure("TOOL_APPROVAL_SCOPE_CHANGED", "审批目标、参数或权限范围已变化，操作未派发，请重新批准。", 409));
        }
    }
    // Keep the largest native dispatch branch off the caller's async frame.
    // 将较大的原生执行分支放到堆上，避免调试构建的浏览器握手层层占用线程栈。
    let started = std::time::Instant::now();
    let outcome = Box::pin(execute_inner(tool_name, args, ctx)).await;
    super::gateway_tool_memory::remember_outcome(tool_name, &outcome, started.elapsed(), ctx).await;
    let mut result = outcome?;
    if result["requiresApproval"] == true && result["invocation"].is_object() {
        result["invocation"]["taskApproval"] = task_approval_binding(tool_name, &approval_arguments, ctx, &result["invocation"]).await?;
    }
    if result.is_object() && ctx.config["activeMount"].is_object() {
        result["mountId"] = ctx.config["activeMountId"].clone();
        result["mountName"] = ctx.config["activeMount"]["name"].clone();
    }
    let server = if let Some(suffix) = tool_name.strip_prefix("mcp.") {
        let id = suffix.split('.').next().unwrap_or("");
        super::gateway_mcp::servers(ctx)
            .into_iter()
            .find(|server| server["id"] == id)
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let environment = execution_environment(tool_name, &result, &server)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("executionEnvironment".into(), environment);
    }
    Ok(result)
}

/// Cache only discovery metadata; reading or running a skill always obtains its current original body.
/// 只缓存发现元信息；读取或运行技能始终取得当前原文，缓存不提供执行或权限依据。
pub(crate) fn invalidate_skill_metadata(ctx: &GatewayToolContext) {
    if let Ok(mut cache) = ctx.runtime.skill_headers.lock() { cache.clear(); }
}

async fn skill_metadata(
    path: &Path,
    version: super::local_files::FileVersion,
    include_body: bool,
    ctx: &GatewayToolContext,
) -> Result<Option<Value>, GatewayError> {
    if !include_body {
        if let Some(cached) = ctx.runtime.skill_headers.lock().ok()
            .and_then(|cache| cache.get(path).filter(|entry| entry.version == version).cloned())
        {
            return Ok(cached.metadata);
        }
    }
    let source = path.to_owned();
    let (text, hash) = tokio::task::spawn_blocking(move || {
        let bytes = super::local_files::read_checked(
            &source, 2 * 1024 * 1024, false, "APP_SKILL_CHANGED",
        )
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| failure("INVALID_APP_SKILL", "技能正文不是 UTF-8。", 400))?
            .trim_start_matches('\u{feff}')
            .to_owned();
        Ok::<_, GatewayError>((text, super::local_files::sha256(&bytes)))
    })
    .await
    .map_err(|_| failure("TOOL_WORKER_FAILED", "技能读取线程退出。", 500))??;
    let header = text.strip_prefix("---")
        .and_then(|text| text.split_once("\n---").map(|(header, _)| header))
        .and_then(|header| serde_yaml::from_str::<Value>(header).ok())
        .filter(|header| header["name"].as_str().is_some_and(|name| !name.is_empty())
            && header["description"].as_str().is_some_and(|description| !description.is_empty()));
    let mut metadata = header.map(|header| json!({
        "name":header["name"],"description":header["description"],
        "sha256":hash,"standardCompliant":true,
    }));
    if let Ok(mut cache) = ctx.runtime.skill_headers.lock() {
        if cache.len() >= 512 { cache.clear(); }
        cache.insert(path.to_owned(), CachedSkillHeader { version, metadata:metadata.clone() });
    }
    if include_body {
        if let Some(metadata) = &mut metadata { metadata["content"] = json!(text); }
    }
    Ok(metadata)
}

/// Select task-related skill headers without loading their bodies or executing a discovery tool call.
/// 按当前任务选择技能头，不加载正文、不额外派发发现工具；普通聊天无需调用此入口。
pub(crate) async fn relevant_skill_headers(
    ctx: &GatewayToolContext,
    query: &str,
) -> Result<Vec<Value>, GatewayError> {
    if query.trim().is_empty() { return Ok(Vec::new()); }
    let checked = checked_configuration(ctx).await?;
    let inventory = skills("skill.list", json!({}), &checked).await?;
    let headers = inventory["skills"].as_array().cloned().unwrap_or_default();
    // Reuse bilingual task signals so Chinese requests can find the shipped English skill names.
    // 复用双语任务信号，使中文要求也能找到随包英文技能名，不增加模型判断调用。
    let signals = super::discovery::selection_signals(&json!({"message":query}));
    let mut topics = [
        ("web", "web-research"), ("browser", "browser-workflow"),
        ("desktop", "desktop-workflow"), ("hostTerminal", "terminal-workflow"),
        ("deviceState", "terminal-workflow"), ("docs", "workspace-inspect"),
    ].into_iter().filter(|(signal, _)| signals[*signal] == true)
        .map(|(_, topic)| topic).collect::<Vec<_>>();
    topics.extend(super::discovery::authoring_topics(query));
    if super::intent::matches(r"代码|编程|修复|重构|\b(?:code|coding|refactor|debug|implementation)\b", &query.to_lowercase()) {
        topics.push("code-workflow");
    }
    if super::intent::matches(r"记忆|记住|偏好|\b(?:memory|remember|preference)\b", &query.to_lowercase()) {
        topics.push("memory-workflow");
    }
    if super::intent::matches(r"修改|编辑|删除|新建文件|\b(?:edit|modify|delete)\b", &query.to_lowercase()) {
        topics.push("safe-file-edit");
    }
    let topics = topics.join(" ");
    let query = format!("{topics} {query}");
    let indices = super::discovery::calculate("tools.searchTools", &json!({"query":query,"descriptors":headers}))
        .map_err(|code| failure(&code, "技能头索引不可用。", 500))?;
    Ok(indices.as_array().into_iter().flatten().filter_map(Value::as_u64)
        .filter_map(|index| headers.get(index as usize).cloned()).take(16).collect())
}

async fn skills(name: &str, args: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let (list_offset, list_limit) = if name == "skill.list" {
        super::service_skill_discovery::list_page(&args).map_err(|error| {
            failure(&error.code, &error.message, error.status_code)
                .with_details(Value::Object(error.details))
        })?
    } else {
        (0, 128)
    };
    let official = ctx.config["officialToolsRoot"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.extension_root.join("official-tools"))
        .join("Skills");
    let mut roots = vec![
        ctx.workdir.join(".kynxa/skills"),
        ctx.extension_root.join("Skills"),
        official.clone(),
    ];
    roots.extend(
        ctx.config["skillDirectories"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(PathBuf::from),
    );
    let mut available = Vec::new();
    'roots: for root in roots.into_iter().filter(|root| root.is_dir()) {
        let mut entries = tokio::fs::read_dir(&root)
            .await
            .map_err(|_| failure("APP_SKILL_UNAVAILABLE", "技能目录不可读。", 503))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| failure("APP_SKILL_UNAVAILABLE", "技能目录读取失败。", 503))?
        {
            check_cancelled(ctx)?;
            if available.len() >= 128 {
                break;
            }
            let path = entry.path().join("SKILL.md");
            let Ok(Some(metadata)) = super::local_files::inspect(&path, false, false) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            // A selected skill needs only its own body; unrelated packages must not slow or break that read.
            // 已选择技能只读取自身正文，其他包的大小或损坏不能拖慢、阻断该次读取。
            let identity = super::service_skill_discovery::identity(
                &path,
                &json!({"bundledDirectory":official,"officialDirectory":official}),
            );
            let id = identity["id"].as_str().unwrap_or("");
            if id.is_empty()
                || name != "skill.list" && args["id"] != id
                || ctx.config["disabledSkills"].as_array().is_some_and(|disabled| {
                    disabled.iter().any(|entry| entry == id
                        || identity["legacyIds"].as_array().is_some_and(|aliases| aliases.contains(entry)))
                })
                || available.iter().any(|entry: &Value| entry["id"] == id)
            {
                continue;
            }
            let Ok(version) = super::local_files::file_version(&path, &metadata) else {
                continue;
            };
            let Some(mut metadata) = skill_metadata(&path, version, name != "skill.list", ctx).await? else {
                continue;
            };
            metadata["id"] = json!(id);
            metadata["source"] = json!(path);
            available.push(metadata);
            if name != "skill.list" {
                break 'roots;
            }
        }
    }
    if name == "skill.list" {
        available.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        return Ok(
            json!({"skills":available.into_iter().skip(list_offset)
            .take(list_limit).map(|mut entry| {entry.as_object_mut().unwrap().remove("content");entry}).collect::<Vec<_>>() }),
        );
    }
    let skill = available
        .into_iter()
        .find(|entry| entry["id"] == args["id"])
        .ok_or_else(|| failure("APP_SKILL_NOT_FOUND", "未找到启用技能。", 404))?;
    if name == "skill.read" {
        return Ok(skill);
    }
    let root = Path::new(skill["source"].as_str().unwrap_or(""))
        .parent()
        .unwrap_or(Path::new("."))
        .to_owned();
    if name == "skill.resource.read" {
        return native(
            ctx,
            "skill.readResource",
            json!({"root":root,"path":args["path"],"offset":args["offset"],"limit":args["limit"]}),
        )
        .await;
    }
    if name == "skill.inspect" || name == "skill.check" {
        let phase = inspect_package(root, ctx).await?;
        return Ok(
            json!({"skill":skill,"inventory":phase,"runtime":"native-windows","nodeAvailable":ctx.config["officialToolsRoot"].as_str()
                .and_then(|root| Path::new(root).parent()).is_some_and(|root| root.join("runtime/mcp/node/node.exe").is_file())}),
        );
    }
    if name == "skill.run" {
        let resolved = native(
            ctx,
            "skill.resolve",
            json!({"root":root,"path":args["path"]}),
        )
        .await?;
        if !["js", "mjs", "cjs"].contains(
            &Path::new(resolved["path"].as_str().unwrap_or(""))
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or(""),
        ) {
            return Err(failure(
                "SKILL_RUNTIME_UNSUPPORTED",
                "当前技能脚本执行支持私有 Node 的 JS/MJS/CJS。",
                400,
            ));
        }
        let phase = inspect_package(root.clone(), ctx).await?;
        let inventory = &phase["inventory"];
        if inventory["valid"] != true || inventory["truncated"] == true {
            return Err(failure(
                "INVALID_APP_SKILL",
                "技能包未完整通过文件与哈希校验，未派发脚本。",
                400,
            ));
        }
        let files = inventory["files"]
            .as_array()
            .ok_or_else(|| failure("INVALID_APP_SKILL", "技能文件清单无效。", 400))?;
        let script = resolved["relativePath"].as_str().unwrap_or("");
        let selected = files
            .iter()
            .find(|file| file["relativePath"] == script)
            .ok_or_else(|| {
                failure(
                    "APP_SKILL_RESOURCE_NOT_FOUND",
                    "脚本不在已核验技能包中。",
                    404,
                )
            })?;
        let manifest = files.iter().map(|file| json!({"path":file["relativePath"],"size":file["size"],"sha256":file["sha256"]})).collect::<Vec<_>>();
        let identity = json!({"id":skill["id"],"source":skill["source"],"sha256":skill["sha256"],"script":script,"files":manifest});
        let needs = super::policy::calculate("tools.needsApproval",
            &json!({"name":name,"permissionMode":ctx.permission_mode,"options":{"verifiedSandbox":true}}))
            .map_err(|message| failure("TOOL_POLICY_FAILED", &message, 500))?;
        if needs == true && !ctx.approved {
            let mut pending = approval(name, &args, ctx);
            pending["invocation"]["skillIdentity"] = identity;
            return Ok(pending);
        }
        // A resumed approval authorizes the inspected package, not a replacement at the same path.
        // 恢复审批仅授权已检查的技能包，不能执行相同路径后来被替换的脚本。
        if ctx.approved
            && ctx.config["approvedInvocation"].is_object()
            && ctx.config["approvedInvocation"]["skillIdentity"] != identity
        {
            return Err(failure(
                "APP_SKILL_CHANGED",
                "技能包在审批期间发生变化，未派发脚本。",
                409,
            ));
        }
        check_configuration_generation(ctx)?;
        let result = super::gateway_process::execute("terminal.run",json!({"command":"node","args":args["args"],
            "timeoutMs":args["timeoutMs"].as_u64().unwrap_or(120_000),"skill":{"root":root,"script":script,"files":manifest}}),ctx).await?;
        let proof = &result["skillExecution"];
        if proof["manifestVersion"] != 1
            || proof["readOnlyPackage"] != true
            || proof["hashChecked"] != true
            || proof["script"] != script
            || proof["scriptSha256"] != selected["sha256"]
        {
            return Err(failure(
                "APP_SKILL_EXECUTION_UNVERIFIED",
                "技能执行回执未匹配本次已核验脚本。",
                500,
            ));
        }
        return Ok(result);
    }
    Err(failure(
        "SKILL_RUNTIME_UNSUPPORTED",
        "此技能依赖发布包不提供的脚本运行时；请使用 Markdown 流程或兼容的原生 MCP。不会安装脚本运行时或转到未批准的宿主执行。",
        503,
    ))
}

async fn inspect_package(root: PathBuf, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let cancellation = Arc::new(AtomicBool::new(*ctx.cancellation.borrow()));
    let signal = cancellation.clone();
    let mut receiver = ctx.cancellation.clone();
    let monitor = tokio::spawn(async move {
        while receiver.changed().await.is_ok() {
            if *receiver.borrow_and_update() {
                signal.store(true, Ordering::Release);
                break;
            }
        }
    });
    let result = tokio::task::spawn_blocking(move || {
        let mut inventory = super::service_skills::SkillInventory::default();
        let call = |result: super::service_error::ServiceResult<Value>| {
            result.map_err(|error| failure(&error.code, &error.message, error.status_code))
        };
        let mut phase = call(inventory.apply(
            "skill.inventoryInit",
            &json!({"root":root,"hashes":true}),
            &Some(cancellation.clone()),
        ))?;
        while !phase["authorization"].is_null() {
            phase = call(inventory.apply(
                "skill.inventoryNext",
                &json!({"allowed":true}),
                &Some(cancellation.clone()),
            ))?;
        }
        Ok::<_, GatewayError>(phase)
    })
    .await
    .map_err(|_| failure("TOOL_WORKER_FAILED", "技能检查线程退出。", 500));
    monitor.abort();
    result?
}

pub(super) fn bundled_mcp_manifest(ctx: &GatewayToolContext) -> Option<(PathBuf, Value)> {
    use std::io::Read;
    let application = Path::new(ctx.config["officialToolsRoot"].as_str()?).parent()?;
    let path = application.join("runtime/mcp/manifest.json");
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let manifest: Value = serde_json::from_slice(&bytes).ok()?;
    if manifest["schemaVersion"] != 1
        || manifest["node"]["path"] != "runtime/mcp/node/node.exe"
        || manifest["node"]["sha256"]
            != "63c259c81e5d472b5f11c8d506070130cb04a1ecf84b80377a34ed6ec9048088"
    {
        return None;
    }
    Some((application.to_path_buf(), manifest))
}

pub(super) fn bundled_mcp_path(application: &Path, relative: &Value) -> Option<PathBuf> {
    let relative = Path::new(relative.as_str()?);
    if relative.is_absolute()
        || !relative.starts_with("runtime/mcp")
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let path = application.join(relative);
    path.is_file().then_some(path)
}

pub fn mcp_presets(ctx: &GatewayToolContext) -> Vec<Value> {
    let catalog: Value =
        serde_json::from_str(include_str!("gateway-catalog.json")).expect("static catalog");
    let mut presets = catalog["mcpPresets"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let bundled = bundled_mcp_manifest(ctx);
    for preset in &mut presets {
        if preset["id"] == "context7" {
            preset["server"] = json!({"id":"context7","name":"Context7","enabled":true,"transport":"streamable-http","url":"https://mcp.context7.com/mcp","command":"","args":[],"disabledTools":[],"protocolVersion":"2025-11-25","startupTimeoutMs":30000});
            preset["requirements"] = json!([]);
            preset["notes"] = json!([
                "Native gateway connects to the official remote HTTP service; no npm/npx is started."
            ]);
        }
        let command = preset["server"]["command"].as_str().unwrap_or("");
        if ["npx", "node", "npm"].contains(&command) {
            let installed = bundled.as_ref().and_then(|(application, manifest)| {
                let service = manifest["services"]
                    .as_array()?
                    .iter()
                    .find(|service| service["id"] == preset["id"])?;
                let package_specifier = format!(
                    "{}@{}",
                    service["package"].as_str().unwrap_or(""),
                    service["version"].as_str().unwrap_or("")
                );
                if !preset["server"]["args"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|argument| argument == package_specifier)
                {
                    return None;
                }
                let node = bundled_mcp_path(application, &manifest["node"]["path"])?;
                let entry = bundled_mcp_path(application, &service["entry"])?;
                let browser = if ["playwright", "chrome-devtools"]
                    .contains(&preset["id"].as_str().unwrap_or(""))
                {
                    Some(bundled_mcp_path(application, &manifest["browser"]["path"])?)
                } else {
                    None
                };
                Some((application.clone(), node, entry, browser))
            });
            if let Some((application, node, entry, browser)) = installed {
                // Only verified bundled defaults are trusted here; user overrides retain their own trust and credentials.
                // 此处仅信任固定随包默认项；用户覆盖仍保留自己的信任决定及凭据配置。
                let args = preset["server"]["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .skip(2)
                    .cloned()
                    .collect::<Vec<_>>();
                preset["server"]["command"] = json!(node);
                preset["server"]["cwd"] = json!(application.join("runtime/mcp/services"));
                preset["server"]["trusted"] = json!(true);
                preset["server"]["args"] =
                    json!([json!(entry)].into_iter().chain(args).collect::<Vec<_>>());
                if let Some(browser) = browser {
                    let browser_option = if preset["id"] == "playwright" {
                        "--executable-path"
                    } else {
                        "--executablePath"
                    };
                    let args = preset["server"]["args"].as_array_mut().unwrap();
                    args.push(json!(browser_option));
                    args.push(json!(browser));
                }
                if preset["id"] == "playwright" {
                    // Installed files stay read-only; browser artifacts belong to the managed extension cache.
                    // 安装文件保持只读；浏览器产物写入应用管理的扩展缓存。
                    let args = preset["server"]["args"].as_array_mut().unwrap();
                    args.push(json!("--output-dir"));
                    args.push(json!(ctx.extension_root.join("McpCache/playwright")));
                }
                preset["runtimeCompatible"] = json!(true);
                preset["bundledRuntime"] = json!("node");
                preset["requirements"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|requirement| requirement["type"] != "runtime");
                preset["notes"] = json!([
                    "Fixed private Node and installed MCP dependencies are bundled; startup performs no npm/npx downloads. Accounts, API keys and explicit database configuration remain separate requirements.",
                    "固定私有 Node 与 MCP 依赖随包提供，启动不调用 npm/npx 下载；账号、API Key 和数据库配置仍须另行提供。"
                ]);
                continue;
            }
            preset["runtimeCompatible"] = json!(false);
            preset["runtimeDiagnostic"] = json!("MCP_BUNDLED_RUNTIME_MISSING");
            preset["optionalRuntime"] = json!("node");
            preset["notes"] = json!([
                "The Rust core supports an installed Node executable with an installed MCP entry script. Configure their absolute paths and trust explicitly; startup never downloads packages through npm/npx.",
                "Rust 核心支持已安装 Node 和 MCP 入口脚本；须配置绝对路径并明确授予信任，启动不通过 npm/npx 下载依赖。"
            ]);
        } else {
            preset["runtimeCompatible"] = json!(true);
        }
    }
    presets
}

pub async fn get_config(ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    // Inspect preset files on a blocking thread; initialize and read configuration under one lock.
    // 在阻塞线程检查预设文件；一次加锁完成配置初始化和读取。
    let preset_context = ctx.clone();
    let presets = tokio::task::spawn_blocking(move || mcp_presets(&preset_context))
        .await
        .map_err(|_| failure("TOOL_WORKER_FAILED", "随包工具配置读取线程结束。", 500))?;
    let config = native(ctx,"config.initRead",json!({"root":ctx.extension_root,"presets":presets,"metadata":{"officialToolsRoot":ctx.config["officialToolsRoot"],"userToolsRoot":ctx.extension_root,"officialPackageVersion":"native-v1"}})).await?;
    let mut configured = ctx.clone();
    configured.config = config.clone();
    super::gateway_tool_memory::synchronize(&configured).await;
    Ok(config)
}

pub async fn update_config(input: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let _update = ctx.runtime.configuration_updates.write().await;
    get_config(ctx).await?;
    // Invalidate already checked dispatches before the first write can yield to another task.
    // 首次配置写入进入异步等待前使已检查的派发失效，避免检查与保存之间的竞争。
    ctx.runtime
        .configuration_generation
        .fetch_add(1, Ordering::AcqRel);
    ctx.runtime.semantic.clear();
    let result = native(ctx, "config.update", json!({"input":input})).await?;
    ctx.runtime.web_search.clear_queries();
    ctx.runtime.web_fetch.clear();
    super::gateway_mcp::reset(ctx).await;
    ctx.runtime.schemas.write().await.clear();
    let mut updated = ctx.clone();
    updated.config = result.clone();
    super::gateway_tool_memory::synchronize(&updated).await;
    prewarm(&updated);
    Ok(result)
}

/// Read saved source credentials and resolved MCP state; settings never initiate a connection handshake.
/// 只读取来源已存凭据及已有 MCP 状态；打开设置不握手，也不把已配置说成网络已经连通。
pub async fn providers(ctx: &GatewayToolContext, selected_id: &str) -> Value {
    let diagnostics = super::gateway_mcp::diagnostics(ctx).await;
    let loaded = ctx.runtime.schemas.read().await;
    let mut rows=ctx.config["mcpServers"].as_array().into_iter().flatten().filter(|server|{
        super::service_web::search_server(server)
            || loaded.iter().any(|tool|tool["serverId"]==server["id"]&&super::service_web::provider_kind(tool).is_some())
    }).map(|server|{
        let state=diagnostics.iter().find(|state|state["serverId"]==server["id"]);
        let enabled=server["enabled"]!=false;
        let cooling=ctx.runtime.web_search.cooling(server);
        let ready=enabled&&cooling.is_none()&&state.is_some_and(|state|state["ready"]==true);
        json!({"id":server["id"],"name":server["name"].as_str().unwrap_or("Web search"),
            "enabled":enabled,"ready":ready,"state":if !enabled{"disabled"}else if cooling.is_some(){"cooldown"}else if ready{"ready"}else if state.is_some_and(|state|!state["error"].is_null()){"error"}else{"disconnected"},
            "retryAfterMs":cooling.as_ref().and_then(|error|error.retry_after_ms),
            "origin":server["origin"].as_str().unwrap_or("user")})
    }).collect::<Vec<_>>();
    drop(loaded);
    // Reuse official model credentials without copying keys or searching when settings opens.
    // 复用官方模型凭据，不复制密钥，也不因打开设置发起搜索。
    let builtin = super::gateway_deepseek_search::source(ctx).await;
    let has_credential = builtin.is_some();
    let cooling = builtin.as_ref().and_then(|source| ctx.runtime.web_search.cooling(source));
    rows.push(json!({"id":super::gateway_deepseek_search::SOURCE_ID,"name":"DeepSeek Search",
        "origin":"builtin","enabled":true,"ready":has_credential && cooling.is_none(),
        "state":if !has_credential{"credential-required"}else if cooling.is_some(){"cooldown"}else{"configured"},
        "credentialState":if has_credential{"configured"}else{"unavailable"},
        "retryAfterMs":cooling.as_ref().and_then(|error|error.retry_after_ms)}));
    json!({"selectedId":selected_id,"providers":rows})
}

/// Existing settings routes do not grant model approval; execution still checks per-call context.
/// 原设置路由不授予模型审批；执行仍校验每次调用的上下文。
pub async fn handle_route(
    method: &str,
    path: &str,
    query: &HashMap<String, String>,
    body: Value,
    ctx: &GatewayToolContext,
) -> Result<Option<(u16, Value)>, GatewayError> {
    if !path.starts_with("/api/agent/") {
        return Ok(None);
    }
    let mut configured = ctx.clone();
    configured.config = get_config(ctx).await?;
    if configured.config["officialToolsRoot"].is_null() {
        configured.config["officialToolsRoot"] = ctx.config["officialToolsRoot"].clone();
    }
    let result = match (method, path) {
        ("GET", "/api/agent/config") => {
            let mut value = configured.config.clone();
            let (sandbox, host, desktop) = tokio::join!(
                super::gateway_process::capabilities("sandbox", &configured),
                super::gateway_process::capabilities("host", &configured),
                super::gateway_process::capabilities("desktop", &configured)
            );
            value["sandboxCapabilities"] = sandbox;
            value["hostTerminalCapabilities"] = host;
            value["desktopCapabilities"] = desktop;
            value
        }
        ("PUT", "/api/agent/config") => update_config(body, &configured).await?,
        ("GET", "/api/agent/tools") => {
            let mut tools = builtin_catalog();
            tools.extend(
                configured
                    .runtime
                    .schemas
                    .read()
                    .await
                    .iter()
                    .filter(|tool| tool["serverId"].is_string())
                    .cloned(),
            );
            json!({"tools":tools,"errors":[],"connections":super::gateway_mcp::diagnostics(&configured).await})
        }
        ("GET", "/api/agent/skills") => {
            let mut listed = configured.clone();
            listed.config["disabledSkills"] = json!([]);
            let mut value = skills("skill.list", json!({}), &listed).await?;
            for skill in value["skills"].as_array_mut().into_iter().flatten() {
                skill["enabled"] = json!(
                    !configured.config["disabledSkills"]
                        .as_array()
                        .is_some_and(|disabled| disabled.contains(&skill["id"]))
                );
            }
            value
        }
        ("GET", "/api/agent/mcp/catalog") => {
            let servers = configured.config["mcpServers"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let presets = mcp_presets(&configured)
                .into_iter()
                .map(|mut preset| {
                    let id = preset["id"].as_str().unwrap_or("");
                    let existing = servers.iter().find(|server| {
                        server["id"] == id || server["id"] == format!("official-{id}")
                    });
                    preset["alreadyConfigured"] = json!(existing.is_some());
                    if let Some(existing) = existing {
                        preset["configuredServerId"] = existing["id"].clone();
                    }
                    preset
                })
                .collect::<Vec<_>>();
            json!({"presets":presets,"reusedCapabilities":["filesystem","chat-memory","work-memory","tool-results","sandbox-terminal","host-terminal","desktop-control","native-browser","public-web-fetch"]})
        }
        ("POST", "/api/agent/mcp/refresh") => {
            json!({"tools":catalog(&configured).await?,"errors":[],"connections":super::gateway_mcp::diagnostics(&configured).await})
        }
        ("POST", "/api/agent/mcp/oauth/start") => {
            super::gateway_mcp_oauth::start(body["serverId"].as_str().unwrap_or(""), &configured).await?
        }
        ("GET", "/api/agent/mcp/oauth/status") => {
            super::gateway_mcp_oauth::status(query.get("serverId").map(String::as_str).unwrap_or(""), query.get("authorizationId").map(String::as_str).unwrap_or(""), &configured).await?
        }
        ("POST", "/api/agent/mcp/oauth/logout") => {
            super::gateway_mcp_oauth::logout(body["serverId"].as_str().unwrap_or(""), &configured).await?
        }
        ("POST", "/api/agent/mcp/disconnect") | ("POST", "/api/agent/mcp/reconnect") => {
            let id = body["serverId"]
                .as_str()
                .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "请指定 serverId。", 400))?;
            let server = super::gateway_mcp::servers(&configured)
                .into_iter()
                .find(|server| server["id"] == id)
                .ok_or_else(|| failure("MCP_SERVER_NOT_FOUND", "MCP 服务不存在。", 404))?;
            if path.ends_with("reconnect") {
                super::gateway_mcp::restart(&server, &configured).await?;
            } else {
                super::gateway_mcp::stop(id, &configured).await?;
            }
            json!({"tools":configured.runtime.schemas.read().await.clone(),"errors":[],"connections":super::gateway_mcp::diagnostics(&configured).await})
        }
        ("POST", "/api/agent/skills/import") => import_skill(body, &configured).await?,
        _ => {
            if method == "GET" && path.starts_with("/api/agent/skills/") {
                let segments = path
                    .trim_start_matches("/api/agent/skills/")
                    .split('/')
                    .collect::<Vec<_>>();
                let id = segments.first().copied().unwrap_or("");
                let mut args = json!({"id":id});
                let action = match segments.get(1).copied() {
                    Some("inspect") => "skill.inspect",
                    Some("check") => "skill.check",
                    Some("resource") => {
                        args["path"] = json!(query.get("path"));
                        args["offset"] = json!(
                            query
                                .get("offset")
                                .and_then(|value| value.parse::<u64>().ok())
                                .unwrap_or(0)
                        );
                        args["limit"] = json!(
                            query
                                .get("limit")
                                .and_then(|value| value.parse::<u64>().ok())
                                .unwrap_or(16000)
                        );
                        "skill.resource.read"
                    }
                    None => "skill.read",
                    _ => return Ok(None),
                };
                let mut listed = configured.clone();
                listed.config["disabledSkills"] = json!([]);
                let result = skills(action, args, &listed).await?;
                if action == "skill.read" {
                    json!({"skill":result})
                } else {
                    result
                }
            } else if method == "POST"
                && path.starts_with("/api/agent/mcp/catalog/")
                && path.ends_with("/add")
            {
                let id = path
                    .trim_start_matches("/api/agent/mcp/catalog/")
                    .trim_end_matches("/add");
                let preset = mcp_presets(&configured)
                    .into_iter()
                    .find(|preset| preset["id"] == id)
                    .ok_or_else(|| failure("MCP_PRESET_NOT_FOUND", "MCP 预设不存在。", 404))?;
                if body["expectedRevision"] != configured.config["revision"] {
                    return Err(failure(
                        "AGENT_CONFIG_CONFLICT",
                        "工具配置已更新，请刷新。",
                        409,
                    ));
                }
                let mut input = configured.config.clone();
                input["expectedRevision"] = input["revision"].clone();
                let mut servers = input["mcpServers"].as_array().cloned().unwrap_or_default();
                if !servers
                    .iter()
                    .any(|server| server["id"] == id || server["id"] == format!("official-{id}"))
                {
                    let mut server = preset["server"].clone();
                    server["id"] = json!(format!("official-{id}"));
                    servers.push(server);
                    input["mcpServers"] = json!(servers);
                    update_config(input, &configured).await?
                } else {
                    input
                }
            } else {
                return Ok(None);
            }
        }
    };
    Ok(Some((200, result)))
}

pub(crate) async fn import_skill(body: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let directory = body["directory"]
        .as_str()
        .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "请指定导入目录。", 400))?;
    let source = PathBuf::from(directory);
    if !source.is_absolute() {
        return Err(failure(
            "INVALID_APP_SKILL_IMPORT",
            "技能导入须指定绝对目录。",
            400,
        ));
    }
    let inspected = inspect_package(source.clone(), ctx).await?;
    let file = source.join("SKILL.md");
    let bytes =
        super::local_files::read_checked(&file, 2 * 1024 * 1024, false, "APP_SKILL_CHANGED")
            .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| failure("INVALID_APP_SKILL_IMPORT", "技能不是 UTF-8。", 400))?;
    let header = text
        .trim_start_matches('\u{feff}')
        .strip_prefix("---")
        .and_then(|text| text.split_once("\n---").map(|(header, _)| header))
        .ok_or_else(|| failure("INVALID_APP_SKILL_IMPORT", "技能缺少 frontmatter。", 400))?;
    let header: Value = serde_yaml::from_str(header)
        .map_err(|_| failure("INVALID_APP_SKILL_IMPORT", "技能 YAML 无效。", 400))?;
    let name = header["name"]
        .as_str()
        .filter(|name| {
            regex::Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
                .unwrap()
                .is_match(name)
                && name.len() <= 64
        })
        .ok_or_else(|| failure("INVALID_APP_SKILL_IMPORT", "技能名称无效。", 400))?;
    if inspected["inventory"]["valid"] != true {
        return Err(failure(
            "INVALID_APP_SKILL_IMPORT",
            "技能包检查失败；原文件已保留。",
            400,
        ));
    }
    let scope=body["scope"].as_str().unwrap_or("global");
    let import_root=match scope {"project"=>ctx.workdir.join(".kynxa"),"global"=>ctx.extension_root.clone(),_=>return Err(failure("INVALID_SKILL_SCOPE","技能范围须为 project 或 global。",400))};
    let target = import_root.join(if scope=="project" {"skills"} else {"Skills"}).join(name);
    let result = native(
        ctx,
        "skill.import",
        json!({"root":import_root,"scope":scope,"target":target,"files":inspected["inventory"]["files"]}),
    )
    .await?;
    let identity = native(
        ctx,
        "discovery.identity",
        json!({"path":target.join("SKILL.md")}),
    )
    .await?;
    let mut skill = skills("skill.read", json!({"id":identity["id"]}), ctx).await?;
    skill["enabled"] = json!(true);
    Ok(json!({"imported":result["imported"],"reused":result["reused"],"scope":scope,"skill":skill,"runtimeVerified":false}))
}

pub async fn shutdown(ctx: &GatewayToolContext) {
    super::gateway_mcp_oauth::shutdown(ctx).await;
    super::gateway_workspace_terminal::shutdown(ctx).await;
    super::gateway_process::shutdown(ctx).await;
    super::gateway_mcp::shutdown(ctx).await;
    super::gateway_browser::shutdown(ctx).await;
}

/// Resume a journaled MCP task without replaying its original mutation.
/// 恢复事件日志中的 MCP 任务身份，禁止重做原写操作。
pub async fn recover_mcp_task_checkpoint(handle: &Value, original_call_id: &str, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    super::gateway_mcp::recover_checkpoint(handle, original_call_id, ctx).await
}

/// Migration requires verified cleanup; best-effort application shutdown is not a readiness receipt.
/// 迁移必须取得核验清理结果；尽力退出应用不能作为迁移就绪回执。
pub async fn shutdown_checked(ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    super::gateway_mcp_oauth::shutdown(ctx).await;
    super::gateway_workspace_terminal::shutdown_checked(ctx).await?;
    super::gateway_process::shutdown_checked(ctx).await?;
    super::gateway_mcp::shutdown_checked(ctx).await?;
    super::gateway_browser::shutdown_checked(ctx).await?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_context() -> (GatewayToolContext, watch::Sender<bool>) {
    let root = std::env::temp_dir().join(format!("kynxa-native-tools-{}", uuid::Uuid::new_v4()));
    for directory in ["work", "data", "extensions", "official/Skills"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    let (sender, cancellation) = watch::channel(false);
    (
        GatewayToolContext {
            data_home: root.join("data"),
            extension_root: root.join("extensions"),
            workdir: root.join("work"),
            toolhost_exe: root.join("ToolHost.exe"),
            permission_mode: "smart".into(),
            conversation_id: uuid::Uuid::new_v4().to_string(),
            project_id: None,
            authorized_roots: vec![root.join("work")],
            cancellation,
            runtime: Arc::new(ToolRuntime::default()),
            config: json!({"officialToolsRoot":root.join("official"),"mcpServers":[]}),
            approved: false,
            events: None,
            external_execute: None,
        },
        sender,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_providers_describe_builtin_credentials_without_connecting_or_exposing_them() {
        let (ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let missing = providers(&ctx, "deepseek-search").await;
        let row = missing["providers"].as_array().unwrap().iter()
            .find(|row| row["id"] == "deepseek-search").unwrap();
        assert_eq!(row["ready"], false);
        assert_eq!(row["credentialState"], "unavailable");
        let home = root.join("models-home");
        std::fs::create_dir_all(&home).unwrap();
        ctx.runtime.model_home.set(home.clone()).unwrap();
        std::fs::write(home.join("connections.json"), json!({"version":1,"providers":[{
            "providerId":"deepseek","displayName":"DeepSeek","baseUrl":"https://api.deepseek.com",
            "apiKey":"fixture-provider-list-secret","models":["deepseek-flash"],"protocol":"openai-completions"
        }]}).to_string()).unwrap();
        let configured = providers(&ctx, "deepseek-search").await;
        let row = configured["providers"].as_array().unwrap().iter()
            .find(|row| row["id"] == "deepseek-search").unwrap();
        assert_eq!(row["state"], "configured");
        assert_eq!(row["credentialState"], "configured");
        assert_eq!(row["origin"], "builtin");
        assert!(!configured.to_string().contains("fixture-provider-list-secret"));
        assert!(super::super::gateway_mcp::diagnostics(&ctx).await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn task_file_write_binding_preserves_live_hash_protection_and_rejects_target_or_configuration_changes() {
        let (mut context, _sender)=test_context();context.permission_mode="ask".into();
        let root=context.workdir.parent().unwrap().to_owned();
        let initial=json!({"path":"report.md","content":"first","expectedHash":null});
        let pending=execute("filesystem.write",initial.clone(),&context).await.unwrap();
        assert_eq!(pending["requiresApproval"],true);
        let mut approved=context.clone();approved.approved=true;approved.config["approvedInvocation"]=pending["invocation"].clone();
        let first=execute("filesystem.write",initial,&approved).await.unwrap();
        let update=json!({"path":context.workdir.join("report.md"),"content":"updated","expectedHash":first["sha256"]});
        let current=execute("filesystem.write",update.clone(),&context).await.unwrap();
        assert_eq!(pending["invocation"]["taskApproval"]["fingerprint"],current["invocation"]["taskApproval"]["fingerprint"]);
        execute("filesystem.write",update.clone(),&approved).await.unwrap();
        assert_eq!(std::fs::read_to_string(context.workdir.join("report.md")).unwrap(),"updated");
        // Matching authorization is not a substitute for the newest overwrite precondition.
        // 相同授权不能替代最新覆盖前提，旧哈希即使已有任务授权也不能改写文件。
        assert!(execute("filesystem.write",update.clone(),&approved).await.is_err());
        assert_eq!(std::fs::read_to_string(context.workdir.join("report.md")).unwrap(),"updated");
        let mut changed=update.clone();changed["path"]=json!("other.md");
        assert_eq!(execute("filesystem.write",changed,&approved).await.unwrap_err().code,"TOOL_APPROVAL_SCOPE_CHANGED");
        let mut protected=update.clone();protected["path"]=json!(context.data_home.join("formal.json"));protected["reason"]=json!("A grant cannot bypass protected data");
        assert_eq!(execute("filesystem.write",protected,&approved).await.unwrap_err().code,"PROTECTED_APP_DATA");
        context.runtime.configuration_generation.fetch_add(1,Ordering::AcqRel);
        assert_eq!(execute("filesystem.write",update,&approved).await.unwrap_err().code,"TOOL_APPROVAL_SCOPE_CHANGED");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn mounted_files_keep_default_identity_and_revoke_only_the_removed_root() {
        use crate::storage::{gateway as data, workspace_mounts};
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let second = root.join("第二个 folder");
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(ctx.workdir.join("same.txt"),"DEFAULT").unwrap();
        std::fs::write(second.join("same.txt"),"SECOND").unwrap();
        let project_id = uuid::Uuid::new_v4().to_string();
        let first_id = uuid::Uuid::new_v4().to_string();
        let second_id = uuid::Uuid::new_v4().to_string();
        data::ensure_conversation(&ctx.data_home,&ctx.conversation_id,"Mount fixture").unwrap();
        let old = data::load_catalog(&ctx.data_home).unwrap();
        let catalog = json!({"Revision":old["Revision"],"Chats":[],"Projects":[{"Id":project_id,"Name":"Mount fixture",
            "Mounts":[{"Id":first_id,"Name":"Default","Path":ctx.workdir,"BindingRevision":1},{"Id":second_id,"Name":"Secondary","Path":second,"BindingRevision":1}],
            "DefaultMountId":first_id,"Chats":[{"Id":ctx.conversation_id,"Title":"Mounts","Messages":[
                {"Id":uuid::Uuid::new_v4().to_string(),"Role":"user","Content":"Mount fixture","Status":"completed"}]}]}]});
        data::handle("PUT","/api/conversations/catalog",&[],&catalog,&ctx.data_home).unwrap().unwrap();
        let relation = data::relationship(&ctx.data_home,&ctx.conversation_id).unwrap();
        ctx.project_id = Some(project_id);
        ctx.permission_mode = "full".into();
        ctx.config["workspaceMounts"] = relation["mounts"].clone();
        ctx.config["defaultMountId"] = relation["defaultMountId"].clone();
        ctx.config["bindingSnapshot"] = workspace_mounts::snapshot(&relation);
        ctx.authorized_roots.push(second.clone());
        let default_read = execute("filesystem.read",json!({"path":"same.txt"}),&ctx).await.unwrap();
        let second_read = execute("filesystem.read",json!({"mountId":second_id,"path":"same.txt"}),&ctx).await.unwrap();
        assert_ne!(default_read["sha256"],second_read["sha256"]);
        execute("filesystem.write",json!({"mountId":second_id,"path":"same.txt","content":"SECOND UPDATED","expectedHash":second_read["sha256"]}),&ctx).await.unwrap();
        assert_eq!(std::fs::read_to_string(ctx.workdir.join("same.txt")).unwrap(),"DEFAULT");
        assert_eq!(std::fs::read_to_string(second.join("same.txt")).unwrap(),"SECOND UPDATED");
        let listing = workspace_files(&json!({"mountId":second_id,"path":"."}),&ctx).await.unwrap();
        assert!(Path::new(listing["path"].as_str().unwrap()).is_absolute());
        assert!(listing["entries"].as_array().unwrap().iter().any(|entry|entry["name"] == "same.txt"));
        assert!(workspace_files(&json!({"mountId":second_id,"path":".."}),&ctx).await.is_err());
        // Explicit external file access retains its original permission/reason contract.
        // 明确指定的外部文件仍遵循原有权限与原因合同，不因多目录支持被全面禁止。
        std::fs::write(root.join("external.txt"),"EXTERNAL").unwrap();
        execute("filesystem.read",json!({"path":root.join("external.txt"),"reason":"Read the explicitly selected fixture file"}),&ctx).await.unwrap();
        let mut next = data::load_catalog(&ctx.data_home).unwrap();
        next["Projects"][0]["Mounts"].as_array_mut().unwrap().retain(|mount|mount["Id"] != second_id);
        data::handle("PUT","/api/conversations/catalog",&[],&next,&ctx.data_home).unwrap().unwrap();
        assert_eq!(execute("filesystem.read",json!({"mountId":second_id,"path":"same.txt"}),&ctx).await.unwrap_err().code,"WORKSPACE_MOUNT_CHANGED");
        assert_eq!(execute("filesystem.read",json!({"path":second.join("same.txt"),"reason":"An old authorization is not a live mount"}),&ctx).await.unwrap_err().code,"WORKSPACE_MOUNT_CHANGED");
        assert_eq!(execute("filesystem.read",json!({"path":"same.txt"}),&ctx).await.unwrap()["sha256"],default_read["sha256"]);
        assert_eq!(execute("work.folder.list",json!({}),&ctx).await.unwrap()["mounts"].as_array().unwrap().len(),1);
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_dispatch_keeps_large_async_branches_off_caller_frames() {
        fn future_bytes<F, Make: FnOnce() -> F>(_: Make) -> usize {
            std::mem::size_of::<F>()
        }
        let (ctx, _sender) = test_context();
        let workspace_input = json!({});
        let dispatch = future_bytes(|| execute("browser.open", json!({}), &ctx));
        let inner = future_bytes(|| execute_inner("browser.open", json!({}), &ctx));
        let browser = future_bytes(|| super::super::gateway_browser::execute("browser.open", json!({}), &ctx));
        let workspace = future_bytes(|| super::super::gateway_browser::workspace_open(&workspace_input, &ctx));
        eprintln!("async frame bytes: dispatch={dispatch}, inner={inner}, browser={browser}, workspace={workspace}");
        std::fs::remove_dir_all(ctx.workdir.parent().unwrap()).unwrap();
        assert!(dispatch < 64 * 1024, "Native dispatch must not inline the browser handshake frame.");
        assert!(browser < 64 * 1024, "Browser dispatch must not inline the WebSocket handshake frame.");
    }

    #[test]
    fn website_launches_target_the_workspace_unless_a_local_browser_is_explicit() {
        let (mut ctx, _keep) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let args = json!({"appPath":"C:/Browser/Chrome.EXE","args":["--new-window","https://example.org/"],"reason":"Open requested page"});
        for message in ["在右侧打开这个网站", "打开这个网站"] {
            ctx.config["trustedUserText"] = json!(message);
            let target = workspace_launch_target(&args, &ctx).unwrap().unwrap();
            assert_eq!(target["kind"], "browser");
            assert_eq!(target["url"], "https://example.org/");
        }
        ctx.config["trustedUserText"] = json!("用本机 Chrome 打开这个网站");
        assert!(workspace_launch_target(&args, &ctx).unwrap().is_none());
        ctx.config["trustedUserText"] = json!("在右侧打开这个网站");
        assert_eq!(workspace_launch_target(&json!({"appPath":"C:/Browser/chrome.exe"}), &ctx)
            .unwrap_err().code, "WORKSPACE_BROWSER_REQUIRED");
        assert!(workspace_launch_target(&json!({"appPath":"C:/Apps/notepad.exe"}), &ctx).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_mcp_presets_resolve_private_paths_and_preserve_credentials() {
        let (ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let missing = mcp_presets(&ctx);
        assert!(missing.iter().any(|preset| preset["id"] == "playwright"
            && preset["runtimeDiagnostic"] == "MCP_BUNDLED_RUNTIME_MISSING"));
        let node = "runtime/mcp/node/node.exe";
        let browser = "runtime/mcp/browsers/chromium/chrome.exe";
        let services = [
            (
                "playwright",
                "@playwright/mcp",
                "0.0.83",
                "runtime/mcp/services/node_modules/@playwright/mcp/cli.js",
            ),
            (
                "chrome-devtools",
                "chrome-devtools-mcp",
                "1.10.1",
                "runtime/mcp/services/node_modules/chrome-devtools-mcp/build/src/bin/chrome-devtools-mcp.js",
            ),
            (
                "brave-search",
                "@brave/brave-search-mcp-server",
                "2.1.4",
                "runtime/mcp/services/node_modules/@brave/brave-search-mcp-server/dist/index.js",
            ),
            (
                "dbhub",
                "@bytebase/dbhub",
                "1.4.0",
                "runtime/mcp/services/node_modules/@bytebase/dbhub/dist/index.js",
            ),
        ];
        for path in [node, browser]
            .into_iter()
            .chain(services.iter().map(|service| service.3))
        {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            // The fixture verifies discovery paths only; no fake executable or dependency is started.
            // 夹具只核验发现路径；不运行模拟执行文件或依赖。
            std::fs::write(file, "private dependency discovery fixture").unwrap();
        }
        let manifest = json!({"schemaVersion":1,
            "node":{"path":node,"sha256":"63c259c81e5d472b5f11c8d506070130cb04a1ecf84b80377a34ed6ec9048088"},
            "browser":{"path":browser},
            "services":services.iter().map(|service| json!({"id":service.0,"package":service.1,"version":service.2,"entry":service.3})).collect::<Vec<_>>()});
        std::fs::write(
            root.join("runtime/mcp/manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let presets = mcp_presets(&ctx);
        for service in services {
            let preset = presets
                .iter()
                .find(|preset| preset["id"] == service.0)
                .unwrap();
            assert_eq!(preset["server"]["command"], json!(root.join(node)));
            assert_eq!(preset["server"]["args"][0], json!(root.join(service.3)));
            assert_eq!(
                preset["server"]["cwd"],
                json!(root.join("runtime/mcp/services"))
            );
            assert_eq!(preset["server"]["trusted"], true);
            assert_eq!(preset["runtimeCompatible"], true);
            assert!(
                !preset["server"]["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|argument| argument == "-y")
            );
        }
        let brave = presets
            .iter()
            .find(|preset| preset["id"] == "brave-search")
            .unwrap();
        assert_eq!(
            brave["server"]["envRefs"]["BRAVE_API_KEY"],
            "KYNXA_BRAVE_API_KEY"
        );
        assert!(!brave["requirements"].as_array().unwrap().is_empty());
        let dbhub = presets
            .iter()
            .find(|preset| preset["id"] == "dbhub")
            .unwrap();
        assert!(
            dbhub["server"]["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|argument| argument == "<absolute-path-to-dbhub-readonly.toml>")
        );
        let context7 = presets
            .iter()
            .find(|preset| preset["id"] == "context7")
            .unwrap();
        assert_eq!(context7["server"]["transport"], "streamable-http");
        assert_eq!(context7["server"]["command"], "");
        assert!(bundled_mcp_path(&root, &json!("runtime/mcp/../../escaped.js")).is_none());
        assert!(bundled_mcp_path(&root, &json!("runtime/agent/kynxa-gateway.exe")).is_none());
        std::fs::remove_file(root.join(node)).unwrap();
        assert!(
            mcp_presets(&ctx)
                .iter()
                .any(|preset| preset["id"] == "playwright" && preset["runtimeCompatible"] == false)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn javascript_skill_requires_private_runtime_and_never_uses_host_fallback() {
        let (ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let skill_root = root.join("official/Skills/example");
        std::fs::create_dir_all(&skill_root).unwrap();
        std::fs::write(skill_root.join("SKILL.md"),
            "---\nname: example\ndescription: Native workflow fixture.\n---\nRead authorized sources with native tools.\n").unwrap();
        std::fs::write(
            skill_root.join("run.js"),
            "console.log('isolated-skill-fixture');",
        )
        .unwrap();
        let inventory = execute("skill.list", json!({}), &ctx).await.unwrap();
        let skill_id = inventory["skills"][0]["id"].as_str().unwrap().to_owned();
        let loaded = execute(
            "tool.load",
            json!({"names":["skill.run","skill.read"]}),
            &ctx,
        )
        .await
        .unwrap();
        let tools = loaded["tools"].as_array().unwrap();
        let script_tool = tools
            .iter()
            .find(|tool| tool["name"] == "skill.run")
            .unwrap();
        assert!(executable_descriptor(script_tool));
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == "skill.read" && executable_descriptor(tool))
        );
        let searched = execute("tool.search", json!({"query":"skill.run"}), &ctx)
            .await
            .unwrap();
        assert!(
            searched["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "skill.run" && tool["runtimeCompatible"] != false)
        );
        assert_eq!(
            execute(
                "skill.run",
                json!({"id":skill_id,"path":"run.js","args":[]}),
                &ctx
            )
            .await
            .unwrap_err()
            .code,
            "NODE_RUNTIME_UNAVAILABLE"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_provenance_preserves_browser_target_and_ignores_mcp_self_report() {
        let mut environment = super::super::contracts::calculate(
            "tools.resolveEnvironment",
            &json!({"descriptor":{"name":"browser.open","source":"builtin"}}),
        )
        .unwrap();
        environment["operationLocation"] = json!("unknown");
        environment["browserTarget"] = json!({"location":"unknown","mode":"existing-browser","evidence":"trusted-browser-configuration"});
        let claimed = json!({"executionEnvironment":environment});
        let browser = execution_environment("browser.open", &claimed, &Value::Null).unwrap();
        assert_eq!(browser["operationLocation"], "unknown");
        assert_eq!(browser["browserTarget"]["mode"], "existing-browser");
        let mcp = execution_environment(
            "mcp.remote.echo",
            &claimed,
            &json!({"transport":"streamable-http"}),
        )
        .unwrap();
        assert_eq!(mcp["executorLocation"], "remote-service");
        assert!(mcp["browserTarget"].is_null());
        let local = execution_environment("filesystem.read", &claimed, &Value::Null).unwrap();
        assert_eq!(local["operationLocation"], "gateway-host");
        assert!(local["browserTarget"].is_null());
        let sandbox = execution_environment("terminal.run", &json!({"sandbox":"appcontainer","tokenVerified":true,"activeProcessesAfterExit":0,"network":false}), &Value::Null).unwrap();
        assert_eq!(sandbox["sandboxVerified"], true);
        assert_eq!(sandbox["network"]["requestOrigin"], "unavailable");
        let unverified = execution_environment(
            "terminal.run",
            &json!({"sandbox":"appcontainer","tokenVerified":false,"activeProcessesAfterExit":0}),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(unverified["sandboxVerified"], false);
    }
    #[tokio::test]
    async fn write_restore_uses_current_hash_and_preserves_approval_boundary() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.permission_mode = "ask".into();
        let denied = execute(
            "filesystem.write",
            json!({"path":"a.txt","content":"A","expectedHash":null}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(denied["requiresApproval"], true);
        assert!(!ctx.workdir.join("a.txt").exists());
        ctx.permission_mode = "smart".into();
        execute(
            "filesystem.write",
            json!({"path":"a.txt","content":"A","expectedHash":null}),
            &ctx,
        )
        .await
        .unwrap();
        let a = execute("filesystem.read", json!({"path":"a.txt"}), &ctx)
            .await
            .unwrap();
        execute(
            "filesystem.write",
            json!({"path":"a.txt","content":"B","expectedHash":a["sha256"]}),
            &ctx,
        )
        .await
        .unwrap();
        let b = execute("filesystem.read", json!({"path":"a.txt"}), &ctx)
            .await
            .unwrap();
        execute(
            "filesystem.write",
            json!({"path":"a.txt","content":"A","expectedHash":b["sha256"]}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(ctx.workdir.join("a.txt")).unwrap(),
            "A"
        );
        assert!(
            execute(
                "filesystem.write",
                json!({"path":"a.txt","content":"C","expectedHash":b["sha256"]}),
                &ctx
            )
            .await
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn protected_data_is_not_reached_through_full_permission() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.permission_mode = "full".into();
        std::fs::write(ctx.data_home.join("secret.txt"), "private").unwrap();
        let error = execute(
            "filesystem.read",
            json!({"path":ctx.data_home.join("secret.txt")}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "PROTECTED_APP_DATA");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn skill_identity_and_resource_use_existing_contract_without_node() {
        let (ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let skill = root.join("official/Skills/demo");
        std::fs::create_dir_all(skill.join("references")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: demo\ndescription: Demo native test skill\n---\nRead this.",
        )
        .unwrap();
        std::fs::write(skill.join("references/note.md"), "body").unwrap();
        let listed = execute("skill.list", json!({}), &ctx).await.unwrap();
        let id = listed["skills"][0]["id"].clone();
        assert_eq!(id.as_str().unwrap().len(), 24);
        let unrelated = ctx.workdir.join(".kynxa/skills/unrelated");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(unrelated.join("SKILL.md"), [0xff]).unwrap();
        let read = execute("skill.read", json!({"id":id}), &ctx).await.unwrap();
        assert!(read["content"].as_str().unwrap().contains("Read this."));
        let page = execute(
            "skill.resource.read",
            json!({"id":id,"path":"references/note.md"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(page["content"], "body");
        assert!(
            execute(
                "skill.resource.read",
                json!({"id":id,"path":"../secret.txt"}),
                &ctx
            )
            .await
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn skill_header_cache_tracks_updates_disabling_and_deletion_without_caching_bodies() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let directory = root.join("official/Skills/web-research");
        let path = directory.join("SKILL.md");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, "---\nname: web-research\ndescription: Search public websites\n---\nOriginal procedure.").unwrap();
        assert!(relevant_skill_headers(&ctx, "").await.unwrap().is_empty());
        assert!(ctx.runtime.skill_headers.lock().unwrap().is_empty());
        let first = relevant_skill_headers(&ctx, "搜索网页").await.unwrap();
        assert_eq!(first[0]["name"], "web-research");
        assert!(first[0].get("content").is_none());
        assert!(ctx.runtime.skill_headers.lock().unwrap().get(&path).unwrap()
            .metadata.as_ref().unwrap().get("content").is_none());
        let cached = skills("skill.list", json!({}), &ctx).await.unwrap();
        assert_eq!(cached["skills"][0]["sha256"], first[0]["sha256"]);

        std::fs::write(&path, "---\nname: web-research\ndescription: Search updated public websites and verify evidence\n---\nUpdated original procedure.").unwrap();
        let updated = skills("skill.list", json!({}), &ctx).await.unwrap();
        assert_eq!(updated["skills"][0]["description"], "Search updated public websites and verify evidence");
        assert_ne!(updated["skills"][0]["sha256"], first[0]["sha256"]);
        let id = first[0]["id"].clone();
        let read = skills("skill.read", json!({"id":id}), &ctx).await.unwrap();
        assert!(read["content"].as_str().unwrap().contains("Updated original procedure."));
        ctx.config["disabledSkills"] = json!([id]);
        assert!(skills("skill.list", json!({}), &ctx).await.unwrap()["skills"].as_array().unwrap().is_empty());
        ctx.config["disabledSkills"] = json!([]);
        std::fs::remove_file(&path).unwrap();
        assert!(skills("skill.list", json!({}), &ctx).await.unwrap()["skills"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn document_skill_discovery_reads_only_relevant_headers_until_explicit_read() {
        let (ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        for name in ["document-authoring", "slides-authoring", "spreadsheets-authoring", "memory-workflow"] {
            let directory = root.join("official/Skills").join(name);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("SKILL.md"), format!("---\nname: {name}\ndescription: {name} native workflow\n---\nPRIVATE-BODY-{name}\n")).unwrap();
        }
        let headers = relevant_skill_headers(&ctx, "生成一份报告.docx").await.unwrap();
        assert_eq!(headers[0]["name"], "document-authoring");
        assert!(headers.len() <= 16);
        assert!(headers.iter().all(|header| header.get("content").is_none()));
        assert!(!serde_json::to_string(&headers).unwrap().contains("PRIVATE-BODY"));
        let loaded = execute("skill.read", json!({"id":headers[0]["id"]}), &ctx).await.unwrap();
        assert!(loaded["content"].as_str().unwrap().contains("PRIVATE-BODY-document-authoring"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn skill_pagination_is_correctable_before_configuration_or_directory_scanning() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let missing = root.join("never-created-skill-directory");
        ctx.config["officialToolsRoot"] = json!(missing);
        for (name, arguments, code, parameter, maximum) in [
            ("skill.list", json!({"offset":-1}), "INVALID_SKILL_LIST_PAGINATION", "offset", 128),
            ("skill.list", json!({"limit":129}), "INVALID_SKILL_LIST_PAGINATION", "limit", 128),
            ("skill.resource.read", json!({"id":"not-loaded","path":"notes.md","offset":2_097_153}),
                "INVALID_SKILL_RESOURCE_PAGINATION", "offset", 2_097_152),
            ("skill.resource.read", json!({"id":"not-loaded","path":"notes.md","limit":"8"}),
                "INVALID_SKILL_RESOURCE_PAGINATION", "limit", 16_000),
        ] {
            let error = execute(name, arguments, &ctx).await.unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(error.details["tool"], name);
            assert_eq!(error.details["parameter"], parameter);
            assert_eq!(error.details["maximum"], maximum);
            assert_eq!(error.details["executed"], false);
            assert_eq!(error.details["recoverable"], true);
            assert_eq!(error.response()["details"], error.details);
        }
        assert!(!missing.exists());
        assert!(ctx.runtime.skill_headers.lock().unwrap().is_empty());
        let official = root.join("official/Skills");
        ctx.config["officialToolsRoot"] = json!(root.join("official"));
        for name in ["first", "second"] {
            let skill = official.join(name);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(skill.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Pagination fixture.\n---\nBody.")).unwrap();
        }
        let page = execute("skill.list", json!({"offset":1,"limit":1}), &ctx).await.unwrap();
        let all = execute("skill.list", json!({}), &ctx).await.unwrap();
        assert_eq!(page["skills"].as_array().unwrap().len(), 1);
        assert_eq!(page["skills"][0], all["skills"][1]);
        shutdown(&ctx).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn skill_pagination_native_diagnostics_keep_only_safe_correction_fields() {
        let error = decode_native(Ok(json!({"error":{
            "code":"INVALID_SKILL_RESOURCE_PAGINATION","message":"invalid page","statusCode":400,
            "tool":"skill.resource.read","parameter":"limit","minimum":1,"maximum":16000,
            "executed":false,"requestDispatched":false,"recoverable":true,"privateBody":"must not propagate"
        }}))).unwrap_err();
        assert_eq!(error.details["parameter"], "limit");
        assert_eq!(error.details["maximum"], 16000);
        assert_eq!(error.details["recoverable"], true);
        assert!(error.details.get("privateBody").is_none());
    }

    #[test]
    fn schema_argument_validation_is_recoverable_without_dispatch() {
        let error = validate_schema(
            &json!({"type":"integer","minimum":1,"maximum":128}),
            &json!(0),
            "skill.list.limit",
        ).unwrap_err();
        assert_eq!(error.code, "INVALID_TOOL_ARGUMENTS");
        assert_eq!(error.details["parameter"], "skill.list.limit");
        assert_eq!(error.details["minimum"], 1);
        assert_eq!(error.details["maximum"], 128);
        assert_eq!(error.details["executed"], false);
        assert_eq!(error.details["requestDispatched"], false);
        assert_eq!(error.details["recoverable"], true);
    }

    #[tokio::test]
    async fn tool_search_never_connects_a_server_to_discover_its_header() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.config["mcpServers"] = json!([{"id":"offline","name":"Offline","enabled":true,"transport":"streamable-http","url":"http://127.0.0.1:1/mcp"}]);
        let value = execute("tool.search", json!({"query":"offline"}), &ctx)
            .await
            .unwrap();
        assert_eq!(value["tools"][0]["name"], "mcp.offline");
        assert!(ctx.runtime.mcp.lock().await.is_empty());
        let chinese_read = execute("tool.search", json!({"query":"读取文件"}), &ctx)
            .await
            .unwrap();
        assert!(
            chinese_read["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "filesystem.read")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn code_versions_track_current_authorized_request_and_release() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.config["activeRequestId"] = json!("first");
        std::fs::write(ctx.workdir.join("code.txt"), "A").unwrap();
        execute("filesystem.read", json!({"path":"code.txt"}), &ctx)
            .await
            .unwrap();
        let before = capture_code_version(&ctx).await.unwrap();
        assert_eq!(before["complete"], true);
        ctx.config["activeRequestId"] = json!("second");
        assert_eq!(
            capture_code_version(&ctx).await.unwrap()["files"],
            json!([])
        );
        release_request(&ctx);
        ctx.config["activeRequestId"] = json!("first");
        std::fs::write(ctx.workdir.join("code.txt"), "B").unwrap();
        let after = capture_code_version(&ctx).await.unwrap();
        assert_ne!(before["signature"], after["signature"]);
        std::fs::remove_file(ctx.workdir.join("code.txt")).unwrap();
        assert_eq!(
            capture_code_version(&ctx).await.unwrap()["files"][0]["state"],
            "missing"
        );
        release_request(&ctx);
        assert!(ctx.runtime.code_versions.lock().unwrap().is_empty());
        ctx.permission_mode = "ask".into();
        execute(
            "filesystem.write",
            json!({"path":"denied.txt","content":"D","expectedHash":null}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            capture_code_version(&ctx).await.unwrap()["files"],
            json!([])
        );
        release_request(&ctx);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn conversation_sandbox_can_work_without_exposing_formal_data() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.workdir = ctx.data_home.join("Sandboxes").join(&ctx.conversation_id);
        std::fs::create_dir_all(&ctx.workdir).unwrap();
        ctx.authorized_roots = vec![ctx.workdir.clone()];
        execute(
            "filesystem.write",
            json!({"path":"test.txt","content":"sandbox-body","expectedHash":null}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            execute("filesystem.read", json!({"path":"test.txt"}), &ctx)
                .await
                .unwrap()
                .to_string()
                .contains("sandbox-body")
        );
        let found = execute(
            "filesystem.search",
            json!({"path":".","query":"sandbox-body"}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(found.to_string().contains("test.txt"), "{found}");
        std::fs::write(ctx.data_home.join("catalog.json"), "formal").unwrap();
        ctx.permission_mode = "full".into();
        assert_eq!(
            execute(
                "filesystem.read",
                json!({"path":ctx.data_home.join("catalog.json")}),
                &ctx
            )
            .await
            .unwrap_err()
            .code,
            "PROTECTED_APP_DATA"
        );
        let foreign = ctx.data_home.join("Sandboxes/foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("secret.txt"), "private").unwrap();
        assert_eq!(
            execute(
                "filesystem.read",
                json!({"path":foreign.join("secret.txt")}),
                &ctx
            )
            .await
            .unwrap_err()
            .code,
            "PROTECTED_APP_DATA"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn archived_image_display_uses_scoped_executor_without_file_or_browser_dispatch() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let displayed = Arc::new(Mutex::new(Vec::new()));
        let captured = displayed.clone();
        ctx.external_execute = Some(Arc::new(move |name, args| {
            let captured = captured.clone();
            Box::pin(async move {
                captured.lock().unwrap().push((name, args.clone()));
                Ok(json!({"kind":"screenshot","presentation":"workspace","sourceResultRef":{"id":args["resultId"]}}))
            })
        }));
        let input = json!({"resultId":uuid::Uuid::new_v4().to_string(),"title":"Final result"});
        assert!(!workspace_path_tool("artifact.open", &input));
        assert_eq!(execute("artifact.open", input.clone(), &ctx).await.unwrap()["kind"], "screenshot");
        assert_eq!(displayed.lock().unwrap().as_slice(), &[("artifact.open".to_owned(), input)]);
        for invalid in [json!({}), json!({"path":"report.png","resultId":"saved"}),
            json!({"url":"https://example.org","resultId":"saved"}), json!({"path":"report.png","url":"https://example.org"})] {
            assert_eq!(execute("artifact.open", invalid, &ctx).await.unwrap_err().code, "INVALID_TOOL_ARGUMENTS");
        }
        assert_eq!(displayed.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn persistent_work_binding_requires_authority_and_keeps_current_roots() {
        let (mut ctx, _sender) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.project_id = Some(uuid::Uuid::new_v4().to_string());
        let external = root.join("new-work");
        std::fs::create_dir_all(&external).unwrap();
        let bindings = Arc::new(Mutex::new(Vec::new()));
        let captured = bindings.clone();
        ctx.external_execute = Some(Arc::new(move |name, args| {
            let captured = captured.clone();
            Box::pin(async move {
                captured.lock().unwrap().push((name, args.clone()));
                Ok(json!({"folderPath":args["path"]}))
            })
        }));
        let roots = ctx.authorized_roots.clone();
        let input = json!({"path":external,"reason":"long-term authorized work"});
        assert_eq!(
            execute("work.folder.bind", input.clone(), &ctx)
                .await
                .unwrap()["requiresApproval"],
            true
        );
        assert!(bindings.lock().unwrap().is_empty());
        ctx.approved = true;
        assert!(execute("work.folder.bind", input, &ctx).await.unwrap()["folderPath"].is_string());
        assert_eq!(ctx.authorized_roots, roots);
        assert_eq!(bindings.lock().unwrap().len(), 1);
        let invalid = execute(
            "work.folder.bind",
            json!({"path":ctx.data_home,"reason":"not allowed"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(invalid.code, "PROTECTED_APP_DATA");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn saved_request_capabilities_are_revocable_without_granting_new_servers() {
        let (mut ctx, _owner) = test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let skill_root = root.join("skills/fixture");
        std::fs::create_dir_all(&skill_root).unwrap();
        std::fs::write(
            skill_root.join("SKILL.md"),
            "---\nname: fixture\ndescription: Isolated skill\n---\nSynthetic body.\n",
        )
        .unwrap();
        std::fs::write(
            skill_root.join("test.mjs"),
            "// Never executed by this test.\n",
        )
        .unwrap();
        let hidden = mcp_presets(&ctx)
            .iter()
            .map(|preset| preset["id"].clone())
            .collect::<Vec<_>>();
        let initial = get_config(&ctx).await.unwrap();
        let server = json!({"id":"fixture","name":"Isolated fixture","enabled":true,"transport":"streamable-http","url":"http://127.0.0.1:1/rpc","trusted":false});
        ctx.config = update_config(json!({"version":1,"expectedRevision":initial["revision"],"mcpServers":[server.clone()],"skillDirectories":[root.join("skills")],"disabledSkills":[],"disabledOfficialMcpServers":hidden}), &ctx).await.unwrap();
        let listed = execute("skill.list", json!({}), &ctx).await.unwrap();
        let skill_id = listed["skills"][0]["id"].clone();
        assert!(skill_id.is_string(), "{listed}");
        ctx.permission_mode = "ask".into();
        let arguments = json!({"id":skill_id,"path":"test.mjs","args":[]});
        let pending = execute("skill.run", arguments.clone(), &ctx).await.unwrap();
        assert_eq!(pending["requiresApproval"], true);
        assert!(pending["invocation"]["skillIdentity"]["files"].is_array());
        let mut approved = ctx.clone();
        approved.approved = true;
        approved.config["approvedInvocation"] = pending["invocation"].clone();
        std::fs::write(
            skill_root.join("test.mjs"),
            "// Changed while approval was pending.\n",
        )
        .unwrap();
        assert_eq!(
            execute("skill.run", arguments, &approved)
                .await
                .unwrap_err()
                .code,
            "APP_SKILL_CHANGED"
        );
        let mut replacement = server.clone();
        replacement["id"] = json!("replacement");
        let saved = update_config(json!({"version":1,"expectedRevision":ctx.config["revision"],"mcpServers":[replacement],"skillDirectories":[root.join("skills")],"disabledSkills":[skill_id],"disabledOfficialMcpServers":hidden}), &ctx).await.unwrap();
        assert_eq!(
            execute("skill.read", json!({"id":skill_id}), &ctx)
                .await
                .unwrap_err()
                .code,
            "APP_SKILL_NOT_FOUND"
        );
        // A task grant bound to the skill call cannot be reused for another tool.
        // 绑定到技能调用的任务授权不能挪用于其他工具。
        assert_eq!(
            execute("mcp.fixture.echo", json!({}), &approved)
                .await
                .unwrap_err()
                .code,
            "TOOL_APPROVAL_SCOPE_CHANGED"
        );
        let mut approved_without_grant = approved.clone();
        approved_without_grant.config["approvedInvocation"] = Value::Null;
        assert_eq!(
            execute("mcp.fixture.echo", json!({}), &approved_without_grant)
                .await
                .unwrap_err()
                .code,
            "MCP_SERVER_NOT_FOUND"
        );
        assert_eq!(
            execute("mcp.replacement.echo", json!({}), &approved_without_grant)
                .await
                .unwrap_err()
                .code,
            "MCP_SERVER_NOT_FOUND"
        );
        let retained = checked_configuration(&ctx).await.unwrap();
        ctx.runtime
            .configuration_generation
            .fetch_add(1, Ordering::AcqRel);
        assert_eq!(
            check_configuration_generation(&retained).unwrap_err().code,
            "TOOL_CONFIGURATION_CHANGED"
        );
        assert_eq!(
            saved["revision"].as_u64(),
            ctx.config["revision"].as_u64().map(|revision| revision + 1)
        );
        shutdown(&ctx).await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
