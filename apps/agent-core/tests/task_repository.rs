//! Fault checks use only disposable task databases and synthetic archive references.
//! 故障检查只使用一次性任务库和虚构归档引用，不读取日常聊天或凭据。

use kynxa_agent_core_native::storage::{sqlite_connections, task_repository::*};
use rusqlite::{Connection, params};
use serde_json::json;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    repository: TaskRepository,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("kynxa-task-repository-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let repository = TaskRepository::open(&root).unwrap();
        Self { root, repository }
    }
    fn task(&self, request: &str, parent: Option<&TaskRecord>) -> TaskRecord {
        let mut input = new_task(request);
        if let Some(parent) = parent {
            input.parent_task_id = Some(parent.task_id.clone());
            input.scope = parent.scope.clone();
        }
        self.repository.accept_task(&input).unwrap()
    }
    fn attempt(&self, task: &TaskRecord) -> TaskAttempt {
        self.repository
            .start_attempt(&task.task_id, task.version, "fixture-worker", 60_000)
            .unwrap()
    }
    fn expire(&self, attempt: &TaskAttempt) {
        let mut attempt = attempt.clone();
        attempt.lease_expires_at_ms = 0;
        let connection = Connection::open(self.root.join("Agent/tasks.sqlite")).unwrap();
        connection
            .execute(
                "UPDATE task_attempts SET lease_expires_at_ms=0,document=? WHERE attempt_id=?",
                params![
                    serde_json::to_string(&attempt).unwrap(),
                    attempt.token.attempt_id
                ],
            )
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = sqlite_connections::close(&self.root, &self.root.join("Agent/tasks.sqlite"));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn new_task(request: &str) -> NewTask {
    NewTask {
        request_id: request.into(),
        goal: "Fix a synthetic fixture and verify its test".into(),
        acceptance: vec!["Targeted test passes".into()],
        scope: TaskScope {
            project_id: Some("01c71587-7759-4b60-8938-225fed06d34e".into()),
            conversation_id: "b9d20773-6e3f-4a69-a2da-83e07036c0d0".into(),
            permission_snapshot: json!({"mode":"work","allowedRootId":"synthetic-root","prohibitedApps":["Chrome","Edge"]}),
        },
        parent_task_id: None,
        dependency_ids: vec![],
        budget: TaskBudget::default(),
        input_reference: Some(json!({"kind":"conversation-message","messageId":"synthetic-user"})),
    }
}

#[test]
fn model_task_presentation_persists_without_new_work_or_usage_reset() {
    let fixture = Fixture::new();
    let task = fixture.task("model-task-presentation", None);
    let attempt = fixture.attempt(&task);
    fixture.repository.record_usage(&attempt.token, "presentation-usage", &TaskUsage {
        model_requests: 2,
        input_tokens: 4096,
        output_tokens: 1024,
        ..TaskUsage::default()
    }).unwrap();
    let described = fixture.repository.describe_task(&attempt.token, " 文档成本对照 ", true).unwrap();
    assert_eq!(described.task_id, task.task_id);
    assert_eq!(described.title.as_deref(), Some("文档成本对照"));
    assert_eq!(described.display_in_task_list, Some(true));
    let reopened = TaskRepository::open(&fixture.root).unwrap();
    let saved = reopened.load(&task.task_id).unwrap();
    assert_eq!(saved.title, described.title);
    assert_eq!(saved.usage.input_tokens, 4096);
    assert_eq!(saved.usage.output_tokens, 1024);
    assert_eq!(reopened.list_unfinished(16).unwrap().len(), 1);
    let hidden = reopened.describe_task(&attempt.token, "文档成本对照", false).unwrap();
    assert_eq!(hidden.display_in_task_list, Some(false));
    assert_eq!(hidden.usage, saved.usage);
    let mut legacy = serde_json::to_value(hidden).unwrap();
    legacy.as_object_mut().unwrap().remove("title");
    legacy.as_object_mut().unwrap().remove("displayInTaskList");
    let legacy: TaskRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.title, None);
    assert_eq!(legacy.display_in_task_list, None);
}

#[test]
fn paused_lookup_is_indexed_bounded_and_reopens_the_same_chat_checkpoint() {
    let fixture = Fixture::new();
    let input = new_task("paused-lookup-root");
    let conversation = &input.scope.conversation_id;
    let project = input.scope.project_id.as_deref();
    assert!(fixture.repository.paused_roots_for_conversation(conversation, project).unwrap().is_empty());
    let root = fixture.repository.accept_task(&input).unwrap();
    let child = fixture.task("paused-lookup-child", Some(&root));
    fixture.repository.control(&root.task_id, root.version, TaskControl::Pause).unwrap();

    let mut foreign_input = new_task("paused-lookup-other-chat");
    foreign_input.scope.conversation_id = uuid::Uuid::new_v4().to_string();
    let foreign = fixture.repository.accept_task(&foreign_input).unwrap();
    fixture.repository.control(&foreign.task_id, foreign.version, TaskControl::Pause).unwrap();
    let mut old_scope_input = new_task("paused-lookup-old-work");
    old_scope_input.scope.project_id = Some(uuid::Uuid::new_v4().to_string());
    let old_scope = fixture.repository.accept_task(&old_scope_input).unwrap();
    fixture.repository.control(&old_scope.task_id, old_scope.version, TaskControl::Pause).unwrap();
    let unique = fixture.repository.paused_roots_for_conversation(conversation, project).unwrap();
    assert_eq!(unique.len(), 1);
    assert_eq!(unique[0].task_id, root.task_id);
    assert_eq!(fixture.repository.load(&child.task_id).unwrap().state, TaskState::Paused);

    let second = fixture.task("paused-lookup-second", None);
    fixture.repository.control(&second.task_id, second.version, TaskControl::Pause).unwrap();
    assert_eq!(fixture.repository.paused_roots_for_conversation(conversation, project).unwrap().len(), 2);
    let third = fixture.task("paused-lookup-third", None);
    fixture.repository.control(&third.task_id, third.version, TaskControl::Pause).unwrap();
    let reopened = TaskRepository::open(&fixture.root).unwrap();
    let ambiguous = reopened.paused_roots_for_conversation(conversation, project).unwrap();
    assert_eq!(ambiguous.len(), 2);
    assert!(ambiguous.iter().all(|task| task.parent_task_id.is_none()
        && task.state == TaskState::Paused && task.scope == input.scope));

    // Verify the actual lookup shape can search the partial index rather than scan ordinary task history.
    // 核对真实查询形状可搜索暂停根任务部分索引，不扫描普通任务历史。
    let connection = Connection::open(fixture.root.join("Agent/tasks.sqlite")).unwrap();
    let mut explain = connection.prepare("EXPLAIN QUERY PLAN SELECT document FROM tasks WHERE conversation_id=?1 AND project_id IS ?2 AND parent_task_id IS NULL AND state='paused' ORDER BY task_id LIMIT 2").unwrap();
    let plan: Vec<String> = explain.query_map(params![conversation, project], |row| row.get(3)).unwrap()
        .map(|row| row.unwrap()).collect();
    assert!(plan.iter().any(|step| step.contains("SEARCH tasks USING INDEX tasks_paused_roots")), "{plan:?}");
}

#[test]
fn user_steering_preserves_identity_scope_and_supersedes_only_obsolete_delegates() {
    let fixture = Fixture::new();
    let task = fixture.task("steering-root", None);
    let child = fixture.task("steering-child", Some(&task));
    let paused = fixture
        .repository
        .control(&task.task_id, task.version, TaskControl::Pause)
        .unwrap();
    let message = uuid::Uuid::new_v4().to_string();
    let hash = "a".repeat(64);
    let changed = fixture
        .repository
        .record_user_steering(&task.task_id, paused.version, &message, "update", &hash)
        .unwrap();
    assert_eq!(changed.task_id, task.task_id);
    assert_eq!(changed.scope, task.scope);
    assert_eq!(changed.goal, task.goal);
    assert_eq!(
        fixture.repository.load(&child.task_id).unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(
        fixture
            .repository
            .load(&child.task_id)
            .unwrap()
            .stop_reason
            .as_deref(),
        Some("superseded-by-user-steering")
    );
    let replay = fixture
        .repository
        .record_user_steering(&task.task_id, paused.version, &message, "update", &hash)
        .unwrap();
    assert_eq!(replay.version, changed.version);
    assert_eq!(
        fixture
            .repository
            .user_steering(&task.task_id)
            .unwrap()
            .len(),
        1
    );
    assert!(
        fixture
            .repository
            .record_user_steering(&task.task_id, changed.version, &message, "continue", &hash)
            .is_err()
    );
    let resumed = fixture
        .repository
        .control(&task.task_id, changed.version, TaskControl::Resume)
        .unwrap();
    assert_eq!(resumed.state, TaskState::Queued);
    assert_eq!(
        fixture.repository.load(&child.task_id).unwrap().state,
        TaskState::Cancelled
    );
    let attempt = fixture.attempt(&resumed);
    let current = fixture.repository.load(&task.task_id).unwrap();
    fixture
        .repository
        .transition(&attempt.token, current.version, TaskState::Completed, None)
        .unwrap();
}
#[test]
fn running_user_steering_retains_execution_and_unfinished_children() {
    let fixture = Fixture::new();
    let task = fixture.task("running-steering-root", None);
    let child = fixture.task("running-steering-child", Some(&task));
    let attempt = fixture.attempt(&task);
    let running = fixture.repository.load(&task.task_id).unwrap();
    let changed = fixture.repository.record_user_steering(
        &task.task_id,
        running.version,
        &uuid::Uuid::new_v4().to_string(),
        "update",
        &"a".repeat(64),
    ).unwrap();
    assert_eq!(changed.state, TaskState::Running);
    assert_eq!(changed.current_attempt_id, running.current_attempt_id);
    assert_eq!(changed.execution_version, running.execution_version);
    assert_eq!(changed.stop_reason, running.stop_reason);
    assert_eq!(fixture.repository.load(&child.task_id).unwrap().state, child.state);
    fixture.repository.transition(&attempt.token, changed.version, TaskState::WaitingChildren, None).unwrap();
}

fn operation(id: &str) -> OperationIntent {
    OperationIntent {
        operation_id: id.into(),
        tool_call_id: format!("call-{id}"),
        tool_name: "filesystem.write".into(),
        input_sha256: "a".repeat(64),
        input_reference: Some(json!({"kind":"tool-call","id":format!("call-{id}")})),
        effect: OperationEffect::FileMutation,
    }
}

#[test]
fn durable_accept_replays_once_and_rejects_changed_input() {
    let fixture = Fixture::new();
    let task = fixture.task("request-1", None);
    assert_eq!(
        fixture
            .repository
            .accept_task(&new_task("request-1"))
            .unwrap()
            .task_id,
        task.task_id
    );
    let reopened = TaskRepository::open(&fixture.root).unwrap();
    assert_eq!(reopened.load(&task.task_id).unwrap().goal, task.goal);
    assert_eq!(reopened.list_unfinished(20).unwrap().len(), 1);
    let mut changed = new_task("request-1");
    changed.goal = "Different work".into();
    assert_eq!(
        reopened.accept_task(&changed).unwrap_err().code(),
        "TASK_CONFLICT"
    );
    assert_eq!(reopened.read_events(&task.task_id, 0, 20).unwrap().len(), 1);
}

#[test]
fn scoped_dependencies_reject_cycles_and_wait_for_success() {
    let fixture = Fixture::new();
    let parent = fixture.task("parent", None);
    let a = fixture.task("a", Some(&parent));
    let b = fixture.task("b", Some(&parent));
    let b = fixture
        .repository
        .set_dependencies(&b.task_id, b.version, std::slice::from_ref(&a.task_id))
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .start_attempt(&b.task_id, b.version, "worker", 60_000)
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    assert_eq!(
        fixture
            .repository
            .set_dependencies(&a.task_id, a.version, std::slice::from_ref(&b.task_id))
            .unwrap_err()
            .code(),
        "INVALID_TASK"
    );
    assert_eq!(
        fixture
            .repository
            .set_dependencies(&a.task_id, a.version, std::slice::from_ref(&parent.task_id))
            .unwrap_err()
            .code(),
        "INVALID_TASK"
    );
    let attempt = fixture.attempt(&a);
    let a = fixture.repository.load(&a.task_id).unwrap();
    fixture
        .repository
        .transition(
            &attempt.token,
            a.version,
            TaskState::Completed,
            Some("verified"),
        )
        .unwrap();
    assert!(
        fixture
            .repository
            .start_attempt(&b.task_id, b.version, "worker", 60_000)
            .is_ok()
    );
    let mut outside = new_task("outside");
    outside.parent_task_id = Some(parent.task_id.clone());
    outside.scope.permission_snapshot["mode"] = json!("full");
    assert_eq!(
        fixture.repository.accept_task(&outside).unwrap_err().code(),
        "TASK_BLOCKED"
    );
}

#[test]
fn takeover_preserves_unknown_effect_and_fences_old_executor() {
    let fixture = Fixture::new();
    let task = fixture.task("takeover", None);
    let first = fixture.attempt(&task);
    fixture
        .repository
        .record_operation(&first.token, &operation("write-a"))
        .unwrap();
    fixture
        .repository
        .dispatch_operation(&first.token, "write-a")
        .unwrap();
    fixture.expire(&first);
    let task = fixture.repository.load(&task.task_id).unwrap();
    let next = fixture
        .repository
        .start_attempt(&task.task_id, task.version, "replacement", 60_000)
        .unwrap();
    assert_eq!(
        next.token.execution_version,
        first.token.execution_version + 1
    );
    assert_eq!(
        fixture.repository.load(&task.task_id).unwrap().state,
        TaskState::WaitingVerification
    );
    assert_eq!(
        fixture
            .repository
            .append_event(&first.token, "late", "text", &json!("late"))
            .unwrap_err()
            .code(),
        "STALE_TASK_ATTEMPT"
    );
    assert_eq!(
        fixture
            .repository
            .dispatch_operation(&next.token, "write-a")
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    assert_eq!(
        fixture
            .repository
            .record_operation(&next.token, &operation("write-b"))
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    assert_eq!(
        fixture
            .repository
            .finish_operation(
                &next.token,
                "write-a",
                OperationState::Succeeded,
                Some(json!({"resultId":"original"})),
                None
            )
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    fixture
        .repository
        .finish_operation(
            &next.token,
            "write-a",
            OperationState::Succeeded,
            Some(json!({"resultId":"original"})),
            Some(json!({"kind":"source-read","sha256":"verified"})),
        )
        .unwrap();
    assert!(
        fixture
            .repository
            .record_operation(&next.token, &operation("write-b"))
            .is_ok()
    );
    let existing = fixture
        .repository
        .record_operation(&next.token, &operation("write-a"))
        .unwrap();
    assert_eq!(existing.state, OperationState::Succeeded);
    assert_eq!(
        fixture
            .repository
            .dispatch_operation(&next.token, "write-a")
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
}

#[test]
fn cancel_is_durable_cascades_and_accepts_only_already_dispatched_receipt() {
    let fixture = Fixture::new();
    let parent = fixture.task("cancel-parent", None);
    let child = fixture.task("cancel-child", Some(&parent));
    let attempt = fixture.attempt(&child);
    fixture
        .repository
        .record_operation(&attempt.token, &operation("before-cancel"))
        .unwrap();
    fixture
        .repository
        .dispatch_operation(&attempt.token, "before-cancel")
        .unwrap();
    let cancelled = fixture
        .repository
        .control(&parent.task_id, parent.version, TaskControl::Cancel)
        .unwrap();
    let replay = fixture
        .repository
        .control(&parent.task_id, parent.version, TaskControl::Cancel)
        .unwrap();
    assert_eq!(replay.version, cancelled.version);
    assert_eq!(
        fixture.repository.load(&child.task_id).unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(
        fixture
            .repository
            .record_operation(&attempt.token, &operation("after-cancel"))
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    fixture
        .repository
        .finish_operation(
            &attempt.token,
            "before-cancel",
            OperationState::Succeeded,
            Some(json!({"resultId":"late-receipt"})),
            None,
        )
        .unwrap();
    assert_eq!(
        fixture.repository.load(&child.task_id).unwrap().state,
        TaskState::Cancelled
    );
    let child = fixture.repository.load(&child.task_id).unwrap();
    assert_eq!(
        fixture
            .repository
            .control(&child.task_id, child.version, TaskControl::Resume)
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    assert!(
        TaskRepository::open(&fixture.root)
            .unwrap()
            .list_unfinished(50)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn parent_pause_resume_preserves_independently_paused_children() {
    let fixture = Fixture::new();
    let parent = fixture.task("pause-parent", None);
    let child = fixture.task("pause-child", Some(&parent));
    let own = fixture.task("own-pause", Some(&parent));
    fixture
        .repository
        .control(&own.task_id, own.version, TaskControl::Pause)
        .unwrap();
    let parent = fixture
        .repository
        .control(&parent.task_id, parent.version, TaskControl::Pause)
        .unwrap();
    assert_eq!(
        fixture.repository.load(&child.task_id).unwrap().state,
        TaskState::Paused
    );
    let child = fixture.repository.load(&child.task_id).unwrap();
    assert_eq!(
        fixture
            .repository
            .control(&child.task_id, child.version, TaskControl::Resume)
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    let resumed = fixture
        .repository
        .control(&parent.task_id, parent.version, TaskControl::Resume)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .control(&parent.task_id, parent.version, TaskControl::Resume)
            .unwrap()
            .version,
        resumed.version
    );
    assert_eq!(
        fixture.repository.load(&child.task_id).unwrap().state,
        TaskState::Queued
    );
    assert_eq!(
        fixture.repository.load(&own.task_id).unwrap().state,
        TaskState::Paused
    );
}

#[test]
fn event_replay_checkpoints_artifacts_and_parent_usage_are_persistent_and_idempotent() {
    let fixture = Fixture::new();
    let mut input = new_task("meter-parent");
    input.budget.max_model_requests = 2;
    let parent = fixture.repository.accept_task(&input).unwrap();
    let child = fixture.task("meter-child", Some(&parent));
    let attempt = fixture.attempt(&child);
    let event = fixture
        .repository
        .append_event(
            &attempt.token,
            "chunk-1",
            "output",
            &json!({"reference":{"resultId":"chunk"}}),
        )
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .append_event(&attempt.token, "chunk-1", "output", &event.payload)
            .unwrap()
            .sequence,
        event.sequence
    );
    assert_eq!(
        fixture
            .repository
            .append_event(&attempt.token, "chunk-1", "output", &json!("changed"))
            .unwrap_err()
            .code(),
        "TASK_CONFLICT"
    );
    let checkpoint = TaskCheckpoint {
        checkpoint_id: "checkpoint-1".into(),
        through_event_sequence: event.sequence,
        context_reference: json!({"kind":"context-archive","id":"synthetic-context"}),
        next_step: json!({"gap":"verify test"}),
        source_versions: vec![],
    };
    fixture
        .repository
        .save_checkpoint(&attempt.token, &checkpoint)
        .unwrap();
    let artifact = TaskArtifact {
        artifact_id: "report".into(),
        kind: "document".into(),
        reference: json!({"kind":"tool-result","id":"report-result"}),
        sha256: "b".repeat(64),
        size_bytes: 240,
        source_version: Some("v1".into()),
    };
    fixture
        .repository
        .save_artifact(&attempt.token, &artifact)
        .unwrap();
    let usage = TaskUsage {
        model_requests: 2,
        failed_requests: 1,
        retries: 1,
        compactions: 1,
        input_tokens: 42,
        output_tokens: 18,
        unknown_token_requests: 1,
        ..TaskUsage::default()
    };
    fixture
        .repository
        .record_usage(&attempt.token, "actual-http-1", &usage)
        .unwrap();
    fixture
        .repository
        .record_usage(&attempt.token, "actual-http-1", &usage)
        .unwrap();
    let reopened = TaskRepository::open(&fixture.root).unwrap();
    assert_eq!(reopened.load(&parent.task_id).unwrap().usage, usage);
    assert_eq!(
        reopened
            .latest_checkpoint(&child.task_id)
            .unwrap()
            .unwrap()
            .checkpoint_id,
        "checkpoint-1"
    );
    assert_eq!(reopened.artifacts(&child.task_id).unwrap().len(), 1);
    let first = reopened.read_events(&child.task_id, 0, 1).unwrap();
    let rest = reopened
        .read_events(&child.task_id, first[0].sequence, 50)
        .unwrap();
    assert!(rest.iter().all(|event| event.sequence > first[0].sequence));
    fixture
        .repository
        .release_attempt(&attempt.token, "waiting")
        .unwrap();
    let child = fixture.repository.load(&child.task_id).unwrap();
    assert_eq!(
        fixture
            .repository
            .start_attempt(&child.task_id, child.version, "next", 60_000)
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
}

#[test]
fn parent_cannot_claim_completion_when_children_are_partial() {
    let fixture = Fixture::new();
    let parent = fixture.task("outcome-parent", None);
    let child = fixture.task("outcome-child", Some(&parent));
    let child_attempt = fixture.attempt(&child);
    let child = fixture.repository.load(&child.task_id).unwrap();
    fixture
        .repository
        .transition(
            &child_attempt.token,
            child.version,
            TaskState::Partial,
            Some("one-source-unavailable"),
        )
        .unwrap();
    let parent_attempt = fixture.attempt(&parent);
    let parent = fixture.repository.load(&parent.task_id).unwrap();
    assert_eq!(
        fixture
            .repository
            .transition(
                &parent_attempt.token,
                parent.version,
                TaskState::Completed,
                None
            )
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    assert!(
        fixture
            .repository
            .transition(
                &parent_attempt.token,
                parent.version,
                TaskState::Partial,
                Some("child-partial")
            )
            .is_ok()
    );
}

#[test]
fn concurrent_children_reserve_one_parent_budget_and_unsent_release_is_free() {
    let fixture = Fixture::new();
    let mut input = new_task("reservation-parent");
    input.budget.max_model_requests = 2;
    let parent = fixture.repository.accept_task(&input).unwrap();
    let a = fixture.task("reservation-a", Some(&parent));
    let b = fixture.task("reservation-b", Some(&parent));
    let a_attempt = fixture.attempt(&a);
    let b_attempt = fixture.attempt(&b);
    let first = fixture
        .repository
        .reserve_model_request(&a_attempt.token, "a-http-1")
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .reserve_model_request(&a_attempt.token, "a-http-1")
            .unwrap()
            .reservation_id,
        first.reservation_id
    );
    fixture
        .repository
        .reserve_model_request(&b_attempt.token, "b-http-1")
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .reserve_model_request(&a_attempt.token, "a-http-2")
            .unwrap_err()
            .code(),
        "TASK_BUDGET_EXHAUSTED"
    );
    fixture
        .repository
        .settle_model_request(&b_attempt.token, "b-http-1", false, &TaskUsage::default())
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .load(&parent.task_id)
            .unwrap()
            .usage
            .model_requests,
        0
    );
    fixture
        .repository
        .reserve_model_request(&a_attempt.token, "a-http-2")
        .unwrap();
    let actual = TaskUsage {
        model_requests: 0,
        failed_requests: 1,
        retries: 1,
        compactions: 1,
        unknown_token_requests: 1,
        ..TaskUsage::default()
    };
    fixture
        .repository
        .settle_model_request(&a_attempt.token, "a-http-1", true, &actual)
        .unwrap();
    fixture
        .repository
        .settle_model_request(&a_attempt.token, "a-http-1", true, &actual)
        .unwrap();
    let usage = fixture.repository.load(&parent.task_id).unwrap().usage;
    assert_eq!(usage.model_requests, 1);
    assert_eq!(usage.failed_requests, 1);
    assert_eq!(usage.unknown_token_requests, 1);
    assert_eq!(
        fixture
            .repository
            .reserve_model_request(&b_attempt.token, "b-http-2")
            .unwrap_err()
            .code(),
        "TASK_BUDGET_EXHAUSTED"
    );
    let scoped = fixture
        .repository
        .list_for_scope(
            Some(&a.scope.conversation_id),
            Some(a.scope.project_id.as_deref().unwrap()),
            None,
            20,
        )
        .unwrap();
    assert_eq!(scoped.len(), 3);
    assert_eq!(
        fixture
            .repository
            .find_by_request_id(&a.scope.conversation_id, "reservation-a")
            .unwrap()
            .unwrap()
            .task_id,
        a.task_id
    );
}

#[test]
fn crash_reservation_stays_unknown_until_original_dispatch_is_verified() {
    let fixture = Fixture::new();
    let mut input = new_task("uncertain-model");
    input.budget.max_model_requests = 1;
    let task = fixture.repository.accept_task(&input).unwrap();
    let first = fixture.attempt(&task);
    fixture
        .repository
        .reserve_model_request(&first.token, "uncertain-http")
        .unwrap();
    fixture.expire(&first);
    let task = fixture.repository.load(&task.task_id).unwrap();
    let next = fixture
        .repository
        .start_attempt(&task.task_id, task.version, "new-worker", 60_000)
        .unwrap();
    let reservation = fixture
        .repository
        .model_reservations(&task.task_id)
        .unwrap()
        .remove(0);
    assert_eq!(reservation.state, ModelReservationState::Unknown);
    assert_eq!(
        fixture
            .repository
            .settle_model_request(&next.token, "uncertain-http", false, &TaskUsage::default())
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    assert_eq!(
        fixture
            .repository
            .reserve_model_request(&next.token, "next-http")
            .unwrap_err()
            .code(),
        "TASK_BUDGET_EXHAUSTED"
    );
    fixture
        .repository
        .reconcile_model_request(
            &next.token,
            "uncertain-http",
            false,
            &TaskUsage::default(),
            json!({"kind":"dispatch-audit","requestDispatched":false}),
        )
        .unwrap();
    assert!(
        fixture
            .repository
            .reserve_model_request(&next.token, "next-http")
            .is_ok()
    );
    assert_eq!(
        fixture
            .repository
            .load(&task.task_id)
            .unwrap()
            .usage
            .model_requests,
        0
    );
}

#[test]
fn recoverable_wait_resumes_once_without_replaying_unknown_writes() {
    let fixture = Fixture::new();
    let task = fixture.task("network-wait", None);
    let attempt = fixture.attempt(&task);
    let task = fixture.repository.load(&task.task_id).unwrap();
    let task = fixture
        .repository
        .transition(
            &attempt.token,
            task.version,
            TaskState::WaitingNetwork,
            Some("network-temporary"),
        )
        .unwrap();
    fixture
        .repository
        .release_attempt(&attempt.token, "network-wait")
        .unwrap();
    let resumed = fixture
        .repository
        .control(&task.task_id, task.version, TaskControl::Resume)
        .unwrap();
    let replay = fixture
        .repository
        .control(&task.task_id, task.version, TaskControl::Resume)
        .unwrap();
    assert_eq!(resumed.version, replay.version);
    assert!(
        fixture
            .repository
            .start_attempt(&task.task_id, resumed.version, "next", 60_000)
            .is_ok()
    );
}
