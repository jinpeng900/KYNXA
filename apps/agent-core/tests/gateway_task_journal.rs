//! Native bridge checks keep all task bodies and databases in disposable test roots.
//! 原生接线检查把所有任务正文和数据库放进一次性测试根目录。

use kynxa_agent_core_native::{
    orchestration::gateway_task_journal::TaskJournal,
    storage::{managed_files, sqlite_connections, task_repository::*},
};
use serde_json::json;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("kynxa-task-journal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = sqlite_connections::close(&self.root, &self.root.join("Agent/tasks.sqlite"));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn input(request: &str) -> NewTask {
    NewTask {
        request_id: request.into(),
        goal: "Synthetic independent child work".into(),
        acceptance: vec!["A verified reference is returned".into()],
        scope: TaskScope {
            project_id: Some("527b243e-5b7a-47e4-be73-4653a3b610c5".into()),
            conversation_id: "e746104c-85d9-41be-bf22-f1b69636cf6b".into(),
            permission_snapshot: json!({"mode":"work","bindingRevision":1}),
        },
        parent_task_id: None,
        dependency_ids: vec![],
        budget: TaskBudget::default(),
        input_reference: Some(
            json!({"kind":"conversation-message","messageId":"fixture-user","options":{"modelId":"fixture-api"}}),
        ),
    }
}

#[tokio::test]
async fn batched_events_flush_before_checkpoint_and_terminal_state() {
    let fixture = Fixture::new();
    let journal =
        TaskJournal::begin(&fixture.root, &input("events"), "fixture-worker", 60_000).unwrap();
    for index in 0..200 {
        journal.emit_control("task.progress",&json!({"index":index,"reference":{"kind":"conversation-segment","messageId":"fixture-assistant","segment":index}})).unwrap();
    }
    journal.reserve_model_request("fixture-http").unwrap();
    journal
        .settle_model_request(
            "fixture-http",
            true,
            &TaskUsage {
                input_tokens: 20,
                output_tokens: 5,
                ..TaskUsage::default()
            },
        )
        .unwrap();
    let checkpoint = journal
        .checkpoint_from_chat(
            "fixture-assistant",
            "events",
            Some(json!({"kind":"conversation-summary","revision":2})),
            json!({"next":"verify result"}),
        )
        .await
        .unwrap();
    let events = journal
        .repository()
        .read_events(journal.task_id(), 0, 512)
        .unwrap();
    let progress: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "task.progress")
        .collect();
    assert_eq!(progress.len(), 200);
    for (index, event) in progress.iter().enumerate() {
        assert_eq!(event.payload["index"], index);
    }
    assert_eq!(
        checkpoint.through_event_sequence,
        events.last().unwrap().sequence
    );
    assert_eq!(
        checkpoint.context_reference["assistantMessageId"],
        "fixture-assistant"
    );
    assert_eq!(
        journal.latest_checkpoint().unwrap().unwrap().checkpoint_id,
        checkpoint.checkpoint_id
    );
    journal
        .emit_control(
            "task.verification",
            &json!({"testReference":{"resultId":"fixture-success"}}),
        )
        .unwrap();
    let task = journal
        .finish(TaskState::Completed, Some("verified"))
        .await
        .unwrap();
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.usage.model_requests, 1);
    assert!(
        journal
            .repository()
            .read_events(journal.task_id(), checkpoint.through_event_sequence, 20)
            .unwrap()
            .iter()
            .any(|event| event.kind == "task.verification")
    );
}

#[tokio::test]
async fn child_blob_is_scoped_hash_verified_and_cancel_preserves_queued_observations() {
    let fixture = Fixture::new();
    let parent =
        TaskJournal::begin(&fixture.root, &input("parent"), "parent-worker", 60_000).unwrap();
    let bytes = vec![b'x'; 96 * 1024];
    let reference = parent.save_blob("child-context", &bytes).await.unwrap();
    assert_eq!(parent.read_blob(&reference).await.unwrap(), bytes);
    let mut child_input = input("child");
    child_input.parent_task_id = Some(parent.task_id().into());
    child_input.scope.conversation_id = "2a83c59d-29a0-4132-b019-960c951e1b3f".into();
    let child_record = parent.repository().accept_task(&child_input).unwrap();
    let child =
        TaskJournal::start_existing(&fixture.root, &child_record, "child-worker", 60_000).unwrap();
    assert_eq!(child.read_blob(&reference).await.unwrap(), bytes);
    let outside =
        TaskJournal::begin(&fixture.root, &input("outside"), "outside-worker", 60_000).unwrap();
    assert_eq!(
        outside.read_blob(&reference).await.unwrap_err().code,
        "INVALID_TASK_ARTIFACT_REFERENCE"
    );
    let relative = reference["relativePath"].as_str().unwrap();
    managed_files::atomic_write(
        &fixture.root,
        &fixture.root.join(relative),
        b"changed",
        32 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        parent.read_blob(&reference).await.unwrap_err().code,
        "INVALID_TASK_ARTIFACT_REFERENCE"
    );
    child
        .emit_control(
            "task.observation",
            &json!({"resultReference":{"id":"already-observed"}}),
        )
        .unwrap();
    let current = parent.load_current().unwrap();
    parent
        .repository()
        .control(parent.task_id(), current.version, TaskControl::Cancel)
        .unwrap();
    assert_eq!(
        child.check_dispatch_fence().unwrap_err().code,
        "TASK_BLOCKED"
    );
    child.flush().await.unwrap();
    assert!(
        child
            .repository()
            .read_events(child.task_id(), 0, 20)
            .unwrap()
            .iter()
            .any(|event| event.kind == "task.observation")
    );
    let ended = child
        .finish(TaskState::Completed, Some("cancelled"))
        .await
        .unwrap();
    assert_eq!(ended.state, TaskState::Cancelled);
}

#[tokio::test]
async fn root_work_context_survives_pause_restart_and_rejects_foreign_or_corrupt_snapshots() {
    let fixture = Fixture::new();
    let original = input("root-recovery");
    let journal = TaskJournal::begin(&fixture.root, &original, "root-first", 60_000).unwrap();
    let origin = json!({"providerId":"fixture-api","model":"fixture","connectionFingerprint":"a".repeat(64)});
    let context = json!({"messages":[{"role":"user","content":"Original goal"}],
        "historySources":[{"checkpointSemanticSummary":{"facts":[{"resultId":"original-source","text":"Actual saved value: 431"}],
            "sources":[{"processedCharacters":300,"totalCharacters":600}]}}],
        "resultSources":[],"formalSources":[],"selectedNames":["filesystem.read"],"appliedSteeringIds":[]});
    let current = journal.load_current().unwrap();
    journal
        .repository()
        .control(journal.task_id(), current.version, TaskControl::Pause)
        .unwrap();
    assert_eq!(
        journal.check_dispatch_fence().unwrap_err().code,
        "TASK_BLOCKED"
    );
    // A completed observation can settle after pause without authorizing another dispatch.
    // 暂停后允许结算已完成观察并保存断点，但不授权下一次派发。
    let reference = journal
        .save_root_work_context("root-recovery", "fingerprint", &origin, &context)
        .await
        .unwrap();
    journal
        .checkpoint_from_chat(
            "assistant-root",
            "root-recovery",
            Some(reference.clone()),
            json!({"next":"unread range"}),
        )
        .await
        .unwrap();
    journal
        .finish(TaskState::Paused, Some("user-pause"))
        .await
        .unwrap();
    let repository = TaskRepository::open(&fixture.root).unwrap();
    let paused = repository.load(journal.task_id()).unwrap();
    let resumed = repository
        .control(journal.task_id(), paused.version, TaskControl::Resume)
        .unwrap();
    let recovered =
        TaskJournal::start_existing(&fixture.root, &resumed, "root-restarted", 60_000).unwrap();
    let restored = recovered
        .read_root_work_context("assistant-root", "root-recovery", "fingerprint", &origin)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored, (context, reference.clone()));
    for (assistant, fingerprint, candidate_origin) in [
        ("another-assistant", "fingerprint", origin.clone()),
        ("assistant-root", "changed-fingerprint", origin.clone()),
        (
            "assistant-root",
            "fingerprint",
            json!({"providerId":"other-api"}),
        ),
    ] {
        assert_eq!(
            recovered
                .read_root_work_context(assistant, "root-recovery", fingerprint, &candidate_origin)
                .await
                .unwrap_err()
                .code,
            "INVALID_ROOT_WORK_CONTEXT"
        );
    }
    let relative = reference["relativePath"].as_str().unwrap();
    managed_files::atomic_write(
        &fixture.root,
        &fixture.root.join(relative),
        b"changed",
        32 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        recovered
            .read_root_work_context("assistant-root", "root-recovery", "fingerprint", &origin)
            .await
            .unwrap_err()
            .code,
        "INVALID_TASK_ARTIFACT_REFERENCE"
    );
    let current = recovered.load_current().unwrap();
    recovered
        .repository()
        .control(recovered.task_id(), current.version, TaskControl::Cancel)
        .unwrap();
    recovered
        .finish(TaskState::Cancelled, Some("fixture-finished"))
        .await
        .unwrap();
    let cleanup = repository
        .purge_conversation(&original.scope.conversation_id)
        .unwrap();
    assert!(
        cleanup
            .files
            .iter()
            .any(|file| file.relative_path == relative),
        "the private work checkpoint is registered for owned cleanup"
    );
}
