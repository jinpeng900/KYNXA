//! Native request lifecycle and invocation-bound approval; no second storage or tool ledger.
//! 原生请求生命周期与绑定调用的审批；不另建正式存储或工具账本。

use crate::{
    gateway_contract::{GatewayConfig, GatewayError, new_id, now_iso8601},
    models::gateway_config::ModelConfigStore,
    models::gateway_preparation::PreparationService,
    storage::gateway as data,
    tools::gateway::{GatewayToolContext, ToolRuntime},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, broadcast, oneshot, watch};

const MAXIMUM_REPLAY_CONTROLS: usize = 4096;
const MAXIMUM_REPLAY_BYTES: usize = 128 * 1024 * 1024;

#[derive(Default)]
struct ReplayState {
    sequence: u64,
    controls: VecDeque<Value>,
    bytes: usize,
    content: String,
    reasoning: String,
    active_segment: Option<Value>,
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
    pub native_cancellation: crate::service_cancellation::CancellationLease,
    pub cancellation_key: String,
    pub events: broadcast::Sender<Value>,
    replay: Mutex<ReplayState>,
    pub finished: AtomicBool,
    pub suspending: AtomicBool,
    pub result: Mutex<Option<Value>>,
    pub created: Instant,
}
impl RequestRun {
    pub fn cancel(&self) {
        self.native_cancellation
            .token
            .store(true, Ordering::Release);
        self.cancellation.send_replace(true);
    }
    pub async fn emit(&self, mut event: Value) {
        let mut replay = self.replay.lock().await;
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        replay.sequence += 1;
        event["conversationId"] = json!(self.conversation_id);
        event["requestId"] = json!(self.request_id);
        event["createdAt"] = json!(now_iso8601());
        event["eventSequence"] = json!(replay.sequence);
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
        // Persist compact deltas and formal references; never re-encode the growing full reply per token.
        // 持久化小增量和正式引用；每个 token 不重复编码不断增长的完整正文。
        if let Some(journal) = &self.journal {
            if event["taskId"].is_null() {
                event["taskId"] = json!(journal.task_id());
            }
            event["rootRequestTaskId"] = json!(journal.task_id());
            let durable = if terminal_event(&event) {
                json!({"type":event["type"],"code":event["code"],"conversationId":self.conversation_id,"requestId":self.request_id,"assistantMessageId":self.request_id,"taskCompletion":event["taskCompletion"]})
            } else if event["type"] == "assistant_segment" {
                json!({"type":"assistant_segment","segmentId":event["segment"]["id"],"phase":event["segment"]["phase"],"status":event["segment"]["status"]})
            } else if event.to_string().len() <= 24 * 1024 {
                event.clone()
            } else {
                json!({"type":event["type"],"conversationId":self.conversation_id,"requestId":self.request_id,"toolCallId":event["tool"]["toolCallId"],"resultRef":event["tool"]["resultRef"],"archived":true})
            };
            if !journal.is_released() {
                if let Err(error) =
                    journal.emit_control(event["type"].as_str().unwrap_or("progress"), &durable)
                {
                    // An unavailable durable journal stops future dispatch rather than silently accepting untracked effects.
                    // 日志不可写时停止后续派发，不静默接受无法追踪的副作用。
                    self.cancel();
                    event["persistenceError"] = json!({"code":error.code,"message":error.message});
                }
                if terminal_event(&event) {
                    if let Err(error) = journal.flush().await {
                        event["persistenceError"] =
                            json!({"code":error.code,"message":error.message});
                    }
                }
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
            return Ok(RecoveryBatch {
                events,
                through: replay.sequence,
            });
        }
        let terminal = events.back().filter(|event| terminal_event(event)).cloned();
        if terminal.is_some() {
            events.pop_back();
        }
        if !self.finished.load(Ordering::Acquire) {
            if let Some(segment) = &replay.active_segment {
                events.push_back(json!({"type":"assistant_segment","segment":segment,"toolStreamProtocol":3,"conversationId":self.conversation_id,"requestId":self.request_id,"createdAt":now_iso8601(),"replayThrough":replay.sequence}));
            }
        }
        events.push_back(json!({"type":"content_snapshot","content":replay.content,"reasoning":replay.reasoning,"conversationId":self.conversation_id,"requestId":self.request_id,"createdAt":now_iso8601(),"replayThrough":replay.sequence}));
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
            native_cancellation: crate::service_cancellation::CancellationLease::acquire(
                &cancellation_key,
            )
            .unwrap(),
            cancellation_key,
            events,
            replay: Default::default(),
            finished: AtomicBool::new(false),
            suspending: AtomicBool::new(false),
            result: Default::default(),
            created: Instant::now(),
        })
    }
}

struct Approval {
    conversation: String,
    request: String,
    call: String,
    response: oneshot::Sender<bool>,
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
    approvals: Mutex<HashMap<String, Approval>>,
    execution_tasks: AtomicUsize,
    extension_prewarm_scheduled: AtomicBool,
    model_selection_received: watch::Sender<bool>,
    background_cancellation: watch::Sender<bool>,
}
impl GatewayRuntime {
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
            config,
            tools: Arc::new(ToolRuntime::default()),
            runs: Default::default(),
            conversations: Default::default(),
            approvals: Default::default(),
            execution_tasks: AtomicUsize::new(0),
            extension_prewarm_scheduled: AtomicBool::new(false),
            model_selection_received: watch::channel(false).0,
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
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {},
                _ = cancellation.changed() => return,
            }
            if *cancellation.borrow() {
                return;
            }
            // Prefer model connection/loading before optional MCP work; none of this joins the UI readiness wait.
            // 模型连接与装载优先于可选 MCP；整个准备过程不加入界面就绪或消息发送等待。
            let mut selection = runtime.model_selection_received.subscribe();
            if !*selection.borrow_and_update() {
                // Give the activated desktop a bounded chance to submit its saved selection before fallback warmup.
                // 窗口激活后有界等待桌面提交保存的选择；独立网关无桌面时仍可开始缺省准备。
                tokio::select! {
                    _ = selection.changed() => {},
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                    _ = cancellation.changed() => return,
                }
            }
            let _ = runtime.prepare_models(Value::Null).await;
            runtime
                .model_preparation
                .wait_idle(cancellation.clone(), Duration::from_secs(90))
                .await;
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
        self.model_selection_received.send_replace(true);
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
        mut input: Value,
        original_message_raw: Option<String>,
    ) -> Result<Arc<RequestRun>, GatewayError> {
        let conversation = checked_id(&input["conversationId"], "conversationId")?;
        if !input["message"].as_str().is_some_and(|text| {
            !text.trim().is_empty() && text.encode_utf16().count() <= 2 * 1024 * 1024
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
            events,
            native_cancellation,
            cancellation_key,
            replay: Default::default(),
            finished: AtomicBool::new(false),
            suspending: AtomicBool::new(false),
            result: Default::default(),
            created: Instant::now(),
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
        Some(
            json!({"requestId":request,"conversationId":conversation,"status":if run.finished.load(Ordering::Acquire){"completed"}else{"running"},"result":result}),
        )
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
            let detail = self.tasks.detail(task_id).await?;
            let task = &detail["task"];
            if task["scope"]["conversationId"] != conversation || !task["parentTaskId"].is_null() {
                return Err(GatewayError::new(
                    "TASK_SCOPE_DENIED",
                    "只能观察本聊天的主任务回复。",
                    403,
                ));
            }
            let request = task["requestId"].as_str().unwrap_or("");
            if let Some(run) = self.runs.lock().await.get(request).cloned() {
                let owns_attempt = run.journal.as_ref().is_some_and(|journal| {
                    detail["attempt"]["token"]["attemptId"] == journal.token().attempt_id
                        && detail["attempt"]["token"]["executionVersion"]
                            == journal.token().execution_version
                });
                if task["state"] != "queued" && owns_attempt {
                    return Ok(run);
                }
            }
            if !["queued", "running", "waiting-children"]
                .contains(&task["state"].as_str().unwrap_or(""))
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
            native_cancellation: crate::service_cancellation::CancellationLease::acquire(
                &cancellation_key,
            )
            .map_err(|code| GatewayError::new(code, "子任务取消状态无法创建。", 503))?,
            cancellation_key,
            events,
            replay: Default::default(),
            finished: AtomicBool::new(false),
            suspending: AtomicBool::new(false),
            result: Default::default(),
            created: Instant::now(),
        });
        let mut runs = self.task_runs.lock().map_err(|_| {
            GatewayError::new("TASK_RUN_LOCK_FAILED", "子任务执行者登记失败。", 503)
        })?;
        runs.retain(|_, run| run.strong_count() > 0);
        runs.insert(task.task_id.clone(), Arc::downgrade(&run));
        Ok(run)
    }
    pub async fn cancel_task_worker(&self, task_id: &str) {
        if let Ok(runs) = self.task_runs.lock() {
            if let Some(run) = runs.get(task_id).and_then(Weak::upgrade) {
                run.cancel();
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
    ) -> Result<Arc<RequestRun>, GatewayError> {
        self.begin_raw(input, original).await
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
            authorized_roots: vec![workdir],
            cancellation,
            runtime: self.tools.clone(),
            config,
            approved: false,
            events: None,
            external_execute: None,
        };
        let mut saved = crate::tools::gateway::get_config(&context).await?;
        saved["trustedUserText"] = json!(text);
        saved["officialToolsRoot"] = json!(self.config.application_root.join("official-tools"));
        saved["bindingSnapshot"] =
            json!({"projectId":relationship["projectId"],"folderPath":relationship["folderPath"]});
        let config = self.config.clone();
        let project = context.project_id.clone();
        saved["retrieval"] = tokio::task::spawn_blocking(move || {
            crate::retrieval::gateway::effective(&config, project.as_deref())
        })
        .await
        .map_err(|_| GatewayError::new("RETRIEVAL_WORKER_FAILED", "检索设置读取失败。", 500))??;
        context.config = saved;
        Ok(context)
    }
    pub async fn await_approval(
        &self,
        run: &Arc<RequestRun>,
        call: &Value,
        receipt: &Value,
        round: u64,
        order: u64,
        task_deadline: Instant,
    ) -> Result<bool, GatewayError> {
        self.await_approval_until(
            run,
            call,
            receipt,
            round,
            order,
            task_deadline,
            Instant::now() + Duration::from_secs(300),
        )
        .await
    }
    async fn await_approval_until(
        &self,
        run: &Arc<RequestRun>,
        call: &Value,
        receipt: &Value,
        round: u64,
        order: u64,
        task_deadline: Instant,
        approval_deadline: Instant,
    ) -> Result<bool, GatewayError> {
        let expires = task_deadline.min(approval_deadline);
        let deadline_code = if task_deadline <= approval_deadline {
            "TOOL_RUN_TIMEOUT"
        } else {
            "TOOL_APPROVAL_EXPIRED"
        };
        if Instant::now() >= expires {
            return Err(GatewayError::new(
                deadline_code,
                "审批或任务有效期已结束，操作未派发。",
                408,
            ));
        }
        let id = receipt["invocation"]["id"]
            .as_str()
            .ok_or_else(|| GatewayError::new("INVALID_APPROVAL_RECEIPT", "审批回执无效。", 500))?
            .to_owned();
        let (sender, receiver) = oneshot::channel();
        self.approvals.lock().await.insert(
            id.clone(),
            Approval {
                conversation: run.conversation_id.clone(),
                request: run.request_id.clone(),
                call: call["id"].as_str().unwrap_or("").to_owned(),
                response: sender,
                expires,
                deadline_code,
            },
        );
        run.emit(json!({"type":"approval_required","tool":{"toolCallId":call["id"],"name":call["name"],"arguments":call["arguments"],"status":"approval-required","approvalId":id,"summary":"等待授权","round":round,"order":order}})).await;
        let mut cancellation = run.cancellation.subscribe();
        if *cancellation.borrow() {
            self.approvals.lock().await.remove(&id);
            return Ok(false);
        }
        // Expiry and cancellation release the conversation task even without a user decision.
        // 即使用户没有决定，过期与取消也会释放当前会话任务，不把审批永远留在活动槽中。
        let approved = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(expires.into()) => Err(GatewayError::new(deadline_code, "审批或任务有效期已结束，操作未派发。", 408)),
            _ = cancellation.changed() => Ok(false),
            result = receiver => {
                if Instant::now() >= expires {
                    Err(GatewayError::new(deadline_code, "审批或任务有效期已结束，操作未派发。", 408))
                } else { Ok(result.unwrap_or(false)) }
            },
        };
        self.approvals.lock().await.remove(&id);
        approved
    }
    /// Child control can stop an approval wait without leaving a stale approval in the parent's UI.
    /// 子任务控制可以结束审批等待，父界面不会留下失效的审批项。
    pub async fn await_child_approval(
        &self,
        parent: &Arc<RequestRun>,
        child: &Arc<RequestRun>,
        call: &Value,
        receipt: &Value,
        round: u64,
        order: u64,
        task_deadline: Instant,
    ) -> Result<bool, GatewayError> {
        let approval_deadline = Instant::now() + Duration::from_secs(300);
        let expires = task_deadline.min(approval_deadline);
        let deadline_code = if task_deadline <= approval_deadline {
            "TOOL_RUN_TIMEOUT"
        } else {
            "TOOL_APPROVAL_EXPIRED"
        };
        if Instant::now() >= expires {
            return Err(GatewayError::new(
                deadline_code,
                "子任务审批或任务有效期已结束，操作未派发。",
                408,
            ));
        }
        let mut child_cancel = child.cancellation.subscribe();
        let mut parent_cancel = parent.cancellation.subscribe();
        if *child_cancel.borrow() || *parent_cancel.borrow() {
            return Ok(false);
        }
        let id = receipt["invocation"]["id"]
            .as_str()
            .ok_or_else(|| {
                GatewayError::new("INVALID_APPROVAL_RECEIPT", "子任务审批回执无效。", 500)
            })?
            .to_owned();
        let (sender, receiver) = oneshot::channel();
        self.approvals.lock().await.insert(
            id.clone(),
            Approval {
                conversation: parent.conversation_id.clone(),
                request: parent.request_id.clone(),
                call: call["id"].as_str().unwrap_or("").into(),
                response: sender,
                expires,
                deadline_code,
            },
        );
        // Child approvals share the parent's user interface, never the parent's active tool sequence.
        // 子审批共用父任务界面，但不能进入父回复的活动工具序号账本。
        parent.emit(json!({"type":"task_approval_required","taskId":child.journal.as_ref().map(|journal| journal.task_id()),
            "tool":{"toolCallId":call["id"],"name":call["name"],"arguments":call["arguments"],"status":"approval-required","approvalId":id,"summary":"子任务等待授权","round":round,"order":order}})).await;
        let approved = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(expires.into()) => Err(GatewayError::new(deadline_code, "子任务审批或任务有效期已结束，操作未派发。", 408)),
            _ = crate::tools::gateway::wait_for_cancellation(&mut child_cancel) => Ok(false),
            _ = crate::tools::gateway::wait_for_cancellation(&mut parent_cancel) => Ok(false),
            result = receiver => if Instant::now() >= expires {
                Err(GatewayError::new(deadline_code, "子任务审批或任务有效期已结束，操作未派发。", 408))
            } else { Ok(result.unwrap_or(false)) },
        };
        self.approvals.lock().await.remove(&id);
        parent.emit(json!({"type":"task_approval_closed","taskId":child.journal.as_ref().map(|journal|journal.task_id()),"approvalId":id,"toolCallId":call["id"],"approved":approved.as_ref().is_ok_and(|approved| *approved),"code":approved.as_ref().err().map(|error|&error.code)})).await;
        approved
    }
    pub async fn approve(&self, body: &Value) -> Result<Value, GatewayError> {
        let id = body["approvalId"]
            .as_str()
            .or_else(|| body["id"].as_str())
            .unwrap_or("");
        let mut approvals = self.approvals.lock().await;
        let pending = approvals
            .get(id)
            .ok_or_else(|| GatewayError::new("APPROVAL_NOT_FOUND", "审批已失效或不存在。", 404))?;
        for (key, expected) in [
            ("conversationId", pending.conversation.as_str()),
            ("requestId", pending.request.as_str()),
            ("toolCallId", pending.call.as_str()),
        ] {
            if body[key].as_str() != Some(expected) {
                return Err(GatewayError::new(
                    "APPROVAL_SCOPE_MISMATCH",
                    "审批范围不匹配。",
                    409,
                ));
            }
        }
        if Instant::now() >= pending.expires {
            let code = pending.deadline_code;
            approvals.remove(id);
            return Err(GatewayError::new(
                code,
                "审批或任务有效期已结束，操作未派发。",
                408,
            ));
        }
        let approved = body["approved"]
            .as_bool()
            .or_else(|| body["allow"].as_bool())
            .ok_or_else(|| GatewayError::new("INVALID_APPROVAL", "审批决定无效。", 400))?;
        let pending = approvals.remove(id).unwrap();
        let _ = pending.response.send(approved);
        Ok(json!({"approved":approved,"approvalId":id,"toolCallId":pending.call}))
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
mod approval_tests {
    use super::*;

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
            assert!(runtime.approvals.lock().await.is_empty());
            assert_eq!(runtime.approve(&json!({"approvalId":receipt["invocation"]["id"],"conversationId":run.conversation_id,"requestId":run.request_id,"toolCallId":"owned-call","approved":true})).await.unwrap_err().code, "APPROVAL_NOT_FOUND");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn approval_submission_checks_expiry_and_scope_and_accepts_live_decision() {
        let (runtime, root) = runtime();
        let run = RequestRun::test_run(16);
        let (sender, _receiver) = oneshot::channel();
        runtime.approvals.lock().await.insert(
            "expired".into(),
            Approval {
                conversation: run.conversation_id.clone(),
                request: run.request_id.clone(),
                call: "call".into(),
                response: sender,
                expires: Instant::now() - Duration::from_millis(1),
                deadline_code: "TOOL_APPROVAL_EXPIRED",
            },
        );
        let decision = json!({"approvalId":"expired","conversationId":run.conversation_id,"requestId":run.request_id,"toolCallId":"call","approved":true});
        assert_eq!(
            runtime.approve(&decision).await.unwrap_err().code,
            "TOOL_APPROVAL_EXPIRED"
        );
        assert!(runtime.approvals.lock().await.is_empty());
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
        while runtime.approvals.lock().await.is_empty() {
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
        assert!(runtime.approvals.lock().await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
