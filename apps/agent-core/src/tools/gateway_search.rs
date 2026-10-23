//! Unified configured search, source cooldowns and conversation-scoped request sharing.
//! 统一已配置的网页搜索、来源冷却与聊天内请求合并；不扩大权限或改写原始查询。

use super::gateway::{GatewayToolContext, check_cancelled, failure, wait_for_cancellation};
use crate::gateway_contract::GatewayError;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_SEARCH_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Sources tried for one query, including the built-in last-resort source.
/// 一次查询最多尝试的来源数，包含内置最后备用来源。
const MAX_SEARCH_SOURCES: usize = 8;

type CachedSearch = Arc<Mutex<SharedSearch>>;

#[derive(Default)]
struct SharedSearch {
    cached: Option<(Instant, Value)>,
    pending: Option<Arc<PendingSearch>>,
}

struct PendingSearch {
    reply: tokio::sync::watch::Sender<Option<Result<Value, GatewayError>>>,
    cancel: tokio::sync::watch::Sender<bool>,
    waiters: AtomicUsize,
}

struct SearchWaiter(Arc<PendingSearch>);

impl Drop for SearchWaiter {
    fn drop(&mut self) {
        // Keep shared reads for remaining waiters; the last waiter cancels the network task.
        // 仍有等待者就保留共享读取；最后一位离开时取消网络任务。
        if self.0.waiters.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.0.cancel.send(true);
        }
    }
}

/// Runtime-owned cooldowns and bounded, conversation-scoped query sharing.
/// 冷却与查询合并归运行时所有；缓存有界并按聊天和授权快照隔离。
#[derive(Default)]
pub(crate) struct SearchState {
    cooldowns: Mutex<HashMap<String, (Instant, Duration, GatewayError)>>,
    requests: Mutex<HashMap<String, CachedSearch>>,
}

impl SearchState {
    fn key(value: &Value) -> String {
        super::local_files::sha256(value.to_string().as_bytes())
    }

    fn provider_key(server: &Value) -> String {
        // Names, aliases and timeout edits do not create a new upstream quota identity.
        // 名称、别名和超时设置变化不能产生新的远端配额身份。
        Self::key(
            &json!({"url":server["url"],"command":server["command"],"args":server["args"],
            "env":server["env"],"envRefs":server["envRefs"],"headers":server["headers"],"headerEnv":server["headerEnv"],"auth":server["auth"]}),
        )
    }

    fn configuration_key(server: &Value) -> String {
        let mut identity = server.clone();
        if let Some(identity) = identity.as_object_mut() {
            identity.remove("tools");
        }
        Self::key(&identity)
    }

    pub(crate) fn cooling(&self, server: &Value) -> Option<GatewayError> {
        let mut cooldowns = self
            .cooldowns
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cooldowns.retain(|_, (started, duration, _)| started.elapsed() < *duration);
        let (started, duration, error) = cooldowns.get(&Self::provider_key(server))?;
        Some(
            error.clone().with_retry_after(Some(
                duration.saturating_sub(started.elapsed()).as_millis() as u64,
            )),
        )
    }

    fn failed(&self, server: &Value, error: &GatewayError) {
        let cooldown_ms = error.retry_after_ms.unwrap_or(match error.code.as_str() {
            "WEB_SEARCH_RATE_LIMITED" => 60_000,
            "WEB_SEARCH_AUTH_FAILED" | "WEB_SEARCH_CREDENTIAL_MISSING" => 300_000,
            "WEB_SEARCH_QUOTA_EXHAUSTED" => 300_000,
            "MCP_CONNECTION_CAPACITY" => 2000,
            "WEB_SEARCH_TRANSIENT" => 2000,
            _ => 30_000,
        });
        let mut cooldowns = self
            .cooldowns
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cooldowns.retain(|_, (started, duration, _)| started.elapsed() < *duration);
        // Do not evict an unexpired rate limit to make a service immediately eligible again.
        // 未到期的限流记录不能为腾缓存而删除，再次请求会重新收到并保留真实限制。
        if cooldowns.len() < 128 || cooldowns.contains_key(&Self::provider_key(server)) {
            cooldowns.insert(
                Self::provider_key(server),
                (
                    Instant::now(),
                    Duration::from_millis(cooldown_ms),
                    error.clone(),
                ),
            );
        }
    }

    fn shared(&self, key: String) -> CachedSearch {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(request) = requests.get(&key) {
            return request.clone();
        }
        requests.retain(|_, request| {
            let state = request.lock().unwrap_or_else(|error| error.into_inner());
            state.pending.is_some()
                || state
                    .cached
                    .as_ref()
                    .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(120))
        });
        if requests.len() >= 32 {
            if let Some(oldest) = requests
                .iter()
                .filter_map(|(key, request)| {
                    let state = request.lock().unwrap_or_else(|error| error.into_inner());
                    if state.pending.is_some() {
                        return None;
                    }
                    Some((
                        key.clone(),
                        state
                            .cached
                            .as_ref()
                            .map(|(at, _)| *at)
                            .unwrap_or_else(Instant::now),
                    ))
                })
                .min_by_key(|(_, at)| *at)
                .map(|(key, _)| key)
            {
                requests.remove(&oldest);
            } else {
                return Arc::new(Mutex::new(SharedSearch::default()));
            }
        }
        requests
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(SharedSearch::default())))
            .clone()
    }

    pub(crate) fn clear_queries(&self) {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for request in requests.values() {
            let state = request.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(pending) = &state.pending {
                let _ = pending.cancel.send(true);
            }
        }
        requests.clear();
        // Configuration invalidates shared work, not an upstream rate-limit cooldown.
        // 配置变化撤销共享查询，但不能清除远端限流的真实冷却记录。
    }
}

fn search_failure(error: GatewayError) -> GatewayError {
    if [
        "TOOL_CANCELLED",
        "MCP_STOPPED",
        "MCP_START_CANCELLED",
        "MCP_SHUTDOWN",
        "RESOURCE_CANCELLED",
    ]
    .contains(&error.code.as_str())
    {
        return error;
    }
    if error.code == "MCP_CONNECTION_CAPACITY" || error.details["failureOrigin"] == "local-capacity" {
        let mut error = error;
        error.details["failureOrigin"] = json!("local-capacity");
        return error;
    }
    let http_status = error.details["httpStatus"].as_u64();
    let transport_failure = super::gateway_mcp::transient_connection_failure(&error.code);
    let quota = error.details["providerCode"].as_str().is_some_and(|code| matches!(
        code.to_ascii_uppercase().as_str(),
        "INSUFFICIENT_QUOTA" | "QUOTA_EXCEEDED" | "CREDITS_EXHAUSTED"
            | "INSUFFICIENT_CREDITS" | "PAYMENT_REQUIRED" | "CREDIT_BALANCE_EXHAUSTED"
    ));
    let code = if http_status == Some(402) || quota {
        "WEB_SEARCH_QUOTA_EXHAUSTED"
    } else if http_status == Some(429) {
        "WEB_SEARCH_RATE_LIMITED"
    } else if matches!(http_status, Some(401 | 403)) || error.code == "WEB_SEARCH_CREDENTIAL_MISSING" {
        "WEB_SEARCH_AUTH_FAILED"
    } else if http_status.is_some_and(|status| status >= 500) || transport_failure
        || [
            "WEB_SEARCH_TRANSIENT",
            "WEB_SEARCH_FAILED",
            "MCP_EXECUTION_UNKNOWN",
        ]
        .contains(&error.code.as_str())
    {
        "WEB_SEARCH_TRANSIENT"
    } else {
        return error;
    };
    let mut details = error.details;
    if transport_failure {
        details["failureOrigin"] = json!("mcp-transport");
        details["causeCode"] = json!(error.code);
    }
    failure(
        code,
        match code {
            "WEB_SEARCH_RATE_LIMITED" => "搜索服务限流，已保留服务要求的冷却时间。",
            "WEB_SEARCH_QUOTA_EXHAUSTED" => "搜索服务明确返回额度或付费限制；未重复请求该来源。",
            "WEB_SEARCH_AUTH_FAILED" => "搜索服务认证或访问权限未满足。",
            _ => "搜索服务暂时不可用，已有结果与其他任务仍可继续。",
        },
        if code == "WEB_SEARCH_QUOTA_EXHAUSTED" { 402 } else { error.status },
    )
    .with_retry_after(error.retry_after_ms)
    .with_details(details)
}

fn tool_failure(value: &Value) -> Option<GatewayError> {
    let has_error = |value: &Value| value.get("error").is_some_and(|error| !error.is_null() && error != &Value::Bool(false));
    if value["isError"] != true && value["structuredContent"]["isError"] != true
        && !has_error(value) && !has_error(&value["structuredContent"]) {
        return None;
    }
    // MCP timeouts and disconnects return unknown results; read-only search may safely retry.
    // MCP 超时或断线返回未知结果；只读搜索可安全重试。
    if value["code"] == "MCP_EXECUTION_UNKNOWN"
        && matches!(value["causeCode"].as_str(), Some("MCP_TIMEOUT" | "MCP_CONNECTION_LOST" | "MCP_CONNECTION_CLOSED"))
    {
        return Some(failure("WEB_SEARCH_TRANSIENT", "搜索服务调用超时或连接中断，可重试或改用备用来源。", 504)
            .with_details(json!({"failureOrigin":"mcp-transport","causeCode":value["causeCode"]})));
    }
    let details = super::gateway_mcp::provider_failure_details(value, "provider-tool");
    // An unspecified tool error is a failed dependency, not an invented upstream HTTP 502.
    // 未提供状态的工具失败属于依赖失败，不伪造远端 HTTP 502 并据此重试。
    let status = details["httpStatus"].as_u64().unwrap_or(424) as u16;
    Some(failure("WEB_SEARCH_TOOL_FAILED", "搜索服务返回失败回执，未将其当作搜索结果。", status)
        .with_retry_after(details["retryAfterMs"].as_u64())
        .with_details(details))
}

fn failure_receipt(error: &GatewayError) -> Value {
    let mut receipt = error.response();
    receipt["message"] = json!(error.message);
    receipt["gatewayStatus"] = json!(error.status);
    if let Some(status) = error.details.get("httpStatus") {
        receipt["httpStatus"] = status.clone();
    }
    receipt
}

fn cacheable(value: &Value) -> bool {
    struct LimitedSize(usize);
    impl std::io::Write for LimitedSize {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > 256 * 1024 {
                return Err(std::io::Error::other("cache size limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // Measure serialized size without copying the full result.
    // 计算序列化大小，不复制完整结果。
    serde_json::to_writer(LimitedSize(0), value).is_ok()
}

fn brave_source(source: &Value) -> Value {
    let mut projected = json!({
        "title": source["title"],
        "url": source["url"],
        "snippet": source["description"],
    });
    super::service_web_sources::preserve_temporal_metadata(source, &mut projected);
    projected
}

async fn search_provider(
    input: &Value,
    server: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    if ctx.config["retrieval"]["web"]["mode"] == "off" {
        return Err(failure("WEB_SEARCH_DISABLED", "网页检索已关闭。", 409));
    }
    let query = input["query"]
        .as_str()
        .filter(|query| !query.trim().is_empty())
        .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "搜索 query 不能为空。", 400))?;
    let count = input["limit"].as_u64().unwrap_or(10).clamp(1, 20);
    if server["builtin"] == super::gateway_deepseek_search::SOURCE_ID {
        return super::gateway_deepseek_search::search(query, count, server, ctx).await;
    }
    let brave =server["id"].as_str().is_some_and(|id| id.contains("brave"));
    let command = std::path::Path::new(server["command"].as_str().unwrap_or(""))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();
    if brave && ["npx", "npx.cmd", "npm", "npm.cmd", "node", "node.exe"].contains(&command.as_str())
    {
        if server["disabledTools"]
            .as_array()
            .is_some_and(|tools| tools.contains(&json!("brave_web_search")))
        {
            return Err(failure(
                "WEB_SEARCH_PROVIDER_UNAVAILABLE",
                "Brave 搜索工具已禁用。",
                403,
            ));
        }
        let key = std::env::var("KYNXA_BRAVE_API_KEY").map_err(|_| {
            failure(
                "WEB_SEARCH_CREDENTIAL_MISSING",
                "Brave API key 未配置。",
                401,
            )
        })?;
        let mut url = url::Url::parse("https://api.search.brave.com/res/v1/web/search")
            .expect("fixed search endpoint");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("count", &count.to_string());
        let transport = super::gateway_web_proxy::client(&url, Duration::from_secs(30)).await?;
        let response = transport
            .client
            .get(url)
            .header("X-Subscription-Token", key)
            .send()
            .await
            .map_err(|_| failure("WEB_SEARCH_FAILED", "Brave 搜索请求失败。", 502))?;
        if !response.status().is_success() {
            return Err(failure(
                "WEB_SEARCH_HTTP_REJECTED",
                "Brave 搜索被服务拒绝。",
                response.status().as_u16(),
            )
            .with_retry_after(super::gateway_mcp::retry_after_ms(response.headers()))
            .with_details(json!({"failureOrigin":"provider-http","httpStatus":response.status().as_u16()})));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| failure("WEB_SEARCH_FAILED", "Brave 搜索读取中断。", 502))?;
            if bytes.len() + chunk.len() > MAX_SEARCH_RESPONSE_BYTES {
                return Err(failure(
                    "WEB_RESPONSE_TOO_LARGE",
                    "Brave 搜索响应超出读取边界。",
                    413,
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| failure("WEB_SEARCH_INVALID_RESULT", "Brave 搜索回执无效。", 502))?;
        let sources = value["web"]["results"]
            .as_array()
            .into_iter()
            .flatten()
            .take(count as usize)
            .map(brave_source)
            .collect::<Vec<_>>();
        return Ok(json!({
            "query": query,
            "providerId": server["id"],
            "sources": sources,
            "executionEnvironment": super::contracts::calculate(
                "tools.resolveEnvironment",
                &json!({"descriptor": {"name": "web.fetch", "source": "builtin"}}),
            ).map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500))?,
        }));
    }
    // A catalog view tolerates unavailable extensions; a selected search needs the actual handshake error.
    // 工具目录允许单个扩展不可用；选定搜索必须保留真实握手失败及冷却，而不是误报不存在。
    super::gateway_mcp::connection(server, ctx).await?;
    let descriptors = super::gateway_mcp::catalog_selected(ctx, vec![server.clone()]).await?;
    let plan = super::gateway::native(
        ctx,
        "web.providerPlan",
        json!({
            "descriptors": descriptors,
            "providerId": server["id"],
            "depth": if count > 6 {
                "deep"
            } else {
                ctx.config["retrieval"]["web"]["depth"].as_str().unwrap_or("standard")
            },
            "language": ctx.config["retrieval"]["web"]["language"].as_str().unwrap_or("auto"),
            "query": query,
            "reason": input["reason"],
            "conversationId": ctx.conversation_id,
            "limit": count,
        }),
    )
    .await?;
    let tool = descriptors[plan["index"].as_u64().unwrap_or(0) as usize].clone();
    let value = super::gateway_mcp::execute(
        tool["name"].as_str().unwrap_or(""),
        json!({
            "arguments": plan["arguments"],
            "policy": {
                "reason": input["reason"].as_str().unwrap_or("Public web search requested by current user"),
            },
        }),
        ctx,
    ).await?;
    if let Some(error) = tool_failure(&value) {
        return Err(error);
    }
    let sources = super::service_web_sources::normalize_result(
        value.get("structuredContent").unwrap_or(&value),
        count as usize,
    );
    Ok(json!({
        "query": query,
        "providerId": server["id"],
        "canonical": value,
        "content": value["content"],
        "isError": value["isError"],
        "status": value["status"],
        "code": value["code"],
        "sources": sources,
        "executionEnvironment": super::contracts::calculate(
            "tools.resolveEnvironment",
            &json!({"descriptor": tool, "server": server}),
        ).map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500))?,
    }))
}

async fn search_inner(input: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    if ctx.config["retrieval"]["web"]["mode"] == "off" {
        return Err(failure("WEB_SEARCH_DISABLED", "网页检索已关闭。", 409));
    }
    input["query"]
        .as_str()
        .filter(|query| !query.trim().is_empty())
        .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "搜索 query 不能为空。", 400))?;
    let selected = ctx.config["retrieval"]["web"]["providerId"]
        .as_str()
        .unwrap_or("auto");
    let servers = search_sources_for(selected, ctx).await;
    if servers.is_empty() {
        return Err(failure(
            "WEB_SEARCH_PROVIDER_UNAVAILABLE",
            "选定搜索服务未启用；未绕过设置启用其他远程服务。",
            503,
        ));
    }
    let key = SearchState::key(&json!({
        "conversation":ctx.conversation_id,"project":ctx.project_id,"permission":ctx.permission_mode,
        "roots":ctx.authorized_roots,"workdir":ctx.workdir,
        "approved":ctx.approved,"provider":selected,"servers":servers.iter().map(SearchState::configuration_key).collect::<Vec<_>>(),
        "query":input["query"],"limit":input["limit"].as_u64().unwrap_or(10).clamp(1,20),
        "objective":input["reason"].as_str().filter(|reason| !reason.trim().is_empty()).unwrap_or_else(||input["query"].as_str().unwrap_or("")),
        "language":ctx.config["retrieval"]["web"]["language"],"depth":ctx.config["retrieval"]["web"]["depth"],
    }));
    search_shared(input, servers, key, ctx).await
}

/// Ordered sources for one query: the selected or all configured search services, then the built-in fallback.
/// 一次查询的有序来源：选定或全部已配置搜索服务在前，内置备用来源在后。
async fn search_sources_for(selected: &str, ctx: &GatewayToolContext) -> Vec<Value> {
    let loaded = ctx.runtime.schemas.read().await.clone();
    let mut servers = super::gateway_mcp::servers(ctx)
        .into_iter()
        .filter_map(|server| {
            // Discovery metadata must not change the connection's configuration identity and force another handshake.
            // 发现目录仅用于筛选能力，不能混入连接配置身份，否则执行时会无故重新握手。
            let mut discovery = server.clone();
            discovery["tools"] = json!(
                loaded
                    .iter()
                    .filter(|tool| tool["serverId"] == server["id"])
                    .cloned()
                    .collect::<Vec<_>>()
            );
            (super::service_web::search_server(&discovery)
                && (selected == "auto" || server["id"] == selected))
                .then_some(server)
        })
        .collect::<Vec<_>>();
    // Use configured official DeepSeek search last, after other sources fail, cool down or exhaust quota.
    // 官方 DeepSeek 搜索放在最后，仅当前序来源失败、冷却或额度耗尽时使用。
    if selected == "auto" || selected == super::gateway_deepseek_search::SOURCE_ID {
        if let Some(source) = super::gateway_deepseek_search::source(ctx).await {
            servers.truncate(MAX_SEARCH_SOURCES - 1);
            servers.push(source);
        }
    }
    servers
}

async fn search_shared(
    input: Value,
    servers: Vec<Value>,
    key: String,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let request = ctx.runtime.web_search.shared(key);
    let (pending, should_start) = {
        let mut state = request.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((at, value)) = state
            .cached
            .as_ref()
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(120))
        {
            let mut value = value.clone();
            value["cache"] =
                json!({"hit":true,"scope":"conversation","ageMs":at.elapsed().as_millis()});
            return Ok(value);
        }
        if let Some(pending) = state.pending.as_ref().filter(|pending| {
            !*pending.cancel.borrow()
                && pending
                    .waiters
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        if count > 0 {
                            count.checked_add(1)
                        } else {
                            None
                        }
                    })
                    .is_ok()
        }) {
            (pending.clone(), false)
        } else {
            let (reply, _) = tokio::sync::watch::channel(None);
            let (cancel, _) = tokio::sync::watch::channel(false);
            let pending = Arc::new(PendingSearch {
                reply,
                cancel,
                waiters: AtomicUsize::new(1),
            });
            state.pending = Some(pending.clone());
            (pending, true)
        }
    };
    let _waiter = SearchWaiter(pending.clone());
    let mut reply = pending.reply.subscribe();
    if should_start {
        let pending = pending.clone();
        let mut operation_ctx = ctx.clone();
        operation_ctx.cancellation = pending.cancel.subscribe();
        // Shared native transactions must not retain the first caller's independently cancellable request token.
        // 共享原生事务不能继续使用首位调用者可独立取消的请求令牌。
        operation_ctx.config["nativeCancellationId"] = json!(uuid::Uuid::new_v4().to_string());
        tokio::spawn(async move {
            let mut cancelled = operation_ctx.cancellation.clone();
            let result = tokio::select! {
                result=tokio::time::timeout(Duration::from_secs(60), search_sources(input, servers, &operation_ctx))=> {
                    result.unwrap_or_else(|_|Err(failure("WEB_TIMEOUT","网页搜索总预算已到期。",504)))
                },
                _=wait_for_cancellation(&mut cancelled)=>Err(failure("TOOL_CANCELLED","网页搜索已取消。",409)),
            };
            let mut state = request.lock().unwrap_or_else(|error| error.into_inner());
            if state
                .pending
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &pending))
            {
                if let Ok(value) = &result {
                    if cacheable(value) {
                        state.cached = Some((Instant::now(), value.clone()));
                    }
                }
                state.pending = None;
            }
            // send_replace retains completion even if a waiter subscribes just after the HTTP result arrived.
            // 保留完成结果，避免 HTTP 先返回、等待者稍后订阅时错过通知。
            pending.reply.send_replace(Some(result));
        });
    }
    loop {
        if let Some(result) = reply.borrow_and_update().clone() {
            return result;
        }
        reply
            .changed()
            .await
            .map_err(|_| failure("WEB_SEARCH_TRANSIENT", "共享搜索任务已结束。", 502))?;
    }
}

async fn search_sources(
    input: Value,
    servers: Vec<Value>,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let mut attempts = Vec::new();
    let mut last_error = None;
    let mut retries = Vec::new();
    for (index, server) in servers.iter().enumerate().take(MAX_SEARCH_SOURCES) {
        if let Some(error) = ctx.runtime.web_search.cooling(server) {
            let mut receipt = failure_receipt(&error);
            receipt["providerId"] = server["id"].clone();
            receipt["status"] = json!("cooldown");
            attempts.push(receipt);
            last_error = Some(error);
            continue;
        }
        check_cancelled(ctx)?;
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            search_provider(&input, server, ctx),
        )
        .await
        .unwrap_or_else(|_| {
            Err(failure(
                "WEB_SEARCH_TRANSIENT",
                "当前搜索服务在本次等待预算内未就绪。",
                504,
            ))
        });
        match result {
            Ok(mut value) => {
                attempts.push(json!({"providerId":server["id"],"status":"completed"}));
                value["attempts"] = json!(attempts);
                value["cache"] = json!({"hit":false,"scope":"conversation","ageMs":0});
                return Ok(value);
            }
            Err(error) => {
                let error = search_failure(error);
                if [
                    "TOOL_CANCELLED",
                    "MCP_STOPPED",
                    "MCP_START_CANCELLED",
                    "MCP_SHUTDOWN",
                    "RESOURCE_CANCELLED",
                ]
                .contains(&error.code.as_str())
                {
                    return Err(error);
                }
                let mut receipt = failure_receipt(&error);
                ctx.runtime.mcp_errors.lock().await.insert(
                    server["id"].as_str().unwrap_or("").to_owned(), receipt.clone());
                if error.status == 400 {
                    return Err(error);
                }
                receipt["providerId"] = server["id"].clone();
                receipt["status"] = json!("failed");
                attempts.push(receipt);
                if error.code == "WEB_SEARCH_TRANSIENT"
                    && error.retry_after_ms.unwrap_or(2000) <= 2000
                {
                    retries.push(index);
                }
                ctx.runtime.web_search.failed(server, &error);
                last_error = Some(error);
            }
        }
    }
    // Try allowed alternatives first; at most one short transient retry, never replay rate-limited or unauthorized requests.
    // 先尝试允许的备用来源；最多重试一次短暂故障，限流或认证失败不重放。
    if let Some(index) = retries.first() {
        let server = &servers[*index];
        let delay = ctx
            .runtime
            .web_search
            .cooling(server)
            .and_then(|error| error.retry_after_ms)
            .unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(delay)).await;
        check_cancelled(ctx)?;
        super::gateway_mcp::prepare_read_retry(server, ctx).await?;
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            search_provider(&input, server, ctx),
        )
        .await
        .unwrap_or_else(|_| {
            Err(failure(
                "WEB_SEARCH_TRANSIENT",
                "搜索重试等待预算已到期。",
                504,
            ))
        });
        match result {
            Ok(mut value) => {
                attempts.push(json!({"providerId":server["id"],"status":"completed","retry":true}));
                value["attempts"] = json!(attempts);
                value["cache"] = json!({"hit":false,"scope":"conversation","ageMs":0});
                return Ok(value);
            }
            Err(error) => {
                let error = search_failure(error);
                if [
                    "TOOL_CANCELLED",
                    "MCP_STOPPED",
                    "MCP_START_CANCELLED",
                    "MCP_SHUTDOWN",
                    "RESOURCE_CANCELLED",
                ]
                .contains(&error.code.as_str())
                {
                    return Err(error);
                }
                ctx.runtime.mcp_errors.lock().await.insert(server["id"].as_str().unwrap_or("").to_owned(),
                    failure_receipt(&error));
                if error.status == 400 {
                    return Err(error);
                }
                ctx.runtime.web_search.failed(server, &error);
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        failure(
            "WEB_SEARCH_PROVIDER_UNAVAILABLE",
            "允许的搜索服务当前都不可用；已有资料仍可继续读取。",
            503,
        )
    }))
}

pub(crate) async fn search(input: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    let mut cancellation = ctx.cancellation.clone();
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(60), search_inner(input, ctx)) => {
            result.unwrap_or_else(|_| {
                Err(failure("WEB_TIMEOUT", "网页搜索总预算已到期。", 504))
            })
        },
        _ = wait_for_cancellation(&mut cancellation) => {
            Err(failure("TOOL_CANCELLED", "网页搜索已取消。", 409))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_timeouts_become_retryable_search_failures_but_tool_errors_do_not() {
        for cause in ["MCP_TIMEOUT", "MCP_START_TIMEOUT", "MCP_CONNECTION_FAILED", "MCP_CONNECTION_LOST", "MCP_CONNECTION_CLOSED"] {
            let error = search_failure(failure(cause, "Pre-dispatch transport fixture.", 502));
            assert_eq!(error.code, "WEB_SEARCH_TRANSIENT", "{cause}");
            assert_eq!(error.details["causeCode"], cause);
        }
        for cause in ["MCP_TIMEOUT", "MCP_CONNECTION_LOST", "MCP_CONNECTION_CLOSED"] {
            let unknown = json!({"isError":true,"status":"unknown","code":"MCP_EXECUTION_UNKNOWN","causeCode":cause,"content":[]});
            let error = search_failure(tool_failure(&unknown).unwrap());
            assert_eq!(error.code, "WEB_SEARCH_TRANSIENT", "{cause}");
            assert!(error.retry_after_ms.unwrap_or(2000) <= 2000, "{cause}");
        }
        let rejected = json!({"isError":true,"content":[{"type":"text","text":"bad query"}]});
        assert_ne!(search_failure(tool_failure(&rejected).unwrap()).code, "WEB_SEARCH_TRANSIENT");
    }

    #[test]
    fn brave_projection_preserves_provider_dates_without_inventing_publication_time() {
        let source = brave_source(&json!({
            "url":"https://example.org/news", "title":"News", "description":"Original snippet",
            "age":"2 days ago", "page_age":"2026-10-15T08:00:00Z", "fetchedAt":"2026-10-18T09:00:00Z"
        }));
        assert_eq!(source["age"], "2 days ago");
        assert_eq!(source["page_age"], "2026-10-15T08:00:00Z");
        assert_eq!(source["snippet"], "Original snippet");
        assert!(source.get("publishedAt").is_none());
        assert!(source.get("fetchedAt").is_none());
    }

    struct SearchFixture {
        server: Value,
        starts: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        arguments: Arc<Mutex<Vec<Value>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl SearchFixture {
        async fn start(kind: &str, failures: usize, status: u16, delay_ms: u64) -> Self {
            use axum::{Json, Router, body::Body, extract::State, http::Response, routing::post};
            #[derive(Clone)]
            struct FixtureState {
                kind: String,
                failures: usize,
                status: u16,
                delay_ms: u64,
                starts: Arc<AtomicUsize>,
                calls: Arc<AtomicUsize>,
                arguments: Arc<Mutex<Vec<Value>>>,
            }
            async fn handler(
                State(state): State<FixtureState>,
                Json(request): Json<Value>,
            ) -> Response<Body> {
                if request["id"].is_null() {
                    if state.kind == "notification-limit" {
                        return Response::builder()
                            .status(429)
                            .header("Retry-After", "60")
                            .body(Body::empty())
                            .unwrap();
                    }
                    return Response::builder().status(202).body(Body::empty()).unwrap();
                }
                let result = match request["method"].as_str().unwrap_or("") {
                    "initialize" => {
                        state.starts.fetch_add(1, Ordering::AcqRel);
                        json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                    }
                    "tools/list" => json!({"tools":[{
                        "name":if state.kind=="parallel"{"web_search"}else{"web_search_exa"},
                        "inputSchema":if state.kind=="parallel" {
                            json!({"type":"object","properties":{"objective":{"type":"string"},"search_queries":{"type":"array"},"session_id":{"type":"string"}},"required":["objective","search_queries"]})
                        } else if state.kind == "exa-objective" {
                            json!({"type":"object","properties":{
                                "query":{"type":"string","minLength":1},
                                "numResults":{"type":"number"},
                                "objective":{"type":"string","minLength":1,"maxLength":4096}
                            },"required":["query","objective"],"additionalProperties":false})
                        } else {json!({"type":"object","properties":{"query":{"type":"string"},"numResults":{"type":"integer"}},"required":["query"]})}
                    }]}),
                    "tools/call" => {
                        let call = state.calls.fetch_add(1, Ordering::AcqRel);
                        state
                            .arguments
                            .lock()
                            .unwrap()
                            .push(request["params"]["arguments"].clone());
                        if state.delay_ms > 0 {
                            tokio::time::sleep(Duration::from_millis(state.delay_ms)).await;
                        }
                        if ["rpc-quota", "rpc-unknown", "rpc-schema"].contains(&state.kind.as_str()) {
                            let error = match state.kind.as_str() {
                                "rpc-quota" => json!({"code":-32000,"message":"private diagnostic",
                                    "data":{"code":"insufficient_quota","httpStatus":402,"retryAfterMs":60000}}),
                                "rpc-schema" => json!({"code":-32602,"message":"private diagnostic"}),
                                _ => json!({"code":-32000,"message":"quota billing temporary failure without machine evidence"}),
                            };
                            return Response::builder().header("Content-Type", "application/json")
                                .body(Body::from(json!({"jsonrpc":"2.0","id":request["id"],"error":error}).to_string())).unwrap();
                        }
                        if call < state.failures {
                            return Response::builder()
                                .status(state.status)
                                .header("Retry-After", if state.status == 429 { "60" } else { "0" })
                                .body(Body::from("private upstream diagnostic must not leak"))
                                .unwrap();
                        }
                        let sources = if state.kind == "empty" {
                            json!({"results":[]})
                        } else {
                            json!({"results":[{"url":"https://example.org/evidence","title":"Evidence","excerpts":["原始证据 / original evidence"]}]})
                        };
                        match state.kind.as_str() {
                            "nested-quota" => json!({"structuredContent":{"error":{"code":"insufficient_quota"}}}),
                            "tool-unknown" => json!({"isError":true,"content":[{"type":"text","text":"quota exceeded but no machine code"}]}),
                            "nested-rate" => json!({"structuredContent":{"error":{"code":"RATE_LIMITED","statusCode":429,"retryAfterMs":60000}}}),
                            _ => json!({"content":[{"type":"text","text":sources.to_string()}],"structuredContent":sources}),
                        }
                    }
                    _ => json!({}),
                };
                Response::builder()
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string(),
                    ))
                    .unwrap()
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let starts = Arc::new(AtomicUsize::new(0));
            let calls = Arc::new(AtomicUsize::new(0));
            let arguments = Arc::new(Mutex::new(Vec::new()));
            let state = FixtureState {
                kind: kind.into(),
                failures,
                status,
                delay_ms,
                starts: starts.clone(),
                calls: calls.clone(),
                arguments: arguments.clone(),
            };
            let task = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new().route("/mcp", post(handler)).with_state(state),
                )
                .await
                .unwrap()
            });
            Self {
                server: json!({"id":format!("official-{kind}"),"name":kind,"transport":"streamable-http","enabled":true,"trusted":true,
                "url":format!("http://{address}/mcp"),"startupTimeoutMs":3000,"timeoutMs":5000}),
                starts,
                calls,
                arguments,
                task,
            }
        }
    }

    impl Drop for SearchFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn cleanup_search(ctx: &GatewayToolContext) {
        super::super::gateway_mcp::shutdown(ctx).await;
        let root = ctx.workdir.parent().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failure_classification_distinguishes_local_capacity_from_upstream_limits() {
        let local = search_failure(failure("MCP_CONNECTION_CAPACITY", "local", 429));
        assert_eq!(local.code, "MCP_CONNECTION_CAPACITY");
        assert_eq!(local.details["failureOrigin"], "local-capacity");
        assert!(local.details.get("httpStatus").is_none());
        let remote = search_failure(failure("MCP_HTTP_REJECTED", "remote", 429)
            .with_details(json!({"failureOrigin":"provider-http","httpStatus":429}))
            .with_retry_after(Some(12345)));
        assert_eq!(remote.code, "WEB_SEARCH_RATE_LIMITED");
        assert_eq!(remote.retry_after_ms, Some(12345));
        assert_eq!(remote.response()["details"]["httpStatus"], 429);
        let unknown = search_failure(failure("MCP_RPC_ERROR", "quota exceeded", 502));
        assert_eq!(unknown.code, "MCP_RPC_ERROR", "Unstructured prose and a gateway status do not establish upstream transience or quota.");
    }

    #[tokio::test]
    async fn quota_receipts_keep_machine_evidence_without_retry_or_success_cache() {
        for kind in ["exa", "rpc-quota", "nested-quota"] {
            let mut exa = SearchFixture::start(kind, if kind == "exa" { usize::MAX } else { 0 }, 402, 0).await;
            exa.server["id"] = json!("official-exa");
            let (mut ctx, _keep) = super::super::gateway::test_context();
            ctx.config["mcpServers"] = json!([exa.server]);
            let error = search(json!({"query":"quota fixture"}), &ctx).await.unwrap_err();
            assert_eq!(error.code, "WEB_SEARCH_QUOTA_EXHAUSTED", "{kind}");
            assert_eq!(exa.calls.load(Ordering::Acquire), 1, "Quota failures are not transient retries.");
            if kind == "rpc-quota" {
                assert_eq!(error.details["failureOrigin"], "provider-rpc");
                assert_eq!(error.details["rpcCode"], -32000);
                assert_eq!(error.details["providerCode"], "insufficient_quota");
                assert_eq!(error.details["httpStatus"], 402);
                assert_eq!(error.retry_after_ms, Some(60000));
                assert!(!error.response().to_string().contains("private diagnostic"));
            }
            if kind == "nested-quota" {
                assert!(error.details.get("httpStatus").is_none(), "A quota machine code is not an HTTP response.");
            }
            ctx.runtime.web_search.cooldowns.lock().unwrap().clear();
            assert_eq!(search(json!({"query":"quota fixture"}), &ctx).await.unwrap_err().code, "WEB_SEARCH_QUOTA_EXHAUSTED");
            assert_eq!(exa.calls.load(Ordering::Acquire), 2, "A nested error must not become a cached empty success.");
            cleanup_search(&ctx).await;
        }
    }

    #[tokio::test]
    async fn unknown_tool_and_rpc_errors_never_guess_quota_or_retry_while_schema_errors_keep_query() {
        for kind in ["tool-unknown", "rpc-unknown", "rpc-schema"] {
            let mut exa = SearchFixture::start(kind, 0, 200, 0).await;
            exa.server["id"] = json!("official-exa");
            let (mut ctx, _keep) = super::super::gateway::test_context();
            ctx.config["mcpServers"] = json!([exa.server]);
            let error = search(json!({"query":"original query preserved"}), &ctx).await.unwrap_err();
            assert!(!["WEB_SEARCH_TRANSIENT", "WEB_SEARCH_QUOTA_EXHAUSTED"].contains(&error.code.as_str()), "{kind}");
            assert!(error.details.get("httpStatus").is_none());
            assert_eq!(exa.calls.load(Ordering::Acquire), 1);
            assert_eq!(exa.arguments.lock().unwrap()[0]["query"], "original query preserved");
            if kind == "rpc-schema" {
                assert_eq!(error.status, 400);
                assert_eq!(error.details["rpcCode"], -32602);
            }
            cleanup_search(&ctx).await;
        }
    }

    #[tokio::test]
    async fn nested_rate_receipts_expose_remaining_cooldown_without_new_search() {
        let mut exa = SearchFixture::start("nested-rate", 0, 200, 0).await;
        exa.server["id"] = json!("official-exa");
        let (mut ctx, _keep) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server]);
        let error = search(json!({"query":"rate fixture"}), &ctx).await.unwrap_err();
        assert_eq!(error.code, "WEB_SEARCH_RATE_LIMITED");
        assert_eq!(error.retry_after_ms, Some(60000));
        let states = super::super::gateway_mcp::diagnostics(&ctx).await;
        let state = states.iter().find(|state| state["serverId"] == "official-exa").unwrap();
        assert_eq!(state["searchCooldown"]["details"]["httpStatus"], 429);
        assert!(state["searchCooldown"]["retryAfterMs"].as_u64().is_some_and(|remaining| remaining > 0 && remaining <= 60000));
        assert_eq!(exa.calls.load(Ordering::Acquire), 1, "Reading diagnostics does not consume another search.");
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn exa_search_dispatches_required_objective_from_actual_mcp_schema() {
        let exa = SearchFixture::start("exa-objective", 0, 200, 0).await;
        let (mut ctx, _cancel) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server]);
        let result = search(json!({
            "query":"original entity and date", "reason":"Verify the current official release"
        }), &ctx).await.unwrap();
        assert_eq!(result["providerId"], "official-exa-objective");
        assert_eq!(exa.calls.load(Ordering::Acquire), 1);
        let arguments = exa.arguments.lock().unwrap();
        assert_eq!(arguments[0]["query"], "original entity and date");
        assert_eq!(arguments[0]["objective"], "Verify the current official release");
        assert!(arguments[0].get("reason").is_none());
        assert!(arguments[0].get("type").is_none());
        drop(arguments);
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn auto_search_falls_back_respects_cooldown_and_keeps_parallel_session_stable() {
        let exa = SearchFixture::start("exa", usize::MAX, 429, 0).await;
        let parallel = SearchFixture::start("parallel", 0, 200, 0).await;
        let (mut ctx, _cancel) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server, parallel.server]);
        let first = search(json!({"query":"public original query","limit":10}), &ctx)
            .await
            .unwrap();
        assert_eq!(first["providerId"], "official-parallel");
        assert_eq!(
            first["sources"][0]["excerpt"],
            "原始证据 / original evidence"
        );
        assert_eq!(first["attempts"][0]["code"], "WEB_SEARCH_RATE_LIMITED");
        assert_eq!(first["attempts"][0]["retryAfterMs"], 60_000);
        assert!(!first.to_string().contains("private upstream diagnostic"));
        let cached = search(json!({"query":"public original query","limit":10}), &ctx)
            .await
            .unwrap();
        assert_eq!(cached["cache"]["hit"], true);
        assert_eq!(parallel.calls.load(Ordering::Acquire), 1);
        let next = search(json!({"query":"second public query"}), &ctx)
            .await
            .unwrap();
        assert_eq!(next["attempts"][0]["status"], "cooldown");
        assert_eq!(exa.calls.load(Ordering::Acquire), 1);
        let requests = parallel.arguments.lock().unwrap();
        assert_eq!(requests[0]["objective"], "public original query");
        assert_eq!(
            requests[0]["search_queries"],
            json!(["public original query"])
        );
        assert!(requests[0].get("query").is_none());
        assert_eq!(requests[0]["session_id"], requests[1]["session_id"]);
        assert_eq!(requests[0]["session_id"].as_str().unwrap().len(), 64);
        drop(requests);
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn explicit_provider_and_disabled_backup_never_silently_change_sources() {
        let exa = SearchFixture::start("exa", usize::MAX, 429, 0).await;
        let parallel = SearchFixture::start("parallel", 0, 200, 0).await;
        let (mut ctx, _cancel) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server, parallel.server]);
        ctx.config["retrieval"] = json!({"web":{"providerId":"official-exa"}});
        let error = search(json!({"query":"explicit source"}), &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.code, "WEB_SEARCH_RATE_LIMITED");
        assert_eq!(error.retry_after_ms, Some(60_000));
        assert_eq!(parallel.calls.load(Ordering::Acquire), 0);
        ctx.config["retrieval"]["web"]["providerId"] = json!("auto");
        ctx.config["mcpServers"][1]["enabled"] = json!(false);
        assert_eq!(
            search(json!({"query":"backup disabled"}), &ctx)
                .await
                .unwrap_err()
                .code,
            "WEB_SEARCH_RATE_LIMITED"
        );
        assert_eq!(parallel.calls.load(Ordering::Acquire), 0);
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn deepseek_search_is_the_last_automatic_source_and_respects_explicit_choices() {
        let exa = SearchFixture::start("exa", 0, 200, 0).await;
        let parallel = SearchFixture::start("parallel", 0, 200, 0).await;
        let (mut ctx, _cancel) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server, parallel.server]);
        let configured = search_sources_for("auto", &ctx).await;
        assert!(!configured.iter().any(|server| server["id"] == super::super::gateway_deepseek_search::SOURCE_ID),
            "Without an official DeepSeek connection nothing is added.");
        let home = ctx.workdir.parent().unwrap().join("search-model-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("connections.json"), json!({"version":1,"providers":[{"providerId":"deepseek",
            "displayName":"DeepSeek","baseUrl":"https://api.deepseek.com","apiKey":"fixture-official-key",
            "models":["deepseek-flash"],"protocol":"openai-completions"}]}).to_string()).unwrap();
        ctx.runtime.model_home.set(home).unwrap();
        let ordered = search_sources_for("auto", &ctx).await;
        assert_eq!(ordered.len(), configured.len() + 1);
        assert_eq!(&ordered[..configured.len()], configured.as_slice(), "Configured sources keep their order and priority.");
        assert_eq!(ordered.last().unwrap()["id"], super::super::gateway_deepseek_search::SOURCE_ID);
        assert!(!json!(ordered).to_string().contains("fixture-official-key"));
        let explicit = search_sources_for("official-exa", &ctx).await;
        assert!(!explicit.iter().any(|server| server["builtin"].is_string()), "An explicit provider never gains a silent fallback.");
        let selected = search_sources_for(super::super::gateway_deepseek_search::SOURCE_ID, &ctx).await;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["builtin"], super::super::gateway_deepseek_search::SOURCE_ID);
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn transient_read_retries_once_but_authentication_never_retries() {
        let exa = SearchFixture::start("exa", 1, 503, 0).await;
        let (mut ctx, _cancel) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server]);
        assert_eq!(
            search(json!({"query":"transient fixture"}), &ctx)
                .await
                .unwrap()["attempts"][1]["retry"],
            true
        );
        assert_eq!(exa.calls.load(Ordering::Acquire), 2);
        let auth = SearchFixture::start("exa", usize::MAX, 401, 0).await;
        ctx.config["mcpServers"] = json!([auth.server]);
        assert_eq!(
            search(json!({"query":"auth fixture"}), &ctx)
                .await
                .unwrap_err()
                .code,
            "WEB_SEARCH_AUTH_FAILED"
        );
        assert_eq!(auth.calls.load(Ordering::Acquire), 1);
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn closed_cached_connection_is_retired_before_read_retry_but_explicit_stop_is_preserved() {
        let exa = SearchFixture::start("exa", 0, 200, 0).await;
        let (mut ctx, _keep) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server]);
        let server = super::super::gateway_mcp::servers(&ctx).remove(0);
        super::super::gateway_mcp::connection(&server, &ctx).await.unwrap();
        assert_eq!(exa.starts.load(Ordering::Acquire), 1);
        let close = ctx.runtime.mcp_aborters.lock().unwrap().get(server["id"].as_str().unwrap()).unwrap().clone();
        close.send(true).unwrap();
        let value = search(json!({"query":"cached connection closed before dispatch"}), &ctx).await.unwrap();
        assert_eq!(value["attempts"][0]["details"]["causeCode"], "MCP_CLEANUP_PENDING");
        assert_eq!(value["attempts"][1]["retry"], true);
        assert_eq!(exa.starts.load(Ordering::Acquire), 2, "A fresh initialize must replace the closed cached connection.");
        assert_eq!(exa.calls.load(Ordering::Acquire), 1, "Only the reconnected read may be dispatched.");
        super::super::gateway_mcp::stop(server["id"].as_str().unwrap(), &ctx).await.unwrap();
        let error = search(json!({"query":"stopped service must remain stopped"}), &ctx).await.unwrap_err();
        assert_eq!(error.code, "MCP_STOPPED");
        assert_eq!(exa.starts.load(Ordering::Acquire), 2);
        assert_eq!(exa.calls.load(Ordering::Acquire), 1);
        cleanup_search(&ctx).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn handshake_eof_reconnects_read_only_search_once_without_reusing_failed_startup() {
        for recover in [true, false] {
            let (mut ctx, _keep) = super::super::gateway::test_context();
            let root = ctx.workdir.parent().unwrap();
            let script_path = root.join("handshake-search.ps1");
            let marker_path = root.join("handshake-count.txt");
            let script = r#"param([string]$CounterPath,[string]$Recover)
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$starts = if ([IO.File]::Exists($CounterPath)) { [int][IO.File]::ReadAllText($CounterPath) } else { 0 }
[IO.File]::WriteAllText($CounterPath, [string]($starts + 1))
$requestLine = [Console]::ReadLine()
if ($starts -eq 0 -or $Recover -ne 'true') { exit 0 }
while ($null -ne $requestLine) {
    $request = $requestLine | ConvertFrom-Json
    if ($null -ne $request.id) {
        $result = switch ($request.method) {
            'initialize' { @{protocolVersion='2025-11-25';capabilities=@{tools=@{}};serverInfo=@{name='eof-search-fixture';version='1'}} }
            'tools/list' { @{tools=@(@{name='web_search_exa';inputSchema=@{type='object';properties=@{query=@{type='string'};numResults=@{type='integer'}};required=@('query')}})} }
            'tools/call' { @{structuredContent=@{results=@(@{url='https://example.org/reconnected';title='Reconnected fixture';excerpts=@('Actual read after a fresh handshake')})}} }
            default { @{} }
        }
        $reply = @{jsonrpc='2.0';id=$request.id;result=$result} | ConvertTo-Json -Depth 20 -Compress
        [Console]::WriteLine($reply)
    }
    $requestLine = [Console]::ReadLine()
}
"#;
            std::fs::write(&script_path, script).unwrap();
            let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
            ctx.config["mcpServers"] = json!([{"id":"official-exa","name":"EOF search fixture","transport":"stdio","trusted":true,"enabled":true,"command":powershell,"args":["-NoLogo","-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-File",script_path.to_string_lossy(),marker_path.to_string_lossy(),if recover {"true"}else{"false"}],"cwd":root,"startupTimeoutMs":3000,"timeoutMs":5000}]);
            let result = search(json!({"query":"handshake EOF fixture"}), &ctx).await;
            assert_eq!(std::fs::read_to_string(&marker_path).unwrap(), "2", "The bounded read retry must start a fresh MCP process exactly once: {result:?}");
            if recover {
                let value = result.unwrap();
                assert_eq!(value["attempts"][0]["code"], "WEB_SEARCH_TRANSIENT");
                assert_eq!(value["attempts"][0]["details"]["causeCode"], "MCP_CONNECTION_CLOSED");
                assert_eq!(value["attempts"][1]["retry"], true);
                assert_eq!(value["sources"][0]["url"], "https://example.org/reconnected");
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, "WEB_SEARCH_TRANSIENT");
                assert_eq!(error.details["causeCode"], "MCP_CONNECTION_CLOSED");
            }
            cleanup_search(&ctx).await;
        }
    }

    #[tokio::test]
    async fn handshake_notification_limits_keep_cooldown_metadata() {
        let mut exa = SearchFixture::start("notification-limit", 0, 200, 0).await;
        exa.server["id"] = json!("official-exa");
        let (mut ctx, _keep) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server]);
        let error = search(json!({"query":"handshake limit"}), &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.code, "WEB_SEARCH_RATE_LIMITED");
        assert_eq!(error.retry_after_ms, Some(60_000));
        assert_eq!(exa.calls.load(Ordering::Acquire), 0);
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn shared_queries_are_scoped_and_one_cancelled_waiter_preserves_other_waiters() {
        let exa = SearchFixture::start("exa", usize::MAX, 429, 250).await;
        let parallel = SearchFixture::start("parallel", 0, 200, 0).await;
        let (mut ctx, _keep) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server, parallel.server]);
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let mut first_ctx = ctx.clone();
        first_ctx.cancellation = receiver;
        let cancellation_key = uuid::Uuid::new_v4().to_string();
        let native_cancellation =
            crate::service_cancellation::CancellationLease::acquire(&cancellation_key).unwrap();
        first_ctx.config["nativeCancellationId"] = json!(cancellation_key);
        let first =
            tokio::spawn(
                async move { search(json!({"query":"shared fixture"}), &first_ctx).await },
            );
        let second_ctx = ctx.clone();
        let second =
            tokio::spawn(
                async move { search(json!({"query":"shared fixture"}), &second_ctx).await },
            );
        tokio::time::timeout(Duration::from_secs(5), async {
            while exa.calls.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        native_cancellation.token.store(true, Ordering::Release);
        cancel.send(true).unwrap();
        assert_eq!(first.await.unwrap().unwrap_err().code, "TOOL_CANCELLED");
        assert_eq!(
            second.await.unwrap().unwrap()["providerId"],
            "official-parallel"
        );
        assert_eq!(parallel.calls.load(Ordering::Acquire), 1);
        assert_eq!(exa.calls.load(Ordering::Acquire), 1);
        ctx.conversation_id = uuid::Uuid::new_v4().to_string();
        search(json!({"query":"shared fixture"}), &ctx)
            .await
            .unwrap();
        assert_eq!(parallel.calls.load(Ordering::Acquire), 2);
        cleanup_search(&ctx).await;
    }

    #[tokio::test]
    async fn cancelled_search_stops_before_backup_dispatch() {
        let exa = SearchFixture::start("exa", usize::MAX, 429, 250).await;
        let parallel = SearchFixture::start("parallel", 0, 200, 0).await;
        let (mut ctx, cancel) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server, parallel.server]);
        let active_ctx = ctx.clone();
        let active =
            tokio::spawn(
                async move { search(json!({"query":"cancel fixture"}), &active_ctx).await },
            );
        tokio::time::timeout(Duration::from_secs(5), async {
            while exa.calls.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel.send(true).unwrap();
        assert_eq!(active.await.unwrap().unwrap_err().code, "TOOL_CANCELLED");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(parallel.calls.load(Ordering::Acquire), 0);
        cleanup_search(&ctx).await;
    }

    #[test]
    fn retry_after_seconds_dates_and_overflow_are_bounded() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Retry-After", "60".parse().unwrap());
        assert_eq!(
            super::super::gateway_mcp::retry_after_ms(&headers),
            Some(60_000)
        );
        headers.insert("Date", "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap());
        headers.insert(
            "Retry-After",
            "Wed, 21 Oct 2015 07:28:30 GMT".parse().unwrap(),
        );
        assert_eq!(
            super::super::gateway_mcp::retry_after_ms(&headers),
            Some(30_000)
        );
        headers.insert(
            "Retry-After",
            "Fri, 20 Nov 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(
            super::super::gateway_mcp::retry_after_ms(&headers),
            Some(30 * 24 * 60 * 60 * 1000)
        );
        headers.insert("Retry-After", u64::MAX.to_string().parse().unwrap());
        assert_eq!(
            super::super::gateway_mcp::retry_after_ms(&headers),
            Some(u64::MAX)
        );
        headers.insert("Retry-After", "not a date".parse().unwrap());
        assert_eq!(super::super::gateway_mcp::retry_after_ms(&headers), None);
    }

    #[tokio::test]
    async fn empty_results_do_not_switch_sources_and_cache_invalidation_keeps_scope_checks() {
        let mut exa = SearchFixture::start("empty", 0, 200, 0).await;
        exa.server["id"] = json!("official-exa");
        let parallel = SearchFixture::start("parallel", 0, 200, 0).await;
        let (mut ctx, _keep) = super::super::gateway::test_context();
        ctx.config["mcpServers"] = json!([exa.server, parallel.server]);
        assert_eq!(
            search(json!({"query":"empty fixture"}), &ctx)
                .await
                .unwrap()["providerId"],
            "official-exa"
        );
        assert_eq!(parallel.calls.load(Ordering::Acquire), 0);
        ctx.runtime.web_search.clear_queries();
        search(json!({"query":"empty fixture"}), &ctx)
            .await
            .unwrap();
        assert_eq!(exa.calls.load(Ordering::Acquire), 2);
        ctx.permission_mode = if ctx.permission_mode == "smart" {
            "full"
        } else {
            "smart"
        }
        .into();
        search(json!({"query":"empty fixture"}), &ctx)
            .await
            .unwrap();
        assert_eq!(exa.calls.load(Ordering::Acquire), 3);
        ctx.config["mcpServers"][0]["enabled"] = json!(false);
        assert_eq!(
            search(json!({"query":"empty fixture"}), &ctx)
                .await
                .unwrap()["providerId"],
            "official-parallel"
        );
        assert_eq!(parallel.calls.load(Ordering::Acquire), 1);
        cleanup_search(&ctx).await;
    }

    #[test]
    fn parallel_excerpts_remain_source_evidence() {
        let source = json!({"kind":"object","fields":[
            ["url",{"kind":"string","text":"https://example.org/evidence"}],
            ["title",{"kind":"string","text":"Evidence"}],
            ["excerpts",{"kind":"array","items":[{"kind":"string","text":"第一段"},{"kind":"string","text":"Second excerpt"}]}]
        ]});
        let graph = json!({"nodes":[],"root":{"kind":"array","items":[source]}});
        let normalized = super::super::service_web_sources::apply(
            "sources.normalize",
            &json!({"graph":graph,"maximum":6}),
        )
        .unwrap();
        assert_eq!(normalized[0]["excerpt"], "第一段\nSecond excerpt");
    }

    #[test]
    fn credential_scoped_cooldowns_survive_cosmetic_edits_but_not_account_changes() {
        let state = SearchState::default();
        let mut server = json!({"id":"exa","url":"https://mcp.exa.ai/mcp","headers":{"Authorization":"Bearer fixture-account-a"}});
        state.failed(&server, &failure("WEB_SEARCH_AUTH_FAILED", "fixture", 401));
        server["name"] = json!("Renamed search");
        server["startupTimeoutMs"] = json!(45000);
        assert!(state.cooling(&server).is_some());
        state.clear_queries();
        assert!(state.cooling(&server).is_some());
        server["headers"]["Authorization"] = json!("Bearer fixture-account-b");
        assert!(state.cooling(&server).is_none());
        state.failed(
            &server,
            &failure("WEB_SEARCH_RATE_LIMITED", "fixture", 429).with_retry_after(Some(u64::MAX)),
        );
        assert!(state.cooling(&server).unwrap().retry_after_ms.unwrap() > 30 * 24 * 60 * 60 * 1000);
    }

    #[tokio::test]
    #[ignore = "Explicit anonymous public search probe; no user credentials or daily application data"]
    async fn official_parallel_anonymous_search_uses_native_transport() {
        let (mut ctx, _keep) = super::super::gateway::test_context();
        ctx.config["retrieval"] = json!({"web":{"providerId":"official-parallel","mode":"auto"}});
        ctx.config["mcpServers"] = json!([{
            "id":"official-parallel","name":"Parallel Search","transport":"streamable-http","enabled":true,
            "url":"https://search.parallel.ai/mcp","startupTimeoutMs":20000,"timeoutMs":20000,
            "disabledTools":["web_fetch"]
        }]);
        let started = Instant::now();
        let result = search(
            json!({"query":"site:docs.parallel.ai Search MCP documentation","limit":3}),
            &ctx,
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis();
        cleanup_search(&ctx).await;
        let result = result.unwrap();
        assert_eq!(result["providerId"], "official-parallel");
        assert_ne!(result["isError"], true);
        assert!(
            result["content"]
                .as_array()
                .is_some_and(|content| !content.is_empty())
        );
        println!(
            "anonymous Parallel native search: provider={}, elapsedMs={}, contentBlocks={}, cacheHit={}",
            result["providerId"],
            elapsed_ms,
            result["content"].as_array().unwrap().len(),
            result["cache"]["hit"]
        );
    }
    #[tokio::test]
    async fn disabled_search_provider_never_creates_a_fallback_connection() {
        let (mut ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.config["retrieval"] = json!({
            "web": {"providerId": "official-exa", "mode": "auto"},
        });
        ctx.config["mcpServers"] = json!([{
            "id": "official-exa",
            "enabled": false,
            "transport": "streamable-http",
            "url": "http://127.0.0.1:1/mcp",
        }]);
        assert_eq!(
            search(json!({"query":"public fixture"}), &ctx)
                .await
                .unwrap_err()
                .code,
            "WEB_SEARCH_PROVIDER_UNAVAILABLE"
        );
        assert!(ctx.runtime.mcp.lock().await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
