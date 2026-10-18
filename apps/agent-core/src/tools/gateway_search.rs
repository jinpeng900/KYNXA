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
        // A cancelled caller leaves the shared read alive for other waiters; the last waiter cancels its owned network task.
        // 单个调用取消不影响其他等待者；最后一个等待者离开才取消该自有网络任务。
        if self.0.waiters.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.0.cancel.send(true);
        }
    }
}

/// Runtime-owned cooldowns and bounded, conversation-scoped query sharing.
/// 冷却与查询合并归运行时所有；缓存有界并按聊天和授权快照隔离。
#[derive(Default)]
pub(crate) struct SearchState {
    cooldowns: Mutex<HashMap<String, (Instant, Duration, String, u16)>>,
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
        cooldowns.retain(|_, (started, duration, _, _)| started.elapsed() < *duration);
        let (started, duration, code, status) = cooldowns.get(&Self::provider_key(server))?;
        Some(
            failure(
                code,
                "搜索服务仍在冷却；可使用已允许的备用来源或稍后重试。",
                *status,
            )
            .with_retry_after(Some(
                duration.saturating_sub(started.elapsed()).as_millis() as u64,
            )),
        )
    }

    fn failed(&self, server: &Value, error: &GatewayError) {
        let cooldown_ms = error.retry_after_ms.unwrap_or(match error.code.as_str() {
            "WEB_SEARCH_RATE_LIMITED" => 60_000,
            "WEB_SEARCH_AUTH_FAILED" | "WEB_SEARCH_CREDENTIAL_MISSING" => 300_000,
            "WEB_SEARCH_TRANSIENT" => 2000,
            _ => 30_000,
        });
        let mut cooldowns = self
            .cooldowns
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cooldowns.retain(|_, (started, duration, _, _)| started.elapsed() < *duration);
        // Do not evict an unexpired rate limit to make a service immediately eligible again.
        // 未到期的限流记录不能为腾缓存而删除，再次请求会重新收到并保留真实限制。
        if cooldowns.len() < 128 || cooldowns.contains_key(&Self::provider_key(server)) {
            cooldowns.insert(
                Self::provider_key(server),
                (
                    Instant::now(),
                    Duration::from_millis(cooldown_ms),
                    error.code.clone(),
                    error.status,
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
    let code = if error.status == 429 {
        "WEB_SEARCH_RATE_LIMITED"
    } else if error.status == 401 || error.status == 403 {
        "WEB_SEARCH_AUTH_FAILED"
    } else if error.status >= 500
        || [
            "MCP_TIMEOUT",
            "MCP_CONNECTION_LOST",
            "MCP_EXECUTION_UNKNOWN",
        ]
        .contains(&error.code.as_str())
    {
        "WEB_SEARCH_TRANSIENT"
    } else {
        return error;
    };
    failure(
        code,
        match code {
            "WEB_SEARCH_RATE_LIMITED" => "搜索服务限流，已保留服务要求的冷却时间。",
            "WEB_SEARCH_AUTH_FAILED" => "搜索服务认证或访问权限未满足。",
            _ => "搜索服务暂时不可用，已有结果与其他任务仍可继续。",
        },
        error.status,
    )
    .with_retry_after(error.retry_after_ms)
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
    // Measure the serialized bound without allocating another full remote result string.
    // 流式核算序列化上界，不为判断能否缓存再分配一份完整远端结果字符串。
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
    let brave = server["id"].as_str().is_some_and(|id| id.contains("brave"));
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
            .with_retry_after(super::gateway_mcp::retry_after_ms(response.headers())));
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
    if value["isError"] == true {
        let status = value["structuredContent"]["statusCode"]
            .as_u64()
            .or_else(|| value["statusCode"].as_u64())
            .unwrap_or(502) as u16;
        let error = failure(
            "WEB_SEARCH_TOOL_FAILED",
            "搜索服务返回失败回执，未将其当作搜索结果。",
            status,
        );
        return Err(error.with_retry_after(
            value["retryAfterMs"]
                .as_u64()
                .or_else(|| value["structuredContent"]["retryAfterMs"].as_u64()),
        ));
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
    let loaded = ctx.runtime.schemas.read().await.clone();
    let servers = super::gateway_mcp::servers(ctx)
        .into_iter()
        .filter_map(|mut server| {
            server["tools"] = json!(
                loaded
                    .iter()
                    .filter(|tool| tool["serverId"] == server["id"])
                    .cloned()
                    .collect::<Vec<_>>()
            );
            (super::service_web::search_server(&server)
                && (selected == "auto" || server["id"] == selected))
                .then_some(server)
        })
        .collect::<Vec<_>>();
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
    for (index, server) in servers.iter().enumerate().take(8) {
        if let Some(error) = ctx.runtime.web_search.cooling(server) {
            attempts.push(json!({"providerId":server["id"],"status":"cooldown","code":error.code,"retryAfterMs":error.retry_after_ms}));
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
                    || error.status == 400
                {
                    return Err(error);
                }
                attempts.push(json!({"providerId":server["id"],"status":"failed","code":error.code,"httpStatus":error.status,"retryAfterMs":error.retry_after_ms}));
                if error.code == "WEB_SEARCH_TRANSIENT"
                    && error.retry_after_ms.unwrap_or(2000) <= 2000
                {
                    retries.push(index);
                }
                ctx.runtime.web_search.failed(server, &error);
                ctx.runtime.mcp_errors.lock().await.insert(server["id"].as_str().unwrap_or("").to_owned(),
                    json!({"code":error.code,"message":error.message,"httpStatus":error.status,"retryAfterMs":error.retry_after_ms}));
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
                    || error.status == 400
                {
                    return Err(error);
                }
                ctx.runtime.web_search.failed(server, &error);
                ctx.runtime.mcp_errors.lock().await.insert(server["id"].as_str().unwrap_or("").to_owned(),
                    json!({"code":error.code,"message":error.message,"httpStatus":error.status,"retryAfterMs":error.retry_after_ms}));
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
                        json!({"content":[{"type":"text","text":sources.to_string()}],"structuredContent":sources})
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
            let calls = Arc::new(AtomicUsize::new(0));
            let arguments = Arc::new(Mutex::new(Vec::new()));
            let state = FixtureState {
                kind: kind.into(),
                failures,
                status,
                delay_ms,
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
