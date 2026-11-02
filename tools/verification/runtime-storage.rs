use kynxa_agent_core_native::storage::{gateway, task_repository::*};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Instant};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-runtime-smoke-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let target = self.0.canonicalize().unwrap();
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(target.parent(), Some(temporary.as_path()));
        assert!(target.file_name().unwrap().to_string_lossy().starts_with("kynxa-runtime-smoke-"));
        kynxa_agent_core_native::storage::sqlite_connections::drain_root(&self.0).unwrap();
        fs::remove_dir_all(target).unwrap();
    }
}

fn read(root: &PathBuf, chat: &str, id: &str) -> Value {
    let (status, raw) = gateway::handle_text("GET", &format!("/api/conversations/{chat}/messages"),
        &[("messageId".into(), id.into())], root).unwrap().unwrap();
    assert_eq!(status, 200);
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn known_reply_is_read_beyond_three_pages_with_live_scope_and_original_bytes() {
    let fixture = Fixture::new();
    let chat = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    let reply = uuid::Uuid::new_v4().to_string();
    gateway::ensure_conversation(&fixture.0, &chat, "Old replies").unwrap();
    gateway::ensure_conversation(&fixture.0, &other, "Another chat").unwrap();
    gateway::upsert_message(&fixture.0, &chat, &json!({"Id":reply,"Role":"assistant","Content":"原有结果",
        "Status":"completed"})).unwrap();
    let log = fixture.0.join("Chats").join(&chat).join("events.jsonl");
    let mut original: Value = serde_json::from_str(fs::read_to_string(&log).unwrap().trim()).unwrap();
    original["message"]["ModelTranscript"] = json!({"private":"must-not-be-public"});
    let mut raw = original.to_string() + "\n";
    for _ in 0..650 {
        raw.push_str(&json!({"version":1,"type":"message.upsert","message":{
            "Id":uuid::Uuid::new_v4().to_string(),"Role":"user","Content":"newer message","Status":"completed"}}).to_string());
        raw.push('\n');
    }
    fs::write(&log, &raw).unwrap();
    let started = Instant::now();
    let page = read(&fixture.0, &chat, &reply);
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["Content"], "原有结果");
    assert!(page["messages"][0].get("ModelTranscript").is_none());
    assert!(read(&fixture.0, &other, &reply)["messages"].as_array().unwrap().is_empty());
    assert_eq!(fs::read_to_string(&log).unwrap(), raw, "read never rewrites the formal log");
    fs::write(&log, raw.replacen("原有结果", "更新结果", 1)).unwrap();
    assert_eq!(read(&fixture.0, &chat, &reply)["messages"][0]["Content"], "更新结果", "warm index rechecks source version");
    let mixed = gateway::handle_text("GET", &format!("/api/conversations/{chat}/messages"),
        &[("messageId".into(), reply), ("cursor".into(), "invalid".into())], &fixture.0).unwrap().unwrap_err();
    assert_eq!(mixed.code(), "INVALID_CONVERSATION_DATA");
    let invalid = gateway::handle_text("GET", &format!("/api/conversations/{chat}/messages"),
        &[("messageId".into(), "../../other".into())], &fixture.0).unwrap().unwrap_err();
    assert!(invalid.status_code() >= 400);
    println!("targeted reply with 650 newer messages: {} ms", started.elapsed().as_millis());
}

#[test]
fn restart_resume_keeps_checkpoint_elapsed_time_and_successful_operation() {
    let fixture = Fixture::new();
    let repository = TaskRepository::open(&fixture.0).unwrap();
    let created = repository.accept_task(&NewTask {
        request_id: uuid::Uuid::new_v4().to_string(), goal: "继续原工作".into(), acceptance: vec!["保存产物".into()],
        scope: TaskScope { project_id: None, conversation_id: uuid::Uuid::new_v4().to_string(), permission_snapshot: json!({"mode":"smart"}) },
        parent_task_id: None, dependency_ids: vec![], budget: TaskBudget::default(), input_reference: Some(json!({"path":"input.json"})),
    }).unwrap();
    let attempt = repository.start_attempt(&created.task_id, created.version, "before-restart", 30000).unwrap();
    let intent = OperationIntent { operation_id: "saved-file".into(), tool_call_id: "call-write".into(), tool_name: "filesystem.write".into(),
        input_sha256: "1".repeat(64), input_reference: Some(json!({"path":"report.md"})), effect: OperationEffect::FileMutation };
    repository.record_operation(&attempt.token, &intent).unwrap();
    repository.dispatch_operation(&attempt.token, &intent.operation_id).unwrap();
    let receipt = json!({"path":"report.md","sha256":"2".repeat(64)});
    repository.finish_operation(&attempt.token, &intent.operation_id, OperationState::Succeeded, Some(receipt.clone()), None).unwrap();
    repository.record_active_duration(&attempt.token, 90000).unwrap();
    let current = repository.load(&created.task_id).unwrap();
    let checkpoint = TaskCheckpoint { checkpoint_id: "saved-context".into(), through_event_sequence: current.event_sequence,
        context_reference: json!({"path":"context.json","assistantMessageId":created.request_id}),
        next_step: json!({"remaining":["交付报告"]}), source_versions: vec![json!({"path":"report.md","sha256":"2".repeat(64)})] };
    repository.save_checkpoint(&attempt.token, &checkpoint).unwrap();
    let pause = repository.control(&created.task_id, repository.load(&created.task_id).unwrap().version, TaskControl::Pause).unwrap();
    assert_eq!(pause.state, TaskState::Paused);
    repository.release_attempt(&attempt.token, "user-paused").unwrap();
    drop(repository);
    kynxa_agent_core_native::storage::sqlite_connections::drain_root(&fixture.0).unwrap();
    let restored = TaskRepository::open(&fixture.0).unwrap();
    assert_eq!(restored.load(&created.task_id).unwrap().state, TaskState::Paused, "opening storage does not resume a task");
    let queued = restored.control(&created.task_id, pause.version, TaskControl::Resume).unwrap();
    let next = restored.start_attempt(&created.task_id, queued.version, "after-restart", 30000).unwrap();
    assert_eq!(next.token.task_id, attempt.token.task_id);
    assert_ne!(next.token.attempt_id, attempt.token.attempt_id);
    assert_eq!(restored.latest_checkpoint(&created.task_id).unwrap().unwrap().context_reference, checkpoint.context_reference);
    let operation = restored.operation(&created.task_id, &intent.operation_id).unwrap().unwrap();
    assert_eq!(operation.state, OperationState::Succeeded);
    assert_eq!(operation.result_reference, Some(receipt));
    assert!(restored.dispatch_operation(&next.token, &intent.operation_id).is_err(), "successful operation must not be dispatched again");
    assert_eq!(restored.load(&created.task_id).unwrap().usage.active_duration_ms, Some(90000));
}
