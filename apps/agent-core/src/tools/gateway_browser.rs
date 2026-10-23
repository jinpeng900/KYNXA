//! Chromium DevTools automation, with an owned isolated profile and explicit tab identity.
//! Chromium DevTools 自动化：独立配置目录、自有进程与明确标签页身份。

use super::gateway::{GatewayToolContext, check_cancelled, failure};
use super::gateway_process::{OwnedProcessJob, hide, terminate};
use crate::gateway_contract::GatewayError;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
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
    let connected = Box::pin(CdpClient::connect(socket)).await?;
    if !socket.contains("/devtools/browser/") {
        for method in ["Page.enable", "Runtime.enable", "Network.enable"] {
            connected.call(method, json!({}), ctx).await?;
        }
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
    client(socket, ctx).await?.call(method, params, ctx).await.map_err(|mut error| {
        if error.code == "BROWSER_TOOL_FAILED" { error.message = format!("{method}: {}", error.message); }
        error
    })
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
    embedded_target: Option<String>,
    workspace_last_active: Option<tokio::time::Instant>,
    workspace_pages: Vec<String>,
    workspace_page_index: usize,
    workspace_seen_pages: HashSet<String>,
    browser_socket: Option<String>,
    workspace_browser: Option<Arc<Mutex<WorkspaceBrowser>>>,
    workspace_address: Option<String>,
    workspace_current_address: Option<String>,
}

/// One application-owned process shares persistent site data; page permissions remain conversation-scoped.
/// 应用自有的单个进程共享持久网站数据；页面操作权限仍按聊天隔离。
pub(crate) struct WorkspaceBrowser {
    child: Child,
    owned: OwnedProcessJob,
    endpoint: String,
    profile: PathBuf,
    socket: String,
    client: Arc<CdpClient>,
    initial_target: Option<String>,
    next_window_index: u32,
}

impl Drop for WorkspaceBrowser {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl WorkspaceBrowser {
    async fn close(&mut self, ctx: &GatewayToolContext) -> Result<(), GatewayError> {
        // Browser.close flushes Cookies/Local Storage before the job fallback releases remaining children.
        // Browser.close 先刷新 Cookie/本地存储，随后才由作业兜底释放残余子进程。
        if self.owned.has_running_processes()? {
            let _ = tokio::time::timeout(Duration::from_secs(2), self.client.call("Browser.close", json!({}), ctx)).await;
            let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
        }
        self.client.close().await?;
        self.owned.terminate_and_wait().await?;
        terminate(&mut self.child).await;
        Ok(())
    }
}

/// Attach only the desktop's external web target; document/chat renderers stay outside automation.
/// 只接入桌面的外部网页目标；聊天及文档渲染器不进入浏览器自动化范围。
pub(crate) async fn attach_embedded(
    endpoint: &str,
    target_id: &str,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let address = url::Url::parse(endpoint)
        .map_err(|_| failure("BROWSER_INVALID_ENDPOINT", "内嵌浏览器地址无效。", 400))?;
    if address.scheme() != "http"
        || address
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_none_or(|host| !host.is_loopback())
        || address.port().is_none()
        || !address.username().is_empty()
        || address.password().is_some()
        || address.path() != "/"
        || address.query().is_some()
        || address.fragment().is_some()
        || target_id.is_empty()
        || target_id.len() > 256
        || !target_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(failure(
            "BROWSER_INVALID_ENDPOINT",
            "内嵌浏览器必须使用本机调试端点和实际标签身份。",
            400,
        ));
    }
    if let Some(browser) = ctx.runtime.workspace_browser.lock().await.as_ref() {
        if browser.lock().await.endpoint == address.as_str().trim_end_matches('/') {
            return Err(failure("BROWSER_ENDPOINT_SCOPE_REQUIRED", "共享浏览器页面沿所属聊天工作区会话接入，不能绕过页面归属重新附着。", 403));
        }
    }
    let mut session = BrowserSession {
        child: None,
        endpoint: address.as_str().trim_end_matches('/').to_owned(),
        profile: PathBuf::new(),
        conversation_id: ctx.conversation_id.clone(),
        tabs: HashMap::new(),
        active_tab: Some(target_id.to_owned()),
        uploads: Vec::new(),
        upload_bytes: 0,
        _owned: None,
        closed: false,
        profile_cleanup_pending: false,
        profile_cleanup_error: None,
        embedded_target: Some(target_id.to_owned()),
        workspace_last_active: None,
        workspace_pages: vec![],
        workspace_page_index: 0,
        workspace_seen_pages: HashSet::new(),
        browser_socket: None,
        workspace_browser: None,
        workspace_address: None,
        workspace_current_address: None,
    };
    let tabs = update_tabs(&mut session, ctx).await?;
    if !session.tabs.contains_key(target_id) {
        return Err(failure(
            "BROWSER_TAB_UNAVAILABLE",
            "内嵌网页标签尚未就绪。",
            409,
        ));
    }
    let handles = ctx
        .runtime
        .browsers
        .lock()
        .await
        .iter()
        .map(|(id, value)| (id.clone(), value.clone()))
        .collect::<Vec<_>>();
    for (id, handle) in handles {
        let old = handle.lock().await;
        let same_chat = old.conversation_id == ctx.conversation_id && old.embedded_target.is_some();
        let same_target = same_chat
            && old.endpoint == session.endpoint
            && old.embedded_target.as_deref() == Some(target_id)
            && !old.closed;
        if same_target {
            return Ok(
                json!({"sessionId":id,"tabId":target_id,"tabs":tabs,"embedded":true,"executionEnvironment":environment(&old)}),
            );
        }
        drop(old);
        if same_chat {
            detach_embedded(&id, ctx).await?;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let execution_environment = environment(&session);
    let mut sessions = ctx.runtime.browsers.lock().await;
    if sessions.len() >= 16 {
        return Err(failure(
            "BROWSER_SESSION_CAPACITY",
            "浏览器会话容量已满。",
            429,
        ));
    }
    sessions.insert(id.clone(), Arc::new(Mutex::new(session)));
    Ok(
        json!({"sessionId":id,"tabId":target_id,"tabs":tabs,"embedded":true,"executionEnvironment":execution_environment}),
    )
}

/// Detaching a view drops automation state and never closes the desktop-owned WebView process.
/// 分离视图只释放自动化状态，绝不关闭桌面拥有的 WebView 进程。
pub(crate) async fn detach_embedded(
    id: &str,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let handle = ctx.runtime.browsers.lock().await.get(id).cloned();
    let Some(handle) = handle else {
        return Ok(json!({"closed":true,"sessionId":id}));
    };
    let mut session = handle.lock().await;
    if session.conversation_id != ctx.conversation_id || session.embedded_target.is_none() {
        return Err(failure(
            "BROWSER_SESSION_NOT_FOUND",
            "此内嵌浏览器不属于当前聊天。",
            404,
        ));
    }
    session.closed = true;
    for socket in session.tabs.values().chain(session.browser_socket.iter()) {
        let connection = ctx
            .runtime
            .cdp
            .lock()
            .await
            .remove(&connection_key(socket, ctx));
        if let Some(connection) = connection {
            connection.close().await?;
        }
    }
    drop(session);
    ctx.runtime.browsers.lock().await.remove(id);
    Ok(json!({"closed":true,"sessionId":id,"externalProcessPreserved":true}))
}

/// Launch the bundled browser only when a page is requested; reuse one persistent application profile.
/// 请求网页时才启动随包浏览器；统一复用应用持久配置，不在软件启动时预开浏览器。
async fn start_workspace_browser(ctx: &GatewayToolContext) -> Result<WorkspaceBrowser, GatewayError> {
    let browser = chrome_candidates(ctx).into_iter().find(|path| {
        path.is_absolute() && path.is_file()
            && super::local_files::inspect(path, false, false).is_ok()
    }).ok_or_else(|| failure("WORKSPACE_CHROME_UNAVAILABLE", "随包 Chromium 不可用，请修复软件资源。", 503))?;
    let profile = ctx.data_home.join("Browser/Workspace");
    tokio::fs::create_dir_all(&profile).await?;
    // A previous crashed launch may leave this discovery file; never reuse an unowned debug endpoint.
    // 上次异常退出可能遗留端口文件；启动前移除，绝不复用不属于当前作业的调试端点。
    match tokio::fs::remove_file(profile.join("DevToolsActivePort")).await {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    super::gateway_browser_permissions::prepare(ctx, &browser).await?;
    let mut command = Command::new(browser);
    hide(&mut command);
    // Disable top-level occlusion handling so embedded pages survive loss of shell focus.
    // 关闭顶层遮挡处理，避免软件失去焦点时嵌入页面停止显示。
    command.args([
        "--remote-debugging-port=0", "--remote-debugging-address=127.0.0.1",
        "--no-first-run", "--no-default-browser-check", "--disable-background-networking",
        "--disable-extensions", "--window-position=-32000,-32000", "--window-size=1200,900",
        "--disable-backgrounding-occluded-windows", "--disable-features=CalculateNativeWinOcclusion",
        "about:blank",
    ]).arg(format!("--user-data-dir={}", profile.display()))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| failure("BROWSER_START_FAILED", "随包浏览器启动失败。", 503))?;
    let owned = match OwnedProcessJob::attach(&child) {
        Ok(owned) => owned,
        Err(error) => { terminate(&mut child).await; return Err(error); }
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let port = loop {
        check_cancelled(ctx)?;
        if !owned.has_running_processes()? {
            return Err(failure("BROWSER_START_FAILED", "随包浏览器在就绪前退出。", 503));
        }
        if let Ok(value) = tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await {
            if let Some(port) = value.lines().next().and_then(|value| value.parse::<u16>().ok()).filter(|port| *port != 0) {
                break port;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failure("BROWSER_START_TIMEOUT", "随包浏览器调试端口启动超时。", 504));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let endpoint = format!("http://127.0.0.1:{port}");
    let version = http_endpoint(&endpoint, "/json/version").await?;
    let socket = version["webSocketDebuggerUrl"].as_str()
        .ok_or_else(|| failure("BROWSER_INVALID_ENDPOINT", "随包浏览器窗口连接尚未就绪。", 502))?.to_owned();
    let client = Box::pin(CdpClient::connect(&socket)).await?;
    // DevTools may be ready before the first page; wait for that page instead of opening another window.
    // DevTools 可能先于首页就绪；等待并接管首页，避免多开窗口。
    let initial_target = loop {
        check_cancelled(ctx)?;
        let targets = client.call("Target.getTargets", json!({}), ctx).await?;
        if let Some(target) = targets["targetInfos"].as_array().into_iter().flatten()
            .find(|target| target["type"] == "page" && target["url"] == "about:blank")
            .and_then(|target| target["targetId"].as_str())
        { break target.to_owned(); }
        if !owned.has_running_processes()? {
            return Err(failure("BROWSER_START_FAILED", "随包浏览器在首页就绪前退出。", 503));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failure("BROWSER_START_TIMEOUT", "随包浏览器首页启动超时。", 504));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    {
        // Repair persisted window placement before measuring the first page; website data stays intact.
        // 首次测量前纠正已保存的异常窗口尺寸，保留网站登录数据与页面缩放设置。
        let window = client.call("Browser.getWindowForTarget", json!({"targetId":initial_target}), ctx).await?;
        if window["bounds"]["windowState"].as_str().is_some_and(|state| state != "normal") {
            client.call("Browser.setWindowBounds", json!({"windowId":window["windowId"],"bounds":{"windowState":"normal"}}), ctx).await?;
        }
        client.call("Browser.setWindowBounds", json!({"windowId":window["windowId"],
            "bounds":{"left":-32000,"top":-32000,"width":1200,"height":900}}), ctx).await?;
    }
    Ok(WorkspaceBrowser { child, owned, endpoint, profile, socket, client, initial_target: Some(initial_target), next_window_index: 1 })
}

/// Each workspace page gets its own target/window while sharing the application login profile.
/// 每个工作区网页拥有独立目标及窗口，同时共享应用登录配置。
pub(crate) async fn workspace_open(
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let url = allowed_url(input["url"].as_str().unwrap_or("about:blank"))?;
    // The registry lock serializes first launch, page allocation and last-close for this single profile.
    // 注册锁串行化同一配置的首次启动、页面分配与末页关闭，避免重复进程抢占目录。
    let mut registry = ctx.runtime.workspace_browser.lock().await;
    let handles = ctx.runtime.browsers.lock().await.iter()
        .map(|(id, handle)| (id.clone(), handle.clone())).collect::<Vec<_>>();
    for (id, handle) in handles {
        let session = handle.lock().await;
        if !session.closed && session.workspace_browser.is_some()
            && session.conversation_id == ctx.conversation_id
            && (session.workspace_address.as_deref() == Some(url.as_str())
                || session.workspace_current_address.as_deref() == Some(url.as_str()))
        {
            if !session.workspace_browser.as_ref().unwrap().lock().await.owned.has_running_processes()? { continue; }
            drop(session); drop(registry);
            return workspace_action(&json!({"sessionId":id,"action":"state"}), ctx).await;
        }
    }
    if ctx.runtime.browsers.lock().await.len() >= 16 {
        return Err(failure("BROWSER_SESSION_CAPACITY", "浏览器会话容量已满。", 429));
    }
    let running = if let Some(browser) = registry.as_ref() {
        browser.lock().await.owned.has_running_processes()?
    } else { false };
    if !running { *registry = Some(Arc::new(Mutex::new(start_workspace_browser(ctx).await?))); }
    let browser_handle = registry.as_ref().unwrap().clone();
    let mut browser = browser_handle.lock().await;
    let target = if let Some(target) = browser.initial_target.take() { target } else {
        let coordinate = -32000 + (browser.next_window_index % 512) as i32 * 16;
        browser.next_window_index = browser.next_window_index.wrapping_add(1);
        let created = browser.client.call("Target.createTarget",
            json!({"url":"about:blank","newWindow":true,"background":true,"left":coordinate,"top":coordinate,"width":1200,"height":900}), ctx).await?;
        created["targetId"].as_str().ok_or_else(|| failure("BROWSER_TAB_UNAVAILABLE", "随包浏览器页面创建失败。", 502))?.to_owned()
    };
    let id = uuid::Uuid::new_v4().to_string();
    let session = BrowserSession {
        child: None, endpoint: browser.endpoint.clone(), profile: browser.profile.clone(),
        conversation_id: ctx.conversation_id.clone(), tabs: HashMap::new(), active_tab: Some(target.clone()),
        uploads: vec![], upload_bytes: 0, _owned: None, closed: false,
        profile_cleanup_pending: false, profile_cleanup_error: None,
        embedded_target: Some(target.clone()), workspace_last_active: Some(tokio::time::Instant::now()),
        workspace_pages: vec![target.clone()], workspace_page_index: 0,
        workspace_seen_pages: HashSet::from([target.clone()]), browser_socket: Some(browser.socket.clone()),
        workspace_browser: Some(browser_handle.clone()),
        workspace_address: Some(url.as_str().to_owned()),
        workspace_current_address: None,
    };
    drop(browser);
    // Register ownership before fallible target initialization so every created window has a cleanup path.
    // 易失败的页面初始化之前先登记归属，保证已创建窗口始终有准确的关闭入口。
    let handle = Arc::new(Mutex::new(session));
    ctx.runtime.browsers.lock().await.insert(id.clone(), handle.clone());
    drop(registry);
    let initialized = async {
        let mut session = handle.lock().await;
        update_tabs(&mut session, ctx).await?;
        let socket = session.tabs.get(&target).ok_or_else(|| failure("BROWSER_TAB_UNAVAILABLE", "随包浏览器页面尚未就绪。", 502))?;
        rpc(socket, "Page.navigate", json!({"url":url.as_str()}), ctx).await?;
        Ok::<(), GatewayError>(())
    }.await;
    if let Err(error) = initialized {
        return Err(cleanup_failed_workspace_open(&id, &handle, error, ctx).await);
    }
    let committed_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match workspace_action(&json!({"sessionId":id,"action":"state"}), ctx).await {
            Ok(value) if value["navigationCommitted"] != true && tokio::time::Instant::now() < committed_deadline => {
                // Wait for the first URL to enter history before a second navigation replaces it.
                // 等首页写入历史再导航，保留可返回的第一页。
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(value) => return Ok(value),
            Err(error) => {
                return Err(cleanup_failed_workspace_open(&id, &handle, error, ctx).await);
            }
        }
    }
}

/// Cleanup cannot inherit an already-cancelled opening request; keep the original failure visible.
/// 关闭不能继承已取消的打开请求信号，同时保留原始初始化错误供界面准确报告。
async fn cleanup_failed_workspace_open(
    id: &str, handle: &Arc<Mutex<BrowserSession>>, mut error: GatewayError, ctx: &GatewayToolContext,
) -> GatewayError {
    let (_sender, cancellation) = tokio::sync::watch::channel(false);
    let mut cleanup = ctx.clone();
    cleanup.cancellation = cancellation;
    let cleanup_result = close_workspace_session(id, handle, &cleanup).await;
    if let Err(cleanup_error) = cleanup_result {
        // A broken CDP connection cannot orphan the last shared process; never kill a peer's live pages.
        // CDP 断开也不能遗留最后一个共享进程；存在其他活跃网页时不能连带终止。
        let registry = ctx.runtime.workspace_browser.lock().await;
        let browser = handle.lock().await.workspace_browser.as_ref().unwrap().clone();
        let peers = ctx.runtime.browsers.lock().await.iter().filter(|(other, _)| other.as_str() != id)
            .map(|(_, handle)| handle.clone()).collect::<Vec<_>>();
        let mut has_peer = false;
        for peer in peers {
            let peer = peer.lock().await;
            if !peer.closed && peer.workspace_browser.as_ref().is_some_and(|other| Arc::ptr_eq(other, &browser)) {
                has_peer = true; break;
            }
        }
        let forced = if !has_peer { Some(browser.lock().await.close(&cleanup).await) } else { None };
        drop(registry);
        if forced.as_ref().is_some_and(Result::is_ok) && close_workspace_session(id, handle, &cleanup).await.is_ok() {
            return error;
        }
        let final_cleanup = forced.and_then(Result::err).unwrap_or(cleanup_error);
        error.message = format!("{}（初始化错误：{}；浏览器关闭未确认：{}，已保留自有会话状态。）",
            error.message, error.code, final_cleanup.code);
        error.code = "BROWSER_INITIALIZATION_CLEANUP_FAILED".to_owned();
        error.status = 503;
    }
    error
}

/// The UI surface exposes fixed navigation actions, never an arbitrary CDP command or script executor.
/// 界面仅开放固定导航动作，不暴露任意 CDP 命令或脚本执行接口。
pub(crate) async fn workspace_action(
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let action = input["action"].as_str().unwrap_or("state");
    if action == "clearSiteData" { return clear_workspace_site_data(ctx).await; }
    if !["navigate", "back", "forward", "reload", "state", "close"].contains(&action) {
        return Err(failure(
            "WORKSPACE_BROWSER_ACTION_INVALID",
            "不支持该浏览器操作。",
            400,
        ));
    }
    let id = input["sessionId"].as_str().unwrap_or("");
    let handle = ctx
        .runtime
        .browsers
        .lock()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| failure("BROWSER_SESSION_NOT_FOUND", "网页会话已关闭。", 404))?;
    let mut session = handle.lock().await;
    if session.conversation_id != ctx.conversation_id
        || session.child.is_none() && session.workspace_browser.is_none()
        || session.workspace_last_active.is_none()
    {
        return Err(failure(
            "BROWSER_SESSION_NOT_FOUND",
            "此网页宿主不属于当前聊天。",
            404,
        ));
    }
    if action == "close" {
        drop(session);
        return Box::pin(execute("browser.close", json!({"sessionId":id}), ctx)).await;
    }
    if session.closed {
        return Err(failure("BROWSER_SESSION_CLOSED", "网页会话已关闭。", 409));
    }
    if let Some(owner) = &session._owned {
        if !owner.has_running_processes()? {
            return Err(failure("BROWSER_DISCONNECTED", "网页浏览器已经退出。", 409));
        }
    }
    if let Some(browser) = &session.workspace_browser {
        if !browser.lock().await.owned.has_running_processes()? {
            return Err(failure("BROWSER_DISCONNECTED", "随包浏览器已经退出。", 409));
        }
    }
    session.workspace_last_active = Some(tokio::time::Instant::now());
    let tabs = update_tabs(&mut session, ctx).await?;
    let browser_socket = match &session.browser_socket {
        Some(socket) => socket.clone(),
        None => {
            let version = http(&session, "/json/version").await?;
            let socket = version["webSocketDebuggerUrl"].as_str()
                .ok_or_else(|| failure("BROWSER_INVALID_ENDPOINT", "浏览器窗口连接尚未就绪。", 502))?.to_owned();
            let address = url::Url::parse(&socket).map_err(|_| failure("BROWSER_INVALID_ENDPOINT", "浏览器窗口连接无效。", 502))?;
            let endpoint = url::Url::parse(&session.endpoint).map_err(|_| failure("BROWSER_INVALID_ENDPOINT", "浏览器端点无效。", 502))?;
            if address.scheme() != "ws" || address.host_str() != endpoint.host_str()
                || address.port_or_known_default() != endpoint.port_or_known_default()
                || !address.username().is_empty() || address.password().is_some()
                || !address.path().starts_with("/devtools/browser/")
            { return Err(failure("BROWSER_ENDPOINT_CHANGED", "浏览器窗口连接身份不匹配。", 409)); }
            session.browser_socket = Some(socket.clone());
            socket
        }
    };
    // Follow real popup targets in the owned profile; keep opener state instead of rewriting site scripts.
    // 仅跟随自有配置中的真实弹页，保留 opener 和原页面状态，不全局改写网站脚本或重放 URL。
    if session.embedded_target.as_ref().is_some_and(|id| session.tabs.contains_key(id)) {
        for page in tabs.as_array().into_iter().flatten().rev() {
            let Some(target_id) = page["tabId"].as_str() else { continue; };
            if allowed_url(page["url"].as_str().unwrap_or("")).is_err() { continue; }
            if !session.workspace_seen_pages.insert(target_id.to_owned()) { continue; }
            let target = rpc(&browser_socket, "Target.getTargetInfo", json!({"targetId":target_id}), ctx).await?;
            if target["targetInfo"]["openerId"].as_str().is_some_and(|opener|
                session.workspace_pages.iter().any(|known| known == opener))
            {
                let keep = session.workspace_page_index + 1;
                session.workspace_pages.truncate(keep);
                session.workspace_pages.push(target_id.to_owned());
                session.workspace_page_index = session.workspace_pages.len() - 1;
                session.embedded_target = Some(target_id.to_owned());
                session.active_tab = Some(target_id.to_owned());
            }
        }
    }
    if session.embedded_target.as_ref().is_none_or(|id| !session.tabs.contains_key(id)) {
        if let Some((index, id)) = session.workspace_pages.iter().enumerate().rev()
            .find(|(_, id)| session.tabs.contains_key(*id))
        {
            let id = id.clone();
            session.workspace_page_index = index;
            session.embedded_target = Some(id.clone()); session.active_tab = Some(id);
        }
    }
    let mut tab = session
        .embedded_target
        .as_deref()
        .ok_or_else(|| failure("BROWSER_TAB_UNAVAILABLE", "网页标签尚未就绪。", 409))?
        .to_owned();
    let mut socket = session
        .tabs
        .get(&tab)
        .cloned()
        .ok_or_else(|| failure("BROWSER_TAB_UNAVAILABLE", "网页标签已经关闭。", 409))?;
    match action {
        "navigate" => {
            let url = allowed_url(input["url"].as_str().unwrap_or(""))?;
            rpc(&socket, "Page.navigate", json!({"url":url.as_str()}), ctx).await?;
            session.workspace_address = Some(url.as_str().to_owned());
        }
        "reload" => {
            rpc(&socket, "Page.reload", json!({}), ctx).await?;
        }
        "back" | "forward" => {
            let history = workspace_navigation_history(&socket, ctx).await?;
            let index = history["currentIndex"].as_i64().unwrap_or(0)
                + if action == "back" { -1 } else { 1 };
            let popup_forward = action == "forward"
                && session.workspace_page_index + 1 < session.workspace_pages.len();
            if let Some(entry) = (!popup_forward).then(|| usize::try_from(index)
                .ok()
                .and_then(|index| history["entries"].get(index))).flatten()
            {
                rpc(
                    &socket,
                    "Page.navigateToHistoryEntry",
                    json!({"entryId":entry["id"]}),
                    ctx,
                )
                .await?;
            } else {
                let next = if action == "back" { session.workspace_page_index.checked_sub(1) }
                    else { session.workspace_page_index.checked_add(1).filter(|index| *index < session.workspace_pages.len()) };
                if let Some((index, target)) = next.and_then(|index|
                    session.workspace_pages.get(index).filter(|target| session.tabs.contains_key(*target))
                        .map(|target| (index, target.clone())))
                {
                    session.workspace_page_index = index;
                    tab = target;
                    socket = session.tabs[&tab].clone();
                    session.embedded_target = Some(tab.clone()); session.active_tab = Some(tab.clone());
                    rpc(&socket, "Page.bringToFront", json!({}), ctx).await?;
                }
            }
        }
        _ => {}
    }
    let history = workspace_navigation_history(&socket, ctx).await?;
    let index = history["currentIndex"].as_u64().unwrap_or(0) as usize;
    let entries = history["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let current = entries
        .get(index)
        .cloned()
        .unwrap_or(Value::Null);
    let target = tabs.as_array().and_then(|tabs| tabs.iter().find(|item| item["tabId"] == tab));
    let current_url = current["url"].as_str().filter(|value| !value.is_empty())
        .or_else(|| target.and_then(|target| target["url"].as_str())).unwrap_or("");
    let current_title = current["title"].as_str().filter(|value| !value.is_empty())
        .or_else(|| target.and_then(|target| target["title"].as_str())).unwrap_or("");
    if !current_url.is_empty() { session.workspace_current_address = Some(current_url.to_owned()); }
    let page_state = evaluate(&socket, include_str!("browser_page_state.js").to_owned(), ctx)
        .await.unwrap_or(Value::Null);
    // Browser-level metadata survives renderer swaps during Page.navigate.
    // 浏览器级窗口信息不依赖当前 renderer，页面导航切换进程期间仍能正确查询。
    let window = rpc(&browser_socket, "Browser.getWindowForTarget", json!({"targetId":tab}), ctx).await?;
    let process_id = if let Some(browser) = &session.workspace_browser { browser.lock().await.child.id() }
        else { session.child.as_ref().and_then(Child::id) };
    Ok(
        json!({"sessionId":id,"tabId":tab,"processId":process_id,"endpoint":session.endpoint,
        "url":current_url,"requestedUrl":session.workspace_address,"title":current_title,"canGoBack":index>0 || session.workspace_page_index>0,
        "canGoForward":index+1<entries.len() || session.workspace_page_index+1<session.workspace_pages.len(),
        "windowId":window["windowId"],"windowBounds":window["bounds"],
        "viewport":page_state["viewport"],
        "navigationCommitted":current["url"].as_str().is_some_and(|url| !url.is_empty() && url != "about:blank"),
        "navigationPending":history["pending"] == true,
        "executor":"native-cdp","embedded":true,
        "needsHumanVerification":page_state["needsHumanVerification"] == true,
        "alternativeSearchUrl":page_state["alternativeSearchUrl"]}),
    )
}

/// After navigation, renderer changes may reject history reads; retry metadata, not navigation.
/// 导航后切换渲染器可能暂时无法读取历史；仅重读元信息，不重放导航。
async fn workspace_navigation_history(socket: &str, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        match rpc(socket, "Page.getNavigationHistory", json!({}), ctx).await {
            Err(error) if error.code == "BROWSER_TOOL_FAILED" && error.message.contains("Not attached to an active page") => {
                if tokio::time::Instant::now() >= deadline {
                    return Ok(json!({"currentIndex":0,"entries":[],"pending":true}));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            result => return result,
        }
    }
}

async fn close_workspace_session(
    id: &str, handle: &Arc<Mutex<BrowserSession>>, ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let mut registry = ctx.runtime.workspace_browser.lock().await;
    let mut session = handle.lock().await;
    if session.conversation_id != ctx.conversation_id {
        return Err(failure("BROWSER_SESSION_NOT_FOUND", "此网页宿主不属于当前聊天。", 404));
    }
    let browser = session.workspace_browser.as_ref().unwrap().clone();
    if browser.lock().await.owned.has_running_processes()? {
        let _ = update_tabs(&mut session, ctx).await;
        let targets = session.tabs.keys().chain(session.workspace_pages.iter())
            .chain(session.workspace_seen_pages.iter()).cloned().collect::<HashSet<_>>();
        let browser = browser.lock().await;
        for target in targets {
            browser.client.call("Target.closeTarget", json!({"targetId":target}), ctx).await?;
        }
    }
    session.closed = true;
    for socket in session.tabs.values().chain(session.browser_socket.iter()) {
        if let Some(connection) = ctx.runtime.cdp.lock().await.remove(&connection_key(socket, ctx)) {
            connection.close().await?;
        }
    }
    for upload in &session.uploads { clean_upload(upload, ctx); }
    drop(session);
    ctx.runtime.browsers.lock().await.remove(id);
    let remaining = ctx.runtime.browsers.lock().await.values().cloned().collect::<Vec<_>>();
    let mut in_use = false;
    for handle in remaining {
        let session = handle.lock().await;
        if !session.closed && session.workspace_browser.as_ref().is_some_and(|other| Arc::ptr_eq(other, &browser)) {
            in_use = true; break;
        }
    }
    if !in_use {
        browser.lock().await.close(ctx).await?;
        if registry.as_ref().is_some_and(|registered| Arc::ptr_eq(registered, &browser)) { *registry = None; }
    }
    Ok(json!({"closed":true,"sessionId":id,"profilePreserved":true,"profileCleanupPending":false,
        "externalProcessPreserved":true,"sharedBrowserRunning":in_use}))
}

/// Explicit UI action clears only KYNXA's global site data, after all its workspace windows have exited.
/// 仅用户显式操作才清除 KYNXA 全局网站数据，并先关闭本应用全部工作区窗口。
async fn clear_workspace_site_data(ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let mut registry = ctx.runtime.workspace_browser.lock().await;
    let handles = ctx.runtime.browsers.lock().await.iter()
        .map(|(id, handle)| (id.clone(), handle.clone())).collect::<Vec<_>>();
    let mut closed = Vec::new();
    for (id, handle) in handles {
        let mut session = handle.lock().await;
        if session.workspace_browser.is_none() { continue; }
        session.closed = true;
        for socket in session.tabs.values().chain(session.browser_socket.iter()) {
            let key = format!("{}\0{socket}", session.conversation_id);
            if let Some(connection) = ctx.runtime.cdp.lock().await.remove(&key) { connection.close().await?; }
        }
        for upload in &session.uploads { clean_upload(upload, ctx); }
        closed.push(id);
    }
    if let Some(browser) = registry.as_ref() { browser.lock().await.close(ctx).await?; }
    *registry = None;
    let mut sessions = ctx.runtime.browsers.lock().await;
    for id in &closed { sessions.remove(id); }
    drop(sessions);
    let profile = ctx.data_home.join("Browser/Workspace");
    match tokio::fs::remove_dir_all(&profile).await {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(_) => return Err(failure("BROWSER_SITE_DATA_CLEANUP_FAILED", "浏览器已关闭，但全局网站数据清除失败，请重试。", 503)),
    }
    Ok(json!({"cleared":true,"scope":"application","closedSessionIds":closed}))
}

/// Clean only confirmed-closed sessions, within the existing sixteen-session limit; retain no user request.
/// 仅清理已确认关闭的会话，沿用十六会话上限，不保留用户请求。
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
                        if sessions
                            .get(&id)
                            .is_some_and(|active| Arc::ptr_eq(active, &handle))
                        {
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
            "Connect to this chat's current right-workspace page without a URL, or open the requested URL there. An unchanged URL preserves the current page. Returns actual sessionId and tabId; inspect with browser.snapshot.",
            json!({"url":{"type":"string"},"foreground":{"type":"boolean"},"browserPath":{"type":"string"},"cdpEndpoint":{"type":"string","description":"Explicit HTTP(S) DevTools endpoint of an already authorized browser; never launches or kills that external process."}}),
            vec!["reason"],
        ),
        (
            "tabs",
            "Discover this chat's registered browser sessions when sessionId is omitted or empty; returns actual sessionId/tabId. With a sessionId, refresh its tabs. Discovery is not page-content observation; use browser.snapshot to inspect.",
            json!({"sessionId":{"type":"string"}}),
            vec!["reason"],
        ),
        (
            "navigate",
            "Navigate an identified browser tab, then inspect current DOM before interacting.",
            json!({"sessionId":{"type":"string"},"tabId":{"type":"string"},"url":{"type":"string"}}),
            vec!["sessionId", "tabId", "url", "reason"],
        ),
        (
            "snapshot",
            "Read article text, headings, source links, tables and actionable controls from an identified tab. Includes extraction/coverage details; controls have current selectors, roles and viewport bounds. References expire on navigation or DOM change.",
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

fn supported_chromium_name(name: &str) -> bool {
    ["chrome.exe", "chromium.exe", "msedge.exe", "brave.exe", "vivaldi.exe"]
        .contains(&name.to_ascii_lowercase().as_str())
}

fn chrome_candidates(ctx: &GatewayToolContext) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    // The workspace uses only its bundled browser; installed or default system browsers are never fallback candidates.
    // 工作区只使用随包浏览器；本机已安装或默认浏览器不作为回退候选。
    if let Some((application, manifest)) = super::gateway::bundled_mcp_manifest(ctx) {
        if let Some(path) =
            super::gateway::bundled_mcp_path(&application, &manifest["browser"]["path"])
        {
            candidates.push(path);
        }
    }
    candidates
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
    candidates.extend(chrome_candidates(ctx));
    candidates
        .into_iter()
        .find(|path| {
            path.is_absolute()
                && path.is_file()
                && path.file_name().and_then(|name| name.to_str()).is_some_and(supported_chromium_name)
                && super::local_files::inspect(path, false, false).is_ok()
                && !excluded_executable(path, &intent)
        })
        .ok_or_else(|| {
            failure(
                "BROWSER_EXECUTABLE_NOT_FOUND",
                "随包 Chromium 不可用；请修复软件资源，或显式配置要启动的外部浏览器。",
                503,
            )
        })
}

/// A pending page can already own an embeddable window; its presentation path is not a navigation receipt.
/// 页面未提交时仍可能已有可嵌入窗口；展示路径可用请求地址，不能改写真实导航回执。
fn workspace_presentation_path(state: &Value) -> Value {
    if state["url"].as_str().is_some_and(|text| url::Url::parse(text)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https")))
    {
        state["url"].clone()
    } else {
        state.get("requestedUrl").filter(|value| value.as_str().is_some_and(|text| !text.is_empty()))
            .cloned().unwrap_or_else(|| state["url"].clone())
    }
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
    http_endpoint(&session.endpoint, action).await
}

async fn http_endpoint(endpoint: &str, action: &str) -> Result<Value, GatewayError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| failure("BROWSER_HTTP_FAILED", "浏览器客户端创建失败。", 500))?;
    let response = client
        .get(format!("{endpoint}{action}"))
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
    if session.embedded_target.is_some() {
        value["operationLocation"] = json!("gateway-host");
        value["browserTarget"] = json!({"location":"gateway-host","mode":"embedded-sidebar","evidence":"desktop-attached-target"});
        return value;
    }
    if !owned {
        value["operationLocation"] = json!("unknown");
    }
    value["browserTarget"] = json!({"location":if owned{"gateway-host"}else{"unknown"},"mode":if owned{"independent-browser"}else{"existing-browser"},
        "evidence":if owned{"application-verified-browser-target"}else{"trusted-browser-configuration"}});
    value
}

async fn update_tabs(session: &mut BrowserSession, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let mut workspace_targets = HashSet::new();
    if let Some(browser) = &session.workspace_browser {
        let browser = browser.lock().await;
        let targets = browser.client.call("Target.getTargets", json!({}), ctx).await?;
        workspace_targets.extend(session.workspace_pages.iter().cloned());
        workspace_targets.extend(session.workspace_seen_pages.iter().cloned());
        // Only targets descended from this view's root join its automation scope, although cookies are shared.
        // Cookie 虽共享，只有本视图根页面衍生的弹页可进入其自动化范围。
        loop {
            let before = workspace_targets.len();
            for target in targets["targetInfos"].as_array().into_iter().flatten() {
                if target["type"] == "page"
                    && target["openerId"].as_str().is_some_and(|id| workspace_targets.contains(id))
                {
                    if let Some(id) = target["targetId"].as_str() { workspace_targets.insert(id.to_owned()); }
                }
            }
            if workspace_targets.len() == before { break; }
        }
    }
    let tabs = http(session, "/json/list").await?;
    session.tabs.clear();
    let mut values = Vec::new();
    for tab in tabs.as_array().into_iter().flatten() {
        if tab["type"] != "page" {
            continue;
        }
        if session.workspace_browser.is_some()
            && tab["id"].as_str().is_none_or(|id| !workspace_targets.contains(id))
        { continue; }
        if session
            .embedded_target
            .as_deref()
            .is_some_and(|target| session.workspace_last_active.is_none() && tab["id"].as_str() != Some(target))
        {
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

/// Pause repeated model navigation on a verification wall; explicit UI navigation stays available.
/// 验证页暂停模型的重复导航；用户在界面里的主动导航及验证操作仍可执行。
fn verification_handoff(state: &Value, destination: Option<&str>) -> Option<Value> {
    if state["needsHumanVerification"] != true { return None; }
    if let Some(destination) = destination {
        let current = url::Url::parse(state["verificationUrl"].as_str()?).ok()?;
        let requested = url::Url::parse(destination).ok()?;
        let same_page = current.origin() == requested.origin()
            && current.path() == requested.path() && current.query() == requested.query();
        let repeated_google_search = state["verificationProvider"] == "google"
            && current.host_str() == requested.host_str() && requested.path() == "/search";
        if !same_page && !repeated_google_search { return None; }
    }
    let mut handoff = state.clone();
    handoff["actionDispatched"] = json!(false);
    handoff["navigationPaused"] = json!(true);
    handoff["recommendedTool"] = json!("web.search");
    handoff["message"] = json!("当前网页需要人工验证，已保留页面且未刷新或重复搜索。普通资料检索请改用 web.search 的已启用搜索来源；此页面由用户完成验证后继续。");
    Some(handoff)
}

async fn inspect_verification_handoff(
    socket: &str, destination: Option<&str>, ctx: &GatewayToolContext,
) -> Result<Option<Value>, GatewayError> {
    let state = evaluate(socket, include_str!("browser_page_state.js").to_owned(), ctx).await?;
    Ok(verification_handoff(&state, destination))
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
    // Copy once from verified handles; delayed form submission must not reopen a changed source.
    // 从已核验句柄复制一次；延迟提交表单时不重读已变化的源路径。
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

/// Runtime identities are chat-scoped navigation hints, never durable memory or page evidence.
/// 运行时身份只提供当前聊天的导航线索，不进入长期记忆，也不冒充页面内容观测。
async fn known_sessions(ctx: &GatewayToolContext) -> Value {
    let handles = ctx.runtime.browsers.lock().await.iter()
        .map(|(id, handle)| (id.clone(), handle.clone())).collect::<Vec<_>>();
    let mut sessions = Vec::new();
    let mut busy = false;
    for (id, handle) in handles {
        let Ok(session) = handle.try_lock() else { busy = true; continue; };
        if session.closed || session.conversation_id != ctx.conversation_id { continue; }
        let tab = session.embedded_target.as_ref().or(session.active_tab.as_ref());
        let url = session.workspace_current_address.as_deref()
            .or(session.workspace_address.as_deref()).unwrap_or("");
        sessions.push((session.workspace_last_active, json!({"sessionId":id,"tabId":tab,
            "url":url.chars().take(1024).collect::<String>(),"embedded":session.embedded_target.is_some(),
            "state":"registered","pageContentObserved":false})));
    }
    sessions.sort_by(|left, right| right.0.cmp(&left.0));
    let values = sessions.into_iter().map(|(_, value)| value).collect::<Vec<_>>();
    let active = values.first().cloned().unwrap_or(Value::Null);
    json!({"sessions":values,"sessionId":active["sessionId"],"activeSessionId":active["sessionId"],
        "tabId":active["tabId"],"activeTabId":active["tabId"],
        "coverage":if busy{"busy-or-unobserved"}else{"current-chat"},"complete":!busy,
        "pageContentObserved":false})
}

pub(crate) async fn prompt_context(ctx: &GatewayToolContext) -> String {
    let mut known = known_sessions(ctx).await;
    let entries = known["sessions"].as_array_mut().unwrap();
    if entries.is_empty() { return String::new(); }
    entries.truncate(4);
    format!("Current chat browser identities (runtime hints, not page contents): {known}. Use these IDs with browser.snapshot to inspect the existing page; browser.tabs without sessionId discovers this chat's sessions. Do not reopen or navigate merely to inspect the page.")
}

pub(crate) async fn execute(
    name: &str,
    input: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    if name == "browser.tabs" && input["sessionId"].as_str().is_none_or(|id| id.trim().is_empty()) {
        return Ok(known_sessions(ctx).await);
    }
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
            if let Some(browser) = ctx.runtime.workspace_browser.lock().await.as_ref() {
                if browser.lock().await.endpoint == endpoint.as_str().trim_end_matches('/') {
                    return Err(failure("BROWSER_ENDPOINT_SCOPE_REQUIRED", "应用共享浏览器必须沿当前聊天已有的网页会话操作，不能把全局端点重新附着为独立浏览器。", 403));
                }
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
                embedded_target: None,
                workspace_last_active: None,
                workspace_pages: vec![],
                workspace_page_index: 0,
                workspace_seen_pages: HashSet::new(),
                browser_socket: None,
                workspace_browser: None,
                workspace_address: None,
                workspace_current_address: None,
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
            let tabs = update_tabs(&mut session, ctx).await?;
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
        let intent = super::intent::calculate("tools.browserIntent", &json!({"message":ctx.config["trustedUserText"],"previousUserMessages":ctx.config["previousUserMessages"]}))
            .map_err(|message| failure("TOOL_INTENT_FAILED", &message, 500))?;
        // Reuse the visible workspace page; external display requires verified user intent.
        // 优先复用可见工作区网页；外部展示须有已核验的用户意图。
        if input["browserPath"].is_null() && intent["presentationTarget"] != "application" {
            let handles = ctx
                .runtime
                .browsers
                .lock()
                .await
                .iter()
                .map(|(id, handle)| (id.clone(), handle.clone()))
                .collect::<Vec<_>>();
            let mut ordered = Vec::new();
            for (id, handle) in handles {
                let active = handle.lock().await.workspace_last_active;
                ordered.push((active, id, handle));
            }
            ordered.sort_by(|left, right| right.0.cmp(&left.0));
            for (_, id, handle) in ordered {
                let mut session = handle.lock().await;
                if session.conversation_id != ctx.conversation_id
                    || session.embedded_target.is_none()
                    || session.closed
                {
                    continue;
                }
                if session.workspace_browser.is_some() {
                    let navigate = input["url"].as_str().is_some_and(|url|
                        session.workspace_address.as_deref() != Some(url)
                            && session.workspace_current_address.as_deref() != Some(url));
                    let tab = session.embedded_target.as_deref().unwrap_or("");
                    if let Some(socket) = session.tabs.get(tab).filter(|_| navigate) {
                        if let Some(handoff) = inspect_verification_handoff(socket, input["url"].as_str(), ctx).await? {
                            return Ok(json!({"sessionId":id,"tabId":tab,"value":handoff,"executionEnvironment":environment(&session)}));
                        }
                    }
                    drop(session);
                    let mut opened = workspace_action(&json!({"sessionId":id,
                        "action":if navigate { "navigate" } else { "state" },"url":input["url"]}), ctx).await?;
                    opened["kind"] = json!("browser"); opened["presentation"] = json!("workspace");
                    opened["path"] = workspace_presentation_path(&opened); opened["mimeType"] = json!("text/html"); opened["version"] = json!("live");
                    return Ok(opened);
                }
                let tabs = update_tabs(&mut session, ctx).await?;
                let tab = session.embedded_target.as_deref().unwrap_or("");
                let socket = session.tabs.get(tab).ok_or_else(|| {
                    failure(
                        "BROWSER_TAB_UNAVAILABLE",
                        "内嵌网页已关闭，请重新打开网页标签。",
                        409,
                    )
                })?;
                if let Some(target) = input["url"].as_str() {
                    let target = allowed_url(target)?;
                    rpc(socket, "Page.navigate", json!({"url":target.as_str()}), ctx).await?;
                }
                return Ok(
                    json!({"sessionId":id,"tabId":tab,"tabs":tabs,"url":input["url"],"embedded":true,"executor":"native-cdp","executionEnvironment":environment(&session)}),
                );
            }
        }
        if input["browserPath"].is_null() && intent["presentationTarget"] != "application" {
            let mut opened = Box::pin(workspace_open(&input, ctx)).await?;
            opened["kind"] = json!("browser");
            opened["presentation"] = json!("workspace");
            opened["path"] = workspace_presentation_path(&opened);
            opened["mimeType"] = json!("text/html");
            opened["version"] = json!("live");
            return Ok(opened);
        }
        let path = executable(&input, ctx)?;
        let forbidden=super::intent::calculate("tools.foregroundForbidden",&json!({"message":ctx.config["trustedUserText"],"target":{"kind":"browser","appPath":path}})).map_err(|message|failure("TOOL_INTENT_FAILED",&message,500))?;
        let workspace_host = input["workspaceHost"] == true;
        if input["foreground"] == true && forbidden == true && !workspace_host {
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
        super::gateway_browser_permissions::prepare(ctx, &path).await?;
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
        if workspace_host {
            command
                .args(["--window-position=-32000,-32000", "--window-size=1200,900"])
                .arg(format!("--app={}", url.as_str()));
        } else if input["foreground"] != true {
            command.arg("--headless=new");
        }
        if !workspace_host {
            command.arg(url.as_str());
        }
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
            embedded_target: None,
            workspace_last_active: workspace_host.then(tokio::time::Instant::now),
            workspace_pages: vec![],
            workspace_page_index: 0,
            workspace_seen_pages: HashSet::new(),
            browser_socket: None,
            workspace_browser: None,
            workspace_address: None,
            workspace_current_address: None,
        };
        let mut tabs = update_tabs(&mut session, ctx).await?;
        if workspace_host {
            loop {
                let pages = tabs.as_array().map(Vec::as_slice).unwrap_or(&[]);
                let matched = pages.iter().find(|page| page["url"] == url.as_str()).or_else(|| {
                    let mut loaded = pages.iter().filter(|page| page["url"].as_str().is_some_and(|address|
                        address.starts_with("http://") || address.starts_with("https://")));
                    let only = loaded.next();
                    loaded.next().is_none().then_some(only).flatten()
                });
                if let Some(tab) = matched.and_then(|page| page["tabId"].as_str()) {
                    session.active_tab = Some(tab.to_owned());
                    break;
                }
                check_cancelled(ctx)?;
                if tokio::time::Instant::now() >= deadline {
                    session.close_owned_process().await?;
                    let _ = session.remove_owned_profile().await;
                    return Err(failure("BROWSER_START_TIMEOUT", "Chrome 网页目标未就绪，请重新打开。", 504));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                tabs = update_tabs(&mut session, ctx).await?;
            }
            session.embedded_target = session.active_tab.clone();
            if let Some(target) = session.active_tab.clone() {
                session.workspace_seen_pages.insert(target.clone());
                session.workspace_pages.push(target);
            }
        } else {
            session.active_tab = session.tabs.keys().next().cloned();
        }
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
        if session.workspace_browser.is_some() {
            drop(session);
            return close_workspace_session(id, &handle, ctx).await;
        }
        let owned = session.child.is_some();
        let execution_environment = environment(&session);
        if session.closed && session.profile_cleanup_pending {
            return Ok(
                json!({"closed":true,"sessionId":id,"profileCleanupPending":true,
                "profileCleanupError":session.profile_cleanup_error.as_ref().map(GatewayError::response),"externalProcessPreserved":!owned,"executionEnvironment":execution_environment}),
            );
        }
        for socket in session.tabs.values().chain(session.browser_socket.iter()) {
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
            return Ok(
                json!({"closed":true,"sessionId":id,"profileCleanupPending":true,
                "profileCleanupError":error.response(),"externalProcessPreserved":!owned,"executionEnvironment":execution_environment}),
            );
        }
        drop(session);
        ctx.runtime.browsers.lock().await.remove(id);
        return Ok(
            json!({"closed":true,"sessionId":id,"profileCleanupPending":false,"externalProcessPreserved":!owned,"executionEnvironment":execution_environment}),
        );
    }
    if session.closed {
        return Err(failure(
            "BROWSER_SESSION_CLOSED",
            "浏览器已关闭；配置清理状态可通过 browser.close 查看 / Browser is closed; inspect browser.close for profile cleanup status.",
            409,
        ));
    }
    if let Some(job) = &session._owned {
        if !job.has_running_processes()? {
            return Err(failure("BROWSER_DISCONNECTED", "浏览器进程已退出。", 409));
        }
    }
    let tabs = update_tabs(&mut session, ctx).await?;
    if name == "browser.tabs" {
        return Ok(
            json!({"sessionId":id,"tabs":tabs,"activeTabId":session.active_tab,"executionEnvironment":environment(&session)}),
        );
    }
    if name == "browser.new" {
        if session.embedded_target.is_some() {
            return Err(failure(
                "BROWSER_EMBEDDED_TAB_REQUIRED",
                "内嵌浏览器使用 browser.open 切换当前网页；通过 artifact.open 可增加网页标签。",
                409,
            ));
        }
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
        let tabs = update_tabs(&mut session, ctx).await?;
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
    if ["browser.reload", "browser.wait", "browser.navigate"].contains(&name) {
        let destination = (name == "browser.navigate").then(|| input["url"].as_str()).flatten();
        if let Some(handoff) = inspect_verification_handoff(&socket, destination, ctx).await? {
            return Ok(json!({"sessionId":id,"tabId":tab,"value":handoff,"executionEnvironment":environment(&session)}));
        }
    }
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
            // Readability works on a clone inside the browser; no Node host or page mutation is involved.
            // 正文提取仅在浏览器内处理文档副本，不增加 Node 宿主，也不改变用户页面。
            let expression = format!("(() => {{ const module = undefined; const exports = undefined; {} const snapshot = ({})({maximum}); return Object.assign(snapshot, {}); }})()",
                include_str!("../../../official-tools/Browser/readability/Readability.js"),
                include_str!("browser_snapshot.js"),
                include_str!("browser_page_state.js"));
            evaluate(&socket, expression, ctx).await?
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
            if session.embedded_target.is_some() {
                rpc(&socket, "Page.navigate", json!({"url":"about:blank"}), ctx).await?;
                return Ok(json!({"closed":true,"sessionId":id,"tabId":tab,"embedded":true}));
            }
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
    let _ = shutdown_checked(ctx).await;
}

pub(crate) async fn shutdown_checked(ctx: &GatewayToolContext) -> Result<(), GatewayError> {
    let mut workspace = ctx.runtime.workspace_browser.lock().await;
    if let Some(browser) = workspace.as_ref() { browser.lock().await.close(ctx).await?; }
    workspace.take();
    drop(workspace);
    let connections = ctx
        .runtime
        .cdp
        .lock()
        .await
        .iter()
        .map(|(id, connection)| (id.clone(), connection.clone()))
        .collect::<Vec<_>>();
    for (id, connection) in connections {
        connection.close().await?;
        ctx.runtime.cdp.lock().await.remove(&id);
    }
    let sessions = ctx
        .runtime
        .browsers
        .lock()
        .await
        .iter()
        .map(|(id, session)| (id.clone(), session.clone()))
        .collect::<Vec<_>>();
    for (id, session) in sessions {
        let mut session = session.lock().await;
        session.close_owned_process().await?;
        for upload in &session.uploads {
            clean_upload(upload, ctx);
        }
        session.remove_owned_profile().await?;
        ctx.runtime.browsers.lock().await.remove(&id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_workspace_page_keeps_requested_presentation_path_without_claiming_navigation() {
        let mut state = json!({"url":"about:blank","requestedUrl":"https://example.org/requested", "navigationCommitted":false});
        assert_eq!(workspace_presentation_path(&state), "https://example.org/requested");
        assert_eq!(state["url"], "about:blank");
        assert_eq!(state["navigationCommitted"], false);
        state["url"] = json!("https://example.org/redirected");
        assert_eq!(workspace_presentation_path(&state), "https://example.org/redirected");
        state["url"] = Value::Null;
        assert_eq!(workspace_presentation_path(&state), "https://example.org/requested");
    }

    #[cfg(windows)]
    fn use_bundled_fixture(ctx: &mut GatewayToolContext) -> bool {
        let bundle = std::env::var_os("KYNXA_BROWSER_BUNDLE_ROOT").map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/runtime/native-desktop/win-x64"));
        ctx.config["officialToolsRoot"] = json!(bundle.join("official-tools"));
        chrome_candidates(ctx).iter().any(|path| path.is_file())
    }
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workspace_chrome_host_navigates_only_its_conversation_and_closes_process() {
        use axum::{Router, response::Html, routing::get};
        let (mut ctx, _sender) = super::super::gateway::test_context();
        if !use_bundled_fixture(&mut ctx) {
            eprintln!("Bundled Chromium fixture unavailable; prepare the product payload or set KYNXA_BROWSER_BUNDLE_ROOT.");
            let _ = std::fs::remove_dir_all(ctx.workdir.parent().unwrap());
            return;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/", get(|| async { Html("<title>First</title><h1>first</h1><a id='popup' target='_blank' href='/popup'>Open video</a><script>window.savedState='parent-preserved'</script>") }))
                .route("/next", get(|| async { Html("<title>Second</title><h1>second</h1>") }))
                .route("/popup", get(|| async { Html("<title>Video page</title><h1>popup</h1>") }))
                .route("/opened", get(|| async { Html("<title>Opened page</title><h1>opened</h1>") }))).await.unwrap();
        });
        let opened = super::super::gateway::execute("artifact.open",
            json!({"url":format!("http://{address}/"),"title":"First"}), &ctx).await.unwrap();
        assert!(opened["processId"].as_u64().is_some());
        assert_eq!(opened["requestedUrl"], format!("http://{address}/"));
        {
            let browser = ctx.runtime.workspace_browser.lock().await.as_ref().unwrap().clone();
            let targets = browser.lock().await.client.call("Target.getTargets", json!({}), &ctx).await.unwrap();
            assert_eq!(targets["targetInfos"].as_array().unwrap().iter().filter(|target| target["type"] == "page").count(), 1,
                "The startup blank page must be consumed, not left behind in another window.");
        }
        let id = opened["sessionId"].as_str().unwrap();
        let discovered = execute("browser.tabs", json!({"reason":"Find the current sidebar page"}), &ctx).await.unwrap();
        assert_eq!(discovered["sessionId"], id);
        assert_eq!(discovered["tabId"], opened["tabId"]);
        assert!(prompt_context(&ctx).await.contains(id));
        let socket = ctx.runtime.browsers.lock().await.get(id).unwrap().clone()
            .lock().await.tabs[opened["tabId"].as_str().unwrap()].clone();
        evaluate(&socket, "window.sidebarContinuity='preserved';true".into(), &ctx).await.unwrap();
        let mut approved = ctx.clone();
        approved.approved = true;
        let connected = super::super::gateway::execute("browser.open",
            json!({"url":format!("http://{address}/"),"reason":"Inspect the existing page"}), &approved).await.unwrap();
        assert_eq!(connected["sessionId"], id);
        assert_eq!(evaluate(&socket, "window.sidebarContinuity".into(), &ctx).await.unwrap(), "preserved");
        let duplicate = workspace_open(&json!({"url":format!("http://{address}/")}), &ctx).await.unwrap();
        assert_eq!(duplicate["sessionId"], id);
        let mut foreign = ctx.clone(); foreign.conversation_id = uuid::Uuid::new_v4().to_string();
        assert!(execute("browser.tabs", json!({"reason":"Discover only this chat"}), &foreign).await.unwrap()["sessions"].as_array().unwrap().is_empty());
        assert_eq!(workspace_action(&json!({"sessionId":id,"action":"state"}), &foreign).await.unwrap_err().code, "BROWSER_SESSION_NOT_FOUND");
        assert_eq!(workspace_action(&json!({"sessionId":id,"action":"evaluate"}), &ctx).await.unwrap_err().code, "WORKSPACE_BROWSER_ACTION_INVALID");
        workspace_action(&json!({"sessionId":id,"action":"navigate","url":format!("http://{address}/next")}), &ctx).await.unwrap();
        let mut current = Value::Null;
        for _ in 0..40 {
            current = workspace_action(&json!({"sessionId":id,"action":"state"}), &ctx).await.unwrap();
            if current["url"].as_str().is_some_and(|url| url.ends_with("/next")) && current["canGoBack"] == true { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(current["canGoBack"], true, "{current}");
        let session = ctx.runtime.browsers.lock().await.get(id).unwrap().clone();
        workspace_action(&json!({"sessionId":id,"action":"back"}), &ctx).await.unwrap();
        for _ in 0..40 {
            current = workspace_action(&json!({"sessionId":id,"action":"state"}), &ctx).await.unwrap();
            if current["url"] == format!("http://{address}/") { break; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(current["url"], format!("http://{address}/"));
        let parent_target = current["tabId"].clone();
        let parent_socket = session.lock().await.tabs[opened["tabId"].as_str().unwrap()].clone();
        rpc(&parent_socket, "Runtime.evaluate", json!({"expression":"document.querySelector('#popup').click()","userGesture":true}), &ctx).await.unwrap();
        for _ in 0..60 {
            current = workspace_action(&json!({"sessionId":id,"action":"state"}), &ctx).await.unwrap();
            if current["url"].as_str().is_some_and(|url| url.ends_with("/popup")) { break; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(current["url"].as_str().unwrap().ends_with("/popup"), "{current}");
        assert_ne!(current["tabId"], parent_target);
        let popup_socket = {
            let session = session.lock().await;
            session.tabs[session.embedded_target.as_ref().unwrap()].clone()
        };
        rpc(&popup_socket, "Runtime.evaluate", json!({"expression":"window.open('/opened','_blank')","userGesture":true}), &ctx).await.unwrap();
        for _ in 0..60 {
            current = workspace_action(&json!({"sessionId":id,"action":"state"}), &ctx).await.unwrap();
            if current["url"].as_str().is_some_and(|url| url.ends_with("/opened")) { break; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(current["url"].as_str().unwrap().ends_with("/opened"), "{current}");
        current = workspace_action(&json!({"sessionId":id,"action":"back"}), &ctx).await.unwrap();
        assert!(current["url"].as_str().unwrap().ends_with("/popup"), "{current}");
        current = workspace_action(&json!({"sessionId":id,"action":"back"}), &ctx).await.unwrap();
        assert_eq!(current["url"], format!("http://{address}/"));
        assert_eq!(evaluate(&parent_socket, "window.savedState".into(), &ctx).await.unwrap(), "parent-preserved");
        current = workspace_action(&json!({"sessionId":id,"action":"forward"}), &ctx).await.unwrap();
        assert!(current["url"].as_str().unwrap().ends_with("/popup"), "{current}");
        evaluate(&parent_socket, "document.cookie='kynxa_login=global-fixture; Max-Age=3600; SameSite=Lax';localStorage.setItem('kynxa_login','global-fixture');true".into(), &ctx).await.unwrap();
        let foreign_page = workspace_open(&json!({"url":format!("http://{address}/next")}), &foreign).await.unwrap();
        assert_eq!(foreign_page["processId"], opened["processId"]);
        assert_ne!(foreign_page["windowId"], current["windowId"]);
        let foreign_id = foreign_page["sessionId"].as_str().unwrap();
        let foreign_handle = ctx.runtime.browsers.lock().await.get(foreign_id).unwrap().clone();
        let foreign_socket = foreign_handle.lock().await.tabs[foreign_page["tabId"].as_str().unwrap()].clone();
        let shared_login = evaluate(&foreign_socket, "({cookie:document.cookie,saved:localStorage.getItem('kynxa_login')})".into(), &foreign).await.unwrap();
        assert!(shared_login["cookie"].as_str().unwrap().contains("kynxa_login=global-fixture"));
        assert_eq!(shared_login["saved"], "global-fixture");
        assert!(!foreign_handle.lock().await.tabs.contains_key(opened["tabId"].as_str().unwrap()));
        assert_eq!(execute("browser.open", json!({"cdpEndpoint":opened["endpoint"]}), &ctx).await.unwrap_err().code,
            "BROWSER_ENDPOINT_SCOPE_REQUIRED");
        assert_eq!(execute("browser.snapshot", json!({"sessionId":foreign_id,"tabId":opened["tabId"]}), &foreign).await.unwrap_err().code,
            "BROWSER_TAB_UNAVAILABLE");
        let browser = session.lock().await.workspace_browser.as_ref().unwrap().clone();
        let profile = browser.lock().await.profile.clone();
        assert_eq!(workspace_action(&json!({"sessionId":id,"action":"close"}), &ctx).await.unwrap()["closed"], true);
        assert!(browser.lock().await.owned.has_running_processes().unwrap());
        let mut cancelled_open = foreign.clone();
        let (_cancelled_sender, cancellation) = tokio::sync::watch::channel(true);
        cancelled_open.cancellation = cancellation;
        let initialization_error = cleanup_failed_workspace_open(foreign_id, &foreign_handle,
            failure("TOOL_CANCELLED", "Fixture opening cancelled after allocation.", 409), &cancelled_open).await;
        assert_eq!(initialization_error.code, "TOOL_CANCELLED");
        assert!(!browser.lock().await.owned.has_running_processes().unwrap());
        assert!(profile.exists());
        let reopened = workspace_open(&json!({"url":format!("http://{address}/")}), &ctx).await.unwrap();
        let reopened_id = reopened["sessionId"].as_str().unwrap();
        let reopened_handle = ctx.runtime.browsers.lock().await.get(reopened_id).unwrap().clone();
        let reopened_socket = reopened_handle.lock().await.tabs[reopened["tabId"].as_str().unwrap()].clone();
        let persisted = evaluate(&reopened_socket, "({cookie:document.cookie,saved:localStorage.getItem('kynxa_login')})".into(), &ctx).await.unwrap();
        assert!(persisted["cookie"].as_str().unwrap().contains("kynxa_login=global-fixture"), "{persisted}");
        assert_eq!(persisted["saved"], "global-fixture");
        let cleared = workspace_action(&json!({"action":"clearSiteData"}), &ctx).await.unwrap();
        assert_eq!(cleared["scope"], "application");
        assert!(cleared["closedSessionIds"].as_array().unwrap().iter().any(|value| value == reopened_id));
        assert!(!profile.exists());
        assert!(ctx.runtime.workspace_browser.lock().await.is_none());
        server.abort();
        shutdown(&ctx).await;
        let _ = std::fs::remove_dir_all(ctx.workdir.parent().unwrap());
    }

    #[tokio::test]
    async fn sidebar_browser_reuses_only_attached_web_target_and_detaches_without_closing_it() {
        use axum::{Router, routing::get};
        let (ctx, _sender) = super::super::gateway::test_context();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service = Router::new().route("/json/list", get(move || async move {
            axum::Json(json!([
                {"id":"trusted-renderer","type":"page","url":"https://kynxa.local/Transcript/index.html","webSocketDebuggerUrl":format!("ws://{address}/devtools/page/trusted-renderer")},
                {"id":"external-page","type":"page","url":"https://example.org/","title":"External page","webSocketDebuggerUrl":format!("ws://{address}/devtools/page/external-page")}
            ]))
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let attached = attach_embedded(&format!("http://{address}"), "external-page", &ctx)
            .await
            .unwrap();
        assert_eq!(attached["tabs"].as_array().unwrap().len(), 1);
        assert_eq!(attached["tabId"], "external-page");
        let reopened = execute(
            "browser.open",
            json!({"reason":"continue visible page"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(attached["sessionId"], reopened["sessionId"]);
        assert_eq!(
            reopened["executionEnvironment"]["browserTarget"]["mode"],
            "embedded-sidebar"
        );
        let id = attached["sessionId"].as_str().unwrap();
        let mut other = ctx.clone();
        other.conversation_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            detach_embedded(id, &other).await.unwrap_err().code,
            "BROWSER_SESSION_NOT_FOUND"
        );
        assert_eq!(
            detach_embedded(id, &ctx).await.unwrap()["externalProcessPreserved"],
            true
        );
        assert!(ctx.runtime.browsers.lock().await.is_empty());
        assert!(
            reqwest::get(format!("http://{address}/json/list"))
                .await
                .unwrap()
                .status()
                .is_success()
        );
        server.abort();
        let _ = std::fs::remove_dir_all(ctx.workdir.parent().unwrap());
    }
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
        if !use_bundled_fixture(&mut ctx) {
            eprintln!("Bundled Chromium fixture unavailable.");
            let _ = std::fs::remove_dir_all(ctx.workdir.parent().unwrap()); return;
        }
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
        let browser_path = executable(&json!({}), &ctx).unwrap();
        let opened=execute("browser.open",json!({"browserPath":browser_path,"url":format!("http://{address}/native"),"reason":"Owned headless native regression"}),&ctx).await.unwrap();
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
        let locked = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .share_mode(0)
            .open(profile.join("owned-cleanup-fixture.lock"))
            .unwrap();
        let closed = execute(
            "browser.close",
            json!({"sessionId":session,"reason":"Close owned fixture"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(closed["closed"], true);
        assert_eq!(closed["profileCleanupPending"], true);
        assert!(
            !handle
                .lock()
                .await
                ._owned
                .as_ref()
                .unwrap()
                .has_running_processes()
                .unwrap()
        );
        drop(locked);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(35);
        loop {
            let state = handle.lock().await;
            if !state.profile_cleanup_pending {
                assert!(
                    state.profile_cleanup_error.is_none(),
                    "{:?}",
                    state.profile_cleanup_error
                );
                break;
            }
            drop(state);
            assert!(
                tokio::time::Instant::now() < deadline,
                "Owned profile cleanup did not complete"
            );
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
    fn verification_handoff_stops_model_retries_without_blocking_other_sites() {
        let google = json!({"needsHumanVerification":true,"verificationProvider":"google",
            "verificationUrl":"https://www.google.com/sorry/index?continue=search","evidenceUsable":false});
        let handoff = verification_handoff(&google, None).unwrap();
        assert_eq!(handoff["actionDispatched"], false);
        assert_eq!(handoff["recommendedTool"], "web.search");
        assert!(verification_handoff(&google, Some("https://www.google.com/search?q=another+query")).is_some());
        assert!(verification_handoff(&google, Some("https://www.google.com/sorry/index?continue=search")).is_some());
        assert!(verification_handoff(&google, Some("https://example.org/article")).is_none());
        assert!(verification_handoff(&json!({"needsHumanVerification":false}), None).is_none());
        let cloudflare = json!({"needsHumanVerification":true,"verificationProvider":"cloudflare",
            "verificationUrl":"https://example.org/article","evidenceUsable":false});
        assert!(verification_handoff(&cloudflare, Some("https://example.org/article#details")).is_some());
        assert!(verification_handoff(&cloudflare, Some("https://example.org/other")).is_none());
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
