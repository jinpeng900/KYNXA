//! MCP JSON-RPC transports implemented in Rust, including legacy SSE and streamable HTTP.
//! Rust 实现 MCP JSON-RPC、旧 SSE 和 Streamable HTTP；外部 Node 仅使用用户配置的已安装入口。

use super::gateway::{GatewayToolContext, check_cancelled, emit, failure, native};
use super::gateway_mcp_budget::{McpBudget, McpQuota};
use super::gateway_process::{OwnedProcessJob, bounded_line, hide, terminate};
use crate::gateway_contract::GatewayError;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc};

const MAX_RPC_BYTES: usize = 32 * 1024 * 1024;
/// Default to ten minutes when the server omits timeoutMs.
/// 服务未设置 timeoutMs 时，单次调用默认十分钟。
const DEFAULT_MCP_EXECUTION_MS: u64 = 600_000;
const PROTOCOL: &str = "2025-11-25";
const MCP_BUDGET_REFRESH: Duration = Duration::from_secs(5);

/// Gates schedule startup and shutdown; the existing client map owns connections.
/// 门控负责启停调度，连接仍由现有 client map 管理。
pub(crate) struct McpLifecycle {
    starts: Arc<McpQuota>,
    calls: Arc<McpQuota>,
    budget: std::sync::Mutex<McpBudget>,
    last_refresh: std::sync::Mutex<Option<tokio::time::Instant>>,
    refreshing: AtomicBool,
    idle_private_bytes: AtomicU64,
    idle_working_set_bytes: AtomicU64,
    idle_processes: AtomicUsize,
    idle_unknown_services: AtomicUsize,
    idle_sampled_at: AtomicU64,
    gates: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    pending: std::sync::Mutex<HashSet<String>>,
    stopped: std::sync::Mutex<HashSet<String>>,
    failures: Mutex<HashMap<String, (String, tokio::time::Instant, GatewayError)>>,
    generation: AtomicU64,
    foreground_waiters: AtomicUsize,
    foreground_active: AtomicUsize,
    closing: AtomicBool,
    prewarmed: AtomicBool,
    maintenance: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Default for McpLifecycle {
    fn default() -> Self {
        let budget = McpBudget::from_snapshot(None, available_cores());
        Self {
            starts: McpQuota::new(budget.starts),
            calls: McpQuota::new(budget.calls),
            budget: std::sync::Mutex::new(budget),
            last_refresh: Default::default(),
            refreshing: AtomicBool::new(false),
            idle_private_bytes: AtomicU64::new(0),
            idle_working_set_bytes: AtomicU64::new(0),
            idle_processes: AtomicUsize::new(0),
            idle_unknown_services: AtomicUsize::new(0),
            idle_sampled_at: AtomicU64::new(0),
            gates: Default::default(),
            pending: Default::default(),
            stopped: Default::default(),
            failures: Default::default(),
            generation: AtomicU64::new(0),
            foreground_waiters: AtomicUsize::new(0),
            foreground_active: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            prewarmed: AtomicBool::new(false),
            maintenance: Default::default(),
        }
    }
}
impl McpLifecycle {
    fn budget(&self) -> McpBudget {
        self.budget
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    fn apply_budget(&self, budget: McpBudget) {
        self.starts.set_limit(budget.starts);
        self.calls.set_limit(budget.calls);
        *self
            .budget
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = budget;
    }
    async fn gate(&self, id: &str) -> Arc<Mutex<()>> {
        let mut gates = self.gates.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(id).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(id.to_owned(), Arc::downgrade(&gate));
        gate
    }
    fn check(&self, id: &str, generation: u64) -> Result<(), GatewayError> {
        if self.closing.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != generation
        {
            return Err(failure(
                "MCP_START_CANCELLED",
                "MCP 生命周期已改变，旧启动已取消。",
                409,
            ));
        }
        if self
            .stopped
            .lock()
            .map_err(|_| failure("TOOL_STATE_POISONED", "MCP 停止状态不可用。", 500))?
            .contains(id)
        {
            return Err(failure(
                "MCP_STOPPED",
                "此 MCP 已主动断开；请明确重新连接后再使用。",
                409,
            ));
        }
        Ok(())
    }
}
fn available_cores() -> usize {
    std::thread::available_parallelism().map_or(2, usize::from)
}
struct BudgetRefresh<'a>(&'a AtomicBool);
impl Drop for BudgetRefresh<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Reuse the authority's adjusted headroom; MCP residency is already reflected in system availability.
/// 复用资源服务已调整的余量；MCP 常驻占用已体现在系统空闲量中，不能再次按整棵树预约。
async fn refresh_budget(ctx: &GatewayToolContext) {
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    {
        let mut last = lifecycle
            .last_refresh
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.is_some_and(|time| time.elapsed() < MCP_BUDGET_REFRESH)
            || lifecycle.refreshing.swap(true, Ordering::AcqRel)
        {
            return;
        }
        *last = Some(tokio::time::Instant::now());
    }
    let _refresh = BudgetRefresh(&lifecycle.refreshing);
    let snapshot = if let Some(service) = crate::resources::gateway::global() {
        service
            .request("snapshot", json!({}), Duration::from_secs(2))
            .await
            .ok()
    } else {
        None
    };
    let observed = McpBudget::from_snapshot(snapshot.as_ref(), available_cores());
    if observed.observed {
        lifecycle.apply_budget(observed);
    } else {
        let mut previous = lifecycle.budget();
        if previous.observed || previous.sampled_at.is_some() {
            // A temporarily missing observation must not expand an earlier pressure-limited budget.
            // 暂时缺失观测时不能借 fallback 扩大此前因压力收紧的额度。
            previous.observed = false;
            previous.reason = "resource-observation-unavailable";
            lifecycle.apply_budget(previous);
        } else {
            lifecycle.apply_budget(observed);
        }
    }
}
struct PendingStart {
    runtime: Arc<super::gateway::ToolRuntime>,
    id: String,
    foreground: bool,
    reserved: bool,
}
impl Drop for PendingStart {
    fn drop(&mut self) {
        if self.reserved {
            if let Ok(mut pending) = self.runtime.mcp_lifecycle.pending.lock() {
                pending.remove(&self.id);
            }
        }
        if self.foreground {
            self.runtime
                .mcp_lifecycle
                .foreground_waiters
                .fetch_sub(1, Ordering::AcqRel);
        }
    }
}
pub(crate) struct McpForeground(Arc<super::gateway::ToolRuntime>);
impl Drop for McpForeground {
    fn drop(&mut self) {
        self.0
            .mcp_lifecycle
            .foreground_active
            .fetch_sub(1, Ordering::AcqRel);
    }
}
pub(crate) fn foreground(runtime: &Arc<super::gateway::ToolRuntime>) -> McpForeground {
    runtime
        .mcp_lifecycle
        .foreground_active
        .fetch_add(1, Ordering::AcqRel);
    McpForeground(runtime.clone())
}

enum Transport {
    Stdio {
        child: Child,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
        stderr: tokio::task::JoinHandle<()>,
        _owned: OwnedProcessJob,
    },
    Http {
        client: reqwest::Client,
        url: url::Url,
        headers: HeaderMap,
        session: Option<String>,
    },
    Sse {
        client: reqwest::Client,
        endpoint: url::Url,
        headers: HeaderMap,
        receiver: mpsc::Receiver<Value>,
        reader: tokio::task::JoinHandle<()>,
    },
}

pub(crate) struct McpClient {
    transport: Transport,
    sequence: Arc<AtomicU64>,
    protocol: String,
    pub(crate) tools: Vec<Value>,
    configuration: String,
    close: tokio::sync::watch::Receiver<bool>,
    resources: bool,
    diagnostic_id: u64,
    last_used: tokio::time::Instant,
    subscriptions: HashSet<String>,
    external_node: bool,
    background_task: bool,
    memory_observation: Value,
    tasks: Value,
    parallel_active: Arc<AtomicUsize>,
    detached_read: bool,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if self.detached_read { self.parallel_active.fetch_sub(1, Ordering::AcqRel); }
        match &mut self.transport {
            Transport::Stdio { child, stderr, .. } => {
                let _ = child.start_kill();
                stderr.abort();
            }
            Transport::Sse { reader, .. } => reader.abort(),
            _ => {}
        }
    }
}

fn is_node(command: &str, args: &[Value]) -> bool {
    let filename = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_lowercase();
    [
        "node", "node.exe", "npm", "npm.cmd", "npx", "npx.cmd", "pnpm", "pnpm.cmd", "yarn",
        "yarn.cmd", "bun", "bun.exe", "deno", "deno.exe",
    ]
    .contains(&filename.as_str())
        || args.iter().filter_map(Value::as_str).any(|argument| {
            [".js", ".mjs", ".cjs"]
                .iter()
                .any(|suffix| argument.to_lowercase().ends_with(suffix))
        })
}

fn installed_path(path: &Path, directory: bool) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if if directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    } {
        return false;
    }
    for ancestor in path.ancestors() {
        let Ok(metadata) = std::fs::symlink_metadata(ancestor) else {
            return false;
        };
        if metadata.file_type().is_symlink() {
            return false;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
    }
    true
}
pub(crate) fn validate_runtime(server: &Value) -> Result<bool, GatewayError> {
    let command = server["command"].as_str().unwrap_or("");
    let args = server["args"].as_array().cloned().unwrap_or_default();
    let filename = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    let package_manager = |argument: &str| {
        argument.split_whitespace().any(|part| {
            let basename = Path::new(part)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(part)
                .to_ascii_lowercase();
            [
                "npm", "npm.cmd", "npx", "npx.cmd", "pnpm", "pnpm.cmd", "yarn", "yarn.cmd", "bun",
                "bun.exe", "deno", "deno.exe",
            ]
            .contains(&basename.as_str())
        })
    };
    if package_manager(command) || args.iter().filter_map(Value::as_str).any(package_manager) {
        return Err(failure(
            "MCP_AUTOMATIC_INSTALL_DISABLED",
            "MCP 首次调用不会下载或升级依赖；请配置已安装入口。",
            409,
        ));
    }
    if ["node", "node.exe"].contains(&filename.as_str()) {
        if !installed_path(Path::new(command), false) {
            return Err(failure(
                "MCP_NODE_DEPENDENCY_MISSING",
                "外部 Node MCP 须配置已安装 node.exe 的明确绝对路径。",
                503,
            ));
        }
        let cwd = server["cwd"]
            .as_str()
            .filter(|path| installed_path(Path::new(path), true));
        let script = args.iter().filter_map(Value::as_str).find(|arg| {
            [".js", ".mjs", ".cjs"]
                .iter()
                .any(|suffix| arg.to_ascii_lowercase().ends_with(suffix))
        });
        let Some(script) = script else {
            return Err(failure(
                "MCP_NODE_ENTRY_MISSING",
                "Node MCP 须配置已安装脚本入口，不接受临时求值或自动下载。",
                503,
            ));
        };
        let path = if Path::new(script).is_absolute() {
            Path::new(script).to_path_buf()
        } else {
            cwd.map(|cwd| Path::new(cwd).join(script)).ok_or_else(|| {
                failure(
                    "MCP_NODE_ENTRY_MISSING",
                    "相对 Node 入口需要明确的已安装 cwd。",
                    503,
                )
            })?
        };
        if !installed_path(&path, false)
            || args.iter().filter_map(Value::as_str).any(|arg| {
                ["-e", "--eval", "-p", "--print"].contains(&arg)
                    || arg.starts_with("--eval=")
                    || arg.starts_with("--print=")
            })
        {
            return Err(failure(
                "MCP_NODE_ENTRY_MISSING",
                "Node MCP 入口不存在或依赖临时求值；不会自动安装。",
                503,
            ));
        }
        return Ok(true);
    }
    if is_node(command, &args) {
        return Err(failure(
            "MCP_SCRIPT_RUNTIME_UNSUPPORTED",
            "脚本 MCP 需要明确的已安装运行时入口，不能通过其他启动器绕过。",
            409,
        ));
    }
    Ok(false)
}

pub(crate) fn servers(ctx: &GatewayToolContext) -> Vec<Value> {
    let source = ctx.config["mcpServers"]
        .as_array()
        .or_else(|| ctx.config["mcp"]["servers"].as_array());
    if let Some(source) = source {
        return source
            .iter()
            .filter(|server| server["enabled"] != false)
            .cloned()
            .collect();
    }
    if let Some(mapping) = ctx.config["mcpServers"].as_object() {
        return mapping
            .iter()
            .map(|(id, server)| {
                let mut server = server.clone();
                server["id"] = json!(id);
                server
            })
            .filter(|server| server["enabled"] != false)
            .collect();
    }
    let catalog: Value =
        serde_json::from_str(include_str!("gateway-catalog.json")).expect("static catalog");
    catalog["mcpPresets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|preset| preset["server"].clone())
        .filter(|server| server["transport"] == "streamable-http" && server["headerEnv"].is_null())
        .collect()
}

async fn headers(server: &Value, ctx: &GatewayToolContext) -> Result<HeaderMap, GatewayError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    for (field, value) in server["headers"].as_object().into_iter().flatten() {
        let name = HeaderName::from_bytes(field.as_bytes())
            .map_err(|_| failure("MCP_INVALID_CONFIG", "MCP 头名称无效。", 400))?;
        let value = HeaderValue::from_str(
            value
                .as_str()
                .ok_or_else(|| failure("MCP_INVALID_CONFIG", "MCP 头值无效。", 400))?,
        )
        .map_err(|_| failure("MCP_INVALID_CONFIG", "MCP 头值无效。", 400))?;
        headers.insert(name, value);
    }
    for (field, variable) in server["headerEnv"].as_object().into_iter().flatten() {
        let value = std::env::var(variable.as_str().unwrap_or("")).map_err(|_| {
            failure(
                "MCP_CREDENTIAL_MISSING",
                "MCP 所需认证环境变量未配置。",
                401,
            )
        })?;
        headers.insert(
            HeaderName::from_bytes(field.as_bytes())
                .map_err(|_| failure("MCP_INVALID_CONFIG", "MCP 头名称无效。", 400))?,
            HeaderValue::from_str(&value)
                .map_err(|_| failure("MCP_INVALID_CONFIG", "MCP 认证头无效。", 400))?,
        );
    }
    if server["auth"]["type"] == "bearer-env" {
        let token = std::env::var(server["auth"]["tokenEnv"].as_str().unwrap_or(""))
            .map_err(|_| failure("MCP_CREDENTIAL_MISSING", "MCP 认证未配置。", 401))?;
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| failure("MCP_INVALID_CONFIG", "MCP 认证无效。", 400))?,
        );
    }
    if server["auth"]["type"] == "oauth-authorization-code" {
        let token = super::gateway_mcp_oauth::token(server, ctx).await?;
        headers.insert(reqwest::header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth token 无效。", 401))?);
    }
    if server["auth"]["type"] == "oauth-client-credentials" {
        let auth = &server["auth"];
        let issuer = url::Url::parse(auth["issuer"].as_str().unwrap_or(""))
            .map_err(|_| failure("MCP_INVALID_AUTH", "OAuth issuer 无效。", 400))?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| failure("MCP_HTTP_FAILED", "HTTP 客户端初始化失败。", 500))?;
        let metadata_url = issuer
            .join(".well-known/oauth-authorization-server")
            .map_err(|_| failure("MCP_INVALID_AUTH", "OAuth metadata 地址无效。", 400))?;
        let metadata: Value = client
            .get(metadata_url)
            .send()
            .await
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth discovery 失败。", 401))?
            .error_for_status()
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth discovery 被拒绝。", 401))?
            .json()
            .await
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth metadata 无效。", 401))?;
        let endpoint = url::Url::parse(metadata["token_endpoint"].as_str().unwrap_or(""))
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth token endpoint 无效。", 401))?;
        if endpoint.origin() != issuer.origin() {
            return Err(failure(
                "MCP_AUTH_ORIGIN_BLOCKED",
                "OAuth 认证地址不能跨已配置 issuer 的源。",
                403,
            ));
        }
        let secret = std::env::var(auth["clientSecretEnv"].as_str().unwrap_or(""))
            .map_err(|_| failure("MCP_CREDENTIAL_MISSING", "OAuth secret 未配置。", 401))?;
        let mut form = HashMap::new();
        form.insert("grant_type", "client_credentials");
        form.insert("scope", auth["scope"].as_str().unwrap_or(""));
        let token: Value = client
            .post(endpoint)
            .basic_auth(auth["clientId"].as_str().unwrap_or(""), Some(secret))
            .form(&form)
            .send()
            .await
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth token 请求失败。", 401))?
            .error_for_status()
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth token 请求被拒绝。", 401))?
            .json()
            .await
            .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth token 响应无效。", 401))?;
        let token = token["access_token"]
            .as_str()
            .ok_or_else(|| failure("MCP_AUTH_FAILED", "OAuth 响应没有 access token。", 401))?;
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| failure("MCP_AUTH_FAILED", "OAuth token 无效。", 401))?,
        );
    }
    Ok(headers)
}

fn sse_frames(buffer: &mut String) -> Vec<(String, String)> {
    // Normalize after chunks are joined, so a CR/LF split cannot hide the event boundary.
    // 合并网络块后再统一换行，避免 CR/LF 跨包时无法识别事件边界。
    if buffer.contains('\r') {
        *buffer = buffer.replace("\r\n", "\n");
    }
    let mut frames = Vec::new();
    while let Some(end) = buffer.find("\n\n") {
        let frame = buffer[..end].to_owned();
        buffer.drain(..end + 2);
        let mut event = String::new();
        let mut data = Vec::new();
        for line in frame.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                event = value.trim().to_owned();
            }
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.trim_start().to_owned());
            }
        }
        if !data.is_empty() {
            frames.push((event, data.join("\n")));
        }
    }
    frames
}

/// One bounded incremental decoder is shared by both MCP SSE transports.
/// 两种 MCP SSE 传输共用同一有界增量解码器，保留跨分包 UTF-8 与 CRLF 状态。
#[derive(Default)]
struct SseBuffer {
    bytes: Vec<u8>,
    text: String,
    previous_cr: bool,
}
impl SseBuffer {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<(String, String)>, GatewayError> {
        if chunk.len() > MAX_RPC_BYTES.saturating_sub(self.bytes.len() + self.text.len()) {
            return Err(failure(
                "MCP_RESULT_TOO_LARGE",
                "MCP 响应超过传输上限。",
                413,
            ));
        }
        self.bytes.extend_from_slice(chunk);
        let decoded_bytes = match std::str::from_utf8(&self.bytes) {
            Ok(_) => self.bytes.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => {
                return Err(failure(
                    "MCP_INVALID_RESPONSE",
                    "MCP SSE 不是有效 UTF-8。",
                    502,
                ));
            }
        };
        let decoded =
            std::str::from_utf8(&self.bytes[..decoded_bytes]).expect("validated UTF-8 prefix");
        for character in decoded.chars() {
            if self.previous_cr && character == '\n' {
                self.previous_cr = false;
                continue;
            }
            self.previous_cr = character == '\r';
            self.text
                .push(if self.previous_cr { '\n' } else { character });
        }
        self.bytes.drain(..decoded_bytes);
        Ok(sse_frames(&mut self.text))
    }
}

async fn response_rpc(response: reqwest::Response, id: u64) -> Result<Value, GatewayError> {
    let response = check_http_status(response, "MCP_HTTP_REJECTED", "MCP 服务拒绝了请求")?;
    let is_sse = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut sse = SseBuffer::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 响应传输中断。", 502))?;
        if is_sse {
            for (_, frame) in sse.push(&chunk)? {
                if let Ok(value) = serde_json::from_str::<Value>(&frame) {
                    if value["id"] == id {
                        return Ok(value);
                    }
                }
            }
        } else {
            if chunk.len() > MAX_RPC_BYTES.saturating_sub(bytes.len()) {
                return Err(failure(
                    "MCP_RESULT_TOO_LARGE",
                    "MCP 响应超过传输上限。",
                    413,
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    if !is_sse {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| failure("MCP_INVALID_RESPONSE", "MCP 返回了无效 JSON。", 502))?;
        if value["id"] == id {
            return Ok(value);
        }
    }
    Err(failure(
        "MCP_RESPONSE_MISSING",
        "MCP 没有返回匹配的请求回执。",
        502,
    ))
}

fn check_http_status(
    response: reqwest::Response,
    code: &str,
    message: &str,
) -> Result<reqwest::Response, GatewayError> {
    if response.status().is_success() {
        return Ok(response);
    }
    Err(failure(
        code,
        &format!("{message}（HTTP {}）。", response.status().as_u16()),
        response.status().as_u16(),
    )
    .with_retry_after(retry_after_ms(response.headers()))
    .with_details(json!({"failureOrigin":"provider-http","httpStatus":response.status().as_u16()})))
}

/// Inspect bounded protocol fields only; error prose is not evidence of account balance or quota.
/// 仅检查有界协议字段；错误正文不能作为余额或配额判断依据。
pub(crate) fn provider_failure_details(value: &Value, origin: &str) -> Value {
    let candidates = [
        &value["structuredContent"]["error"],
        &value["structuredContent"],
        &value["data"]["error"],
        &value["data"],
        &value["error"],
        value,
    ];
    let mut details = json!({"failureOrigin":origin});
    if let Some(status) = candidates.iter().find_map(|candidate| {
        ["httpStatus", "statusCode", "status"].iter().find_map(|key| {
            candidate[*key].as_u64().filter(|status| (400..=599).contains(status))
        })
    }) {
        details["httpStatus"] = json!(status);
    }
    if origin == "provider-rpc" {
        if let Some(code) = value["code"].as_i64() {
            details["rpcCode"] = json!(code);
        }
    }
    if let Some(code) = candidates.iter().find_map(|candidate| {
        ["code", "type"].iter().find_map(|key| {
            let code = &candidate[*key];
            if code.as_str().is_some_and(|code| !code.is_empty() && code.len() <= 128
                && code.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))) {
                Some(code.clone())
            } else if code.is_i64() && !(origin == "provider-rpc" && std::ptr::eq(*candidate, value)) {
                Some(code.clone())
            } else {
                None
            }
        })
    }) {
        details["providerCode"] = code;
    }
    if let Some(milliseconds) = candidates.iter().find_map(|candidate| candidate["retryAfterMs"].as_u64()) {
        details["retryAfterMs"] = json!(milliseconds);
    }
    details
}

fn rpc_failure(value: &Value) -> GatewayError {
    let details = provider_failure_details(value, "provider-rpc");
    let status = details["httpStatus"].as_u64().map(|status| status as u16)
        .unwrap_or(if details["rpcCode"] == -32602 { 400 } else { 424 });
    failure("MCP_RPC_ERROR", "MCP 服务返回 JSON-RPC 失败回执。", status)
        .with_retry_after(details["retryAfterMs"].as_u64())
        .with_details(details)
}

fn failure_diagnostic(error: &GatewayError) -> Value {
    let mut value = error.response();
    value["message"] = json!(error.message);
    value["gatewayStatus"] = json!(error.status);
    value
}

/// Preserve seconds/date cooldowns without retaining remote bodies, URLs or credentials.
/// 保留秒数或日期形式的冷却提示，不留存远端正文、URL 或凭据。
pub(crate) fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    let milliseconds = if let Ok(seconds) = value.parse::<u64>() {
        seconds.saturating_mul(1000)
    } else {
        let deadline = chrono::DateTime::parse_from_rfc2822(value).ok()?;
        let now = headers
            .get(reqwest::header::DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| chrono::DateTime::parse_from_rfc2822(value).ok())
            .map(|value| value.timestamp_millis())
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        deadline.timestamp_millis().saturating_sub(now).max(0) as u64
    };
    // Preserve the upstream interval; callers compare elapsed time instead of adding an overflowing Instant.
    // 保留远端真实冷却；调用端比较经过时间，不对 Instant 做可能溢出的日期加法。
    Some(milliseconds)
}

impl McpClient {
    async fn connect(server: &Value, ctx: &GatewayToolContext) -> Result<Self, GatewayError> {
        check_cancelled(ctx)?;
        let protocol = server["protocolVersion"].as_str().unwrap_or(PROTOCOL);
        if !super::service_mcp::supports_protocol(protocol) {
            return Err(failure("MCP_UNSUPPORTED_PROTOCOL", "所选 MCP 协议尚未实现，请选择兼容的 2025-11-25 服务。", 409));
        }
        let external_node = if server["transport"] == "stdio" {
            validate_runtime(server)?
        } else {
            false
        };
        let transport = if server["transport"] == "stdio" {
            let executable = server["command"].as_str().unwrap_or("");
            let args = server["args"].as_array().cloned().unwrap_or_default();
            if executable.is_empty() {
                return Err(failure(
                    "MCP_INVALID_CONFIG",
                    "MCP 没有配置可执行文件。",
                    400,
                ));
            }
            let mut command = Command::new(executable);
            hide(&mut command);
            command.env_clear();
            for field in [
                "PATH",
                "SystemRoot",
                "WINDIR",
                "TEMP",
                "TMP",
                "USERPROFILE",
                "LOCALAPPDATA",
                "APPDATA",
                "PROGRAMFILES",
                "PROGRAMFILES(X86)",
                "COMSPEC",
                "PATHEXT",
            ] {
                if let Some(value) = std::env::var_os(field) {
                    command.env(field, value);
                }
            }
            for (field, value) in server["env"].as_object().into_iter().flatten() {
                command.env(field, value.as_str().unwrap_or(""));
            }
            for (field, reference) in server["envRefs"].as_object().into_iter().flatten() {
                command.env(
                    field,
                    std::env::var(reference.as_str().unwrap_or(""))
                        .map_err(|_| failure("MCP_ENV_MISSING", "MCP 所需环境变量未配置。", 400))?,
                );
            }
            command
                .args(args.iter().filter_map(Value::as_str))
                .current_dir(
                    server["cwd"]
                        .as_str()
                        .map(Path::new)
                        .unwrap_or(&ctx.workdir),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = command
                .spawn()
                .map_err(|_| failure("MCP_COMMAND_NOT_FOUND", "MCP 本机可执行文件不可用。", 503))?;
            let input = child.stdin.take().unwrap();
            let owned = match OwnedProcessJob::attach(&child) {
                Ok(job) => job,
                Err(error) => {
                    terminate(&mut child).await;
                    return Err(error);
                }
            };
            let output = BufReader::new(child.stdout.take().unwrap());
            let mut errors = BufReader::new(child.stderr.take().unwrap());
            let stderr = tokio::spawn(async move {
                while let Ok(Some(line)) = bounded_line(&mut errors, 65536).await {
                    if line.len() > MAX_RPC_BYTES {
                        break;
                    }
                }
            });
            Transport::Stdio {
                child,
                input,
                output,
                stderr,
                _owned: owned,
            }
        } else {
            let url = url::Url::parse(server["url"].as_str().unwrap_or(""))
                .map_err(|_| failure("MCP_INVALID_URL", "MCP 远程地址无效。", 400))?;
            if !["http", "https"].contains(&url.scheme())
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(failure(
                    "MCP_INVALID_URL",
                    "MCP 地址协议或认证字段无效。",
                    400,
                ));
            }
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_millis(
                    server["timeoutMs"].as_u64().unwrap_or(DEFAULT_MCP_EXECUTION_MS),
                ))
                .build()
                .map_err(|_| failure("MCP_HTTP_FAILED", "MCP HTTP 客户端初始化失败。", 500))?;
            let headers = headers(server, ctx).await?;
            if server["transport"] == "sse" {
                let response = client
                    .get(url.clone())
                    .headers(headers.clone())
                    .send()
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_FAILED", "MCP SSE 无法连接。", 503))?;
                let response =
                    check_http_status(response, "MCP_CONNECTION_FAILED", "MCP SSE 连接被拒绝")?;
                let (sender, mut receiver) = mpsc::channel(128);
                let origin = url.origin();
                let base = url.clone();
                let reader = tokio::spawn(async move {
                    let mut stream = response.bytes_stream();
                    let mut buffer = SseBuffer::default();
                    while let Some(Ok(chunk)) = stream.next().await {
                        let Ok(frames) = buffer.push(&chunk) else {
                            break;
                        };
                        for (event, data) in frames {
                            let value = if event == "endpoint" {
                                json!({"endpoint":data})
                            } else {
                                serde_json::from_str(&data).unwrap_or(Value::Null)
                            };
                            if sender.send(value).await.is_err() {
                                return;
                            }
                        }
                    }
                });
                // A cancelled or failed handshake must also retire its not-yet-owned SSE reader.
                // 握手取消或失败时也释放尚未转交连接所有权的 SSE 读取任务。
                struct PendingReader(Option<tokio::task::AbortHandle>);
                impl Drop for PendingReader {
                    fn drop(&mut self) {
                        if let Some(reader) = self.0.take() {
                            reader.abort();
                        }
                    }
                }
                let mut pending_reader = PendingReader(Some(reader.abort_handle()));
                let endpoint = tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        let value = receiver.recv().await.ok_or_else(|| {
                            failure("MCP_CONNECTION_CLOSED", "SSE 连接已关闭。", 502)
                        })?;
                        if let Some(endpoint) = value["endpoint"].as_str() {
                            break base.join(endpoint).map_err(|_| {
                                failure("MCP_INVALID_ENDPOINT", "SSE endpoint 无效。", 502)
                            });
                        }
                    }
                })
                .await
                .map_err(|_| failure("MCP_START_TIMEOUT", "MCP SSE endpoint 等待超时。", 504))??;
                if endpoint.origin() != origin {
                    return Err(failure(
                        "MCP_ENDPOINT_ORIGIN_BLOCKED",
                        "MCP SSE endpoint 不能跨配置源。",
                        403,
                    ));
                }
                pending_reader.0.take();
                Transport::Sse {
                    client,
                    endpoint,
                    headers,
                    receiver,
                    reader,
                }
            } else {
                Transport::Http {
                    client,
                    url,
                    headers,
                    session: None,
                }
            }
        };
        let (close_tx, close) = tokio::sync::watch::channel(false);
        ctx.runtime
            .mcp_aborters
            .lock()
            .map_err(|_| failure("TOOL_STATE_POISONED", "MCP 状态锁不可用。", 500))?
            .insert(server["id"].as_str().unwrap_or("").to_owned(), close_tx);
        let mut client = Self {
            transport,
            sequence: Arc::new(AtomicU64::new(0)),
            protocol: protocol.to_owned(),
            tools: Vec::new(),
            configuration: server.to_string(),
            close,
            resources: false,
            diagnostic_id: 0,
            last_used: tokio::time::Instant::now(),
            subscriptions: HashSet::new(),
            external_node,
            background_task: server["backgroundTask"] == true,
            memory_observation: json!({"measured":false,"reason":"not-yet-sampled"}),
            tasks: Value::Null,
            parallel_active: Arc::new(AtomicUsize::new(0)),
            detached_read: false,
        };
        let result=client.rpc("initialize",json!({"protocolVersion":client.protocol,"capabilities":{"tasks":{}},"clientInfo":{"name":"KYNXA","version":env!("CARGO_PKG_VERSION")}}),ctx,server["startupTimeoutMs"].as_u64().unwrap_or(30_000)).await?;
        client.protocol = result["protocolVersion"]
            .as_str()
            .ok_or_else(|| failure("MCP_INVALID_INITIALIZE", "MCP 初始化没有协议版本。", 502))?
            .to_owned();
        if !super::service_mcp::supports_protocol(&client.protocol) {
            return Err(failure("MCP_UNSUPPORTED_PROTOCOL", "MCP 服务返回了尚未实现的协议版本，未继续发送不兼容请求。", 502));
        }
        let supports_resources = result["capabilities"]["resources"].is_object();
        client.tasks = if client.protocol.as_str() >= "2025-11-25" { result["capabilities"]["tasks"].clone() } else { Value::Null };
        client.resources = supports_resources;
        client
            .notify("notifications/initialized", json!({}), ctx)
            .await?;
        let mut cursor = Value::Null;
        for _ in 0..if result["capabilities"]["tools"].is_object() { 32 } else { 0 } {
            let params = if cursor.is_null() {
                json!({})
            } else {
                json!({"cursor":cursor})
            };
            let result = client.rpc("tools/list", params, ctx, 30_000).await?;
            for tool in result["tools"].as_array().into_iter().flatten() {
                if tool["name"].is_string() && tool["inputSchema"].is_object() {
                    client.tools.push(tool.clone());
                }
            }
            cursor = result["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
            if client.tools.len() > 4096 {
                return Err(failure(
                    "MCP_CATALOG_TOO_LARGE",
                    "MCP 目录超过服务接纳限额。",
                    413,
                ));
            }
        }
        if !cursor.is_null() {
            return Err(failure(
                "MCP_CATALOG_TOO_LARGE",
                "MCP 目录尚有未覆盖分页，未伪装为完整目录。",
                413,
            ));
        }
        if supports_resources {
            for (name, operation, schema) in [
                (
                    "kynxa_resources_list",
                    "resources/list",
                    json!({"type":"object","properties":{"cursor":{"type":"string","maxLength":2000}},"additionalProperties":false}),
                ),
                (
                    "kynxa_resources_templates_list",
                    "resources/templates/list",
                    json!({"type":"object","properties":{"cursor":{"type":"string","maxLength":2000}},"additionalProperties":false}),
                ),
                (
                    "kynxa_resources_read",
                    "resources/read",
                    json!({"type":"object","properties":{"uri":{"type":"string","minLength":1,"maxLength":8192}},"required":["uri"],"additionalProperties":false}),
                ),
            ] {
                if !client.tools.iter().any(|tool| tool["name"] == name) {
                    client.tools.push(json!({"name":name,"description":operation,"inputSchema":schema,"operation":operation}));
                }
            }
        }
        Ok(client)
    }

    async fn refresh_auth(&mut self, ctx: &GatewayToolContext) -> Result<(), GatewayError> {
        let server: Value = serde_json::from_str(&self.configuration)
            .map_err(|_| failure("MCP_INVALID_CONFIG", "MCP 连接配置不可读。", 500))?;
        if server["auth"]["type"] == "oauth-authorization-code" {
            let fresh = headers(&server, ctx).await?;
            match &mut self.transport {
                Transport::Http { headers, .. } | Transport::Sse { headers, .. } => *headers = fresh,
                _ => {}
            }
        }
        Ok(())
    }

    async fn notify(&mut self, method: &str, params: Value, ctx: &GatewayToolContext) -> Result<(), GatewayError> {
        self.refresh_auth(ctx).await?;
        let request = json!({"jsonrpc":"2.0","method":method,"params":params});
        match &mut self.transport {
            Transport::Stdio { input, .. } => {
                input
                    .write_all(format!("{request}\n").as_bytes())
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 通知发送失败。", 502))?;
                input
                    .flush()
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 通知刷新失败。", 502))?;
            }
            Transport::Http {
                client,
                url,
                headers,
                session,
            } => {
                let mut request = client
                    .post(url.clone())
                    .headers(headers.clone())
                    .header("MCP-Protocol-Version", &self.protocol)
                    .json(&request);
                if let Some(session) = session {
                    request = request.header("Mcp-Session-Id", session.as_str());
                }
                let response = request
                    .send()
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 通知发送失败。", 502))?;
                check_http_status(response, "MCP_NOTIFICATION_REJECTED", "MCP 通知被服务拒绝")?;
            }
            Transport::Sse {
                client,
                endpoint,
                headers,
                ..
            } => {
                let response = client
                    .post(endpoint.clone())
                    .headers(headers.clone())
                    .json(&request)
                    .send()
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 通知发送失败。", 502))?;
                check_http_status(response, "MCP_NOTIFICATION_REJECTED", "MCP 通知被服务拒绝")?;
            }
        }
        Ok(())
    }

    pub(crate) async fn rpc(
        &mut self,
        method: &str,
        params: Value,
        ctx: &GatewayToolContext,
        timeout_ms: u64,
    ) -> Result<Value, GatewayError> {
        self.refresh_auth(ctx).await?;
        self.last_used = tokio::time::Instant::now();
        let subscription_uri = params["uri"].as_str().map(str::to_owned);
        let id = self.sequence.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let mut cancellation = ctx.cancellation.clone();
        let mut close = self.close.clone();
        let operation = async {
            let envelope = match &mut self.transport {
                Transport::Stdio { input, output, .. } => {
                    input
                        .write_all(format!("{request}\n").as_bytes())
                        .await
                        .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 请求发送失败。", 502))?;
                    input
                        .flush()
                        .await
                        .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 请求刷新失败。", 502))?;
                    loop {
                        let line = bounded_line(output, MAX_RPC_BYTES)
                            .await
                            .map_err(|error| {
                                failure(
                                    if error.kind() == std::io::ErrorKind::InvalidData {
                                        "MCP_INVALID_RESPONSE"
                                    } else {
                                        "MCP_CONNECTION_LOST"
                                    },
                                    "MCP 输出读取失败或超出帧边界。",
                                    502,
                                )
                            })?
                            .ok_or_else(|| {
                                failure("MCP_CONNECTION_CLOSED", "MCP 子进程已关闭。", 502)
                            })?;
                        if line.len() > MAX_RPC_BYTES {
                            return Err(failure("MCP_RESULT_TOO_LARGE", "MCP 回执过大。", 413));
                        }
                        let value: Value = serde_json::from_str(&line).map_err(|_| {
                            failure("MCP_INVALID_RESPONSE", "MCP 输出不是有效 JSON-RPC。", 502)
                        })?;
                        if value["id"] == id {
                            break value;
                        }
                        if value["method"].is_string() && !value["id"].is_null() {
                            let answer = json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Client callback capability is not advertised"}});
                            input
                                .write_all(format!("{answer}\n").as_bytes())
                                .await
                                .map_err(|_| {
                                    failure("MCP_CONNECTION_LOST", "MCP 回调拒绝无法发送。", 502)
                                })?;
                        }
                    }
                }
                Transport::Http {
                    client,
                    url,
                    headers,
                    session,
                } => {
                    let mut outgoing = client
                        .post(url.clone())
                        .headers(headers.clone())
                        .header("MCP-Protocol-Version", &self.protocol)
                        .json(&request);
                    if let Some(session) = session.as_ref() {
                        outgoing = outgoing.header("Mcp-Session-Id", session);
                    }
                    let response = outgoing
                        .send()
                        .await
                        .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 远程请求失败。", 502))?;
                    if let Some(value) = response
                        .headers()
                        .get("Mcp-Session-Id")
                        .and_then(|value| value.to_str().ok())
                    {
                        *session = Some(value.to_owned());
                    }
                    response_rpc(response, id).await?
                }
                Transport::Sse {
                    client,
                    endpoint,
                    headers,
                    receiver,
                    ..
                } => {
                    let response = client
                        .post(endpoint.clone())
                        .headers(headers.clone())
                        .json(&request)
                        .send()
                        .await
                        .map_err(|_| {
                            failure("MCP_CONNECTION_LOST", "MCP SSE 请求发送失败。", 502)
                        })?;
                    check_http_status(response, "MCP_REQUEST_REJECTED", "MCP SSE 请求被拒绝")?;
                    loop {
                        let value = receiver.recv().await.ok_or_else(|| {
                            failure("MCP_CONNECTION_CLOSED", "MCP SSE 已关闭。", 502)
                        })?;
                        if value["id"] == id {
                            break value;
                        }
                    }
                }
            };
            if !envelope["error"].is_null() {
                return Err(rpc_failure(&envelope["error"]));
            }
            envelope
                .get("result")
                .cloned()
                .ok_or_else(|| failure("MCP_INVALID_RESPONSE", "MCP 回执没有 result。", 502))
        };
        let result = tokio::select! {
            result=tokio::time::timeout(Duration::from_millis(timeout_ms),operation)=>result.map_err(|_|failure("MCP_TIMEOUT","MCP 等待超时；已发送的副作用调用不得盲目重放。",504))?,
            _=async {loop {if *cancellation.borrow_and_update(){break;}if cancellation.changed().await.is_err(){std::future::pending::<()>().await;}}}=>Err(failure("TOOL_CANCELLED","MCP 调用已取消，已发送操作需核验真实状态。",409)),
            _=async {loop {if *close.borrow_and_update(){break;}if close.changed().await.is_err(){std::future::pending::<()>().await;}}}=>Err(failure("MCP_CONNECTION_CLOSED","MCP 连接已关闭，已发送操作需核验真实状态。",409)),
        };
        self.last_used = tokio::time::Instant::now();
        if result.is_ok() {
            if let Some(uri) = subscription_uri {
                if method == "resources/subscribe" {
                    self.subscriptions.insert(uri);
                } else if method == "resources/unsubscribe" {
                    self.subscriptions.remove(&uri);
                }
            }
        }
        result
    }
    async fn close_transport(&mut self) -> Result<(), GatewayError> {
        match &mut self.transport {
            Transport::Stdio {
                child,
                stderr,
                _owned,
                ..
            } => {
                _owned.terminate_and_wait().await?;
                terminate(child).await;
                stderr.abort();
            }
            Transport::Sse { reader, .. } => reader.abort(),
            _ => {}
        }
        Ok(())
    }

    /// Streamable HTTP supports independent request/response exchanges in the established session.
    /// Streamable HTTP 在既有会话中复用独立交换；stdio/SSE 保持原有顺序队列。
    fn independent_http_read(&self, descriptor: &Value, args: &Value) -> Option<Self> {
        if descriptor["annotations"]["readOnlyHint"] != true || descriptor["annotations"]["destructiveHint"] == true
            || ["tabId","pageId","sessionId","browserId"].iter().any(|key|args.get(key).is_some())
            || !["url","path","uri","filePath"].iter().any(|key|args[*key].as_str().is_some_and(|v|!v.trim().is_empty() && v.len()<=8192)) {
            return None;
        }
        let Transport::Http { client,url,headers,session } = &self.transport else { return None; };
        self.parallel_active.fetch_add(1, Ordering::AcqRel);
        Some(Self {
            transport:Transport::Http { client:client.clone(),url:url.clone(),headers:headers.clone(),session:session.clone() },
            sequence:self.sequence.clone(),protocol:self.protocol.clone(),tools:self.tools.clone(),configuration:self.configuration.clone(),
            close:self.close.clone(),resources:self.resources,diagnostic_id:self.diagnostic_id,last_used:tokio::time::Instant::now(),
            subscriptions:HashSet::new(),external_node:false,background_task:self.background_task,memory_observation:self.memory_observation.clone(),tasks:self.tasks.clone(),
            parallel_active:self.parallel_active.clone(),detached_read:true,
        })
    }
}

pub(crate) async fn connection(
    server: &Value,
    ctx: &GatewayToolContext,
) -> Result<Arc<Mutex<McpClient>>, GatewayError> {
    connection_with_priority(server, ctx, false, None).await
}

pub(crate) fn transient_connection_failure(code: &str) -> bool {
    matches!(code, "MCP_TIMEOUT" | "MCP_START_TIMEOUT" | "MCP_CONNECTION_FAILED"
        | "MCP_CONNECTION_LOST" | "MCP_CONNECTION_CLOSED" | "MCP_CLEANUP_PENDING")
}

/// Retry bounded search reads under current configuration, generation and resource rules; honor explicit stops.
/// 按当前配置、代次和资源规则有界重试搜索读取；保留用户停止，不重放调用。
pub(crate) async fn prepare_read_retry(server: &Value, ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    super::gateway::check_configuration_generation(ctx)?;
    let id = server["id"].as_str().unwrap_or("");
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    let generation = invocation_generation(ctx);
    lifecycle.check(id, generation)?;
    check_cancelled(ctx)?;
    let gate = lifecycle.gate(id).await;
    let _singleflight = tokio::select! {guard=gate.lock_owned()=>guard,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
    lifecycle.check(id, generation)?;
    let cached = ctx.runtime.mcp.lock().await.get(id).cloned();
    if let Some(client) = cached {
        let closed = {
            let client = tokio::select! {client=client.lock()=>client,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
            let closed = *client.close.borrow();
            closed
        };
        if closed { disconnect(id, ctx).await?; }
    }
    let configuration = server.to_string();
    let mut failures = lifecycle.failures.lock().await;
    // The caller already observed its short cooldown; auth, quota and a different configuration keep their failures.
    // 调用方已遵守短冷却；认证、额度以及其他配置的失败记录保留，不通过重试旁路清除。
    if failures.get(id).is_some_and(|(saved, _, error)| saved == &configuration && transient_connection_failure(&error.code)) {
        failures.remove(id);
    }
    lifecycle.check(id, generation)
}

async fn lifecycle_cancelled(ctx: &GatewayToolContext, id: &str, generation: u64) -> GatewayError {
    loop {
        if let Err(error) =
            check_cancelled(ctx).and_then(|_| ctx.runtime.mcp_lifecycle.check(id, generation))
        {
            return error;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
struct AdmissionCancellation {
    signal: tokio::sync::watch::Sender<bool>,
    monitor: tokio::task::JoinHandle<()>,
}
impl Drop for AdmissionCancellation {
    fn drop(&mut self) {
        let _ = self.signal.send(true);
        self.monitor.abort();
    }
}

/// Admission covers temporary gateway buffers, not an already observed external model/process residency.
/// 准入只覆盖网关的临时缓冲与 CPU，不再次扣除已观测的外部进程常驻占用。
async fn admit_buffers(
    ctx: &GatewayToolContext,
    id: &str,
    generation: u64,
    background: bool,
) -> Result<Option<crate::resources::gateway::ResourceLease>, GatewayError> {
    let Some(service) = crate::resources::gateway::global() else {
        return Ok(None);
    };
    check_cancelled(ctx)?;
    ctx.runtime.mcp_lifecycle.check(id, generation)?;
    let (signal, cancellation) = tokio::sync::watch::channel(false);
    let watcher = signal.clone();
    let context = ctx.clone();
    let server_id = id.to_owned();
    let monitor = tokio::spawn(async move {
        let _ = lifecycle_cancelled(&context, &server_id, generation).await;
        let _ = watcher.send(true);
    });
    let _cancellation = AdmissionCancellation { signal, monitor };
    let workspace = ctx.project_id.as_deref().unwrap_or(&ctx.conversation_id);
    let request = json!({"taskId":uuid::Uuid::new_v4().to_string(),"workspaceId":if workspace.is_empty(){"mcp"}else{workspace},
        "kind":if background{"background"}else{"foreground"},"workload":"compute","cpuThreads":1,
        "memoryBytes":MAX_RPC_BYTES,"gpuMemoryBytes":0,"waitMs":if background{5000}else{30000},"ttlMs":30000});
    // After caller cancellation, settle any late admission grant before releasing its owner.
    // 调用方取消后仍结算晚到授额，再释放准入任务。
    let attempt = tokio::spawn(async move { service.acquire_waiting(request, cancellation).await });
    let lease = attempt
        .await
        .map_err(|_| failure("RESOURCE_RUNTIME_EXITED", "MCP 资源准入任务退出。", 503))??;
    check_cancelled(ctx)?;
    ctx.runtime.mcp_lifecycle.check(id, generation)?;
    Ok(Some(lease))
}
pub(crate) fn configuration_generation(ctx: &GatewayToolContext) -> u64 {
    ctx.runtime.mcp_lifecycle.generation.load(Ordering::Acquire)
}

/// Saved definitions support discovery without startup; execution reconnects and checks the live schema.
/// 已存定义供发现使用，不启动服务；执行时重新连接并核验实际定义。
pub(crate) fn reusable_catalog(ctx: &GatewayToolContext) -> Vec<Value> {
    let generation = configuration_generation(ctx);
    let mut candidates = ctx.runtime.schemas.try_read().map(|schemas| schemas.clone()).unwrap_or_default();
    candidates.extend(super::gateway_tool_memory::cached_catalog(ctx));
    let clients = ctx.runtime.mcp.try_lock().ok();
    let mut reusable = Vec::new();
    let mut names = HashSet::new();
    for tool in candidates.iter().filter(|tool| super::gateway::executable_descriptor(tool)) {
        let Some(name) = tool["name"].as_str().filter(|name| name.starts_with("mcp.")) else { continue; };
        let Ok(server) = authorized_server(name, ctx) else { continue; };
        let id = server["id"].as_str().unwrap_or("");
        if server["trusted"] == false
            || server["transport"] == "stdio" && server["trusted"] != true
            || ctx.runtime.mcp_lifecycle.check(id, generation).is_err()
        {
            continue;
        }
        let fingerprint = super::gateway_tool_memory::server_fingerprint(&server);
        let persisted = tool["configurationFingerprint"] == fingerprint;
        let live = clients.as_ref().and_then(|clients| clients.get(id)).and_then(|client| client.try_lock().ok());
        let reusable_definition = if let Some(client) = live {
            client.configuration == server.to_string() && !*client.close.borrow()
                && client.tools.iter().any(|actual| actual["name"] == tool["toolName"]
                    && actual["inputSchema"] == tool["originalInputSchema"])
        } else {
            persisted
        };
        if reusable_definition && names.insert(name.to_owned()) {
            reusable.push(tool.clone());
        }
    }
    if configuration_generation(ctx) != generation { reusable.clear(); }
    reusable
}

fn invocation_generation(ctx: &GatewayToolContext) -> u64 {
    ctx.config["mcpConfigurationGeneration"]
        .as_u64()
        .unwrap_or_else(|| configuration_generation(ctx))
}

async fn connection_with_priority(
    server: &Value,
    ctx: &GatewayToolContext,
    background: bool,
    expected_generation: Option<u64>,
) -> Result<Arc<Mutex<McpClient>>, GatewayError> {
    let id = server["id"].as_str().unwrap_or("");
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    let generation = expected_generation.unwrap_or_else(|| invocation_generation(ctx));
    super::gateway::check_configuration_generation(ctx)?;
    lifecycle.check(id, generation)?;
    if !background {
        lifecycle.foreground_waiters.fetch_add(1, Ordering::AcqRel);
    }
    let mut reservation = PendingStart {
        runtime: ctx.runtime.clone(),
        id: id.to_owned(),
        foreground: !background,
        reserved: false,
    };
    refresh_budget(ctx).await;
    lifecycle.check(id, generation)?;
    // Yield the service gate to foreground work to avoid mutual waiting.
    // 向前台让路时释放服务锁，避免互相等待。
    let gate = lifecycle.gate(id).await;
    let (_singleflight, background_permit) = if background {
        loop {
            lifecycle.check(id, generation)?;
            check_cancelled(ctx)?;
            if lifecycle.foreground_waiters.load(Ordering::Acquire) == 0
                && lifecycle.foreground_active.load(Ordering::Acquire) == 0
            {
                if let Some(permit) = lifecycle.starts.try_acquire_owned() {
                    // Return capacity when the service gate is busy; its foreground owner may need that capacity.
                    // 服务锁繁忙时立即归还额度，持锁的前台可能正等待该额度。
                    if let Ok(guard) = gate.clone().try_lock_owned() {
                        break (guard, Some(permit));
                    }
                    drop(permit);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    } else {
        let guard = tokio::select! { guard=gate.lock_owned()=>guard,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error) };
        (guard, None)
    };
    lifecycle.check(id, generation)?;
    let configuration = server.to_string();
    let existing = { ctx.runtime.mcp.lock().await.get(id).cloned() };
    if let Some(client) = existing {
        let state = tokio::select! {state=client.lock()=>state,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
        let compatible = state.configuration == configuration;
        let closed = *state.close.borrow();
        drop(state);
        if closed {
            return Err(failure(
                "MCP_CLEANUP_PENDING",
                "MCP 正在关闭，尚不能重新派发。",
                409,
            ));
        }
        if compatible {
            return Ok(client);
        }
        disconnect(id, ctx).await?;
    }
    if let Some((saved, when, error)) = lifecycle.failures.lock().await.get(id) {
        if saved == &configuration
            && when.elapsed() < Duration::from_millis(error.retry_after_ms.unwrap_or(2000))
        {
            return Err(error.clone());
        }
    }
    {
        let clients = ctx.runtime.mcp.lock().await;
        let mut pending = lifecycle
            .pending
            .lock()
            .map_err(|_| failure("TOOL_STATE_POISONED", "MCP 启动额度不可用。", 500))?;
        if clients.len() + pending.len() >= 32 {
            return Err(failure(
                "MCP_CONNECTION_CAPACITY",
                "MCP 连接及启动预约达到32个；未启动额外进程。",
                429,
            ).with_details(json!({"failureOrigin":"local-capacity"})));
        }
        pending.insert(id.to_owned());
        reservation.reserved = true;
    }
    let _permit = if let Some(permit) = background_permit {
        permit
    } else {
        tokio::select! {permit=lifecycle.starts.acquire_owned()=>permit,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)}
    };
    lifecycle.check(id, generation)?;
    let _buffers = admit_buffers(ctx, id, generation, background).await?;
    // Runtime diagnostics use the existing native ledger; settings reads never initiate a handshake.
    // 连接诊断复用原生账本；读取设置不能为展示状态而发起新握手。
    let diagnostic_id = native(
        ctx,
        "mcp.diagnosis.begin",
        json!({"serverId":id,"transport":server["transport"]}),
    )
    .await?
    .as_u64()
    .unwrap_or(0);
    let connected_result = tokio::select! {result=McpClient::connect(server,ctx)=>result,error=lifecycle_cancelled(ctx,id,generation)=>Err(error)};
    let mut connected = match connected_result {
        Ok(client) => client,
        Err(error) => {
            let _ = native(
                ctx,
                "mcp.diagnosis.startupFailure",
                json!({"stateId":diagnostic_id,"serverId":id,"code":error.code}),
            )
            .await;
            lifecycle.failures.lock().await.insert(
                id.to_owned(),
                (configuration, tokio::time::Instant::now(), error.clone()),
            );
            if let Ok(mut aborters) = ctx.runtime.mcp_aborters.lock() {
                if let Some(close) = aborters.remove(id) {
                    let _ = close.send(true);
                }
            }
            return Err(error);
        }
    };
    if let Err(error) = lifecycle.check(id, generation) {
        connected.close_transport().await?;
        return Err(error);
    }
    connected.diagnostic_id = diagnostic_id;
    native(ctx, "mcp.diagnosis.connected", json!({"stateId":diagnostic_id,"serverId":id,"toolCount":connected.tools.len(),"resources":connected.resources,"connectedAt":chrono::Utc::now().to_rfc3339()})).await?;
    if let Err(error) = lifecycle.check(id, generation) {
        connected.close_transport().await?;
        return Err(error);
    }
    let client = Arc::new(Mutex::new(connected));
    let mut clients = ctx.runtime.mcp.lock().await;
    if let Err(error) = lifecycle.check(id, generation) {
        drop(clients);
        client.lock().await.close_transport().await?;
        return Err(error);
    }
    clients.insert(id.to_owned(), client.clone());
    drop(clients);
    lifecycle.failures.lock().await.remove(id);
    ctx.runtime.mcp_errors.lock().await.remove(id);
    start_maintenance(ctx);
    Ok(client)
}

fn start_maintenance(ctx: &GatewayToolContext) {
    let Ok(mut task) = ctx.runtime.mcp_lifecycle.maintenance.lock() else {
        return;
    };
    if task.as_ref().is_some_and(|task| !task.is_finished()) {
        return;
    }
    let runtime = Arc::downgrade(&ctx.runtime);
    *task = Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            if runtime.mcp_lifecycle.closing.load(Ordering::Acquire) {
                break;
            }
            let (_keep, cancellation) = tokio::sync::watch::channel(false);
            // Maintenance retains neither a request's replay buffers nor its permissions/configuration.
            // 维护不保留请求重放缓冲、权限或配置，仅通过既有 runtime 诊断和拥有的连接释放资源。
            let ctx = GatewayToolContext {
                data_home: Default::default(),
                extension_root: Default::default(),
                workdir: Default::default(),
                toolhost_exe: Default::default(),
                permission_mode: "ask".into(),
                conversation_id: String::new(),
                project_id: None,
                authorized_roots: Vec::new(),
                cancellation,
                runtime,
                config: json!({}),
                approved: false,
                events: None,
                external_execute: None,
            };
            refresh_budget(&ctx).await;
            let timeout = Duration::from_millis(ctx.runtime.mcp_lifecycle.budget().idle_timeout_ms);
            reap_idle(&ctx, timeout).await;
        }
    }));
}
async fn reap_idle(ctx: &GatewayToolContext, timeout: Duration) {
    let budget = ctx.runtime.mcp_lifecycle.budget();
    let clients = ctx
        .runtime
        .mcp
        .lock()
        .await
        .iter()
        .map(|(id, client)| (id.clone(), client.clone()))
        .collect::<Vec<_>>();
    let mut eligible = Vec::new();
    for (id, client) in clients {
        if Arc::strong_count(&client) > 2 {
            continue;
        }
        let Ok(mut state) = client.try_lock() else {
            continue;
        };
        if state.background_task || !state.subscriptions.is_empty() || state.parallel_active.load(Ordering::Acquire)>0 {
            continue;
        }
        let (used, owned) = (
            state.last_used,
            matches!(state.transport, Transport::Stdio { .. }),
        );
        let memory = match &state.transport {
            Transport::Stdio { _owned, .. } => match _owned.memory_usage() {
                Ok(sample) => {
                    json!({"measured":true,"privateBytes":sample.private_bytes,"workingSetBytes":sample.working_set_bytes,
                    "processes":sample.processes,"sampledAt":chrono::Utc::now().to_rfc3339(),"reason":"owned-process-tree-point-sample"})
                }
                Err(error) => {
                    json!({"measured":false,"reason":error.code,"sampledAt":chrono::Utc::now().to_rfc3339()})
                }
            },
            _ => json!({"measured":false,"reason":"remote-process-not-owned"}),
        };
        state.memory_observation = memory.clone();
        drop(state);
        eligible.push((id, client, used, owned, memory));
    }
    eligible.sort_by_key(|entry| entry.2);
    let mut private = eligible
        .iter()
        .filter_map(|entry| entry.4["privateBytes"].as_u64())
        .fold(0u64, u64::saturating_add);
    let mut working_set = eligible
        .iter()
        .filter_map(|entry| entry.4["workingSetBytes"].as_u64())
        .fold(0u64, u64::saturating_add);
    let mut processes = eligible
        .iter()
        .filter_map(|entry| entry.4["processes"].as_u64())
        .sum::<u64>() as usize;
    let mut unknown = eligible
        .iter()
        .filter(|entry| entry.3 && entry.4["measured"] != true)
        .count();
    for (id, client, used, owned, memory) in eligible {
        let memory_pressure =
            owned && (private > budget.idle_memory_bytes || budget.idle_memory_bytes == 0);
        if used.elapsed() < timeout && !memory_pressure {
            continue;
        }
        let gate = ctx.runtime.mcp_lifecycle.gate(&id).await;
        let Ok(_gate) = gate.try_lock() else {
            continue;
        };
        if Arc::strong_count(&client) > 2 {
            continue;
        }
        let Ok(mut state) = client.try_lock() else {
            continue;
        };
        if state.background_task || !state.subscriptions.is_empty() || state.parallel_active.load(Ordering::Acquire)>0 {
            continue;
        }
        if state.last_used != used || (state.last_used.elapsed() < timeout && !memory_pressure) {
            continue;
        }
        let actual = ctx
            .runtime
            .mcp
            .lock()
            .await
            .get(&id)
            .is_some_and(|active| Arc::ptr_eq(active, &client));
        if !actual {
            continue;
        }
        match state.close_transport().await {
            Ok(()) => {
                let state_id = state.diagnostic_id;
                drop(state);
                let mut clients = ctx.runtime.mcp.lock().await;
                if clients
                    .get(&id)
                    .is_some_and(|active| Arc::ptr_eq(active, &client))
                {
                    clients.remove(&id);
                }
                drop(clients);
                if let Ok(mut aborters) = ctx.runtime.mcp_aborters.lock() {
                    if let Some(cancel) = aborters.remove(&id) {
                        let _ = cancel.send(true);
                    }
                }
                ctx.runtime
                    .schemas
                    .write()
                    .await
                    .retain(|tool| tool["serverId"] != id);
                let _=native(ctx,"mcp.diagnosis.disconnected",json!({"serverId":id,"stateId":state_id,"unexpected":false,"reason":"idle-reclaimed"})).await;
                private = private.saturating_sub(memory["privateBytes"].as_u64().unwrap_or(0));
                working_set =
                    working_set.saturating_sub(memory["workingSetBytes"].as_u64().unwrap_or(0));
                processes =
                    processes.saturating_sub(memory["processes"].as_u64().unwrap_or(0) as usize);
                if owned && memory["measured"] != true {
                    unknown = unknown.saturating_sub(1);
                }
            }
            Err(error) => {
                ctx.runtime
                    .mcp_errors
                    .lock()
                    .await
                    .insert(id, failure_diagnostic(&error));
            }
        }
    }
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    lifecycle
        .idle_private_bytes
        .store(private, Ordering::Release);
    lifecycle
        .idle_working_set_bytes
        .store(working_set, Ordering::Release);
    lifecycle.idle_processes.store(processes, Ordering::Release);
    lifecycle
        .idle_unknown_services
        .store(unknown, Ordering::Release);
    lifecycle.idle_sampled_at.store(
        chrono::Utc::now().timestamp_millis().max(0) as u64,
        Ordering::Release,
    );
}

/// Warm trusted installed services within observed capacity; do not install dependencies or inherit chat authority.
/// 按观测额度预热受信任的已安装服务；不安装依赖或继承聊天授权。
pub(crate) fn prewarm(ctx: &GatewayToolContext) -> Value {
    // Bind queued configuration before spawning; a reset cannot lend new authority to an old task.
    // 排队前绑定配置代次；重置后的新代次不能为旧预热任务重新赋予启动权限。
    let generation = ctx.runtime.mcp_lifecycle.generation.load(Ordering::Acquire);
    if ctx
        .runtime
        .mcp_lifecycle
        .prewarmed
        .swap(true, Ordering::AcqRel)
    {
        return json!({"scheduled":0,"state":"already-scheduled"});
    }
    let mut selected = Vec::new();
    let mut skipped = Vec::new();
    for server in servers(ctx)
        .into_iter()
        .filter(|server| server["transport"] == "stdio")
    {
        let id = server["id"].as_str().unwrap_or("");
        if let Err(error) = ctx.runtime.mcp_lifecycle.check(id, generation) {
            skipped.push(json!({"serverId":id,"code":error.code}));
            continue;
        }
        if server["trusted"] != true {
            skipped.push(json!({"serverId":id,"code":"MCP_NOT_TRUSTED"}));
            continue;
        }
        // Browser transports may spawn Chromium during handshake; discover them only for an actual foreground request.
        // 浏览器服务握手时可能启动 Chromium；后台预热跳过，实际使用时仍走原来的按需连接与复用。
        if super::service_broker::connection_facts(&server)
            .is_ok_and(|(_, facts)| facts["connection"].is_object())
        {
            skipped.push(json!({"serverId":id,"code":"MCP_BROWSER_ON_DEMAND"}));
            continue;
        }
        if server["envRefs"]
            .as_object()
            .into_iter()
            .flatten()
            .any(|(_, reference)| {
                reference
                    .as_str()
                    .is_none_or(|name| std::env::var_os(name).is_none_or(|value| value.is_empty()))
            })
        {
            skipped.push(json!({"serverId":id,"code":"MCP_ENV_MISSING"}));
            continue;
        }
        if server["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|arg| {
                arg.contains("<absolute-path") || (arg.starts_with('<') && arg.ends_with('>'))
            })
        {
            skipped.push(json!({"serverId":id,"code":"MCP_CONFIGURATION_REQUIRED"}));
            continue;
        }
        if server["cwd"]
            .as_str()
            .is_none_or(|cwd| !installed_path(Path::new(cwd), true))
        {
            skipped.push(json!({"serverId":id,"code":"MCP_PREWARM_CWD_MISSING"}));
            continue;
        }
        match validate_runtime(&server) {
            Ok(true) => selected.push(server),
            Ok(false)
                if server["command"].as_str().is_some_and(|command| {
                    let path = Path::new(command);
                    installed_path(path, false)
                        && ![
                            "uvx.exe",
                            "uv.exe",
                            "pip.exe",
                            "pip3.exe",
                            "cmd.exe",
                            "powershell.exe",
                            "pwsh.exe",
                        ]
                        .contains(
                            &path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("")
                                .to_ascii_lowercase()
                                .as_str(),
                        )
                }) =>
            {
                selected.push(server)
            }
            Ok(false) => {
                skipped.push(json!({"serverId":id,"code":"MCP_PREWARM_INSTALLED_ENTRY_REQUIRED"}))
            }
            Err(error) => skipped.push(json!({"serverId":id,"code":error.code})),
        }
    }
    selected.sort_by(|left, right| {
        right["prewarmPriority"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&left["prewarmPriority"].as_u64().unwrap_or(0))
            .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
    });
    selected.truncate(32);
    let scheduled = selected.len();
    let mut background = ctx.clone();
    background.events = None;
    background.external_execute = None;
    background.approved = false;
    background.authorized_roots.clear();
    tokio::spawn(async move {
        // Configuration requests may finish before prewarming; service stop/generation still cancels this work.
        // 配置请求结束不等于用户取消预热；停止标记和生命周期代次仍负责取消后台服务准备。
        let (_prewarm_owner, prewarm_cancellation) = tokio::sync::watch::channel(false);
        background.cancellation = prewarm_cancellation;
        refresh_budget(&background).await;
        let budget = background.runtime.mcp_lifecycle.budget();
        selected.truncate(budget.warm_services);
        let mut pending = futures_util::stream::iter(selected.into_iter().map(|server| {
            let mut ctx = background.clone();
            ctx.workdir = Path::new(server["cwd"].as_str().unwrap()).to_path_buf();
            async move {
                if let Err(error) =
                    connection_with_priority(&server, &ctx, true, Some(generation)).await
                {
                    let id = server["id"].as_str().unwrap_or("");
                    let mut errors = ctx.runtime.mcp_errors.lock().await;
                    if ctx.runtime.mcp_lifecycle.check(id, generation).is_ok() {
                        errors.insert(
                            id.to_owned(),
                            failure_diagnostic(&error),
                        );
                    }
                }
                if ctx.runtime.mcp_lifecycle.generation.load(Ordering::Acquire) == generation
                    && !ctx.runtime.mcp_lifecycle.closing.load(Ordering::Acquire)
                {
                    reap_idle(
                        &ctx,
                        Duration::from_millis(ctx.runtime.mcp_lifecycle.budget().idle_timeout_ms),
                    )
                    .await;
                }
            }
        }))
        .buffer_unordered(budget.starts);
        while pending.next().await.is_some() {}
    });
    let mut notice = ctx.runtime.mcp_lifecycle.budget().value();
    notice["scheduled"] = json!(scheduled);
    notice["skipped"] = json!(skipped);
    notice["state"] = json!("background-queued");
    notice["scheduledIsCandidateCount"] = json!(true);
    notice
}

pub(crate) async fn catalog(ctx: &GatewayToolContext) -> Result<Vec<Value>, GatewayError> {
    catalog_selected(ctx, servers(ctx)).await
}

/// Sync the handshake catalog before recording outcomes; keep disk I/O outside the connection lock.
/// 记录调用结果前同步握手目录；磁盘 I/O 放在连接锁外。
async fn remember_actual_catalog(
    server: &Value,
    actual: &[Value],
    generation: u64,
    ctx: &GatewayToolContext,
) -> Result<Vec<Value>, GatewayError> {
    let id = server["id"].as_str().unwrap_or("");
    let fingerprint = super::gateway_tool_memory::server_fingerprint(server);
    let discovered = actual.iter().map(|tool| json!({
        "name":format!("mcp.{id}.{}",tool["name"].as_str().unwrap_or("")),
        "description":tool["description"],"source":format!("mcp:{id}"),"serverId":id,"toolName":tool["name"],
        "configurationFingerprint":fingerprint,"originalInputSchema":tool["inputSchema"],
        "annotations":tool["annotations"],"execution":tool["execution"],
        "inputSchema":{"type":"object","properties":{"arguments":tool["inputSchema"],
            "policy":{"type":"object","properties":{"reason":{"type":"string","minLength":1,"maxLength":2000}}}},
            "required":["arguments","policy"],"additionalProperties":false}
    })).collect::<Vec<_>>();
    let enabled = discovered.iter().filter(|tool| !server["disabledTools"].as_array()
        .is_some_and(|disabled| disabled.contains(&tool["toolName"]))).cloned().collect::<Vec<_>>();
    {
        let mut schemas = ctx.runtime.schemas.write().await;
        super::gateway::check_configuration_generation(ctx)?;
        ctx.runtime.mcp_lifecycle.check(id, generation)?;
        schemas.retain(|tool| tool["serverId"] != id);
        schemas.extend(enabled.iter().cloned());
    }
    super::gateway_tool_memory::remember_catalog(&format!("mcp:{id}"), &fingerprint, discovered, ctx).await;
    Ok(enabled)
}

pub(crate) async fn catalog_selected(
    ctx: &GatewayToolContext,
    servers: Vec<Value>,
) -> Result<Vec<Value>, GatewayError> {
    let generation = invocation_generation(ctx);
    refresh_budget(ctx).await;
    let concurrency = ctx.runtime.mcp_lifecycle.starts.limit();
    let mut all = Vec::new();
    let mut pending = futures_util::stream::iter(servers.into_iter().map(|server| async move {
        let result = connection(&server, ctx).await;
        (server, result)
    }))
    .buffer_unordered(concurrency);
    while let Some((server, result)) = pending.next().await {
        match result {
            Ok(client) => {
                ctx.runtime
                    .mcp_errors
                    .lock()
                    .await
                    .remove(server["id"].as_str().unwrap_or(""));
                let actual = client.lock().await.tools.clone();
                all.extend(remember_actual_catalog(&server, &actual, generation, ctx).await?);
            }
            Err(error) => {
                ctx.runtime.mcp_errors.lock().await.insert(
                    server["id"].as_str().unwrap_or("").to_owned(),
                    failure_diagnostic(&error),
                );
                emit(ctx,json!({"type":"mcp_status","serverId":server["id"],"ready":false,"code":error.code,"message":error.message})).await;
            }
        }
    }
    Ok(all)
}

pub(crate) fn authorized_server(
    name: &str,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let (id, tool) = name
        .strip_prefix("mcp.")
        .and_then(|suffix| suffix.split_once('.'))
        .ok_or_else(|| failure("MCP_TOOL_NOT_FOUND", "MCP 工具名无效。", 404))?;
    let server = servers(ctx)
        .into_iter()
        .find(|server| server["id"] == id)
        .ok_or_else(|| failure("MCP_SERVER_NOT_FOUND", "MCP 服务未启用或配置已改变。", 404))?;
    if server["disabledTools"]
        .as_array()
        .is_some_and(|disabled| disabled.iter().any(|entry| entry == tool))
    {
        return Err(failure("MCP_TOOL_DISABLED", "MCP 工具已被用户禁用。", 403));
    }
    Ok(server)
}

pub(crate) fn builtin_task(name: &str) -> bool {
    ["mcp.task.status","mcp.task.result","mcp.task.cancel"].contains(&name)
}

pub(crate) fn task_descriptors() -> Vec<Value> {
    [("mcp.task.status","Read the current state of a previously created MCP task using its original archived resultRef. Does not restart the tool; working/input_required is not completion."),
        ("mcp.task.result","Retrieve the actual final result of an existing MCP task through its original archived resultRef. Never repeat the original tools/call. A task status alone does not prove result quality."),
        ("mcp.task.cancel","Explicitly cancel an existing remote MCP task using its original archived resultRef. Requires the server's advertised cancellation capability; preserves the original operation and its known/unknown outcome.")]
        .into_iter().map(|(name,description)|json!({"name":name,"source":"builtin","description":description,
            "inputSchema":{"type":"object","properties":{"resultRef":{"type":"object","properties":{"id":{"type":"string","minLength":1},"sha256":{"type":"string"},"bytes":{"type":"integer","minimum":0}},"required":["id"],"additionalProperties":false},"reason":{"type":"string","maxLength":2000}},"required":["resultRef"],"additionalProperties":false}})).collect()
}

fn task_handle(server: &Value, task: &Value, name: &str, ctx: &GatewayToolContext) -> Value {
    json!({"kind":"mcp-task","serverId":server["id"],"taskId":task["taskId"],"toolName":name,
        "conversationId":ctx.conversation_id,"requestId":ctx.config["activeRequestId"],"toolCallId":ctx.config["activeToolCallId"],"operationId":ctx.config["activeOperationId"],
        "configurationFingerprint":super::gateway_tool_memory::server_fingerprint(server)})
}

fn unknown_task(handle: &Value, code: &str, cause: Option<&GatewayError>) -> Value {
    json!({"isError":true,"status":"unknown","code":code,"causeCode":cause.map(|error|&error.code),"recoveryHandle":handle,
        "content":[{"type":"text","text":"The remote task was accepted. Query its original task ID; never repeat the original action. / 远端任务已接受，必须查询原任务，不能重新执行原动作。"}]})
}

fn checked_task(value: Value, id: Option<&str>) -> Result<Value,GatewayError> {
    let task_id = value["taskId"].as_str().filter(|id|!id.is_empty() && id.len()<=1024)
        .ok_or_else(||failure("MCP_INVALID_TASK","MCP 任务回执缺少真实任务身份。",502))?;
    if id.is_some_and(|id|id!=task_id) || !matches!(value["status"].as_str(),Some("working"|"input_required"|"completed"|"failed"|"cancelled")) {
        return Err(failure("MCP_INVALID_TASK","MCP 任务身份或状态无效，未推断完成。",502));
    }
    Ok(value)
}

/// Recognize protocol errors and Blender's exact execution-failure wrapper, not arbitrary logs.
/// 识别协议错误及 Blender 明确的执行失败包装，不扫描普通日志中的错误单词。
pub(crate) fn explicit_tool_error(name: &str, result: &Value) -> bool {
    if result["isError"] == true { return true; }
    let blender_execution = name.strip_prefix("mcp.").and_then(|suffix| suffix.split_once('.'))
        .is_some_and(|(_, tool)| tool == "execute_blender_code");
    if !blender_execution { return false; }
    let failed = |text: &str| text.trim_start().starts_with("Error executing code:");
    result["content"].as_array().and_then(|blocks| blocks.first())
        .filter(|block| block["type"] == "text").and_then(|block| block["text"].as_str())
        .is_some_and(failed)
        || result["structuredContent"]["result"].as_str().is_some_and(failed)
}

fn normalize_tool_result(name: &str, mut result: Value) -> Value {
    if explicit_tool_error(name, &result) { result["isError"] = json!(true); }
    result
}

/// Crash recovery queries a journaled remote identity; it never dispatches the original tool again.
/// 崩溃恢复只查询正式事件日志中的远端身份，绝不再次派发原工具。
pub(crate) async fn recover_checkpoint(handle: &Value, original_call_id: &str, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    if handle["kind"] != "mcp-task" || handle["conversationId"] != ctx.conversation_id
        || handle["toolCallId"] != original_call_id || original_call_id.is_empty() {
        return Err(failure("MCP_TASK_SCOPE_INVALID", "远端任务检查点不属于此聊天的原调用。", 403));
    }
    let server = servers(ctx).into_iter().find(|server| server["id"] == handle["serverId"])
        .ok_or_else(|| failure("MCP_SERVER_NOT_FOUND", "原 MCP 服务未启用，已保留任务检查点。", 409))?;
    if super::gateway_tool_memory::server_fingerprint(&server) != handle["configurationFingerprint"] {
        return Err(failure("MCP_TASK_CONFIG_CHANGED", "服务配置已改变，未查询其他来源。", 409));
    }
    let id = handle["taskId"].as_str().filter(|id|!id.is_empty() && id.len()<=1024)
        .ok_or_else(|| failure("MCP_INVALID_TASK", "检查点没有有效远端任务 ID。", 409))?;
    let task = checked_task(task_rpc(&server, "tasks/get", id, ctx).await?, Some(id))?;
    if !matches!(task["status"].as_str(), Some("completed"|"failed"|"cancelled")) {
        return Ok(json!({"status":"waiting","remoteTask":task,"recoveryHandle":handle}));
    }
    let mut result = task_rpc(&server,"tasks/result",id,ctx).await?;
    if !result.is_object() { return Err(failure("MCP_INVALID_TASK_RESULT","远端任务结果无效，未判定完成。",502)); }
    if task["status"] != "completed" { result["isError"] = json!(true); }
    result["remoteTask"] = task;
    result["recoveryHandle"] = handle.clone();
    let result = normalize_tool_result(handle["toolName"].as_str().unwrap_or(""), result);
    Ok(json!({"status":"settled","result":result}))
}

async fn task_rpc(server: &Value, method: &str, task_id: &str, ctx: &GatewayToolContext) -> Result<Value,GatewayError> {
    for attempt in 0..2 {
        let generation = invocation_generation(ctx);
        let id = server["id"].as_str().unwrap_or("");
        let result=async {
            let client = connection_with_priority(server,ctx,false,Some(generation)).await?;
            let _permit = tokio::select! {permit=ctx.runtime.mcp_lifecycle.calls.acquire_owned()=>permit,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
            let _buffers=admit_buffers(ctx,id,generation,false).await?;
            let mut client = tokio::select! {client=client.lock()=>client,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
            if !client.tasks.is_object() || method=="tasks/cancel" && !client.tasks["cancel"].is_object() {
                return Err(failure("MCP_TASK_UNSUPPORTED","该服务未协商对应任务协议，不会重做原调用。",409));
            }
            client.rpc(method,json!({"taskId":task_id}),ctx,server["timeoutMs"].as_u64().unwrap_or(DEFAULT_MCP_EXECUTION_MS)).await
        }.await;
        match result {
            Err(error) if attempt==0 && method!="tasks/cancel" && transient_connection_failure(&error.code)=> {
                // Only the identity query may retry; a healthy stdio owner is never killed to retry a task read.
                // 仅身份查询可短重试；不会为了重试读取而杀掉健康的 stdio 任务宿主。
                prepare_read_retry(server,ctx).await?;
                let mut cancellation=ctx.cancellation.clone();
                tokio::select! {_=tokio::time::sleep(Duration::from_millis(100))=>{},_=super::gateway::wait_for_cancellation(&mut cancellation)=>return Err(failure("TOOL_CANCELLED","任务状态查询已取消，远端身份已保留。",409))}
            },
            other=>return other,
        }
    }
    unreachable!("bounded task query retry always returns its second result")
}

async fn poll_task(server: &Value, first: Value, name: &str, ctx: &GatewayToolContext) -> Result<Value,GatewayError> {
    let mut task = match checked_task(first.clone(),None) {
        Ok(task)=>task,
        Err(error)=>return Ok(unknown_task(&task_handle(server,&first,name,ctx),"MCP_EXECUTION_UNKNOWN",Some(&error))),
    };
    let handle = task_handle(server,&task,name,ctx);
    let task_id = task["taskId"].as_str().unwrap().to_owned();
    // The normal run event journal preserves this identity before any deferred-result wait or network failure.
    // 远端身份先进入现有运行事件日志，再等待延迟结果；断线或重启后可据此查询，不能重派动作。
    emit(ctx,json!({"type":"mcp_task_checkpoint","toolCallId":ctx.config["activeToolCallId"],"operationId":ctx.config["activeOperationId"],"recoveryHandle":handle})).await;
    loop {
        if matches!(task["status"].as_str(),Some("completed"|"failed"|"cancelled")) {
            return match task_rpc(server,"tasks/result",&task_id,ctx).await {
                Ok(mut result) => {
                    if !result.is_object(){return Ok(unknown_task(&handle,"MCP_INVALID_TASK_RESULT",None));}
                    if task["status"]!="completed" {result["isError"]=json!(true);}
                    result["remoteTask"] = task;
                    Ok(result)
                },
                Err(error) => Ok(unknown_task(&handle,"MCP_EXECUTION_UNKNOWN",Some(&error))),
            };
        }
        if task["status"]=="input_required" {return Ok(unknown_task(&handle,"MCP_TASK_INPUT_REQUIRED",None));}
        let interval = task["pollInterval"].as_u64().unwrap_or(1000).max(50);
        let Some(deadline) = tokio::time::Instant::now().checked_add(Duration::from_millis(interval)) else {
            return Ok(unknown_task(&handle,"MCP_INVALID_TASK_POLL",None));
        };
        let mut cancellation = ctx.cancellation.clone();
        tokio::select! {
            _=tokio::time::sleep_until(deadline)=>{},
            _=super::gateway::wait_for_cancellation(&mut cancellation)=>return Ok(unknown_task(&handle,"MCP_EXECUTION_UNKNOWN",None)),
        }
        match task_rpc(server,"tasks/get",&task_id,ctx).await.and_then(|value|checked_task(value,Some(&task_id))) {
            Ok(observed) => {task=observed;emit(ctx,json!({"type":"mcp_task_progress","toolCallId":ctx.config["activeToolCallId"],"remoteTaskId":task_id,"status":task["status"]})).await;},
            Err(error)=>return Ok(unknown_task(&handle,"MCP_EXECUTION_UNKNOWN",Some(&error))),
        }
    }
}

pub(crate) async fn execute_task(name:&str,args:Value,ctx:&GatewayToolContext)->Result<Value,GatewayError>{
    let root=ctx.data_home.clone();let conversation=ctx.conversation_id.clone();
    let reference=args["resultRef"].clone();
    let archived=tokio::task::spawn_blocking(move ||crate::storage::gateway::load_private_tool_result(&root,&conversation,reference["id"].as_str().unwrap_or("")))
        .await.map_err(|_|failure("MCP_TASK_ARCHIVE_FAILED","原任务回执读取失败。",503))??;
    if args["resultRef"]["sha256"].as_str().is_some_and(|hash|archived["sha256"]!=hash){return Err(failure("MCP_TASK_ARCHIVE_CHANGED","原任务回执身份已改变。",409));}
    let handle=&archived["canonical"]["recoveryHandle"];
    if handle["kind"]!="mcp-task" || handle["conversationId"]!=ctx.conversation_id || archived["toolCallId"]!=handle["toolCallId"] {
        return Err(failure("MCP_TASK_SCOPE_INVALID","此回执不属于当前聊天的原 MCP 任务。",403));
    }
    let server=servers(ctx).into_iter().find(|server|server["id"]==handle["serverId"])
        .ok_or_else(||failure("MCP_SERVER_NOT_FOUND","原 MCP 服务尚未启用。",409))?;
    if super::gateway_tool_memory::server_fingerprint(&server)!=handle["configurationFingerprint"] {return Err(failure("MCP_TASK_CONFIG_CHANGED","原任务的服务配置已变化，未查询其他来源。",409));}
    let id=handle["taskId"].as_str().ok_or_else(||failure("MCP_INVALID_TASK","原任务身份无效。",409))?;
    if name=="mcp.task.cancel" {return task_rpc(&server,"tasks/cancel",id,ctx).await.and_then(|value|checked_task(value,Some(id)));}
    let task=checked_task(task_rpc(&server,"tasks/get",id,ctx).await?,Some(id))?;
    if name=="mcp.task.status" {return Ok(json!({"remoteTask":task,"recoveryHandle":handle}));}
    if !matches!(task["status"].as_str(),Some("completed"|"failed"|"cancelled")){return Ok(unknown_task(handle,"MCP_TASK_RUNNING",None));}
    let mut result=task_rpc(&server,"tasks/result",id,ctx).await?;
    if !result.is_object(){return Err(failure("MCP_INVALID_TASK_RESULT","原任务结果格式无效，未标记成功。",502));}
    if task["status"]!="completed" {result["isError"]=json!(true);}
    result["_mcpTaskProof"]=json!({"serverId":handle["serverId"],"taskId":id,"toolCallId":handle["toolCallId"],"configurationFingerprint":handle["configurationFingerprint"],"terminalStatus":task["status"]});
    Ok(normalize_tool_result(handle["toolName"].as_str().unwrap_or(""), result))
}

pub(crate) async fn execute(
    name: &str,
    args: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let suffix = name
        .strip_prefix("mcp.")
        .ok_or_else(|| failure("MCP_TOOL_NOT_FOUND", "MCP 工具名无效。", 404))?;
    let (id, tool) = suffix
        .split_once('.')
        .ok_or_else(|| failure("MCP_TOOL_NOT_FOUND", "MCP 工具名无效。", 404))?;
    let server = authorized_server(name, ctx)?;
    let generation = invocation_generation(ctx);
    let client = connection_with_priority(&server, ctx, false, Some(generation)).await?;
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    let actual = {
        let connected = tokio::select! {client=client.lock()=>client,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
        connected.tools.clone()
    };
    remember_actual_catalog(&server, &actual, generation, ctx).await?;
    // Keep one exchange per connection, while independent services share a dynamically resized quota.
    // 单连接保持一次完整协议交换，不同服务共用动态可收缩的调用额度；排队取消尚未派发副作用。
    let descriptor = actual.iter().find(|entry| entry["name"] == tool).ok_or_else(|| failure(
            "MCP_TOOL_NOT_FOUND",
            "MCP 工具不存在于实际服务目录。",
            404,
        ))?;
    let arguments = args.get("arguments").cloned().unwrap_or(args);
    super::gateway::validate_schema(&descriptor["inputSchema"], &arguments, name)?;
    let operation = descriptor["operation"].as_str().unwrap_or("tools/call");
    let mut params = if operation == "tools/call" {
        json!({"name":tool,"arguments":arguments})
    } else {
        arguments
    };
    let mut client = tokio::select! {client=client.lock()=>client,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
    let _permit = tokio::select! {permit=lifecycle.calls.acquire_owned()=>permit,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
    let _buffers = admit_buffers(ctx, id, generation, false).await?;
    super::gateway::check_configuration_generation(ctx)?;
    lifecycle.check(id, generation)?;
    if *client.close.borrow() {
        return Err(failure(
            "MCP_CONNECTION_CLOSED",
            "MCP 连接已关闭，未派发操作。",
            409,
        ));
    }
    let task_supported=operation=="tools/call" && client.tasks["requests"]["tools"]["call"].is_object()
        && matches!(descriptor["execution"]["taskSupport"].as_str(),Some("optional"|"required"));
    if operation=="tools/call" && descriptor["execution"]["taskSupport"]=="required" && !task_supported {
        return Err(failure("MCP_TASK_UNSUPPORTED","该工具要求服务尚未协商的任务协议，未派发。",409));
    }
    if task_supported {params["task"]=json!({});}
    let independent = if operation == "tools/call" { client.independent_http_read(descriptor, &params["arguments"]) } else { None };
    let was_independent=independent.is_some();
    let timeout = server["timeoutMs"].as_u64().unwrap_or(DEFAULT_MCP_EXECUTION_MS);
    let result = if let Some(mut independent) = independent {
        client.last_used = tokio::time::Instant::now();
        drop(client);
        independent.rpc(operation, params, ctx, timeout).await
    } else {
        let result = client.rpc(operation,params,ctx,timeout).await;
        drop(client);
        result
    };
    drop(_permit);
    drop(_buffers);
    if was_independent {
        let cached=ctx.runtime.mcp.lock().await.get(id).cloned();
        if let Some(cached)=cached { if let Ok(mut canonical)=cached.try_lock() {
            if canonical.configuration==server.to_string() {canonical.last_used=tokio::time::Instant::now();}
        } }
    }
    match result {
        Ok(value) => {
            ctx.runtime.mcp_errors.lock().await.remove(id);
            let value = if task_supported { poll_task(&server,value["task"].clone(),name,ctx).await? } else { value };
            Ok(normalize_tool_result(name, value))
        }
        Err(error)
            if [
                "MCP_TIMEOUT",
                "MCP_CONNECTION_LOST",
                "MCP_CONNECTION_CLOSED",
                "TOOL_CANCELLED",
                "MCP_RESULT_TOO_LARGE",
                "MCP_INVALID_RESPONSE",
                "MCP_RESPONSE_MISSING",
            ]
            .contains(&error.code.as_str()) =>
        {
            ctx.runtime.mcp_errors.lock().await.insert(
                id.to_owned(),
                failure_diagnostic(&error),
            );
            // A failed independent HTTP read must not cancel other healthy exchanges sharing this session.
            // 独立 HTTP 读取失败不能连带取消同一会话中其它健康交换。
            if !was_independent {let _ = disconnect(id, ctx).await;}
            Ok(
                json!({"isError":true,"status":"unknown","code":"MCP_EXECUTION_UNKNOWN","causeCode":error.code,"content":[{"type":"text","text":error.message}],"executionEnvironment":{"executorKind":if server["transport"]=="stdio"{"mcp-stdio"}else{"mcp-http"},"executorLocation":if server["transport"]=="stdio"{"gateway-host"}else{"remote-service"}}}),
            )
        }
        Err(error) => Err(error),
    }
}

pub async fn shutdown(ctx: &GatewayToolContext) {
    let _ = shutdown_checked(ctx).await;
}

pub(crate) async fn shutdown_checked(ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    ctx.runtime
        .mcp_lifecycle
        .closing
        .store(true, Ordering::Release);
    ctx.runtime
        .mcp_lifecycle
        .generation
        .fetch_add(1, Ordering::AcqRel);
    let maintenance = ctx
        .runtime
        .mcp_lifecycle
        .maintenance
        .lock()
        .ok()
        .and_then(|mut task| task.take());
    if let Some(task) = maintenance {
        task.abort();
        let _ = task.await;
    }
    if let Ok(mut aborters) = ctx.runtime.mcp_aborters.lock() {
        for (_, cancel) in aborters.drain() {
            let _ = cancel.send(true);
        }
    }
    let ids = ctx
        .runtime
        .mcp
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut cleanup_error = None;
    for id in ids {
        if let Err(error) = disconnect(&id, ctx).await {
            cleanup_error.get_or_insert(error);
        }
    }
    // Closing invalidates queued starts; wait for their owners to release reservations before moving roots.
    // 关闭使排队启动失效；迁移根目录前等待所有者实际释放启动预约。
    loop {
        let pending = ctx
            .runtime
            .mcp_lifecycle
            .pending
            .lock()
            .map_err(|_| failure("TOOL_STATE_POISONED", "MCP 启动状态不可用。", 500))?
            .is_empty();
        if pending {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if let Some(error) = cleanup_error {
        return Err(error);
    }
    Ok(())
}

pub(crate) async fn reset(ctx: &GatewayToolContext) {
    shutdown(ctx).await;
    ctx.runtime.mcp_lifecycle.failures.lock().await.clear();
    ctx.runtime
        .mcp_lifecycle
        .prewarmed
        .store(false, Ordering::Release);
    ctx.runtime
        .mcp_lifecycle
        .closing
        .store(false, Ordering::Release);
}
pub(crate) async fn stop(id: &str, ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    ctx.runtime
        .mcp_lifecycle
        .stopped
        .lock()
        .map_err(|_| failure("TOOL_STATE_POISONED", "MCP 停止状态不可用。", 500))?
        .insert(id.to_owned());
    disconnect(id, ctx).await
}
pub(crate) async fn restart(
    server: &Value,
    ctx: &GatewayToolContext,
) -> Result<Arc<Mutex<McpClient>>, GatewayError> {
    let id = server["id"].as_str().unwrap_or("");
    disconnect(id, ctx).await?;
    ctx.runtime
        .mcp_lifecycle
        .stopped
        .lock()
        .map_err(|_| failure("TOOL_STATE_POISONED", "MCP 停止状态不可用。", 500))?
        .remove(id);
    ctx.runtime.mcp_lifecycle.failures.lock().await.remove(id);
    connection(server, ctx).await
}
pub(crate) async fn disconnect(id: &str, ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    if let Ok(mut aborters) = ctx.runtime.mcp_aborters.lock() {
        if let Some(cancel) = aborters.remove(id) {
            let _ = cancel.send(true);
        }
    }
    let client = ctx.runtime.mcp.lock().await.get(id).cloned();
    if let Some(client) = client {
        let mut state = client.lock().await;
        if let Err(error) = state.close_transport().await {
            ctx.runtime.mcp_errors.lock().await.insert(
                id.to_owned(),
                failure_diagnostic(&error),
            );
            return Err(error);
        }
        let state_id = state.diagnostic_id;
        drop(state);
        let mut clients = ctx.runtime.mcp.lock().await;
        if clients
            .get(id)
            .is_some_and(|active| Arc::ptr_eq(active, &client))
        {
            clients.remove(id);
        }
        drop(clients);
        let unexpected = ctx.runtime.mcp_errors.lock().await.contains_key(id);
        let _ = native(
            ctx,
            "mcp.diagnosis.disconnected",
            json!({"serverId":id,"stateId":state_id,"unexpected":unexpected}),
        )
        .await;
    }
    ctx.runtime
        .schemas
        .write()
        .await
        .retain(|tool| tool["serverId"] != id);
    Ok(())
}

pub(crate) async fn diagnostics(ctx: &GatewayToolContext) -> Vec<Value> {
    let connected = ctx
        .runtime
        .mcp
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let errors = ctx.runtime.mcp_errors.lock().await.clone();
    let snapshots = native(ctx, "mcp.diagnosis.snapshot", json!({}))
        .await
        .unwrap_or_else(|_| json!([]));
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    let budget = lifecycle.budget().value();
    let clients = ctx.runtime.mcp.lock().await.clone();
    let mut memory = HashMap::new();
    for (id, client) in clients {
        if let Ok(state) = client.try_lock() {
            memory.insert(id, (state.memory_observation.clone(), state.external_node));
        }
    }
    servers(ctx).into_iter().map(|server| {
        let id = server["id"].as_str().unwrap_or("");
        let ready = connected.iter().any(|value| value == id);
        let mut state = snapshots.as_array().into_iter().flatten().find(|value| value["serverId"] == id).cloned().unwrap_or_else(||json!({"serverId":id,"transport":server["transport"],"state":"disconnected","toolCount":0,"resourceCapabilities":{"resources":false,"templates":false}}));
        if !ready && state["state"] == "ready" { state["state"] = json!("disconnected"); state["toolCount"] = json!(0); }
        if let Some(error) = errors.get(id) { state["code"] = error["code"].clone(); state["state"] = json!(if ["MCP_AUTH_REQUIRED","MCP_OAUTH_LOGIN_REQUIRED","MCP_CREDENTIAL_MISSING","MCP_OAUTH_CREDENTIAL_LOCKED"].iter().any(|code| error["code"] == *code) {"auth-required"} else {"error"}); }
        state["name"] = server["name"].clone();
        state["enabled"] = json!(server["enabled"] != false);
        state["ready"] = json!(ready && state["state"] == "ready");
        if ctx.runtime.mcp_lifecycle.stopped.lock().is_ok_and(|stopped|stopped.contains(id)) {
            state["ready"]=json!(false);state["state"]=json!("stopped");state["code"]=json!("MCP_STOPPED");
        }
        if let Some(fields)=budget.as_object() {for (name,value)in fields {state[name]=value.clone();}}
        state["activeStarts"]=json!(lifecycle.starts.active());
        state["activeCalls"]=json!(lifecycle.calls.active());
        let sampled_at=lifecycle.idle_sampled_at.load(Ordering::Acquire);
        state["idleCacheMemory"]=json!({"privateBytes":lifecycle.idle_private_bytes.load(Ordering::Acquire),
            "workingSetBytes":lifecycle.idle_working_set_bytes.load(Ordering::Acquire),"processes":lifecycle.idle_processes.load(Ordering::Acquire),
            "unmeasuredOwnedServices":lifecycle.idle_unknown_services.load(Ordering::Acquire),
            "sampledAt":if sampled_at==0 {None}else{Some(sampled_at)},
            "budgetVerified":sampled_at>0 && lifecycle.idle_unknown_services.load(Ordering::Acquire)==0 && lifecycle.idle_private_bytes.load(Ordering::Acquire)<=budget["idleMemoryBudgetBytes"].as_u64().unwrap_or(0),"measurement":"eligible-idle-owned-process-point-samples"});
        let observation=memory.get(id).map(|entry|entry.0.clone()).unwrap_or_else(||json!({"measured":false,"reason":if ready{"connection-busy-or-not-yet-sampled"}else{"not-connected"}}));
        state["processTreeMemoryMeasured"]=observation["measured"].clone();
        state["processTreeMemory"]=observation;
        state["externalNodeRuntime"]=json!(memory.get(id).map(|entry|entry.1));
        state["error"] = json!(errors.get(id));
        state["searchCooldown"] = ctx.runtime.web_search.cooling(&server)
            .map(|error| error.response()).unwrap_or(Value::Null);
        state
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blender_execution_error_wrapper_overrides_false_success_without_rewriting_output() {
        let text = "Error executing code: Could not connect to Blender. Make sure the Blender addon is running.";
        let original = json!({"isError":false,"content":[{"type":"text","text":text}],
            "structuredContent":{"result":text}});
        assert!(explicit_tool_error("mcp.blender.execute_blender_code", &original));
        let normalized = normalize_tool_result("mcp.blender.execute_blender_code", original.clone());
        assert_eq!(normalized["isError"], true);
        assert_eq!(normalized["content"], original["content"]);
        assert_eq!(normalized["structuredContent"], original["structuredContent"]);
        assert_eq!(original["isError"], false);
        assert!(explicit_tool_error("mcp.personal-blender.execute_blender_code",
            &json!({"structuredContent":{"result":text}})));
        assert!(explicit_tool_error("mcp.other.read", &json!({"isError":true})));
    }

    #[test]
    fn ordinary_logs_and_examples_are_not_blender_execution_failures() {
        for text in [
            "Code executed successfully: error_count=0\nError executing code: example output",
            "Example: Error executing code: connection unavailable",
            "```python\nprint('Error executing code: example')\n```",
            "The source contains Error executing code: for its error handler.",
        ] {
            let result = json!({"isError":false,"content":[{"type":"text","text":text}],
                "structuredContent":{"result":text}});
            assert!(!explicit_tool_error("mcp.blender.execute_blender_code", &result));
            assert_eq!(normalize_tool_result("mcp.blender.execute_blender_code", result.clone()), result);
        }
        let unrelated = json!({"content":[{"type":"text","text":"Error executing code: a quoted source"}]});
        assert!(!explicit_tool_error("mcp.blender.get_scene_info", &unrelated));
        assert!(!explicit_tool_error("mcp.documents.read", &unrelated));
        let later_log = json!({"content":[{"type":"text","text":"Code executed successfully: inspected log"},
            {"type":"text","text":"Error executing code: previous example"}]});
        assert!(!explicit_tool_error("mcp.blender.execute_blender_code", &later_log));
    }

    #[tokio::test]
    async fn blender_wrapper_failure_remains_a_tool_result_and_the_same_connection_can_recover() {
        use axum::{Json, Router, extract::State, routing::post};
        async fn handler(State(initializations): State<Arc<AtomicUsize>>, Json(request): Json<Value>) -> Json<Value> {
            let result = match request["method"].as_str().unwrap_or("") {
                "initialize" => {
                    initializations.fetch_add(1, Ordering::AcqRel);
                    json!({"protocolVersion":PROTOCOL,"capabilities":{"tools":{}},"serverInfo":{"name":"blender-fixture","version":"1"}})
                }
                "tools/list" => json!({"tools":[{"name":"execute_blender_code","inputSchema":{"type":"object","properties":{"recover":{"type":"boolean"}}}}]}),
                "tools/call" => {
                    let text = if request["params"]["arguments"]["recover"] == true {
                        "Code executed successfully: CONNECTED; error_count=0"
                    } else { "Error executing code: Could not connect to Blender. Make sure the Blender addon is running." };
                    json!({"isError":false,"content":[{"type":"text","text":text}],"structuredContent":{"result":text}})
                }
                _ => json!({}),
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
        let initializations = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/mcp", post(handler)).with_state(initializations.clone());
        let service = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let (mut ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.config["mcpServers"] = json!([{"id":"blender-fixture","enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp")}]);
        let failed = execute("mcp.blender-fixture.execute_blender_code", json!({"arguments":{"recover":false}}), &ctx).await.unwrap();
        assert_eq!(failed["isError"], true);
        assert!(failed["content"][0]["text"].as_str().unwrap().starts_with("Error executing code:"));
        let recovered = execute("mcp.blender-fixture.execute_blender_code", json!({"arguments":{"recover":true}}), &ctx).await.unwrap();
        assert_eq!(recovered["isError"], false);
        assert_eq!(initializations.load(Ordering::Acquire), 1);
        shutdown(&ctx).await;
        service.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn remote_task_checkpoint_recovers_actual_result_without_replaying_original_mutation() {
        use axum::{Json, Router, extract::State, routing::post};
        #[derive(Clone,Default)] struct StateData { calls:Arc<AtomicUsize>, reads:Arc<AtomicUsize>, fail:Arc<AtomicBool> }
        async fn handler(State(state):State<StateData>,Json(request):Json<Value>)->Json<Value> {
            let result=match request["method"].as_str().unwrap_or("") {
                "initialize"=>json!({"protocolVersion":PROTOCOL,"capabilities":{"tools":{},"tasks":{"requests":{"tools":{"call":{}}},"cancel":{}}},"serverInfo":{"name":"task-fixture","version":"1"}}),
                "tools/list"=>json!({"tools":[{"name":"render","inputSchema":{"type":"object"},"execution":{"taskSupport":"optional"}}]}),
                "tools/call"=> { assert!(request["params"]["task"].is_object());state.calls.fetch_add(1,Ordering::AcqRel);json!({"task":{"taskId":"original-render","status":"working","pollInterval":50}}) },
                "tasks/get"=> {
                    assert_eq!(request["params"]["taskId"],"original-render");
                    if state.fail.load(Ordering::Acquire) {return Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32099,"message":"temporary fixture disconnect"}}));}
                    json!({"taskId":"original-render","status":"completed"})
                },
                "tasks/result"=> {state.reads.fetch_add(1,Ordering::AcqRel);json!({"content":[{"type":"text","text":"actual-render-result"}]})},
                _=>json!({}),
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
        let state=StateData::default();state.fail.store(true,Ordering::Release);
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let app=Router::new().route("/mcp",post(handler)).with_state(state.clone());
        let service=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let (mut ctx,_keep)=super::super::gateway::test_context();let root=ctx.workdir.parent().unwrap().to_owned();
        ctx.config["mcpServers"]=json!([{"id":"task-fixture","enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp")}]);
        ctx.config["activeToolCallId"]=json!("original-call");ctx.config["activeOperationId"]=json!("original-operation");
        let (events,mut observed)=tokio::sync::mpsc::channel(16);ctx.events=Some(events);
        let unknown=execute("mcp.task-fixture.render",json!({"arguments":{}}),&ctx).await.unwrap();
        assert_eq!(unknown["status"],"unknown");assert_eq!(unknown["recoveryHandle"]["taskId"],"original-render");
        let mut checkpoint=None;
        while let Ok(event)=observed.try_recv(){if event["type"]=="mcp_task_checkpoint" {checkpoint=Some(event);}}
        let checkpoint=checkpoint.unwrap();assert_eq!(checkpoint["operationId"],"original-operation");
        state.fail.store(false,Ordering::Release);
        let recovered=recover_checkpoint(&checkpoint["recoveryHandle"],"original-call",&ctx).await.unwrap();
        assert_eq!(recovered["status"],"settled");assert_eq!(recovered["result"]["content"][0]["text"],"actual-render-result");
        assert_eq!(state.calls.load(Ordering::Acquire),1);assert_eq!(state.reads.load(Ordering::Acquire),1);
        assert_eq!(recover_checkpoint(&checkpoint["recoveryHandle"],"other-call",&ctx).await.unwrap_err().code,"MCP_TASK_SCOPE_INVALID");
        shutdown(&ctx).await;service.abort();std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn streamable_http_parallelizes_actual_independent_reads_and_keeps_writes_serial() {
        use axum::{Json,Router,extract::State,routing::post};
        #[derive(Clone,Default)] struct StateData { active:Arc<AtomicUsize>, peak:Arc<AtomicUsize>, ids:Arc<Mutex<HashSet<u64>>> }
        async fn handler(State(state):State<StateData>,Json(request):Json<Value>)->Json<Value> {
            let result=match request["method"].as_str().unwrap_or("") {
                "initialize"=>json!({"protocolVersion":PROTOCOL,"capabilities":{"tools":{}},"serverInfo":{"name":"parallel-fixture","version":"1"}}),
                "tools/list"=>json!({"tools":[{"name":"read","inputSchema":{"type":"object","properties":{"path":{"type":"string"}}},"annotations":{"readOnlyHint":true,"destructiveHint":false}},{"name":"write","inputSchema":{"type":"object","properties":{"path":{"type":"string"}}}}]}),
                "tools/call"=> {
                    assert!(request["params"].get("task").is_none());
                    assert!(state.ids.lock().await.insert(request["id"].as_u64().unwrap()),"Concurrent exchanges must keep distinct RPC identities");
                    let active=state.active.fetch_add(1,Ordering::AcqRel)+1;state.peak.fetch_max(active,Ordering::AcqRel);
                    let path=request["params"]["arguments"]["path"].as_str().unwrap_or("");
                    tokio::time::sleep(Duration::from_millis(if path=="bad"{20}else{80})).await;state.active.fetch_sub(1,Ordering::AcqRel);
                    if path=="bad" {return Json(json!({"jsonrpc":"2.0","id":request["id"],"invalid":true}));}
                    json!({"content":[{"type":"text","text":request["params"]["arguments"]["path"]}]})
                },
                _=>json!({}),
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
        let state=StateData::default();let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let app=Router::new().route("/mcp",post(handler)).with_state(state.clone());let service=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
        let (mut ctx,_keep)=super::super::gateway::test_context();let root=ctx.workdir.parent().unwrap().to_owned();
        ctx.config["mcpServers"]=json!([{"id":"parallel-fixture","enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp")}]);
        let mut budget=ctx.runtime.mcp_lifecycle.budget();budget.calls=4;budget.sampled_at=Some(0);ctx.runtime.mcp_lifecycle.apply_budget(budget);
        *ctx.runtime.mcp_lifecycle.last_refresh.lock().unwrap()=Some(tokio::time::Instant::now());
        let (a,b)=tokio::join!(execute("mcp.parallel-fixture.read",json!({"arguments":{"path":"a.txt"}}),&ctx),execute("mcp.parallel-fixture.read",json!({"arguments":{"path":"b.txt"}}),&ctx));
        assert!(a.is_ok() && b.is_ok());assert_eq!(state.peak.load(Ordering::Acquire),2);
        let actual=ctx.runtime.schemas.read().await.iter().find(|tool|tool["name"]=="mcp.parallel-fixture.read").unwrap().clone();
        assert_eq!(actual["annotations"]["readOnlyHint"],true);
        state.peak.store(0,Ordering::Release);
        let (a,b)=tokio::join!(execute("mcp.parallel-fixture.write",json!({"arguments":{"path":"same.txt"}}),&ctx),execute("mcp.parallel-fixture.write",json!({"arguments":{"path":"same.txt"}}),&ctx));
        assert!(a.is_ok() && b.is_ok());assert_eq!(state.peak.load(Ordering::Acquire),1);
        let (bad,healthy)=tokio::join!(execute("mcp.parallel-fixture.read",json!({"arguments":{"path":"bad"}}),&ctx),execute("mcp.parallel-fixture.read",json!({"arguments":{"path":"healthy"}}),&ctx));
        assert_eq!(bad.unwrap()["code"],"MCP_EXECUTION_UNKNOWN");
        assert_eq!(healthy.unwrap()["content"][0]["text"],"healthy","One failed exchange must not close a healthy concurrent read");
        shutdown(&ctx).await;service.abort();std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rpc_failure_retains_only_safe_machine_metadata_and_default_errors_stay_compatible() {
        let error = rpc_failure(&json!({"code":-32000,"message":"private billing message",
            "data":{"code":"insufficient_quota","httpStatus":402,"retryAfterMs":30000,
                "Authorization":"secret","url":"https://private.example/path"}}));
        assert_eq!(error.status, 402);
        assert_eq!(error.details, json!({"failureOrigin":"provider-rpc","rpcCode":-32000,
            "providerCode":"insufficient_quota","httpStatus":402,"retryAfterMs":30000}));
        assert_eq!(error.retry_after_ms, Some(30000));
        assert_eq!(failure_diagnostic(&error)["details"]["httpStatus"], 402);
        assert_eq!(failure_diagnostic(&error)["retryAfterMs"], 30000);
        assert!(!error.response().to_string().contains("private"));
        let unsafe_code = rpc_failure(&json!({"code":-32000,"data":{"code":"Bearer secret with spaces"}}));
        assert!(unsafe_code.details.get("providerCode").is_none());
        assert_eq!(unsafe_code.status, 424);
        assert_eq!(failure("LEGACY", "message", 500).response(), json!({"code":"LEGACY","error":"message"}));
    }

    #[tokio::test]
    async fn durable_mcp_catalog_survives_runtime_restart_without_connecting() {
        let (mut ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let server = json!({"id":"durable","enabled":true,"trusted":true,"transport":"streamable-http",
            "url":"http://127.0.0.1:9/mcp","disabledTools":[]});
        ctx.config["mcpServers"] = json!([server.clone()]);
        super::super::gateway_tool_memory::synchronize(&ctx).await;
        let schema = json!({"type":"object","properties":{}});
        remember_actual_catalog(&server, &[json!({"name":"echo","inputSchema":schema}),
            json!({"name":"retired","inputSchema":schema})], configuration_generation(&ctx), &ctx).await.unwrap();
        let updated = json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]});
        remember_actual_catalog(&server, &[json!({"name":"echo","inputSchema":updated})],
            configuration_generation(&ctx), &ctx).await.unwrap();
        let current = ctx.runtime.schemas.read().await;
        assert_eq!(current.iter().filter(|tool| tool["serverId"] == "durable").count(), 1,
            "Refreshing a server withdraws its old tool definitions.");
        assert_eq!(current.iter().find(|tool| tool["name"] == "mcp.durable.echo").unwrap()["originalInputSchema"], updated);
        drop(current);
        ctx.runtime = Arc::new(super::super::gateway::ToolRuntime::default());
        super::super::gateway_tool_memory::synchronize(&ctx).await;
        assert!(ctx.runtime.mcp.try_lock().unwrap().is_empty());
        assert_eq!(reusable_catalog(&ctx).len(), 1, "A persisted definition does not require a live MCP connection.");
        ctx.config["mcpServers"][0]["disabledTools"] = json!(["echo"]);
        assert!(reusable_catalog(&ctx).is_empty());
        ctx.config["mcpServers"][0] = server.clone();
        ctx.config["mcpServers"][0]["url"] = json!("http://127.0.0.1:9/new-version");
        assert!(reusable_catalog(&ctx).is_empty());
        ctx.config["mcpServers"][0] = server.clone();
        ctx.config["mcpServers"][0]["trusted"] = json!(false);
        assert!(reusable_catalog(&ctx).is_empty());
        ctx.config["mcpServers"] = json!([]);
        assert!(reusable_catalog(&ctx).is_empty());
        assert!(ctx.runtime.mcp.try_lock().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cached_catalog_reuses_only_current_enabled_connection_definitions() {
        let (mut ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let server = json!({"id":"cached","enabled":true,"transport":"streamable-http",
            "url":"http://127.0.0.1:9/mcp","disabledTools":[]});
        ctx.config["mcpServers"] = json!([server.clone()]);
        let schema = json!({"type":"object","properties":{}});
        let tool = json!({"name":"mcp.cached.echo","toolName":"echo","source":"mcp:cached","serverId":"cached",
            "inputSchema":schema,"originalInputSchema":schema});
        let (owner, close) = tokio::sync::watch::channel(false);
        let client = Arc::new(Mutex::new(McpClient {
            transport:Transport::Http { client:reqwest::Client::new(), url:url::Url::parse("http://127.0.0.1:9/mcp").unwrap(),
                headers:HeaderMap::new(),session:None },
            sequence:Arc::new(AtomicU64::new(0)),protocol:PROTOCOL.into(),tools:vec![json!({"name":"echo","inputSchema":schema})],
            configuration:server.to_string(),close,resources:false,diagnostic_id:0,
            last_used:tokio::time::Instant::now(),subscriptions:HashSet::new(),external_node:false,
            background_task:false,memory_observation:Value::Null,tasks:Value::Null,parallel_active:Arc::new(AtomicUsize::new(0)),detached_read:false,
        }));
        ctx.runtime.mcp.lock().await.insert("cached".into(), client.clone());
        ctx.runtime.schemas.write().await.push(tool);
        assert_eq!(reusable_catalog(&ctx).len(), 1);
        ctx.conversation_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(reusable_catalog(&ctx).len(), 1, "Definitions can cross chats without starting another connection.");
        ctx.config["mcpServers"][0]["disabledTools"] = json!(["echo"]);
        assert!(reusable_catalog(&ctx).is_empty());
        ctx.config["mcpServers"][0] = server.clone();
        ctx.config["mcpServers"][0]["url"] = json!("http://127.0.0.1:9/changed");
        assert!(reusable_catalog(&ctx).is_empty());
        ctx.config["mcpServers"][0] = server.clone();
        ctx.config["mcpServers"][0]["enabled"] = json!(false);
        assert!(reusable_catalog(&ctx).is_empty());
        ctx.config["mcpServers"][0] = server;
        { let _busy = client.lock().await; assert!(reusable_catalog(&ctx).is_empty(), "Busy connections do not delay selection."); }
        ctx.runtime.mcp_lifecycle.stopped.lock().unwrap().insert("cached".into());
        assert!(reusable_catalog(&ctx).is_empty());
        ctx.runtime.mcp_lifecycle.stopped.lock().unwrap().clear();
        owner.send_replace(true);
        assert!(reusable_catalog(&ctx).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn browser_mcp_prewarm_leaves_processes_unstarted_until_requested() {
        let (mut ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.config["mcpServers"] = json!([
            {"id":"playwright","enabled":true,"trusted":true,"transport":"stdio","command":"node.exe","args":["D:/bundled/node_modules/@playwright/mcp/cli.js"],"cwd":ctx.workdir},
            {"id":"chrome-devtools","enabled":true,"trusted":true,"transport":"stdio","command":"node.exe","args":["D:/bundled/node_modules/chrome-devtools-mcp/build/src/index.js"],"cwd":ctx.workdir}
        ]);
        let receipt = prewarm(&ctx);
        assert_eq!(receipt["scheduled"], 0);
        let skipped = receipt["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 2);
        assert!(skipped.iter().all(|service| service["code"] == "MCP_BROWSER_ON_DEMAND"));
        assert!(ctx.runtime.mcp.lock().await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_node_launchers_and_preserves_non_node_stdio() {
        assert!(is_node("C:\\runtime\\node.exe", &[]));
        assert!(is_node("npx", &[]));
        assert!(is_node("cmd", &[json!("server.mjs")]));
        assert!(!is_node("uvx", &[json!("mcp-server-fetch")]));
        assert_eq!(
            validate_runtime(&json!({"command":"npx","args":["some-package"]}))
                .unwrap_err()
                .code,
            "MCP_AUTOMATIC_INSTALL_DISABLED"
        );
        assert_eq!(
            validate_runtime(&json!({"command":"node","args":["server.js"]}))
                .unwrap_err()
                .code,
            "MCP_NODE_DEPENDENCY_MISSING"
        );
        let root = std::env::temp_dir().join(format!(
            "kynxa-installed-node-policy-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("node.exe"), "policy fixture only; never executed").unwrap();
        std::fs::write(root.join("server.js"), "policy fixture only").unwrap();
        let server =
            json!({"command":root.join("node.exe"),"args":[root.join("server.js")],"cwd":root});
        assert_eq!(validate_runtime(&server).unwrap(), true);
        let mut missing = server.clone();
        missing["args"] = json!([root.join("absent.js")]);
        assert_eq!(
            validate_runtime(&missing).unwrap_err().code,
            "MCP_NODE_ENTRY_MISSING"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn lazy_singleflight_limits_starts_and_preserves_busy_or_subscribed_services() {
        use axum::{Json, Router, extract::State, routing::post};
        #[derive(Clone, Default)]
        struct Starts {
            total: Arc<AtomicUsize>,
            active: Arc<AtomicUsize>,
            peak: Arc<AtomicUsize>,
        }
        async fn handler(State(counts): State<Starts>, Json(request): Json<Value>) -> Json<Value> {
            if request["method"] == "initialize" {
                counts.total.fetch_add(1, Ordering::AcqRel);
                let active = counts.active.fetch_add(1, Ordering::AcqRel) + 1;
                counts.peak.fetch_max(active, Ordering::AcqRel);
                tokio::time::sleep(Duration::from_millis(30)).await;
                counts.active.fetch_sub(1, Ordering::AcqRel);
            }
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":fixture_result(&request)}))
        }
        let counts = Starts::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service = Router::new()
            .route("/mcp/{id}", post(handler))
            .with_state(counts.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let (ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        assert!(ctx.runtime.mcp.lock().await.is_empty());
        let mut budget = ctx.runtime.mcp_lifecycle.budget();
        budget.starts = 2;
        budget.sampled_at = Some(0);
        ctx.runtime.mcp_lifecycle.apply_budget(budget);
        *ctx.runtime.mcp_lifecycle.last_refresh.lock().unwrap() = Some(tokio::time::Instant::now());
        let server = |id: &str| json!({"id":id,"name":id,"enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp/{id}")});
        let mut held_gates = Vec::new();
        let mut background_starts = Vec::new();
        for id in ["gate-alpha", "gate-beta"] {
            held_gates.push(ctx.runtime.mcp_lifecycle.gate(id).await.lock_owned().await);
            let server = server(id);
            let context = ctx.clone();
            background_starts.push(tokio::spawn(async move {
                connection_with_priority(&server, &context, true, None).await
            }));
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            ctx.runtime.mcp_lifecycle.starts.available_permits(),
            ctx.runtime.mcp_lifecycle.starts.limit(),
            "Background gate waiters must release startup capacity for foreground work"
        );
        let mut foreground_starts = Vec::new();
        for id in ["gate-alpha", "gate-beta"] {
            let server = server(id);
            let context = ctx.clone();
            foreground_starts.push(tokio::spawn(
                async move { connection(&server, &context).await },
            ));
        }
        tokio::task::yield_now().await;
        assert_eq!(
            ctx.runtime
                .mcp_lifecycle
                .foreground_waiters
                .load(Ordering::Acquire),
            2
        );
        drop(held_gates);
        let joined = tokio::time::timeout(
            Duration::from_secs(3),
            futures_util::future::join_all(foreground_starts),
        )
        .await
        .unwrap();
        assert!(
            joined
                .iter()
                .all(|result| result.as_ref().is_ok_and(Result::is_ok))
        );
        let joined = tokio::time::timeout(
            Duration::from_secs(3),
            futures_util::future::join_all(background_starts),
        )
        .await
        .unwrap();
        assert!(
            joined
                .iter()
                .all(|result| result.as_ref().is_ok_and(Result::is_ok))
        );
        for id in ["gate-alpha", "gate-beta"] {
            disconnect(id, &ctx).await.unwrap();
        }
        counts.total.store(0, Ordering::Release);
        let same = server("same-fixture");
        let shared = futures_util::future::join_all((0..8).map(|_| connection(&same, &ctx))).await;
        assert!(shared.iter().all(Result::is_ok));
        assert_eq!(counts.total.load(Ordering::Acquire), 1);
        drop(shared);
        let peers = [
            "alpha-fixture",
            "beta-fixture",
            "active-fixture",
            "idle-fixture",
        ]
        .map(server);
        let loaded =
            futures_util::future::join_all(peers.iter().map(|server| connection(server, &ctx)))
                .await;
        assert!(loaded.iter().all(Result::is_ok));
        drop(loaded);
        assert!(counts.peak.load(Ordering::Acquire) <= ctx.runtime.mcp_lifecycle.starts.limit());
        assert_eq!(counts.total.load(Ordering::Acquire), 5);
        stop("same-fixture", &ctx).await.unwrap();
        assert_eq!(
            connection(&same, &ctx).await.err().unwrap().code,
            "MCP_STOPPED"
        );
        assert_eq!(counts.total.load(Ordering::Acquire), 5);
        drop(restart(&same, &ctx).await.unwrap());
        assert_eq!(counts.total.load(Ordering::Acquire), 6);
        {
            let clients = ctx.runtime.mcp.lock().await.clone();
            for id in ["same-fixture", "alpha-fixture", "beta-fixture"] {
                clients[id].lock().await.external_node = true;
            }
            clients["same-fixture"].lock().await.last_used =
                tokio::time::Instant::now() - Duration::from_secs(100);
        }
        reap_idle(&ctx, Duration::from_secs(90)).await;
        assert!(!ctx.runtime.mcp.lock().await.contains_key("same-fixture"));
        assert_eq!(ctx.runtime.mcp.lock().await.len(), 4);
        {
            let clients = ctx.runtime.mcp.lock().await.clone();
            for client in clients.values() {
                client.lock().await.last_used =
                    tokio::time::Instant::now() - Duration::from_secs(2);
            }
            clients["alpha-fixture"].lock().await.background_task = true;
            clients["beta-fixture"]
                .lock()
                .await
                .subscriptions
                .insert("fixture://subscribed".into());
        }
        let active = ctx.runtime.mcp.lock().await["active-fixture"].clone();
        let held = active.lock().await;
        reap_idle(&ctx, Duration::from_secs(1)).await;
        let retained = ctx
            .runtime
            .mcp
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        assert_eq!(
            retained,
            HashSet::from([
                "alpha-fixture".into(),
                "beta-fixture".into(),
                "active-fixture".into()
            ])
        );
        drop(held);
        drop(active);
        shutdown(&ctx).await;
        assert!(ctx.runtime.mcp.lock().await.is_empty());
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn real_rpc_queue_shrinks_grows_and_cancels_before_dispatch_without_replay() {
        use axum::{Json, Router, extract::State, routing::post};
        #[derive(Clone)]
        struct Calls {
            entered: tokio::sync::mpsc::UnboundedSender<String>,
            release: Arc<tokio::sync::Semaphore>,
            active: Arc<AtomicUsize>,
            peak: Arc<AtomicUsize>,
        }
        async fn handler(State(calls): State<Calls>, Json(request): Json<Value>) -> Json<Value> {
            if request["method"] == "tools/call" {
                let active = calls.active.fetch_add(1, Ordering::AcqRel) + 1;
                calls.peak.fetch_max(active, Ordering::AcqRel);
                calls
                    .entered
                    .send(
                        request["params"]["arguments"]["text"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                    )
                    .unwrap();
                calls.release.acquire().await.unwrap().forget();
                calls.active.fetch_sub(1, Ordering::AcqRel);
            }
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":fixture_result(&request)}))
        }
        let (entered, mut received) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let counts = Calls {
            entered,
            release: release.clone(),
            active: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/mcp", post(handler))
            .with_state(counts.clone());
        let server_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let (mut ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let ids = ["alpha", "beta", "cancelled", "delta", "epsilon", "zeta"];
        let configured=ids.iter().map(|id|json!({"id":id,"enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp")})).collect::<Vec<_>>();
        ctx.config = json!({"mcpServers":configured});
        let mut budget = ctx.runtime.mcp_lifecycle.budget();
        budget.calls = 2;
        budget.sampled_at = Some(0);
        ctx.runtime.mcp_lifecycle.apply_budget(budget);
        *ctx.runtime.mcp_lifecycle.last_refresh.lock().unwrap() = Some(tokio::time::Instant::now());
        for server in servers(&ctx) {
            drop(connection(&server, &ctx).await.unwrap());
        }
        let launch = |id: &str, ctx: GatewayToolContext| {
            let id = id.to_owned();
            tokio::spawn(async move {
                execute(&format!("mcp.{id}.echo"),json!({"arguments":{"text":id},"policy":{"reason":"isolated scheduling fixture"}}),&ctx).await
            })
        };
        let mut first = launch("alpha", ctx.clone());
        let mut second = launch("beta", ctx.clone());
        let mut actual = HashSet::new();
        for _ in 0..2 {
            actual.insert(
                tokio::time::timeout(Duration::from_secs(2), received.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(
            actual,
            HashSet::from(["alpha".to_owned(), "beta".to_owned()])
        );
        assert_eq!(ctx.runtime.mcp_lifecycle.calls.active(), 2);
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let mut cancelled = ctx.clone();
        cancelled.cancellation = receiver;
        let waiting = launch("cancelled", cancelled);
        let queued = ctx.runtime.mcp.lock().await["cancelled"].clone();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if queued.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel.send(true).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), waiting)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .code,
            "TOOL_CANCELLED"
        );
        assert!(
            received.try_recv().is_err(),
            "Cancelled queued work must never reach the remote handler"
        );
        let mut budget = ctx.runtime.mcp_lifecycle.budget();
        budget.calls = 1;
        ctx.runtime.mcp_lifecycle.apply_budget(budget);
        let third = launch("delta", ctx.clone());
        release.add_permits(1);
        let first_completed = tokio::select! {result=&mut first=>{result.unwrap().unwrap();true},result=&mut second=>{result.unwrap().unwrap();false}};
        let remaining = if first_completed { second } else { first };
        assert_eq!(ctx.runtime.mcp_lifecycle.calls.active(), 1);
        assert!(
            received.try_recv().is_err(),
            "Shrinking must wait for the second admitted exchange"
        );
        release.add_permits(1);
        remaining.await.unwrap().unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap()
                .unwrap(),
            "delta"
        );
        let mut budget = ctx.runtime.mcp_lifecycle.budget();
        budget.calls = 3;
        ctx.runtime.mcp_lifecycle.apply_budget(budget);
        let fourth = launch("epsilon", ctx.clone());
        let fifth = launch("zeta", ctx.clone());
        let mut grown = HashSet::new();
        for _ in 0..2 {
            grown.insert(
                tokio::time::timeout(Duration::from_secs(2), received.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(
            grown,
            HashSet::from(["epsilon".to_owned(), "zeta".to_owned()])
        );
        assert_eq!(ctx.runtime.mcp_lifecycle.calls.active(), 3);
        assert_eq!(counts.peak.load(Ordering::Acquire), 3);
        release.add_permits(3);
        for task in [third, fourth, fifth] {
            assert!(!task.await.unwrap().unwrap()["content"].is_null());
        }
        assert_eq!(ctx.runtime.mcp_lifecycle.calls.active(), 0);
        drop(queued);
        stop("cancelled", &ctx).await.unwrap();
        assert_eq!(
            execute(
                "mcp.cancelled.echo",
                json!({"arguments":{"text":"not dispatched"}}),
                &ctx
            )
            .await
            .unwrap_err()
            .code,
            "MCP_STOPPED"
        );
        assert!(received.try_recv().is_err());
        shutdown(&ctx).await;
        server_task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sse_multiline_frames_and_partial_frames_are_preserved() {
        let mut value = "event: message\ndata: {\ndata: \"id\":1}\n\npartial".to_owned();
        assert_eq!(
            sse_frames(&mut value),
            vec![("message".into(), "{\n\"id\":1}".into())]
        );
        assert_eq!(value, "partial");
        let mut split = "data: {\"id\":2}\r".to_owned();
        assert!(sse_frames(&mut split).is_empty());
        split.push_str("\n\r");
        assert!(sse_frames(&mut split).is_empty());
        split.push('\n');
        assert_eq!(sse_frames(&mut split), vec![(String::new(), "{\"id\":2}".into())]);
        assert!(split.is_empty());
    }

    #[tokio::test]
    async fn resource_only_servers_skip_tools_and_unknown_protocols_stop_before_calls() {
        use axum::{Json, Router, extract::State, routing::post};
        #[derive(Clone)]
        struct Fixture {
            version: &'static str,
            methods: Arc<std::sync::Mutex<Vec<String>>>,
        }
        async fn handler(State(fixture): State<Fixture>, Json(request): Json<Value>) -> Json<Value> {
            fixture.methods.lock().unwrap().push(request["method"].as_str().unwrap().to_owned());
            let result = if request["method"] == "initialize" {
                json!({"protocolVersion":fixture.version,"capabilities":{"resources":{}},"serverInfo":{"name":"resource-only","version":"1"}})
            } else {
                assert_ne!(request["method"], "tools/list", "A resource-only server has no tools capability");
                json!({"contents":[{"uri":"fixture://source","text":"source evidence"}]})
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
        for version in ["2025-11-25", "2026-07-28"] {
            let methods = Arc::new(std::sync::Mutex::new(Vec::new()));
            let fixture = Fixture { version, methods: methods.clone() };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server_task = tokio::spawn(async move {
                axum::serve(listener, Router::new().route("/mcp", post(handler)).with_state(fixture)).await.unwrap();
            });
            let (mut ctx, _keep) = super::super::gateway::test_context();
            let root = ctx.workdir.parent().unwrap().to_owned();
            ctx.approved = true;
            let server = json!({"id":"resources","name":"Resources","enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp")});
            ctx.config["mcpServers"] = json!([server]);
            let client = connection(&ctx.config["mcpServers"][0], &ctx).await;
            if version == PROTOCOL {
                assert_eq!(client.unwrap().lock().await.tools.len(), 3);
                let reply = execute("mcp.resources.kynxa_resources_read", json!({"arguments":{"uri":"fixture://source"}}), &ctx).await.unwrap();
                assert_eq!(reply["contents"][0]["text"], "source evidence");
                assert!(!methods.lock().unwrap().iter().any(|method| method == "tools/list"));
            } else {
                assert_eq!(client.err().unwrap().code, "MCP_UNSUPPORTED_PROTOCOL");
                assert_eq!(*methods.lock().unwrap(), vec!["initialize"]);
                let mut unsupported = ctx.config["mcpServers"][0].clone();
                unsupported["protocolVersion"] = json!(version);
                assert_eq!(McpClient::connect(&unsupported, &ctx).await.err().unwrap().code, "MCP_UNSUPPORTED_PROTOCOL");
                assert_eq!(methods.lock().unwrap().len(), 1, "Unsupported local configuration must not connect");
            }
            shutdown(&ctx).await;
            server_task.abort();
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn installed_node_mcp_singleflight_reuses_and_confirms_owned_process_exit() {
        let Some(node) = std::env::var_os("KYNXA_TEST_NODE_EXE") else {
            eprintln!(
                "未设置已安装 Node 夹具路径，跳过实际进程 / Installed Node fixture path is unset; no process was run."
            );
            return;
        };
        let node = std::path::PathBuf::from(node);
        assert!(installed_path(&node, false));
        let (mut ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let script = ctx.workdir.join("mcp-fixture.cjs");
        let started = ctx.workdir.join("started.log");
        // This isolated optional external MCP fixture uses only an already installed runtime and built-in modules.
        // 独立可选外部 MCP 夹具仅使用已安装运行时和内置模块，不下载包、不成为核心后端代码。
        std::fs::write(&script, r#"
const fs = require('node:fs');
fs.appendFileSync(process.argv[2], `${process.pid}\n`);
require('node:readline').createInterface({ input: process.stdin }).on('line', line => {
  const q = JSON.parse(line);
  if (q.id === undefined) return;
  const result = q.method === 'initialize'
    ? { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'owned-fixture', version: '1' } }
    : q.method === 'tools/list'
    ? { tools: [{ name: 'echo', inputSchema: { type: 'object', properties: { text: { type: 'string' } } } }] }
    : { content: [{ type: 'text', text: JSON.stringify({ pid: process.pid, text: q.params.arguments.text }) }] };
  process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: q.id, result })}\n`);
});
"#).unwrap();
        let mut server = json!({"id":"installed-node-fixture","name":"Installed fixture","enabled":true,"trusted":false,
            "transport":"stdio","command":node,"args":[script,started],"cwd":ctx.workdir,"startupTimeoutMs":10000});
        ctx.config = json!({"mcpServers":[server.clone()]});
        assert_eq!(prewarm(&ctx)["scheduled"], 0);
        assert!(!started.exists());
        reset(&ctx).await;
        server["trusted"] = json!(true);
        ctx.config = json!({"mcpServers":[server.clone()]});
        let old_generation = ctx.runtime.mcp_lifecycle.generation.load(Ordering::Acquire);
        let queued_old_configuration =
            connection_with_priority(&server, &ctx, true, Some(old_generation));
        reset(&ctx).await;
        assert_eq!(
            queued_old_configuration.await.err().unwrap().code,
            "MCP_START_CANCELLED"
        );
        assert!(
            !started.exists(),
            "Stale configuration started a real process after reset"
        );
        let foreground_request = foreground(&ctx.runtime);
        assert_eq!(prewarm(&ctx)["scheduled"], 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!started.exists());
        let connections = tokio::time::timeout(
            Duration::from_secs(15),
            futures_util::future::join_all((0..8).map(|_| connection(&server, &ctx))),
        )
        .await
        .unwrap();
        let first = connections[0].as_ref().unwrap().clone();
        for connection in &connections {
            assert!(Arc::ptr_eq(&first, connection.as_ref().unwrap()));
        }
        assert_eq!(
            std::fs::read_to_string(&started).unwrap().lines().count(),
            1
        );
        let mut state = first.lock().await;
        let reply = state
            .rpc(
                "tools/call",
                json!({"name":"echo","arguments":{"text":"hot call"}}),
                &ctx,
                10000,
            )
            .await
            .unwrap();
        let echoed: Value =
            serde_json::from_str(reply["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(echoed["text"], "hot call");
        let old_pid = match &state.transport {
            Transport::Stdio { child, .. } => child.id().unwrap(),
            _ => panic!("Expected owned stdio fixture"),
        };
        assert_eq!(echoed["pid"], json!(old_pid));
        match &state.transport {
            Transport::Stdio { _owned, .. } => {
                let memory = _owned.memory_usage().unwrap();
                assert!(
                    memory.private_bytes > 0
                        && memory.working_set_bytes > 0
                        && memory.processes > 0
                );
            }
            _ => unreachable!(),
        }
        drop(state);
        drop(foreground_request);
        stop("installed-node-fixture", &ctx).await.unwrap();
        assert!(ctx.runtime.mcp.lock().await.is_empty());
        assert_eq!(
            connection(&server, &ctx).await.err().unwrap().code,
            "MCP_STOPPED"
        );
        match &mut first.lock().await.transport {
            Transport::Stdio { child, _owned, .. } => {
                assert!(child.try_wait().unwrap().is_some());
                assert!(!_owned.has_running_processes().unwrap());
            }
            _ => unreachable!(),
        }
        let reconnected = restart(&server, &ctx).await.unwrap();
        let new_pid = match &reconnected.lock().await.transport {
            Transport::Stdio { child, .. } => child.id().unwrap(),
            _ => unreachable!(),
        };
        assert_ne!(old_pid, new_pid);
        assert_eq!(
            std::fs::read_to_string(&started).unwrap().lines().count(),
            2
        );
        let mut budget = ctx.runtime.mcp_lifecycle.budget();
        budget.idle_memory_bytes = 0;
        ctx.runtime.mcp_lifecycle.apply_budget(budget);
        reap_idle(&ctx, Duration::from_secs(600)).await;
        assert!(
            ctx.runtime
                .mcp
                .lock()
                .await
                .contains_key("installed-node-fixture"),
            "An externally held active connection must not be evicted to meet an idle-only budget"
        );
        let weak = Arc::downgrade(&reconnected);
        drop(connections);
        drop(first);
        drop(reconnected);
        reap_idle(&ctx, Duration::from_secs(600)).await;
        assert!(
            ctx.runtime.mcp.lock().await.is_empty(),
            "Actual owned private memory above the idle budget must be reclaimed before the age timeout"
        );
        assert!(weak.upgrade().is_none());
        assert_eq!(
            ctx.runtime
                .mcp_lifecycle
                .idle_private_bytes
                .load(Ordering::Acquire),
            0
        );
        let reconnected = connection(&server, &ctx).await.unwrap();
        shutdown(&ctx).await;
        match &mut reconnected.lock().await.transport {
            Transport::Stdio { child, _owned, .. } => {
                assert!(child.try_wait().unwrap().is_some());
                assert!(!_owned.has_running_processes().unwrap());
            }
            _ => unreachable!(),
        }
        drop(reconnected);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn fixture_result(request: &Value) -> Value {
        match request["method"].as_str().unwrap_or("") {
            "initialize" => {
                json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"echo","description":"Echo fixture","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]})
            }
            "tools/call" => {
                json!({"content":[{"type":"text","text":request["params"]["arguments"]["text"]}]})
            }
            "resources/read" => {
                json!({"contents":[{"uri":request["params"]["uri"],"mimeType":"text/plain","text":"original resource"}]})
            }
            _ => json!({}),
        }
    }

    #[tokio::test]
    async fn actual_streamable_http_negotiates_session_and_resource_calls_without_node() {
        use axum::{
            Json, Router,
            body::Body,
            http::{HeaderMap, Response, StatusCode},
            routing::post,
        };
        async fn handler(headers: HeaderMap, Json(request): Json<Value>) -> Response<Body> {
            if request["method"] != "initialize" {
                assert_eq!(headers.get("mcp-session-id").unwrap(), "fixture-session");
            }
            if request["id"].is_null() {
                return Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .body(Body::empty())
                    .unwrap();
            }
            let value =
                json!({"jsonrpc":"2.0","id":request["id"],"result":fixture_result(&request)});
            if request["method"] == "tools/call" {
                return Response::builder()
                    .header("Content-Type", "text/event-stream")
                    .body(Body::from(format!("event: message\ndata: {value}\n\n")))
                    .unwrap();
            }
            Response::builder()
                .header("Content-Type", "application/json")
                .header("Mcp-Session-Id", "fixture-session")
                .body(Body::from(value.to_string()))
                .unwrap()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/mcp", post(handler)))
                .await
                .unwrap();
        });
        let (mut ctx, _cancel) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.approved = true;
        ctx.config["mcpServers"] = json!([{"id":"fixture","name":"Fixture","enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp")}]);
        let loaded =
            super::super::gateway::execute("tool.load", json!({"names":["mcp.fixture"]}), &ctx)
                .await
                .unwrap();
        assert_eq!(loaded["tools"].as_array().unwrap().len(), 4);
        let result = execute(
            "mcp.fixture.echo",
            json!({"arguments":{"text":"actual rpc"},"policy":{"reason":"fixture"}}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(result["content"][0]["text"], "actual rpc");
        let resource = execute(
            "mcp.fixture.kynxa_resources_read",
            json!({"arguments":{"uri":"fixture://original"},"policy":{"reason":"fixture"}}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(resource["contents"][0]["text"], "original resource");
        shutdown(&ctx).await;
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn actual_legacy_sse_uses_announced_post_endpoint_and_matches_rpc_ids() {
        use axum::{
            Json, Router,
            extract::State,
            http::StatusCode,
            response::sse::{Event, Sse},
            routing::{get, post},
        };
        use std::convert::Infallible;
        #[derive(Clone)]
        struct Fixture {
            sender: mpsc::Sender<Value>,
            receiver: Arc<Mutex<Option<mpsc::Receiver<Value>>>>,
        }
        async fn events(
            State(state): State<Fixture>,
        ) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
            let receiver = state.receiver.lock().await.take().unwrap();
            let initial = futures_util::stream::once(async {
                Ok(Event::default().event("endpoint").data("/messages"))
            });
            let values = tokio_stream::wrappers::ReceiverStream::new(receiver)
                .map(|value| Ok(Event::default().event("message").data(value.to_string())));
            Sse::new(initial.chain(values))
        }
        async fn messages(State(state): State<Fixture>, Json(request): Json<Value>) -> StatusCode {
            if !request["id"].is_null() {
                state.sender.send(json!({"jsonrpc":"2.0","id":request["id"],"result":fixture_result(&request)})).await.unwrap();
            }
            StatusCode::ACCEPTED
        }
        let (sender, receiver) = mpsc::channel(32);
        let state = Fixture {
            sender,
            receiver: Arc::new(Mutex::new(Some(receiver))),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/sse", get(events))
                    .route("/messages", post(messages))
                    .with_state(state),
            )
            .await
            .unwrap();
        });
        let (mut ctx, _cancel) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.approved = true;
        ctx.config["mcpServers"] = json!([{"id":"legacy","name":"Legacy","enabled":true,"transport":"sse","url":format!("http://{address}/sse")}]);
        let tools = catalog(&ctx).await.unwrap();
        assert_eq!(tools.len(), 4);
        let result = execute(
            "mcp.legacy.echo",
            json!({"arguments":{"text":"legacy actual"},"policy":{"reason":"fixture"}}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(result["content"][0]["text"], "legacy actual");
        shutdown(&ctx).await;
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sse_incremental_decoder_preserves_all_line_endings_and_utf8_boundaries() {
        for ending in ["\n", "\r\n", "\r"] {
            let source = format!(
                "event: message{ending}data: {{\"id\":1,\"text\":\"中文\"}}{ending}{ending}"
            );
            for split in 0..=source.len() {
                let mut buffer = SseBuffer::default();
                let mut frames = buffer.push(&source.as_bytes()[..split]).unwrap();
                frames.extend(buffer.push(&source.as_bytes()[split..]).unwrap());
                assert_eq!(
                    frames,
                    vec![("message".into(), "{\"id\":1,\"text\":\"中文\"}".into())],
                    "{ending:?} split {split}"
                );
            }
            let mut buffer = SseBuffer::default();
            let mut frames = Vec::new();
            for byte in source.as_bytes() {
                frames.extend(buffer.push(&[*byte]).unwrap());
            }
            assert_eq!(frames.len(), 1);
        }
    }

    #[test]
    fn sse_incremental_decoder_limits_accumulated_unterminated_frames() {
        let mut buffer = SseBuffer::default();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..32 {
            assert!(buffer.push(&chunk).unwrap().is_empty());
        }
        assert_eq!(buffer.push(b"x").unwrap_err().code, "MCP_RESULT_TOO_LARGE");
        assert_eq!(
            SseBuffer::default().push(&[0xff]).unwrap_err().code,
            "MCP_INVALID_RESPONSE"
        );
    }

    #[tokio::test]
    async fn streamable_http_rpc_decodes_fragmented_crlf_and_rejects_oversized_sse() {
        use axum::{Router, response::IntoResponse, routing::get};
        async fn fragmented() -> impl IntoResponse {
            let data = "data: {\"id\":7,\"result\":{\"text\":\"中文\"}}\r\n\r\n";
            let chunks = data
                .as_bytes()
                .iter()
                .map(|byte| vec![*byte])
                .collect::<Vec<_>>();
            let stream =
                futures_util::stream::unfold(chunks.into_iter(), |mut chunks| async move {
                    let chunk = chunks.next()?;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    Some((Ok::<_, std::convert::Infallible>(chunk), chunks))
                });
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(stream),
            )
        }
        async fn oversized() -> impl IntoResponse {
            let stream = futures_util::stream::iter(
                (0..33).map(|_| Ok::<_, std::convert::Infallible>(vec![b'x'; 1024 * 1024])),
            );
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(stream),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/fragmented", get(fragmented))
                    .route("/oversized", get(oversized)),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let response = client
            .get(format!("http://{address}/fragmented"))
            .send()
            .await
            .unwrap();
        let value = response_rpc(response, 7).await.unwrap();
        assert_eq!(value["result"]["text"], "中文");
        let response = client
            .get(format!("http://{address}/oversized"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response_rpc(response, 7).await.unwrap_err().code,
            "MCP_RESULT_TOO_LARGE"
        );
        task.abort();
        let _ = task.await;
    }
}
