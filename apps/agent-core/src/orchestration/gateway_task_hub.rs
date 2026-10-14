//! Background coordination reuses native workers, resource admission and the sole task repository.
//! 后台协调复用原生执行器、资源准入和唯一任务仓储；控制不排在模型调用之后。

use super::{
    gateway_runtime::{GatewayRuntime, RequestRun},
    gateway_task_journal::TaskJournal,
};
use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    storage::{ContractError, gateway as data, task_repository::*},
    tools::gateway::GatewayToolContext,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{Notify, OnceCell, Semaphore, watch};

type Result<T> = std::result::Result<T, GatewayError>;
struct Runner {
    cancel: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
}

pub struct TaskHub {
    config: GatewayConfig,
    runtime: Weak<GatewayRuntime>,
    repository: OnceCell<Arc<TaskRepository>>,
    runners: Mutex<HashMap<String, Runner>>,
    restoring_roots: Mutex<HashSet<String>>,
    root_slots: Mutex<HashMap<String, Arc<Semaphore>>>,
    workspace_writers: Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>,
    changes: Notify,
    stopping: std::sync::atomic::AtomicBool,
}
impl TaskHub {
    pub fn new(config: GatewayConfig, runtime: Weak<GatewayRuntime>) -> Self {
        Self {
            config,
            runtime,
            repository: OnceCell::new(),
            runners: Mutex::new(HashMap::new()),
            restoring_roots: Mutex::new(HashSet::new()),
            root_slots: Mutex::new(HashMap::new()),
            workspace_writers: Mutex::new(HashMap::new()),
            changes: Notify::new(),
            stopping: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub async fn workspace_mutation_guard(
        &self,
        workdir: &std::path::Path,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        let workdir = workdir.to_owned();
        let workdir = tokio::task::spawn_blocking(move || std::fs::canonicalize(workdir))
            .await
            .map_err(|_| failure("TASK_WORKSPACE_LOCK_FAILED"))?
            .map_err(|_| failure("TASK_WORKSPACE_LOCK_FAILED"))?;
        let writer = {
            let mut writers = self
                .workspace_writers
                .lock()
                .map_err(|_| failure("TASK_WORKSPACE_LOCK_FAILED"))?;
            writers.retain(|_, writer| writer.strong_count() > 0);
            if let Some(writer) = writers.get(&workdir).and_then(Weak::upgrade) {
                writer
            } else {
                let writer = Arc::new(tokio::sync::Mutex::new(()));
                writers.insert(workdir.to_owned(), Arc::downgrade(&writer));
                writer
            }
        };
        Ok(writer.lock_owned().await)
    }
    async fn repository(&self) -> Result<Arc<TaskRepository>> {
        let root = self.config.data_root.clone();
        self.repository
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || TaskRepository::open(&root).map(Arc::new))
                    .await
                    .map_err(|_| failure("TASK_STORE_WORKER_FAILED"))?
                    .map_err(GatewayError::from)
            })
            .await
            .cloned()
    }
    async fn store<T: Send + 'static>(
        &self,
        action: impl FnOnce(&TaskRepository) -> std::result::Result<T, ContractError> + Send + 'static,
    ) -> Result<T> {
        let repository = self.repository().await?;
        tokio::task::spawn_blocking(move || action(&repository))
            .await
            .map_err(|_| failure("TASK_STORE_WORKER_FAILED"))?
            .map_err(GatewayError::from)
    }
    pub async fn list(
        &self,
        conversation: Option<&str>,
        project: Option<&str>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        let conversation = conversation.map(str::to_owned);
        let project = project.map(str::to_owned);
        let after = after.map(str::to_owned);
        let count = limit.clamp(1, 256);
        let mut tasks = self
            .store(move |repository| {
                repository.list_roots_for_scope(
                    conversation.as_deref(),
                    project.as_deref(),
                    after.as_deref(),
                    count + 1,
                )
            })
            .await?;
        let has_more = tasks.len() > count;
        tasks.truncate(count);
        let next = if has_more {
            tasks.last().map(|task| task.task_id.clone())
        } else {
            None
        };
        let displayed: Vec<Value> = tasks
            .iter()
            .map(|task| {
                let mut value = serde_json::to_value(task).unwrap_or(Value::Null);
                value["displayInTaskList"] = json!(
                    task.usage.output_tokens >= 8192
                        || task.usage.tool_duration_ms > 0
                            && (task.usage.model_requests >= 3
                                || task.usage.compactions > 0
                                || task.usage.tool_duration_ms >= 30_000)
                        || matches!(
                            task.state,
                            TaskState::Paused
                                | TaskState::WaitingChildren
                                | TaskState::WaitingApproval
                                | TaskState::WaitingVerification
                        )
                );
                value
            })
            .collect();
        Ok(json!({"tasks":displayed,"hasMore":has_more,"nextTaskId":next}))
    }
    pub async fn paused(&self, conversation: &str, project: Option<&str>) -> Result<Value> {
        let conversation = conversation.to_owned();
        let project = project.map(str::to_owned);
        self.store(move |repository| {
            let tasks =
                repository.paused_roots_for_conversation(&conversation, project.as_deref())?;
            Ok(json!({"tasks":tasks}))
        })
        .await
    }
    pub async fn detail(&self, id: &str) -> Result<Value> {
        let id = id.to_owned();
        self.store(move|repository| {
            let task=repository.load(&id)?;let children=repository.children(&id)?;
            Ok(json!({"task":task,"children":children,"attempt":repository.current_attempt(&id)?,"operations":repository.operations(&id)?,"checkpoint":repository.latest_checkpoint(&id)?,"artifacts":repository.artifacts(&id)?,"modelReservations":repository.model_reservations(&id)?}))
        }).await
    }
    pub async fn events(&self, id: &str, after: u64, limit: usize) -> Result<Value> {
        let id = id.to_owned();
        let count = limit.clamp(1, 256);
        self.store(move|repository| {
            let task=repository.load(&id)?;let mut events=repository.read_events(&id,after,count+1)?;
            let has_more=events.len()>count;events.truncate(count);
            let through=events.last().map(|event|event.sequence).unwrap_or(after.min(task.event_sequence));
            Ok(json!({"taskId":id,"events":events,"throughSequence":through,"hasMore":has_more,"state":task.state,"version":task.version}))
        }).await
    }
    async fn validate_binding(&self, task: &TaskRecord) -> Result<()> {
        let root = self.config.data_root.clone();
        let conversation = task.scope.conversation_id.clone();
        let relation =
            tokio::task::spawn_blocking(move || data::relationship(&root, &conversation))
                .await
                .map_err(|_| failure("TASK_SCOPE_WORKER_FAILED"))??;
        let binding = &task.scope.permission_snapshot["bindingSnapshot"];
        if task.scope.project_id.as_deref() != relation["projectId"].as_str()
            || (binding.is_object()
                && (binding["projectId"] != relation["projectId"]
                    || binding["folderPath"] != relation["folderPath"]))
        {
            return Err(GatewayError::new(
                "TASK_SCOPE_CHANGED",
                "Task work scope changed; review it before continuing. / 任务工作范围已变化，请核对后继续。",
                409,
            ));
        }
        Ok(())
    }
    pub async fn control(
        self: &Arc<Self>,
        id: &str,
        version: u64,
        action: TaskControl,
    ) -> Result<Value> {
        let repository = self.repository().await?;
        let task = repository.load(id)?;
        if action == TaskControl::Resume {
            self.validate_binding(&task).await?;
            let runtime = self
                .runtime
                .upgrade()
                .ok_or_else(|| failure("TASK_RUNTIME_UNAVAILABLE"))?;
            tokio::time::timeout(Duration::from_secs(35), async {
                self.wait_existing_lease(id, None).await?;
                if task.parent_task_id.is_none() {
                    runtime.wait_task_delivery_boundary(id).await;
                }
                Ok::<(), GatewayError>(())
            })
            .await
            .map_err(|_| {
                GatewayError::new(
                    "TASK_RESUME_WAITING",
                    "旧操作正在结算，进度已保存，请等待任务暂停完成。",
                    409,
                )
            })??;
        }
        let owned = id.to_owned();
        let cloned = action.clone();
        let changed = self
            .store(move |repository| repository.control(&owned, version, cloned))
            .await?;
        let mut affected = vec![changed.task_id.clone()];
        let mut index = 0;
        while index < affected.len() {
            let id = affected[index].clone();
            let children = self
                .store(move |repository| repository.children(&id))
                .await?;
            affected.extend(children.into_iter().map(|child| child.task_id));
            index += 1;
        }
        if action != TaskControl::Resume {
            if let Ok(runners) = self.runners.lock() {
                for id in &affected {
                    if let Some(runner) = runners.get(id) {
                        runner.cancel.send_replace(true);
                    }
                }
            }
            if let Some(runtime) = self.runtime.upgrade() {
                for id in &affected {
                    runtime.cancel_task_worker(id).await;
                }
            }
        } else {
            self.schedule_root_recovery(&changed).await?;
        }
        self.changes.notify_waiters();
        Ok(json!({"task":changed,"affectedTaskIds":affected}))
    }
    /// Pause before accepting an amendment; resume only after the old executor has settled its lease.
    /// 先暂停再接收补充要求，旧执行者结算租约之后才恢复；正式用户消息与原授权范围不被模型代写。
    pub async fn steer(
        self: &Arc<Self>,
        id: &str,
        version: u64,
        message_id: &str,
        message: &str,
        action: &str,
    ) -> Result<Value> {
        if !["continue", "update"].contains(&action)
            || message.trim().is_empty()
            || message.len() > 32_768
        {
            return Err(GatewayError::new(
                "INVALID_TASK_STEERING",
                "插话动作或正文无效。",
                400,
            ));
        }
        super::gateway_runtime::checked_id(&json!(message_id), "userMessageId")?;
        let repository = self.repository().await?;
        let task = repository.load(id)?;
        if task.parent_task_id.is_some() {
            return Err(failure("TASK_STEERING_ROOT_REQUIRED"));
        }
        self.validate_binding(&task).await?;
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| failure("TASK_RUNTIME_UNAVAILABLE"))?;
        let gate = runtime.conversation_gate(&format!("steer:{id}")).await;
        let _steering = gate.lock().await;
        let content_hash = crate::retrieval::gateway_index::hash(message);
        let existing = repository
            .user_steering(id)?
            .into_iter()
            .find(|event| event.payload["userMessageId"] == message_id);
        if let Some(existing) = existing {
            if existing.payload["contentHash"] != content_hash
                || existing.payload["action"] != action
            {
                return Err(GatewayError::new(
                    "TASK_STEERING_CONFLICT",
                    "插话身份与已保存内容冲突。",
                    409,
                ));
            }
            let current = repository.load(id)?;
            return Ok(
                json!({"task":current,"affectedTaskIds":[id],"steeringAccepted":true,"reused":true,"reconnectRequired":true,"resumeRequired":current.state==TaskState::Paused}),
            );
        }
        let paused = self.control(id, version, TaskControl::Pause).await?;
        let paused_version = paused["task"]["version"]
            .as_u64()
            .ok_or_else(|| failure("TASK_STEERING_VERSION_MISSING"))?;
        let root = self.config.data_root.clone();
        let conversation = task.scope.conversation_id.clone();
        let formal = json!({"Id":message_id,"Role":"user","Content":message,"CreatedAt":crate::gateway_contract::now_iso8601(),"Status":"completed"});
        let project = task.scope.project_id.clone();
        tokio::task::spawn_blocking(move || {
            let ids = HashSet::from([formal["Id"].as_str().unwrap().to_owned()]);
            let read = crate::storage::conversation_history::read_latest(
                &root,
                &json!({"Id":conversation,"ProjectId":project}),
                Some(&ids),
                false,
            )?;
            for raw in read.messages {
                let saved: Value =
                    serde_json::from_str(&crate::storage::raw_json::scalar_projection(&raw))
                        .map_err(|_| {
                            ContractError::new("插话原消息无效。", "INVALID_CONVERSATION_DATA", 500)
                        })?;
                if saved["Id"] == formal["Id"]
                    && (saved["Role"] != "user" || saved["Content"] != formal["Content"])
                {
                    return Err(ContractError::new(
                        "插话消息身份与已有正文冲突。",
                        "TASK_STEERING_CONFLICT",
                        409,
                    ));
                }
            }
            data::upsert_message(&root, &conversation, &formal)
        })
        .await
        .map_err(|_| failure("TASK_STEERING_MESSAGE_FAILED"))??;
        let changed = repository.record_user_steering(
            id,
            paused_version,
            message_id,
            action,
            &content_hash,
        )?;
        match tokio::time::timeout(Duration::from_secs(35), async {
            self.wait_existing_lease(id, None).await?;
            runtime.wait_task_delivery_boundary(id).await;
            Ok::<(), GatewayError>(())
        })
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Ok(
                    json!({"task":repository.load(id)?,"affectedTaskIds":[id],"steeringAccepted":true,"reconnectRequired":true,"resumeRequired":true}),
                );
            }
        }
        let current = repository.load(id)?;
        // A later user control wins over this request; do not silently undo another pause or cancel.
        // 后到的用户控制优先，不能静默撤销再次暂停或取消。
        if current.state != TaskState::Paused || current.stop_reason != changed.stop_reason {
            return Ok(
                json!({"task":current,"affectedTaskIds":[id],"steeringAccepted":true,"reconnectRequired":true}),
            );
        }
        let mut response = self
            .control(id, current.version, TaskControl::Resume)
            .await?;
        response["steeringAccepted"] = json!(true);
        response["reconnectRequired"] = json!(true);
        Ok(response)
    }
    pub async fn tool(
        self: &Arc<Self>,
        parent: Arc<RequestRun>,
        context: GatewayToolContext,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        let journal = parent
            .journal
            .as_ref()
            .ok_or_else(|| failure("TASK_JOURNAL_UNAVAILABLE"))?;
        match name {
            "task.delegate" => self.delegate(parent, context, args).await,
            "task.wait" => self.wait(parent, context, args).await,
            "task.read" => {
                self.check_tool_scope(journal.task_id(), &context).await?;
                self.read_result(journal, &args).await
            }
            "task.result" if args["reference"].is_object() => {
                self.check_tool_scope(journal.task_id(), &context).await?;
                self.read_result(journal, &args).await
            }
            "task.status" | "task.result" => {
                let id = args["taskId"].as_str().unwrap_or(journal.task_id());
                self.check_tool_scope(id, &context).await?;
                self.detail(id).await
            }
            "task.control" => {
                let id = args["taskId"].as_str().unwrap_or(journal.task_id());
                self.check_tool_scope(id, &context).await?;
                let version = args["expectedVersion"]
                    .as_u64()
                    .ok_or_else(|| failure("INVALID_TASK_CONTROL"))?;
                let action: TaskControl = serde_json::from_value(args["action"].clone())
                    .map_err(|_| failure("INVALID_TASK_CONTROL"))?;
                self.control(id, version, action).await
            }
            _ => Err(failure("TOOL_NOT_IMPLEMENTED")),
        }
    }
    async fn check_tool_scope(&self, id: &str, context: &GatewayToolContext) -> Result<TaskRecord> {
        let id = id.to_owned();
        let task = self.store(move |repository| repository.load(&id)).await?;
        if task.scope.conversation_id != context.conversation_id
            || task.scope.project_id != context.project_id
        {
            return Err(GatewayError::new(
                "TASK_SCOPE_DENIED",
                "Task is outside the current authorized chat. / 任务不在当前授权聊天内。",
                403,
            ));
        }
        self.validate_binding(&task).await?;
        Ok(task)
    }
    async fn delegate(
        self: &Arc<Self>,
        parent: Arc<RequestRun>,
        context: GatewayToolContext,
        args: Value,
    ) -> Result<Value> {
        let journal = parent
            .journal
            .as_ref()
            .ok_or_else(|| failure("TASK_JOURNAL_UNAVAILABLE"))?;
        let root_task = journal.check_dispatch_fence()?;
        if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
            return Err(failure("TASK_HUB_STOPPING"));
        }
        if root_task.state == TaskState::WaitingVerification {
            return Err(failure("TASK_WAITING_VERIFICATION"));
        }
        let goal = args["goal"]
            .as_str()
            .filter(|goal| !goal.trim().is_empty())
            .ok_or_else(|| failure("INVALID_TASK_GOAL"))?;
        let request = args["requestId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                crate::retrieval::gateway_index::hash(&json!([root_task.task_id, args]).to_string())
            });
        let reference = root_task
            .input_reference
            .as_ref()
            .ok_or_else(|| failure("TASK_INPUT_MISSING"))?;
        let (root_input, _, _) =
            super::gateway_task_input::read(&self.config.data_root, reference).await?;
        let input = json!({"provider":root_input["provider"],"model":root_input["model"],"goal":goal,"acceptance":args["acceptance"],"contextRefs":args["contextRefs"],"writeTargets":args["writeTargets"],"rootUserConstraints":{"goal":root_task.goal,"permissionSnapshot":root_task.scope.permission_snapshot,"inputReference":root_task.input_reference},"runLimits":root_input["runLimits"]});
        let bytes = serde_json::to_vec(&input).map_err(|_| failure("INVALID_TASK_INPUT"))?;
        let context_ref = journal.save_blob("child-context", &bytes).await?;
        let acceptance: Vec<String> = args["acceptance"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let dependency_ids: Vec<String> = args["dependencyIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let task_input = NewTask {
            request_id: format!("child:{request}"),
            goal: goal.into(),
            acceptance,
            scope: root_task.scope.clone(),
            parent_task_id: Some(root_task.task_id.clone()),
            dependency_ids,
            budget: root_task.budget.clone(),
            input_reference: Some(
                json!({"kind":"task-child-input","contextReference":context_ref}),
            ),
        };
        let child = self
            .store(move |repository| repository.accept_task(&task_input))
            .await?;
        if !child.state.is_terminal() && child.state != TaskState::Paused {
            self.queue_child(parent, child.clone(), context, input)
                .await?;
        }
        Ok(
            json!({"taskId":child.task_id,"parentTaskId":child.parent_task_id,"status":child.state,"task":child,"contextRef":context_ref}),
        )
    }
    async fn queue_child(
        self: &Arc<Self>,
        parent: Arc<RequestRun>,
        child: TaskRecord,
        mut context: GatewayToolContext,
        input: Value,
    ) -> Result<()> {
        let mut runners = self
            .runners
            .lock()
            .map_err(|_| failure("TASK_RUNNER_LOCK_FAILED"))?;
        if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
            return Err(failure("TASK_HUB_STOPPING"));
        }
        runners.retain(|_, runner| !runner.handle.is_finished());
        if runners.contains_key(&child.task_id) {
            return Ok(());
        }
        let (cancel, receiver) = watch::channel(false);
        context.cancellation = receiver;
        let id = child.task_id.clone();
        let hub = self.clone();
        let runner_id = id.clone();
        let handle = tokio::spawn(async move {
            let result = hub.execute_child(parent, child, context, input).await;
            if let Err(error) = result {
                let _ = hub.record_worker_failure(&runner_id, &error).await;
            }
            if let Ok(mut runners) = hub.runners.lock() {
                runners.remove(&runner_id);
            }
            hub.changes.notify_waiters();
        });
        runners.insert(id, Runner { cancel, handle });
        Ok(())
    }
    async fn execute_child(
        self: &Arc<Self>,
        parent: Arc<RequestRun>,
        child: TaskRecord,
        context: GatewayToolContext,
        input: Value,
    ) -> Result<Value> {
        // A replacement waits for an existing lease instead of stealing a live worker's receipts.
        // 替代执行先等待旧租约，不能抢占仍有效 Worker 的回执提交权。
        self.wait_existing_lease(&child.task_id, Some(context.cancellation.clone()))
            .await?;
        let root_id = child
            .parent_task_id
            .clone()
            .ok_or_else(|| failure("TASK_PARENT_MISSING"))?;
        let gate = {
            let mut gates = self
                .root_slots
                .lock()
                .map_err(|_| failure("TASK_QUEUE_LOCK_FAILED"))?;
            gates
                .entry(root_id)
                .or_insert_with(|| Arc::new(Semaphore::new(3)))
                .clone()
        };
        loop {
            let notified = self.changes.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let repository = self.repository().await?;
            let current = repository.load(&child.task_id)?;
            if current.state.is_terminal()
                || current.state == TaskState::Paused
                || *context.cancellation.borrow()
            {
                return Ok(json!({"taskId":current.task_id,"status":current.state}));
            }
            let dependencies: Vec<TaskRecord> = current
                .dependency_ids
                .iter()
                .map(|id| repository.load(id))
                .collect::<std::result::Result<_, _>>()?;
            if dependencies
                .iter()
                .any(|task| task.state.is_terminal() && task.state != TaskState::Completed)
            {
                repository.mark_unstarted_wait(
                    &current.task_id,
                    current.version,
                    TaskState::Failed,
                    "required-dependency-incomplete",
                )?;
                return Ok(json!({"taskId":current.task_id,"status":"failed"}));
            }
            if dependencies
                .iter()
                .all(|task| task.state == TaskState::Completed)
            {
                break;
            }
            let mut cancellation = context.cancellation.clone();
            tokio::select! {_=notified=>{},_=crate::tools::gateway::wait_for_cancellation(&mut cancellation)=>return Err(failure("ABORT_ERR"))}
        }
        let mut cancellation = context.cancellation.clone();
        let _slot = tokio::select! {permit=gate.acquire_owned()=>permit.map_err(|_|failure("TASK_QUEUE_CLOSED"))?,_=crate::tools::gateway::wait_for_cancellation(&mut cancellation)=>return Err(failure("ABORT_ERR"))};
        let writes = input["writeTargets"]
            .as_array()
            .is_some_and(|targets| !targets.is_empty());
        let _write_guard = if writes {
            Some(
                tokio::select! {guard=self.workspace_mutation_guard(&context.workdir)=>guard?,_=crate::tools::gateway::wait_for_cancellation(&mut cancellation)=>return Err(failure("ABORT_ERR"))},
            )
        } else {
            None
        };
        let current = self.repository().await?.load(&child.task_id)?;
        self.validate_binding(&current).await?;
        if current.state.is_terminal()
            || current.state == TaskState::Paused
            || *context.cancellation.borrow()
        {
            return Ok(json!({"taskId":current.task_id,"status":current.state}));
        }
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| failure("TASK_RUNTIME_UNAVAILABLE"))?;
        super::gateway_task_worker::execute(runtime, parent, current, context, input).await
    }
    async fn record_worker_failure(&self, id: &str, error: &GatewayError) -> Result<()> {
        let repository = self.repository().await?;
        let task = repository.load(id)?;
        if task.state.is_terminal() || task.state == TaskState::Paused {
            return Ok(());
        }
        // The worker owns releasing its attempt. Queue failures cannot revoke another executor.
        // 尝试由 Worker 自己释放；排队失败不得撤销另一个仍有效执行者的尝试。
        let task = repository.load(id)?;
        let stopping = self.stopping.load(std::sync::atomic::Ordering::Acquire);
        let state = if stopping {
            TaskState::WaitingNetwork
        } else if repository.operations(id)?.iter().any(|operation| {
            operation.state == OperationState::Unknown
                && operation.intent.effect != OperationEffect::Observation
        }) {
            TaskState::WaitingVerification
        } else if error.code == "ABORT_ERR" {
            TaskState::WaitingConfig
        } else {
            TaskState::WaitingNetwork
        };
        let reason = if stopping {
            "gateway-shutdown"
        } else {
            error.code.as_str()
        };
        let _ = repository.mark_unstarted_wait(id, task.version, state, reason);
        Ok(())
    }
    async fn wait(
        &self,
        parent: Arc<RequestRun>,
        context: GatewayToolContext,
        args: Value,
    ) -> Result<Value> {
        let default = parent
            .journal
            .as_ref()
            .ok_or_else(|| failure("TASK_JOURNAL_UNAVAILABLE"))?
            .task_id()
            .to_owned();
        let ids: Vec<String> = args["taskIds"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| vec![default]);
        if ids.is_empty() || ids.len() > 128 {
            return Err(failure("INVALID_TASK_WAIT"));
        }
        let timeout = Duration::from_millis(
            args["timeoutMs"]
                .as_u64()
                .unwrap_or(1_800_000)
                .clamp(100, 3_600_000),
        );
        let started = tokio::time::Instant::now();
        loop {
            let notified = self.changes.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut tasks = Vec::new();
            for id in &ids {
                tasks.push(self.check_tool_scope(id, &context).await?);
            }
            if tasks.iter().all(|task| task.state.is_terminal())
                || tasks.iter().any(|task| {
                    matches!(
                        task.state,
                        TaskState::WaitingNetwork
                            | TaskState::WaitingConfig
                            | TaskState::WaitingVerification
                            | TaskState::Paused
                    )
                })
            {
                return self.wait_response(&parent, tasks, false).await;
            }
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return self.wait_response(&parent, tasks, true).await;
            };
            let mut cancellation = parent.cancellation.subscribe();
            tokio::select! {_=tokio::time::sleep(remaining)=>return self.wait_response(&parent,tasks,true).await,_=notified=>{},_=crate::tools::gateway::wait_for_cancellation(&mut cancellation)=>return Err(failure("ABORT_ERR"))}
        }
    }
    async fn wait_response(
        &self,
        parent: &Arc<RequestRun>,
        tasks: Vec<TaskRecord>,
        timed_out: bool,
    ) -> Result<Value> {
        let journal = parent
            .journal
            .as_ref()
            .ok_or_else(|| failure("TASK_JOURNAL_UNAVAILABLE"))?;
        let repository = self.repository().await?;
        let mut results = Vec::new();
        let mut preview_budget = 32_768usize;
        for task in &tasks {
            let mut result = json!({"taskId":task.task_id,"state":task.state,"outputRef":null,"summaryPreview":"","evidence":[],"verification":[]});
            if let Some(artifact) = repository.latest_artifact(&task.task_id, "task-output")? {
                result["outputRef"] = artifact.reference.clone();
                match journal.read_blob(&artifact.reference).await {
                    Ok(bytes) => {
                        let report: Value = serde_json::from_slice(&bytes)
                            .map_err(|_| failure("INVALID_TASK_OUTPUT"))?;
                        let summary = report["summary"].as_str().unwrap_or("");
                        let preview = bounded_preview(summary, preview_budget.min(4096));
                        preview_budget =
                            preview_budget.saturating_sub(preview.encode_utf16().count());
                        result["summaryTruncated"] = json!(preview.len() != summary.len());
                        result["summaryPreview"] = json!(preview);
                        result["evidence"] = json!(
                            report["evidence"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .take(16)
                                .filter(|reference| serde_json::to_vec(reference)
                                    .is_ok_and(|bytes| bytes.len() <= 2048))
                                .cloned()
                                .collect::<Vec<_>>()
                        );
                        result["verification"] = json!(
                            report["verification"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .take(8)
                                .filter(|verification| serde_json::to_vec(verification)
                                    .is_ok_and(|bytes| bytes.len() <= 4096))
                                .cloned()
                                .collect::<Vec<_>>()
                        );
                        result["evidenceTruncated"] = json!(
                            report["evidence"].as_array().map(Vec::len).unwrap_or(0)
                                != result["evidence"].as_array().map(Vec::len).unwrap_or(0)
                        );
                        result["verificationTruncated"] = json!(
                            report["verification"].as_array().map(Vec::len).unwrap_or(0)
                                != result["verification"].as_array().map(Vec::len).unwrap_or(0)
                        );
                        result["completionMeans"] = json!(
                            "executor-reported; only the listed real verification receipts establish tests passed"
                        );
                    }
                    Err(error) => {
                        result["outputUnavailable"] = json!(error.code);
                    }
                }
            }
            results.push(result);
        }
        Ok(
            json!({"tasks":tasks,"results":results,"timedOut":timed_out,"allCompleted":tasks.iter().all(|task|task.state==TaskState::Completed || task.state==TaskState::Cancelled && task.stop_reason.as_deref()==Some("superseded-by-user-steering"))}),
        )
    }
    async fn read_result(&self, journal: &Arc<TaskJournal>, args: &Value) -> Result<Value> {
        let reference = &args["reference"];
        let bytes = journal.read_blob(reference).await?;
        let text = String::from_utf8(bytes).map_err(|_| failure("TASK_ARTIFACT_NOT_TEXT"))?;
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(16_384).clamp(1, 65_536) as usize;
        let (mut units, mut start, mut end) = (0usize, text.len(), text.len());
        let mut actual_offset = text.encode_utf16().count();
        for (byte, character) in text.char_indices() {
            if units >= offset && start == text.len() {
                start = byte;
                actual_offset = units;
            }
            if start != text.len() && units >= actual_offset.saturating_add(limit) {
                end = byte;
                break;
            }
            units += character.len_utf16();
        }
        let total = text.encode_utf16().count();
        let rendered = &text[start..end];
        let length = rendered.encode_utf16().count();
        Ok(
            json!({"text":rendered,"reference":reference,"unit":"utf16","offset":actual_offset,"endOffset":actual_offset+length,"totalUnits":total,"hasMore":end<text.len()}),
        )
    }
    async fn schedule_root_recovery(self: &Arc<Self>, task: &TaskRecord) -> Result<()> {
        if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
            return Err(failure("TASK_HUB_STOPPING"));
        }
        let mut root = task.clone();
        let repository = self.repository().await?;
        for _ in 0..64 {
            if let Some(parent) = root.parent_task_id.as_deref() {
                root = repository.load(parent)?;
            } else {
                break;
            }
        }
        {
            let mut restoring = self
                .restoring_roots
                .lock()
                .map_err(|_| failure("TASK_QUEUE_LOCK_FAILED"))?;
            if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
                return Err(failure("TASK_HUB_STOPPING"));
            }
            if !restoring.insert(root.task_id.clone()) {
                return Ok(());
            }
        }
        let hub = self.clone();
        tokio::spawn(async move {
            let id = root.task_id.clone();
            if let Err(error) = hub.clone().restore_root(root).await {
                let _ = hub.record_worker_failure(&id, &error).await;
                hub.changes.notify_waiters();
            }
            if let Ok(mut restoring) = hub.restoring_roots.lock() {
                restoring.remove(&id);
            }
        });
        Ok(())
    }
    fn restore_root(
        self: Arc<Self>,
        task: TaskRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
        Box::pin(async move {
            if task.state.is_terminal() || task.state == TaskState::Paused {
                return Ok(());
            }
            let repository = self.repository().await?;
            self.wait_existing_lease(&task.task_id, None).await?;
            if let Some(runtime) = self.runtime.upgrade() {
                runtime.wait_task_delivery_boundary(&task.task_id).await;
            }
            let task = repository.load(&task.task_id)?;
            if task.state.is_terminal() || task.state == TaskState::Paused {
                return Ok(());
            }
            self.validate_binding(&task).await?;
            let mut tree = vec![task.clone()];
            let mut index = 0;
            while index < tree.len() {
                tree.extend(repository.children(&tree[index].task_id)?);
                index += 1;
            }
            let mut unknown = false;
            for member in &tree {
                // An unresolved HTTP reservation also protects shared cost, even without a write.
                // 未核验的 HTTP 预约同样保护共享成本，即使它没有执行文件写入。
                unknown |= repository
                    .operations(&member.task_id)?
                    .iter()
                    .any(|operation| {
                        matches!(
                            operation.state,
                            OperationState::Unknown | OperationState::Dispatched
                        ) && operation.intent.effect != OperationEffect::Observation
                    })
                    || repository
                        .model_reservations(&member.task_id)?
                        .iter()
                        .any(|reservation| {
                            matches!(
                                reservation.state,
                                ModelReservationState::Unknown | ModelReservationState::Reserved
                            )
                        });
            }
            if unknown {
                repository.mark_unstarted_wait(
                    &task.task_id,
                    task.version,
                    TaskState::WaitingVerification,
                    "external-effect-or-cost-needs-verification",
                )?;
                self.changes.notify_waiters();
                return Ok(());
            }
            let reference = task
                .input_reference
                .as_ref()
                .ok_or_else(|| failure("TASK_INPUT_MISSING"))?;
            let (input, original, _) =
                super::gateway_task_input::read(&self.config.data_root, reference).await?;
            let runtime = self
                .runtime
                .upgrade()
                .ok_or_else(|| failure("TASK_RUNTIME_UNAVAILABLE"))?;
            let parent = runtime.begin_recovered(input.clone(), original).await?;
            if let Some(journal) = parent.journal.as_ref() {
                let context = runtime
                    .tool_context(
                        &task.scope.conversation_id,
                        task.scope.permission_snapshot["permissionMode"]
                            .as_str()
                            .unwrap_or("smart"),
                        input["message"].as_str().unwrap_or(""),
                        parent.cancellation.subscribe(),
                    )
                    .await?;
                // Already accepted children retain task identity and their original context references.
                // 已接受的子任务保持稳定身份及原始上下文引用，重启不能只恢复父任务后永久等待。
                for child in repository.children(&task.task_id)? {
                    if child.state.is_terminal() || child.state == TaskState::Paused {
                        continue;
                    }
                    let reference = child
                        .input_reference
                        .as_ref()
                        .filter(|value| value["kind"] == "task-child-input")
                        .ok_or_else(|| failure("TASK_CHILD_INPUT_MISSING"))?;
                    let bytes = journal.read_blob(&reference["contextReference"]).await?;
                    let child_input = serde_json::from_slice(&bytes)
                        .map_err(|_| failure("INVALID_TASK_INPUT"))?;
                    self.queue_child(parent.clone(), child, context.clone(), child_input)
                        .await?;
                }
            }
            self.changes.notify_waiters();
            Ok(())
        })
    }
    async fn wait_existing_lease(
        &self,
        id: &str,
        mut cancellation: Option<watch::Receiver<bool>>,
    ) -> Result<()> {
        loop {
            if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
                return Err(failure("TASK_HUB_STOPPING"));
            }
            let repository = self.repository().await?;
            let Some(attempt) = repository.current_attempt(id)? else {
                return Ok(());
            };
            if attempt.ended_at_ms.is_some() {
                return Ok(());
            }
            let remaining = attempt
                .lease_expires_at_ms
                .saturating_sub(chrono::Utc::now().timestamp_millis());
            if remaining <= 0 {
                return Ok(());
            }
            let notified = self.changes.notified();
            let delay = Duration::from_millis((remaining as u64 + 25).min(1000));
            if let Some(receiver) = cancellation.as_mut() {
                tokio::select! {_=tokio::time::sleep(delay)=>{},_=notified=>{},_=crate::tools::gateway::wait_for_cancellation(receiver)=>return Err(failure("ABORT_ERR"))}
            } else {
                tokio::select! {_=tokio::time::sleep(delay)=>{},_=notified=>{}}
            }
        }
    }
    /// Trash deletion stops execution but keeps restorable task/chat records and evidence.
    /// 回收站删除停止执行，但保留可恢复的任务、聊天记录和证据。
    pub async fn reconcile_deleted_chats(self: &Arc<Self>, catalog: &Value) -> Result<Value> {
        let mut affected = HashSet::new();
        for item in catalog["Tombstones"].as_array().into_iter().flatten() {
            let conversation = item["Id"]
                .as_str()
                .ok_or_else(|| failure("INVALID_TASK_CLEANUP"))?
                .to_owned();
            let targets = self
                .store(move |repository| repository.cleanup_targets(&conversation))
                .await?;
            for task in targets {
                self.cancel_for_deletion(&task).await?;
                affected.insert(task.task_id);
            }
        }
        Ok(json!({"cancelledTaskIds":affected,"recordsPreserved":true}))
    }

    async fn cancel_for_deletion(self: &Arc<Self>, task: &TaskRecord) -> Result<()> {
        let repository = self.repository().await?;
        for _ in 0..3 {
            let current = repository.load(&task.task_id)?;
            if current.state.is_terminal() {
                break;
            }
            match self
                .control(&current.task_id, current.version, TaskControl::Cancel)
                .await
            {
                Ok(_) => break,
                Err(error) if error.code == "TASK_CONFLICT" => continue,
                Err(error) => return Err(error),
            }
        }
        if let Ok(runners) = self.runners.lock() {
            if let Some(runner) = runners.get(&task.task_id) {
                runner.cancel.send_replace(true);
            }
        }
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.cancel_task_worker(&task.task_id).await;
        }
        self.changes.notify_waiters();
        Ok(())
    }

    /// Permanent deletion is separate from catalog trash and never removes generated workspace files.
    /// 永久删除与目录回收站分开，绝不删除用户工作目录里生成的文件。
    pub async fn purge_conversation(self: &Arc<Self>, conversation_id: &str) -> Result<Value> {
        let conversation = conversation_id.to_owned();
        let targets = self
            .store(move |repository| repository.cleanup_targets(&conversation))
            .await?;
        for task in &targets {
            self.cancel_for_deletion(task).await?;
        }
        let started = tokio::time::Instant::now();
        loop {
            let mut active = false;
            if let Ok(runners) = self.runners.lock() {
                active = targets.iter().any(|task| {
                    runners
                        .get(&task.task_id)
                        .is_some_and(|runner| !runner.handle.is_finished())
                });
            }
            if let Some(runtime) = self.runtime.upgrade() {
                for task in &targets {
                    if task.parent_task_id.is_none()
                        && runtime
                            .run_status(&task.scope.conversation_id, &task.request_id)
                            .await
                            .is_some_and(|status| status["status"] == "running")
                    {
                        active = true;
                    }
                }
            }
            let ids: Vec<String> = targets.iter().map(|task| task.task_id.clone()).collect();
            active |= self
                .store(move |repository| {
                    for id in ids {
                        if repository.current_attempt(&id)?.is_some_and(|attempt| {
                            attempt.ended_at_ms.is_none()
                                && attempt.lease_expires_at_ms
                                    > chrono::Utc::now().timestamp_millis()
                        }) {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                })
                .await?;
            if !active {
                break;
            }
            if started.elapsed() >= Duration::from_secs(60) {
                return Err(failure("TASK_CLEANUP_WAITING_FOR_EXECUTOR"));
            }
            tokio::select! {_=self.changes.notified()=>{},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
        let conversation = conversation_id.to_owned();
        let plan = self
            .store(move |repository| repository.purge_conversation(&conversation))
            .await?;
        let cleanup_id = plan.cleanup_id.clone();
        match self
            .store(move |repository| repository.retry_cleanup_files(&cleanup_id))
            .await
        {
            Ok(_) => Ok(
                json!({"deletedTaskIds":plan.task_ids,"cleanupId":plan.cleanup_id,"filesRemoved":plan.files.len(),"cleanupPending":false}),
            ),
            Err(error) => Ok(
                json!({"deletedTaskIds":plan.task_ids,"cleanupId":plan.cleanup_id,"cleanupPending":true,"errorCode":error.code}),
            ),
        }
    }
    pub async fn recover(self: &Arc<Self>) -> Result<Value> {
        let repository = self.repository().await?;
        // File-cleanup markers survive metadata deletion; retry them before accepting old work.
        // 元数据删除后的文件清理标记仍存活；恢复先重试它们，不能丢失待清理路径。
        let pending = repository.pending_cleanups(512)?;
        for plan in pending {
            let id = plan.cleanup_id;
            let _ = self
                .store(move |repository| repository.retry_cleanup_files(&id))
                .await;
        }
        let mut after = None;
        let mut scheduled = 0;
        loop {
            let page = repository.list_unfinished_after(after.as_deref(), 256)?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|task| task.task_id.clone());
            for task in page {
                if task.parent_task_id.is_none() && task.state != TaskState::Paused {
                    self.schedule_root_recovery(&task).await?;
                    scheduled += 1;
                }
            }
        }
        Ok(json!({"scheduled":scheduled,"source":"persistent-task-index"}))
    }
    pub fn active_workers(&self) -> Result<usize> {
        let runners = self
            .runners
            .lock()
            .map_err(|_| failure("TASK_RUNNER_LOCK_FAILED"))?
            .values()
            .filter(|runner| !runner.handle.is_finished())
            .count();
        let restoring = self
            .restoring_roots
            .lock()
            .map_err(|_| failure("TASK_QUEUE_LOCK_FAILED"))?
            .len();
        Ok(runners + restoring)
    }

    /// Migration waits for owned workers to settle journals, rather than treating cancellation as completion.
    /// 迁移等待自有 Worker 实际结算日志，不能把发送取消信号当作完成。
    pub async fn quiesce(self: &Arc<Self>) -> Result<()> {
        self.shutdown().await;
        while self.active_workers()? != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(())
    }

    pub async fn shutdown(self: &Arc<Self>) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(runners) = self.runners.lock() {
            for runner in runners.values() {
                runner.cancel.send_replace(true);
            }
        }
        self.changes.notify_waiters();
    }
}
fn failure(code: &str) -> GatewayError {
    GatewayError::new(
        code,
        "任务步骤当前无法执行；已取得结果和原始回执继续保留。",
        if code == "ABORT_ERR" { 499 } else { 409 },
    )
}
fn bounded_preview(text: &str, limit_utf16: usize) -> String {
    let mut units = 0usize;
    text.chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= limit_utf16
        })
        .collect()
}
