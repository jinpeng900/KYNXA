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
const PROTOCOL: &str = "2025-11-25";
const MCP_BUDGET_REFRESH: Duration = Duration::from_secs(5);

/// Lifecycle gates own startup/stop scheduling; the existing client map remains the only connection owner.
/// 生命周期门控仅负责启动、停止调度，现有 client map 仍是唯一连接所有者，不另造连接账本。
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
    sequence: u64,
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
}

impl Drop for McpClient {
    fn drop(&mut self) {
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
fn validate_runtime(server: &Value) -> Result<bool, GatewayError> {
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

async fn headers(server: &Value) -> Result<HeaderMap, GatewayError> {
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

async fn response_rpc(response: reqwest::Response, id: u64) -> Result<Value, GatewayError> {
    if !response.status().is_success() {
        return Err(failure(
            "MCP_HTTP_REJECTED",
            &format!("MCP 服务返回 HTTP {}。", response.status().as_u16()),
            response.status().as_u16(),
        ));
    }
    let is_sse = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 响应传输中断。", 502))?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_RPC_BYTES {
            return Err(failure(
                "MCP_RESULT_TOO_LARGE",
                "MCP 响应超过传输上限。",
                413,
            ));
        }
        if is_sse {
            // Retain incomplete UTF-8 bytes until the next chunk instead of lossy packet decoding.
            // 跨分包保留不完整 UTF-8 字节，不按每个网络块有损解码。
            if let Ok(decoded) = std::str::from_utf8(&bytes) {
                text.push_str(&decoded.replace("\r\n", "\n"));
                bytes.clear();
            }
            for (_, frame) in sse_frames(&mut text) {
                if let Ok(value) = serde_json::from_str::<Value>(&frame) {
                    if value["id"] == id {
                        return Ok(value);
                    }
                }
            }
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

impl McpClient {
    async fn connect(server: &Value, ctx: &GatewayToolContext) -> Result<Self, GatewayError> {
        check_cancelled(ctx)?;
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
                    server["timeoutMs"].as_u64().unwrap_or(120_000),
                ))
                .build()
                .map_err(|_| failure("MCP_HTTP_FAILED", "MCP HTTP 客户端初始化失败。", 500))?;
            let headers = headers(server).await?;
            if server["transport"] == "sse" {
                let response = client
                    .get(url.clone())
                    .headers(headers.clone())
                    .send()
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_FAILED", "MCP SSE 无法连接。", 503))?
                    .error_for_status()
                    .map_err(|_| failure("MCP_CONNECTION_FAILED", "MCP SSE 连接被拒绝。", 503))?;
                let (sender, mut receiver) = mpsc::channel(128);
                let origin = url.origin();
                let base = url.clone();
                let reader = tokio::spawn(async move {
                    let mut stream = response.bytes_stream();
                    let mut buffer = String::new();
                    let mut bytes = Vec::new();
                    while let Some(Ok(chunk)) = stream.next().await {
                        bytes.extend_from_slice(&chunk);
                        if bytes.len() + buffer.len() > MAX_RPC_BYTES {
                            break;
                        }
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            buffer.push_str(&text.replace("\r\n", "\n"));
                            bytes.clear();
                        }
                        for (event, data) in sse_frames(&mut buffer) {
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
                    reader.abort();
                    return Err(failure(
                        "MCP_ENDPOINT_ORIGIN_BLOCKED",
                        "MCP SSE endpoint 不能跨配置源。",
                        403,
                    ));
                }
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
            sequence: 0,
            protocol: server["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL)
                .to_owned(),
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
        };
        let result=client.rpc("initialize",json!({"protocolVersion":client.protocol,"capabilities":{},"clientInfo":{"name":"KYNXA","version":env!("CARGO_PKG_VERSION")}}),ctx,server["startupTimeoutMs"].as_u64().unwrap_or(30_000)).await?;
        client.protocol = result["protocolVersion"]
            .as_str()
            .ok_or_else(|| failure("MCP_INVALID_INITIALIZE", "MCP 初始化没有协议版本。", 502))?
            .to_owned();
        let supports_resources = result["capabilities"]["resources"].is_object();
        client.resources = supports_resources;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        let mut cursor = Value::Null;
        for _ in 0..32 {
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

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), GatewayError> {
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
                request
                    .send()
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 通知发送失败。", 502))?
                    .error_for_status()
                    .map_err(|_| {
                        failure("MCP_NOTIFICATION_REJECTED", "MCP 通知被服务拒绝。", 502)
                    })?;
            }
            Transport::Sse {
                client,
                endpoint,
                headers,
                ..
            } => {
                client
                    .post(endpoint.clone())
                    .headers(headers.clone())
                    .json(&request)
                    .send()
                    .await
                    .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP 通知发送失败。", 502))?
                    .error_for_status()
                    .map_err(|_| failure("MCP_NOTIFICATION_REJECTED", "MCP 通知被拒绝。", 502))?;
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
        self.last_used = tokio::time::Instant::now();
        let subscription_uri = params["uri"].as_str().map(str::to_owned);
        self.sequence += 1;
        let id = self.sequence;
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
                    client
                        .post(endpoint.clone())
                        .headers(headers.clone())
                        .json(&request)
                        .send()
                        .await
                        .map_err(|_| failure("MCP_CONNECTION_LOST", "MCP SSE 请求发送失败。", 502))?
                        .error_for_status()
                        .map_err(|_| {
                            failure("MCP_REQUEST_REJECTED", "MCP SSE 请求被拒绝。", 502)
                        })?;
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
                return Err(failure(
                    "MCP_RPC_ERROR",
                    envelope["error"]["message"]
                        .as_str()
                        .unwrap_or("MCP 服务执行失败。"),
                    502,
                ));
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
}

pub(crate) async fn connection(
    server: &Value,
    ctx: &GatewayToolContext,
) -> Result<Arc<Mutex<McpClient>>, GatewayError> {
    connection_with_priority(server, ctx, false, None).await
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
    // An owned admission future survives caller cancellation long enough to retire a racing late grant.
    // 自有准入任务在调用方取消后继续核销竞争中的晚到授额，不能丢弃尚未兑现的预约回执。
    let attempt = tokio::spawn(async move { service.acquire(request, cancellation).await });
    let lease = attempt
        .await
        .map_err(|_| failure("RESOURCE_RUNTIME_EXITED", "MCP 资源准入任务退出。", 503))??;
    check_cancelled(ctx)?;
    ctx.runtime.mcp_lifecycle.check(id, generation)?;
    Ok(Some(lease))
}
async fn connection_with_priority(
    server: &Value,
    ctx: &GatewayToolContext,
    background: bool,
    expected_generation: Option<u64>,
) -> Result<Arc<Mutex<McpClient>>, GatewayError> {
    let id = server["id"].as_str().unwrap_or("");
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    let generation =
        expected_generation.unwrap_or_else(|| lifecycle.generation.load(Ordering::Acquire));
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
    // Background waiters never hold the per-service gate while yielding to a foreground request.
    // 后台等待前台让路时不能占住服务单飞锁，否则前台申请同一服务会互相等待。
    let gate = lifecycle.gate(id).await;
    let (_singleflight, background_permit) = if background {
        loop {
            lifecycle.check(id, generation)?;
            check_cancelled(ctx)?;
            if lifecycle.foreground_waiters.load(Ordering::Acquire) == 0
                && lifecycle.foreground_active.load(Ordering::Acquire) == 0
            {
                if let Some(permit) = lifecycle.starts.try_acquire_owned() {
                    // Never wait for a service gate while holding capacity needed by a foreground owner of that gate.
                    // 前台可能已持服务锁等待启动额度；后台拿不到锁就立即归还额度，不能形成反向互等。
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
        if saved == &configuration && when.elapsed() < Duration::from_secs(2) {
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
            ));
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
        if state.background_task || !state.subscriptions.is_empty() {
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
        if state.background_task || !state.subscriptions.is_empty() {
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
                    .insert(id, json!({"code":error.code,"message":error.message}));
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

/// Warm trusted installed services using observed scheduling capacity; never install dependencies or borrow chat authority.
/// 依据真实资源额度预热受信任的已安装服务，不安装依赖、不继承聊天授权或执行陌生仓库命令。
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
                            json!({"code":error.code,"message":error.message}),
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

pub(crate) async fn catalog_selected(
    ctx: &GatewayToolContext,
    servers: Vec<Value>,
) -> Result<Vec<Value>, GatewayError> {
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
                for tool in &client.lock().await.tools {
                    if server["disabledTools"]
                        .as_array()
                        .is_some_and(|disabled| disabled.contains(&tool["name"]))
                    {
                        continue;
                    }
                    all.push(json!({"name":format!("mcp.{}.{}",server["id"].as_str().unwrap_or(""),tool["name"].as_str().unwrap_or("")),
                    "description":tool["description"],"source":format!("mcp:{}",server["id"].as_str().unwrap_or("")),"serverId":server["id"],"toolName":tool["name"],
                    "originalInputSchema":tool["inputSchema"],"inputSchema":{"type":"object","properties":{"arguments":tool["inputSchema"],"policy":{"type":"object","properties":{"reason":{"type":"string","minLength":1,"maxLength":2000}}}},"required":["arguments","policy"],"additionalProperties":false}}));
                }
            }
            Err(error) => {
                ctx.runtime.mcp_errors.lock().await.insert(
                    server["id"].as_str().unwrap_or("").to_owned(),
                    json!({"code":error.code,"message":error.message}),
                );
                emit(ctx,json!({"type":"mcp_status","serverId":server["id"],"ready":false,"code":error.code,"message":error.message})).await;
            }
        }
    }
    Ok(all)
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
    let server = servers(ctx)
        .into_iter()
        .find(|server| server["id"] == id)
        .ok_or_else(|| failure("MCP_SERVER_NOT_FOUND", "MCP 服务未启用。", 404))?;
    if server["disabledTools"]
        .as_array()
        .is_some_and(|disabled| disabled.iter().any(|value| value == tool))
    {
        return Err(failure("MCP_TOOL_DISABLED", "MCP 工具已被用户禁用。", 403));
    }
    let client = connection(&server, ctx).await?;
    let lifecycle = &ctx.runtime.mcp_lifecycle;
    let generation = lifecycle.generation.load(Ordering::Acquire);
    // Keep one exchange per connection, while independent services share a dynamically resized quota.
    // 单连接保持一次完整协议交换，不同服务共用动态可收缩的调用额度；排队取消尚未派发副作用。
    let mut client = tokio::select! {client=client.lock()=>client,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
    if !client.tools.iter().any(|entry| entry["name"] == tool) {
        return Err(failure(
            "MCP_TOOL_NOT_FOUND",
            "MCP 工具不存在于实际服务目录。",
            404,
        ));
    }
    let descriptor = client
        .tools
        .iter()
        .find(|entry| entry["name"] == tool)
        .cloned()
        .unwrap();
    let arguments = args.get("arguments").cloned().unwrap_or(args);
    let operation = descriptor["operation"].as_str().unwrap_or("tools/call");
    let params = if operation == "tools/call" {
        json!({"name":tool,"arguments":arguments})
    } else {
        arguments
    };
    let _permit = tokio::select! {permit=lifecycle.calls.acquire_owned()=>permit,error=lifecycle_cancelled(ctx,id,generation)=>return Err(error)};
    let _buffers = admit_buffers(ctx, id, generation, false).await?;
    check_cancelled(ctx)?;
    lifecycle.check(id, generation)?;
    if *client.close.borrow() {
        return Err(failure(
            "MCP_CONNECTION_CLOSED",
            "MCP 连接已关闭，未派发操作。",
            409,
        ));
    }
    let result = client
        .rpc(
            operation,
            params,
            ctx,
            server["timeoutMs"].as_u64().unwrap_or(120_000),
        )
        .await;
    drop(client);
    match result {
        Ok(value) => {
            ctx.runtime.mcp_errors.lock().await.remove(id);
            Ok(value)
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
                json!({"code":error.code,"message":error.message}),
            );
            let _ = disconnect(id, ctx).await;
            Ok(
                json!({"isError":true,"status":"unknown","code":"MCP_EXECUTION_UNKNOWN","causeCode":error.code,"content":[{"type":"text","text":error.message}],"executionEnvironment":{"executorKind":if server["transport"]=="stdio"{"mcp-stdio"}else{"mcp-http"},"executorLocation":if server["transport"]=="stdio"{"gateway-host"}else{"remote-service"}}}),
            )
        }
        Err(error) => Err(error),
    }
}

pub async fn shutdown(ctx: &GatewayToolContext) {
    ctx.runtime
        .mcp_lifecycle
        .closing
        .store(true, Ordering::Release);
    ctx.runtime
        .mcp_lifecycle
        .generation
        .fetch_add(1, Ordering::AcqRel);
    if let Ok(mut task) = ctx.runtime.mcp_lifecycle.maintenance.lock() {
        if let Some(task) = task.take() {
            task.abort();
        }
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
    for id in ids {
        let _ = disconnect(&id, ctx).await;
    }
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
                json!({"code":error.code,"message":error.message}),
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
        if let Some(error) = errors.get(id) { state["code"] = error["code"].clone(); state["state"] = json!(if error["code"] == "MCP_AUTH_REQUIRED" {"auth-required"} else {"error"}); }
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
        state
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
