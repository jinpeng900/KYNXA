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
fn reservation_usage_projection_keeps_scope_cache_coverage_and_exactly_once_child_costs() {
    let fixture = Fixture::new();
    let root = fixture.task("usage-root", None);
    let parent = fixture.attempt(&root);
    let child = fixture.task("usage-child", Some(&root));
    let child_attempt = fixture.attempt(&child);
    let usage = TaskUsage { input_tokens:100, output_tokens:10, ..Default::default() };
    let provider = ProviderTokenUsage { input_tokens:Some(100), output_tokens:Some(10),
        cache_read_tokens:Some(80), cache_creation_tokens:Some(10), uncached_input_tokens:Some(10),
        client_metrics:Some(ModelRequestClientMetrics {first_visible_text_ms:Some(125),
            cache_policy:Some(json!({"provider":"openai","mode":"implicit","breakpoints":0,
                "prefix":{"schemaVersion":1,"scope":"main-request","state":"compared","breakAt":"tools","lostTokensEstimate":120}}))}) };
    fixture.repository.reserve_model_request(&parent.token, "root-request").unwrap();
    for _ in 0..2 {
        fixture.repository.settle_model_request_with_provider_usage(&parent.token, "root-request", true,
            &usage, Some(provider.clone())).unwrap();
    }
    let mut changed_provider = provider;
    changed_provider.cache_read_tokens = Some(81);
    changed_provider.uncached_input_tokens = Some(9);
    assert!(fixture.repository.settle_model_request_with_provider_usage(&parent.token, "root-request", true,
        &usage, Some(changed_provider)).is_err(), "settled cache facts are immutable too");
    fixture.repository.reserve_model_request(&child_attempt.token, "child-request").unwrap();
    fixture.repository.settle_model_request_with_provider_usage(&child_attempt.token, "child-request", true,
        &TaskUsage { input_tokens:200, output_tokens:20, ..Default::default() },
        Some(ProviderTokenUsage { input_tokens:Some(200), output_tokens:Some(20), cache_read_tokens:Some(0), cache_creation_tokens:None, ..Default::default() })).unwrap();
    fixture.repository.reserve_model_request(&parent.token, "legacy-request").unwrap();
    fixture.repository.settle_model_request(&parent.token, "legacy-request", true, &usage).unwrap();
    fixture.repository.reserve_model_request(&parent.token, "uncertain-request").unwrap();
    fixture.repository.mark_model_request_unknown(&parent.token, "uncertain-request", "fixture-disconnected").unwrap();
    let summary = fixture.repository.usage_summary(Some(&root.scope.conversation_id), None).unwrap();
    assert_eq!(summary["modelRequests"], 3);
    assert_eq!(summary["inputTokens"], 400);
    assert_eq!(summary["outputTokens"], 40);
    assert_eq!(summary["knownTokenRequests"], 3);
    assert_eq!(summary["unknownTokenRequests"], 1);
    assert_eq!(summary["pendingRequests"], 1);
    assert_eq!(summary["knownCacheRequests"], 2);
    assert_eq!(summary["prefixDiagnostics"]["observedMainRequests"], 1);
    assert_eq!(summary["prefixDiagnostics"]["bySegment"]["tools"], 1);
    assert_eq!(summary["prefixDiagnostics"]["lostTokensEstimate"], 120);
    assert_eq!(summary["cacheInputTokens"], 300);
    assert_eq!(summary["cacheReadTokens"], 80);
    assert_eq!(summary["cacheCreationTokens"], 10);
    assert_eq!(summary["uncachedInputTokens"], 10);
    assert_eq!(summary["knownCacheCreationRequests"], 1);
    assert_eq!(summary["knownUncachedRequests"], 1);
    assert_eq!(summary["knownFirstVisibleTextRequests"], 1);
    assert_eq!(summary["firstVisibleTextMsTotal"], 125);
    assert_eq!(summary["firstVisibleTextMs"], 125.0);
    assert!((summary["cacheHitRate"].as_f64().unwrap() - 80.0 / 300.0).abs() < 0.0001);
    assert_eq!(fixture.repository.load(&root.task_id).unwrap().usage.model_requests, 3);
    let mut other = new_task("other-chat-usage");
    other.scope.conversation_id = "7314105d-0266-4191-ac1e-7b7102a4f8c8".into();
    let other = fixture.repository.accept_task(&other).unwrap();
    let other_attempt = fixture.attempt(&other);
    fixture.repository.reserve_model_request(&other_attempt.token, "other-request").unwrap();
    fixture.repository.settle_model_request(&other_attempt.token, "other-request", true, &usage).unwrap();
    assert_eq!(fixture.repository.usage_summary(Some(&root.scope.conversation_id), None).unwrap()["modelRequests"], 3);
    assert_eq!(fixture.repository.usage_summary(None, root.scope.project_id.as_deref()).unwrap()["modelRequests"], 4);
    assert!(fixture.repository.usage_summary(None, None).is_err());
}

#[test]
fn reservation_usage_projection_keeps_partial_tokens_and_unknown_cache_as_unknown() {
    let fixture = Fixture::new();
    let task = fixture.task("partial-provider-usage", None);
    let attempt = fixture.attempt(&task);
    fixture.repository.reserve_model_request(&attempt.token, "partial").unwrap();
    fixture.repository.settle_model_request_with_provider_usage(&attempt.token, "partial", true,
        &TaskUsage { input_tokens:50, unknown_token_requests:1, failed_requests:1, ..Default::default() },
        Some(ProviderTokenUsage { input_tokens:Some(50), ..Default::default() })).unwrap();
    let summary = fixture.repository.usage_summary(Some(&task.scope.conversation_id), None).unwrap();
    assert_eq!(summary["modelRequests"], 1);
    assert_eq!(summary["inputTokens"], 50);
    assert_eq!(summary["unknownTokenRequests"], 1);
    assert_eq!(summary["knownTokenRequests"], 0);
    assert_eq!(summary["knownCacheRequests"], 0);
    assert!(summary["cacheHitRate"].is_null());
    assert!(summary["cacheCreationTokens"].is_null());
    assert!(summary["uncachedInputTokens"].is_null());
    assert!(summary["firstVisibleTextMs"].is_null());
    assert_eq!(summary["knownFirstVisibleTextRequests"], 0);
    // A stream can end after cache accounting but before its total input count; the task must remain resumable.
    // 流可能在缓存计量后、输入总数前中断；保留部分事实，不能因补计量阻挡原任务续接。
    fixture.repository.reserve_model_request(&attempt.token, "cache-only").unwrap();
    let partial = ProviderTokenUsage { cache_read_tokens:Some(80),
        client_metrics:Some(ModelRequestClientMetrics {first_visible_text_ms:Some(42),cache_policy:None}),
        ..Default::default() };
    fixture.repository.settle_model_request_with_provider_usage(&attempt.token, "cache-only", true,
        &TaskUsage { unknown_token_requests:1, failed_requests:1, ..Default::default() },Some(partial.clone())).unwrap();
    assert_eq!(fixture.repository.model_reservations(&task.task_id).unwrap().into_iter()
        .find(|reservation|reservation.reservation_id=="cache-only").unwrap().provider_usage,Some(partial));
    let summary = fixture.repository.usage_summary(Some(&task.scope.conversation_id), None).unwrap();
    assert_eq!(summary["modelRequests"], 2);
    assert_eq!(summary["pendingRequests"], 0);
    assert_eq!(summary["unknownTokenRequests"], 2);
    assert_eq!(summary["knownFirstVisibleTextRequests"], 1);
    fixture.repository.reserve_model_request(&attempt.token, "can-continue").unwrap();
}

#[test]
fn old_provider_receipts_keep_their_serialized_settlement_identity() {
    let old = json!({"inputTokens":100,"outputTokens":10,"cacheReadTokens":80,"cacheCreationTokens":null});
    let decoded: ProviderTokenUsage = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(decoded.uncached_input_tokens, None);
    assert_eq!(serde_json::to_value(decoded).unwrap(), old);
}

fn legacy_fixture() -> Fixture {
    let fixture = Fixture::new();
    let connection = Connection::open(fixture.root.join("Agent/tasks.sqlite")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    for table in ["task_cleanup_plans", "task_model_reservations", "task_usage", "task_artifacts", "task_checkpoints", "task_operations", "task_events", "task_attempts", "task_dependencies", "tasks"] {
        connection.execute_batch(&format!("DROP TABLE {table};")).unwrap();
    }
    let source = include_str!("../src/storage/task_repository_schema.rs").replace("\r\n", "\n");
    let legacy = source.split("r#\"").nth(1).unwrap().split("\"#;").next().unwrap()
        .replace("TEXT NOT NULL PRIMARY KEY", "TEXT PRIMARY KEY")
        .replace(",\n UNIQUE(task_id,attempt_id)", "")
        .replace(",\n FOREIGN KEY(task_id,attempt_id) REFERENCES task_attempts(task_id,attempt_id)", "")
        .replace("PRAGMA user_version=2", "PRAGMA user_version=1");
    connection.execute_batch(&legacy).unwrap();
    fixture
}

#[test]
fn legacy_integrity_migration_preserves_real_records_and_enforces_attempt_ownership() {
    let fixture = legacy_fixture();
    let first = fixture.task("legacy-first", None);
    let second = fixture.task("legacy-second", None);
    let first_attempt = fixture.attempt(&first);
    let second_attempt = fixture.attempt(&second);
    fixture.repository.record_operation(&first_attempt.token, &operation("original-operation")).unwrap();
    fixture.repository.reserve_model_request(&first_attempt.token, "original-reservation").unwrap();
    let before = fixture.repository.load(&first.task_id).unwrap();
    sqlite_connections::close(&fixture.root, &fixture.root.join("Agent/tasks.sqlite")).unwrap();
    let repository = TaskRepository::open(&fixture.root).unwrap();
    assert_eq!(serde_json::to_value(repository.load(&first.task_id).unwrap()).unwrap(), serde_json::to_value(before).unwrap());
    let connection = Connection::open(fixture.root.join("Agent/tasks.sqlite")).unwrap();
    assert_eq!(connection.query_row("PRAGMA user_version", [], |row|row.get::<_,i64>(0)).unwrap(), 2);
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    assert!(connection.execute("UPDATE task_operations SET attempt_id=? WHERE task_id=?", params![second_attempt.token.attempt_id,first.task_id]).is_err());
    assert!(connection.execute("UPDATE task_model_reservations SET attempt_id=? WHERE task_id=?", params![second_attempt.token.attempt_id,first.task_id]).is_err());
    for (table, key) in [("tasks","task_id"), ("task_attempts","attempt_id"), ("task_cleanup_plans","cleanup_id")] {
        let required: i64 = connection.query_row("SELECT \"notnull\" FROM pragma_table_info(?) WHERE name=?",params![table,key], |row|row.get(0)).unwrap();
        assert_eq!(required,1);
    }
    assert_eq!(repository.operations(&first.task_id).unwrap().len(),1);
    assert_eq!(repository.model_reservations(&first.task_id).unwrap().len(),1);
}

#[test]
fn invalid_legacy_ownership_rolls_back_without_deleting_or_rewriting_records() {
    let fixture = legacy_fixture();
    let first = fixture.task("legacy-invalid-first", None);
    let second = fixture.task("legacy-invalid-second", None);
    let first_attempt = fixture.attempt(&first);
    let second_attempt = fixture.attempt(&second);
    fixture.repository.record_operation(&first_attempt.token, &operation("preserved-operation")).unwrap();
    sqlite_connections::close(&fixture.root, &fixture.root.join("Agent/tasks.sqlite")).unwrap();
    let path=fixture.root.join("Agent/tasks.sqlite");
    let connection=Connection::open(&path).unwrap();
    connection.execute("UPDATE task_operations SET attempt_id=? WHERE task_id=?",params![second_attempt.token.attempt_id,first.task_id]).unwrap();
    let original:String=connection.query_row("SELECT document FROM task_operations",[],|row|row.get(0)).unwrap();
    drop(connection);
    let error=TaskRepository::open(&fixture.root).err().expect("invalid ownership must prevent migration");
    assert_eq!(error.code(),"STORAGE_INTEGRITY_MIGRATION_REQUIRED");
    let connection=Connection::open(path).unwrap();
    assert_eq!(connection.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(connection.query_row("SELECT document FROM task_operations",[],|row|row.get::<_,String>(0)).unwrap(),original);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '__kynxa_integrity_%'",[],|row|row.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn effective_wall_time_is_attempt_idempotent_and_parallel_child_costs_do_not_inflate_it() {
    let fixture=Fixture::new();
    let mut input=new_task("wall-clock-root");
    input.budget.max_execution_ms=Some(10_000);
    let root=fixture.repository.accept_task(&input).unwrap();
    let parent=fixture.attempt(&root);
    let child_a=fixture.task("wall-child-a",Some(&root));
    let child_b=fixture.task("wall-child-b",Some(&root));
    let a=fixture.attempt(&child_a);
    let b=fixture.attempt(&child_b);
    fixture.repository.record_usage(&a.token,"cost-a",&TaskUsage{model_duration_ms:30_000,tool_duration_ms:20_000,..Default::default()}).unwrap();
    fixture.repository.record_usage(&b.token,"cost-b",&TaskUsage{tool_duration_ms:40_000,..Default::default()}).unwrap();
    fixture.repository.record_active_duration(&a.token,3_000).unwrap();
    fixture.repository.record_active_duration(&b.token,4_000).unwrap();
    let task=fixture.repository.record_active_duration(&parent.token,5_000).unwrap();
    assert_eq!(task.usage.active_duration_ms,Some(5_000));
    assert_eq!(task.usage.model_duration_ms+task.usage.tool_duration_ms,90_000);
    assert!(!task.usage.is_exhausted(&task.budget));
    assert_eq!(fixture.repository.record_active_duration(&parent.token,4_000).unwrap().usage.active_duration_ms,Some(5_000));
    assert_eq!(fixture.repository.record_active_duration(&parent.token,5_000).unwrap().usage.active_duration_ms,Some(5_000));
    fixture.repository.release_attempt(&a.token,"segment-ended").unwrap();
    let previous=fixture.repository.load(&child_a.task_id).unwrap();
    let next=fixture.repository.start_attempt(&child_a.task_id,previous.version,"next-segment",60_000).unwrap();
    assert_eq!(fixture.repository.record_active_duration(&next.token,1_000).unwrap().usage.active_duration_ms,Some(4_000));
    assert!(fixture.repository.record_active_duration(&a.token,9_000).is_err());
    assert_eq!(fixture.repository.load(&root.task_id).unwrap().usage.active_duration_ms,Some(5_000));
    let task=fixture.repository.record_active_duration(&parent.token,10_000).unwrap();
    assert!(task.usage.is_exhausted(&task.budget));
    assert!(fixture.repository.record_usage(&parent.token,"forged-wall",&TaskUsage{active_duration_ms:Some(99_000),..Default::default()}).is_err());
}

#[test]
fn partial_work_can_resume_same_identity_without_reviving_completed_or_cancelled_work() {
    let fixture=Fixture::new();
    let task=fixture.task("partial-resume",None);
    let old=fixture.attempt(&task);
    fixture.repository.record_operation(&old.token,&operation("saved-write")).unwrap();
    fixture.repository.dispatch_operation(&old.token,"saved-write").unwrap();
    fixture.repository.finish_operation(&old.token,"saved-write",OperationState::Succeeded,Some(json!({"id":"original-result"})),None).unwrap();
    let current=fixture.repository.load(&task.task_id).unwrap();
    let partial=fixture.repository.transition(&old.token,current.version,TaskState::Partial,Some("remaining-work")).unwrap();
    fixture.repository.release_attempt(&old.token,"partial-boundary").unwrap();
    assert!(fixture.repository.start_attempt(&task.task_id,partial.version,"uncontrolled",60_000).is_err());
    let resumed=fixture.repository.control(&task.task_id,partial.version,TaskControl::Resume).unwrap();
    assert_eq!(resumed.task_id,task.task_id);
    assert_eq!(resumed.state,TaskState::Queued);
    let next=fixture.repository.start_attempt(&task.task_id,resumed.version,"continued",60_000).unwrap();
    assert!(fixture.repository.record_operation(&old.token,&operation("stale-write")).is_err());
    assert_eq!(fixture.repository.operation(&task.task_id,"saved-write").unwrap().unwrap().state,OperationState::Succeeded);
    let current=fixture.repository.load(&task.task_id).unwrap();
    let complete=fixture.repository.transition(&next.token,current.version,TaskState::Completed,None).unwrap();
    fixture.repository.release_attempt(&next.token,"completed").unwrap();
    assert!(fixture.repository.control(&task.task_id,complete.version,TaskControl::Resume).is_err());
    let cancelled=fixture.task("cancelled-remains-ended",None);
    let cancelled=fixture.repository.control(&cancelled.task_id,cancelled.version,TaskControl::Cancel).unwrap();
    assert!(fixture.repository.control(&cancelled.task_id,cancelled.version,TaskControl::Resume).is_err());
}

#[test]
fn explicit_revision_keeps_the_settled_root_and_receipts_without_reviving_cancelled_work() {
    let fixture = Fixture::new();
    let task = fixture.task("explicit-root-revision", None);
    let attempt = fixture.attempt(&task);
    fixture.repository.record_operation(&attempt.token, &operation("saved-before-revision")).unwrap();
    fixture.repository.dispatch_operation(&attempt.token, "saved-before-revision").unwrap();
    fixture.repository.finish_operation(&attempt.token, "saved-before-revision", OperationState::Succeeded,
        Some(json!({"id":"original-success"})), None).unwrap();
    let current = fixture.repository.load(&task.task_id).unwrap();
    let completed = fixture.repository.transition(&attempt.token, current.version, TaskState::Completed, None).unwrap();
    assert!(fixture.repository.control(&task.task_id, completed.version, TaskControl::Revise).is_err());
    fixture.repository.release_attempt(&attempt.token, "settled-completion").unwrap();
    assert!(fixture.repository.control(&task.task_id, completed.version, TaskControl::Resume).is_err());
    let revised = fixture.repository.control(&task.task_id, completed.version, TaskControl::Revise).unwrap();
    assert_eq!(revised.task_id, task.task_id);
    assert_eq!(revised.request_id, task.request_id);
    assert_eq!(revised.goal, task.goal);
    assert_eq!(revised.acceptance, task.acceptance);
    assert_eq!(revised.input_reference, task.input_reference);
    assert_eq!(revised.state, TaskState::Queued);
    assert_eq!(revised.stop_reason.as_deref(), Some("user-requested-revision"));
    assert_eq!(fixture.repository.control(&task.task_id, completed.version, TaskControl::Revise).unwrap().version, revised.version);
    assert_eq!(fixture.repository.operation(&task.task_id, "saved-before-revision").unwrap().unwrap().state, OperationState::Succeeded);
    let child = fixture.task("child-cannot-revise-itself", Some(&revised));
    assert!(fixture.repository.control(&child.task_id, child.version, TaskControl::Revise).is_err());
    let cancelled = fixture.task("cancelled-not-revised", None);
    let cancelled = fixture.repository.control(&cancelled.task_id, cancelled.version, TaskControl::Cancel).unwrap();
    assert!(fixture.repository.control(&cancelled.task_id, cancelled.version, TaskControl::Revise).is_err());
}

#[test]
fn unknown_model_cost_acknowledgement_preserves_uncertainty_and_allows_later_real_settlement() {
    let fixture=Fixture::new();
    let task=fixture.task("unknown-model-cost",None);
    let old=fixture.attempt(&task);
    fixture.repository.reserve_model_request(&old.token,"unknown-http").unwrap();
    fixture.expire(&old);
    let current=fixture.repository.load(&task.task_id).unwrap();
    let recovery=fixture.repository.start_attempt(&task.task_id,current.version,"recovery",60_000).unwrap();
    fixture.repository.release_attempt(&recovery.token,"verify-cost").unwrap();
    let waiting=fixture.repository.load(&task.task_id).unwrap();
    let identities=vec![ModelReservationIdentity{task_id:task.task_id.clone(),reservation_id:"unknown-http".into()}];
    assert!(fixture.repository.acknowledge_unknown_model_reservations(&task.task_id,waiting.version+1,&identities).is_err());
    let acknowledged=fixture.repository.acknowledge_unknown_model_reservations(&task.task_id,waiting.version,&identities).unwrap();
    assert_eq!(fixture.repository.acknowledge_unknown_model_reservations(&task.task_id,waiting.version,&identities).unwrap().version,acknowledged.version);
    let unknown=fixture.repository.model_reservations(&task.task_id).unwrap().pop().unwrap();
    assert_eq!(unknown.state,ModelReservationState::Unknown);
    assert!(unknown.is_unknown_acknowledged() && !unknown.blocks_recovery());
    assert!(unknown.dispatched.is_none() && unknown.usage.is_none());
    assert_eq!(acknowledged.usage.model_requests,0);
    let resumed=fixture.repository.control(&task.task_id,acknowledged.version,TaskControl::Resume).unwrap();
    let next=fixture.repository.start_attempt(&task.task_id,resumed.version,"resumed",60_000).unwrap();
    assert_eq!(fixture.repository.load(&task.task_id).unwrap().state,TaskState::Running);
    let current=fixture.repository.load(&task.task_id).unwrap();
    fixture.repository.transition(&next.token,current.version,TaskState::Completed,Some("work-verified-cost-remains-unknown")).unwrap();
    assert_eq!(fixture.repository.model_reservations(&task.task_id).unwrap()[0].state,ModelReservationState::Unknown);
    let settled=fixture.repository.reconcile_model_request(&next.token,"unknown-http",true,&TaskUsage{input_tokens:7,output_tokens:3,..Default::default()},json!({"kind":"provider-original-receipt","requestId":"actual-original"})).unwrap();
    assert_eq!(settled.state,ModelReservationState::Settled);
    assert_eq!(fixture.repository.load(&task.task_id).unwrap().usage.model_requests,1);
}

#[test]
fn quiescent_live_verification_acknowledges_unknowns_and_resumes_the_original_attempt() {
    let fixture=Fixture::new();
    let task=fixture.task("live-verification",None);
    let old=fixture.attempt(&task);
    fixture.repository.record_operation(&old.token,&operation("old-unknown-write")).unwrap();
    fixture.repository.dispatch_operation(&old.token,"old-unknown-write").unwrap();
    fixture.repository.reserve_model_request(&old.token,"old-unknown-cost").unwrap();
    fixture.expire(&old);
    let current=fixture.repository.load(&task.task_id).unwrap();
    let live=fixture.repository.start_attempt(&task.task_id,current.version,"live-recovery",60_000).unwrap();
    let waiting=fixture.repository.load(&task.task_id).unwrap();
    let operation_ids=vec!["old-unknown-write".into()];
    let costs=vec![ModelReservationIdentity{task_id:task.task_id.clone(),reservation_id:"old-unknown-cost".into()}];
    assert!(fixture.repository.acknowledge_unknown_operations(&task.task_id,waiting.version,&operation_ids).is_err());
    assert!(fixture.repository.acknowledge_unknown_model_reservations(&task.task_id,waiting.version,&costs).is_err());
    assert!(fixture.repository.record_operation(&live.token,&operation("must-not-dispatch")).is_err());
    assert!(fixture.repository.reserve_model_request(&live.token,"must-not-request").is_err());
    assert!(fixture.repository.transition(&live.token,waiting.version,TaskState::Running,None).is_err());
    let quiet=fixture.repository.mark_verification_quiescent(&live.token,"await-original-verification").unwrap();
    assert_eq!(quiet.current_attempt_id.as_deref(),Some(live.token.attempt_id.as_str()));
    assert_eq!(fixture.repository.mark_verification_quiescent(&live.token,"repeated-poll").unwrap().version,quiet.version);
    let acknowledged=fixture.repository.acknowledge_unknown_operations(&task.task_id,quiet.version,&operation_ids).unwrap();
    let acknowledged=fixture.repository.acknowledge_unknown_model_reservations(&task.task_id,acknowledged.version,&costs).unwrap();
    assert_eq!(fixture.repository.operation(&task.task_id,"old-unknown-write").unwrap().unwrap().state,OperationState::Unknown);
    assert_eq!(fixture.repository.model_reservations(&task.task_id).unwrap()[0].state,ModelReservationState::Unknown);
    let running=fixture.repository.transition(&live.token,acknowledged.version,TaskState::Running,Some("user-acknowledged")).unwrap();
    assert_eq!(running.current_attempt_id,quiet.current_attempt_id);
    fixture.repository.reserve_model_request(&live.token,"next-request").unwrap();
    fixture.repository.settle_model_request(&live.token,"next-request",false,&TaskUsage::default()).unwrap();
    // The old marker cannot authorize a later waiting state after fresh execution.
    // 新执行后的再次等待不能复用旧静止标记。
    fixture.repository.record_operation(&live.token,&operation("next-unknown-write")).unwrap();
    fixture.repository.dispatch_operation(&live.token,"next-unknown-write").unwrap();
    assert!(fixture.repository.mark_verification_quiescent(&live.token,"still-in-flight").is_err());
    fixture.repository.finish_operation(&live.token,"next-unknown-write",OperationState::Unknown,None,None).unwrap();
    let current=fixture.repository.load(&task.task_id).unwrap();
    let waiting=fixture.repository.transition(&live.token,current.version,TaskState::WaitingVerification,None).unwrap();
    assert!(fixture.repository.acknowledge_unknown_operations(&task.task_id,waiting.version,&["next-unknown-write".into()]).is_err());
    let quiet=fixture.repository.mark_verification_quiescent(&live.token,"second-verification").unwrap();
    fixture.repository.acknowledge_unknown_operations(&task.task_id,quiet.version,&["next-unknown-write".into()]).unwrap();
}

#[test]
fn quiescence_rejects_inflight_reservations_and_keeps_descendant_receipts_writable() {
    let fixture=Fixture::new();
    let task=fixture.task("quiescence-parent",None);
    let parent=fixture.attempt(&task);
    let child=fixture.task("quiescence-child",Some(&task));
    let delegated=fixture.attempt(&child);
    fixture.repository.record_operation(&delegated.token,&operation("delegated-write")).unwrap();
    fixture.repository.dispatch_operation(&delegated.token,"delegated-write").unwrap();
    fixture.repository.finish_operation(&delegated.token,"delegated-write",OperationState::Unknown,None,None).unwrap();
    fixture.repository.reserve_model_request(&parent.token,"inflight-model").unwrap();
    assert!(fixture.repository.mark_verification_quiescent(&parent.token,"model-not-settled").is_err());
    let unknown=fixture.repository.mark_model_request_unknown(&parent.token,"inflight-model","io-ended-receipt-unavailable").unwrap();
    assert_eq!(unknown.state,ModelReservationState::Unknown);
    assert!(unknown.dispatched.is_none() && unknown.usage.is_none());
    assert_eq!(fixture.repository.mark_model_request_unknown(&parent.token,"inflight-model","repeat").unwrap().state,ModelReservationState::Unknown);
    let quiet=fixture.repository.mark_verification_quiescent(&parent.token,"parent-idle").unwrap();
    let ids=vec![TaskOperationIdentity{task_id:child.task_id.clone(),operation_id:"delegated-write".into()}];
    assert!(fixture.repository.acknowledge_unknown_task_operations(&task.task_id,quiet.version,&ids).is_err());
    assert!(fixture.repository.record_operation(&delegated.token,&operation("blocked-by-parent")).is_err());
    fixture.repository.mark_verification_quiescent(&delegated.token,"child-idle").unwrap();
    fixture.repository.acknowledge_unknown_task_operations(&task.task_id,quiet.version,&ids).unwrap();
    let settled=fixture.repository.finish_operation(&delegated.token,"delegated-write",OperationState::Succeeded,Some(json!({"id":"late-original-receipt"})),Some(json!({"kind":"verified-original"}))).unwrap();
    assert_eq!(settled.state,OperationState::Succeeded);
    fixture.repository.renew_attempt(&delegated.token,60_000).unwrap();
    fixture.repository.reconcile_model_request(&parent.token,"inflight-model",false,&TaskUsage::default(),json!({"kind":"verified-no-dispatch"})).unwrap();
    assert!(fixture.repository.mark_model_request_unknown(&parent.token,"inflight-model","cannot-reopen-settled").is_err());
}

#[test]
fn operating_budget_extension_is_monotonic_audited_and_never_expands_parent_or_token_limits() {
    let fixture=Fixture::new();
    let mut input=new_task("budget-extension-root");
    input.budget=TaskBudget{max_model_requests:10,max_known_tokens:Some(100_000),max_execution_ms:Some(1_000)};
    let root=fixture.repository.accept_task(&input).unwrap();
    let parent=fixture.attempt(&root);
    let child=fixture.task("budget-extension-child",Some(&root));
    let child_attempt=fixture.attempt(&child);
    let updated=fixture.repository.extend_operational_budget(&child_attempt.token,512,3_600_000,"authorized-segment-continuation").unwrap();
    assert_eq!(updated.budget.max_model_requests,512);
    assert_eq!(updated.budget.max_execution_ms,None,"unlimited time must remain unlimited");
    assert_eq!(fixture.repository.load(&root.task_id).unwrap().budget,root.budget);
    let updated=fixture.repository.extend_operational_budget(&parent.token,20,2_000,"authorized-root-continuation").unwrap();
    assert_eq!(updated.budget.max_known_tokens,Some(100_000));
    assert_eq!(updated.budget.max_execution_ms,Some(2_000));
    assert_eq!(fixture.repository.extend_operational_budget(&parent.token,5,500,"smaller-repeated-request").unwrap().budget,updated.budget);
    assert_eq!(fixture.repository.read_events(&root.task_id,0,128).unwrap().iter().filter(|event|event.kind=="task.budget.extended").count(),1);
}

#[test]
fn legacy_json_projection_mismatch_is_preserved_for_review_instead_of_chosen_as_truth() {
    let fixture=legacy_fixture();
    let task=fixture.task("legacy-json-mismatch",None);
    sqlite_connections::close(&fixture.root,&fixture.root.join("Agent/tasks.sqlite")).unwrap();
    let path=fixture.root.join("Agent/tasks.sqlite");
    let connection=Connection::open(&path).unwrap();
    connection.execute("UPDATE tasks SET state='completed' WHERE task_id=?",[&task.task_id]).unwrap();
    let document:String=connection.query_row("SELECT document FROM tasks WHERE task_id=?",[&task.task_id],|row|row.get(0)).unwrap();
    drop(connection);
    assert_eq!(TaskRepository::open(&fixture.root).err().unwrap().code(),"STORAGE_INTEGRITY_MIGRATION_REQUIRED");
    let connection=Connection::open(path).unwrap();
    assert_eq!(connection.query_row("SELECT document FROM tasks WHERE task_id=?",[&task.task_id],|row|row.get::<_,String>(0)).unwrap(),document);
    assert_eq!(connection.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),1);
}

#[test]
fn unknown_operation_acknowledgement_rejects_ambiguous_child_ids_and_supports_task_qualified_selection() {
    let fixture=Fixture::new();
    let root=fixture.task("qualified-ack-root",None);
    let parent=fixture.attempt(&root);
    let child_a=fixture.task("qualified-ack-a",Some(&root));
    let child_b=fixture.task("qualified-ack-b",Some(&root));
    let attempts=[fixture.attempt(&child_a),fixture.attempt(&child_b)];
    for attempt in &attempts {
        fixture.repository.record_operation(&attempt.token,&operation("same-local-operation")).unwrap();
        fixture.repository.dispatch_operation(&attempt.token,"same-local-operation").unwrap();
    }
    for attempt in &attempts {
        fixture.repository.release_attempt(&attempt.token,"unknown-after-interruption").unwrap();
    }
    let current=fixture.repository.load(&root.task_id).unwrap();
    let waiting=fixture.repository.transition(&parent.token,current.version,TaskState::WaitingVerification,None).unwrap();
    fixture.repository.release_attempt(&parent.token,"verification-required").unwrap();
    assert_eq!(fixture.repository.acknowledge_unknown_operations(&root.task_id,waiting.version,&["same-local-operation".into()]).unwrap_err().code(),"INVALID_TASK");
    let selected=TaskOperationIdentity{task_id:child_a.task_id.clone(),operation_id:"same-local-operation".into()};
    fixture.repository.acknowledge_unknown_task_operations(&root.task_id,waiting.version,&[selected]).unwrap();
    assert!(fixture.repository.operation(&child_a.task_id,"same-local-operation").unwrap().unwrap().is_unknown_acknowledged());
    assert!(!fixture.repository.operation(&child_b.task_id,"same-local-operation").unwrap().unwrap().is_unknown_acknowledged());
}

#[test]
fn direct_parent_can_return_only_the_current_settled_child_output_for_revision() {
    let fixture=Fixture::new();
    let parent=fixture.task("review-parent",None);
    let parent_attempt=fixture.attempt(&parent);
    let other=fixture.task("other-review-parent",None);
    let other_attempt=fixture.attempt(&other);
    let child=fixture.task("reviewed-child",Some(&parent));
    let attempt=fixture.attempt(&child);
    fixture.repository.record_operation(&attempt.token,&operation("saved-effect")).unwrap();
    fixture.repository.dispatch_operation(&attempt.token,"saved-effect").unwrap();
    fixture.repository.finish_operation(&attempt.token,"saved-effect",OperationState::Succeeded,Some(json!({"id":"saved-effect-result"})),None).unwrap();
    fixture.repository.record_usage(&attempt.token,"original-model",&TaskUsage{model_requests:1,..Default::default()}).unwrap();
    let output_ref=json!({"kind":"task-blob","path":"synthetic-output","sha256":"a".repeat(64),"bytes":13});
    fixture.repository.save_artifact(&attempt.token,&TaskArtifact{artifact_id:"original-output".into(),kind:"task-output".into(),reference:output_ref.clone(),sha256:"a".repeat(64),size_bytes:13,source_version:None}).unwrap();
    let current=fixture.repository.load(&child.task_id).unwrap();
    let completed=fixture.repository.transition(&attempt.token,current.version,TaskState::Completed,None).unwrap();
    assert!(fixture.repository.request_child_revision(&parent_attempt.token,&child.task_id,completed.version,"not yet settled",&output_ref).is_err());
    assert!(fixture.repository.accept_child_output(&parent_attempt.token,&child.task_id,completed.version,"not yet settled",&output_ref).is_err());
    fixture.repository.release_attempt(&attempt.token,"worker-ended").unwrap();
    assert!(fixture.repository.control(&child.task_id,completed.version,TaskControl::Resume).is_err());
    assert!(fixture.repository.accept_child_output(&other_attempt.token,&child.task_id,completed.version,"wrong parent",&output_ref).is_err());
    assert!(fixture.repository.accept_child_output(&parent_attempt.token,&child.task_id,completed.version,"stale output",&json!({"id":"different"})).is_err());
    fixture.repository.accept_child_output(&parent_attempt.token,&child.task_id,completed.version,"verified current output",&output_ref).unwrap();
    let accepted=fixture.repository.latest_child_review(&parent.task_id,&child.task_id).unwrap().unwrap();
    assert_eq!(accepted.payload["decision"],"accept");
    fixture.repository.accept_child_output(&parent_attempt.token,&child.task_id,completed.version,"verified current output",&output_ref).unwrap();
    assert_eq!(fixture.repository.latest_child_review(&parent.task_id,&child.task_id).unwrap().unwrap().sequence,accepted.sequence);
    assert!(fixture.repository.request_child_revision(&other_attempt.token,&child.task_id,completed.version,"wrong parent",&output_ref).is_err());
    assert!(fixture.repository.request_child_revision(&parent_attempt.token,&child.task_id,completed.version,"stale output",&json!({"id":"different"})).is_err());
    let revised=fixture.repository.request_child_revision(&parent_attempt.token,&child.task_id,completed.version,"complete the missing acceptance check",&output_ref).unwrap();
    assert_eq!(revised.task_id,child.task_id);
    assert_eq!(revised.state,TaskState::Queued);
    assert_eq!(revised.usage,completed.usage);
    assert_eq!(fixture.repository.request_child_revision(&parent_attempt.token,&child.task_id,completed.version,"complete the missing acceptance check",&output_ref).unwrap().version,revised.version);
    assert_eq!(fixture.repository.operation(&child.task_id,"saved-effect").unwrap().unwrap().state,OperationState::Succeeded);
    assert_eq!(fixture.repository.latest_artifact(&child.task_id,"task-output").unwrap().unwrap().reference,output_ref);
    assert_eq!(fixture.repository.latest_child_return(&child.task_id).unwrap().unwrap().payload["reviewedVersion"],completed.version);
    fixture.repository.append_event(&parent_attempt.token,"parent-review-first","task.child.review",&json!({"childTaskId":child.task_id,"decision":"accept","reviewedVersion":completed.version,"outputRef":output_ref})).unwrap();
    fixture.repository.append_event(&parent_attempt.token,"different-child-review","task.child.review",&json!({"childTaskId":other.task_id,"decision":"accept"})).unwrap();
    fixture.repository.append_event(&parent_attempt.token,"parent-review-current","task.child.review",&json!({"childTaskId":child.task_id,"decision":"revise","reviewedVersion":completed.version,"outputRef":output_ref})).unwrap();
    assert_eq!(fixture.repository.latest_child_review(&parent.task_id,&child.task_id).unwrap().unwrap().event_id,"parent-review-current");
    fixture.repository.start_attempt(&child.task_id,revised.version,"revision-worker",60_000).unwrap();
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

#[test]
fn steering_message_has_one_root_per_chat_and_retries_keep_the_original_receipt() {
    let fixture = Fixture::new();
    let first = fixture.task("steering-owner-first", None);
    let second = fixture.task("steering-owner-second", None);
    let message = uuid::Uuid::new_v4().to_string();
    let hash = "b".repeat(64);
    let accepted = fixture.repository.record_user_steering(&first.task_id, first.version, &message, "continue", &hash).unwrap();
    assert_eq!(fixture.repository.user_steering_roots(&first.scope.conversation_id, &message).unwrap(), vec![first.task_id.clone()]);
    let rejected = fixture.repository.record_user_steering(&second.task_id, second.version, &message, "continue", &hash).unwrap_err();
    assert_eq!(rejected.code(), "TASK_STEERING_TARGET_CONFLICT");
    assert_eq!(fixture.repository.load(&second.task_id).unwrap().version, second.version);
    assert!(fixture.repository.user_steering(&second.task_id).unwrap().is_empty());
    let retry = fixture.repository.record_user_steering(&first.task_id, first.version, &message, "continue", &hash).unwrap();
    assert_eq!(retry.version, accepted.version);
    assert_eq!(fixture.repository.user_steering(&first.task_id).unwrap().len(), 1);
    let mut other_chat = new_task("steering-owner-other-chat");
    other_chat.scope.conversation_id = uuid::Uuid::new_v4().to_string();
    let other = fixture.repository.accept_task(&other_chat).unwrap();
    fixture.repository.record_user_steering(&other.task_id, other.version, &message, "continue", &hash).unwrap();
    assert_eq!(fixture.repository.user_steering_roots(&first.scope.conversation_id, &message).unwrap(), vec![first.task_id]);
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
    let verified=fixture.repository.load(&task.task_id).unwrap();
    fixture.repository.transition(&next.token,verified.version,TaskState::Running,Some("original-effect-verified")).unwrap();
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
        "TASK_BLOCKED"
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
    let verified=fixture.repository.load(&task.task_id).unwrap();
    fixture.repository.transition(&next.token,verified.version,TaskState::Running,Some("original-dispatch-verified")).unwrap();
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

#[test]
fn acknowledged_unknown_effect_stays_unknown_and_stops_blocking_recovery() {
    let fixture = Fixture::new();
    let task = fixture.task("acknowledge-unknown", None);
    let first = fixture.attempt(&task);
    let mut command = operation("run-a");
    command.tool_name = "terminal.host.run".into();
    command.effect = OperationEffect::Irreversible;
    fixture.repository.record_operation(&first.token, &command).unwrap();
    fixture.repository.dispatch_operation(&first.token, "run-a").unwrap();
    let mut observation = operation("read-a");
    observation.tool_name = "filesystem.read".into();
    observation.effect = OperationEffect::Observation;
    fixture.repository.record_operation(&first.token, &observation).unwrap();
    fixture.repository.dispatch_operation(&first.token, "read-a").unwrap();
    fixture.expire(&first);
    // Before confirmation the takeover stays waiting and new mutations remain blocked.
    // 确认前接管仍处于等待核验，新的修改操作继续被阻止。
    let task = fixture.repository.load(&task.task_id).unwrap();
    let recovery = fixture.repository.start_attempt(&task.task_id, task.version, "recovery", 60_000).unwrap();
    assert_eq!(fixture.repository.load(&task.task_id).unwrap().state, TaskState::WaitingVerification);
    assert_eq!(fixture.repository.record_operation(&recovery.token, &operation("write-b")).unwrap_err().code(), "TASK_BLOCKED");
    fixture.repository.release_attempt(&recovery.token, "recovery-evidence-required").unwrap();
    let waiting = fixture.repository.load(&task.task_id).unwrap();
    assert_eq!(waiting.state, TaskState::WaitingVerification);
    let ids = vec!["run-a".to_owned()];
    assert_eq!(fixture.repository.acknowledge_unknown_operations(&task.task_id, waiting.version, &["read-a".into()]).unwrap_err().code(), "TASK_BLOCKED");
    assert_eq!(fixture.repository.acknowledge_unknown_operations(&task.task_id, waiting.version, &["missing".into()]).unwrap_err().code(), "INVALID_TASK");
    assert_eq!(fixture.repository.acknowledge_unknown_operations(&task.task_id, waiting.version + 1, &ids).unwrap_err().code(), "TASK_CONFLICT");
    assert!(!fixture.repository.operation(&task.task_id, "run-a").unwrap().unwrap().is_unknown_acknowledged());

    let acknowledged = fixture.repository.acknowledge_unknown_operations(&task.task_id, waiting.version, &ids).unwrap();
    assert_eq!(acknowledged.version, waiting.version + 1);
    let replayed = fixture.repository.acknowledge_unknown_operations(&task.task_id, waiting.version, &ids).unwrap();
    assert_eq!(replayed.version, acknowledged.version, "Replaying the same confirmation is idempotent.");
    let settled = fixture.repository.operation(&task.task_id, "run-a").unwrap().unwrap();
    assert_eq!(settled.state, OperationState::Unknown, "Confirmation never turns an unknown outcome into success.");
    assert!(settled.is_unknown_acknowledged() && !settled.blocks_recovery());
    assert_eq!(settled.verification_reference.as_ref().unwrap()["kind"], UNKNOWN_ACKNOWLEDGED_KIND);
    let events = fixture.repository.read_events(&task.task_id, 0, 512).unwrap();
    assert_eq!(events.iter().filter(|event| event.kind == "task.operation.unknown-acknowledged").count(), 1);
    assert!(events.iter().any(|event| event.kind == "task.unknown-acknowledged"));

    // After confirmation the same root resumes, accepts new work and can finish without a fake receipt.
    // 确认后同一根任务可继续、执行新工作并完成，原操作仍保持未知且没有伪造回执。
    let resumed = fixture.repository.control(&task.task_id, acknowledged.version, TaskControl::Resume).unwrap();
    assert_eq!(resumed.state, TaskState::Queued);
    let next = fixture.repository.start_attempt(&task.task_id, resumed.version, "resumed", 60_000).unwrap();
    assert_eq!(fixture.repository.load(&task.task_id).unwrap().state, TaskState::Running);
    fixture.repository.record_operation(&next.token, &operation("write-b")).unwrap();
    fixture.repository.dispatch_operation(&next.token, "write-b").unwrap();
    fixture.repository.finish_operation(&next.token, "write-b", OperationState::Succeeded,
        Some(json!({"resultId":"fixture-write"})), None).unwrap();
    let current = fixture.repository.load(&task.task_id).unwrap();
    fixture.repository.transition(&next.token, current.version, TaskState::Completed, None).unwrap();
    assert_eq!(fixture.repository.operation(&task.task_id, "run-a").unwrap().unwrap().state, OperationState::Unknown);
}
