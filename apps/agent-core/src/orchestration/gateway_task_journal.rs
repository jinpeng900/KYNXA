//! Thin I/O bridge from the existing request loop to its one persistent task repository.
//! 现有请求循环到唯一任务仓储的薄 I/O 接线；不复制工具原文、模型事实或资源账本。

use crate::gateway_contract::GatewayError;
use crate::storage::{managed_files, task_repository::*};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Notify};

const FLUSH_BYTES: usize = 8 * 1024;
const MAX_PENDING_BYTES: usize = 384 * 1024;
const MAX_PENDING_EVENTS: usize = 4096;
const MAX_BLOB_BYTES: u64 = 32 * 1024 * 1024;
type Result<T> = std::result::Result<T, GatewayError>;
type PendingEvent = (String, String, Value);

#[derive(Default)]
struct EventBuffer {
    events: Vec<PendingEvent>,
    bytes: usize,
}

/// An attempt owns this bridge; stable work identity stays in TaskRepository across replacement.
/// 尝试拥有本接线；更换执行者时稳定工作身份和已取得回执继续由 TaskRepository 保存。
pub struct TaskJournal {
    repository: Arc<TaskRepository>,
    root: PathBuf,
    token: AttemptToken,
    input_reference: Option<Value>,
    pending: Mutex<EventBuffer>,
    flush_serial: AsyncMutex<()>,
    scheduled: AtomicBool,
    wake_flush: Notify,
    last_flush_error: Mutex<Option<GatewayError>>,
    released: AtomicBool,
    renewal: Mutex<Option<tokio::task::JoinHandle<()>>>,
    renewal_stop: Arc<Notify>,
}

impl TaskJournal {
    pub fn begin(
        root: &Path,
        input: &NewTask,
        executor_id: &str,
        lease_ms: u64,
    ) -> Result<Arc<Self>> {
        let repository = Arc::new(TaskRepository::open(root)?);
        let task = repository.accept_task(input)?;
        Self::claim(root, repository, &task, executor_id, lease_ms)
    }
    pub fn start_existing(
        root: &Path,
        task: &TaskRecord,
        executor_id: &str,
        lease_ms: u64,
    ) -> Result<Arc<Self>> {
        let repository = Arc::new(TaskRepository::open(root)?);
        let stored = repository.load(&task.task_id)?;
        if stored.version != task.version {
            return Err(GatewayError::new(
                "TASK_CONFLICT",
                "Task changed before worker attachment. / Worker 接入前任务已变化。",
                409,
            ));
        }
        Self::claim(root, repository, &stored, executor_id, lease_ms)
    }
    fn claim(
        root: &Path,
        repository: Arc<TaskRepository>,
        task: &TaskRecord,
        executor_id: &str,
        lease_ms: u64,
    ) -> Result<Arc<Self>> {
        let attempt =
            repository.start_attempt(&task.task_id, task.version, executor_id, lease_ms)?;
        let journal = Arc::new(Self {
            repository,
            root: root.to_owned(),
            token: attempt.token,
            input_reference: task.input_reference.clone(),
            pending: Mutex::new(EventBuffer::default()),
            flush_serial: AsyncMutex::new(()),
            scheduled: AtomicBool::new(false),
            wake_flush: Notify::new(),
            last_flush_error: Mutex::new(None),
            released: AtomicBool::new(false),
            renewal: Default::default(),
            renewal_stop: Arc::new(Notify::new()),
        });
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&journal);
            let task = runtime.spawn(Self::renew_while_owned(
                weak,
                lease_ms,
                journal.renewal_stop.clone(),
            ));
            *journal.renewal.lock().map_err(|_| {
                GatewayError::new("TASK_HEARTBEAT_CLEANUP_FAILED", "任务心跳状态不可用。", 503)
            })? = Some(task);
        }
        Ok(journal)
    }
    pub fn repository(&self) -> &Arc<TaskRepository> {
        &self.repository
    }
    pub fn token(&self) -> &AttemptToken {
        &self.token
    }
    pub fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }
    pub fn task_id(&self) -> &str {
        &self.token.task_id
    }
    pub fn input_reference(&self) -> Option<&Value> {
        self.input_reference.as_ref()
    }
    pub fn latest_checkpoint(&self) -> Result<Option<TaskCheckpoint>> {
        Ok(self.repository.latest_checkpoint(self.task_id())?)
    }
    pub fn load_current(&self) -> Result<TaskRecord> {
        Ok(self.repository.load(self.task_id())?)
    }
    pub fn check_dispatch_fence(&self) -> Result<TaskRecord> {
        Ok(self.repository.check_attempt(&self.token)?)
    }

    async fn renew_while_owned(weak: Weak<Self>, lease_ms: u64, stop: Arc<Notify>) {
        let normal_delay = Duration::from_millis((lease_ms / 3).clamp(1_000, 5_000));
        let mut delay = normal_delay;
        let mut retry_delay = Duration::from_millis(100);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = stop.notified() => break,
            }
            let Some(journal) = weak.upgrade() else { break };
            if journal.is_released() {
                break;
            }
            let repository = journal.repository.clone();
            let token = journal.token.clone();
            // Lease renewal performs no model call and never revives a paused/cancelled task.
            // 租约续期不调用模型，也不让暂停、取消的任务复活。
            let renewed =
                tokio::task::spawn_blocking(move || repository.renew_attempt(&token, lease_ms))
                    .await;
            match renewed {
                Ok(Ok(_)) => {
                    delay = normal_delay;
                    retry_delay = Duration::from_millis(100);
                }
                Ok(Err(error)) if error.code() == "TASK_STORAGE_FAILED" => {
                    // Retry a transient writer failure while the repository still owns lease validation.
                    // 临时存储失败继续尝试续租；仓储仍核验有效期与执行权，不续活过期或已取消尝试。
                    delay = retry_delay;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                }
                Err(_) => {
                    delay = retry_delay;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                }
                Ok(Err(_)) => break,
            }
        }
    }

    /// Queue compact controls, then batch disk writes outside streaming/model execution.
    /// 小型控制事件先入队；流式和模型执行路径不随每个 token 同步写 SQLite。
    pub fn emit_control(self: &Arc<Self>, kind: &str, payload: &Value) -> Result<()> {
        if self.is_released() {
            return Err(GatewayError::new(
                "TASK_JOURNAL_CLOSED",
                "Task attempt is already released. / 任务尝试已释放。",
                409,
            ));
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            GatewayError::new(
                "TASK_RUNTIME_UNAVAILABLE",
                "Task event runtime is unavailable. / 任务事件运行时不可用。",
                500,
            )
        })?;
        let bytes = serde_json::to_vec(payload)
            .map_err(|_| {
                GatewayError::new(
                    "INVALID_TASK_EVENT",
                    "Task event is invalid. / 任务事件无效。",
                    400,
                )
            })?
            .len();
        if bytes > 48 * 1024 {
            return Err(GatewayError::new(
                "TASK_EVENT_REFERENCE_REQUIRED",
                "Large content requires an original artifact or tool-result reference. / 大正文须使用产物或工具原文引用。",
                400,
            ));
        }
        let is_full = {
            let mut pending = self.pending.lock().map_err(|_| {
                GatewayError::new(
                    "TASK_EVENT_BUFFER_FAILED",
                    "Task event buffer is unavailable. / 任务事件缓存不可用。",
                    500,
                )
            })?;
            if pending.events.len() >= MAX_PENDING_EVENTS
                || pending.bytes.saturating_add(bytes) > MAX_PENDING_BYTES
            {
                return Err(GatewayError::new(
                    "TASK_EVENT_BACKPRESSURE",
                    "Task event persistence is delayed. / 任务事件持久化出现延迟。",
                    503,
                ));
            }
            pending.events.push((
                uuid::Uuid::new_v4().to_string(),
                kind.to_owned(),
                payload.clone(),
            ));
            pending.bytes += bytes;
            pending.bytes >= FLUSH_BYTES
        };
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            let journal = self.clone();
            runtime.spawn(journal.pump());
        }
        if is_full {
            self.wake_flush.notify_one();
        }
        Ok(())
    }
    async fn pump(self: Arc<Self>) {
        loop {
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(100))=>{},_=self.wake_flush.notified()=>{}}
            let succeeded = self.flush().await.is_ok();
            // Reset scheduling under the buffer lock so an arriving last event cannot be stranded.
            // 在缓存锁内复位调度标志，避免最后一个晚到事件在后台等待时漏掉定期持久化。
            let Ok(pending) = self.pending.lock() else {
                self.scheduled.store(false, Ordering::Release);
                break;
            };
            if !succeeded || pending.events.is_empty() {
                self.scheduled.store(false, Ordering::Release);
                break;
            }
        }
    }
    pub async fn flush(&self) -> Result<()> {
        let _serial = self.flush_serial.lock().await;
        let batch = {
            let mut pending = self.pending.lock().map_err(|_| {
                GatewayError::new(
                    "TASK_EVENT_BUFFER_FAILED",
                    "Task event buffer is unavailable. / 任务事件缓存不可用。",
                    500,
                )
            })?;
            pending.bytes = 0;
            std::mem::take(&mut pending.events)
        };
        if batch.is_empty() {
            return Ok(());
        }
        let repository = self.repository.clone();
        let token = self.token.clone();
        let dispatch = batch.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Stable event IDs make a partly committed batch safe to replay after a failure.
            // 稳定事件 ID 允许部分批次提交后安全重放，单次事务仍遵守仓储的 512 条上限。
            for portion in dispatch.chunks(512) {
                repository.append_events(&token, portion)?;
            }
            Ok::<(), crate::storage::ContractError>(())
        })
        .await
        .map_err(|_| {
            GatewayError::new(
                "TASK_EVENT_FLUSH_FAILED",
                "Task event writer ended unexpectedly. / 任务事件写入者意外退出。",
                500,
            )
        })
        .and_then(|result| result.map(|_| ()).map_err(GatewayError::from));
        if let Err(error) = &result {
            let mut pending = self.pending.lock().map_err(|_| error.clone())?;
            let mut restored = batch;
            restored.append(&mut pending.events);
            pending.events = restored;
            pending.bytes = pending
                .events
                .iter()
                .map(|(_, _, payload)| {
                    serde_json::to_vec(payload)
                        .map(|raw| raw.len())
                        .unwrap_or(0)
                })
                .sum();
        }
        if let Ok(mut saved) = self.last_flush_error.lock() {
            *saved = result.as_ref().err().cloned();
        }
        result
    }
    pub fn last_persistence_error(&self) -> Option<GatewayError> {
        self.last_flush_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    /// Only task-owned work context/logs use blobs; existing chat/tool originals stay referenced.
    /// 仅任务自己的工作上下文、日志使用正文对象；已有聊天/工具原文继续按正式引用回读。
    pub async fn save_task_blob(&self, kind: &str, content: &Value) -> Result<TaskArtifact> {
        let bytes = serde_json::to_vec(content).map_err(|_| {
            GatewayError::new(
                "INVALID_TASK_ARTIFACT",
                "Task artifact is invalid. / 任务产物无效。",
                400,
            )
        })?;
        self.store_blob_bytes(kind, &bytes).await
    }
    pub async fn save_blob(&self, kind: &str, bytes: &[u8]) -> Result<Value> {
        Ok(self.store_blob_bytes(kind, bytes).await?.reference)
    }
    async fn store_blob_bytes(&self, kind: &str, bytes: &[u8]) -> Result<TaskArtifact> {
        if !matches!(
            kind,
            "child-context"
                | "worker-log"
                | "task-output"
                | "task-plan"
                | "task-checkpoint"
                | "document-progress"
        ) {
            return Err(GatewayError::new(
                "INVALID_TASK_ARTIFACT_KIND",
                "Use existing chat and tool archives for their original bodies. / 聊天和工具正文须使用现有正式归档。",
                400,
            ));
        }
        self.repository.check_receipt_attempt(&self.token)?;
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(GatewayError::new(
                "TASK_ARTIFACT_TOO_LARGE",
                "Task artifact exceeds the bounded blob capacity. / 任务产物超过有界正文容量。",
                413,
            ));
        }
        let digest = managed_files::digest(&bytes);
        let artifact_id = format!("{kind}-{digest}");
        let relative = format!("Agent/task-artifacts/{}/{artifact_id}.json", self.task_id());
        let root = self.root.clone();
        let path = root.join(&relative);
        let write = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            managed_files::atomic_write(&root, &path, &write, MAX_BLOB_BYTES)
        })
        .await
        .map_err(|_| {
            GatewayError::new(
                "TASK_ARTIFACT_WRITE_FAILED",
                "Task artifact writer ended unexpectedly. / 任务产物写入者意外退出。",
                500,
            )
        })??;
        let artifact = TaskArtifact {
            artifact_id: artifact_id.clone(),
            kind: kind.into(),
            reference: json!({"kind":"task-blob","taskId":self.task_id(),"artifactId":artifact_id,"relativePath":relative,"sha256":digest,"bytes":bytes.len()}),
            sha256: digest,
            size_bytes: bytes.len() as u64,
            source_version: None,
        };
        Ok(self.repository.save_artifact(&self.token, &artifact)?)
    }
    pub async fn read_blob(&self, reference: &Value) -> Result<Vec<u8>> {
        let failure = || {
            GatewayError::new(
                "INVALID_TASK_ARTIFACT_REFERENCE",
                "Task artifact reference is invalid or outside the task tree. / 任务产物引用无效或超出任务树。",
                409,
            )
        };
        if reference["kind"] != "task-blob" {
            return Err(failure());
        }
        let owner_id = reference["taskId"].as_str().ok_or_else(failure)?;
        let artifact_id = reference["artifactId"].as_str().ok_or_else(failure)?;
        let owner = self.repository.load(owner_id)?;
        let current = self.load_current()?;
        if owner.scope.project_id != current.scope.project_id
            || owner.scope.permission_snapshot != current.scope.permission_snapshot
        {
            return Err(failure());
        }
        let root_id = |mut task: TaskRecord| -> Result<String> {
            for _ in 0..64 {
                if let Some(parent) = task.parent_task_id.as_deref() {
                    task = self.repository.load(parent)?;
                } else {
                    return Ok(task.task_id);
                }
            }
            Err(failure())
        };
        if root_id(owner)? != root_id(current)? {
            return Err(failure());
        }
        let artifact = self
            .repository
            .artifacts(owner_id)?
            .into_iter()
            .find(|artifact| {
                artifact.artifact_id == artifact_id && artifact.reference == *reference
            })
            .ok_or_else(failure)?;
        let relative = format!("Agent/task-artifacts/{owner_id}/{artifact_id}.json");
        if reference["relativePath"].as_str() != Some(relative.as_str())
            || artifact.size_bytes > MAX_BLOB_BYTES
        {
            return Err(failure());
        }
        let root = self.root.clone();
        let path = root.join(relative);
        let expected = artifact.sha256;
        let expected_size = artifact.size_bytes;
        let bytes =
            tokio::task::spawn_blocking(move || managed_files::read(&root, &path, MAX_BLOB_BYTES))
                .await
                .map_err(|_| {
                    GatewayError::new(
                        "TASK_ARTIFACT_READ_FAILED",
                        "Task artifact reader ended unexpectedly. / 任务产物读取者意外退出。",
                        500,
                    )
                })??
                .ok_or_else(failure)?;
        if bytes.len() as u64 != expected_size || managed_files::digest(&bytes) != expected {
            return Err(failure());
        }
        Ok(bytes)
    }
    pub async fn save_checkpoint(&self, checkpoint: &TaskCheckpoint) -> Result<TaskCheckpoint> {
        self.flush().await?;
        Ok(self.repository.save_checkpoint(&self.token, checkpoint)?)
    }
    /// A root work snapshot is private task state, bound to the original request, scope and provider identity.
    /// 根工作快照属于私有任务状态，绑定原请求、权限范围和供应商身份，不作为公开工具证据。
    pub async fn save_root_work_context(
        &self,
        request_id: &str,
        request_fingerprint: &str,
        origin: &Value,
        context: &Value,
    ) -> Result<Value> {
        let task = self.repository.check_receipt_attempt(&self.token)?;
        if task.parent_task_id.is_some() || task.request_id != request_id {
            return Err(GatewayError::new(
                "INVALID_ROOT_WORK_CONTEXT",
                "根任务上下文身份不一致。",
                409,
            ));
        }
        let envelope = json!({"format":"kynxa-root-work-context","version":1,"taskId":task.task_id,
            "requestId":request_id,"requestFingerprint":request_fingerprint,"scope":task.scope,"origin":origin,"context":context});
        let bytes = serde_json::to_vec(&envelope).map_err(|_| {
            GatewayError::new("INVALID_ROOT_WORK_CONTEXT", "根任务上下文无法编码。", 409)
        })?;
        self.save_blob("task-checkpoint", &bytes).await
    }
    /// Legacy checkpoints have no sidecar; malformed or foreign snapshots cannot silently become trusted context.
    /// 旧检查点没有侧车时安全降级；损坏或跨任务快照不能静默升级为可信上下文。
    pub async fn read_root_work_context(
        &self,
        assistant_message_id: &str,
        request_id: &str,
        request_fingerprint: &str,
        origin: &Value,
    ) -> Result<Option<(Value, Value)>> {
        let invalid = || {
            GatewayError::new(
                "INVALID_ROOT_WORK_CONTEXT",
                "根任务上下文来源、版本或范围不一致。",
                409,
            )
        };
        let task = self.load_current()?;
        if task.parent_task_id.is_some() || task.request_id != request_id {
            return Err(invalid());
        }
        let Some(checkpoint) = self.latest_checkpoint()? else {
            return Ok(None);
        };
        let checkpoint = &checkpoint.context_reference;
        let reference = &checkpoint["summaryReference"];
        if reference["kind"] != "task-blob" {
            return Ok(None);
        }
        if checkpoint["kind"] != "conversation-context"
            || checkpoint["conversationId"] != task.scope.conversation_id
            || checkpoint["assistantMessageId"] != assistant_message_id
            || checkpoint["requestId"] != request_id
            || reference["taskId"] != task.task_id
        {
            return Err(invalid());
        }
        let envelope: Value =
            serde_json::from_slice(&self.read_blob(reference).await?).map_err(|_| invalid())?;
        if envelope["format"] != "kynxa-root-work-context"
            || envelope["version"] != 1
            || envelope["taskId"] != task.task_id
            || envelope["requestId"] != request_id
            || envelope["requestFingerprint"] != request_fingerprint
            || envelope["origin"] != *origin
            || envelope["scope"] != serde_json::to_value(&task.scope).map_err(|_| invalid())?
            || !envelope["context"].is_object()
        {
            return Err(invalid());
        }
        Ok(Some((envelope["context"].clone(), reference.clone())))
    }
    pub async fn checkpoint_from_chat(
        &self,
        assistant_message_id: &str,
        request_id: &str,
        summary_reference: Option<Value>,
        next_step: Value,
    ) -> Result<TaskCheckpoint> {
        self.flush().await?;
        let task = self.load_current()?;
        let checkpoint = TaskCheckpoint {
            checkpoint_id: uuid::Uuid::new_v4().to_string(),
            through_event_sequence: task.event_sequence,
            context_reference: json!({"kind":"conversation-context","conversationId":task.scope.conversation_id,"assistantMessageId":assistant_message_id,"requestId":request_id,"summaryReference":summary_reference,"inputReference":self.input_reference}),
            next_step,
            source_versions: vec![],
        };
        Ok(self.repository.save_checkpoint(&self.token, &checkpoint)?)
    }
    pub fn reserve_model_request(&self, id: &str) -> Result<ModelRequestReservation> {
        Ok(self.repository.reserve_model_request(&self.token, id)?)
    }
    pub fn settle_model_request(
        &self,
        id: &str,
        dispatched: bool,
        usage: &TaskUsage,
    ) -> Result<ModelRequestReservation> {
        self.settle_model_request_with_provider_usage(id, dispatched, usage, None)
    }
    pub fn settle_model_request_with_provider_usage(
        &self, id: &str, dispatched: bool, usage: &TaskUsage,
        provider_usage: Option<crate::storage::task_repository::ProviderTokenUsage>,
    ) -> Result<ModelRequestReservation> {
        // The real I/O outcome precedes accounting, so a crash between the two retains verifiable settlement facts.
        // 真实 I/O 结果先于计量结算落盘，使两步之间崩溃后仍有可核验的精确结果。
        let mut receipt = json!({"reservationId":id,"attemptId":self.token.attempt_id,"dispatched":dispatched,"usage":usage});
        if let Some(provider) = &provider_usage { receipt["providerUsage"] = json!(provider); }
        self.repository.append_events(&self.token, &[(
            format!("model-receipt:{id}"), "task.model.receipt".into(),
            receipt,
        )])?;
        Ok(self
            .repository
            .settle_model_request_with_provider_usage(&self.token, id, dispatched, usage, provider_usage)?)
    }
    pub fn record_operation(&self, intent: &OperationIntent) -> Result<TaskOperation> {
        Ok(self.repository.record_operation(&self.token, intent)?)
    }
    pub fn dispatch_operation(&self, id: &str) -> Result<TaskOperation> {
        Ok(self.repository.dispatch_operation(&self.token, id)?)
    }
    pub fn finish_operation(
        &self,
        id: &str,
        state: OperationState,
        result_reference: Option<Value>,
        verification_reference: Option<Value>,
    ) -> Result<TaskOperation> {
        let operation = self.repository.operation(self.task_id(), id)?
            .ok_or_else(|| GatewayError::new("TASK_OPERATION_NOT_FOUND", "待结算操作不存在。", 404))?;
        self.repository.append_events(&self.token, &[(
            uuid::Uuid::new_v4().to_string(), "task.operation.receipt".into(),
            json!({"intent":operation.intent,"state":state,"resultReference":result_reference,"verificationReference":verification_reference}),
        )])?;
        Ok(self.repository.finish_operation(
            &self.token,
            id,
            state,
            result_reference,
            verification_reference,
        )?)
    }

    /// Settle only native I/O evidence already persisted before interruption; this never calls a model or repeats a tool.
    /// 只结算中断前已落盘的原生 I/O 证据，不调用模型，也不重做工具操作。
    pub async fn reconcile_receipts(&self) -> Result<Value> {
        let task = self.load_current()?;
        let events = self.repository.recovery_receipts(self.task_id())?;
        let mut resolved_operations = 0u64;
        let mut resolved_reservations = 0u64;
        let mut pending = Vec::new();
        let mut acknowledged = Vec::new();
        for reservation in self.repository.model_reservations(self.task_id())?.into_iter()
            .filter(|reservation| matches!(reservation.state, ModelReservationState::Unknown | ModelReservationState::Reserved))
        {
            let witness = events.iter().rev().find(|event| event.kind == "task.model.receipt"
                && event.payload["reservationId"] == reservation.reservation_id
                && event.payload["attemptId"] == reservation.attempt_id);
            if let Some(witness) = witness {
                if let (Some(dispatched), Ok(usage)) = (witness.payload["dispatched"].as_bool(),
                    serde_json::from_value::<TaskUsage>(witness.payload["usage"].clone()))
                {
                    let provider_usage = witness.payload.get("providerUsage").filter(|value| !value.is_null())
                        .map(|value| serde_json::from_value(value.clone())).transpose()
                        .map_err(|_| GatewayError::new("TASK_MODEL_RECEIPT_INVALID", "供应商用量回执格式无效，保留原预约。", 409))?;
                    self.repository.reconcile_model_request_with_provider_usage(&self.token, &reservation.reservation_id, dispatched, &usage, provider_usage,
                        json!({"kind":"task-event","taskId":self.task_id(),"eventId":witness.event_id,"sequence":witness.sequence}))?;
                    resolved_reservations += 1;
                    continue;
                }
            }
            if reservation.is_unknown_acknowledged() {
                acknowledged.push(json!({"taskId":self.task_id(),"reservationId":reservation.reservation_id,"costUnknown":true}));
                continue;
            }
            pending.push(json!({"taskId":self.task_id(),"reservationId":reservation.reservation_id,
                "code":"MODEL_DISPATCH_EVIDENCE_REQUIRED","message":"模型请求尚无可核验的派发与用量回执。需核对供应商请求记录；不会假定未发送、伪造用量或重新发送原请求。"}));
        }
        for operation in self.repository.operations(self.task_id())?.into_iter()
            .filter(|operation| matches!(operation.state, OperationState::Unknown | OperationState::Dispatched))
        {
            match self.reconcile_operation(&task, &operation, &events).await {
                Ok(true) => resolved_operations += 1,
                // Missing observation receipts never become unknown mutations or block task recovery.
                // 观察操作缺少回执不等于未知写入，也不能因此阻塞任务恢复。
                _ if operation.intent.effect == OperationEffect::Observation => {},
                // The user accepted this outcome as unknown: keep it unknown, never replay, and do not block.
                // 用户已确认该结果未知：保持 unknown、不重放，也不再阻塞恢复。
                _ if operation.is_unknown_acknowledged() => acknowledged.push(json!({"taskId":self.task_id(),
                    "operationId":operation.intent.operation_id,"toolCallId":operation.intent.tool_call_id,
                    "toolName":operation.intent.tool_name})),
                Ok(false) => pending.push(json!({"taskId":self.task_id(),"operationId":operation.intent.operation_id,
                    "toolCallId":operation.intent.tool_call_id,"toolName":operation.intent.tool_name,
                    "code":"TOOL_RECEIPT_EVIDENCE_REQUIRED","message":"未找到与原始参数、派发时间及调用身份匹配的唯一真实工具回执。请核对原操作的实际结果；不会自动重做该操作。"})),
                Err(error) => pending.push(json!({"taskId":self.task_id(),"operationId":operation.intent.operation_id,
                    "toolCallId":operation.intent.tool_call_id,"toolName":operation.intent.tool_name,
                    "code":error.code,"message":error.message})),
            }
        }
        Ok(json!({"resolvedOperations":resolved_operations,"resolvedReservations":resolved_reservations,"pending":pending,
            "acknowledgedUnknown":acknowledged}))
    }

    /// Poll a remote task by its journaled identity and settle its original operation exactly once.
    /// 使用正式日志中的远端任务身份查询，仅结算原操作一次，不再次派发原工具。
    pub async fn reconcile_remote_tasks(&self, context: &crate::tools::gateway::GatewayToolContext) -> Result<Value> {
        let task = self.load_current()?;
        let mut resolved = 0;
        let mut pending = Vec::new();
        let events = self.repository.recovery_receipts(self.task_id())?;
        for operation in self.repository.operations(self.task_id())?.into_iter()
            .filter(|operation| operation.blocks_recovery()) {
            let Some(checkpoint) = events.iter().rev().find(|event| event.kind == "mcp_task_checkpoint"
                && event.payload["operationId"] == operation.intent.operation_id
                && event.payload["toolCallId"] == operation.intent.tool_call_id) else { continue; };
            let result = crate::tools::gateway::recover_mcp_task_checkpoint(&checkpoint.payload["recoveryHandle"],
                &operation.intent.tool_call_id, context).await?;
            if result["status"] != "settled" {
                pending.push(json!({"taskId":task.task_id,"operationId":operation.intent.operation_id,"remoteTask":result["remoteTask"]}));
                continue;
            }
            let Some(call) = self.recovery_call(&task, &operation).await? else { continue; };
            let request = if task.parent_task_id.is_some() { task.task_id.clone() } else { task.request_id.clone() };
            let (root, chat, stored_call, canonical) = (self.root.clone(), task.scope.conversation_id.clone(), call.clone(), result["result"].clone());
            let reference = tokio::task::spawn_blocking(move || crate::storage::gateway::save_tool_result(&root, &chat,
                &request, &stored_call, &canonical, None)).await
                .map_err(|_| GatewayError::new("TASK_RECOVERY_WRITE_FAILED", "远端任务回执落盘待重试。", 503))??;
            let failed = result["result"]["isError"] == true;
            let receipt = json!({"status":if failed {"error"} else {"completed"},"isError":failed,
                "content":result["result"],"resultRef":reference});
            let owned = if task.parent_task_id.is_some() {
                self.save_blob("worker-log", &serde_json::to_vec(&receipt).map_err(|_| GatewayError::new("INVALID_TASK_RECEIPT", "远端任务回执无法编码。", 409))?).await?
            } else { self.publish_recovered_root_receipt(&task, &call, &receipt).await?; reference };
            self.finish_operation(&operation.intent.operation_id, if failed {OperationState::Failed} else {OperationState::Succeeded},
                Some(owned), Some(json!({"kind":"mcp-remote-task","checkpointEventId":checkpoint.event_id,"remoteTask":result["result"]["remoteTask"]})))?;
            resolved += 1;
        }
        Ok(json!({"resolved":resolved,"pending":pending}))
    }

    async fn recovery_call(&self, task: &TaskRecord, operation: &TaskOperation) -> Result<Option<Value>> {
        let mut candidates = Vec::new();
        if task.parent_task_id.is_none() {
            let root = self.root.clone();
            let conversation = task.scope.conversation_id.clone();
            let messages = tokio::task::spawn_blocking(move || crate::storage::gateway::read_model_messages(&root, &conversation))
                .await.map_err(|_| GatewayError::new("TASK_RECOVERY_READ_FAILED", "原始调用记录读取失败。", 503))??;
            for message in messages.iter().filter(|message| message["Role"] == "assistant"
                && (message["RequestId"] == task.request_id || message["Id"] == task.request_id))
            {
                if operation.intent.input_reference.as_ref().and_then(|reference| reference["assistantMessageId"].as_str())
                    .is_some_and(|id| message["Id"] != id) { continue; }
                for round in message["ModelTranscript"]["rounds"].as_array().into_iter().flatten() {
                    if operation.intent.input_reference.as_ref().is_some_and(|reference|
                        reference["conversationId"] != task.scope.conversation_id || reference["requestId"] != task.request_id
                        || reference["round"] != round["round"])
                    { continue; }
                    for call in round["calls"].as_array().into_iter().flatten() {
                        let mut call = call.clone();
                        call["round"] = round["round"].clone();
                        if operation.intent.input_reference.as_ref().is_some_and(|reference| reference["assistantMessageId"].is_string()) {
                            call["assistantMessageId"] = message["Id"].clone();
                        }
                        candidates.push(call);
                    }
                }
            }
        } else if let Some(checkpoint) = self.latest_checkpoint()? {
            let checkpoint: Value = serde_json::from_slice(&self.read_blob(&checkpoint.context_reference).await?)
                .map_err(|_| GatewayError::new("INVALID_TASK_CHECKPOINT", "子任务原始调用检查点损坏。", 409))?;
            candidates.extend(checkpoint["pending"]["calls"].as_array().into_iter().flatten().cloned());
        }
        let mut matched = Vec::new();
        for call in candidates.into_iter().filter(|call| call["id"] == operation.intent.tool_call_id) {
            // Steering copies the transcript into the new assistant; identical copies are one original call.
            // 插话会把调用记录复制到新回复；完全相同的副本仍是同一次原始调用。
            if !matched.contains(&call) { matched.push(call); }
        }
        if matched.len() != 1 { return Ok(None); }
        let call = matched.pop().unwrap();
        let input = if task.parent_task_id.is_none() { call["arguments"].to_string() }
            else { json!({"name":call["name"],"arguments":call["arguments"]}).to_string() };
        Ok((call["name"] == operation.intent.tool_name
            && managed_files::digest(input.as_bytes()) == operation.intent.input_sha256).then_some(call))
    }

    async fn reconcile_operation(&self, task: &TaskRecord, operation: &TaskOperation, events: &[TaskEvent]) -> Result<bool> {
        let Some(call) = self.recovery_call(task, operation).await? else { return Ok(false); };
        let witness = events.iter().rev().find(|event| event.kind == "task.operation.receipt"
            && event.payload["intent"] == serde_json::to_value(&operation.intent).unwrap_or(Value::Null)
            && matches!(event.payload["state"].as_str(), Some("succeeded" | "failed")));
        let mut result_reference = witness.map(|event| event.payload["resultReference"].clone())
            .or_else(|| operation.result_reference.clone()).unwrap_or(Value::Null);
        let mut child_receipt = Value::Null;
        if result_reference["kind"] == "task-blob" {
            child_receipt = serde_json::from_slice(&self.read_blob(&result_reference).await?)
                .map_err(|_| GatewayError::new("INVALID_TASK_RECEIPT", "原始子任务回执损坏。", 409))?;
            result_reference = child_receipt["resultRef"].clone();
        }
        let request = if task.parent_task_id.is_some() { task.task_id.clone() } else { task.request_id.clone() };
        let root = self.root.clone();
        let conversation = task.scope.conversation_id.clone();
        let archive = if let Some(id) = result_reference["id"].as_str() {
            let id = id.to_owned();
            tokio::task::spawn_blocking(move || crate::storage::gateway::load_private_tool_result(&root, &conversation, &id))
                .await.map_err(|_| GatewayError::new("TASK_RECOVERY_READ_FAILED", "原始工具回执读取失败。", 503))??
        } else {
            let Some(dispatch) = events.iter().find(|event| event.kind == "task.operation.dispatched"
                && event.payload["operationId"] == operation.intent.operation_id) else { return Ok(false); };
            let call_id = operation.intent.tool_call_id.clone();
            let tool_name = operation.intent.tool_name.clone();
            let request = request.clone();
            let dispatched_at = dispatch.created_at_ms;
            tokio::task::spawn_blocking(move || crate::storage::gateway::find_recovery_tool_result(
                &root, &conversation, &request, &call_id, &tool_name, dispatched_at))
                .await.map_err(|_| GatewayError::new("TASK_RECOVERY_READ_FAILED", "原始工具回执查找失败。", 503))??
        };
        if archive.is_null() { return Ok(false); }
        if archive["requestId"] != request || archive["conversationId"] != task.scope.conversation_id
            || archive["toolCallId"] != operation.intent.tool_call_id || archive["toolName"] != operation.intent.tool_name
            || !result_reference.is_null() && (archive["sha256"] != result_reference["sha256"] || archive["bytes"] != result_reference["bytes"])
        { return Err(GatewayError::new("TASK_RECOVERY_RECEIPT_MISMATCH", "原始回执与待核验调用不匹配。", 409)); }
        let canonical = &archive["canonical"];
        if canonical["status"] == "unknown" || canonical["outcome"] == "unknown"
            || canonical["code"].as_str().is_some_and(|code| code.contains("UNKNOWN"))
        { return Ok(false); }
        let failed = canonical["isError"] == true || canonical.get("error").is_some_and(|error| !error.is_null())
            || matches!(canonical["status"].as_str(), Some("error" | "failed"));
        let outcome = if failed { OperationState::Failed } else { OperationState::Succeeded };
        if witness.is_some_and(|event| event.payload["state"] != serde_json::to_value(&outcome).unwrap_or(Value::Null))
        { return Err(GatewayError::new("TASK_RECOVERY_RECEIPT_MISMATCH", "回执结果与持久化结算证据不一致。", 409)); }
        let archive_reference = json!({"id":archive["id"],"sha256":archive["sha256"],"bytes":archive["bytes"]});
        let receipt = json!({"status":if failed {"error"} else {"completed"},"isError":failed,"content":canonical,"resultRef":archive_reference});
        let owned_reference = if task.parent_task_id.is_some() {
            if child_receipt.is_object() && child_receipt["content"] != *canonical { return Ok(false); }
            self.save_blob("worker-log", &serde_json::to_vec(&receipt).map_err(|_| GatewayError::new("INVALID_TASK_RECEIPT", "核验回执无法编码。", 409))?).await?
        } else {
            self.publish_recovered_root_receipt(task, &call, &receipt).await?;
            archive_reference.clone()
        };
        self.finish_operation(&operation.intent.operation_id, outcome, Some(owned_reference),
            Some(json!({"kind":"verified-tool-receipt","taskId":self.task_id(),"inputSha256":operation.intent.input_sha256,"resultRef":archive_reference})))?;
        Ok(true)
    }

    async fn publish_recovered_root_receipt(&self, task: &TaskRecord, call: &Value, receipt: &Value) -> Result<()> {
        self.repository.check_receipt_attempt(&self.token)?;
        let root = self.root.clone();
        let task = task.clone();
        let call = call.clone();
        let receipt = receipt.clone();
        tokio::task::spawn_blocking(move || {
            let mut messages = crate::storage::gateway::read_model_messages(&root, &task.scope.conversation_id)?;
            let mut published = false;
            for message in messages.iter_mut().rev().filter(|message| message["Role"] == "assistant"
                && (message["RequestId"] == task.request_id || message["Id"] == task.request_id))
            {
                if call["assistantMessageId"].as_str().is_some_and(|id| message["Id"] != id) { continue; }
                let mut patched = false;
                for round in message["ModelTranscript"]["rounds"].as_array_mut().into_iter().flatten()
                    .filter(|round| round["round"] == call["round"])
                {
                    let mut observations = round["observations"].as_array().cloned().unwrap_or_default();
                    observations.retain(|observation| observation["id"] != call["id"]);
                    observations.push(json!({"id":call["id"],"name":call["name"],"status":receipt["status"],"ref":receipt["resultRef"],
                        "verifiedSuccess":super::task::successful_completion_receipt(&receipt),
                        "completionEvidence":super::task::completed_output_paths(call["name"].as_str().unwrap_or(""), &receipt)}));
                    round["observations"] = json!(observations);
                    patched = true;
                }
                if !patched { continue; }
                let mut activities = message["ToolActivities"].as_array().cloned().unwrap_or_default();
                let index = activities.iter().position(|activity| activity["toolCallId"] == call["id"] && activity["round"] == call["round"]);
                let mut activity = index.map(|index| activities[index].clone()).unwrap_or_else(|| json!({"toolCallId":call["id"],"name":call["name"],"arguments":call["arguments"],"round":call["round"],"order":activities.len()+1}));
                activity["status"] = receipt["status"].clone();
                activity["resultRef"] = receipt["resultRef"].clone();
                activity["summary"] = json!("原始回执已核验");
                if let Some(index) = index { activities[index] = activity; } else { activities.push(activity); }
                message["ToolActivities"] = json!(activities);
                message["RecoveredReceiptVersion"] = json!(task.event_sequence);
                crate::storage::gateway::upsert_message(&root, &task.scope.conversation_id, message)?;
                published = true;
                break;
            }
            if !published {
                return Err(crate::storage::ContractError::new("原始助手轮次不存在，核验回执尚未发布。", "TASK_RECOVERY_PROJECTION_MISSING", 409));
            }
            Ok::<(), crate::storage::ContractError>(())
        }).await.map_err(|_| GatewayError::new("TASK_RECOVERY_SAVE_FAILED", "核验后的调用记录保存失败。", 503))??;
        Ok(())
    }
    pub async fn finish(&self, state: TaskState, reason: Option<&str>) -> Result<TaskRecord> {
        if self.is_released() {
            // The final state and release may already be committed when draining was interrupted.
            // 最终状态与释放可能已提交、仅后台收尾被中断；重试不能再次使用已结束的尝试写入。
            self.drain_background().await?;
            let task = self.load_current()?;
            if task.state == state || matches!(task.state, TaskState::Paused | TaskState::Cancelled) {
                return Ok(task);
            }
            return Err(GatewayError::new(
                "TASK_FINAL_STATE_CONFLICT",
                "Task final state changed after release. / 任务释放后的最终状态与预期不一致。",
                409,
            ));
        }
        self.flush().await?;
        let task = self.load_current()?;
        if task.state.is_terminal() || task.state == TaskState::Paused {
            self.repository
                .release_attempt(&self.token, reason.unwrap_or("controlled-boundary"))?;
            self.released.store(true, Ordering::Release);
            self.drain_background().await?;
            return Ok(task);
        }
        let ended = self
            .repository
            .transition(&self.token, task.version, state, reason)?;
        self.repository
            .release_attempt(&self.token, reason.unwrap_or("task-boundary"))?;
        self.released.store(true, Ordering::Release);
        self.drain_background().await?;
        Ok(ended)
    }
    pub async fn release(&self, reason: &str) -> Result<()> {
        self.flush().await?;
        self.repository.release_attempt(&self.token, reason)?;
        self.released.store(true, Ordering::Release);
        self.drain_background().await?;
        Ok(())
    }

    /// Released journals must not retain a writer or heartbeat against a root about to move.
    /// 已释放日志不能继续持有即将迁移根目录上的写入者或心跳。
    pub async fn drain_background(&self) -> Result<()> {
        if !self.is_released() {
            return Err(GatewayError::new(
                "TASK_JOURNAL_STILL_ACTIVE",
                "任务日志尚未释放。",
                409,
            ));
        }
        self.renewal_stop.notify_one();
        let renewal = self
            .renewal
            .lock()
            .map_err(|_| {
                GatewayError::new("TASK_HEARTBEAT_CLEANUP_FAILED", "任务心跳状态不可用。", 503)
            })?
            .take();
        if let Some(task) = renewal {
            task.await.map_err(|_| {
                GatewayError::new("TASK_HEARTBEAT_CLEANUP_FAILED", "任务心跳退场未确认。", 503)
            })?;
        }
        self.wake_flush.notify_one();
        while self.scheduled.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if let Some(error) = self.last_persistence_error() {
            return Err(error);
        }
        Ok(())
    }
}
