//! Native request lifecycle and invocation-bound approval; no second storage or tool ledger.
//! 原生请求生命周期与绑定调用的审批；不另建正式存储或工具账本。

use crate::{
    gateway_contract::{GatewayConfig, GatewayError, new_id, now_iso8601},
    models::gateway_config::ModelConfigStore,
    models::gateway_preparation::PreparationService,
    storage::{gateway as data, task_repository::TaskState},
    tools::gateway::{GatewayToolContext, ToolRuntime},
};
use serde_json::{Value, json};
use crate::tools::approval_grant::{ApprovalScope, TaskApprovalGrants};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, broadcast, oneshot, watch};

const MAXIMUM_REPLAY_CONTROLS: usize = 4096;
const MAXIMUM_REPLAY_BYTES: usize = 128 * 1024 * 1024;
const MAXIMUM_SAFE_ELAPSED_MS: u64 = 9_007_199_254_740_991;

/// Use only saved generation time and a real monotonic start, never a message creation date.
/// 仅使用已保存的生成用时与真实单调起点，不根据消息创建日期推算工作时间。
pub(crate) fn generation_elapsed_ms(base: Option<u64>, started: Option<Instant>) -> Option<u64> {
    if base.is_none() && started.is_none() {
        return None;
    }
    Some(base.unwrap_or(0).saturating_add(
        started.map_or(0, |start| start.elapsed().as_millis().min(MAXIMUM_SAFE_ELAPSED_MS as u128) as u64),
    ).min(MAXIMUM_SAFE_ELAPSED_MS))
}

fn saved_generation_elapsed(value: &Value) -> Option<u64> {
    value.as_u64().filter(|elapsed| *elapsed <= MAXIMUM_SAFE_ELAPSED_MS)
}

#[derive(Default)]
struct GenerationTiming {
    elapsed_ms: Option<u64>,
    started: Option<Instant>,
}

impl GenerationTiming {
    fn elapsed(&self) -> Option<u64> {
        generation_elapsed_ms(self.elapsed_ms, self.started)
    }

    fn set(&mut self, elapsed_ms: u64, active: bool) {
        self.elapsed_ms = Some(elapsed_ms.min(MAXIMUM_SAFE_ELAPSED_MS));
        self.started = active.then(Instant::now);
    }

    fn project(&self, event: &mut Value) {
        if let Some(elapsed) = self.elapsed() {
            event["generationElapsedMs"] = json!(elapsed);
            event["generationActive"] = json!(self.started.is_some());
        }
    }
}

#[derive(Default)]
struct ReplayState {
    sequence: u64,
    controls: VecDeque<Value>,
    bytes: usize,
    content: String,
    reasoning: String,
    active_segment: Option<Value>,
    reply_anchor: Option<Value>,
    generation: GenerationTiming,
    unavailable: bool,
}

pub(crate) struct RecoveryBatch {
    pub events: VecDeque<Value>,
    pub through: u64,
}

pub(crate) fn terminal_event(event: &Value) -> bool {
    matches!(
        event["type"].as_str(),
        Some("completed" | "interrupted" | "error")
    )
}

pub struct RequestRun {
    pub conversation_id: String,
    pub request_id: String,
    pub fingerprint: String,
    pub original_message_raw: Option<String>,
    pub journal: Option<Arc<super::gateway_task_journal::TaskJournal>>,
    pub parent_task_id: Option<String>,
    pub cancellation: watch::Sender<bool>,
    pub steering: watch::Sender<u64>,
    pub native_cancellation: crate::service_cancellation::CancellationLease,
    pub cancellation_key: String,
    pub events: broadcast::Sender<Value>,
    emission: Mutex<()>,
    replay: Mutex<ReplayState>,
    pub finished: AtomicBool,
    pub suspending: AtomicBool,
    pub result: Mutex<Option<Value>>,
    observed_context_limits: std::sync::Mutex<Value>,
    pub created: Instant,
    pub resource_wait_ms: AtomicU64,
    /// Persisted active wall time before this attempt; parallel descendants never add to this clock.
    /// 本次尝试前已持久化的有效墙钟；并行子任务不会重复计入此时钟。
    pub active_duration_base_ms: u64,
    /// Restored background work yields new model admission to current user interaction.
    /// 后台恢复任务在新增模型准入时让出当前用户交互的优先位置。
    pub is_background_recovery: AtomicBool,
    /// Set when repeated recovery made no new model request; the next execute settles instead of retrying.
    /// 连续恢复没有产生新的模型请求时置位；下一次执行如实收尾，不再重试。
    pub recovery_stalled: std::sync::atomic::AtomicBool,
}
impl RequestRun {
    /// Reuse the current replay projection across reattachment; offline time is never added.
    /// 复用现有回放投影恢复计时；离线与暂停等待不计入已生成用时。
    pub(crate) async fn restore_generation_timing(&self, saved: Option<u64>) -> Option<u64> {
        let _emission = self.emission.lock().await;
        let mut replay = self.replay.lock().await;
        let elapsed = match (replay.generation.elapsed(), saved) {
            (Some(current), Some(saved)) => Some(current.max(saved)),
            (current, saved) => current.or(saved),
        };
        if let Some(elapsed) = elapsed {
            replay.generation.set(elapsed, false);
        }
        elapsed
    }

    /// Freeze the existing projection when execution yields, independently of visible text segments.
    /// 执行让出时冻结现有计时投影，不依赖是否发布了可见文字分段。
    pub(crate) async fn freeze_generation_timing(&self, settled: Option<u64>) {
        let elapsed = settled.or(self.replay.lock().await.generation.elapsed());
        if let Some(elapsed) = elapsed {
            self.emit(json!({"type":"generation_timing","generationElapsedMs":elapsed,
                "generationActive":false})).await;
        }
    }

    pub(crate) async fn generation_elapsed(&self) -> Option<u64> {
        self.replay.lock().await.generation.elapsed()
    }

    /// Model-step retries share learned limits only while the verified connection identity stays unchanged.
    /// 仅在核验连接身份不变时跨模型步骤复用已学额度；请求重建不能恢复已被拒绝的预算。
    pub(crate) fn observed_model_limits(&self, origin: &str) -> Value {
        let observed = self.observed_context_limits.lock().unwrap();
        if observed["origin"].as_str() == Some(origin) { observed["limits"].clone() } else { Value::Null }
    }

    pub(crate) fn retain_model_limits(&self, origin: &str, limits: &Value) {
        let mut observed = self.observed_context_limits.lock().unwrap();
        let previous = if observed["origin"].as_str() == Some(origin) { &observed["limits"] } else { &Value::Null };
        *observed = json!({"origin":origin,"limits":crate::models::recovery::retain_numeric_limits(previous, limits)});
    }

    /// Resource admission is suspended execution, not time spent running the accepted task.
    /// 资源准入等待属于执行挂起，不消耗已接受任务的实际执行时限。
    pub fn active_elapsed(&self, start: Instant) -> Duration {
        start.elapsed().saturating_sub(Duration::from_millis(
            self.resource_wait_ms.load(Ordering::Acquire),
        ))
    }

    pub(crate) fn effective_active_ms(&self) -> u64 {
        self.active_duration_base_ms.saturating_add(self.active_elapsed(self.created).as_millis() as u64)
    }

    /// A checkpoint advances the current attempt's monotonic wall-time high-water only once.
    /// 检查点只推进当前尝试的单调墙钟高水位；重复保存不会重复计时。
    pub(crate) async fn checkpoint_active_duration(&self) -> Result<(), GatewayError> {
        let Some(journal) = &self.journal else { return Ok(()); };
        let repository = journal.repository().clone();
        let token = journal.token().clone();
        let elapsed = self.active_elapsed(self.created).as_millis() as u64;
        tokio::task::spawn_blocking(move || repository.record_active_duration(&token, elapsed))
            .await.map_err(|_| GatewayError::new("TASK_CLOCK_SAVE_FAILED", "任务有效时长保存暂不可用，进度仍保留。", 503))??;
        Ok(())
    }

    pub fn cancel(&self) {
        self.native_cancellation
            .token
            .store(true, Ordering::Release);
        self.cancellation.send_replace(true);
    }

    /// Keep one queued control across temporary storage pressure without cancelling its accepted task.
    /// 临时持久化拥塞时保留同一控制事件，等待原批次落盘，不取消已接受任务或重派副作用。
    async fn persist_event(
        &self,
        journal: &Arc<super::gateway_task_journal::TaskJournal>,
        kind: &str,
        durable: &Value,
        _terminal: bool,
    ) -> Result<(), GatewayError> {
        let mut queued = false;
        let mut cancellation = self.cancellation.subscribe();
        let mut delay = Duration::from_millis(100);
        let mut waiting_since = None;
        let result = loop {
            if !queued {
                match journal.emit_control(kind, durable) {
                    Ok(()) => queued = true,
                    Err(error) if error.code == "TASK_EVENT_BACKPRESSURE" => {
                        waiting_since.get_or_insert_with(Instant::now);
                    }
                    Err(error) => break Err(error),
                }
            }
            // A displayed delta is a durable promise too: enqueueing alone cannot acknowledge it.
            // 可见正文增量同样承诺可恢复；仅入内存队列不能作为提交确认或推进回放水位。
            match journal.flush().await {
                Ok(()) if queued => break Ok(()),
                Ok(()) => continue,
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "TASK_STORAGE_FAILED" | "TASK_EVENT_FLUSH_FAILED"
                    ) =>
                {
                    waiting_since.get_or_insert_with(Instant::now);
                    if *cancellation.borrow() || self.suspending.load(Ordering::Acquire) {
                        break Err(error);
                    }
                    // Flush owns stable event IDs; retrying storage never repeats a model or tool call.
                    // flush 持有稳定事件 ID；存储重试不重复模型请求或工具调用，用户取消仍立即唤醒。
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {},
                        _ = cancellation.changed() => break Err(error),
                    }
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
                Err(error) => break Err(error),
            }
        };
        if let Some(started) = waiting_since {
            self.resource_wait_ms.fetch_add(
                started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                Ordering::AcqRel,
            );
        }
        result
    }

    pub(crate) async fn published_stream_sequence(&self) -> u64 {
        self.replay.lock().await.sequence
    }

    pub async fn emit(&self, event: Value) {
        // Large provider chunks are split on UTF-8 boundaries, never replaced by an empty archive marker.
        // 供应商大块正文按UTF-8边界拆分；已发布的任何字符都不能被空归档标记替代。
        if matches!(event["type"].as_str(), Some("text_delta" | "reasoning_delta")) {
            if let Some(delta) = event["delta"].as_str().filter(|delta| delta.len() > 16 * 1024 || event.to_string().len() > 32 * 1024) {
                let mut start: usize = 0;
                while start < delta.len() {
                    let mut end = start.saturating_add(16 * 1024).min(delta.len());
                    while !delta.is_char_boundary(end) { end -= 1; }
                    let mut portion = event.clone();
                    portion["delta"] = json!(&delta[start..end]);
                    while portion.to_string().len() > 32 * 1024 && end > start + 1 {
                        let first_character_end = start + delta[start..].chars().next().unwrap().len_utf8();
                        if end <= first_character_end { break; }
                        end = start + (end - start) / 2;
                        while end > start && !delta.is_char_boundary(end) { end -= 1; }
                        if end == start { end = start + delta[start..].chars().next().unwrap().len_utf8(); }
                        portion["delta"] = json!(&delta[start..end]);
                    }
                    self.emit_one(portion).await;
                    start = end;
                }
                return;
            }
        }
        self.emit_one(event).await;
    }

    async fn emit_one(&self, mut event: Value) {
        // Remote task handles are private recovery evidence, never an unrecognized desktop stream event.
        // 远端任务句柄属于私有恢复证据，不能作为桌面无法识别的新事件进入聊天流。
        if event["type"] == "mcp_task_checkpoint" {
            if let Some(journal) = &self.journal {
                event["conversationId"] = json!(self.conversation_id);
                event["requestId"] = json!(self.request_id);
                if !journal.is_released() { let _ = self.persist_event(journal, "mcp_task_checkpoint", &event, true).await; }
            }
            return;
        }
        let _emission = self.emission.lock().await;
        let mut replay = self.replay.lock().await;
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        event["conversationId"] = json!(self.conversation_id);
        event["requestId"] = json!(self.request_id);
        event["createdAt"] = json!(now_iso8601());
        event["eventSequence"] = json!(replay.sequence + 1);
        if event["replyMessageId"].is_null() {
            event["replyMessageId"] = replay.reply_anchor.as_ref()
                .map(|anchor| anchor["replyMessageId"].clone())
                .unwrap_or_else(||json!(self.request_id));
        }
        if event["type"] == "generation_timing" {
            if let (Some(elapsed), Some(active)) = (
                saved_generation_elapsed(&event["generationElapsedMs"]),
                event["generationActive"].as_bool(),
            ) {
                replay.generation.set(elapsed, active);
            }
        } else if terminal_event(&event) {
            if let Some(elapsed) = saved_generation_elapsed(&event["durationMs"])
                .or_else(|| replay.generation.elapsed()) {
                replay.generation.set(elapsed, false);
            }
        }
        if matches!(event["type"].as_str(), Some("generation_timing" | "reply_anchor" | "content_snapshot"))
            || terminal_event(&event) {
            replay.generation.project(&mut event);
        }
        drop(replay);
        // Persist compact deltas and formal references; never re-encode the growing full reply per token.
        // 持久化小增量和正式引用；每个 token 不重复编码不断增长的完整正文。
        if let Some(journal) = &self.journal {
            if event["taskId"].is_null() {
                event["taskId"] = json!(journal.task_id());
            }
            event["rootRequestTaskId"] = json!(journal.task_id());
            let mut durable = if terminal_event(&event) {
                json!({"type":event["type"],"code":event["code"],"conversationId":self.conversation_id,"requestId":self.request_id,"assistantMessageId":event["replyMessageId"],"taskCompletion":event["taskCompletion"]})
            } else if matches!(event["type"].as_str(), Some("text_delta" | "reasoning_delta")) {
                event.clone()
            } else if event["type"] == "assistant_segment" {
                let mut segment = event["segment"].clone();
                let empty = segment["content"].as_str() == Some("") && segment["reasoning"].as_str() == Some("");
                if let Some(fields) = segment.as_object_mut() { fields.remove("content"); fields.remove("reasoning"); }
                segment["emptyAtEmission"] = json!(empty);
                json!({"type":"assistant_segment","segmentId":segment["id"],"phase":segment["phase"],"status":segment["status"],"segment":segment})
            } else if event.to_string().len() <= 24 * 1024 {
                event.clone()
            } else {
                json!({"type":event["type"],"conversationId":self.conversation_id,"requestId":self.request_id,"toolCallId":event["tool"]["toolCallId"],"resultRef":event["tool"]["resultRef"],"archived":true})
            };
            // Preserve the original stream identity even when a control body uses a compact reference.
            // 控制正文采用精简引用时也保留原请求、回复与流序号；恢复不得猜归属或从零复用序号。
            for field in ["conversationId", "requestId", "replyMessageId", "eventSequence"] {
                durable[field] = event[field].clone();
            }
            if !journal.is_released() {
                if let Err(error) = self
                    .persist_event(
                        journal,
                        event["type"].as_str().unwrap_or("progress"),
                        &durable,
                        terminal_event(&event),
                    )
                    .await
                {
                    // Ownership/invariant failures stay diagnostic; they are never forged into a user cancellation.
                    // 执行权或不变量错误继续保留诊断，不伪装成用户取消；后续派发仍须通过正式日志围栏。
                    journal.remember_persistence_error(error);
                    // Never publish an uncommitted event; cancellation/ownership recovery retains the journal.
                    // 未提交事件不能发布；取消或执行权恢复保留原日志，不伪造已交付内容或终态。
                    return;
                }
            }
        }
        // Publish replay watermarks only after persistence has stopped waiting.
        // 持久化等待结束后才推进回放水位，避免新观察者提前终结或跳过尚未发布的事件。
        let mut replay = self.replay.lock().await;
        replay.sequence = event["eventSequence"].as_u64().unwrap();
        if event["type"] == "reply_anchor" {
            // A new reply changes only replay/display ownership; the durable task journal keeps earlier receipts.
            // 新回复仅切换回放与展示归属；正式任务日志继续保留此前调用回执。
            replay.content.clear();
            replay.reasoning.clear();
            replay.active_segment = None;
            replay.controls.clear();
            replay.bytes = 0;
            replay.unavailable = false;
            replay.reply_anchor = Some(event.clone());
        }
        if !replay.unavailable {
            match event["type"].as_str() {
                Some("text_delta") => {
                    let delta = event["delta"].as_str().unwrap_or("");
                    replay.content.push_str(delta);
                    if let Some(segment) = replay
                        .active_segment
                        .as_mut()
                        .filter(|segment| segment["id"] == event["segmentId"])
                    {
                        if let Some(Value::String(content)) = segment.get_mut("content") {
                            content.push_str(delta);
                        }
                    }
                }
                Some("reasoning_delta") => {
                    let delta = event["delta"].as_str().unwrap_or("");
                    replay.reasoning.push_str(delta);
                    if let Some(segment) = replay
                        .active_segment
                        .as_mut()
                        .filter(|segment| segment["id"] == event["segmentId"])
                    {
                        if let Some(Value::String(content)) = segment.get_mut("reasoning") {
                            content.push_str(delta);
                        }
                    }
                }
                Some("assistant_segment") => {
                    replay.active_segment = if event["segment"]["status"] == "streaming" {
                        Some(event["segment"].clone())
                    } else {
                        None
                    };
                }
                Some("content_snapshot") => {
                    // Recovery may replace a reply projection before new deltas; later observers need that same base.
                    // 恢复可在新增量前替换回复投影；后续观察者必须取得相同的正文基线。
                    replay.content = event["content"].as_str().unwrap_or("").to_owned();
                    replay.reasoning = event["reasoning"].as_str().unwrap_or("").to_owned();
                }
                Some("completed" | "interrupted") => {
                    if let Some(content) = event["content"].as_str() {
                        replay.content = content.to_owned();
                    }
                    if let Some(reasoning) = event["reasoning"].as_str() {
                        replay.reasoning = reasoning.to_owned();
                    }
                    replay.active_segment = None;
                }
                _ => {}
            }
            // Preserve protocol boundaries, never an unbounded journal of token or terminal-output events.
            // 保留协议边界，不无限记录 token 或终端输出事件；重放不增加执行或权限事实。
            if !matches!(
                event["type"].as_str(),
                Some("text_delta" | "reasoning_delta" | "terminal_output" | "content_snapshot")
            ) {
                replay.bytes = replay.bytes.saturating_add(event.to_string().len());
                replay.controls.push_back(event.clone());
            }
            if replay.controls.len() > MAXIMUM_REPLAY_CONTROLS
                || replay
                    .bytes
                    .saturating_add(replay.content.len())
                    .saturating_add(replay.reasoning.len())
                    > MAXIMUM_REPLAY_BYTES
            {
                replay.unavailable = true;
                replay.controls.clear();
                replay.content.clear();
                replay.reasoning.clear();
                replay.active_segment = None;
            }
        }
        // Publish the final reply before waking either HTTP reader; emit/finish cannot leave a lost wakeup.
        // 唤醒 HTTP 读取者前发布最终结果；emit/finish 之间不能出现漏唤醒窗口。
        if terminal_event(&event) {
            *self.result.lock().await = Some(event.clone());
            self.finished.store(true, Ordering::Release);
        }
        let _ = self.events.send(event);
    }
    pub async fn finish(&self, result: Value) {
        self.emit(result).await;
    }

    pub(crate) async fn recovery(&self, after: u64) -> Result<RecoveryBatch, GatewayError> {
        let replay = self.replay.lock().await;
        if replay.unavailable {
            return Err(GatewayError::new(
                "CHAT_REPLAY_CAPACITY_EXCEEDED",
                "任务回放超过传输缓存额度，请读取已保存的正式记录。",
                409,
            ));
        }
        let mut events: VecDeque<Value> = replay
            .controls
            .iter()
            .filter(|event| event["eventSequence"].as_u64().unwrap_or(0) > after)
            .cloned()
            .collect();
        if replay.sequence <= after || replay.sequence == 0 {
            if replay.generation.elapsed().is_some() {
                let mut timing = json!({"type":"generation_timing",
                    "replyMessageId":replay.reply_anchor.as_ref().map(|anchor|&anchor["replyMessageId"])
                        .unwrap_or(&Value::Null),
                    "conversationId":self.conversation_id,"requestId":self.request_id,"createdAt":now_iso8601(),
                    "replayThrough":replay.sequence});
                replay.generation.project(&mut timing);
                events.push_back(timing);
            }
            return Ok(RecoveryBatch {
                events,
                through: replay.sequence,
            });
        }
        let terminal = events.back().filter(|event| terminal_event(event)).cloned();
        if let Some(anchor) = &replay.reply_anchor {
            if !events.iter().any(|event|event["type"]=="reply_anchor") {
                let mut anchor = anchor.clone();
                replay.generation.project(&mut anchor);
                events.push_front(anchor);
            }
        }
        if terminal.is_some() {
            events.pop_back();
        }
        if !self.finished.load(Ordering::Acquire) {
            if let Some(segment) = &replay.active_segment {
                events.push_back(json!({"type":"assistant_segment","replyMessageId":replay.reply_anchor.as_ref().map(|anchor|&anchor["replyMessageId"]),"segment":segment,"toolStreamProtocol":3,"conversationId":self.conversation_id,"requestId":self.request_id,"createdAt":now_iso8601(),"replayThrough":replay.sequence}));
            }
        }
        let mut snapshot = json!({"type":"content_snapshot","replyMessageId":replay.reply_anchor.as_ref().map(|anchor|&anchor["replyMessageId"]),"content":replay.content,"reasoning":replay.reasoning,"conversationId":self.conversation_id,"requestId":self.request_id,"createdAt":now_iso8601(),"replayThrough":replay.sequence});
        replay.generation.project(&mut snapshot);
        events.push_back(snapshot);
        if let Some(terminal) = terminal {
            events.push_back(terminal);
        }
        Ok(RecoveryBatch {
            events,
            through: replay.sequence,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_run(capacity: usize) -> Arc<Self> {
        let (cancellation, _) = watch::channel(false);
        let (events, _) = broadcast::channel(capacity);
        let cancellation_key = new_id();
        Arc::new(Self {
            conversation_id: new_id(),
            request_id: new_id(),
            fingerprint: String::new(),
            original_message_raw: None,
            journal: None,
            parent_task_id: None,
            cancellation,
            steering: watch::channel(0).0,
            native_cancellation: crate::service_cancellation::CancellationLease::acquire(
                &cancellation_key,
            )
            .unwrap(),
            cancellation_key,
            events,
            emission: Default::default(),
            replay: Default::default(),
            finished: AtomicBool::new(false),
            suspending: AtomicBool::new(false),
            result: Default::default(),
            observed_context_limits: Default::default(),
            created: Instant::now(),
            resource_wait_ms: AtomicU64::new(0),
            active_duration_base_ms: 0,
            is_background_recovery: AtomicBool::new(false),
            recovery_stalled: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

#[derive(Clone, Copy, Default)]
struct ApprovalDecision { approved: bool, scope: ApprovalScope }
struct LiveTaskApproval {
    conversation_id: String,
    bindings: TaskApprovalGrants,
    touched: Instant,
}
#[derive(Default)]
struct RuntimeApprovals {
    pending: HashMap<String, Approval>,
    tasks: HashMap<String, LiveTaskApproval>,
}
struct Approval {
    conversation: String,
    request: String,
    call: String,
    response: oneshot::Sender<ApprovalDecision>,
    owner: Weak<RequestRun>,
    task_binding: Option<(String, String)>,
    expires: Instant,
    deadline_code: &'static str,
}
pub struct GatewayRuntime {
    pub config: GatewayConfig,
    pub tools: Arc<ToolRuntime>,
    pub models: ModelConfigStore,
    pub model_preparation: Arc<PreparationService>,
    pub tasks: Arc<super::gateway_task_hub::TaskHub>,
    pub model_slots: Arc<super::gateway_model_pool::ModelSlots>,
    task_runs: std::sync::Mutex<HashMap<String, Weak<RequestRun>>>,
    task_journals: std::sync::Mutex<Vec<Weak<super::gateway_task_journal::TaskJournal>>>,
    runs: Mutex<HashMap<String, Arc<RequestRun>>>,
    conversations: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    approvals: Mutex<RuntimeApprovals>,
    execution_tasks: AtomicUsize,
    extension_prewarm_scheduled: AtomicBool,
    background_cancellation: watch::Sender<bool>,
}
impl GatewayRuntime {
    pub(crate) fn live_task_run(&self, task_id: &str) -> Option<Arc<RequestRun>> {
        self.task_runs.lock().ok().and_then(|runs| runs.get(task_id).and_then(Weak::upgrade))
            .filter(|run| !run.finished.load(Ordering::Acquire))
    }
    /// Reuse live ancestor attempts to renew operational slices without weakening explicit cost caps.
    /// 复用仍有效的祖先尝试续展运行预算，不改变明确的费用/token上限，也不创建新账本。
    pub(crate) fn budget_journals(&self, run: &RequestRun) -> Result<Vec<Arc<super::gateway_task_journal::TaskJournal>>, GatewayError> {
        let Some(own) = &run.journal else { return Ok(Vec::new()); };
        let mut result = vec![own.clone()];
        let registered = self.task_journals.lock().map_err(|_| GatewayError::new("TASK_JOURNAL_REGISTRY_FAILED", "任务预算状态暂不可用。", 503))?;
        let mut parent = own.load_current()?.parent_task_id;
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = parent {
            if !visited.insert(id.clone()) { return Err(GatewayError::new("TASK_DEPENDENCY_CYCLE", "任务关系异常，已有进度保留。", 409)); }
            let Some(journal) = registered.iter().filter_map(Weak::upgrade)
                .find(|journal| journal.task_id() == id && journal.check_dispatch_fence().is_ok()) else { break; };
            parent = journal.load_current()?.parent_task_id;
            result.push(journal);
        }
        Ok(result)
    }
    pub fn new(config: GatewayConfig) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            models: ModelConfigStore::new(&config.data_home),
            model_preparation: PreparationService::new(config.clone()),
            tasks: Arc::new(super::gateway_task_hub::TaskHub::new(
                config.clone(),
                weak.clone(),
            )),
            model_slots: Arc::new(super::gateway_model_pool::ModelSlots::default()),
            task_runs: Default::default(),
            task_journals: Default::default(),
            tools: Arc::new(ToolRuntime::with_gateway_config(&config)),
            config,
            runs: Default::default(),
            conversations: Default::default(),
            approvals: Default::default(),
            execution_tasks: AtomicUsize::new(0),
            extension_prewarm_scheduled: AtomicBool::new(false),
            background_cancellation: watch::channel(false).0,
        })
    }
    pub async fn conversation_gate(&self, id: &str) -> Arc<Mutex<()>> {
        let mut gates = self.conversations.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(id).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(id.to_owned(), Arc::downgrade(&gate));
        gate
    }

    /// Health is delivered first; optional local extensions start later without joining startup readiness.
    /// 先交付健康就绪；可选本地扩展延后启动，不加入软件启动等待，也不继承工作目录权限。
    pub fn schedule_extension_prewarm(self: &Arc<Self>) {
        if self
            .extension_prewarm_scheduled
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut cancellation = runtime.background_cancellation.subscribe();
            if *cancellation.borrow() {
                return;
            }
            // Prefer model connection/loading before optional MCP work; none of this joins the UI readiness wait.
            // 模型连接与装载优先于可选 MCP；整个准备过程不加入界面就绪或消息发送等待。
            // An empty selection reuses the saved choice, or observes unselected local models without loading them.
            // 空选择沿用已有明确选择；尚未收到选择时，本地模型只观察不加载，API 连接无需空等桌面。
            let _ = runtime.prepare_models(Value::Null).await;
            runtime
                .model_preparation
                .wait_idle(cancellation.clone(), Duration::from_secs(90))
                .await;
            // Delay only optional extensions; the model connection has already started independently.
            // 仅延后可选扩展，模型连接已独立开始准备。
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {},
                _ = cancellation.changed() => return,
            }
            if *cancellation.borrow() {
                return;
            }
            let mut context = GatewayToolContext {
                data_home: runtime.config.data_root.clone(),
                extension_root: runtime.config.extension_root.clone(),
                workdir: runtime.config.data_root.join("Sandboxes"),
                toolhost_exe: runtime.config.toolhost_exe.clone(),
                permission_mode: "ask".into(),
                conversation_id: String::new(),
                project_id: None,
                authorized_roots: vec![],
                cancellation,
                runtime: runtime.tools.clone(),
                config: json!({"officialToolsRoot":runtime.config.application_root.join("official-tools")}),
                approved: false,
                events: None,
                external_execute: None,
            };
            if let Ok(mut config) = crate::tools::gateway::get_config(&context).await {
                config["officialToolsRoot"] =
                    json!(runtime.config.application_root.join("official-tools"));
                context.config = config;
                crate::tools::gateway::prewarm(&context);
            }
        });
    }
    pub async fn prepare_models(self: &Arc<Self>, selection: Value) -> Result<Value, GatewayError> {
        let status = self
            .model_preparation
            .schedule(
                ModelConfigStore::new(&self.config.data_home),
                selection,
                self.background_cancellation.subscribe(),
            )
            .await?;
        if status["scheduled"] == true {
            let runtime = self.clone();
            let priority = crate::tools::gateway::foreground(&runtime.tools);
            tokio::spawn(async move {
                let _priority = priority;
                runtime
                    .model_preparation
                    .wait_idle(
                        runtime.background_cancellation.subscribe(),
                        Duration::from_secs(90),
                    )
                    .await;
            });
        }
        Ok(status)
    }
    pub async fn begin(self: &Arc<Self>, input: Value) -> Result<Arc<RequestRun>, GatewayError> {
        self.begin_raw(input, None).await
    }
    pub async fn begin_raw(
        self: &Arc<Self>,
        input: Value,
        original_message_raw: Option<String>,
    ) -> Result<Arc<RequestRun>, GatewayError> {
        self.begin_with_priority(input, original_message_raw, false).await
    }
    async fn begin_with_priority(
        self: &Arc<Self>,
        mut input: Value,
        original_message_raw: Option<String>,
        background: bool,
    ) -> Result<Arc<RequestRun>, GatewayError> {
        let conversation = checked_id(&input["conversationId"], "conversationId")?;
        let attachments = input.get("attachmentIds").filter(|value| !value.is_null());
        if let Some(attachments) = attachments {
            let ids = attachments.as_array().filter(|ids| ids.len() <= 128)
                .ok_or_else(|| GatewayError::new("INVALID_CHAT_REQUEST", "附件列表无效。", 400))?;
            let mut seen = std::collections::HashSet::new();
            for id in ids {
                if !seen.insert(checked_id(id, "attachmentId")?) {
                    return Err(GatewayError::new("INVALID_CHAT_REQUEST", "附件 ID 重复。", 400));
                }
            }
        }
        if !input["message"].as_str().is_some_and(|text| {
            (!text.trim().is_empty() || attachments.is_some_and(|ids| ids.as_array().is_some_and(|ids| !ids.is_empty())))
                && text.encode_utf16().count() <= 2 * 1024 * 1024
        }) {
            return Err(GatewayError::new(
                "INVALID_CHAT_REQUEST",
                "消息不能为空或超过输入限制。",
                400,
            ));
        }
        for field in ["requestId", "userMessageId"] {
            if input[field].is_null() {
                input[field] = json!(new_id());
            }
        }
        let request_id = checked_id(&input["requestId"], "requestId")?;
        let message_id = checked_id(&input["userMessageId"], "userMessageId")?;
        if request_id == message_id {
            return Err(GatewayError::new(
                "INVALID_CHAT_REQUEST",
                "请求与消息 ID 必须不同。",
                400,
            ));
        }
        use sha2::{Digest, Sha256};
        let units = if let Some(raw) = &original_message_raw {
            let projected =
                serde_json::from_str::<Value>(&crate::storage::raw_json::scalar_projection(raw))
                    .map_err(|_| {
                        GatewayError::new("INVALID_CHAT_REQUEST", "消息编码无效。", 400)
                    })?;
            if projected != input["message"] {
                return Err(GatewayError::new(
                    "INVALID_CHAT_REQUEST",
                    "消息原文与投影不一致。",
                    400,
                ));
            }
            crate::storage::raw_json::utf16_identity(raw).ok_or_else(|| {
                GatewayError::new("INVALID_CHAT_REQUEST", "消息原文不是字符串。", 400)
            })?
        } else {
            input["message"]
                .as_str()
                .unwrap()
                .encode_utf16()
                .collect::<Vec<_>>()
        };
        let mut identity = Sha256::new();
        for unit in units {
            identity.update(unit.to_le_bytes());
        }
        let fingerprint = crate::retrieval::gateway_index::hash(
            &json!([input, format!("{:x}", identity.finalize())]).to_string(),
        );
        let acceptance_gate = self
            .conversation_gate(&format!("accept:{request_id}"))
            .await;
        let _acceptance = acceptance_gate.lock().await;
        let mut runs = self.runs.lock().await;
        if let Some(existing) = runs.get(&request_id) {
            if !background { existing.is_background_recovery.store(false, Ordering::Release); }
            if existing.conversation_id != conversation || existing.fingerprint != fingerprint {
                return Err(GatewayError::new(
                    "CHAT_REQUEST_CONFLICT",
                    "请求 ID 已用于不同内容。",
                    409,
                ));
            }
            if !existing.finished.load(Ordering::Acquire)
                || existing
                    .result
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|value| value["type"] == "completed")
            {
                return Ok(existing.clone());
            }
            runs.remove(&request_id);
        }
        runs.retain(|_, run| {
            !run.finished.load(Ordering::Acquire) || run.created.elapsed().as_secs() < 1800
        });
        if runs.len() >= 64 {
            return Err(GatewayError::new(
                "CHAT_QUEUE_FULL",
                "当前任务队列已满，已有任务继续运行。",
                429,
            ));
        }
        drop(runs);
        let task = super::gateway_task_input::accept(
            &self.config,
            &input,
            original_message_raw.as_deref(),
            &fingerprint,
        )
        .await?;
        use crate::storage::task_repository::TaskState;
        if matches!(
            task.state,
            TaskState::Paused | TaskState::Cancelled | TaskState::Failed | TaskState::Partial
        ) {
            return Err(GatewayError::new(
                "TASK_CONTROLLED",
                "任务已暂停或结束，请查看任务状态并明确继续。",
                409,
            ));
        }
        let journal = if task.state == TaskState::Completed {
            None
        } else {
            Some(super::gateway_task_journal::TaskJournal::start_existing(
                &self.config.data_root,
                &task,
                &format!("gateway:{}", std::process::id()),
                30_000,
            )?)
        };
        let (cancellation, _) = watch::channel(false);
        let (events, _) = broadcast::channel(256);
        let cancellation_key = new_id();
        let native_cancellation =
            crate::service_cancellation::CancellationLease::acquire(&cancellation_key)
                .map_err(|code| GatewayError::new(code, "任务取消状态无法创建。", 503))?;
        let run = Arc::new(RequestRun {
            conversation_id: conversation,
            request_id: request_id.clone(),
            fingerprint,
            original_message_raw,
            journal,
            parent_task_id: None,
            cancellation,
            steering: watch::channel(0).0,
            events,
            emission: Default::default(),
            native_cancellation,
            cancellation_key,
            // The ledger high-water includes every published event and survives a new execution attempt.
            // 账本水位覆盖已发布事件并跨尝试保留；私有记录只造成跳号，不复用客户端已经确认的序号。
            replay: Mutex::new(ReplayState { sequence:task.event_sequence, ..Default::default() }),
            finished: AtomicBool::new(false),
            suspending: AtomicBool::new(false),
            result: Default::default(),
            observed_context_limits: Default::default(),
            created: Instant::now(),
            resource_wait_ms: AtomicU64::new(0),
            active_duration_base_ms: task.usage.active_duration_ms.unwrap_or(0),
            is_background_recovery: AtomicBool::new(background),
            recovery_stalled: std::sync::atomic::AtomicBool::new(false),
        });
        self.task_runs
            .lock()
            .map_err(|_| GatewayError::new("TASK_RUN_LOCK_FAILED", "任务执行者登记失败。", 500))?
            .insert(task.task_id, Arc::downgrade(&run));
        if let Some(journal) = &run.journal {
            self.register_journal(journal)?;
        }
        self.runs.lock().await.insert(request_id, run.clone());
        let runtime = self.clone();
        let operation = run.clone();
        self.execution_tasks.fetch_add(1, Ordering::AcqRel);
        tokio::spawn(async move {
            struct ExecutionTask(Arc<GatewayRuntime>);
            impl Drop for ExecutionTask {
                fn drop(&mut self) {
                    self.0.execution_tasks.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _execution = ExecutionTask(runtime.clone());
            super::gateway_loop::run(runtime, operation, input).await;
        });
        Ok(run)
    }
    pub async fn cancel(&self, conversation: &str, request: &str) -> Result<Value, GatewayError> {
        let run = self
            .runs
            .lock()
            .await
            .get(request)
            .filter(|run| run.conversation_id == conversation)
            .cloned()
            .ok_or_else(|| GatewayError::new("CHAT_RUN_NOT_FOUND", "任务不存在。", 404))?;
        if let Some(journal) = &run.journal {
            let task = journal.load_current()?;
            self.tasks
                .control(
                    &task.task_id,
                    task.version,
                    crate::storage::task_repository::TaskControl::Cancel,
                )
                .await?;
        }
        run.cancel();
        self.retire_terminal_approvals(&run).await;
        Ok(json!({"cancelled":true}))
    }
    pub async fn run_status(&self, conversation: &str, request: &str) -> Option<Value> {
        let run = self
            .runs
            .lock()
            .await
            .get(request)
            .filter(|run| run.conversation_id == conversation)
            .cloned()?;
        let result = run.result.lock().await.clone();
        let replay = run.replay.lock().await;
        let mut reply_anchor = replay.reply_anchor.clone();
        if let Some(anchor) = &mut reply_anchor {
            replay.generation.project(anchor);
        }
        let mut status = json!({"requestId":request,"conversationId":conversation,
            "status":if run.finished.load(Ordering::Acquire){"completed"}else{"running"},
            "result":result,"replyAnchor":reply_anchor});
        replay.generation.project(&mut status);
        Some(status)
    }
    /// Reattach only to an already accepted request in this exact conversation.
    /// 仅重新连接当前聊天中已接受的原请求；观察不创建任务，也不重复发送用户消息。
    pub async fn observe_request(
        &self,
        conversation: &str,
        request: &str,
    ) -> Result<Arc<RequestRun>, GatewayError> {
        self.runs
            .lock()
            .await
            .get(request)
            .filter(|run| run.conversation_id == conversation)
            .cloned()
            .ok_or_else(|| GatewayError::new("CHAT_RUN_NOT_FOUND", "任务不存在。", 404))
    }
    /// Attach to an existing task execution; observing a stream never starts or replays an operation.
    /// 连接已有任务执行，观察事件流不启动模型，也不重放操作；等待恢复执行者登记有界。
    pub async fn observe_task(
        &self,
        task_id: &str,
        conversation: &str,
    ) -> Result<Arc<RequestRun>, GatewayError> {
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            let task = self.tasks.metadata(task_id).await?;
            if task.scope.conversation_id != conversation || task.parent_task_id.is_some() {
                return Err(GatewayError::new(
                    "TASK_SCOPE_DENIED",
                    "只能观察本聊天的主任务回复。",
                    403,
                ));
            }
            if let Some(run) = self.runs.lock().await.get(&task.request_id).cloned() {
                let owns_attempt = run.journal.as_ref().is_some_and(|journal| {
                    journal.task_id() == task.task_id
                        && task.current_attempt_id.as_deref() == Some(journal.token().attempt_id.as_str())
                        && task.execution_version == journal.token().execution_version
                });
                if task.state != TaskState::Queued && owns_attempt
                    && run.conversation_id == conversation
                {
                    return Ok(run);
                }
            }
            if !matches!(task.state, TaskState::Queued | TaskState::Running | TaskState::WaitingChildren)
                || Instant::now() >= deadline
            {
                return Err(GatewayError::new(
                    "TASK_STREAM_NOT_READY",
                    "任务回复尚未可连接，已保存的进度仍可读取。",
                    409,
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    /// Child agents have independent execution/cancellation, while retaining the parent's permission scope.
    /// 子代理拥有独立执行和取消身份，权限范围仍由父任务确定。
    fn register_journal(
        &self,
        journal: &Arc<super::gateway_task_journal::TaskJournal>,
    ) -> Result<(), GatewayError> {
        let mut journals = self.task_journals.lock().map_err(|_| {
            GatewayError::new(
                "TASK_JOURNAL_REGISTRY_FAILED",
                "任务日志生命周期状态不可用。",
                503,
            )
        })?;
        journals.retain(|journal| journal.strong_count() > 0);
        journals.push(Arc::downgrade(journal));
        Ok(())
    }

    pub fn child_run(
        &self,
        task: &crate::storage::task_repository::TaskRecord,
        journal: Arc<super::gateway_task_journal::TaskJournal>,
    ) -> Result<Arc<RequestRun>, GatewayError> {
        self.register_journal(&journal)?;
        let background = task.parent_task_id.as_deref().and_then(|id| self.task_runs.lock().ok()
            .and_then(|runs| runs.get(id).and_then(Weak::upgrade)))
            .is_some_and(|parent| parent.is_background_recovery.load(Ordering::Acquire));
        let cancellation_key = new_id();
        let (cancellation, _) = watch::channel(false);
        let (events, _) = broadcast::channel(256);
        let run = Arc::new(RequestRun {
            conversation_id: task.scope.conversation_id.clone(),
            // Child request deduplication keys may be namespaced; formal receipt ownership requires a stable UUID.
            // 子任务查重键可以带命名空间；正式工具回执使用稳定 task UUID，恢复时不换身份，也不伪造主聊天消息。
            request_id: task.task_id.clone(),
            fingerprint: String::new(),
            original_message_raw: None,
            journal: Some(journal),
            parent_task_id: task.parent_task_id.clone(),
            cancellation,
            steering: watch::channel(0).0,
            native_cancellation: crate::service_cancellation::CancellationLease::acquire(
                &cancellation_key,
            )
            .map_err(|code| GatewayError::new(code, "子任务取消状态无法创建。", 503))?,
            cancellation_key,
            events,
            emission: Default::default(),
            replay: Mutex::new(ReplayState { sequence:task.event_sequence, ..Default::default() }),
            finished: AtomicBool::new(false),
            suspending: AtomicBool::new(false),
            result: Default::default(),
            observed_context_limits: Default::default(),
            created: Instant::now(),
            resource_wait_ms: AtomicU64::new(0),
            active_duration_base_ms: task.usage.active_duration_ms.unwrap_or(0),
            is_background_recovery: AtomicBool::new(background),
            recovery_stalled: std::sync::atomic::AtomicBool::new(false),
        });
        let mut runs = self.task_runs.lock().map_err(|_| {
            GatewayError::new("TASK_RUN_LOCK_FAILED", "子任务执行者登记失败。", 503)
        })?;
        runs.retain(|_, run| run.strong_count() > 0);
        runs.insert(task.task_id.clone(), Arc::downgrade(&run));
        Ok(run)
    }
    pub async fn cancel_task_worker(&self, task_id: &str) {
        let run = self.task_runs.lock().ok().and_then(|runs|runs.get(task_id).and_then(Weak::upgrade));
        if let Some(run) = run {
            run.cancel();
            self.retire_terminal_approvals(&run).await;
        }
    }
    /// Interrupt only the current model turn; executing tools and the task lifetime keep their own cancellation.
    /// 只打断当前模型轮次；执行中的工具与整个任务继续使用原取消生命周期。
    pub fn signal_task_steering(&self, task_id: &str) {
        if let Ok(runs) = self.task_runs.lock() {
            if let Some(run) = runs.get(task_id).and_then(Weak::upgrade) {
                run.steering.send_modify(|sequence| *sequence = sequence.saturating_add(1));
            }
        }
    }
    pub async fn wait_task_delivery_boundary(&self, task_id: &str) {
        loop {
            let run = self
                .task_runs
                .lock()
                .ok()
                .and_then(|runs| runs.get(task_id).and_then(Weak::upgrade));
            if run.is_none_or(|run| run.finished.load(Ordering::Acquire)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    pub async fn begin_recovered(
        self: &Arc<Self>,
        input: Value,
        original: Option<String>,
        background: bool,
    ) -> Result<Arc<RequestRun>, GatewayError> {
        self.begin_with_priority(input, original, background).await
    }
    pub async fn tool_context(
        &self,
        conversation: &str,
        permission: &str,
        text: &str,
        cancellation: watch::Receiver<bool>,
    ) -> Result<GatewayToolContext, GatewayError> {
        let root = self.config.data_root.clone();
        let id = conversation.to_owned();
        let relationship = tokio::task::spawn_blocking(move || data::relationship(&root, &id))
            .await
            .map_err(|_| GatewayError::new("DATA_WORKER_FAILED", "工作范围读取失败。", 500))??;
        let folder = relationship["folderPath"]
            .as_str()
            .or_else(|| relationship["workspaceRoot"].as_str())
            .filter(|text| !text.is_empty());
        let workdir = folder
            .map(PathBuf::from)
            .unwrap_or_else(|| self.config.data_root.join("Sandboxes").join(conversation));
        tokio::fs::create_dir_all(&workdir).await?;
        let config = json!({"trustedUserText":text,"officialToolsRoot":self.config.application_root.join("official-tools")});
        let mut context = GatewayToolContext {
            data_home: self.config.data_root.clone(),
            extension_root: self.config.extension_root.clone(),
            workdir: workdir.clone(),
            toolhost_exe: self.config.toolhost_exe.clone(),
            permission_mode: permission.to_owned(),
            conversation_id: conversation.to_owned(),
            project_id: relationship["projectId"].as_str().map(str::to_owned),
            authorized_roots: relationship["mounts"].as_array().into_iter().flatten()
                .filter_map(|mount|mount["path"].as_str().map(PathBuf::from)).chain(std::iter::once(workdir)).collect(),
            cancellation,
            runtime: self.tools.clone(),
            config,
            approved: false,
            events: None,
            external_execute: None,
        };
        let config = self.config.clone();
        let project = context.project_id.clone();
        // Independent preparation shares this request's lifetime; await both before publishing its context.
        // 独立准备共享本次请求生命周期，两项均完成后再发布上下文，不额外创建后台任务。
        let (saved, retrieval) = tokio::join!(
            crate::tools::gateway::get_config(&context),
            tokio::task::spawn_blocking(move || {
                crate::retrieval::gateway::effective(&config, project.as_deref())
            })
        );
        let mut saved = saved?;
        saved["trustedUserText"] = json!(text);
        saved["officialToolsRoot"] = json!(self.config.application_root.join("official-tools"));
        saved["bindingSnapshot"] = crate::storage::workspace_mounts::snapshot(&relationship);
        saved["workspaceMounts"] = relationship["mounts"].clone();
        saved["defaultMountId"] = relationship["defaultMountId"].clone();
        saved["retrieval"] = retrieval
            .map_err(|_| GatewayError::new("RETRIEVAL_WORKER_FAILED", "检索设置读取失败。", 500))??;
        context.config = saved;
        Ok(context)
    }
    /// Live permission belongs to the durable task, never a replaceable request/reply ID.
    /// 活动授权归属持久任务；暂停可复用，终态和取消失效，重启不从历史文字恢复授权。
    fn task_approval_binding(run: &RequestRun, receipt: &Value) -> Option<(String, String)> {
        if run.finished.load(Ordering::Acquire) || *run.cancellation.borrow() { return None; }
        let journal = run.journal.as_ref()?;
        let task = journal.check_dispatch_fence().ok()?;
        if task.state.is_terminal() || task.state == crate::storage::task_repository::TaskState::Paused
            || task.scope.conversation_id != run.conversation_id { return None; }
        let fingerprint = receipt["invocation"]["taskApproval"]["fingerprint"].as_str()?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) { return None; }
        Some((journal.task_id().into(), fingerprint.into()))
    }

    pub(crate) async fn retire_terminal_approvals(&self, run: &RequestRun) {
        let Some(journal) = &run.journal else { return; };
        // Read formal state outside the approval lock; a paused attempt is deliberately retained.
        // 正式状态读取不持审批锁；暂停尝试故意保留授权，只有真实终态才回收。
        if journal.load_current().is_ok_and(|task| task.state.is_terminal()) {
            self.approvals.lock().await.tasks.remove(journal.task_id());
        }
    }

    pub async fn await_approval(
        &self, run: &Arc<RequestRun>, call: &Value, receipt: &Value, round: u64, order: u64, task_deadline: Instant,
    ) -> Result<bool, GatewayError> {
        self.await_approval_until(run, call, receipt, round, order, task_deadline, Instant::now()+Duration::from_secs(300)).await
    }
    async fn await_approval_until(
        &self, run: &Arc<RequestRun>, call: &Value, receipt: &Value, round: u64, order: u64,
        task_deadline: Instant, approval_deadline: Instant,
    ) -> Result<bool, GatewayError> {
        self.await_bound_approval(run, run, call, receipt, round, order, task_deadline, approval_deadline).await
    }
    pub async fn await_child_approval(
        &self, parent: &Arc<RequestRun>, child: &Arc<RequestRun>, call: &Value, receipt: &Value,
        round: u64, order: u64, task_deadline: Instant,
    ) -> Result<bool, GatewayError> {
        self.await_bound_approval(child, parent, call, receipt, round, order, task_deadline, Instant::now()+Duration::from_secs(300)).await
    }
    async fn approval_owner_finished(run: &RequestRun) {
        let mut events=run.events.subscribe();
        loop {
            if run.finished.load(Ordering::Acquire) { return; }
            match events.recv().await {
                Ok(event) if terminal_event(&event) => return,
                Err(broadcast::error::RecvError::Closed) => return,
                _ => {}
            }
        }
    }
    async fn await_bound_approval(
        &self, owner: &Arc<RequestRun>, display: &Arc<RequestRun>, call: &Value, receipt: &Value,
        round: u64, order: u64, task_deadline: Instant, approval_deadline: Instant,
    ) -> Result<bool, GatewayError> {
        let expires = task_deadline.min(approval_deadline);
        let deadline_code = if task_deadline <= approval_deadline { "TOOL_RUN_TIMEOUT" } else { "TOOL_APPROVAL_EXPIRED" };
        if Instant::now() >= expires { return Err(GatewayError::new(deadline_code, "审批或任务有效期已结束，操作未派发。", 408)); }
        if *owner.cancellation.borrow() || *display.cancellation.borrow() || owner.finished.load(Ordering::Acquire) || display.finished.load(Ordering::Acquire) { return Ok(false); }
        self.retire_terminal_approvals(owner).await;
        let binding = Self::task_approval_binding(owner, receipt);
        if let Some((task_id, fingerprint)) = &binding {
            let mut approvals = self.approvals.lock().await;
            if let Some(task) = approvals.tasks.get_mut(task_id) {
                if task.conversation_id == owner.conversation_id && task.bindings.contains(task_id, fingerprint) {
                    task.touched = Instant::now();
                    return Ok(true);
                }
            }
        }
        let id = receipt["invocation"]["id"].as_str().ok_or_else(||GatewayError::new("INVALID_APPROVAL_RECEIPT", "审批回执无效。", 500))?.to_owned();
        let (sender, receiver) = oneshot::channel();
        let mut owner_cancel = owner.cancellation.subscribe();
        let mut display_cancel = display.cancellation.subscribe();
        self.approvals.lock().await.pending.insert(id.clone(), Approval {
            conversation:display.conversation_id.clone(), request:display.request_id.clone(), call:call["id"].as_str().unwrap_or("").into(),
            response:sender, owner:Arc::downgrade(owner), task_binding:binding.clone(), expires, deadline_code,
        });
        let child = !Arc::ptr_eq(owner, display);
        let task_id = owner.journal.as_ref().map(|journal|journal.task_id());
        let description = if binding.is_none() { "当前调用不具备有效任务范围，仅可单次批准。" }
            else if receipt["invocation"]["taskApproval"]["kind"] == "same-file-write" { "本任务内允许写入同一文件；每次仍校验当前文件哈希。暂停后保留，重启后需重新批准。" }
            else { "本任务内允许相同工具、参数与工作范围；更换目标或配置需再次批准。暂停后保留，重启后需重新批准。" };
        display.emit(json!({"type":if child{"task_approval_required"}else{"approval_required"},"taskId":task_id,
            "tool":{"toolCallId":call["id"],"name":call["name"],"arguments":call["arguments"],"status":"approval-required","approvalId":id,
            "summary":if child{"子任务等待授权"}else{"等待授权"},"round":round,"order":order,
            "taskApprovalAvailable":binding.is_some(),"taskApprovalDescription":description}})).await;
        let waiting_started = Instant::now();
        let decision = if *owner_cancel.borrow() || *display_cancel.borrow() || owner.finished.load(Ordering::Acquire) || display.finished.load(Ordering::Acquire) {
            Ok(ApprovalDecision::default())
        } else {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(expires.into()) => Err(GatewayError::new(deadline_code,"审批或任务有效期已结束，操作未派发。",408)),
                _ = crate::tools::gateway::wait_for_cancellation(&mut owner_cancel) => Ok(ApprovalDecision::default()),
                _ = crate::tools::gateway::wait_for_cancellation(&mut display_cancel) => Ok(ApprovalDecision::default()),
                _ = Self::approval_owner_finished(owner) => Ok(ApprovalDecision::default()),
                _ = Self::approval_owner_finished(display) => Ok(ApprovalDecision::default()),
                result = receiver => if Instant::now()>=expires { Err(GatewayError::new(deadline_code,"审批或任务有效期已结束，操作未派发。",408)) }
                    else { Ok(result.unwrap_or_default()) },
            }
        };
        self.approvals.lock().await.pending.remove(&id);
        owner.resource_wait_ms.fetch_add(waiting_started.elapsed().as_millis() as u64, Ordering::AcqRel);
        let cancelled = *owner.cancellation.borrow() || *display.cancellation.borrow() || owner.finished.load(Ordering::Acquire) || display.finished.load(Ordering::Acquire);
        let approved = decision.as_ref().is_ok_and(|decision|decision.approved) && !cancelled;
        let scope = decision.as_ref().map_or("once",|decision|decision.scope.as_str());
        let code = decision.as_ref().err().map(|error|error.code.as_str()).or_else(||if cancelled{Some("TOOL_APPROVAL_CANCELLED")}else if !approved{Some("TOOL_APPROVAL_DENIED")}else{None});
        let closed = json!({"type":"approval_closed","taskId":task_id,"approvalId":id,"toolCallId":call["id"],"approved":approved,"scope":scope,"code":code});
        display.emit(closed.clone()).await;
        if child { let mut compatible=closed;compatible["type"]=json!("task_approval_closed");display.emit(compatible).await; }
        self.retire_terminal_approvals(owner).await;
        decision.map(|_|approved)
    }
    pub async fn approve(&self, body: &Value) -> Result<Value, GatewayError> {
        let id = body["approvalId"].as_str().or_else(||body["id"].as_str()).unwrap_or("");
        let scope = ApprovalScope::parse(body.get("scope").filter(|value|!value.is_null()).map(|value|value.as_str().unwrap_or("invalid")))
            .map_err(|message|GatewayError::new("INVALID_APPROVAL_SCOPE",message,400))?;
        let mut approvals = self.approvals.lock().await;
        let pending = approvals.pending.get(id).ok_or_else(||GatewayError::new("APPROVAL_NOT_FOUND","审批已失效或不存在。",404))?;
        for (key, expected) in [("conversationId",pending.conversation.as_str()),("requestId",pending.request.as_str()),("toolCallId",pending.call.as_str())] {
            if body[key].as_str()!=Some(expected) { return Err(GatewayError::new("APPROVAL_SCOPE_MISMATCH","审批范围不匹配。",409)); }
        }
        if Instant::now()>=pending.expires { let code=pending.deadline_code;approvals.pending.remove(id);return Err(GatewayError::new(code,"审批或任务有效期已结束，操作未派发。",408)); }
        let approved=body["approved"].as_bool().or_else(||body["allow"].as_bool()).ok_or_else(||GatewayError::new("INVALID_APPROVAL","审批决定无效。",400))?;
        if scope==ApprovalScope::Task && pending.task_binding.is_none() { return Err(GatewayError::new("TASK_APPROVAL_UNAVAILABLE","当前调用只能单次批准。",400)); }
        let pending=approvals.pending.remove(id).unwrap();drop(approvals);
        let owner=pending.owner.upgrade().filter(|run|!run.finished.load(Ordering::Acquire)&&!*run.cancellation.borrow())
            .ok_or_else(||GatewayError::new("APPROVAL_NOT_FOUND","任务已暂停、结束或取消，本次审批已关闭。",404))?;
        let mut granted_task=None;
        if approved && scope==ApprovalScope::Task {
            let (task_id,fingerprint)=pending.task_binding.as_ref().unwrap();
            let journal=owner.journal.as_ref().ok_or_else(||GatewayError::new("TASK_APPROVAL_UNAVAILABLE","当前调用没有持久任务身份。",409))?;
            let current=journal.check_dispatch_fence()?;
            if current.state.is_terminal() || current.state==crate::storage::task_repository::TaskState::Paused || journal.task_id()!=task_id {
                return Err(GatewayError::new("APPROVAL_NOT_FOUND","任务授权范围已失效。",409));
            }
            let mut approvals=self.approvals.lock().await;
            if approvals.tasks.len()>=256 && !approvals.tasks.contains_key(task_id) {
                if let Some(old)=approvals.tasks.iter().min_by_key(|(_,task)|task.touched).map(|(id,_)|id.clone()) { approvals.tasks.remove(&old); }
            }
            let task=approvals.tasks.entry(task_id.clone()).or_insert_with(||LiveTaskApproval{conversation_id:owner.conversation_id.clone(),bindings:Default::default(),touched:Instant::now()});
            task.bindings.allow(task_id,fingerprint);task.touched=Instant::now();granted_task=Some(task_id.clone());
        }
        let _=pending.response.send(ApprovalDecision{approved,scope});
        Ok(json!({"approved":approved,"approvalId":id,"toolCallId":pending.call,"scope":scope.as_str(),"taskId":granted_task}))
    }
    pub async fn active_requests(&self) -> usize {
        let unfinished = self
            .runs
            .lock()
            .await
            .values()
            .filter(|run| !run.finished.load(Ordering::Acquire))
            .count();
        unfinished
            .max(self.execution_tasks.load(Ordering::Acquire))
            .saturating_add(self.tasks.active_workers().unwrap_or(usize::MAX))
    }

    /// The router must first close its maintenance gate; no new request may enter while roots drain.
    /// 路由必须先关闭维护入口；根目录结算期间不得进入新请求。
    pub async fn quiesce_storage(&self) -> Result<(), GatewayError> {
        if self.active_requests().await != 0 {
            return Err(GatewayError::new(
                "STORAGE_MIGRATION_BUSY",
                "仍有活动任务，存储尚未就绪。",
                409,
            ));
        }
        self.tasks.quiesce().await?;
        self.background_cancellation.send_replace(true);
        self.model_preparation.shutdown().await;
        crate::models::gateway_local_models::shutdown(&self.config.data_home).await?;
        crate::models::gateway_local_engine::shutdown_for(&self.config.data_home).await;
        // Released attempts may still own a batched writer; drain it before moving the root.
        // 已释放的尝试可能仍拥有批量写入者；移动根目录前等待其实际退场。
        let journals = self
            .task_journals
            .lock()
            .map_err(|_| {
                GatewayError::new(
                    "TASK_JOURNAL_REGISTRY_FAILED",
                    "任务日志生命周期状态不可用。",
                    503,
                )
            })?
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for journal in journals {
            journal.drain_background().await?;
        }
        let (_owner, cancellation) = watch::channel(false);
        let context = GatewayToolContext {
            data_home: self.config.data_root.clone(),
            extension_root: self.config.extension_root.clone(),
            workdir: self.config.data_root.join("Sandboxes"),
            toolhost_exe: self.config.toolhost_exe.clone(),
            permission_mode: "ask".into(),
            conversation_id: String::new(),
            project_id: None,
            authorized_roots: vec![],
            cancellation,
            runtime: self.tools.clone(),
            config: json!({}),
            approved: false,
            events: None,
            external_execute: None,
        };
        crate::tools::gateway::shutdown_checked(&context).await?;
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || crate::retrieval::gateway::drain_background(&config))
            .await
            .map_err(|_| {
                GatewayError::new("RETRIEVAL_WORKER_FAILED", "检索后台结算线程退出。", 503)
            })??;
        let retired = tokio::task::spawn_blocking(crate::models::gateway_inference::shutdown_idle)
            .await
            .map_err(|_| {
                GatewayError::new("INFERENCE_CLEANUP_FAILED", "推理运行时结算线程退出。", 503)
            })??;
        if !retired {
            return Err(GatewayError::new(
                "INFERENCE_CLEANUP_PENDING",
                "仍有推理资产在使用，存储尚未就绪。",
                409,
            ));
        }
        let root = self.config.data_root.clone();
        tokio::task::spawn_blocking(move || crate::storage::sqlite_connections::drain_root(&root))
            .await
            .map_err(|_| {
                GatewayError::new("STORAGE_CLEANUP_FAILED", "数据库结算线程退出。", 503)
            })??;
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.background_cancellation.send_replace(true);
        for run in self.runs.lock().await.values() {
            if !run.finished.load(Ordering::Acquire) {
                run.suspending.store(true, Ordering::Release);
                run.cancel();
            }
        }
        self.tasks.shutdown().await;
        self.model_preparation.shutdown().await;
        let _ = crate::models::gateway_local_models::shutdown(&self.config.data_home).await;
        crate::models::gateway_local_engine::shutdown().await;
        let (_owner, cancellation) = watch::channel(false);
        let context = GatewayToolContext {
            data_home: self.config.data_root.clone(),
            extension_root: self.config.extension_root.clone(),
            workdir: self.config.data_root.join("Sandboxes"),
            toolhost_exe: self.config.toolhost_exe.clone(),
            permission_mode: "ask".into(),
            conversation_id: String::new(),
            project_id: None,
            authorized_roots: vec![],
            cancellation,
            runtime: self.tools.clone(),
            config: json!({}),
            approved: false,
            events: None,
            external_execute: None,
        };
        crate::tools::gateway::shutdown(&context).await;
        let config = self.config.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::retrieval::gateway::stop_background(&config)
        })
        .await;
        let _ = tokio::task::spawn_blocking(crate::models::gateway_inference::shutdown_idle).await;
        if let Some(service) = crate::resources::gateway::global() {
            service.close().await;
        }
    }
}

pub fn checked_id(value: &Value, field: &str) -> Result<String, GatewayError> {
    value
        .as_str()
        .and_then(|text| uuid::Uuid::parse_str(text).ok())
        .map(|id| id.to_string())
        .ok_or_else(|| GatewayError::new("INVALID_CHAT_REQUEST", format!("{field} 无效。"), 400))
}

#[cfg(test)]
mod generation_timing_tests {
    use super::*;

    #[test]
    fn generation_timing_requires_a_real_start_and_preserves_restored_duration() {
        assert_eq!(generation_elapsed_ms(None, None), None);
        assert_eq!(generation_elapsed_ms(Some(12_345), None), Some(12_345));
        let started = Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
        assert!(generation_elapsed_ms(Some(12_345), Some(started)).unwrap() >= 14_345);
        assert_eq!(generation_elapsed_ms(Some(u64::MAX), Some(started)), Some(MAXIMUM_SAFE_ELAPSED_MS));
    }

    #[tokio::test]
    async fn generation_timing_survives_reply_anchor_and_replays_without_visible_segments() {
        let run = RequestRun::test_run(8);
        run.emit(json!({"type":"started","status":"queued"})).await;
        let before = run.recovery(0).await.unwrap();
        assert!(before.events.iter().all(|event| event["generationElapsedMs"].is_null()));

        run.emit(json!({"type":"generation_timing","generationElapsedMs":12_000,
            "generationActive":true})).await;
        let reply = new_id();
        run.emit(json!({"type":"reply_anchor","replyMessageId":reply,
            "userMessageId":new_id()})).await;
        let recovered = run.recovery(0).await.unwrap();
        assert!(!recovered.events.iter().any(|event| event["type"] == "assistant_segment"));
        for kind in ["reply_anchor", "content_snapshot"] {
            let event = recovered.events.iter().find(|event| event["type"] == kind).unwrap();
            assert_eq!(event["replyMessageId"], reply);
            assert!(event["generationElapsedMs"].as_u64().unwrap() >= 12_000);
            assert_eq!(event["generationActive"], true);
        }
        let caught_up = run.recovery(recovered.through).await.unwrap();
        let timing = caught_up.events.iter().find(|event| event["type"] == "generation_timing").unwrap();
        assert!(timing["createdAt"].as_str().is_some_and(|value| !value.is_empty()));
        assert!(timing["generationElapsedMs"].as_u64().unwrap() >= 12_000);
        assert_eq!(timing["generationActive"], true);
    }

    #[tokio::test]
    async fn generation_timing_freezes_and_restores_without_counting_offline_time() {
        let run = RequestRun::test_run(8);
        assert_eq!(run.restore_generation_timing(Some(21_000)).await, Some(21_000));
        assert_eq!(run.replay.lock().await.generation.elapsed(), Some(21_000));
        assert!(run.replay.lock().await.generation.started.is_none());
        run.emit(json!({"type":"generation_timing","generationElapsedMs":21_000,
            "generationActive":true})).await;
        run.freeze_generation_timing(Some(22_000)).await;
        assert_eq!(run.replay.lock().await.generation.elapsed(), Some(22_000));
        assert_eq!(run.restore_generation_timing(Some(20_000)).await, Some(22_000));
        run.finish(json!({"type":"completed","durationMs":22_500})).await;
        let recovered = run.recovery(0).await.unwrap();
        let snapshot = recovered.events.iter().find(|event| event["type"] == "content_snapshot").unwrap();
        assert_eq!(snapshot["generationElapsedMs"], 22_500);
        assert_eq!(snapshot["generationActive"], false);
        assert_eq!(run.replay.lock().await.generation.elapsed(), Some(22_500));
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;

    #[test]
    fn model_recovery_retains_narrowed_limits_only_for_the_original_connection() {
        let run = RequestRun::test_run(4);
        run.retain_model_limits("origin-a", &json!({"contextWindowTokens":8192,"estimatedInputBudgetTokens":4800}));
        run.retain_model_limits("origin-a", &json!({"contextWindowTokens":16384,"estimatedInputBudgetTokens":3600}));
        assert_eq!(run.observed_model_limits("origin-a"), json!({"contextWindowTokens":8192,"estimatedInputBudgetTokens":3600}));
        assert!(run.observed_model_limits("origin-b").is_null());
        run.retain_model_limits("origin-b", &json!({"contextWindowTokens":32768}));
        assert_eq!(run.observed_model_limits("origin-b"), json!({"contextWindowTokens":32768}));
        assert!(run.observed_model_limits("origin-a").is_null());
    }

    fn runtime() -> (Arc<GatewayRuntime>, PathBuf) {
        let root = std::env::temp_dir().join(format!("kynxa-approval-{}", new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let runtime = GatewayRuntime::new(GatewayConfig {
            data_home: root.join("models"),
            data_root: root.join("data"),
            extension_root: root.join("extensions"),
            application_root: root.join("app"),
            port: 0,
            toolhost_exe: root.join("missing.exe"),
            vector_extension: root.join("missing-vector.dll"),
            inference_library: root.join("missing-inference.dll"),
        });
        (runtime, root)
    }

    fn durable_run(runtime: &GatewayRuntime) -> Arc<RequestRun> {
        use crate::storage::task_repository::{NewTask, TaskBudget, TaskScope};
        let mut run = RequestRun::test_run(16);
        let journal = super::super::gateway_task_journal::TaskJournal::begin(
            &runtime.config.data_root,
            &NewTask {
                request_id: run.request_id.clone(),
                goal: "Read-only stream recovery fixture".into(),
                acceptance: vec![],
                scope: TaskScope {
                    project_id: None,
                    conversation_id: run.conversation_id.clone(),
                    permission_snapshot: json!({"permissionMode":"full"}),
                },
                parent_task_id: None,
                dependency_ids: vec![],
                budget: TaskBudget::default(),
                input_reference: None,
            },
            "stream-fixture",
            60_000,
        )
        .unwrap();
        Arc::get_mut(&mut run).unwrap().journal = Some(journal);
        run
    }

    #[tokio::test]
    async fn durable_stream_publication_preserves_large_unicode_deltas_and_owned_recovery_suffix() {
        use crate::storage::task_repository::TaskState;
        let (runtime, root) = runtime();
        let run = durable_run(&runtime);
        let journal = run.journal.as_ref().unwrap();
        let reply = new_id(); let segment = new_id();
        let mut wire = run.events.subscribe();
        run.emit(json!({"type":"reply_anchor","replyMessageId":reply,"userMessageId":new_id()})).await;
        run.emit(json!({"type":"assistant_segment","segment":{"id":segment,"phase":"commentary","status":"streaming","round":1,"order":1,"content":"","reasoning":""}})).await;
        let text = "金色 🏰\n\0".repeat(4000);
        run.emit(json!({"type":"text_delta","segmentId":segment,"delta":text})).await;
        run.emit(json!({"type":"reasoning_delta","segmentId":segment,"delta":"saved visible reasoning"})).await;
        let committed = journal.repository().read_events(journal.task_id(),0,512).unwrap();
        let mut published_text = String::new(); let mut first_delta = None;
        while let Ok(event) = wire.try_recv() {
            if event["type"] == "text_delta" {
                first_delta.get_or_insert(event["eventSequence"].as_u64().unwrap());
                published_text.push_str(event["delta"].as_str().unwrap());
                let saved = committed.iter().find(|saved|saved.payload["eventSequence"] == event["eventSequence"]
                    && saved.payload["requestId"] == run.request_id).expect("broadcast requires committed original observation");
                assert_eq!(saved.payload["delta"],event["delta"]);
            }
        }
        assert_eq!(published_text,text);
        let full = journal.repository().stream_projection(journal.task_id(),&run.request_id,&reply,0).unwrap();
        assert_eq!(full["complete"],true); assert_eq!(full["content"],text);
        assert_eq!(full["reasoning"],"saved visible reasoning");
        let suffix = journal.repository().stream_projection(journal.task_id(),&run.request_id,&reply,first_delta.unwrap()).unwrap();
        assert_eq!(suffix["baseContent"].as_str().unwrap().to_owned()+suffix["content"].as_str().unwrap(),text);
        assert!(journal.repository().stream_projection(journal.task_id(),&new_id(),&reply,0).is_err());
        let connection = crate::storage::sqlite_connections::acquire(&runtime.config.data_root,
            &runtime.config.data_root.join("Agent/tasks.sqlite"),30_000).unwrap();
        let compressed: i64 = connection.query_row("SELECT count(*) FROM task_events WHERE task_id=? AND json_extract(document,'$.streamEncoding')='zlib-task-event-v1'",
            [journal.task_id()],|row|row.get(0)).unwrap();
        assert!(compressed>0,"real ledger stores lossless compact stream records");
        drop(connection);
        assert_eq!(journal.load_current().unwrap().usage.model_requests,0);
        assert!(journal.repository().operations(journal.task_id()).unwrap().is_empty());
        journal.finish(TaskState::Partial,Some("fixture-complete")).await.unwrap();
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(run); drop(runtime); std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn remote_task_checkpoints_are_durable_private_evidence_without_public_stream_events() {
        use crate::storage::task_repository::TaskState;
        let (runtime, root) = runtime();
        let run = durable_run(&runtime);
        run.emit(json!({"type":"mcp_task_checkpoint","operationId":"round-1:remote-write","toolCallId":"remote-write",
            "recoveryHandle":{"kind":"mcp-task","taskId":"remote-owned-id"}})).await;
        let replay = run.recovery(0).await.unwrap();
        assert_eq!(replay.through, 0);
        assert!(replay.events.iter().all(|event| event["type"] != "mcp_task_checkpoint"));
        let journal = run.journal.as_ref().unwrap();
        let checkpoints = journal.repository().recovery_receipts(journal.task_id()).unwrap();
        assert!(checkpoints.iter().any(|event| event.kind == "mcp_task_checkpoint"
            && event.payload["operationId"] == "round-1:remote-write"
            && event.payload["requestId"] == run.request_id));
        journal.finish(TaskState::Partial, Some("fixture-complete")).await.unwrap();
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(run);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn pending_terminal_persistence_keeps_replay_watermark_unpublished() {
        use crate::storage::task_repository::TaskState;
        let (runtime, root) = runtime();
        let run = durable_run(&runtime);
        let blocker = rusqlite::Connection::open(runtime.config.data_root.join("Agent/tasks.sqlite")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let operation = run.clone();
        let finishing = tokio::spawn(async move {
            operation.finish(json!({"type":"completed","content":"durable final","reasoning":""})).await;
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while run.emission.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let replay = tokio::time::timeout(Duration::from_millis(100), run.recovery(0)).await;
        let prematurely_finished = run.finished.load(Ordering::Acquire);
        blocker.execute_batch("ROLLBACK").unwrap();
        drop(blocker);
        tokio::time::timeout(Duration::from_secs(5), finishing).await.unwrap().unwrap();
        let replay = replay.unwrap().unwrap();
        assert_eq!(replay.through, 0);
        assert!(replay.events.is_empty());
        assert!(!prematurely_finished);
        assert!(run.finished.load(Ordering::Acquire));
        run.journal.as_ref().unwrap().finish(TaskState::Completed, Some("fixture-complete")).await.unwrap();
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(run);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn released_journal_finalization_is_idempotent_and_preserves_actual_outcome() {
        use crate::storage::task_repository::TaskState;
        let (runtime, root) = runtime();
        let run = durable_run(&runtime);
        let journal = run.journal.as_ref().unwrap();
        let first = journal.finish(TaskState::Completed, Some("fixture-complete")).await.unwrap();
        assert!(journal.is_released());
        let repeated = journal.finish(TaskState::Completed, Some("fixture-complete")).await.unwrap();
        assert_eq!(first.version, repeated.version);
        assert_eq!(first.execution_version, repeated.execution_version);
        assert_eq!(journal.finish(TaskState::WaitingVerification, Some("different-outcome")).await.unwrap_err().code,
            "TASK_FINAL_STATE_CONFLICT");
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(run);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn recovery_snapshot_remains_the_base_of_later_reconnect_deltas() {
        let run = RequestRun::test_run(16);
        run.emit(json!({"type":"text_delta","delta":"superseded content"})).await;
        run.emit(json!({"type":"reply_anchor","replyMessageId":new_id(),"userMessageId":new_id()})).await;
        run.emit(json!({"type":"content_snapshot","content":"preserved progress","reasoning":"preserved reasoning"})).await;
        run.emit(json!({"type":"text_delta","delta":" and continuation"})).await;
        let replay = run.recovery(0).await.unwrap();
        let snapshot = replay.events.iter().find(|event| event["type"] == "content_snapshot").unwrap();
        assert_eq!(snapshot["content"], "preserved progress and continuation");
        assert_eq!(snapshot["reasoning"], "preserved reasoning");
        assert!(!*run.cancellation.borrow());
        assert!(!run.finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn event_backpressure_flushes_original_controls_without_cancelling_task() {
        use crate::storage::task_repository::TaskState;
        let (runtime, root) = runtime();
        let run = durable_run(&runtime);
        let journal = run.journal.as_ref().unwrap();
        let mut queued = 0;
        loop {
            match journal.emit_control("fixture.pending", &json!({"index":queued,"padding":"x".repeat(16 * 1024)})) {
                Ok(()) => queued += 1,
                Err(error) => {
                    assert_eq!(error.code, "TASK_EVENT_BACKPRESSURE");
                    break;
                }
            }
        }
        assert!(queued > 0);
        tokio::time::timeout(
            Duration::from_secs(5),
            run.emit(json!({"type":"task_progress","phase":"working","summary":"original request continues"})),
        )
        .await
        .unwrap();
        assert!(!*run.cancellation.borrow());
        assert!(!run.finished.load(Ordering::Acquire));
        journal.flush().await.unwrap();
        let mut after = 0;
        let mut progress = 0;
        let mut saved_pending = 0;
        loop {
            let events = journal.repository().read_events(journal.task_id(), after, 512).unwrap();
            if events.is_empty() { break; }
            after = events.last().unwrap().sequence;
            progress += events.iter().filter(|event| event.kind == "task_progress").count();
            saved_pending += events.iter().filter(|event| event.kind == "fixture.pending").count();
        }
        assert_eq!(progress, 1);
        assert_eq!(saved_pending, queued);
        journal.finish(TaskState::Completed, Some("fixture-complete")).await.unwrap();
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(run);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_only_task_reconnect_uses_metadata_without_scanning_auxiliary_history() {
        let (runtime, root) = runtime();
        let run = durable_run(&runtime);
        let journal = run.journal.as_ref().unwrap();
        let task_id = journal.task_id().to_owned();
        runtime.runs.lock().await.insert(run.request_id.clone(), run.clone());
        data::ensure_conversation(&runtime.config.data_root, &run.conversation_id, "Reconnect fixture").unwrap();
        runtime.tasks.metadata(&task_id).await.unwrap();
        // An unreadable diagnostic artifact must not enter the live stream's permission/attempt check.
        // 无法读取的诊断成果不应参与活动回复的范围与执行者核验；原记录仍保留供独立诊断。
        let connection = rusqlite::Connection::open(runtime.config.data_root.join("Agent/tasks.sqlite")).unwrap();
        connection.execute(
            "INSERT INTO task_artifacts(task_id,artifact_id,fingerprint,document) VALUES(?,?,?,?)",
            rusqlite::params![&task_id, new_id(), "fixture", "{}"],
        ).unwrap();
        drop(connection);
        assert!(runtime.tasks.detail(&task_id).await.is_err());
        let attached = tokio::time::timeout(Duration::from_secs(5), runtime.observe_task(&task_id, &run.conversation_id))
            .await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&run, &attached));
        assert_eq!(runtime.observe_task(&task_id, &new_id()).await.err().unwrap().code, "TASK_SCOPE_DENIED");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = crate::gateway::router(runtime.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let response = tokio::time::timeout(Duration::from_secs(5), reqwest::Client::new()
            .get(format!("http://{address}/api/tasks/{task_id}/stream?conversationId={}", run.conversation_id))
            .send()).await.unwrap().unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        drop(response);
        assert_eq!(runtime.runs.lock().await.len(), 1);
        assert_eq!(journal.load_current().unwrap().usage.model_requests, 0);
        assert!(!*run.cancellation.borrow());
        server.abort();
        journal.finish(TaskState::Completed, Some("fixture-complete")).await.unwrap();
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(attached);
        drop(run);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_only_chat_reconnect_retains_original_run_scope_anchor_and_task() {
        use crate::storage::task_repository::TaskState;
        let (runtime, root) = runtime();
        let run = durable_run(&runtime);
        runtime.runs.lock().await.insert(run.request_id.clone(), run.clone());
        let reply = new_id();
        let user_message = new_id();
        run.emit(json!({"type":"started"})).await;
        run.emit(json!({"type":"reply_anchor","replyMessageId":reply,"userMessageId":user_message,"supersededReplyMessageId":run.request_id})).await;
        run.emit(json!({"type":"text_delta","segmentId":"fixture-segment","delta":"saved progress"})).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = crate::gateway::router(runtime.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let client = reqwest::Client::new();
        let query = format!("conversationId={}&requestId={}", run.conversation_id, run.request_id);
        let status: Value = client.get(format!("http://{address}/api/chat/status?{query}"))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(status["status"], "running");
        assert_eq!(status["replyAnchor"]["replyMessageId"], reply);
        assert_eq!(status["replyAnchor"]["userMessageId"], user_message);
        let stream = client.get(format!("http://{address}/api/chat/stream?{query}"))
            .send().await.unwrap().error_for_status().unwrap();
        assert_eq!(stream.headers()["content-type"], "text/event-stream");
        drop(stream);
        tokio::task::yield_now().await;
        assert!(!*run.cancellation.borrow());
        assert!(!run.finished.load(Ordering::Acquire));
        for endpoint in ["status", "stream"] {
            let response = client.get(format!("http://{address}/api/chat/{endpoint}?conversationId={}&requestId={}", new_id(), run.request_id))
                .send().await.unwrap();
            assert_eq!(response.status().as_u16(), 404);
        }
        run.finish(json!({"type":"completed","content":"saved progress","reasoning":"","replyMessageId":reply,"taskCompletion":{"completionState":"complete"}})).await;
        let body = client.get(format!("http://{address}/api/chat/stream?{query}"))
            .send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
        assert!(body.contains("content_snapshot"));
        assert!(body.contains("saved progress"));
        let status: Value = client.get(format!("http://{address}/api/chat/status?{query}"))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(status["status"], "completed");
        assert_eq!(status["result"]["replyMessageId"], reply);
        assert!(Arc::ptr_eq(&run, &runtime.observe_request(&run.conversation_id, &run.request_id).await.unwrap()));
        assert_eq!(runtime.runs.lock().await.len(), 1);
        server.abort();
        run.journal.as_ref().unwrap().finish(TaskState::Completed, Some("fixture-complete")).await.unwrap();
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(run);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn task_scope_authorization_survives_pause_and_new_request_but_not_a_different_task_or_terminal_state() {
        use crate::storage::task_repository::{TaskControl,TaskState};
        let (runtime,root)=runtime();let run=durable_run(&runtime);let mut events=run.events.subscribe();
        let receipt=json!({"invocation":{"id":new_id(),"taskApproval":{"fingerprint":"f".repeat(64),"kind":"same-file-write"}}});
        let call=json!({"id":"scoped-call","name":"filesystem.write","arguments":{"path":"report.md"}});
        let waiting={let runtime=runtime.clone();let run=run.clone();let receipt=receipt.clone();let call=call.clone();
            tokio::spawn(async move{runtime.await_approval(&run,&call,&receipt,1,1,Instant::now()+Duration::from_secs(10)).await})};
        let required=loop { let event=events.recv().await.unwrap();if event["type"]=="approval_required" {break event;} };
        assert_eq!(required["tool"]["taskApprovalAvailable"],true);
        let response=runtime.approve(&json!({"approvalId":receipt["invocation"]["id"],"conversationId":run.conversation_id,"requestId":run.request_id,
            "toolCallId":"scoped-call","approved":true,"scope":"task"})).await.unwrap();
        let task_id=run.journal.as_ref().unwrap().task_id().to_owned();assert_eq!(response["taskId"],task_id);assert_eq!(response["scope"],"task");
        assert!(waiting.await.unwrap().unwrap());
        let closed=loop {let event=events.recv().await.unwrap();if event["type"]=="approval_closed" {break event;} };
        assert_eq!(closed["approvalId"],receipt["invocation"]["id"]);assert_eq!(closed["scope"],"task");assert_eq!(closed["approved"],true);
        assert_eq!(runtime.approve(&json!({"approvalId":receipt["invocation"]["id"],"conversationId":run.conversation_id,"requestId":run.request_id,
            "toolCallId":"scoped-call","approved":true,"scope":"task"})).await.unwrap_err().code,"APPROVAL_NOT_FOUND");
        assert!(runtime.await_approval(&run,&call,&receipt,2,1,Instant::now()+Duration::from_secs(1)).await.unwrap());
        let foreign=durable_run(&runtime);
        let foreign_binding=GatewayRuntime::task_approval_binding(&foreign,&receipt).unwrap();
        assert!(!runtime.approvals.lock().await.tasks[&task_id].bindings.contains(&foreign_binding.0,&foreign_binding.1));
        foreign.journal.as_ref().unwrap().finish(TaskState::Completed,Some("fixture-complete")).await.unwrap();
        let journal=run.journal.as_ref().unwrap();
        journal.flush().await.unwrap();
        let current=journal.load_current().unwrap();
        journal.repository().control(&task_id,current.version,TaskControl::Pause).unwrap();
        journal.finish(TaskState::Paused,Some("fixture-pause")).await.unwrap();
        run.cancel();runtime.retire_terminal_approvals(&run).await;assert!(runtime.approvals.lock().await.tasks.contains_key(&task_id));
        let paused=journal.load_current().unwrap();assert_eq!(paused.state,TaskState::Paused);
        let queued=journal.repository().control(&task_id,paused.version,TaskControl::Resume).unwrap();
        let next=super::super::gateway_task_journal::TaskJournal::start_existing(&runtime.config.data_root,&queued,"approval-resume",60_000).unwrap();
        let mut resumed=RequestRun::test_run(16);{let value=Arc::get_mut(&mut resumed).unwrap();value.conversation_id=run.conversation_id.clone();value.journal=Some(next);}
        assert_ne!(resumed.request_id,run.request_id);
        assert!(runtime.await_approval(&resumed,&call,&receipt,3,1,Instant::now()+Duration::from_secs(1)).await.unwrap());
        resumed.journal.as_ref().unwrap().finish(TaskState::Completed,Some("fixture-complete")).await.unwrap();
        runtime.retire_terminal_approvals(&resumed).await;assert!(!runtime.approvals.lock().await.tasks.contains_key(&task_id));
        assert!(GatewayRuntime::task_approval_binding(&resumed,&receipt).is_none());
        crate::storage::sqlite_connections::drain_root(&runtime.config.data_root).unwrap();
        drop(events);drop(foreign);drop(resumed);drop(run);drop(runtime);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cancelled_inline_approval_emits_close_without_finishing_the_reply_or_dispatching() {
        let (runtime,root)=runtime();let run=RequestRun::test_run(16);let mut events=run.events.subscribe();
        let receipt=json!({"invocation":{"id":new_id()}});let call=json!({"id":"cancel-call","name":"terminal.host.run","arguments":{}});
        let waiting={let runtime=runtime.clone();let run=run.clone();let receipt=receipt.clone();
            tokio::spawn(async move{runtime.await_approval(&run,&call,&receipt,1,1,Instant::now()+Duration::from_secs(10)).await})};
        loop {if events.recv().await.unwrap()["type"]=="approval_required" {break;}}
        run.cancel();assert!(!waiting.await.unwrap().unwrap());
        let closed=loop{let event=events.recv().await.unwrap();if event["type"]=="approval_closed"{break event;}};
        assert_eq!(closed["approvalId"],receipt["invocation"]["id"]);assert_eq!(closed["toolCallId"],"cancel-call");assert_eq!(closed["approved"],false);
        assert_eq!(closed["code"],"TOOL_APPROVAL_CANCELLED");assert!(!run.finished.load(Ordering::Acquire));
        assert!(runtime.approvals.lock().await.pending.is_empty());std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn formal_approval_expires_and_task_deadline_interrupts_waiting() {
        let (runtime, root) = runtime();
        let run = RequestRun::test_run(16);
        let call = json!({"id":"owned-call","name":"skill.run","arguments":{}});
        for (approval_ms, task_ms, code) in [
            (20, 2000, "TOOL_APPROVAL_EXPIRED"),
            (20_000, 20, "TOOL_RUN_TIMEOUT"),
        ] {
            let receipt = json!({"invocation":{"id":new_id()}});
            let error = runtime
                .await_approval_until(
                    &run,
                    &call,
                    &receipt,
                    1,
                    1,
                    Instant::now() + Duration::from_millis(task_ms),
                    Instant::now() + Duration::from_millis(approval_ms),
                )
                .await
                .unwrap_err();
            assert_eq!(error.code, code);
            assert!(runtime.approvals.lock().await.pending.is_empty());
            assert_eq!(runtime.approve(&json!({"approvalId":receipt["invocation"]["id"],"conversationId":run.conversation_id,"requestId":run.request_id,"toolCallId":"owned-call","approved":true})).await.unwrap_err().code, "APPROVAL_NOT_FOUND");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn approval_submission_checks_expiry_and_scope_and_accepts_live_decision() {
        let (runtime, root) = runtime();
        let run = RequestRun::test_run(16);
        let (sender, _receiver) = oneshot::channel();
        runtime.approvals.lock().await.pending.insert(
            "expired".into(),
            Approval {
                conversation: run.conversation_id.clone(),
                request: run.request_id.clone(),
                call: "call".into(),
                response: sender,
                owner: Arc::downgrade(&run),
                task_binding: None,
                expires: Instant::now() - Duration::from_millis(1),
                deadline_code: "TOOL_APPROVAL_EXPIRED",
            },
        );
        let decision = json!({"approvalId":"expired","conversationId":run.conversation_id,"requestId":run.request_id,"toolCallId":"call","approved":true});
        assert_eq!(
            runtime.approve(&decision).await.unwrap_err().code,
            "TOOL_APPROVAL_EXPIRED"
        );
        assert!(runtime.approvals.lock().await.pending.is_empty());
        let receipt = json!({"invocation":{"id":new_id()}});
        let call = json!({"id":"call","name":"skill.run","arguments":{}});
        let waiting = {
            let runtime = runtime.clone();
            let run = run.clone();
            let receipt = receipt.clone();
            tokio::spawn(async move {
                runtime
                    .await_approval(
                        &run,
                        &call,
                        &receipt,
                        1,
                        1,
                        Instant::now() + Duration::from_secs(2),
                    )
                    .await
            })
        };
        while runtime.approvals.lock().await.pending.is_empty() {
            tokio::task::yield_now().await;
        }
        let mut live = decision;
        live["approvalId"] = receipt["invocation"]["id"].clone();
        live["requestId"] = json!(new_id());
        assert_eq!(
            runtime.approve(&live).await.unwrap_err().code,
            "APPROVAL_SCOPE_MISMATCH"
        );
        live["requestId"] = json!(run.request_id);
        assert_eq!(runtime.approve(&live).await.unwrap()["approved"], true);
        assert!(waiting.await.unwrap().unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn child_approval_also_obeys_task_deadline() {
        let (runtime, root) = runtime();
        let parent = RequestRun::test_run(16);
        let child = RequestRun::test_run(16);
        let call = json!({"id":"child-call","name":"skill.run","arguments":{}});
        let receipt = json!({"invocation":{"id":new_id()}});
        let error = runtime
            .await_child_approval(
                &parent,
                &child,
                &call,
                &receipt,
                1,
                1,
                Instant::now() + Duration::from_millis(20),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "TOOL_RUN_TIMEOUT");
        assert!(runtime.approvals.lock().await.pending.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
