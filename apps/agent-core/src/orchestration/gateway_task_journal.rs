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
        });
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&journal);
            runtime.spawn(Self::renew_while_owned(weak, lease_ms));
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

    async fn renew_while_owned(weak: Weak<Self>, lease_ms: u64) {
        let delay = Duration::from_millis((lease_ms / 3).clamp(1_000, 5_000));
        loop {
            tokio::time::sleep(delay).await;
            let Some(journal) = weak.upgrade() else { break };
            let repository = journal.repository.clone();
            let token = journal.token.clone();
            // Lease renewal performs no model call and never revives a paused/cancelled task.
            // 租约续期不调用模型，也不让暂停、取消的任务复活。
            let renewed =
                tokio::task::spawn_blocking(move || repository.renew_attempt(&token, lease_ms))
                    .await;
            if !matches!(renewed, Ok(Ok(_))) {
                break;
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
            "child-context" | "worker-log" | "task-output" | "task-plan" | "task-checkpoint" | "document-progress"
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
        Ok(self
            .repository
            .settle_model_request(&self.token, id, dispatched, usage)?)
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
        Ok(self.repository.finish_operation(
            &self.token,
            id,
            state,
            result_reference,
            verification_reference,
        )?)
    }
    pub async fn finish(&self, state: TaskState, reason: Option<&str>) -> Result<TaskRecord> {
        self.flush().await?;
        let task = self.load_current()?;
        if task.state.is_terminal() || task.state == TaskState::Paused {
            self.repository
                .release_attempt(&self.token, reason.unwrap_or("controlled-boundary"))?;
            self.released.store(true, Ordering::Release);
            return Ok(task);
        }
        let ended = self
            .repository
            .transition(&self.token, task.version, state, reason)?;
        self.repository
            .release_attempt(&self.token, reason.unwrap_or("task-boundary"))?;
        self.released.store(true, Ordering::Release);
        Ok(ended)
    }
    pub async fn release(&self, reason: &str) -> Result<()> {
        self.flush().await?;
        self.repository.release_attempt(&self.token, reason)?;
        self.released.store(true, Ordering::Release);
        Ok(())
    }
}
