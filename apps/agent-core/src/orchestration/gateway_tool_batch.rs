//! Bounded scheduling for independent read-only calls returned in one model step.
//! 同一模型步骤内独立只读调用的有界调度。
//!
//! Only consecutive independent reads form a batch; any other call is an ordered barrier, so writes,
//! browser/terminal sessions, unknown MCP tools and calls on one MCP connection keep their serial order.
//! 只有相邻的独立只读调用组成批次；其他调用都是有序屏障，写操作、浏览器／终端会话、未知 MCP
//! 以及同一 MCP 连接上的调用仍保持原有串行顺序。

use super::{gateway_runtime::RequestRun, gateway_task_journal::TaskJournal};
use crate::{
    gateway_contract::GatewayError,
    resources::gateway::ResourceLease,
    storage::gateway as data,
    tools::{gateway as tools, gateway::GatewayToolContext},
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    ops::Range,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// Upper bound on calls of one batch running at the same time; resource admission may lower it further.
/// 同一批次同时运行的调用上限；资源服务准入可进一步降低实际并发。
pub(crate) const MAX_PARALLEL_TOOL_CALLS: usize = 16;

/// Gateway buffers reserved for each extra concurrent read; the first call keeps the former serial cost.
/// 每个额外并发读取预约的网关缓冲；第一个调用沿用原串行路径，不额外预约。
const PARALLEL_READ_MEMORY_BYTES: u64 = 32 * 1024 * 1024;

/// A call executed ahead of the ordered result loop, still settled there in its original position.
/// 在有序结果循环之前执行的调用；回执仍在原调用位置写回模型历史与正式归档。
pub(crate) struct Prefetched {
    pub started: Instant,
    pub dispatched: bool,
    pub execution_ms: u64,
    pub outcome: PrefetchOutcome,
}

pub(crate) enum PrefetchOutcome {
    /// Stopped while waiting for resources; nothing was dispatched.
    /// 等待资源时被停止，未派发任何操作。
    NotDispatched(Value),
    /// The tool asked for approval; the ordered loop runs the normal approval path.
    /// 工具要求审批；由有序循环走原审批路径，不在批次内等待用户。
    AwaitingApproval(Result<Value, GatewayError>),
    /// Receipt archived and its journal operation finished as soon as the call completed.
    /// 调用完成时已即时归档回执并结算操作日志。
    Archived { receipt: Value, reference: Value },
    /// Archiving or journal settlement failed; the ordered loop reports it like the serial path.
    /// 归档或日志结算失败；有序循环按串行路径同样上报。
    Failed(GatewayError),
}

/// Resource identity of an independent read, or `None` when the call must stay an ordered barrier.
/// 独立读取的资源身份；返回 `None` 表示该调用必须作为有序屏障。
pub(crate) fn parallel_resource(call: &Value) -> Option<String> {
    if call["unavailable"] == true {
        return None;
    }
    let name = call["name"].as_str()?;
    let arguments = &call["arguments"];
    if crate::tools::calculate("tools.canParallel", &json!({"name":name})) != Ok(json!(true)) {
        return None;
    }
    if let Some(server) = name.strip_prefix("mcp.").and_then(|rest| rest.split('.').next()) {
        // Explicitly read-only targets may use independent MCP sessions; unknown targets retain the service barrier.
        // 明确只读的独立目标可使用独立MCP会话；目标未知时保留服务级屏障。
        let target = ["url", "path", "uri"].iter().find_map(|field| arguments[*field].as_str()
            .or_else(|| arguments["arguments"][*field].as_str()));
        return Some(format!("mcp:{server}:{}", target.unwrap_or("shared")));
    }
    let id = call["id"].as_str().unwrap_or("");
    Some(match name {
        // Independent pages may overlap; provider pacing remains owned by the fetch/MCP service.
        // 独立网页允许并行；站点节流由抓取/MCP服务管理，同一页面仍保持顺序。
        "web.fetch" => format!(
            "web:{}",
            arguments["url"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call:{id}"))
        ),
        // Pages of one document share read progress and stay in their requested order.
        // 同一文档的分页共享阅读进度，按请求顺序读取。
        "document.read" => format!(
            "document:{}:{}",
            arguments["mountId"].as_str().unwrap_or(""),
            arguments["path"].as_str().unwrap_or(id)
        ),
        _ => format!("call:{name}:{id}"),
    })
}

fn catalog_parallel_resource(call: &Value, catalog: &[Value]) -> Option<String> {
    if let Some(resource) = parallel_resource(call) { return Some(resource); }
    let name = call["name"].as_str()?;
    if call["unavailable"] == true || !name.starts_with("mcp.") { return None; }
    let descriptor = catalog.iter().find(|tool| tool["name"] == name)?;
    if descriptor["annotations"]["readOnlyHint"] != true { return None; }
    let arguments = &call["arguments"];
    let arguments = arguments.get("arguments").filter(|value| value.is_object()).unwrap_or(arguments);
    if ["tabId", "pageId", "sessionId"].iter().any(|field| !arguments[*field].is_null()) { return None; }
    let target = ["url", "path", "uri", "filePath"].iter().find_map(|field| arguments[*field].as_str())?;
    let server = name.strip_prefix("mcp.")?.split('.').next()?;
    Some(format!("mcp:{server}:{target}"))
}

/// Split the model's calls into batches of at least two consecutive independent reads.
/// 将模型调用划分为至少两个相邻独立读取组成的批次；其余调用保持串行。
#[cfg(test)]
pub(crate) fn plan(calls: &[Value]) -> Vec<Range<usize>> {
    plan_with_catalog(calls, &[])
}

/// Take concurrency hints from the current catalog; model arguments grant no permissions.
/// 并发提示取自当前目录，模型参数不授予权限。
pub(crate) fn plan_with_catalog(calls: &[Value], catalog: &[Value]) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut resources = HashSet::new();
    let close = |start: usize, end: usize, batches: &mut Vec<Range<usize>>| {
        if end.saturating_sub(start) >= 2 {
            batches.push(start..end);
        }
    };
    for (index, call) in calls.iter().enumerate() {
        match catalog_parallel_resource(call, catalog) {
            Some(resource)
                if index - start < MAX_PARALLEL_TOOL_CALLS && !resources.contains(&resource) =>
            {
                resources.insert(resource);
            }
            Some(resource) => {
                // A full batch or a shared resource starts the next batch after this one settles.
                // 批次已满或资源相同，则在当前批次全部结算后开始下一批。
                close(start, index, &mut batches);
                start = index;
                resources.clear();
                resources.insert(resource);
            }
            None => {
                close(start, index, &mut batches);
                start = index + 1;
                resources.clear();
            }
        }
    }
    close(start, calls.len(), &mut batches);
    batches
}

#[derive(Clone, Copy)]
enum CallPhase {
    Waiting,
    Executing,
}

#[derive(Default)]
struct BatchActivity {
    waiting: usize,
    executing: usize,
    suspended_since: Option<Instant>,
}

impl BatchActivity {
    /// Count fully suspended interval unions, excluding overlapping waits and other active calls.
    /// 仅计整个批次完全挂起的时间并集，不叠加等待或抵扣其他执行。
    fn transition(&mut self, previous: Option<CallPhase>, next: Option<CallPhase>, now: Instant) -> Duration {
        let suspended = self.suspended_since.take()
            .map_or(Duration::ZERO, |started| now.saturating_duration_since(started));
        match previous {
            Some(CallPhase::Waiting) => self.waiting -= 1,
            Some(CallPhase::Executing) => self.executing -= 1,
            None => {},
        }
        match next {
            Some(CallPhase::Waiting) => self.waiting += 1,
            Some(CallPhase::Executing) => self.executing += 1,
            None => {},
        }
        if self.waiting > 0 && self.executing == 0 {
            self.suspended_since = Some(now);
        }
        suspended
    }
}

struct BatchResourceWait<'a> {
    resource_wait_ms: &'a AtomicU64,
    activity: Mutex<BatchActivity>,
}

impl BatchResourceWait<'_> {
    fn transition(&self, previous: Option<CallPhase>, next: Option<CallPhase>) {
        let suspended = self.activity.lock().unwrap().transition(previous, next, Instant::now());
        self.resource_wait_ms.fetch_add(
            suspended.as_millis().min(u64::MAX as u128) as u64,
            Ordering::AcqRel,
        );
    }

    fn begin(&self, phase: CallPhase) -> BatchCallActivity<'_, '_> {
        self.transition(None, Some(phase));
        BatchCallActivity { batch: self, phase }
    }
}

struct BatchCallActivity<'batch, 'run> {
    batch: &'batch BatchResourceWait<'run>,
    phase: CallPhase,
}

impl BatchCallActivity<'_, '_> {
    fn admitted(&mut self) {
        self.batch.transition(Some(self.phase), Some(CallPhase::Executing));
        self.phase = CallPhase::Executing;
    }
}

impl Drop for BatchCallActivity<'_, '_> {
    fn drop(&mut self) {
        // Cancellation/drop closes waiting intervals without dispatch or replay.
        // 取消或丢弃时结算等待区间，不派发或重放排队调用。
        self.batch.transition(Some(self.phase), None);
    }
}

/// Run bounded concurrent batches and return results in original order.
/// 有界并发执行批次，按原调用顺序返回结果。
///
/// The first call needs no extra admission. Each further call waits in the resource queue; a
/// cancelled wait yields `rejected` without executing the call.
/// 第一个调用无需额外准入；其余调用在资源队列中等待，等待期间被停止时返回 `rejected`，不执行调用。
pub(crate) async fn run_batch<Item, Lease, Output, Admit, AdmitFuture, Execute, ExecuteFuture>(
    resource_wait_ms: &AtomicU64,
    items: Vec<Item>,
    admit: Admit,
    execute: Execute,
    rejected: impl Fn(Item, GatewayError) -> Output,
) -> Vec<Output>
where
    Admit: Fn(usize) -> AdmitFuture,
    AdmitFuture: Future<Output = Result<Option<Lease>, GatewayError>>,
    Execute: Fn(Item) -> ExecuteFuture,
    ExecuteFuture: Future<Output = Output>,
{
    let activity = BatchResourceWait { resource_wait_ms, activity: Mutex::new(BatchActivity::default()) };
    let (admit, execute, rejected, activity) = (&admit, &execute, &rejected, &activity);
    stream::iter(items.into_iter().enumerate())
        .map(|(index, item)| async move {
            let mut activity = activity.begin(if index == 0 { CallPhase::Executing } else { CallPhase::Waiting });
            let lease = if index == 0 { Ok(None) } else { admit(index).await };
            match lease {
                Ok(lease) => {
                    if index != 0 { activity.admitted(); }
                    let output = execute(item).await;
                    drop(lease);
                    output
                }
                Err(error) => rejected(item, error),
            }
        })
        .buffered(MAX_PARALLEL_TOOL_CALLS)
        .collect()
        .await
}

/// Reserve each extra read's capacity; queue shortages instead of reporting execution failure.
/// 为额外读取预约资源，容量不足时排队，不报执行失败。
///
/// Only an explicit stop ends the wait. Without a resource service, or when the service itself is
/// unavailable, the call proceeds like the former serial path, which had no extra reservation.
/// 只有用户明确停止才结束等待；没有资源服务或服务本身不可用时，按原串行路径执行，原路径也无额外预约。
pub(crate) async fn admit_parallel_read(
    ctx: &GatewayToolContext,
    task_id: &str,
    cancellation: watch::Receiver<bool>,
) -> Result<Option<ResourceLease>, GatewayError> {
    let Some(service) = crate::resources::gateway::global() else {
        return Ok(None);
    };
    let workspace = ctx.project_id.as_deref().unwrap_or(&ctx.conversation_id);
    let request = json!({"taskId":task_id,"workspaceId":if workspace.is_empty(){"tools"}else{workspace},
        "kind":"foreground","workload":"compute","cpuThreads":1,"memoryBytes":PARALLEL_READ_MEMORY_BYTES,
        "gpuMemoryBytes":0,"waitMs":30000,"ttlMs":30000});
    match service.acquire_waiting(request, cancellation.clone()).await {
        Ok(lease) => Ok(Some(lease)),
        Err(error) if error.code == "RESOURCE_CANCELLED" || *cancellation.borrow() => Err(error),
        Err(_) => Ok(None),
    }
}

/// Scope rollover rounds to their formal reply; existing first-reply identities remain compatible.
/// 续段轮次使用正式回复身份隔离，首条回复保留既有操作身份以兼容旧检查点。
pub(crate) fn operation_id(context: &GatewayToolContext, call: &Value, round: u64) -> String {
    let prefix = context.config["operationNamespace"].as_str().unwrap_or("");
    if prefix.is_empty() { format!("round-{round}:{}", call["id"].as_str().unwrap_or("")) }
    else { format!("reply-{prefix}:round-{round}:{}", call["id"].as_str().unwrap_or("")) }
}

/// Record and dispatch one real tool operation before its execution starts.
/// 在真实执行开始前记录并派发一次工具操作；未加载、复用和未派发的回执不进入此路径。
pub(crate) fn record_dispatch(
    journal: &TaskJournal,
    run: &RequestRun,
    context: &GatewayToolContext,
    call: &Value,
    round: u64,
    operation_id: &str,
    is_observation: bool,
) -> Result<(), GatewayError> {
    use crate::storage::task_repository::{OperationEffect, OperationIntent, OperationState};
    let name = call["name"].as_str().unwrap_or("");
    // Only real dispatch owns an operation transition; unavailable and reused receipts remain observations.
    // 只有真实派发才推进操作状态；未加载与复用回执保留为观察，不伪造新执行。
    let mut input_reference = json!({"kind":"conversation-tool-call","conversationId":run.conversation_id,"requestId":run.request_id,"toolCallId":call["id"],"round":round});
    if context.config["operationNamespace"].is_string() {
        input_reference["assistantMessageId"] = context.config["activeAssistantMessageId"].clone();
    }
    input_reference["mountBinding"] = if tools::workspace_path_tool(name, &call["arguments"]) {
        tools::operation_mount_binding(context, &call["arguments"])?
    } else {
        Value::Null
    };
    // Preserve the fingerprint of an undispatched legacy operation during an upgrade.
    // 升级续接时保留旧版未派发操作的指纹，不因新增挂载元数据重建操作。
    let input_reference = journal
        .repository()
        .operation(journal.task_id(), operation_id)?
        .filter(|operation| {
            operation.state == OperationState::Pending
                && operation
                    .intent
                    .input_reference
                    .as_ref()
                    .is_some_and(|reference| reference.get("mountBinding").is_none())
        })
        .and_then(|operation| operation.intent.input_reference)
        .unwrap_or(input_reference);
    journal.record_operation(&OperationIntent {
        operation_id: operation_id.into(),
        tool_call_id: call["id"].as_str().unwrap_or("").into(),
        tool_name: name.into(),
        input_sha256: crate::retrieval::gateway_index::hash(&call["arguments"].to_string()),
        input_reference: Some(input_reference),
        effect: if is_observation {
            OperationEffect::Observation
        } else if name.starts_with("filesystem.") || name == "document.create" {
            OperationEffect::FileMutation
        } else {
            OperationEffect::Irreversible
        },
    })?;
    journal.dispatch_operation(operation_id)?;
    Ok(())
}

/// Settle a dispatched operation with its archived receipt and measured tool time.
/// 用已归档回执与实测工具耗时结算已派发操作。
pub(crate) fn finish_dispatch(
    journal: &TaskJournal,
    operation_id: &str,
    receipt: &Value,
    reference: &Value,
    name: &str,
    tool_execution_ms: u64,
) -> Result<(), GatewayError> {
    use crate::storage::task_repository::{OperationState, TaskUsage};
    let status = if receipt["status"] == "unknown" {
        OperationState::Unknown
    } else if receipt["isError"] == true {
        OperationState::Failed
    } else {
        OperationState::Succeeded
    };
    journal.finish_operation(operation_id, status, Some(reference.clone()), None)?;
    journal.repository().record_usage(
        journal.token(),
        &format!("tool-duration:{operation_id}"),
        &TaskUsage {
            tool_duration_ms: if name == "task.wait" { 0 } else { tool_execution_ms },
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Run admitted calls concurrently; archive/settle on completion and return in original order.
/// 已准入调用并发执行，完成即归档结算，按原顺序返回。
pub(crate) async fn execute(
    data_root: &Path,
    run: &RequestRun,
    ctx: &GatewayToolContext,
    calls: Vec<(usize, Value, GatewayToolContext)>,
    round: u64,
) -> HashMap<usize, Prefetched> {
    let not_dispatched = |(index, _, _): (usize, Value, GatewayToolContext), error: GatewayError| {
        (index, Prefetched {
            started: Instant::now(),
            dispatched: false,
            execution_ms: 0,
            outcome: PrefetchOutcome::NotDispatched(json!({"status":"error","executed":false,"isError":true,
                "code":"ABORT_ERR","content":{"notice":"Not dispatched: the task was stopped while this call waited for resources.","resourceCode":error.code}})),
        })
    };
    run_batch(
        &run.resource_wait_ms,
        calls,
        |_| admit_parallel_read(ctx, &run.request_id, ctx.cancellation.clone()),
        |(index, call, context)| async move {
            let started = Instant::now();
            let name = call["name"].as_str().unwrap_or("").to_owned();
            let operation_id = operation_id(&context, &call, round);
            let mut prefetched = Prefetched { started, dispatched: false, execution_ms: 0,
                outcome: PrefetchOutcome::Failed(GatewayError::new("TOOL_BATCH_FAILED", "工具批次未完成。", 500)) };
            if let Some(journal) = &run.journal {
                if let Err(error) = record_dispatch(journal, run, &context, &call, round, &operation_id,
                    crate::tools::policy::is_read_only_call(&name, &call["arguments"])) {
                    prefetched.outcome = PrefetchOutcome::Failed(error);
                    return (index, prefetched);
                }
                prefetched.dispatched = true;
            }
            let mut call_context = context;
            call_context.config["activeToolCallId"] = call["id"].clone();
            call_context.config["activeOperationId"] = json!(operation_id);
            let dispatched_at = Instant::now();
            let result = tools::execute(&name, call["arguments"].clone(), &call_context).await;
            prefetched.execution_ms = dispatched_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
            if matches!(&result, Ok(value) if value["requiresApproval"] == true) {
                prefetched.outcome = PrefetchOutcome::AwaitingApproval(result);
                return (index, prefetched);
            }
            let mut receipt = super::gateway_loop::tool_execution_receipt(result);
            let settled = async {
                let canonical = receipt["content"].clone();
                let (root, conversation, request) = (data_root.to_path_buf(), run.conversation_id.clone(), run.request_id.clone());
                let stored_call = call.clone();
                let reference = tokio::task::spawn_blocking(move || {
                    let environment = canonical.get("executionEnvironment").cloned();
                    data::save_tool_result(&root, &conversation, &request, &stored_call, &canonical, environment.as_ref())
                })
                .await
                .map_err(|_| GatewayError::new("DATA_WORKER_FAILED", "工具回执保存失败。", 500))??;
                receipt["resultRef"] = reference.clone();
                if prefetched.dispatched && let Some(journal) = &run.journal {
                    finish_dispatch(journal, &operation_id, &receipt, &reference, &name, prefetched.execution_ms)?;
                }
                Ok::<_, GatewayError>(reference)
            }
            .await;
            prefetched.outcome = match settled {
                Ok(reference) => PrefetchOutcome::Archived { receipt, reference },
                Err(error) => PrefetchOutcome::Failed(error),
            };
            (index, prefetched)
        },
        not_dispatched,
    )
    .await
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::{Barrier, Notify};

    fn call(id: &str, name: &str, arguments: Value) -> Value {
        json!({"id":id,"name":name,"arguments":arguments})
    }

    #[test]
    fn independent_reads_batch_while_writes_sessions_and_unknown_tools_stay_ordered() {
        let calls = vec![
            call("a", "filesystem.read", json!({"path":"a.txt"})),
            call("b", "filesystem.read", json!({"path":"b.txt"})),
            call("c", "web.fetch", json!({"url":"https://example.com/page"})),
            call("w", "filesystem.write", json!({"path":"a.txt","content":"x","expectedHash":null})),
            call("d", "knowledge.search", json!({"query":"q"})),
            call("e", "document.read", json!({"path":"one.docx"})),
            call("f", "document.read", json!({"path":"two.pdf"})),
            call("s1", "browser.snapshot", json!({"tabId":"t1"})),
            call("s2", "browser.click", json!({"tabId":"t1","ref":"r"})),
            call("t1", "terminal.host.input", json!({"terminalId":"x","text":"ls"})),
            call("u1", "mcp.unknown-server.lookup", json!({})),
            call("u2", "mcp.unknown-server.lookup", json!({})),
            call("g", "filesystem.stat", json!({"path":"a.txt"})),
        ];
        assert_eq!(plan(&calls), vec![0..3, 4..7]);
    }

    #[test]
    fn shared_resources_and_the_concurrency_limit_split_batches() {
        let calls = vec![
            call("a", "web.fetch", json!({"url":"https://Example.com/a"})),
            call("b", "web.fetch", json!({"url":"https://example.com/b"})),
            call("c", "web.fetch", json!({"url":"https://other.test/c"})),
            call("d", "document.read", json!({"path":"same.pdf","offset":0})),
            call("e", "document.read", json!({"path":"same.pdf","offset":4000})),
            call("f", "filesystem.read", json!({"path":"x"})),
        ];
        // Independent pages on one host overlap; the same document remains an ordered barrier.
        // 同主机的独立网页可以并行；同一文档仍作为有序屏障。
        assert_eq!(plan(&calls), vec![0..4, 4..6]);
        let many = (0..MAX_PARALLEL_TOOL_CALLS + 2)
            .map(|index| call(&format!("r{index}"), "filesystem.read", json!({"path":index})))
            .collect::<Vec<_>>();
        assert_eq!(
            plan(&many),
            vec![0..MAX_PARALLEL_TOOL_CALLS, MAX_PARALLEL_TOOL_CALLS..MAX_PARALLEL_TOOL_CALLS + 2]
        );
        let unloaded = vec![
            json!({"id":"x","name":"filesystem.read","arguments":{},"unavailable":true}),
            call("y", "filesystem.read", json!({"path":"y"})),
        ];
        assert!(plan(&unloaded).is_empty());
    }

    #[test]
    fn current_catalog_hints_allow_independent_mcp_reads_without_granting_page_or_mutation_parallelism() {
        let catalog = vec![json!({"name":"mcp.fixture.scrape","annotations":{"readOnlyHint":true}})];
        let a = call("a", "mcp.fixture.scrape", json!({"arguments":{"url":"https://example.com/a"}}));
        let b = call("b", "mcp.fixture.scrape", json!({"arguments":{"url":"https://example.com/b"}}));
        assert_eq!(plan_with_catalog(&[a.clone(),b.clone()], &catalog), vec![0..2]);
        assert!(plan(&[a.clone(),b.clone()]).is_empty());
        let same = call("same", "mcp.fixture.scrape", json!({"arguments":{"url":"https://example.com/a"}}));
        assert!(plan_with_catalog(&[a,same], &catalog).is_empty());
        let page = call("page", "mcp.fixture.scrape", json!({"arguments":{"url":"https://example.com/c","tabId":"one"}}));
        assert!(plan_with_catalog(&[b,page], &catalog).is_empty());
        let write = call("write", "mcp.fixture.write", json!({"arguments":{"path":"one.txt","readOnlyHint":true}}));
        assert!(plan_with_catalog(&[write.clone(),write], &catalog).is_empty());
    }

    #[test]
    fn mcp_search_overlaps_only_across_distinct_connections() {
        let calls = vec![
            call("a", "mcp.exa.web_search_exa", json!({"query":"one"})),
            call("b", "mcp.exa.web_search_exa", json!({"query":"two"})),
            call("c", "mcp.brave.brave_web_search", json!({"query":"three"})),
        ];
        assert_eq!(plan(&calls), vec![1..3]);
    }

    #[test]
    fn overlapping_admission_waits_exempt_only_the_fully_suspended_union() {
        let origin = Instant::now();
        let at = |milliseconds| origin + Duration::from_millis(milliseconds);
        let mut activity = BatchActivity::default();
        let mut suspended = Duration::ZERO;
        suspended += activity.transition(None, Some(CallPhase::Executing), at(0));
        for milliseconds in [10, 20, 30] {
            suspended += activity.transition(None, Some(CallPhase::Waiting), at(milliseconds));
        }
        suspended += activity.transition(Some(CallPhase::Executing), None, at(100));
        suspended += activity.transition(Some(CallPhase::Waiting), Some(CallPhase::Executing), at(1100));
        suspended += activity.transition(Some(CallPhase::Executing), None, at(1200));
        suspended += activity.transition(Some(CallPhase::Waiting), Some(CallPhase::Executing), at(2200));
        suspended += activity.transition(Some(CallPhase::Executing), None, at(2300));
        // A cancellation closes the final wait without dispatching it.
        // 最后一个等待被取消时结算其区间，不把它记为实际执行。
        suspended += activity.transition(Some(CallPhase::Waiting), None, at(2500));
        assert_eq!(suspended, Duration::from_millis(2200));
        assert_eq!(Duration::from_millis(2500) - suspended, Duration::from_millis(300));
        assert_eq!((activity.waiting, activity.executing), (0, 0));
        assert!(activity.suspended_since.is_none());
    }

    #[tokio::test]
    async fn resource_queue_longer_than_task_budget_resumes_without_charging_queue_time() {
        let run = RequestRun::test_run(4);
        let started = Instant::now();
        let active_budget = Duration::from_millis(150);
        let capacity = Arc::new(Notify::new());
        let (queued, mut queued_event) = tokio::sync::mpsc::unbounded_channel();
        let executions = Arc::new(AtomicUsize::new(0));
        let batch = {
            let run = run.clone();
            let capacity = capacity.clone();
            let executions = executions.clone();
            tokio::spawn(async move {
                run_batch(
                    &run.resource_wait_ms,
                    vec!["first", "queued-one", "queued-two"],
                    move |_| {
                        let capacity = capacity.clone();
                        let queued = queued.clone();
                        async move {
                            queued.send(()).unwrap();
                            capacity.notified().await;
                            Ok::<Option<()>, GatewayError>(Some(()))
                        }
                    },
                    move |id| {
                        let executions = executions.clone();
                        async move {
                            executions.fetch_add(1, Ordering::SeqCst);
                            format!("saved:{id}")
                        }
                    },
                    |id, _| format!("not-dispatched:{id}"),
                ).await
            })
        };
        queued_event.recv().await.unwrap();
        queued_event.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(started.elapsed() > active_budget);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        capacity.notify_waiters();
        let outputs = tokio::time::timeout(Duration::from_secs(5), batch).await.unwrap().unwrap();
        assert_eq!(outputs, vec!["saved:first", "saved:queued-one", "saved:queued-two"]);
        assert_eq!(executions.load(Ordering::SeqCst), 3, "receipts settle once; completed calls are not replayed");
        assert!(run.active_elapsed(started) < active_budget, "resource-only queue time cannot exhaust the task budget");
        assert!(run.resource_wait_ms.load(Ordering::Acquire) <= started.elapsed().as_millis() as u64,
            "overlapping queued calls must not multiply the exemption");
    }

    #[tokio::test]
    async fn cancelling_a_queued_batch_settles_waiting_time_without_dispatching_the_call() {
        let run = RequestRun::test_run(4);
        let executions = Arc::new(AtomicUsize::new(0));
        let (queued, queued_event) = tokio::sync::oneshot::channel();
        let queued = Mutex::new(Some(queued));
        let mut cancellation = run.cancellation.subscribe();
        let batch = run_batch(
            &run.resource_wait_ms,
            vec!["saved-first", "waiting"],
            |_| {
                let mut cancellation = cancellation.clone();
                queued.lock().unwrap().take().unwrap().send(()).unwrap();
                async move {
                    let _ = cancellation.wait_for(|stopped| *stopped).await;
                    Err::<Option<()>, GatewayError>(GatewayError::new("RESOURCE_CANCELLED", "资源等待已取消。", 499))
                }
            },
            |id| {
                let executions = executions.clone();
                async move {
                    executions.fetch_add(1, Ordering::SeqCst);
                    id
                }
            },
            |_, error| {
                assert_eq!(error.code, "RESOURCE_CANCELLED");
                "not-dispatched"
            },
        );
        let stop = async {
            queued_event.await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            run.cancel();
        };
        let (outputs, ()) = tokio::join!(batch, stop);
        assert_eq!(outputs, vec!["saved-first", "not-dispatched"]);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(run.resource_wait_ms.load(Ordering::Acquire) >= 20);
        assert!(*cancellation.borrow_and_update());
    }

    #[tokio::test]
    async fn independent_reads_actually_overlap() {
        // Each call waits for both to start; a serial runner would never reach the barrier twice.
        // 每个调用都等待两者同时开始；串行执行永远无法让屏障凑齐两方。
        let barrier = Arc::new(Barrier::new(2));
        let outputs = tokio::time::timeout(
            Duration::from_secs(5),
            run_batch(
                &AtomicU64::new(0),
                vec!["a", "b"],
                |_| async { Ok::<Option<()>, GatewayError>(None) },
                |id| {
                    let barrier = barrier.clone();
                    async move {
                        barrier.wait().await;
                        id
                    }
                },
                |id, _| id,
            ),
        )
        .await
        .expect("independent reads must run concurrently");
        assert_eq!(outputs, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn results_pair_with_their_calls_when_completion_order_differs() {
        let executions = Arc::new(AtomicUsize::new(0));
        let outputs = run_batch(
            &AtomicU64::new(0),
            vec![("a", 60u64), ("b", 0), ("c", 30)],
            |_| async { Ok::<Option<()>, GatewayError>(None) },
            |(id, delay)| {
                let executions = executions.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    executions.fetch_add(1, Ordering::SeqCst);
                    (id, format!("result-of-{id}"))
                }
            },
            |(id, _), _| (id, "rejected".to_owned()),
        )
        .await;
        assert_eq!(
            outputs,
            vec![
                ("a", "result-of-a".to_owned()),
                ("b", "result-of-b".to_owned()),
                ("c", "result-of-c".to_owned()),
            ]
        );
        assert_eq!(executions.load(Ordering::SeqCst), 3, "every call executes exactly once");
    }

    #[tokio::test]
    async fn short_resources_queue_instead_of_failing_and_a_stop_cancels_waiting_calls() {
        let capacity = Arc::new(Notify::new());
        let started = Arc::new(AtomicUsize::new(0));
        let queued = {
            let capacity = capacity.clone();
            let started = started.clone();
            tokio::spawn(async move {
                run_batch(
                    &AtomicU64::new(0),
                    vec!["first", "queued"],
                    move |_| {
                        let capacity = capacity.clone();
                        async move {
                            capacity.notified().await;
                            Ok::<Option<()>, GatewayError>(Some(()))
                        }
                    },
                    move |id| {
                        let started = started.clone();
                        async move {
                            started.fetch_add(1, Ordering::SeqCst);
                            format!("done:{id}")
                        }
                    },
                    |id, error| format!("not-dispatched:{id}:{}", error.code),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(started.load(Ordering::SeqCst), 1, "the queued call waits for capacity");
        capacity.notify_one();
        assert_eq!(queued.await.unwrap(), vec!["done:first", "done:queued"]);

        let (stop, cancellation) = watch::channel(false);
        let stopped = tokio::spawn(async move {
            run_batch(
                &AtomicU64::new(0),
                vec!["first", "waiting"],
                move |_| {
                    let mut cancellation = cancellation.clone();
                    async move {
                        let _ = cancellation.wait_for(|stopped| *stopped).await;
                        Err::<Option<()>, GatewayError>(GatewayError::new(
                            "RESOURCE_CANCELLED",
                            "资源等待已取消。",
                            499,
                        ))
                    }
                },
                |id| async move { format!("done:{id}") },
                |id, error| format!("not-dispatched:{id}:{}", error.code),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        stop.send(true).unwrap();
        assert_eq!(
            stopped.await.unwrap(),
            vec!["done:first", "not-dispatched:waiting:RESOURCE_CANCELLED"]
        );
    }
}
