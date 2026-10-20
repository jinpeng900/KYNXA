//! Reusable capability metadata and derived experience; never a permission or execution ledger.
//! 可复用能力元信息与衍生经验；不替代权限检查或正式执行账本。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};

use super::gateway::GatewayToolContext;
use crate::gateway_contract::GatewayError;
use crate::storage::tool_memory::ToolMemory;

#[derive(Default)]
struct MemoryState {
    root: PathBuf,
    signature: String,
    revision: u64,
    store: Option<ToolMemory>,
    sources: HashMap<String, String>,
    disabled: HashSet<String>,
}

#[derive(Default)]
pub(crate) struct ToolMemoryRuntime {
    state: Arc<Mutex<MemoryState>>,
    updates: tokio::sync::Mutex<()>,
}

fn digest(value: &Value) -> String {
    super::local_files::sha256(&serde_json::to_vec(value).expect("JSON capability metadata"))
}

/// Cosmetic edits and one disabled tool do not discard experience for unrelated capabilities.
/// 名称、预热偏好及单项禁用不使其它能力的经验失效；连接与信任变化仍改变身份。
pub(crate) fn server_fingerprint(server: &Value) -> String {
    let mut identity = server.clone();
    if let Some(object) = identity.as_object_mut() {
        for field in ["name", "disabledTools", "backgroundTask", "prewarmPriority"] {
            object.remove(field);
        }
    }
    digest(&identity)
}

fn builtins() -> &'static (Vec<Value>, String) {
    static CATALOG: OnceLock<(Vec<Value>, String)> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let descriptors = super::gateway::builtin_catalog();
        let fingerprint = digest(&json!(descriptors));
        (descriptors, fingerprint)
    })
}

fn configuration(ctx: &GatewayToolContext) -> (HashMap<String, String>, HashSet<String>, String) {
    let mut sources = HashMap::from([("builtin".to_owned(), builtins().1.clone())]);
    let mut disabled = HashSet::new();
    for server in super::gateway_mcp::servers(ctx) {
        if server["trusted"] == false
            || server["transport"] == "stdio" && server["trusted"] != true
        {
            continue;
        }
        let Some(id) = server["id"].as_str().filter(|id| !id.is_empty()) else { continue; };
        sources.insert(format!("mcp:{id}"), server_fingerprint(&server));
        for tool in server["disabledTools"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            disabled.insert(format!("mcp.{id}.{tool}"));
        }
    }
    // Stable ordering avoids needless disk writes when map iteration order changes.
    // 稳定排序避免哈希容器迭代顺序变化引发无意义落盘。
    let ordered = sources.iter().collect::<std::collections::BTreeMap<_, _>>();
    let ordered_disabled = disabled.iter().collect::<std::collections::BTreeSet<_>>();
    let signature = digest(&json!({"sources":ordered,"disabled":ordered_disabled}));
    (sources, disabled, signature)
}

fn report_failure(code: &str) {
    // Derived memory failures must not turn a completed tool or ordinary chat into a failure.
    // 衍生记忆异常只记机器码，不把已完成工具或普通聊天变成失败，也不输出私有内容。
    eprintln!("[tool-memory] {code}");
}

/// Hydrate once at the existing configuration-read boundary, then reconcile only actual changes.
/// 在既有配置读取边界首次加载；后续未变化时不读盘、不联网、不调用模型。
pub(crate) async fn synchronize(ctx: &GatewayToolContext) {
    let (sources, disabled, signature) = configuration(ctx);
    let root = ctx.data_home.clone();
    let revision = ctx.config["revision"].as_u64().unwrap_or(0);
    if let Ok(mut state) = ctx.runtime.tool_memory.state.try_lock() {
        if state.root == root {
            if revision < state.revision { return; }
            if state.signature == signature {
                state.revision = revision;
                return;
            }
        }
    }
    let _update = ctx.runtime.tool_memory.updates.lock().await;
    let shared = ctx.runtime.tool_memory.state.clone();
    let work = tokio::task::spawn_blocking(move || {
        let mut state = shared.lock().map_err(|_| "TOOL_MEMORY_LOCK_FAILED".to_owned())?;
        if state.root == root {
            if revision < state.revision { return Ok(()); }
            if state.signature == signature {
                state.revision = revision;
                return Ok(());
            }
        }
        if state.root != root || state.signature.is_empty() {
            state.store = match ToolMemory::load(&root) {
                Ok(store) => Some(store),
                Err(error) => {
                    report_failure(error.code());
                    Some(ToolMemory::memory_only(&root))
                }
            };
            state.root = root;
        }
        state.signature = signature;
        state.revision = revision;
        state.sources = sources;
        state.disabled = disabled;
        let sources = state.sources.clone();
        let disabled = state.disabled.clone();
        if let Some(store) = state.store.as_mut() {
            let mut changed = store.reconcile(&sources, &disabled);
            changed |= store.replace_catalog("builtin", &builtins().1, builtins().0.clone());
            if changed {
                store.save().map_err(|error| error.code().to_owned())?;
            }
        }
        Ok::<_, String>(())
    }).await;
    match work {
        Ok(Ok(())) => {}
        Ok(Err(code)) => report_failure(&code),
        Err(_) => report_failure("TOOL_MEMORY_WORKER_FAILED"),
    }
}

/// Reading cached hints never waits for disk, a busy writer, or an MCP connection.
/// 读取能力提示不等待磁盘、繁忙写入者或 MCP 连接；调用方仍核对当前授权和配置身份。
pub(crate) fn cached_catalog(ctx: &GatewayToolContext) -> Vec<Value> {
    ctx.runtime.tool_memory.state.try_lock().ok()
        .filter(|state| state.root == ctx.data_home)
        .and_then(|state| state.store.as_ref().map(ToolMemory::catalog))
        .unwrap_or_default()
}

pub(crate) async fn remember_catalog(
    source: &str,
    fingerprint: &str,
    descriptors: Vec<Value>,
    ctx: &GatewayToolContext,
) {
    let _update = ctx.runtime.tool_memory.updates.lock().await;
    let shared = ctx.runtime.tool_memory.state.clone();
    let root = ctx.data_home.clone();
    let source = source.to_owned();
    let fingerprint = fingerprint.to_owned();
    let result = tokio::task::spawn_blocking(move || {
        let mut state = shared.lock().map_err(|_| "TOOL_MEMORY_LOCK_FAILED".to_owned())?;
        if state.root != root || state.sources.get(&source) != Some(&fingerprint) {
            return Ok(());
        }
        let disabled = state.disabled.clone();
        let descriptors = descriptors.into_iter().filter(|tool| {
            !tool["name"].as_str().is_some_and(|name| disabled.contains(name))
        }).collect();
        if let Some(store) = state.store.as_mut() {
            if store.replace_catalog(&source, &fingerprint, descriptors) {
                store.save().map_err(|error| error.code().to_owned())?;
            }
        }
        Ok::<_, String>(())
    }).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(code)) => report_failure(&code),
        Err(_) => report_failure("TOOL_MEMORY_WORKER_FAILED"),
    }
}

fn learning_outcome<'a>(name: &str, result: &'a Result<Value, GatewayError>) -> Option<(bool, Option<&'a str>)> {
    match result {
        Ok(value) => {
            let status = value["status"].as_str().unwrap_or("");
            if value["requiresApproval"] == true || value["executed"] == false
                || value["cancelled"] == true
                || ["running", "queued", "pending", "unknown", "cancelled", "not-dispatched"].contains(&status)
                || ["running", "queued"].contains(&value["job"]["status"].as_str().unwrap_or(""))
            {
                return None;
            }
            let failed = value["isError"] == true || ["failed", "error"].contains(&status);
            // Transport success does not make a challenge page usable evidence or an unvalidated file a deliverable.
            // 传输成功不能把验证页面变成可用证据，也不能把未核验文件当成已交付成果。
            if !failed && value["evidenceUsable"] == false {
                return Some((false,Some(value["code"].as_str().unwrap_or("WEB_EVIDENCE_UNUSABLE"))));
            }
            if !failed && name == "document.create" && value["validated"] != true {
                return None;
            }
            Some((!failed, value["code"].as_str()))
        }
        Err(error) => {
            if error.code.contains("CANCEL") || error.code.starts_with("RESOURCE_")
                || error.code.contains("APPROVAL") || error.code.contains("PERMISSION")
                || ["ABORT_ERR", "INVALID_TOOL_ARGUMENTS", "TOOL_CONFIGURATION_CHANGED",
                    "MCP_TOOL_DISABLED", "MCP_SERVER_NOT_FOUND", "MCP_UNTRUSTED"].contains(&error.code.as_str())
            {
                return None;
            }
            Some((false, Some(&error.code)))
        }
    }
}

pub(crate) async fn remember_outcome(
    name: &str,
    result: &Result<Value, GatewayError>,
    elapsed: Duration,
    ctx: &GatewayToolContext,
) {
    // Discovery calls are infrastructure, not evidence that a user action succeeded.
    // 工具发现属于基础设施；等待、审批及未知结果也不计作动作成功。
    if name.starts_with("tool.") { return; }
    let Some((succeeded, error_code)) = learning_outcome(name,result) else { return; };
    let name = name.to_owned();
    let error_code = error_code.map(str::to_owned);
    let root = ctx.data_home.clone();
    let shared = ctx.runtime.tool_memory.state.clone();
    let source = name.strip_prefix("mcp.").and_then(|suffix| suffix.split_once('.'))
        .map(|(id, _)| format!("mcp:{id}")).unwrap_or_else(|| "builtin".to_owned());
    let expected = if source == "builtin" {
        builtins().1.clone()
    } else {
        let Ok(server) = super::gateway_mcp::authorized_server(&name, ctx) else { return; };
        server_fingerprint(&server)
    };
    let _update = ctx.runtime.tool_memory.updates.lock().await;
    let result = tokio::task::spawn_blocking(move || {
        let mut state = shared.lock().map_err(|_| "TOOL_MEMORY_LOCK_FAILED".to_owned())?;
        if state.root != root || state.sources.get(&source) != Some(&expected)
            || state.disabled.contains(&name)
        {
            return Ok(());
        }
        if let Some(store) = state.store.as_mut() {
            if store.record(&name, succeeded, elapsed.as_millis().min(u64::MAX as u128) as u64, error_code.as_deref()) {
                store.save().map_err(|error| error.code().to_owned())?;
            }
        }
        Ok::<_, String>(())
    }).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(code)) => report_failure(&code),
        Err(_) => report_failure("TOOL_MEMORY_WORKER_FAILED"),
    }
}

/// Keep relevance authoritative; history only breaks ties among existing matching candidates.
/// 相关性始终优先；经验只在已有同分候选间排序，不新增工具或扩大候选数量。
pub(crate) fn rank_tool_hints(ctx: &GatewayToolContext, descriptors: &[Value], hints: &mut [Value]) {
    let Ok(state) = ctx.runtime.tool_memory.state.try_lock() else { return; };
    if state.root != ctx.data_home { return; }
    let Some(store) = state.store.as_ref() else { return; };
    let experience = |hit: &Value| {
        hit.as_u64().or_else(|| hit["index"].as_u64())
            .and_then(|index| descriptors.get(index as usize))
            .and_then(|tool| tool["name"].as_str())
            .map(|name| store.score(name)).unwrap_or(0.0)
    };
    hints.sort_by(|left, right| {
        let left_relevance = left["score"].as_u64();
        let right_relevance = right["score"].as_u64();
        if left_relevance.is_none() || right_relevance.is_none() {
            return std::cmp::Ordering::Equal;
        }
        right_relevance.cmp(&left_relevance)
            .then_with(|| experience(right).total_cmp(&experience(left)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_memory_learns_usable_evidence_and_validated_documents_only() {
        assert_eq!(learning_outcome("web.fetch",&Ok(json!({"evidenceUsable":false,"needsHumanVerification":true}))),Some((false,Some("WEB_EVIDENCE_UNUSABLE"))));
        assert_eq!(learning_outcome("web.fetch",&Ok(json!({"evidenceUsable":false,"code":"WEB_RENDER_REQUIRED"}))),Some((false,Some("WEB_RENDER_REQUIRED"))));
        assert_eq!(learning_outcome("web.fetch",&Ok(json!({"evidenceUsable":true}))),Some((true,None)));
        for value in [json!({}),json!({"validated":false}),json!({"validated":true,"status":"pending"}),json!({"validated":true,"status":"unknown"}),json!({"validated":true,"executed":false})] {
            assert_eq!(learning_outcome("document.create",&Ok(value)),None);
        }
        assert_eq!(learning_outcome("document.create",&Ok(json!({"validated":true,"status":"completed"}))),Some((true,None)));
        assert_eq!(learning_outcome("document.create",&Ok(json!({"validated":false,"isError":true,"code":"DOCUMENT_VALIDATION_FAILED"}))),Some((false,Some("DOCUMENT_VALIDATION_FAILED"))));
    }

    #[tokio::test]
    async fn tool_memory_learns_real_reads_restarts_and_keeps_chat_call_free() {
        let (mut ctx, _keep) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        synchronize(&ctx).await;
        std::fs::write(ctx.workdir.join("note.txt"), "fixture content").unwrap();
        let value = super::super::gateway::execute("filesystem.read", json!({"path":"note.txt"}), &ctx).await.unwrap();
        assert_ne!(value["isError"], true);
        let descriptors = vec![json!({"name":"filesystem.stat"}), json!({"name":"filesystem.read"})];
        let mut tied = vec![json!({"index":0,"score":100}), json!({"index":1,"score":100})];
        rank_tool_hints(&ctx, &descriptors, &mut tied);
        assert_eq!(tied[0]["index"], 1, "A real successful read supplies secondary experience.");
        let mut exact = vec![json!({"index":0,"score":100000}), json!({"index":1,"score":100})];
        rank_tool_hints(&ctx, &descriptors, &mut exact);
        assert_eq!(exact[0]["index"], 0, "Experience cannot override relevance.");
        let path = ctx.data_home.join("Agent/tool-memory.json");
        let before = std::fs::read(&path).unwrap();
        remember_outcome("filesystem.read", &Ok(json!({"requiresApproval":true})), Duration::ZERO, &ctx).await;
        remember_outcome("filesystem.read", &Ok(json!({"status":"unknown"})), Duration::ZERO, &ctx).await;
        assert_eq!(std::fs::read(&path).unwrap(), before);
        ctx.runtime = Arc::new(super::super::gateway::ToolRuntime::default());
        synchronize(&ctx).await;
        let mut restored = vec![json!({"index":0,"score":100}), json!({"index":1,"score":100})];
        rank_tool_hints(&ctx, &descriptors, &mut restored);
        assert_eq!(restored, tied);
        assert_eq!(std::fs::read(&path).unwrap(), before, "Restart reuses unchanged metadata and experience.");
        for message in ["你好", "hello"] {
            let hints = super::super::discovery::calculate("tools.searchTools", &json!({
                "query":message,"descriptors":super::super::gateway::builtin_catalog(),"includeScores":true
            })).unwrap();
            assert!(hints.as_array().unwrap().is_empty());
            synchronize(&ctx).await;
        }
        assert!(ctx.runtime.mcp.try_lock().unwrap().is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let server = json!({"id":"fixture","transport":"streamable-http","url":"http://127.0.0.1:9/mcp","enabled":true,"trusted":true});
        ctx.config["mcpServers"] = json!([server.clone()]);
        synchronize(&ctx).await;
        remember_catalog("mcp:fixture", &server_fingerprint(&server), vec![json!({
            "name":"mcp.fixture.echo","toolName":"echo","serverId":"fixture","inputSchema":{"type":"object"}
        })], &ctx).await;
        assert!(cached_catalog(&ctx).iter().any(|tool| tool["name"] == "mcp.fixture.echo"));
        ctx.config["mcpServers"][0]["disabledTools"] = json!(["echo"]);
        synchronize(&ctx).await;
        let persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(persisted["entries"]["mcp.fixture.echo"].is_null());
        ctx.runtime = Arc::new(super::super::gateway::ToolRuntime::default());
        synchronize(&ctx).await;
        assert!(super::super::gateway_mcp::reusable_catalog(&ctx).is_empty());
        remember_outcome("web.fetch",&Ok(json!({"evidenceUsable":false,"content":"CHALLENGE_BODY_MUST_NOT_PERSIST"})),Duration::from_millis(25),&ctx).await;
        ctx.runtime = Arc::new(super::super::gateway::ToolRuntime::default());
        synchronize(&ctx).await;
        let persisted=std::fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("CHALLENGE_BODY_MUST_NOT_PERSIST"));
        let persisted:Value=serde_json::from_str(&persisted).unwrap();
        assert_eq!(persisted["entries"]["web.fetch"]["experience"]["successes"],0);
        assert_eq!(persisted["entries"]["web.fetch"]["experience"]["failures"],1);
        assert_eq!(persisted["entries"]["web.fetch"]["experience"]["lastErrorCode"],"WEB_EVIDENCE_UNUSABLE");
        std::fs::remove_dir_all(root).unwrap();
    }
}
