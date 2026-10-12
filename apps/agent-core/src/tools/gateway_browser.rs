//! Chromium DevTools automation, with an owned isolated profile and explicit tab identity.
//! Chromium DevTools 自动化：独立配置目录、自有进程与明确标签页身份。

use super::gateway::{GatewayToolContext, check_cancelled, failure};
use super::gateway_process::{OwnedProcessJob, hide, terminate};
use crate::gateway_contract::GatewayError;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

type CdpReply = oneshot::Sender<Result<Value, GatewayError>>;
struct CdpCommand {
    id: u64,
    message: Value,
    reply: CdpReply,
}
#[derive(Default)]
struct EventBuffer {
    records: VecDeque<Value>,
    bytes: usize,
    discarded: u64,
    sequence: u64,
}
impl EventBuffer {
    fn push(&mut self, mut value: Value) {
        self.sequence += 1;
        value["sequence"] = json!(self.sequence);
        let size = value.to_string().len();
        if size > 128 * 1024 {
            self.discarded += 1;
            return;
        }
        while self.records.len() >= 2048 || self.bytes + size > 8 * 1024 * 1024 {
            let Some(record) = self.records.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(record.to_string().len());
            self.discarded += 1;
        }
        self.bytes += size;
        self.records.push_back(value);
    }
}
pub(crate) struct CdpClient {
    sender: mpsc::Sender<CdpCommand>,
    next_id: AtomicU64,
    events: Arc<Mutex<EventBuffer>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for CdpClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl CdpClient {
    async fn close(&self) -> Result<(), GatewayError> {
        self.task.abort();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.task.is_finished() {
            if tokio::time::Instant::now() >= deadline {
                return Err(failure(
                    "BROWSER_CONNECTION_CLEANUP_FAILED",
                    "浏览器事件连接退场未确认。",
                    503,
                ));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(())
    }
    async fn connect(socket: &str) -> Result<Arc<Self>, GatewayError> {
        let configuration = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(32 * 1024 * 1024))
            .max_frame_size(Some(32 * 1024 * 1024));
        let (mut connection, _) = tokio::time::timeout(
            Duration::from_secs(30),
            tokio_tungstenite::connect_async_with_config(socket, Some(configuration), false),
        )
        .await
        .map_err(|_| {
            failure(
                "BROWSER_CONNECT_TIMEOUT",
                "浏览器调试连接超时，动作尚未派发。",
                504,
            )
        })?
        .map_err(|_| failure("BROWSER_DISCONNECTED", "浏览器标签连接失败。", 502))?;
        let (sender, mut commands) = mpsc::channel::<CdpCommand>(64);
        let events = Arc::new(Mutex::new(EventBuffer::default()));
        let observed = events.clone();
        // Keep one owned connection for events/dialogs/traces; bounded buffers cannot grow with an idle page forever.
        // 事件、对话框、跟踪共享自有长连接；有界缓冲不能随闲置页面无限增长。
        let task = tokio::spawn(async move {
            let mut pending = HashMap::<u64, CdpReply>::new();
            loop {
                tokio::select! {
                    command = commands.recv() => {
                        let Some(command) = command else { break; };
                        pending.retain(|_, reply| !reply.is_closed());
                        if pending.len() >= 64 { let _ = command.reply.send(Err(failure("BROWSER_QUEUE_FULL", "浏览器动作队列已满。", 429))); continue; }
                        if connection.send(Message::Text(command.message.to_string().into())).await.is_err() {
                            let _ = command.reply.send(Err(failure("BROWSER_EXECUTION_UNKNOWN", "浏览器请求发送未确认。", 502))); break;
                        }
                        pending.insert(command.id, command.reply);
                    }
                    frame = connection.next() => {
                        let text = match frame { Some(Ok(Message::Text(text))) => text,
                            None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break, _ => continue };
                        let Ok(value) = serde_json::from_str::<Value>(&text) else { break; };
                        if let Some(id) = value["id"].as_u64() {
                            if let Some(reply) = pending.remove(&id) {
                                let result = if !value["error"].is_null() { Err(failure("BROWSER_TOOL_FAILED", value["error"]["message"].as_str().unwrap_or("浏览器操作失败。"), 502)) } else { Ok(value["result"].clone()) };
                                let _ = reply.send(result);
                            }
                        } else if value["method"].is_string() { observed.lock().await.push(value); }
                    }
                }
            }
            for (_, reply) in pending {
                let _ = reply.send(Err(failure(
                    "BROWSER_EXECUTION_UNKNOWN",
                    "浏览器已接收请求后连接断开，先观察状态。",
                    502,
                )));
            }
        });
        Ok(Arc::new(Self {
            sender,
            next_id: AtomicU64::new(1),
            events,
            task,
        }))
    }
    async fn call(
        &self,
        method: &str,
        params: Value,
        ctx: &GatewayToolContext,
    ) -> Result<Value, GatewayError> {
        check_cancelled(ctx)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, result) = oneshot::channel();
        self.sender
            .send(CdpCommand {
                id,
                message: json!({"id":id,"method":method,"params":params}),
                reply,
            })
            .await
            .map_err(|_| {
                failure(
                    "BROWSER_DISCONNECTED",
                    "浏览器连接不可用，动作未派发。",
                    502,
                )
            })?;
        let mut cancellation = ctx.cancellation.clone();
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(60), result) => result.map_err(|_|failure("BROWSER_EXECUTION_UNKNOWN","浏览器动作超时，先核对页面。",504))?
                .map_err(|_|failure("BROWSER_EXECUTION_UNKNOWN","浏览器执行回执未确认。",502))?,
            _ = async { loop { if *cancellation.borrow_and_update() { break; } if cancellation.changed().await.is_err() { std::future::pending::<()>().await; } } } => Err(failure("BROWSER_EXECUTION_UNKNOWN","浏览器已提交动作后取消，先核对页面。",409))
        }
    }
}
fn connection_key(socket: &str, ctx: &GatewayToolContext) -> String {
    format!("{}\0{socket}", ctx.conversation_id)
}
async fn client(socket: &str, ctx: &GatewayToolContext) -> Result<Arc<CdpClient>, GatewayError> {
    let key = connection_key(socket, ctx);
    let mut clients = ctx.runtime.cdp.lock().await;
    if let Some(client) = clients
        .get(&key)
        .filter(|client| !client.task.is_finished())
    {
        return Ok(client.clone());
    }
    if clients.len() >= 128 {
        return Err(failure(
            "BROWSER_CONNECTION_CAPACITY",
            "浏览器活动标签连接达到有界容量。",
            429,
        ));
    }
    let connected = CdpClient::connect(socket).await?;
    for method in ["Page.enable", "Runtime.enable", "Network.enable"] {
        connected.call(method, json!({}), ctx).await?;
    }
    clients.insert(key, connected.clone());
    Ok(connected)
}
async fn rpc(
    socket: &str,
    method: &str,
    params: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    client(socket, ctx).await?.call(method, params, ctx).await
}

pub(crate) struct BrowserSession {
    child: Option<Child>,
    endpoint: String,
    profile: PathBuf,
    conversation_id: String,
    tabs: HashMap<String, String>,
    active_tab: Option<String>,
    uploads: Vec<PathBuf>,
    upload_bytes: u64,
    _owned: Option<OwnedProcessJob>,
    closed: bool,
    profile_cleanup_pending: bool,
    profile_cleanup_error: Option<GatewayError>,
}

/// Cleanup keeps only the already-closed owned session; the existing sixteen-session bound also bounds these jobs.
/// 清理仅持有已确认关闭的自有会话，复用既有十六会话上限约束后台任务，不保留用户请求或另建调度器。
fn defer_owned_profile_cleanup(
    id: String,
    handle: Arc<Mutex<BrowserSession>>,
    runtime: &Arc<super::gateway::ToolRuntime>,
) {
    let runtime = Arc::downgrade(runtime);
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let mut session = handle.lock().await;
            match session.remove_owned_profile().await {
                Ok(()) => {
                    session.profile_cleanup_pending = false;
                    session.profile_cleanup_error = None;
                    drop(session);
                    if let Some(runtime) = runtime.upgrade() {
                        let mut sessions = runtime.browsers.lock().await;
                        if sessions.get(&id).is_some_and(|active| Arc::ptr_eq(active, &handle)) {
                            sessions.remove(&id);
                        }
                    }
                    return;
                }
                Err(error) => {
                    let retryable = error.code == "BROWSER_PROFILE_CLEANUP_FAILED";
                    session.profile_cleanup_error = Some(error);
                    if !retryable || tokio::time::Instant::now() >= deadline {
                        session.profile_cleanup_pending = false;
                        return;
                    }
                }
            }
        }
    });
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

impl BrowserSession {
    async fn close_owned_process(&mut self) -> Result<(), GatewayError> {
        if let Some(job) = &self._owned {
            job.terminate_and_wait().await?;
        }
        if let Some(child) = &mut self.child {
            terminate(child).await;
        }
        Ok(())
    }

    async fn remove_owned_profile(&self) -> Result<(), GatewayError> {
        let Some(job) = &self._owned else {
            return Ok(());
        };
        // Only retry transient Windows file locks after the entire owned job has verifiably exited.
        // 仅在整个自有作业已确认退出后，重试 Windows 临时文件锁；不能把等待进程隐藏为文件清理。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if job.has_running_processes()? {
                return Err(failure(
                    "BROWSER_PROCESS_CLEANUP_FAILED",
                    "浏览器进程树尚未退出，保留配置目录。",
                    503,
                ));
            }
            match tokio::fs::remove_dir_all(&self.profile).await {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error)
                    if matches!(error.raw_os_error(), Some(5 | 32 | 33 | 145))
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => {
                    return Err(failure(
                        "BROWSER_PROFILE_CLEANUP_FAILED",
                        &format!(
                            "浏览器已退出，但其自有配置目录清理未完成 / Owned browser profile cleanup failed: os={:?}, kind={:?}.",
                            error.raw_os_error(),
                            error.kind()
                        ),
                        503,
                    ));
                }
            }
        }
    }
}

pub(crate) fn descriptors() -> Vec<Value> {
    let reason = json!({"type":"string","minLength":1,"maxLength":2000});
    let mut output = Vec::new();
    for (name, description, properties, required) in [
        (
            "open",
            "Open a native Chromium browser in an isolated profile. Default headless/background; only start it when browser interaction is requested. Returns sessionId and tabId.",
            json!({"url":{"type":"string"},"foreground":{"type":"boolean"},"browserPath":{"type":"string"},"cdpEndpoint":{"type":"string","description":"Explicit HTTP(S) DevTools endpoint of an already authorized browser; never launches or kills that external process."}}),
            vec!["reason"],
        ),
        (
            "tabs",
            "List verified tabs in this chat's owned browser session.",
            json!({"sessionId":{"type":"string"}}),
            vec!["sessionId", "reason"],
        ),
        (
            "navigate",
            "Navigate an identified browser tab, then inspect current DOM before interacting.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"url":{"type":"string"}}),
            vec!["sessionId", "tabId", "url", "reason"],
        ),
        (
            "snapshot",
            "Read bounded DOM text and actionable element references from an identified tab. References expire on navigation or DOM change.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"maxChars":{"type":"integer","minimum":1,"maximum":64000}}),
            vec!["sessionId", "tabId", "reason"],
        ),
        (
            "screenshot",
            "Capture a background tab as PNG; does not activate a desktop window. Returns actual image data for vision-capable models.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"fullPage":{"type":"boolean"}}),
            vec!["sessionId", "tabId", "reason"],
        ),
        (
            "click",
            "Click a DOM selector in an identified tab. Require exactly one matching current element, never guess another tab.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"selector":{"type":"string","minLength":1,"maxLength":2048}}),
            vec!["sessionId", "tabId", "selector", "reason"],
        ),
        (
            "type",
            "Fill an editable DOM control identified by a unique current selector. No clipboard or foreground activation.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"selector":{"type":"string","minLength":1,"maxLength":2048},"text":{"type":"string","maxLength":16000}}),
            vec!["sessionId", "tabId", "selector", "text", "reason"],
        ),
        (
            "key",
            "Dispatch a supported browser key such as Enter, Tab, Escape, ArrowDown or Control+a.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"key":{"type":"string","maxLength":64}}),
            vec!["sessionId", "tabId", "key", "reason"],
        ),
        (
            "evaluate",
            "Evaluate JavaScript inside the selected webpage only. This is browser content scripting, not a backend Node runtime. Side effects remain subject to approval.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"script":{"type":"string","maxLength":64000}}),
            vec!["sessionId", "tabId", "script", "reason"],
        ),
        (
            "close",
            "Close an identified tab or this chat's owned browser session. Does not close unrelated local browser processes.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"}}),
            vec!["sessionId", "reason"],
        ),
    ] {
        let mut properties = properties;
        properties["reason"] = reason.clone();
        output.push(json!({"name":format!("browser.{name}"),"source":"builtin","description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}));
    }
    for (name, description, properties, required) in [
        (
            "new",
            "Create a tab in the identified browser, in the background by default.",
            json!({"url":{"type":"string"}}),
            vec!["url"],
        ),
        (
            "select",
            "Select an identified tab for automation; foreground activation requires an explicit current user request.",
            json!({"foreground":{"type":"boolean"}}),
            vec!["tabId"],
        ),
        (
            "back",
            "Navigate the identified tab to its previous history entry.",
            json!({}),
            vec!["tabId"],
        ),
        (
            "forward",
            "Navigate the identified tab to its next history entry.",
            json!({}),
            vec!["tabId"],
        ),
        (
            "reload",
            "Reload the identified tab and read its current state afterwards.",
            json!({}),
            vec!["tabId"],
        ),
        (
            "resize",
            "Set the identified tab's viewport without activating a desktop window.",
            json!({"width":{"type":"integer","minimum":100,"maximum":8192},"height":{"type":"integer","minimum":100,"maximum":8192}}),
            vec!["tabId", "width", "height"],
        ),
        (
            "wait",
            "Wait for a selector, visible text, or bounded time in the identified tab; never wait indefinitely.",
            json!({"selector":{"type":"string"},"text":{"type":"string"},"timeoutMs":{"type":"integer","minimum":1,"maximum":60000}}),
            vec!["tabId"],
        ),
        (
            "hover",
            "Move the pointer over a unique visible DOM selector without activating a window.",
            json!({"selector":{"type":"string","minLength":1,"maxLength":2048}}),
            vec!["tabId", "selector"],
        ),
        (
            "drag",
            "Drag between two unique current visible DOM selectors.",
            json!({"sourceSelector":{"type":"string"},"targetSelector":{"type":"string"}}),
            vec!["tabId", "sourceSelector", "targetSelector"],
        ),
        (
            "select_option",
            "Select values in a unique current HTML select control and dispatch input/change.",
            json!({"selector":{"type":"string"},"values":{"type":"array","maxItems":100,"items":{"type":"string"}}}),
            vec!["tabId", "selector", "values"],
        ),
        (
            "fill",
            "Fill identified editable fields with input/change events. Submitted actions need separate user authorization.",
            json!({"fields":{"type":"array","maxItems":100,"items":{"type":"object","properties":{"selector":{"type":"string"},"value":{"type":"string","maxLength":16000}},"required":["selector","value"],"additionalProperties":false}}}),
            vec!["tabId", "fields"],
        ),
        (
            "upload",
            "Set authorized local files on one current input[type=file]. Scope, sensitive-file and protected-data checks apply before upload; no native file chooser is opened.",
            json!({"selector":{"type":"string","minLength":1,"maxLength":2048},"paths":{"type":"array","maxItems":32,"items":{"type":"string","minLength":1}}}),
            vec!["tabId", "selector", "paths"],
        ),
        (
            "dialog",
            "Accept or dismiss the identified tab's actual JavaScript dialog; provide prompt text only for an actual prompt.",
            json!({"accept":{"type":"boolean"},"promptText":{"type":"string","maxLength":16000}}),
            vec!["tabId", "accept"],
        ),
        (
            "console",
            "Read retained console events from this tab, with an explicit discarded-event count; no host console is read.",
            json!({"afterSequence":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}}),
            vec!["tabId"],
        ),
        (
            "network",
            "Read retained request/response metadata, or a verified request's bounded response body; cookie and authorization headers are excluded.",
            json!({"requestId":{"type":"string"},"includeBody":{"type":"boolean"},"afterSequence":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}}),
            vec!["tabId"],
        ),
        (
            "trace_start",
            "Begin a native Chromium performance trace for this browser. It remains bounded and must be explicitly stopped; no Playwright/Node process is used.",
            json!({}),
            vec!["tabId"],
        ),
        (
            "trace_stop",
            "Stop the actual native Chromium trace and return bounded trace JSON with coverage/truncation information.",
            json!({"maxBytes":{"type":"integer","minimum":1024,"maximum":8388608}}),
            vec!["tabId"],
        ),
    ] {
        let mut properties = properties;
        properties["sessionId"] = json!({"type":"string"});
        if name != "new" {
            properties["tabId"] = json!({"type":"string"});
        }
        properties["reason"] = reason.clone();
        let mut required = required;
        required.extend(["sessionId", "reason"]);
        output.push(json!({"name":format!("browser.{name}"),"source":"builtin","description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}));
    }
    output
}

fn excluded_executable(path: &Path, intent: &Value) -> bool {
    // Windows casing aliases do not change an application's identity or the user's explicit ban.
    // Windows 大小写别名不会改变应用身份，也不能改变用户的明确禁令。
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();
    intent["excludedBrowserExecutables"]
        .as_array()
        .is_some_and(|excluded| {
            excluded
                .iter()
                .filter_map(Value::as_str)
                .any(|value| value.to_lowercase() == basename)
        })
}

fn executable(input: &Value, ctx: &GatewayToolContext) -> Result<PathBuf, GatewayError> {
    let intent=super::intent::calculate("tools.browserIntent",&json!({"message":ctx.config["trustedUserText"],"previousUserMessages":ctx.config["previousUserMessages"]})).map_err(|message|failure("TOOL_INTENT_FAILED",&message,500))?;
    let mut candidates = Vec::new();
    for value in [
        input["browserPath"].as_str(),
        ctx.config["browser"]["executablePath"].as_str(),
    ] {
        if let Some(value) = value {
            candidates.push(PathBuf::from(value));
        }
    }
    for variable in ["PROGRAMFILES(X86)", "PROGRAMFILES", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            for suffix in [
                "Microsoft/Edge/Application/msedge.exe",
                "Google/Chrome/Application/chrome.exe",
            ] {
                candidates.push(PathBuf::from(&root).join(suffix));
            }
        }
    }
    candidates
        .into_iter()
        .find(|path| {
            path.is_absolute()
                && path.is_file()
                && ["msedge.exe", "chrome.exe", "chromium.exe"].contains(
                    &path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("")
                        .to_lowercase()
                        .as_str(),
                )
                && super::local_files::inspect(path, false, false).is_ok()
                && !excluded_executable(path, &intent)
        })
        .ok_or_else(|| {
            failure(
                "BROWSER_EXECUTABLE_NOT_FOUND",
                "未找到本机 Edge/Chrome；请配置受支持浏览器的绝对路径。",
                503,
            )
        })
}

fn allowed_url(input: &str) -> Result<url::Url, GatewayError> {
    if input == "about:blank" {
        return url::Url::parse(input)
            .map_err(|_| failure("BROWSER_INVALID_URL", "浏览器地址无效。", 400));
    }
    let url = url::Url::parse(input)
        .map_err(|_| failure("BROWSER_INVALID_URL", "浏览器地址无效。", 400))?;
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(failure(
            "BROWSER_INVALID_URL",
            "浏览器只接收 HTTP(S) 或 about:blank；凭据不能放在 URL。",
            400,
        ));
    }
    Ok(url)
}

async fn http(session: &BrowserSession, action: &str) -> Result<Value, GatewayError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| failure("BROWSER_HTTP_FAILED", "浏览器客户端创建失败。", 500))?;
    let response = client
        .get(format!("{}{}", session.endpoint, action))
        .send()
        .await
        .map_err(|_| failure("BROWSER_DISCONNECTED", "浏览器 DevTools 连接已断开。", 502))?;
    if !response.status().is_success() {
        return Err(failure(
            "BROWSER_TAB_UNAVAILABLE",
            "浏览器标签页不可用。",
            409,
        ));
    }
    let mut bytes = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk =
            chunk.map_err(|_| failure("BROWSER_INVALID_RESPONSE", "浏览器回执读取失败。", 502))?;
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err(failure(
                "BROWSER_RESPONSE_TOO_LARGE",
                "浏览器标签目录过大。",
                413,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| failure("BROWSER_INVALID_RESPONSE", "浏览器回执 JSON 无效。", 502))
}

fn environment(session: &BrowserSession) -> Value {
    let mut value = super::contracts::calculate(
        "tools.resolveEnvironment",
        &json!({"descriptor":{"name":"browser.snapshot","source":"builtin"}}),
    )
    .unwrap_or(Value::Null);
    let owned = session.child.is_some();
    if !owned {
        value["operationLocation"] = json!("unknown");
    }
    value["browserTarget"] = json!({"location":if owned{"gateway-host"}else{"unknown"},"mode":if owned{"independent-browser"}else{"existing-browser"},
        "evidence":if owned{"application-verified-browser-target"}else{"trusted-browser-configuration"}});
    value
}

async fn update_tabs(session: &mut BrowserSession) -> Result<Value, GatewayError> {
    let tabs = http(session, "/json/list").await?;
    session.tabs.clear();
    let mut values = Vec::new();
    for tab in tabs.as_array().into_iter().flatten() {
        if tab["type"] != "page" {
            continue;
        }
        if let (Some(id), Some(socket)) = (tab["id"].as_str(), tab["webSocketDebuggerUrl"].as_str())
        {
            let url = url::Url::parse(socket).map_err(|_| {
                failure(
                    "BROWSER_INVALID_ENDPOINT",
                    "浏览器 websocket 地址无效。",
                    502,
                )
            })?;
            let endpoint = url::Url::parse(&session.endpoint)
                .map_err(|_| failure("BROWSER_INVALID_ENDPOINT", "浏览器端点无效。", 502))?;
            if !["ws", "wss"].contains(&url.scheme())
                || !url.username().is_empty()
                || url.password().is_some()
                || url.host_str() != endpoint.host_str()
                || url.port_or_known_default() != endpoint.port_or_known_default()
                || endpoint.scheme() == "https" && url.scheme() != "wss"
            {
                return Err(failure(
                    "BROWSER_ENDPOINT_CHANGED",
                    "浏览器调试端点身份不匹配。",
                    409,
                ));
            }
            session.tabs.insert(id.to_owned(), socket.to_owned());
            values.push(json!({"tabId":id,"url":tab["url"],"title":tab["title"]}));
        }
    }
    Ok(json!(values))
}

async fn evaluate(
    socket: &str,
    expression: String,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let value = rpc(
        socket,
        "Runtime.evaluate",
        json!({"expression":expression,"returnByValue":true,"awaitPromise":true}),
        ctx,
    )
    .await?;
    if !value["exceptionDetails"].is_null() {
        return Err(failure(
            "BROWSER_SCRIPT_FAILED",
            value["exceptionDetails"]["text"]
                .as_str()
                .unwrap_or("页面脚本执行失败。"),
            502,
        ));
    }
    Ok(value["result"]["value"].clone())
}

async fn point(
    socket: &str,
    selector: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    evaluate(socket,format!("(() => {{const elements=document.querySelectorAll({});if(elements.length!==1)throw new Error('Unique current selector required');const element=elements[0];if(!element.getClientRects().length)throw new Error('Target is not visible');element.scrollIntoView({{block:'center',inline:'center'}});const bounds=element.getBoundingClientRect();return {{x:bounds.x+bounds.width/2,y:bounds.y+bounds.height/2}};}})()",selector),ctx).await
}
fn redact_headers(mut value: Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields.iter_mut() {
                    if key.eq_ignore_ascii_case("headers") {
                        if let Some(headers) = value.as_object_mut() {
                            headers.retain(|key, _| {
                                ![
                                    "cookie",
                                    "set-cookie",
                                    "authorization",
                                    "proxy-authorization",
                                ]
                                .contains(&key.to_lowercase().as_str())
                            });
                        }
                    } else if key == "postData" {
                        *value = Value::Null;
                    } else {
                        strip(value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    strip(value);
                }
            }
            _ => {}
        }
    }
    strip(&mut value);
    value
}
async fn trace_stream(
    socket: &str,
    handle: &str,
    maximum: usize,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    use base64::Engine;
    let mut bytes = Vec::new();
    let mut truncated = false;
    loop {
        let response = rpc(
            socket,
            "IO.read",
            json!({"handle":handle,"size":65536}),
            ctx,
        )
        .await?;
        let data = response["data"].as_str().unwrap_or("");
        let chunk = if response["base64Encoded"] == true {
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| failure("BROWSER_TRACE_INVALID", "跟踪流编码无效。", 502))?
        } else {
            data.as_bytes().to_vec()
        };
        let remaining = maximum.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
        if chunk.len() > remaining {
            truncated = true;
            break;
        }
        if response["eof"] == true {
            break;
        }
        if chunk.is_empty() {
            return Err(failure(
                "BROWSER_TRACE_EXECUTION_UNKNOWN",
                "跟踪流未取得进展，完成状态尚未确认。",
                502,
            ));
        }
    }
    Ok(
        json!({"trace":String::from_utf8_lossy(&bytes),"bytes":bytes.len(),"format":"chrome-trace-json","complete":!truncated,"truncated":truncated}),
    )
}
fn clean_upload(directory: &Path, ctx: &GatewayToolContext) {
    let root = ctx
        .data_home
        .join("Browser/Uploads")
        .join(&ctx.conversation_id);
    if directory != root
        && super::local_files::within(&root, directory)
        && super::local_files::inspect(directory, false, false).is_ok()
    {
        let _ = std::fs::remove_dir_all(directory);
    }
}
async fn stage_upload(
    input: &Value,
    ctx: &GatewayToolContext,
    resident: u64,
) -> Result<(PathBuf, Vec<String>, u64), GatewayError> {
    let mut targets = Vec::new();
    for path in input["paths"].as_array().into_iter().flatten() {
        let (path, outside) =
            super::gateway::canonical_authorized(ctx, path.as_str().unwrap_or(""), false)?;
        if super::gateway::protected_path(ctx, &path) {
            return Err(failure(
                "PROTECTED_APP_DATA",
                "正式聊天、凭据、扩展不能作为网页上传材料。",
                403,
            ));
        }
        if (outside
            || super::service_skills::private_resource(&super::local_files::path_text(&path)))
            && !ctx.approved
            && ctx.permission_mode != "full"
        {
            return Err(failure(
                "BROWSER_UPLOAD_APPROVAL_REQUIRED",
                "本次上传文件需要明确审批。",
                403,
            ));
        }
        targets.push(path);
    }
    let directory = ctx
        .data_home
        .join("Browser/Uploads")
        .join(&ctx.conversation_id)
        .join(uuid::Uuid::new_v4().to_string());
    let created = directory.clone();
    let mut cancellation = ctx.cancellation.clone();
    let maximum = ctx.config["browser"]["uploadCacheLimitBytes"]
        .as_u64()
        .unwrap_or(2 * 1024 * 1024 * 1024)
        .clamp(1024 * 1024, 16 * 1024 * 1024 * 1024);
    // Copy from stable verified handles once; Chromium's delayed form submission must not reopen a changed source path.
    // 从稳定核验句柄复制一次；Chromium 延后提交表单时不能重新读取已改变的源路径。
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        super::local_files::inspect(&created, true, false)
            .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
        std::fs::create_dir_all(&created)?;
        let mut files = Vec::new();
        let mut total = 0u64;
        for (index, path) in targets.iter().enumerate() {
            let metadata = super::local_files::inspect(path, false, false)
                .map_err(|error| failure(&error.code, &error.message, error.status_code))?
                .filter(|metadata| metadata.is_file())
                .ok_or_else(|| {
                    failure("BROWSER_UPLOAD_INVALID_FILE", "上传目标不是普通文件。", 400)
                })?;
            let mut source = super::local_files::open_read(path)
                .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
            let before = super::local_files::handle_version(&source, &source.metadata()?)
                .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
            let expected = super::local_files::file_version(path, &metadata)
                .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
            if before != expected {
                return Err(failure(
                    "BROWSER_UPLOAD_SOURCE_CHANGED",
                    "上传文件在打开时已改变。",
                    409,
                ));
            }
            if resident.saturating_add(total).saturating_add(before.size) > maximum {
                return Err(failure(
                    "BROWSER_UPLOAD_CACHE_LIMIT",
                    "本浏览器上传快照超出容量，请关闭会话回收后再上传。",
                    413,
                ));
            }
            let folder = created.join(index.to_string());
            std::fs::create_dir(&folder)?;
            let target = folder.join(
                path.file_name()
                    .ok_or_else(|| failure("BROWSER_UPLOAD_INVALID_FILE", "文件名无效。", 400))?,
            );
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            let mut buffer = [0u8; 65536];
            let mut size = 0u64;
            loop {
                if *cancellation.borrow_and_update() {
                    return Err(failure("TOOL_CANCELLED", "上传快照准备已取消。", 499));
                }
                let read = source.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                size += read as u64;
                if size > before.size {
                    return Err(failure(
                        "BROWSER_UPLOAD_SOURCE_CHANGED",
                        "上传文件在读取时已改变。",
                        409,
                    ));
                }
                output.write_all(&buffer[..read])?;
            }
            output.sync_all()?;
            let after = super::local_files::handle_version(&source, &source.metadata()?)
                .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
            if before != after || size != before.size {
                return Err(failure(
                    "BROWSER_UPLOAD_SOURCE_CHANGED",
                    "上传文件在读取时已改变。",
                    409,
                ));
            }
            total += size;
            files.push(super::local_files::path_text(&target));
        }
        Ok::<_, GatewayError>((files, total))
    })
    .await
    .map_err(|_| {
        failure(
            "BROWSER_UPLOAD_WORKER_FAILED",
            "上传快照 worker 退出。",
            500,
        )
    })?;
    match result {
        Ok((files, bytes)) => Ok((directory, files, bytes)),
        Err(error) => {
            clean_upload(&directory, ctx);
            Err(error)
        }
    }
}

pub(crate) async fn execute(
    name: &str,
    input: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    if name == "browser.open" {
        if let Some(endpoint) = input["cdpEndpoint"].as_str() {
            let endpoint = url::Url::parse(endpoint).map_err(|_| {
                failure(
                    "BROWSER_INVALID_ENDPOINT",
                    "请提供实际 HTTP(S) DevTools 地址。",
                    400,
                )
            })?;
            if !["http", "https"].contains(&endpoint.scheme())
                || endpoint.host_str().is_none()
                || !endpoint.username().is_empty()
                || endpoint.password().is_some()
                || endpoint.path() != "/"
                || endpoint.query().is_some()
            {
                return Err(failure(
                    "BROWSER_INVALID_ENDPOINT",
                    "DevTools 地址须为无凭据的 HTTP(S) 服务根路径。",
                    400,
                ));
            }
            let mut session = BrowserSession {
                child: None,
                endpoint: endpoint.as_str().trim_end_matches('/').to_owned(),
                profile: PathBuf::new(),
                conversation_id: ctx.conversation_id.clone(),
                tabs: HashMap::new(),
                active_tab: None,
                uploads: Vec::new(),
                upload_bytes: 0,
                _owned: None,
                closed: false,
                profile_cleanup_pending: false,
                profile_cleanup_error: None,
            };
            let version = http(&session, "/json/version").await?;
            let intent = super::intent::calculate("tools.browserIntent", &json!({"message":ctx.config["trustedUserText"],"previousUserMessages":ctx.config["previousUserMessages"]}))
                .map_err(|message| failure("TOOL_INTENT_FAILED", &message, 500))?;
            let product = version["Browser"].as_str().unwrap_or("").to_lowercase();
            if intent["excludedBrowserExecutables"]
                .as_array()
                .is_some_and(|bans| {
                    bans.iter().any(|ban| {
                        ban == "chrome.exe" && product.starts_with("chrome/")
                            || ban == "msedge.exe"
                                && (product.contains("edge/") || product.contains("edg/"))
                    })
                })
            {
                return Err(failure(
                    "BROWSER_IDENTITY_FORBIDDEN",
                    "此 DevTools 服务属于用户明确禁用的浏览器。",
                    403,
                ));
            }
            let tabs = update_tabs(&mut session).await?;
            session.active_tab = tabs[0]["tabId"].as_str().map(str::to_owned);
            let id = uuid::Uuid::new_v4().to_string();
            let active = session.active_tab.clone();
            let mut sessions = ctx.runtime.browsers.lock().await;
            if sessions.len() >= 16 {
                return Err(failure(
                    "BROWSER_SESSION_CAPACITY",
                    "浏览器会话容量已满。",
                    429,
                ));
            }
            sessions.insert(id.clone(), Arc::new(Mutex::new(session)));
            let execution_environment = environment(&*sessions.get(&id).unwrap().lock().await);
            return Ok(
                json!({"sessionId":id,"tabId":active,"tabs":tabs,"profile":"explicit-existing-endpoint","loginVerified":false,"processOwned":false,"executor":"native-cdp","executionEnvironment":execution_environment}),
            );
        }
        let path = executable(&input, ctx)?;
        let forbidden=super::intent::calculate("tools.foregroundForbidden",&json!({"message":ctx.config["trustedUserText"],"target":{"kind":"browser","appPath":path}})).map_err(|message|failure("TOOL_INTENT_FAILED",&message,500))?;
        if input["foreground"] == true && forbidden == true {
            return Err(failure(
                "BROWSER_FOREGROUND_FORBIDDEN",
                "用户明确要求后台，未启动前台浏览器。",
                403,
            ));
        }
        let url = allowed_url(input["url"].as_str().unwrap_or("about:blank"))?;
        let id = uuid::Uuid::new_v4().to_string();
        let profile = ctx.data_home.join("Browser/Native").join(&id);
        tokio::fs::create_dir_all(&profile).await?;
        let mut command = Command::new(path);
        hide(&mut command);
        command
            .args([
                "--remote-debugging-port=0",
                "--remote-debugging-address=127.0.0.1",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-extensions",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if input["foreground"] != true {
            command.arg("--headless=new");
        }
        command.arg(url.as_str());
        let mut child = command
            .spawn()
            .map_err(|_| failure("BROWSER_START_FAILED", "浏览器原生启动失败。", 503))?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let owned = match OwnedProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                terminate(&mut child).await;
                return Err(error);
            }
        };
        let port;
        loop {
            if *ctx.cancellation.borrow() {
                terminate(&mut child).await;
                return Err(failure("TOOL_CANCELLED", "浏览器启动已取消。", 409));
            }
            if !owned.has_running_processes()? {
                return Err(failure("BROWSER_START_FAILED", "浏览器在就绪前退出。", 503));
            }
            if let Ok(value) = tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await {
                if let Some(value) = value
                    .lines()
                    .next()
                    .and_then(|value| value.parse::<u16>().ok())
                    .filter(|port| *port != 0)
                {
                    port = value;
                    break;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                terminate(&mut child).await;
                return Err(failure(
                    "BROWSER_START_TIMEOUT",
                    "浏览器调试端口启动超时。",
                    504,
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut session = BrowserSession {
            child: Some(child),
            endpoint: format!("http://127.0.0.1:{port}"),
            profile,
            conversation_id: ctx.conversation_id.clone(),
            tabs: HashMap::new(),
            active_tab: None,
            uploads: Vec::new(),
            upload_bytes: 0,
            _owned: Some(owned),
            closed: false,
            profile_cleanup_pending: false,
            profile_cleanup_error: None,
        };
        let tabs = update_tabs(&mut session).await?;
        session.active_tab = session.tabs.keys().next().cloned();
        let active = session.active_tab.clone();
        let mut sessions = ctx.runtime.browsers.lock().await;
        if sessions.len() >= 16 {
            if let Some(child) = &mut session.child {
                terminate(child).await;
            }
            return Err(failure(
                "BROWSER_SESSION_CAPACITY",
                "浏览器会话已达到16个，未启动更多常驻进程。",
                429,
            ));
        }
        sessions.insert(id.clone(), Arc::new(Mutex::new(session)));
        let execution_environment = environment(&*sessions.get(&id).unwrap().lock().await);
        return Ok(
            json!({"sessionId":id,"tabId":active,"tabs":tabs,"foreground":input["foreground"]==true,"profile":"isolated","executor":"native-cdp","executionEnvironment":execution_environment}),
        );
    }
    let id = input["sessionId"].as_str().unwrap_or("");
    let handle = ctx
        .runtime
        .browsers
        .lock()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| failure("BROWSER_SESSION_NOT_FOUND", "此浏览器会话不存在。", 404))?;
    let mut session = handle.lock().await;
    if session.conversation_id != ctx.conversation_id {
        return Err(failure(
            "BROWSER_SESSION_NOT_FOUND",
            "此浏览器会话不属于当前聊天。",
            404,
        ));
    }
    if name == "browser.close" && input["tabId"].is_null() {
        let owned = session.child.is_some();
        let execution_environment = environment(&session);
        if session.closed && session.profile_cleanup_pending {
            return Ok(json!({"closed":true,"sessionId":id,"profileCleanupPending":true,
                "profileCleanupError":session.profile_cleanup_error.as_ref().map(GatewayError::response),"externalProcessPreserved":!owned,"executionEnvironment":execution_environment}));
        }
        for socket in session.tabs.values() {
            let connected = ctx
                .runtime
                .cdp
                .lock()
                .await
                .remove(&connection_key(socket, ctx));
            if let Some(connected) = connected {
                connected.close().await?;
            }
        }
        session.close_owned_process().await?;
        session.closed = true;
        for upload in &session.uploads {
            clean_upload(upload, ctx);
        }
        if let Err(error) = session.remove_owned_profile().await {
            if error.code != "BROWSER_PROFILE_CLEANUP_FAILED" {
                return Err(error);
            }
            session.profile_cleanup_pending = true;
            session.profile_cleanup_error = Some(error.clone());
            drop(session);
            defer_owned_profile_cleanup(id.to_owned(), handle, &ctx.runtime);
            return Ok(json!({"closed":true,"sessionId":id,"profileCleanupPending":true,
                "profileCleanupError":error.response(),"externalProcessPreserved":!owned,"executionEnvironment":execution_environment}));
        }
        drop(session);
        ctx.runtime.browsers.lock().await.remove(id);
        return Ok(
            json!({"closed":true,"sessionId":id,"profileCleanupPending":false,"externalProcessPreserved":!owned,"executionEnvironment":execution_environment}),
        );
    }
    if session.closed {
        return Err(failure("BROWSER_SESSION_CLOSED", "浏览器已关闭；配置清理状态可通过 browser.close 查看 / Browser is closed; inspect browser.close for profile cleanup status.", 409));
    }
    if let Some(job) = &session._owned {
        if !job.has_running_processes()? {
            return Err(failure("BROWSER_DISCONNECTED", "浏览器进程已退出。", 409));
        }
    }
    let tabs = update_tabs(&mut session).await?;
    if name == "browser.tabs" {
        return Ok(
            json!({"sessionId":id,"tabs":tabs,"activeTabId":session.active_tab,"executionEnvironment":environment(&session)}),
        );
    }
    if name == "browser.new" {
        let current = session
            .tabs
            .values()
            .next()
            .ok_or_else(|| failure("BROWSER_TAB_UNAVAILABLE", "当前没有可用标签页。", 409))?;
        let url = allowed_url(input["url"].as_str().unwrap_or("about:blank"))?;
        let created = rpc(
            current,
            "Target.createTarget",
            json!({"url":url.as_str(),"background":true}),
            ctx,
        )
        .await?;
        let tabs = update_tabs(&mut session).await?;
        session.active_tab = created["targetId"].as_str().map(str::to_owned);
        return Ok(
            json!({"sessionId":id,"tabId":created["targetId"],"tabs":tabs,"foreground":false,"executionEnvironment":environment(&session)}),
        );
    }
    let tab = input["tabId"]
        .as_str()
        .ok_or_else(|| failure("BROWSER_TAB_REQUIRED", "必须指定实际返回的 tabId。", 400))?;
    let socket = session
        .tabs
        .get(tab)
        .cloned()
        .ok_or_else(|| failure("BROWSER_TAB_UNAVAILABLE", "当前浏览器没有此标签页。", 409))?;
    let value = match name {
        "browser.select" => {
            if input["foreground"] == true {
                let interaction = super::intent::calculate("tools.browserInteraction", &json!({"message":ctx.config["trustedUserText"]}))
                    .map_err(|message| failure("TOOL_INTENT_FAILED", &message, 500))?;
                if interaction["background"] != false { return Err(failure("BROWSER_FOREGROUND_FORBIDDEN", "用户没有要求激活前台；可在后台选择此标签。", 403)); }
                rpc(&socket,"Target.activateTarget",json!({"targetId":tab}),ctx).await?;
            }
            json!({"selected":true,"foreground":input["foreground"]==true})
        }
        "browser.back" | "browser.forward" => {
            let history=rpc(&socket,"Page.getNavigationHistory",json!({}),ctx).await?;
            let index=history["currentIndex"].as_i64().unwrap_or(0)+if name.ends_with("back"){-1}else{1};
            let entry=index.try_into().ok().and_then(|index:usize|history["entries"].get(index));
            let Some(entry)=entry else {return Err(failure("BROWSER_HISTORY_BOUNDARY","没有该方向的历史记录。",409));};
            rpc(&socket,"Page.navigateToHistoryEntry",json!({"entryId":entry["id"]}),ctx).await?
        }
        "browser.reload" => rpc(&socket,"Page.reload",json!({}),ctx).await?,
        "browser.resize" => rpc(&socket,"Emulation.setDeviceMetricsOverride",json!({"width":input["width"],"height":input["height"],"deviceScaleFactor":1,"mobile":false}),ctx).await?,
        "browser.wait" => {
            let selector=serde_json::to_string(&input["selector"].as_str().unwrap_or("")).unwrap();
            let text=serde_json::to_string(&input["text"].as_str().unwrap_or("")).unwrap();
            let timeout=input["timeoutMs"].as_u64().unwrap_or(5000).clamp(1,60000);
            let deadline=tokio::time::Instant::now()+Duration::from_millis(timeout);
            loop {
                check_cancelled(ctx)?;
                let ready=evaluate(&socket,format!("(() => {{const selector={selector},text={text};return (!selector||!!document.querySelector(selector))&&(!text||(document.body?.innerText||'').includes(text));}})()"),ctx).await?;
                if ready==true && (!input["selector"].is_null() || !input["text"].is_null()) {break json!({"ready":true});}
                if tokio::time::Instant::now()>=deadline {if input["selector"].is_null()&&input["text"].is_null(){break json!({"waitedMs":timeout});}return Err(failure("BROWSER_WAIT_TIMEOUT","等待的页面状态尚未出现。",504));}
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        "browser.hover" | "browser.drag" => {
            let source=point(&socket,if name.ends_with("hover"){&input["selector"]}else{&input["sourceSelector"]},ctx).await?;
            rpc(&socket,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":source["x"],"y":source["y"]}),ctx).await?;
            if name.ends_with("drag") {
                let target=point(&socket,&input["targetSelector"],ctx).await?;
                rpc(&socket,"Input.dispatchMouseEvent",json!({"type":"mousePressed","button":"left","clickCount":1,"x":source["x"],"y":source["y"]}),ctx).await?;
                rpc(&socket,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","buttons":1,"x":target["x"],"y":target["y"]}),ctx).await?;
                rpc(&socket,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","button":"left","clickCount":1,"x":target["x"],"y":target["y"]}),ctx).await?;
            }
            json!({"delivered":true})
        }
        "browser.select_option" => {
            let selector=serde_json::to_string(&input["selector"]).unwrap(); let values=input["values"].to_string();
            evaluate(&socket,format!("(() => {{const found=document.querySelectorAll({selector});if(found.length!==1||!(found[0] instanceof HTMLSelectElement))throw new Error('Unique select required');const element=found[0],values={values};if(values.some(value=>!Array.from(element.options).some(option=>option.value===value)))throw new Error('Requested option not found');if(!element.multiple&&values.length>1)throw new Error('Select is not multiple');for(const option of element.options)option.selected=values.includes(option.value);element.dispatchEvent(new Event('input',{{bubbles:true}}));element.dispatchEvent(new Event('change',{{bubbles:true}}));return {{selected:Array.from(element.selectedOptions).map(option=>option.value)}};}})()"),ctx).await?
        }
        "browser.fill" => {
            evaluate(&socket,format!("(() => {{const fields={};const targets=fields.map(field=>{{const found=document.querySelectorAll(field.selector);if(found.length!==1||!found[0].getClientRects().length)throw new Error('Unique visible field required');const e=found[0];if(!(e instanceof HTMLInputElement)&&!(e instanceof HTMLTextAreaElement)&&!e.isContentEditable)throw new Error('Field is not editable');return [e,field.value];}});for(const [e,value] of targets){{const setter=Object.getOwnPropertyDescriptor(e instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value')?.set;if(setter)setter.call(e,value);else e.textContent=value;e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));}}return {{filled:targets.length}};}})()",input["fields"]),ctx).await?
        }
        "browser.upload" => {
            let selector=serde_json::to_string(&input["selector"]).unwrap();
            let valid=evaluate(&socket,format!("(() => {{const items=document.querySelectorAll({selector});return items.length===1&&items[0] instanceof HTMLInputElement&&items[0].type==='file';}})()"),ctx).await?;
            if valid!=true{return Err(failure("BROWSER_UPLOAD_TARGET_INVALID","请选择唯一的当前文件上传控件。",400));}
            let (directory,files,bytes)=stage_upload(&input,ctx,session.upload_bytes).await?;
            session.uploads.push(directory); session.upload_bytes+=bytes;
            let document=rpc(&socket,"DOM.getDocument",json!({"depth":0}),ctx).await?;
            let nodes=rpc(&socket,"DOM.querySelectorAll",json!({"nodeId":document["root"]["nodeId"],"selector":input["selector"]}),ctx).await?;
            if nodes["nodeIds"].as_array().is_none_or(|nodes|nodes.len()!=1){return Err(failure("BROWSER_UPLOAD_TARGET_CHANGED","上传控件已经改变，请重新读取当前页面。",409));}
            rpc(&socket,"DOM.setFileInputFiles",json!({"nodeId":nodes["nodeIds"][0],"files":files}),ctx).await?;
            json!({"attachedFiles":input["paths"].as_array().map_or(0,Vec::len),"bytes":bytes,"submitted":false})
        }
        "browser.dialog" => rpc(&socket,"Page.handleJavaScriptDialog",json!({"accept":input["accept"],"promptText":input["promptText"].as_str().unwrap_or("")}),ctx).await?,
        "browser.console" | "browser.network" => {
            let connected=client(&socket,ctx).await?; let events=connected.events.lock().await;
            let after=input["afterSequence"].as_u64().unwrap_or(0); let limit=input["limit"].as_u64().unwrap_or(100).clamp(1,200) as usize;
            let prefix=if name.ends_with("console"){"Runtime.consoleAPICalled"}else{"Network."};
            let records=events.records.iter().filter(|event|event["sequence"].as_u64().unwrap_or(0)>after&&event["method"].as_str().is_some_and(|method|method.starts_with(prefix)))
                .filter(|event|input["requestId"].is_null()||event["params"]["requestId"]==input["requestId"]).take(limit).cloned().map(redact_headers).collect::<Vec<_>>();
            let mut result=json!({"events":records,"discardedEvents":events.discarded,"nextSequence":records.last().and_then(|value|value["sequence"].as_u64()).unwrap_or(after),"coverage":"since-native-connection"}); drop(events);
            if name.ends_with("network")&&input["includeBody"]==true {
                if input["requestId"].as_str().is_none_or(|id|id.is_empty())||records.is_empty(){return Err(failure("BROWSER_REQUEST_UNVERIFIED","请指定该标签已观察到的 requestId。",409));}
                let body=rpc(&socket,"Network.getResponseBody",json!({"requestId":input["requestId"]}),ctx).await?;
                let text=body["body"].as_str().unwrap_or("");result["body"]=json!(text.chars().take(262144).collect::<String>());result["bodyTruncated"]=json!(text.chars().count()>262144);result["base64Encoded"]=body["base64Encoded"].clone();
            }
            result
        }
        "browser.trace_start" => rpc(&socket,"Tracing.start",json!({"traceConfig":{"recordMode":"recordUntilFull","traceBufferSizeInKb":8192,"includedCategories":["devtools.timeline","v8.execute","blink.user_timing"]},"bufferUsageReportingInterval":1000,"transferMode":"ReturnAsStream"}),ctx).await?,
        "browser.trace_stop" => {
            let connected=client(&socket,ctx).await?; let sequence=connected.events.lock().await.sequence;
            rpc(&socket,"Tracing.end",json!({}),ctx).await?;
            let deadline=tokio::time::Instant::now()+Duration::from_secs(30);
            let handle=loop {
                check_cancelled(ctx)?; let completed=connected.events.lock().await.records.iter().find(|event|event["sequence"].as_u64().unwrap_or(0)>sequence&&event["method"]=="Tracing.tracingComplete").cloned();
                if let Some(handle)=completed.and_then(|event|event["params"]["stream"].as_str().map(str::to_owned)){break handle;}
                if tokio::time::Instant::now()>deadline{return Err(failure("BROWSER_TRACE_EXECUTION_UNKNOWN","跟踪停止已发出，完成状态尚未确认。",504));}
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let maximum=input["maxBytes"].as_u64().unwrap_or(8*1024*1024).clamp(1024,8*1024*1024) as usize;
            let result=trace_stream(&socket,&handle,maximum,ctx).await;
            let _=rpc(&socket,"IO.close",json!({"handle":handle}),ctx).await; result?
        }
        "browser.navigate" => {
            let url = allowed_url(input["url"].as_str().unwrap_or(""))?;
            rpc(&socket, "Page.navigate", json!({"url":url.as_str()}), ctx).await?
        }
        "browser.snapshot" => {
            let maximum = input["maxChars"].as_u64().unwrap_or(32000).clamp(1, 64000);
            evaluate(&socket,format!(r#"(() => {{const max={maximum};const text=(document.body?.innerText||'').slice(0,max);const elements=Array.from(document.querySelectorAll('a,button,input,select,textarea,[role="button"]')).slice(0,1000).filter(e=>e.getClientRects().length).map((e,i)=>({{ref:i,tag:e.tagName.toLowerCase(),name:(e.getAttribute('aria-label')||e.innerText||e.getAttribute('placeholder')||'').slice(0,200),id:e.id,href:e.href||null,password:e.type==='password',value:e.type==='password'?null:e.value||null}}));return {{title:document.title,url:location.href,text,totalChars:(document.body?.innerText||'').length,truncated:(document.body?.innerText||'').length>max,elements}};}})()"#),ctx).await?
        }
        "browser.screenshot" => {
            let result=rpc(&socket,"Page.captureScreenshot",json!({"format":"png","captureBeyondViewport":input["fullPage"]==true,"fromSurface":true}),ctx).await?;
            if !result["data"].is_string() {
                return Err(failure("BROWSER_CAPTURE_FAILED", "浏览器未返回截图。", 502));
            }
            json!({"content":[{"type":"image","mimeType":"image/png","data":result["data"]}],"structuredContent":{"sessionId":id,"tabId":tab}})
        }
        "browser.click" | "browser.type" => {
            let selector = serde_json::to_string(&input["selector"]).unwrap_or_default();
            let action = if name.ends_with("click") {
                "e.click();".to_owned()
            } else {
                let text = serde_json::to_string(&input["text"]).unwrap_or_default();
                format!(
                    "e.focus(); const setter=Object.getOwnPropertyDescriptor(e instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value')?.set; if(setter)setter.call(e,{text});else if(e.isContentEditable)e.textContent={text};else throw new Error('Target is not editable'); e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));"
                )
            };
            evaluate(&socket,format!("(() => {{const matches=document.querySelectorAll({selector});if(matches.length!==1)throw new Error('Selector must match exactly one current element');const e=matches[0];if(!e.getClientRects().length)throw new Error('Target is not visible');{action}return {{delivered:true,url:location.href}};}})()"),ctx).await?
        }
        "browser.key" => {
            let key = input["key"].as_str().unwrap_or("");
            let pieces = key.split('+').collect::<Vec<_>>();
            let name = pieces.last().copied().unwrap_or("");
            let modifiers = pieces.iter().fold(0, |value, piece| {
                value
                    | match *piece {
                        "Alt" => 1,
                        "Control" | "Ctrl" => 2,
                        "Meta" => 4,
                        "Shift" => 8,
                        _ => 0,
                    }
            });
            rpc(
                &socket,
                "Input.dispatchKeyEvent",
                json!({"type":"keyDown","key":name,"modifiers":modifiers}),
                ctx,
            )
            .await?;
            rpc(
                &socket,
                "Input.dispatchKeyEvent",
                json!({"type":"keyUp","key":name,"modifiers":modifiers}),
                ctx,
            )
            .await?
        }
        "browser.evaluate" => {
            let interaction=super::intent::calculate("tools.browserInteraction",&json!({"message":ctx.config["trustedUserText"]}))
                .map_err(|message|failure("TOOL_INTENT_FAILED",&message,500))?;
            if interaction["allowForeground"]==false {
                let source=input["script"].as_str().unwrap_or("").to_owned();
                let requests=tokio::task::spawn_blocking(move||super::service_browser_script::script_requests_foreground(&source)).await
                    .map_err(|_|failure("BROWSER_SCRIPT_PARSER_UNAVAILABLE","浏览器脚本解析 worker 退出。",500))?
                    .map_err(|code|failure(&code,"浏览器脚本语法校验失败。",400))?;
                if requests{return Err(failure("BROWSER_FOREGROUND_FORBIDDEN","后台要求不能通过脚本焦点或窗口操作绕过。",403));}
            }
            evaluate(
                &socket,
                input["script"].as_str().unwrap_or("").to_owned(),
                ctx,
            )
            .await?
        }
        "browser.close" => {
            let response = reqwest::Client::new()
                .get(format!("{}/json/close/{tab}", session.endpoint))
                .send()
                .await;
            session.tabs.remove(tab);
            ctx.runtime.cdp.lock().await.remove(&connection_key(&socket,ctx));
            json!({"closed":response.is_ok_and(|response|response.status().is_success()),"tabId":tab})
        }
        _ => return Err(failure("BROWSER_TOOL_NOT_FOUND", "浏览器操作不存在。", 404)),
    };
    session.active_tab = Some(tab.to_owned());
    Ok(
        json!({"sessionId":id,"tabId":tab,"value":value,"executionEnvironment":environment(&session)}),
    )
}

pub async fn shutdown(ctx: &GatewayToolContext) {
    let connections = ctx
        .runtime
        .cdp
        .lock()
        .await
        .drain()
        .map(|(_, connection)| connection)
        .collect::<Vec<_>>();
    for connection in connections {
        let _ = connection.close().await;
    }
    let sessions = ctx
        .runtime
        .browsers
        .lock()
        .await
        .drain()
        .map(|(_, session)| session)
        .collect::<Vec<_>>();
    for session in sessions {
        let mut session = session.lock().await;
        let _ = session.close_owned_process().await;
        for upload in &session.uploads {
            clean_upload(upload, ctx);
        }
        let _ = session.remove_owned_profile().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_buffers_are_bounded_and_private_headers_are_not_projected() {
        let mut events = EventBuffer::default();
        for index in 0..3000 {
            events.push(json!({"method":"Network.requestWillBeSent","params":{"requestId":index,"request":{"headers":{"Cookie":"secret","Authorization":"secret","Accept":"text/html"},"postData":"private"}}}));
        }
        assert!(events.records.len() <= 2048);
        assert!(events.bytes <= 8 * 1024 * 1024);
        assert_eq!(events.discarded, 952);
        let value = redact_headers(events.records.back().unwrap().clone());
        assert!(value["params"]["request"]["headers"]["Cookie"].is_null());
        assert!(value["params"]["request"]["headers"]["Authorization"].is_null());
        assert_eq!(value["params"]["request"]["headers"]["Accept"], "text/html");
        assert!(value["params"]["request"]["postData"].is_null());
    }
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owned_headless_cdp_preserves_forms_upload_dialog_trace_and_existing_endpoint() {
        use axum::{Router, response::Html, routing::get};
        let (mut ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.approved = true;
        ctx.config["trustedUserText"] = json!("在独立后台浏览器验证本地测试页面，不激活桌面窗口。");
        std::fs::write(ctx.workdir.join("fixture.txt"), "native upload bytes").unwrap();
        let service=Router::new().route("/native",get(||async{Html("<html><title>Native fixture</title><body><h1 id='title'>Native fixture</h1><input id='text'><input id='file' type='file'><select id='option'><option value='a'>A</option><option value='b'>B</option></select><button id='button'>Button</button></body></html>")}))
            .route("/other",get(||async{Html("<h1 id='other'>Other page</h1>")}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let opened=execute("browser.open",json!({"url":format!("http://{address}/native"),"reason":"Owned headless native regression"}),&ctx).await.unwrap();
        let session = opened["sessionId"].as_str().unwrap().to_owned();
        let tab = opened["tabId"].as_str().unwrap().to_owned();
        async fn call(
            name: &str,
            mut value: Value,
            session: &str,
            tab: &str,
            ctx: &GatewayToolContext,
        ) -> Value {
            value["sessionId"] = json!(session);
            value["tabId"] = json!(tab);
            value["reason"] = json!("Owned local regression");
            execute(name, value, ctx).await.unwrap()
        }
        call(
            "browser.wait",
            json!({"selector":"#title","timeoutMs":10000}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        let snapshot = call("browser.snapshot", json!({}), &session, &tab, &ctx).await;
        assert!(
            snapshot["value"]["text"]
                .as_str()
                .unwrap()
                .contains("Native fixture")
        );
        call(
            "browser.fill",
            json!({"fields":[{"selector":"#text","value":"native typed"}]}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        call(
            "browser.select_option",
            json!({"selector":"#option","values":["b"]}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        call(
            "browser.hover",
            json!({"selector":"#button"}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        call(
            "browser.upload",
            json!({"selector":"#file","paths":["fixture.txt"]}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        let values=call("browser.evaluate",json!({"script":"({text:document.querySelector('#text').value,option:document.querySelector('#option').value,file:document.querySelector('#file').files[0].name})"}),&session,&tab,&ctx).await;
        assert_eq!(values["value"]["text"], "native typed");
        assert_eq!(values["value"]["option"], "b");
        assert_eq!(values["value"]["file"], "fixture.txt");
        call(
            "browser.evaluate",
            json!({"script":"console.log('native console');true"}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        let console = call("browser.console", json!({}), &session, &tab, &ctx).await;
        assert!(console.to_string().contains("native console"));
        call(
            "browser.evaluate",
            json!({"script":"setTimeout(()=>alert('native dialog'),0);true"}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        call(
            "browser.dialog",
            json!({"accept":true}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        call("browser.trace_start", json!({}), &session, &tab, &ctx).await;
        call("browser.evaluate",json!({"script":"performance.mark('native-trace');for(let i=0;i<20000;i++)Math.sqrt(i);true"}),&session,&tab,&ctx).await;
        let trace = call("browser.trace_stop", json!({}), &session, &tab, &ctx).await;
        assert_eq!(trace["value"]["format"], "chrome-trace-json");
        assert!(trace["value"]["bytes"].as_u64().unwrap() > 0);
        call(
            "browser.navigate",
            json!({"url":format!("http://{address}/other")}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        call(
            "browser.wait",
            json!({"selector":"#other","timeoutMs":10000}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        let network = call("browser.network", json!({}), &session, &tab, &ctx).await;
        assert!(
            network["value"]["events"]
                .as_array()
                .is_some_and(|events| !events.is_empty())
        );
        call("browser.back", json!({}), &session, &tab, &ctx).await;
        call(
            "browser.wait",
            json!({"selector":"#title","timeoutMs":10000}),
            &session,
            &tab,
            &ctx,
        )
        .await;
        let handle = ctx
            .runtime
            .browsers
            .lock()
            .await
            .get(&session)
            .unwrap()
            .clone();
        let endpoint = handle.lock().await.endpoint.clone();
        let attached = execute(
            "browser.open",
            json!({"cdpEndpoint":endpoint,"reason":"Explicit fixture endpoint"}),
            &ctx,
        )
        .await
        .unwrap();
        execute(
            "browser.close",
            json!({"sessionId":attached["sessionId"],"reason":"Disconnect fixture endpoint"}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            handle
                .lock()
                .await
                ._owned
                .as_ref()
                .unwrap()
                .has_running_processes()
                .unwrap()
        );
        let profile = handle.lock().await.profile.clone();
        use std::os::windows::fs::OpenOptionsExt;
        let locked = std::fs::OpenOptions::new().create(true).write(true).share_mode(0)
            .open(profile.join("owned-cleanup-fixture.lock")).unwrap();
        let closed = execute(
            "browser.close",
            json!({"sessionId":session,"reason":"Close owned fixture"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(closed["closed"], true);
        assert_eq!(closed["profileCleanupPending"], true);
        assert!(!handle.lock().await._owned.as_ref().unwrap().has_running_processes().unwrap());
        drop(locked);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(35);
        loop {
            let state = handle.lock().await;
            if !state.profile_cleanup_pending {
                assert!(state.profile_cleanup_error.is_none(), "{:?}", state.profile_cleanup_error);
                break;
            }
            drop(state);
            assert!(tokio::time::Instant::now() < deadline, "Owned profile cleanup did not complete");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(!profile.exists());
        drop(handle);
        shutdown(&ctx).await;
        server.abort();
        let _ = server.await;
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn page_url_never_accepts_a_host_file_or_credential_url() {
        assert!(allowed_url("https://example.org").is_ok());
        assert!(allowed_url("about:blank").is_ok());
        assert!(allowed_url("file:///C:/secret").is_err());
        assert!(allowed_url("https://user:secret@example.org").is_err());
        let bans = json!({"excludedBrowserExecutables":["chrome.exe","msedge.exe"]});
        for name in ["CHROME.EXE", "Chrome.Exe", "MSEDGE.EXE", "MsEdge.Exe"] {
            assert!(excluded_executable(Path::new(name), &bans));
        }
        assert!(!excluded_executable(Path::new("chromium.exe"), &bans));
        assert!(excluded_executable(
            Path::new("chrome.exe"),
            &json!({"excludedBrowserExecutables":["CHROME.EXE"]})
        ));
    }
}
