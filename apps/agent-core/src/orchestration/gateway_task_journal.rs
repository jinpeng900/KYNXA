//! Connect the request loop to its single task repository without duplicating ledgers.
//! 请求循环复用唯一任务仓储，不复制账本。

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

#[cfg(test)]
mod durability_tests {
    use super::*;

    #[tokio::test]
    async fn cold_claim_uses_verified_original_limits_per_dimension_without_resetting_usage() {
        let root = std::env::temp_dir().join(format!("kynxa-cold-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for (limits, requests, active_ms, succeeds) in [
            (json!({}), 256, 1_800_000, true),
            (json!({"maxDurationMs":1_800_000}), 256, 1_800_000, false),
            (json!({"maxModelRequests":256}), 256, 1_800_000, false),
            (json!({"maxDurationMs":1_800_000}), 256, 0, true),
            (json!({"maxModelRequests":256}), 0, 1_800_000, true),
        ] {
            let request = uuid::Uuid::new_v4().to_string();
            let conversation = uuid::Uuid::new_v4().to_string();
            let document = json!({"input":{"requestId":request,"conversationId":conversation,
                "message":"继续完成有实际成果的原任务","runLimits":limits},"fingerprint":"saved-fingerprint"});
            let bytes = serde_json::to_vec(&document).unwrap();
            let hash = managed_files::digest(&bytes);
            let relative = format!("Agent/task-inputs/{request}-{hash}.json");
            managed_files::atomic_write(&root, &root.join(&relative), &bytes, MAX_BLOB_BYTES).unwrap();
            let input = NewTask {request_id:request, goal:"Preserve original progress".into(), acceptance:vec![],
                scope:TaskScope {project_id:None, conversation_id:conversation,
                    permission_snapshot:json!({"requestFingerprint":"saved-fingerprint"})},
                parent_task_id:None, dependency_ids:vec![],
                budget:TaskBudget {max_model_requests:256,max_known_tokens:Some(10_000),max_execution_ms:Some(1_800_000)},
                input_reference:Some(json!({"kind":"task-input","relativePath":relative,"sha256":hash,"bytes":bytes.len()})),
            };
            let original = TaskJournal::begin(&root, &input, "first-worker", 60_000).unwrap();
            original.repository.record_usage(original.token(), "actual-prior-usage", &TaskUsage {
                model_requests:requests, input_tokens:100, output_tokens:20,
                unknown_token_requests:u64::from(requests > 0), ..Default::default()
            }).unwrap();
            original.repository.record_active_duration(original.token(), active_ms).unwrap();
            original.release("shutdown-checkpoint").await.unwrap();
            let before = original.load_current().unwrap();
            let claimed = TaskJournal::start_existing(&root, &before, "resumed-worker", 60_000);
            if succeeds {
                let resumed = claimed.unwrap();
                let after = resumed.load_current().unwrap();
                assert_eq!(after.task_id, before.task_id);
                assert_eq!(after.usage, before.usage);
                assert_eq!(after.budget.max_known_tokens, Some(10_000));
                assert_eq!(after.budget.max_model_requests, if limits["maxModelRequests"].is_null() && requests == 256 {320} else {256});
                assert_eq!(after.budget.max_execution_ms, Some(if limits["maxDurationMs"].is_null() && active_ms == 1_800_000 {3_600_000} else {1_800_000}));
                resumed.release("fixture-complete").await.unwrap();
            } else {
                assert!(claimed.is_err(), "{limits}");
                let after = original.load_current().unwrap();
                assert_eq!(after.budget, before.budget);
                assert_eq!(after.usage, before.usage);
            }
        }
        crate::storage::sqlite_connections::drain_root(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cold_child_claim_inherits_root_caps_and_renews_only_the_implicit_tree() {
        let root = std::env::temp_dir().join(format!("kynxa-child-cold-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for explicit in [false, true] {
            let request = uuid::Uuid::new_v4().to_string();
            let conversation = uuid::Uuid::new_v4().to_string();
            let limits = if explicit {json!({"maxModelRequests":256})} else {json!({})};
            let bytes = serde_json::to_vec(&json!({"input":{"requestId":request,"conversationId":conversation,
                "runLimits":limits},"fingerprint":"original-root"})).unwrap();
            let hash = managed_files::digest(&bytes);
            let relative = format!("Agent/task-inputs/{request}-{hash}.json");
            managed_files::atomic_write(&root, &root.join(&relative), &bytes, MAX_BLOB_BYTES).unwrap();
            let input = NewTask {request_id:request, goal:"Continue original root".into(), acceptance:vec![],
                scope:TaskScope {project_id:None, conversation_id:conversation, permission_snapshot:json!({})},
                parent_task_id:None, dependency_ids:vec![],
                budget:TaskBudget {max_model_requests:256,max_known_tokens:None,max_execution_ms:Some(1_800_000)},
                input_reference:Some(json!({"kind":"task-input","relativePath":relative,"sha256":hash,"bytes":bytes.len()})),
            };
            let parent = TaskJournal::begin(&root,&input,"parent",60_000).unwrap();
            let child_input = NewTask {request_id:uuid::Uuid::new_v4().to_string(),parent_task_id:Some(parent.task_id().into()),
                input_reference:Some(json!({"kind":"task-child-input"})),..input};
            let child = TaskJournal::begin(&root,&child_input,"child",60_000).unwrap();
            child.repository.record_usage(child.token(),"prior-child-provider-usage",
                &TaskUsage {model_requests:256,input_tokens:120,unknown_token_requests:1,..Default::default()}).unwrap();
            child.repository.record_active_duration(child.token(),1_800_000).unwrap();
            parent.repository.record_active_duration(parent.token(),1_800_000).unwrap();
            child.release("shutdown-checkpoint").await.unwrap();
            parent.release("shutdown-checkpoint").await.unwrap();
            let old_child = child.load_current().unwrap();
            let old_parent = parent.load_current().unwrap();
            let claimed = TaskJournal::start_existing(&root,&old_child,"continued-child",60_000);
            if explicit {
                assert!(claimed.is_err());
            } else {
                let continued = claimed.unwrap();
                assert_eq!(continued.load_current().unwrap().budget.max_model_requests,320);
                assert_eq!(parent.load_current().unwrap().budget.max_model_requests,320);
                continued.release("fixture-complete").await.unwrap();
            }
            assert_eq!(child.load_current().unwrap().usage,old_child.usage);
            assert_eq!(parent.load_current().unwrap().usage,old_parent.usage);
            if explicit {
                assert_eq!(child.load_current().unwrap().budget,old_child.budget);
                assert_eq!(parent.load_current().unwrap().budget,old_parent.budget);
            }
        }
        crate::storage::sqlite_connections::drain_root(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn child_dispatch_rechecks_formal_ancestor_steering_and_keeps_successful_receipts() {
        let (mut context, _cancel) = crate::tools::gateway::test_context();
        let root = context.data_home.clone();
        let input = NewTask { request_id:uuid::Uuid::new_v4().to_string(), goal:"开始制作文件".into(), acceptance:vec![],
            scope:TaskScope {project_id:None,conversation_id:context.conversation_id.clone(),permission_snapshot:json!({})},
            parent_task_id:None,dependency_ids:vec![],budget:TaskBudget::default(),input_reference:None };
        crate::storage::gateway::ensure_conversation(&root,&context.conversation_id,"Ancestor scope fixture").unwrap();
        let parent = TaskJournal::begin(&root,&input,"parent",60_000).unwrap();
        let child_input = NewTask {request_id:uuid::Uuid::new_v4().to_string(),parent_task_id:Some(parent.task_id().into()),..input.clone()};
        let child = TaskJournal::begin(&root,&child_input,"child",60_000).unwrap();
        child.refresh_ancestor_work_scope(&mut context,&child_input.goal).await.unwrap();
        let intent = |id:&str| OperationIntent {operation_id:id.into(),tool_call_id:id.into(),tool_name:"filesystem.write".into(),
            input_sha256:"a".repeat(64),input_reference:None,effect:OperationEffect::FileMutation};
        child.record_operation(&intent("saved-success")).unwrap();
        child.dispatch_with_ancestor_scope("saved-success",&context).unwrap();
        let saved_receipt = child.save_blob("worker-log",br#"{"status":"completed","executed":true,"fixture":"original-receipt"}"#).await.unwrap();
        child.finish_operation("saved-success",OperationState::Succeeded,Some(saved_receipt.clone()),None).unwrap();
        child.record_operation(&intent("not-dispatched")).unwrap();
        let quick_id = uuid::Uuid::new_v4().to_string();
        let quick = "快一点";
        crate::storage::gateway::upsert_message(&root,&context.conversation_id,
            &json!({"Id":quick_id,"Role":"user","Content":quick,"Status":"completed"})).unwrap();
        let current = parent.load_current().unwrap();
        parent.repository.record_user_steering(parent.task_id(),current.version,&quick_id,"continue",
            &super::super::gateway_task_hub::steering_content_hash(quick, &Value::Null)).unwrap();
        let waiting = child.refresh_ancestor_work_scope(&mut context,&child_input.goal).await.unwrap().unwrap();
        assert_eq!(waiting["stage"],"execute", "an ordinary speed request cannot change the task into an explanation");
        assert_eq!(waiting["ancestorControlPending"],true);
        assert_eq!(child.dispatch_with_ancestor_scope("not-dispatched",&context).unwrap_err().code,"TASK_PARENT_SCOPE_CHANGED");
        parent.emit_control("task.user-directive",&json!({"userMessageId":quick_id,"stop":false,"deliverExisting":false})).unwrap();
        parent.flush().await.unwrap();
        let resumed = child.refresh_ancestor_work_scope(&mut context,&child_input.goal).await.unwrap().unwrap();
        assert_eq!(resumed["stage"],"execute");
        assert_eq!(resumed["ancestorControlPending"],false);
        let message_id = uuid::Uuid::new_v4().to_string();
        let text = "先只给我方案，不要修改文件";
        crate::storage::gateway::upsert_message(&root,&context.conversation_id,
            &json!({"Id":message_id,"Role":"user","Content":text,"Status":"completed"})).unwrap();
        let current = parent.load_current().unwrap();
        parent.repository.record_user_steering(parent.task_id(),current.version,&message_id,"continue",
            &super::super::gateway_task_hub::steering_content_hash(text, &Value::Null)).unwrap();
        assert_eq!(parent.load_current().unwrap().state,TaskState::Running);
        assert_eq!(child.dispatch_with_ancestor_scope("not-dispatched",&context).unwrap_err().code,"TASK_PARENT_SCOPE_CHANGED");
        let scope = child.refresh_ancestor_work_scope(&mut context,&child_input.goal).await.unwrap().unwrap();
        assert!(!super::super::task::scope_allows_call(&scope,"filesystem.write",false));
        assert!(super::super::task::scope_allows_call(&scope,"filesystem.read",true));
        let operations = child.repository.operations(child.task_id()).unwrap();
        assert_eq!(operations.iter().find(|item|item.intent.operation_id=="saved-success").unwrap().state,OperationState::Succeeded);
        assert_eq!(operations.iter().find(|item|item.intent.operation_id=="saved-success").unwrap().result_reference,Some(saved_receipt));
        assert_eq!(operations.iter().find(|item|item.intent.operation_id=="not-dispatched").unwrap().state,OperationState::Pending);
        child.finish(TaskState::Partial,Some("fixture-complete")).await.unwrap();
        parent.finish(TaskState::Partial,Some("fixture-complete")).await.unwrap();
        drop(child);drop(parent);crate::storage::sqlite_connections::drain_root(&root).unwrap();
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn model_checkpoints_restore_scoped_images_without_changing_receipts_or_raw_hashes() {
        let root = std::env::temp_dir().join(format!("kynxa-journal-images-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let input = NewTask {
            request_id:uuid::Uuid::new_v4().to_string(), goal:"Resume image evidence".into(), acceptance:Vec::new(),
            scope:TaskScope {project_id:None, conversation_id:uuid::Uuid::new_v4().to_string(), permission_snapshot:json!({"permissionMode":"full"})},
            parent_task_id:None, dependency_ids:Vec::new(), budget:TaskBudget::default(), input_reference:None,
        };
        crate::storage::gateway::ensure_conversation(&root, &input.scope.conversation_id, "Image fixture").unwrap();
        let journal = TaskJournal::begin(&root, &input, "image-first", 60_000).unwrap();
        let pixels = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a3ioAAAAASUVORK5CYII=";
        let image = json!({"type":"image","mimeType":"image/png","data":pixels});
        let context = json!({"messages":[{"role":"tool","content":[image.clone()]}],
            "progress":{"completedCallIds":["saved-call"],"sequence":7},
            "signature":image,"documentText":pixels});
        let origin = json!({"providerId":"fixture","model":"fixture"});
        let reference = journal.save_root_work_context(&input.request_id, "fingerprint", &origin, &context).await.unwrap();
        journal.checkpoint_from_chat("assistant-image", &input.request_id, Some(reference.clone()), json!({"next":"remaining detail"})).await.unwrap();
        let raw = journal.read_blob(&reference).await.unwrap();
        assert_eq!(reference["sha256"], managed_files::digest(&raw));
        let archived: Value = serde_json::from_slice(&raw).unwrap();
        let image_ref = &archived["context"]["messages"][0]["content"][0]["data"]["kynxaImageRef"];
        assert_eq!(image_ref["requestId"], input.request_id);
        assert_eq!(archived["context"]["signature"], context["signature"]);
        assert_eq!(archived["context"]["documentText"], pixels);
        assert_eq!(journal.read_root_work_context("assistant-image", &input.request_id, "wrong", &origin)
            .await.unwrap_err().code, "INVALID_ROOT_WORK_CONTEXT");
        let current = journal.load_current().unwrap();
        journal.repository.control(journal.task_id(), current.version, TaskControl::Pause).unwrap();
        let paused = journal.finish(TaskState::Paused, Some("fixture-pause")).await.unwrap();
        let resumed = journal.repository.control(journal.task_id(), paused.version, TaskControl::Resume).unwrap();
        let recovered = TaskJournal::start_existing(&root, &resumed, "image-resumed", 60_000).unwrap();
        assert_eq!(recovered.read_root_work_context("assistant-image", &input.request_id, "fingerprint", &origin)
            .await.unwrap().unwrap(), (context.clone(), reference.clone()));
        assert_eq!(recovered.read_blob(&reference).await.unwrap(), raw);

        // A child can read a verified parent context, but each saved checkpoint uses its own chat identity.
        // 子任务可读取已核验的父上下文，自存检查点仍使用自己的聊天身份。
        let mut child_input = input.clone();
        child_input.request_id = "child-image-fixture".into();
        child_input.parent_task_id = Some(recovered.task_id().into());
        child_input.scope.conversation_id = uuid::Uuid::new_v4().to_string();
        crate::storage::gateway::ensure_conversation(&root, &child_input.scope.conversation_id, "Child image fixture").unwrap();
        let child = TaskJournal::begin(&root, &child_input, "image-child", 60_000).unwrap();
        let inherited: Value = serde_json::from_slice(&child.read_model_blob(&reference).await.unwrap()).unwrap();
        assert_eq!(inherited["context"], context);
        let child_reference = child.save_task_blob("child-context", &context).await.unwrap().reference;
        let child_raw: Value = serde_json::from_slice(&child.read_blob(&child_reference).await.unwrap()).unwrap();
        assert_eq!(child_raw["messages"][0]["content"][0]["data"]["kynxaImageRef"]["requestId"], child.task_id());
        let child_restored: Value = serde_json::from_slice(&child.read_model_blob(&child_reference).await.unwrap()).unwrap();
        assert_eq!(child_restored, context);

        let payload = root.join("Chats").join(&input.scope.conversation_id).join("model-images")
            .join(format!("{}.bin", image_ref["id"].as_str().unwrap()));
        std::fs::remove_file(payload).unwrap();
        let (expired, _) = recovered.read_root_work_context("assistant-image", &input.request_id, "fingerprint", &origin)
            .await.unwrap().unwrap();
        assert_eq!(expired["messages"][0]["content"][0]["type"], "text");
        assert!(expired["messages"][0]["content"][0]["text"].as_str().unwrap().contains(image_ref["imageSha256"].as_str().unwrap()));
        assert_eq!(expired["progress"], context["progress"]);
        assert_eq!(recovered.read_blob(&reference).await.unwrap(), raw);
        assert_eq!(serde_json::from_slice::<Value>(&child.read_model_blob(&child_reference).await.unwrap()).unwrap(), context);
        let plain = b"{ \"document\": \"unchanged text\" }";
        let plain_reference = child.save_blob("worker-log", plain).await.unwrap();
        assert_eq!(child.read_blob(&plain_reference).await.unwrap(), plain);
        child.finish(TaskState::Partial, Some("fixture-finished")).await.unwrap();
        recovered.finish(TaskState::Partial, Some("fixture-finished")).await.unwrap();
        drop(child); drop(recovered); drop(journal);
        crate::storage::gateway_memory::stop_candidates(&root);
        crate::storage::sqlite_connections::drain_root(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn durable_journal_cancelled_flush_keeps_original_ids_until_commit_acknowledgement() {
        let root = std::env::temp_dir().join(format!("kynxa-durable-journal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let journal = TaskJournal::begin(&root, &NewTask {
            request_id:uuid::Uuid::new_v4().to_string(), goal:"Durable stream fixture".into(), acceptance:Vec::new(),
            scope:TaskScope { project_id:None, conversation_id:uuid::Uuid::new_v4().to_string(), permission_snapshot:json!({"permissionMode":"full"}) },
            parent_task_id:None, dependency_ids:Vec::new(), budget:TaskBudget::default(), input_reference:None,
        }, "journal-fixture", 60_000).unwrap();
        // Disable only the test pump so cancellation targets the foreground flush deterministically.
        // 仅在夹具中关闭定时泵，使取消准确作用于前台flush，而不是由时间竞争决定测试结果。
        journal.scheduled.store(true, Ordering::Release);
        journal.emit_control("text_delta", &json!({"type":"text_delta","delta":"preserved 🏰"})).unwrap();
        let original_id = journal.pending.lock().unwrap().events[0].0.clone();
        let writer = crate::storage::sqlite_connections::acquire_write(&root, &root.join("Agent/tasks.sqlite"), 30_000).unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let operation = journal.clone(); let observed = started.clone();
        let flushing = tokio::spawn(async move { observed.store(true, Ordering::Release); operation.flush().await });
        while !started.load(Ordering::Acquire) { tokio::task::yield_now().await; }
        flushing.abort(); assert!(flushing.await.unwrap_err().is_cancelled());
        assert_eq!(journal.pending.lock().unwrap().events[0].0, original_id);
        drop(writer);
        journal.flush().await.unwrap();
        assert!(journal.pending.lock().unwrap().events.is_empty());
        let events = journal.repository.read_events(journal.task_id(), 0, 512).unwrap();
        let saved = events.iter().filter(|event|event.kind=="text_delta").collect::<Vec<_>>();
        assert_eq!(saved.len(),1); assert_eq!(saved[0].event_id,original_id);
        assert_eq!(saved[0].payload["delta"],"preserved 🏰");
        journal.scheduled.store(false, Ordering::Release);
        journal.finish(TaskState::Partial,Some("fixture-complete")).await.unwrap();
        drop(journal); crate::storage::sqlite_connections::drain_root(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Resolve authority once from the original root input; legacy unknown origins retain their existing caps.
/// 一次性读取原始根输入的授权；来源不可确认的旧任务保留原上限。
fn takeover_budget_extensions(
    root: &Path,
    repository: &TaskRepository,
    task: &TaskRecord,
) -> Result<(Option<super::budget::OperationalBudgetPolicy>, Vec<TaskAttemptBudgetExtension>)> {
    let mut owners = vec![task.clone()];
    while let Some(parent) = owners.last().and_then(|owner| owner.parent_task_id.as_deref()) {
        if owners.len() >= 64 || owners.iter().any(|owner|owner.task_id == parent) {
            return Err(GatewayError::new("TASK_DEPTH_LIMIT", "任务祖先层级无效，原进度保留。", 409));
        }
        owners.push(repository.load(parent)?);
    }
    let origin = owners.last().expect("current task is an owner");
    let Some(reference) = origin.input_reference.as_ref().filter(|reference|reference["kind"] == "task-input") else {
        return Ok((None, Vec::new()));
    };
    let (input, _, fingerprint) = super::gateway_task_input::read_sync(root, reference)?;
    if input["requestId"] != origin.request_id || input["conversationId"] != origin.scope.conversation_id
        || origin.scope.permission_snapshot["requestFingerprint"].as_str().is_some_and(|saved|saved != fingerprint) {
        return Err(GatewayError::new("TASK_INPUT_SCOPE_MISMATCH", "原始预算输入与任务身份不符，原进度保留。", 409));
    }
    let policy = super::budget::OperationalBudgetPolicy::from_input(&input);
    let extensions = owners.iter().filter_map(|owner| {
        let (requests, execution_ms) = super::budget::operational_budget_floor(
            policy, owner.usage.model_requests, owner.usage.active_duration_ms.unwrap_or(0),
            owner.budget.max_model_requests, owner.budget.max_execution_ms,
        );
        (requests > owner.budget.max_model_requests
            || owner.budget.max_execution_ms.is_some_and(|limit|execution_ms > limit))
            .then(||TaskAttemptBudgetExtension {
                task_id:owner.task_id.clone(), expected_version:owner.version,
                min_model_requests:requests, min_execution_ms:execution_ms,
            })
    }).collect();
    Ok((Some(policy), extensions))
}

/// The attempt owns this bridge; TaskRepository retains task IDs and receipts across replacement.
/// 尝试持有本接线，更换执行者后任务身份及回执仍由 TaskRepository 保留。
pub struct TaskJournal {
    repository: Arc<TaskRepository>,
    root: PathBuf,
    token: AttemptToken,
    input_reference: Option<Value>,
    operational_budget_policy: Option<super::budget::OperationalBudgetPolicy>,
    pending: Mutex<EventBuffer>,
    flush_serial: AsyncMutex<()>,
    scheduled: AtomicBool,
    wake_flush: Notify,
    last_flush_error: Mutex<Option<GatewayError>>,
    released: AtomicBool,
    renewal: Mutex<Option<tokio::task::JoinHandle<()>>>,
    renewal_stop: Arc<Notify>,
    #[cfg(test)]
    committed_event_batches: Arc<std::sync::atomic::AtomicUsize>,
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
        let (operational_budget_policy, extensions) = takeover_budget_extensions(root, &repository, task)?;
        let attempt = repository.start_attempt_with_budget_extension(
            &task.task_id, task.version, executor_id, lease_ms, &extensions,
        )?;
        let journal = Arc::new(Self {
            repository,
            root: root.to_owned(),
            token: attempt.token,
            input_reference: task.input_reference.clone(),
            operational_budget_policy,
            pending: Mutex::new(EventBuffer::default()),
            flush_serial: AsyncMutex::new(()),
            scheduled: AtomicBool::new(false),
            wake_flush: Notify::new(),
            last_flush_error: Mutex::new(None),
            released: AtomicBool::new(false),
            renewal: Default::default(),
            renewal_stop: Arc::new(Notify::new()),
            #[cfg(test)]
            committed_event_batches: Default::default(),
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
    #[cfg(test)]
    pub(super) fn committed_event_batches(&self) -> usize {
        self.committed_event_batches.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(super) fn pending_event_ids(&self) -> Vec<String> {
        self.pending.lock().unwrap().events.iter().map(|event| event.0.clone()).collect()
    }
    pub fn input_reference(&self) -> Option<&Value> {
        self.input_reference.as_ref()
    }
    pub(super) fn operational_budget_policy(&self) -> Option<super::budget::OperationalBudgetPolicy> {
        self.operational_budget_policy
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

    /// Refresh only when formal ancestor instructions change; cached scope grants no independent authority.
    /// 仅在正式祖先引导变化时刷新；范围缓存本身不产生权限。
    pub(crate) async fn refresh_ancestor_work_scope(
        &self, context: &mut crate::tools::gateway::GatewayToolContext, goal: &str,
    ) -> Result<Option<Value>> {
        let mut ancestors = Vec::new();
        let mut current = self.check_dispatch_fence()?;
        while let Some(parent) = current.parent_task_id.as_deref() {
            current = self.repository.load(parent)?;
            ancestors.push(current.clone());
            if ancestors.len() >= 64 { return Err(GatewayError::new("TASK_DEPTH_LIMIT", "任务祖先层级无效。", 409)); }
        }
        ancestors.reverse();
        let mut version = Vec::new();
        for ancestor in &ancestors {
            let sequence = self.repository.latest_event(&ancestor.task_id, "task.user-steering")?.map_or(0, |event| event.sequence);
            let directive = self.repository.latest_event(&ancestor.task_id, "task.user-directive")?.map_or(0, |event| event.sequence);
            version.push(json!({"taskId":ancestor.task_id,"sequence":sequence,"directiveSequence":directive}));
        }
        let version = json!(version);
        if context.config["ancestorWorkScopeVersion"] == version { return Ok(None); }
        let mut scope = Value::Null;
        let mut control_pending = false;
        for ancestor in ancestors {
            let inherited = scope.clone();
            scope = super::task::resolve_work_scope(&ancestor.goal, &scope);
            let events = self.repository.user_steering(&ancestor.task_id)?;
            if !events.is_empty() {
                let root = self.root.clone();
                let scope_row = ancestor.scope.clone();
                let ids = events.iter().filter_map(|event| event.payload["userMessageId"].as_str().map(str::to_owned)).collect();
                let messages = tokio::task::spawn_blocking(move || crate::storage::conversation_history::read_latest(
                    &root, &json!({"Id":scope_row.conversation_id,"ProjectId":scope_row.project_id}), Some(&ids), false))
                    .await.map_err(|_|GatewayError::new("TASK_SCOPE_READ_FAILED", "父级要求读取失败，原进度保留。", 503))??;
                let latest_user = events.last().map(|event|event.payload["userMessageId"].clone());
                for event in events {
                    let saved = messages.messages.iter().filter_map(|raw| serde_json::from_str::<Value>(
                        &crate::storage::raw_json::scalar_projection(raw)).ok()).find(|message|
                        message["Id"] == event.payload["userMessageId"] && message["Role"] == "user")
                        .ok_or_else(||GatewayError::new("TASK_STEERING_MESSAGE_MISSING", "父级引导原文不可读取，旧调用未派发。", 409))?;
                    let text = saved["Content"].as_str().ok_or_else(||GatewayError::new("TASK_STEERING_MESSAGE_INVALID", "父级引导正文无效。", 409))?;
                    if super::gateway_task_hub::steering_content_hash(text, &saved["Attachments"]) != event.payload["contentHash"] {
                        return Err(GatewayError::new("TASK_STEERING_MESSAGE_CHANGED", "父级引导正文已变化，旧调用未派发。", 409));
                    }
                    scope = super::task::resolve_work_scope(text, &scope);
                }
                let directive = self.repository.latest_event(&ancestor.task_id, "task.user-directive")?;
                let latest_directive = directive.as_ref().filter(|event| Some(&event.payload["userMessageId"]) == latest_user.as_ref());
                control_pending |= latest_directive.is_none();
                if latest_directive.is_some_and(|event| event.payload["stop"] == true || event.payload["deliverExisting"] == true) {
                    return Err(GatewayError::new("TASK_PARENT_EXECUTION_STOPPED", "父级已停止后续执行，原子任务进度及回执保留。", 409));
                }
            }
            if inherited.is_object() && !super::task::scope_allows_mutation(&inherited) { scope["stage"] = inherited["stage"].clone(); }
        }
        let inherited = scope.clone();
        scope = super::task::resolve_work_scope(goal, &scope);
        if inherited.is_object() && !super::task::scope_allows_mutation(&inherited) { scope["stage"] = inherited["stage"].clone(); }
        scope["ancestorRequest"] = inherited["request"].clone();
        scope["ancestorControlPending"] = json!(control_pending);
        context.config["currentWorkScope"] = scope.clone();
        context.config["ancestorWorkScopeVersion"] = version;
        Ok(Some(scope))
    }

    pub(crate) fn dispatch_with_ancestor_scope(&self, id: &str, context: &crate::tools::gateway::GatewayToolContext) -> Result<TaskOperation> {
        if context.config["currentWorkScope"]["ancestorControlPending"] == true {
            return Err(GatewayError::new("TASK_PARENT_SCOPE_CHANGED", "父级正在解析新要求，原调用尚未派发。", 409));
        }
        Ok(self.repository.dispatch_operation_with_ancestor_scope(&self.token, id, &context.config["ancestorWorkScopeVersion"])?)
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

    /// Queue controls and batch writes instead of synchronously writing SQLite per token.
    /// 控制事件入队后批量落盘，不逐 token 同步写 SQLite。
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
            // Reset scheduling under the buffer lock to avoid stranding late events.
            // 缓存锁内复位调度，避免遗漏晚到事件。
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
            let pending = self.pending.lock().map_err(|_| {
                GatewayError::new(
                    "TASK_EVENT_BUFFER_FAILED",
                    "Task event buffer is unavailable. / 任务事件缓存不可用。",
                    500,
                )
            })?;
            // Retain original events until commit acknowledgement, even if the waiting future is cancelled.
            // 提交确认前保留原事件，等待 Future 取消也不丢失。
            pending.events.clone()
        };
        if batch.is_empty() {
            return Ok(());
        }
        let repository = self.repository.clone();
        let token = self.token.clone();
        let event_ids = batch.iter().map(|(id, _, _)| id.clone()).collect::<Vec<_>>();
        #[cfg(test)]
        let committed_event_batches = self.committed_event_batches.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Stable IDs allow replay after partial commits; keep each transaction within 512 events.
            // 稳定 ID 支持部分提交后重放，单次事务仍不超 512 条。
            for portion in batch.chunks(512) {
                repository.append_events(&token, portion)?;
                #[cfg(test)]
                committed_event_batches.fetch_add(1, Ordering::AcqRel);
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
        if result.is_ok() {
            let mut pending = self.pending.lock().map_err(|_| GatewayError::new(
                "TASK_EVENT_BUFFER_FAILED", "Task event buffer is unavailable. / 任务事件缓存不可用。", 500))?;
            if pending.events.len() < event_ids.len()
                || !pending.events.iter().zip(&event_ids).all(|((id, _, _), expected)| id == expected)
            {
                return Err(GatewayError::new("TASK_EVENT_BUFFER_FAILED",
                    "Task event acknowledgement does not match the queued batch. / 事件提交确认与原队列不匹配。", 500));
            }
            pending.events.drain(..event_ids.len());
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
    pub(crate) fn remember_persistence_error(&self, error: GatewayError) {
        if let Ok(mut saved) = self.last_flush_error.lock() { *saved = Some(error); }
    }

    /// Store task context/logs as blobs; reference existing chat/tool originals.
    /// 任务上下文及日志存正文对象，已有聊天和工具原文仅保留引用。
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
        let task = self.repository.check_receipt_attempt(&self.token)?;
        let root = self.root.clone();
        let owned_kind = kind.to_owned();
        let mut write = bytes.to_vec();
        let (digest, relative, size_bytes) = tokio::task::spawn_blocking(move || -> Result<_> {
            // Bound and hash the final references; disk I/O and image projection stay off the async executor.
            // 对最终引用正文限长及哈希；磁盘读写和图像投影不占异步执行线程。
            if matches!(owned_kind.as_str(), "task-checkpoint" | "worker-log" | "task-output" | "child-context") {
                if let Ok(value) = serde_json::from_slice::<Value>(&write) {
                    let request_id = if task.parent_task_id.is_some() { &task.task_id } else { &task.request_id };
                    let stored = crate::storage::gateway::externalize_model_images(
                        &root, &task.scope.conversation_id, request_id, &value)?;
                    if stored != value { write = serde_json::to_vec(&stored).map_err(|_| {
                        GatewayError::new("INVALID_TASK_ARTIFACT", "Task image references cannot be encoded. / 任务图像引用无法编码。", 400)
                    })?; }
                }
            }
            if write.len() as u64 > MAX_BLOB_BYTES {
                return Err(GatewayError::new("TASK_ARTIFACT_TOO_LARGE",
                    "Task artifact exceeds the bounded blob capacity. / 任务产物超过有界正文容量。", 413));
            }
            let digest = managed_files::digest(&write);
            let relative = format!("Agent/task-artifacts/{}/{owned_kind}-{digest}.json", task.task_id);
            managed_files::atomic_write(&root, &root.join(&relative), &write, MAX_BLOB_BYTES)?;
            Ok((digest, relative, write.len() as u64))
        })
        .await
        .map_err(|_| {
            GatewayError::new(
                "TASK_ARTIFACT_WRITE_FAILED",
                "Task artifact writer ended unexpectedly. / 任务产物写入者意外退出。",
                500,
            )
        })??;
        let artifact_id = format!("{kind}-{digest}");
        let artifact = TaskArtifact {
            artifact_id: artifact_id.clone(),
            kind: kind.into(),
            reference: json!({"kind":"task-blob","taskId":self.task_id(),"artifactId":artifact_id,"relativePath":relative,"sha256":digest,"bytes":size_bytes}),
            sha256: digest,
            size_bytes,
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
    /// Hydrate only verified model context; receipt readers keep the immutable archived bytes.
    /// 仅还原已核验的模型上下文；回执读取仍使用不可变归档原文。
    pub async fn read_model_blob(&self, reference: &Value) -> Result<Vec<u8>> {
        let bytes = self.read_blob(reference).await?;
        let invalid = || GatewayError::new("INVALID_TASK_CHECKPOINT", "Task model context is invalid. / 任务模型上下文无效。", 409);
        let owner = self.repository.load(reference["taskId"].as_str().ok_or_else(invalid)?)?;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            let restored = Self::restore_model_images(&root, &owner, &value)?;
            serde_json::to_vec(&restored).map_err(|_| invalid())
        }).await.map_err(|_| invalid())?
    }
    fn restore_model_images(root: &Path, task: &TaskRecord, value: &Value) -> Result<Value> {
        if task.state == TaskState::Running {
            crate::storage::gateway::protect_model_images(root, &task.scope.conversation_id, value, 30 * 60 * 1000)?;
        }
        Ok(crate::storage::gateway::hydrate_model_images(root, &task.scope.conversation_id, value)?["value"].take())
    }
    pub async fn save_checkpoint(&self, checkpoint: &TaskCheckpoint) -> Result<TaskCheckpoint> {
        self.flush().await?;
        Ok(self.repository.save_checkpoint(&self.token, checkpoint)?)
    }
    /// Bind private root snapshots to request, scope and provider; they are not public tool evidence.
    /// 根私有快照绑定请求、范围及供应商，不作公开工具证据。
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
    /// Fall back for missing sidecars; reject corrupt or foreign snapshots as trusted context.
    /// 缺少侧车时安全回退，不信任损坏或跨任务快照。
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
        let root = self.root.clone();
        let restored = tokio::task::spawn_blocking(move || Self::restore_model_images(&root, &task, &envelope["context"]))
            .await.map_err(|_| invalid())??;
        Ok(Some((restored, reference.clone())))
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
        // Persist I/O outcomes before accounting so crashes retain verifiable facts.
        // 先保存 I/O 结果再结算，确保崩溃后仍可核验。
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

    /// Settle persisted pre-interruption evidence only, without model calls or tool replay.
    /// 仅结算中断前已保存证据，不调用模型或重放工具。
    pub async fn reconcile_receipts(&self) -> Result<Value> {
        let task = self.load_current()?;
        let events = self.repository.recovery_receipts(self.task_id())?;
        let mut resolved_operations = 0u64;
        let mut resolved_reservations = 0u64;
        let mut unconfirmed = Vec::new();
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
            unconfirmed.push(json!({"taskId":self.task_id(),"reservationId":reservation.reservation_id,
                "costUnknown":true}));
        }
        for operation in self.repository.operations(self.task_id())?.into_iter()
            .filter(|operation| matches!(operation.state, OperationState::Unknown | OperationState::Dispatched))
        {
            match self.reconcile_operation(&task, &operation, &events).await {
                Ok(true) => resolved_operations += 1,
                // Missing read receipts are not unknown writes and cannot block recovery.
                // 缺失读取回执不是未知写入，不阻塞恢复。
                _ if operation.intent.effect == OperationEffect::Observation => {},
                Ok(false) => unconfirmed.push(json!({"taskId":self.task_id(),"operationId":operation.intent.operation_id,
                    "toolCallId":operation.intent.tool_call_id,"toolName":operation.intent.tool_name})),
                Err(error) => unconfirmed.push(json!({"taskId":self.task_id(),"operationId":operation.intent.operation_id,
                    "toolCallId":operation.intent.tool_call_id,"toolName":operation.intent.tool_name,
                    "code":error.code,"message":error.message})),
            }
        }
        Ok(json!({"resolvedOperations":resolved_operations,"resolvedReservations":resolved_reservations,
            "unconfirmed":unconfirmed}))
    }

    /// Poll the journaled remote ID and settle its original operation once, without redispatch.
    /// 按日志中的远端 ID 查询并结算原操作一次，不重新派发。
    pub async fn reconcile_remote_tasks(&self, context: &crate::tools::gateway::GatewayToolContext) -> Result<Value> {
        let task = self.load_current()?;
        let mut resolved = 0;
        let mut pending = Vec::new();
        let events = self.repository.recovery_receipts(self.task_id())?;
        for operation in self.repository.operations(self.task_id())?.into_iter()
            .filter(|operation| matches!(operation.state, OperationState::Unknown | OperationState::Dispatched)) {
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
            // Identical transcript copies after steering remain one original call.
            // 插话复制的相同调用记录仍算同一次原始调用。
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
                        "completionProof":super::completion_evidence::from_receipt(&call, &receipt),
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
            // If final state/release already committed, retry cleanup without reusing the closed attempt.
            // 终态及释放已提交时，重试仅收尾，不再使用已结束尝试写入。
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
