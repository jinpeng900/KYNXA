//! Control and subscription fixtures make no provider calls and use isolated formal catalogs.
//! 控制及订阅夹具不调用供应商，使用隔离的正式目录和任务库。

use kynxa_agent_core_native::{
    gateway_contract::GatewayConfig,
    orchestration::{gateway_runtime::GatewayRuntime, gateway_task_journal::TaskJournal},
    storage::{gateway as data, sqlite_connections, task_repository::*},
    tools::gateway::GatewayToolContext,
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Fixture {
    root: PathBuf,
    runtime: Arc<GatewayRuntime>,
    scope: TaskScope,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-task-hub-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = GatewayConfig {
            data_home: root.join("Config"),
            data_root: root.clone(),
            extension_root: root.join("Extensions"),
            application_root: root.join("Application"),
            port: 0,
            toolhost_exe: root.join("ToolHost/fixture.exe"),
            vector_extension: root.join("Native/fixture.dll"),
            inference_library: root.join("Native/fixture-ort.dll"),
        };
        let conversation = "eb8fdf26-d5c1-4a8a-80bc-cde518bd4d36";
        data::ensure_conversation(&root, conversation, "Synthetic hub fixture").unwrap();
        let scope = TaskScope {
            project_id: None,
            conversation_id: conversation.into(),
            permission_snapshot: json!({"permissionMode":"smart","bindingSnapshot":{"projectId":null,"folderPath":null}}),
        };
        Self {
            root,
            runtime: GatewayRuntime::new(config),
            scope,
        }
    }
    fn input(&self, request: &str, parent: Option<&str>) -> NewTask {
        NewTask {
            request_id: request.into(),
            goal: "Synthetic hub work".into(),
            acceptance: vec![],
            scope: self.scope.clone(),
            parent_task_id: parent.map(str::to_owned),
            dependency_ids: vec![],
            budget: TaskBudget::default(),
            input_reference: Some(json!({"kind":"fixture-original-reference"})),
        }
    }
    fn context(&self) -> GatewayToolContext {
        GatewayToolContext {
            data_home: self.root.clone(),
            extension_root: self.runtime.config.extension_root.clone(),
            workdir: self.root.clone(),
            toolhost_exe: self.runtime.config.toolhost_exe.clone(),
            permission_mode: "smart".into(),
            conversation_id: self.scope.conversation_id.clone(),
            project_id: None,
            authorized_roots: vec![self.root.clone()],
            cancellation: tokio::sync::watch::channel(false).1,
            runtime: self.runtime.tools.clone(),
            config: json!({}),
            approved: false,
            events: None,
            external_execute: None,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = sqlite_connections::close(&self.root, &self.root.join("Agent/tasks.sqlite"));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn paused_lookup_http_distinguishes_none_unique_ambiguous_and_rejects_wrong_scope() {
    use kynxa_agent_core_native::{
        gateway_contract::GatewayError, orchestration::gateway_task_http,
    };
    use serde_json::Value;
    async fn read_paused(
        runtime: &Arc<GatewayRuntime>,
        conversation: &str,
        project: Option<&str>,
    ) -> Result<Value, GatewayError> {
        let mut query = vec![("conversationId".into(), conversation.into())];
        if let Some(project) = project {
            query.push(("projectId".into(), project.into()));
        }
        gateway_task_http::handle(runtime, "GET", "/api/tasks/paused", &query, &Value::Null)
            .await
            .unwrap()
            .map(|(status, value)| {
                assert_eq!(status, 200);
                value
            })
    }
    let fixture = Fixture::new();
    let conversation = &fixture.scope.conversation_id;
    let none = read_paused(&fixture.runtime, conversation, None)
        .await
        .unwrap();
    assert_eq!(none["tasks"].as_array().unwrap().len(), 0);
    let repository = TaskRepository::open(&fixture.root).unwrap();
    let root = repository
        .accept_task(&fixture.input("lookup-http-root", None))
        .unwrap();
    repository
        .accept_task(&fixture.input("lookup-http-child", Some(&root.task_id)))
        .unwrap();
    repository
        .control(&root.task_id, root.version, TaskControl::Pause)
        .unwrap();
    let other_chat = uuid::Uuid::new_v4().to_string();
    data::ensure_conversation(&fixture.root, &other_chat, "Other isolated chat").unwrap();
    let mut other_input = fixture.input("lookup-http-other-chat", None);
    other_input.scope.conversation_id = other_chat;
    let other = repository.accept_task(&other_input).unwrap();
    repository
        .control(&other.task_id, other.version, TaskControl::Pause)
        .unwrap();
    let unique = read_paused(&fixture.runtime, conversation, None)
        .await
        .unwrap();
    assert_eq!(unique["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(unique["tasks"][0]["taskId"], root.task_id);
    assert!(unique["tasks"][0]["parentTaskId"].is_null());
    let second = repository
        .accept_task(&fixture.input("lookup-http-second", None))
        .unwrap();
    repository
        .control(&second.task_id, second.version, TaskControl::Pause)
        .unwrap();
    let ambiguous = read_paused(&fixture.runtime, conversation, None)
        .await
        .unwrap();
    assert_eq!(ambiguous["tasks"].as_array().unwrap().len(), 2);
    assert!(
        ambiguous["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|task| task["scope"]["conversationId"].as_str() == Some(conversation.as_str()))
    );
    let wrong_project = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        read_paused(&fixture.runtime, conversation, Some(&wrong_project))
            .await
            .unwrap_err()
            .code,
        "TASK_SCOPE_DENIED"
    );
    assert_eq!(
        read_paused(&fixture.runtime, "invalid-chat-id", None)
            .await
            .unwrap_err()
            .status,
        400
    );
    assert_eq!(
        repository.load(&root.task_id).unwrap().state,
        TaskState::Paused
    );
    assert_eq!(
        repository.load(&root.task_id).unwrap().version,
        unique["tasks"][0]["version"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn cross_root_steering_rejection_preserves_formal_user_and_superseded_reply() {
    let fixture = Fixture::new();
    let repository = TaskRepository::open(&fixture.root).unwrap();
    let first = repository.accept_task(&fixture.input(&uuid::Uuid::new_v4().to_string(), None)).unwrap();
    let second = repository.accept_task(&fixture.input(&uuid::Uuid::new_v4().to_string(), None)).unwrap();
    let message = uuid::Uuid::new_v4().to_string();
    let original_reply = uuid::Uuid::new_v4().to_string();
    repository.record_user_steering(&first.task_id, first.version, &message, "continue", &"d".repeat(64)).unwrap();
    data::upsert_message(&fixture.root, &fixture.scope.conversation_id,
        &json!({"Id":message,"Role":"user","Content":"Continue the original task","Status":"completed","ReplyMessageId":original_reply})).unwrap();
    data::upsert_message(&fixture.root, &fixture.scope.conversation_id,
        &json!({"Id":second.request_id,"Role":"assistant","Content":"Existing answer stays intact","Status":"completed"})).unwrap();
    let read = || data::read_messages(&fixture.root, &fixture.scope.conversation_id, false).unwrap();
    let before = read();
    let error = fixture.runtime.tasks.steer(&second.task_id, second.version, &message,
        Some(&uuid::Uuid::new_v4().to_string()), "Continue the original task", "continue", &json!([])).await.unwrap_err();
    assert_eq!(error.code, "TASK_STEERING_TARGET_CONFLICT");
    assert_eq!(error.details["boundTaskIds"], json!([first.task_id]));
    assert_eq!(read(), before, "a rejected retarget cannot replace ReplyMessageId or mark an unrelated assistant interrupted");
    assert_eq!(repository.load(&second.task_id).unwrap().version, second.version);
    assert!(repository.user_steering(&second.task_id).unwrap().is_empty());
}

#[tokio::test]
async fn root_pagination_does_not_let_children_shadow_roots_or_replay_controls() {
    let fixture = Fixture::new();
    let repository = TaskRepository::open(&fixture.root).unwrap();
    for root_index in 0..3 {
        let task = repository
            .accept_task(&fixture.input(&format!("root-{root_index}"), None))
            .unwrap();
        for child_index in 0..5 {
            repository
                .accept_task(&fixture.input(
                    &format!("child-{root_index}-{child_index}"),
                    Some(&task.task_id),
                ))
                .unwrap();
        }
    }
    let page = fixture
        .runtime
        .tasks
        .list(Some(&fixture.scope.conversation_id), None, None, 2)
        .await
        .unwrap();
    let first = page["tasks"].as_array().unwrap();
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|task| task["parentTaskId"].is_null()));
    assert_eq!(page["hasMore"], true);
    let last = fixture
        .runtime
        .tasks
        .list(
            Some(&fixture.scope.conversation_id),
            None,
            page["nextTaskId"].as_str(),
            2,
        )
        .await
        .unwrap();
    assert_eq!(last["tasks"].as_array().unwrap().len(), 1);
    let id = first[0]["taskId"].as_str().unwrap();
    let version = first[0]["version"].as_u64().unwrap();
    let paused = fixture
        .runtime
        .tasks
        .control(id, version, TaskControl::Pause)
        .await
        .unwrap();
    assert_eq!(paused["affectedTaskIds"].as_array().unwrap().len(), 6);
    let replay = fixture
        .runtime
        .tasks
        .control(id, version, TaskControl::Pause)
        .await
        .unwrap();
    assert_eq!(replay["task"]["version"], paused["task"]["version"]);
    let detail = fixture.runtime.tasks.detail(id).await.unwrap();
    assert!(
        detail["children"]
            .as_array()
            .unwrap()
            .iter()
            .all(|task| task["state"] == "paused")
    );
    let events = fixture.runtime.tasks.events(id, 0, 1).await.unwrap();
    assert_eq!(events["hasMore"], true);
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_follow_up_inherits_the_unmet_minimum_duration_of_the_previous_task() {
    use kynxa_agent_core_native::orchestration::gateway_task_input;
    let fixture = Fixture::new();
    let message = |text: &str| json!({"conversationId":fixture.scope.conversation_id,"requestId":uuid::Uuid::new_v4().to_string(),
        "message":text,"provider":"unconfigured-fixture","model":"fixture","permissionMode":"smart"});
    let persisted = |task: &kynxa_agent_core_native::storage::task_repository::TaskRecord| -> serde_json::Value {
        let reference = task.input_reference.as_ref().unwrap();
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(fixture.root.join(reference["relativePath"].as_str().unwrap())).unwrap())
            .unwrap()["resolvedRunLimits"].clone()
    };
    let first = gateway_task_input::accept(&fixture.runtime.config, &message("至少工作一小时，把场景精修好"), None, &"a".repeat(64)).await.unwrap();
    assert_eq!(persisted(&first)["minActiveMs"], 3_600_000);
    let follow_up = gateway_task_input::accept(&fixture.runtime.config, &message("继续"), None, &"b".repeat(64)).await.unwrap();
    let limits = persisted(&follow_up);
    assert_eq!(limits["minActiveMs"], 3_600_000);
    assert_eq!(limits["minimumInheritedFrom"], first.task_id);
    assert_eq!(follow_up.goal, first.goal);
    assert_eq!(follow_up.acceptance, first.acceptance);
    assert!(limits["maxDurationMs"].as_u64().unwrap() >= 3_600_000);
    // A cancelled task does not carry its minimum forward.
    // 已取消任务不继承最低时长。
    let repository = TaskRepository::open(&fixture.root).unwrap();
    let current = repository.load(&follow_up.task_id).unwrap();
    repository.control(&current.task_id, current.version, kynxa_agent_core_native::storage::task_repository::TaskControl::Cancel).unwrap();
    let fresh = gateway_task_input::accept(&fixture.runtime.config, &message("你好"), None, &"c".repeat(64)).await.unwrap();
    assert!(persisted(&fresh).get("minActiveMs").is_none());
}

#[tokio::test]
async fn a_follow_up_keeps_committed_requirement_revocations_and_never_revives_the_old_hour() {
    use kynxa_agent_core_native::orchestration::gateway_task_input;
    for (requirements, suspended) in [(Vec::<String>::new(), None), (vec!["保留金色穹顶并只调整彩窗".into()], None),
        (Vec::new(), Some(vec!["继续制作已有的金色穹顶".to_owned()]))] {
        let fixture = Fixture::new();
        let message = |text: &str| json!({"conversationId":fixture.scope.conversation_id,"requestId":uuid::Uuid::new_v4().to_string(),
            "message":text,"permissionMode":"smart"});
        let read_input = |task: &TaskRecord| -> serde_json::Value {
            let reference = task.input_reference.as_ref().unwrap();
            serde_json::from_slice(&std::fs::read(fixture.root.join(reference["relativePath"].as_str().unwrap())).unwrap()).unwrap()
        };
        let previous = gateway_task_input::accept(&fixture.runtime.config, &message("至少工作一小时，精修教堂；增加雕像"), None, &"a".repeat(64)).await.unwrap();
        let original = read_input(&previous);
        let journal = TaskJournal::start_existing(&fixture.root, &previous, "directive-fixture", 60_000).unwrap();
        let mut directive = json!({"userMessageId":uuid::Uuid::new_v4().to_string(),
            "requirements":requirements,"minimumActiveMs":0,"stop":requirements.is_empty()});
        if let Some(suspended) = &suspended { directive["suspendedRequirements"] = json!(suspended); }
        journal.emit_control("task.user-directive", &directive).unwrap();
        journal.flush().await.unwrap();
        let current = journal.load_current().unwrap();
        journal.repository().control(&previous.task_id, current.version, TaskControl::Pause).unwrap();
        journal.finish(TaskState::Paused, Some("fixture-stopped")).await.unwrap();
        let continued = gateway_task_input::accept(&fixture.runtime.config, &message("继续"), None, &"b".repeat(64)).await.unwrap();
        let saved = read_input(&continued);
        assert_eq!(saved["acceptanceRequirements"], json!(suspended.as_ref().unwrap_or(&requirements)));
        assert_eq!(saved["resolvedRunLimits"]["minActiveMs"].as_u64().unwrap_or(0), 0);
        assert!(saved["resolvedRunLimits"].get("minimumInheritedFrom").is_none());
        assert!(!continued.goal.contains("一小时"));
        assert!(!continued.goal.contains("雕像"));
        assert_eq!(read_input(&previous), original, "the original accepted request stays immutable");
        assert_eq!(journal.load_current().unwrap().state, TaskState::Paused);
    }
}

#[tokio::test]
async fn a_new_topic_or_completed_work_never_inherits_an_old_minimum() {
    use kynxa_agent_core_native::orchestration::gateway_task_input;
    for (finish_previous, next_message) in [(false, "你好"), (false, "查询今天的天气"),
        (false, "继续工作，至少只说你好100小时"), (true, "继续")] {
        let fixture = Fixture::new();
        let message = |text: &str| json!({"conversationId":fixture.scope.conversation_id,"requestId":uuid::Uuid::new_v4().to_string(),
            "message":text,"provider":"unconfigured-fixture","model":"fixture","permissionMode":"smart"});
        let previous = gateway_task_input::accept(&fixture.runtime.config, &message("至少工作一小时，精修教堂；不要座椅"), None, &"a".repeat(64)).await.unwrap();
        if finish_previous {
            let journal = TaskJournal::start_existing(&fixture.root, &previous, "fixture", 60_000).unwrap();
            journal.finish(TaskState::Completed, None).await.unwrap();
        }
        let fresh = gateway_task_input::accept(&fixture.runtime.config, &message(next_message), None, &"b".repeat(64)).await.unwrap();
        let reference = fresh.input_reference.as_ref().unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture.root.join(reference["relativePath"].as_str().unwrap())).unwrap()).unwrap();
        assert!(saved["resolvedRunLimits"].get("minActiveMs").is_none(), "{next_message}");
        assert!(saved["resolvedRunLimits"].get("continuedFromTaskId").is_none(), "{next_message}");
        assert_eq!(fresh.goal, next_message);
        assert_eq!(fresh.acceptance, vec![next_message.to_owned()]);
    }
}

#[tokio::test]
async fn long_acceptance_keeps_original_requirements_outside_the_small_task_row() {
    use kynxa_agent_core_native::orchestration::gateway_task_input;
    let fixture = Fixture::new();
    let message = |text: &str| json!({"conversationId":fixture.scope.conversation_id,"requestId":uuid::Uuid::new_v4().to_string(),
        "message":text,"provider":"unconfigured-fixture","model":"fixture","permissionMode":"smart"});
    let goal = format!("至少精修八小时，{}；保留法线贴图", "金色穹顶与彩窗要求\"\\".repeat(6000));
    let first = gateway_task_input::accept(&fixture.runtime.config, &message(&goal), None, &"a".repeat(64)).await.unwrap();
    assert!(serde_json::to_vec(&first.acceptance).unwrap().len() <= 8 * 1024);
    let read_input = |task: &kynxa_agent_core_native::storage::task_repository::TaskRecord| -> serde_json::Value {
        let reference = task.input_reference.as_ref().unwrap();
        serde_json::from_slice(&std::fs::read(fixture.root.join(reference["relativePath"].as_str().unwrap())).unwrap()).unwrap()
    };
    let saved = read_input(&first);
    assert_eq!(saved["input"]["message"], goal);
    assert_eq!(saved["acceptanceRequirements"][0], goal.split('；').next().unwrap());
    let continued = gateway_task_input::accept(&fixture.runtime.config, &message("继续"), None, &"b".repeat(64)).await.unwrap();
    assert_eq!(read_input(&continued)["acceptanceRequirements"], saved["acceptanceRequirements"]);
    assert!(serde_json::to_vec(&continued.acceptance).unwrap().len() <= 8 * 1024);
}

#[tokio::test]
async fn delegation_ids_isolate_parents_and_dispatches_while_retries_reuse_the_same_children() {
    use kynxa_agent_core_native::orchestration::gateway_task_input;
    let fixture = Fixture::new();
    let mut parents = Vec::new();
    for request in ["batch-parent-a", "batch-parent-b"] {
        let input = json!({"conversationId":fixture.scope.conversation_id,"requestId":request,
            "message":"Delegate independent read-only work.","provider":"unconfigured-fixture","model":"fixture","permissionMode":"smart"});
        let root = gateway_task_input::accept(&fixture.runtime.config,&input,None,&"a".repeat(64)).await.unwrap();
        let journal = TaskJournal::start_existing(&fixture.root,&root,"batch-fixture",60_000).unwrap();
        let run = fixture.runtime.child_run(&root,journal.clone()).unwrap();
        parents.push((root,journal,run));
    }
    let args = json!({"count":2,"goal":"Read the assigned reference.","writeTargets":[]});
    let mut context = fixture.context();
    context.config["activeOperationId"] = json!("round-1:delegate");
    let first = fixture.runtime.tasks.tool(parents[0].2.clone(),context.clone(),"task.delegate",args.clone()).await.unwrap();
    let repeated = fixture.runtime.tasks.tool(parents[0].2.clone(),context.clone(),"task.delegate",args.clone()).await.unwrap();
    let second_parent = fixture.runtime.tasks.tool(parents[1].2.clone(),context.clone(),"task.delegate",args.clone()).await.unwrap();
    for index in 0..2 {
        assert_eq!(first["tasks"][index]["taskId"],repeated["tasks"][index]["taskId"]);
        assert_ne!(first["tasks"][index]["taskId"],second_parent["tasks"][index]["taskId"]);
        assert_eq!(first["tasks"][index]["parentTaskId"],parents[0].0.task_id);
        assert_eq!(second_parent["tasks"][index]["parentTaskId"],parents[1].0.task_id);
    }
    context.config["activeOperationId"] = json!("round-2:delegate");
    let independent = fixture.runtime.tasks.tool(parents[0].2.clone(),context.clone(),"task.delegate",args).await.unwrap();
    assert_ne!(first["tasks"][0]["taskId"],independent["tasks"][0]["taskId"]);
    let explicit = json!({"requestId":"client-request","goal":"Read the assigned reference.","writeTargets":[]});
    let one = fixture.runtime.tasks.tool(parents[0].2.clone(),context.clone(),"task.delegate",explicit.clone()).await.unwrap();
    let other = fixture.runtime.tasks.tool(parents[1].2.clone(),context.clone(),"task.delegate",explicit.clone()).await.unwrap();
    context.config["activeOperationId"] = json!("round-3:delegate");
    let retry = fixture.runtime.tasks.tool(parents[0].2.clone(),context,"task.delegate",explicit).await.unwrap();
    assert_ne!(one["taskId"],other["taskId"]);
    assert_eq!(one["taskId"],retry["taskId"]);
    let repository = TaskRepository::open(&fixture.root).unwrap();
    assert_eq!(repository.children(&parents[0].0.task_id).unwrap().len(),5);
    assert_eq!(repository.children(&parents[1].0.task_id).unwrap().len(),3);
    fixture.runtime.shutdown().await;
    for (_,journal,_) in parents { journal.release("batch-fixture-finished").await.unwrap(); }
}

#[tokio::test]
async fn legacy_batch_children_are_reused_only_by_their_original_parent_dispatch() {
    legacy_batch_dispatch_recovery(false).await;
}

#[tokio::test]
async fn legacy_child_worker_dispatch_reuses_its_children_with_the_original_digest() {
    legacy_batch_dispatch_recovery(true).await;
}

async fn legacy_batch_dispatch_recovery(child_worker_digest: bool) {
    use kynxa_agent_core_native::orchestration::gateway_task_input;
    let fixture = Fixture::new();
    let input = json!({"conversationId":fixture.scope.conversation_id,"requestId":"legacy-batch-parent",
        "message":"Delegate read-only work.","provider":"unconfigured-fixture","model":"fixture","permissionMode":"smart"});
    let root = gateway_task_input::accept(&fixture.runtime.config,&input,None,&"b".repeat(64)).await.unwrap();
    let journal = TaskJournal::start_existing(&fixture.root,&root,"legacy-batch",60_000).unwrap();
    let parent = fixture.runtime.child_run(&root,journal.clone()).unwrap();
    let args = json!({"count":2,"goal":"Read the assigned reference.","writeTargets":[]});
    let mut seed_args = args.clone(); seed_args["requestId"] = json!("seed-batch");
    let seed = fixture.runtime.tasks.tool(parent.clone(),fixture.context(),"task.delegate",seed_args).await.unwrap();
    let repository = TaskRepository::open(&fixture.root).unwrap();
    let digest = kynxa_agent_core_native::retrieval::gateway_index::hash(&args.to_string());
    let mut legacy_ids = Vec::new();
    for index in 0..2 {
        let template = repository.load(seed["tasks"][index]["taskId"].as_str().unwrap()).unwrap();
        let legacy = repository.accept_task(&NewTask {
            request_id:format!("child:{digest}:{index}"), goal:template.goal, acceptance:template.acceptance,
            scope:template.scope, parent_task_id:template.parent_task_id, dependency_ids:template.dependency_ids,
            budget:template.budget, input_reference:template.input_reference,
        }).unwrap();
        repository.control(&legacy.task_id,legacy.version,TaskControl::Pause).unwrap();
        legacy_ids.push(legacy.task_id);
    }
    let dispatch_digest = if child_worker_digest {
        kynxa_agent_core_native::retrieval::gateway_index::hash(&json!({"name":"task.delegate","arguments":args}).to_string())
    } else { digest };
    journal.record_operation(&OperationIntent {operation_id:"legacy-dispatch".into(),tool_call_id:"legacy-call".into(),
        tool_name:"task.delegate".into(),input_sha256:dispatch_digest,input_reference:None,effect:OperationEffect::Irreversible}).unwrap();
    journal.dispatch_operation("legacy-dispatch").unwrap();
    let mut context = fixture.context(); context.config["activeOperationId"] = json!("legacy-dispatch");
    for _ in 0..2 {
        let resumed = fixture.runtime.tasks.tool(parent.clone(),context.clone(),"task.delegate",args.clone()).await.unwrap();
        for index in 0..2 { assert_eq!(resumed["tasks"][index]["taskId"],legacy_ids[index]); }
        assert_eq!(repository.children(&root.task_id).unwrap().len(),4);
    }
    let receipt = journal.save_blob("worker-log",&serde_json::to_vec(&json!({"tasks":legacy_ids})).unwrap()).await.unwrap();
    journal.finish_operation("legacy-dispatch",OperationState::Succeeded,Some(receipt),None).unwrap();
    context.config["activeOperationId"] = json!("new-independent-dispatch");
    let new_batch = fixture.runtime.tasks.tool(parent.clone(),context,"task.delegate",args).await.unwrap();
    assert!(new_batch["tasks"].as_array().unwrap().iter()
        .all(|child| !legacy_ids.iter().any(|id|child["taskId"] == *id)));
    assert_eq!(repository.children(&root.task_id).unwrap().len(),6);
    fixture.runtime.shutdown().await;
    journal.release("legacy-batch-finished").await.unwrap();
}

#[tokio::test]
async fn declared_dependencies_and_execution_budget_survive_the_hub_boundary() {
    use kynxa_agent_core_native::orchestration::gateway_task_input;
    let fixture = Fixture::new();
    let request = uuid::Uuid::new_v4().to_string();
    let input = json!({"conversationId":fixture.scope.conversation_id,"requestId":request,"message":"Delegate after prerequisite.","provider":"unconfigured-fixture","model":"fixture","permissionMode":"smart","runLimits":{"maxDurationMs":3_600_000}});
    let root = gateway_task_input::accept(&fixture.runtime.config, &input, None, &"a".repeat(64))
        .await
        .unwrap();
    // Effective wall time is measured independently; parallel descendant costs do not multiply the user's limit.
    // 有效墙钟独立计量；并行后代的成本不能把用户时长上限乘以执行器数量。
    assert_eq!(root.budget.max_execution_ms, Some(3_600_000));
    let journal = TaskJournal::start_existing(&fixture.root, &root, "fixture", 60_000).unwrap();
    let parent = fixture.runtime.child_run(&root, journal.clone()).unwrap();
    let repository = TaskRepository::open(&fixture.root).unwrap();
    let mut prerequisite = fixture.input("prerequisite", Some(&root.task_id));
    prerequisite.scope = root.scope.clone();
    let prerequisite = repository.accept_task(&prerequisite).unwrap();
    let delegated = fixture.runtime.tasks.tool(parent, fixture.context(), "task.delegate", json!({"goal":"Read only after prerequisite.","dependencyIds":[prerequisite.task_id],"writeTargets":[]})).await.unwrap();
    let child = repository
        .load(delegated["taskId"].as_str().unwrap())
        .unwrap();
    assert_eq!(child.dependency_ids, vec![prerequisite.task_id]);
    assert_eq!(child.budget.max_execution_ms, Some(3_600_000));
    assert_eq!(child.state, TaskState::Queued);
    assert_eq!(child.usage.model_requests, 0);
    fixture.runtime.shutdown().await;
    journal.release("fixture-ended").await.unwrap();
}

#[tokio::test]
async fn waiting_child_wakes_on_control_and_network_gap_returns_without_model_polling() {
    let fixture = Fixture::new();
    let journal = TaskJournal::begin(
        &fixture.root,
        &fixture.input("parent", None),
        "fixture-hub",
        60_000,
    )
    .unwrap();
    let parent_task = journal.load_current().unwrap();
    let parent = fixture
        .runtime
        .child_run(&parent_task, journal.clone())
        .unwrap();
    let child = journal
        .repository()
        .accept_task(&fixture.input("child", Some(journal.task_id())))
        .unwrap();
    let hub = fixture.runtime.tasks.clone();
    let parent_wait = parent.clone();
    let context = fixture.context();
    let child_id = child.task_id.clone();
    let waiting = tokio::spawn(async move {
        hub.tool(
            parent_wait,
            context,
            "task.wait",
            json!({"taskIds":[child_id],"timeoutMs":30_000}),
        )
        .await
    });
    tokio::task::yield_now().await;
    fixture
        .runtime
        .tasks
        .control(&child.task_id, child.version, TaskControl::Cancel)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["timedOut"], false);
    assert_eq!(result["allCompleted"], false);
    let network = journal
        .repository()
        .accept_task(&fixture.input("network", Some(journal.task_id())))
        .unwrap();
    journal
        .repository()
        .mark_unstarted_wait(
            &network.task_id,
            network.version,
            TaskState::WaitingNetwork,
            "fixture-temporary-network",
        )
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.runtime.tasks.tool(
            parent,
            fixture.context(),
            "task.wait",
            json!({"taskIds":[network.task_id],"timeoutMs":1_800_000}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["tasks"][0]["state"], "waiting-network");
    assert_eq!(journal.load_current().unwrap().usage.model_requests, 0);
}

#[tokio::test]
async fn parent_and_child_share_one_workspace_mutation_guard() {
    let fixture = Fixture::new();
    let guard = fixture
        .runtime
        .tasks
        .workspace_mutation_guard(&fixture.root)
        .await
        .unwrap();
    let hub = fixture.runtime.tasks.clone();
    let workdir = fixture.root.clone();
    let mut waiting = tokio::spawn(async move { hub.workspace_mutation_guard(&workdir).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut waiting)
            .await
            .is_err()
    );
    drop(guard);
    let second = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(second);
}

#[tokio::test]
async fn burst_controls_utf16_readback_and_elapsed_budget_keep_their_contracts() {
    let fixture = Fixture::new();
    let mut input = fixture.input("burst", None);
    input.budget.max_execution_ms = Some(1000);
    let journal = TaskJournal::begin(&fixture.root, &input, "fixture", 60_000).unwrap();
    for index in 0..700 {
        journal
            .emit_control("task.progress", &json!({"index":index}))
            .unwrap();
    }
    journal.flush().await.unwrap();
    let mut events = journal
        .repository()
        .read_events(journal.task_id(), 0, 512)
        .unwrap();
    events.extend(
        journal
            .repository()
            .read_events(journal.task_id(), events.last().unwrap().sequence, 512)
            .unwrap(),
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "task.progress")
            .count(),
        700
    );
    let reference = journal
        .save_blob("child-context", "甲😀乙丙".as_bytes())
        .await
        .unwrap();
    let parent = fixture
        .runtime
        .child_run(&journal.load_current().unwrap(), journal.clone())
        .unwrap();
    let result = fixture
        .runtime
        .tasks
        .tool(
            parent,
            fixture.context(),
            "task.read",
            json!({"reference":reference,"offset":2,"limit":1}),
        )
        .await
        .unwrap();
    assert_eq!(result["text"], "乙");
    assert_eq!(result["offset"], 3);
    assert_eq!(result["endOffset"], 4);
    let child = journal
        .repository()
        .accept_task(&fixture.input("elapsed-child", Some(journal.task_id())))
        .unwrap();
    let attempt = journal
        .repository()
        .start_attempt(&child.task_id, child.version, "child", 60_000)
        .unwrap();
    let usage = TaskUsage {
        model_duration_ms: 700,
        tool_duration_ms: 300,
        ..Default::default()
    };
    journal
        .repository()
        .record_usage(&attempt.token, "elapsed-once", &usage)
        .unwrap();
    journal
        .repository()
        .record_usage(&attempt.token, "elapsed-once", &usage)
        .unwrap();
    assert_eq!(journal.load_current().unwrap().usage.model_duration_ms, 700);
    assert!(!journal.load_current().unwrap().usage.is_exhausted(&input.budget),
        "Aggregated child model/tool costs are not the parent's effective wall clock.");
    journal.repository().record_active_duration(journal.token(),1000).unwrap();
    assert_eq!(
        journal
            .reserve_model_request("after-total-duration")
            .unwrap_err()
            .code,
        "TASK_BUDGET_EXHAUSTED"
    );
}

#[tokio::test]
async fn replayed_unknown_acknowledgements_do_not_resume_a_paused_task() {
    let fixture = Fixture::new();
    let journal = TaskJournal::begin(&fixture.root, &fixture.input("paused-unknown-ack", None), "fixture", 60_000).unwrap();
    journal.reserve_model_request("unknown-cost").unwrap();
    journal.record_operation(&OperationIntent { operation_id:"unknown-write".into(),tool_call_id:"unknown-call".into(),
        tool_name:"file.write".into(),input_sha256:"a".repeat(64),input_reference:None,effect:OperationEffect::FileMutation }).unwrap();
    journal.dispatch_operation("unknown-write").unwrap();
    journal.release("fixture-process-exit").await.unwrap();
    let repository = journal.repository();
    let task = journal.load_current().unwrap();
    let attempt = repository.start_attempt(&task.task_id, task.version, "fixture-reconcile", 60_000).unwrap();
    repository.mark_verification_quiescent(&attempt.token, "fixture-known-quiescence").unwrap();
    let costs = vec![ModelReservationIdentity { task_id:task.task_id.clone(),reservation_id:"unknown-cost".into() }];
    let operations = vec![TaskOperationIdentity { task_id:task.task_id.clone(),operation_id:"unknown-write".into() }];
    repository.acknowledge_unknown_model_reservations(&task.task_id, repository.load(&task.task_id).unwrap().version, &costs).unwrap();
    repository.acknowledge_unknown_task_operations(&task.task_id, repository.load(&task.task_id).unwrap().version, &operations).unwrap();
    repository.release_attempt(&attempt.token, "fixture-acknowledged-unknown").unwrap();
    let paused = fixture.runtime.tasks.control(&task.task_id, repository.load(&task.task_id).unwrap().version, TaskControl::Pause).await.unwrap();
    let version = paused["task"]["version"].as_u64().unwrap();
    // A replay confirms the same uncertainty; it cannot override a later user pause.
    // 重放只确认相同未知结果，不能覆盖用户随后作出的暂停。
    let repeated_cost = fixture.runtime.tasks.acknowledge_unknown_costs(&task.task_id, task.version, costs).await.unwrap();
    assert_eq!(repeated_cost["task"]["state"], "paused");
    assert_eq!(repeated_cost["task"]["version"], version);
    assert_eq!(repeated_cost["costUnknown"], true);
    let repeated_operation = fixture.runtime.tasks.acknowledge_unknown_operations_scoped(&task.task_id, task.version, operations).await.unwrap();
    assert_eq!(repeated_operation["task"]["state"], "paused");
    assert_eq!(repeated_operation["task"]["version"], version);
    assert_eq!(repository.current_attempt(&task.task_id).unwrap().unwrap().token, attempt.token);
    assert_eq!(repository.load(&task.task_id).unwrap().usage.model_requests, 0);
    let reservation = &repository.model_reservations(&task.task_id).unwrap()[0];
    assert_eq!(reservation.state, ModelReservationState::Unknown);
    assert!(reservation.is_unknown_acknowledged());
    assert_eq!(repository.operations(&task.task_id).unwrap()[0].state, OperationState::Unknown);
}

#[tokio::test]
async fn restart_blocks_unknown_effects_without_replaying_or_consuming_model_budget() {
    let fixture = Fixture::new();
    let journal = TaskJournal::begin(
        &fixture.root,
        &fixture.input("unknown", None),
        "fixture",
        60_000,
    )
    .unwrap();
    journal
        .record_operation(&OperationIntent {
            operation_id: "sideeffect".into(),
            tool_call_id: "call-1".into(),
            tool_name: "file.write".into(),
            input_sha256: "a".repeat(64),
            input_reference: None,
            effect: OperationEffect::FileMutation,
        })
        .unwrap();
    journal.dispatch_operation("sideeffect").unwrap();
    journal.release("fixture-process-exit").await.unwrap();
    fixture.runtime.tasks.recover().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let task = journal.load_current().unwrap();
            if task.state == TaskState::WaitingVerification {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(journal.load_current().unwrap().usage.model_requests, 0);
    assert_eq!(
        journal.repository().operations(journal.task_id()).unwrap()[0].state,
        OperationState::Unknown
    );
    fixture.runtime.tasks.shutdown().await;
    assert_ne!(journal.load_current().unwrap().state, TaskState::Cancelled);
}

#[tokio::test]
async fn unknown_child_effect_blocks_parent_and_sibling_writes_but_allows_verification_reads() {
    let fixture = Fixture::new();
    let parent = TaskJournal::begin(
        &fixture.root,
        &fixture.input("tree", None),
        "parent",
        60_000,
    )
    .unwrap();
    let child = parent
        .repository()
        .accept_task(&fixture.input("child", Some(parent.task_id())))
        .unwrap();
    let child = TaskJournal::start_existing(&fixture.root, &child, "child", 60_000).unwrap();
    let intent = OperationIntent {
        operation_id: "write-1".into(),
        tool_call_id: "call-1".into(),
        tool_name: "filesystem.write".into(),
        input_sha256: "a".repeat(64),
        input_reference: None,
        effect: OperationEffect::FileMutation,
    };
    child.record_operation(&intent).unwrap();
    child.dispatch_operation("write-1").unwrap();
    child.release("interrupted-write").await.unwrap();
    let mut next = intent.clone();
    next.operation_id = "parent-next".into();
    assert_eq!(
        parent.record_operation(&next).unwrap_err().code,
        "TASK_BLOCKED"
    );
    let sibling = parent
        .repository()
        .accept_task(&fixture.input("sibling", Some(parent.task_id())))
        .unwrap();
    let sibling = TaskJournal::start_existing(&fixture.root, &sibling, "sibling", 60_000).unwrap();
    next.operation_id = "sibling-next".into();
    assert_eq!(
        sibling.record_operation(&next).unwrap_err().code,
        "TASK_BLOCKED"
    );
    next.effect = OperationEffect::Observation;
    next.tool_name = "filesystem.read".into();
    parent.record_operation(&next).unwrap();
    parent.dispatch_operation("sibling-next").unwrap();
}

#[tokio::test]
async fn wait_returns_latest_bounded_child_report_with_a_verified_full_read_entrance() {
    let fixture = Fixture::new();
    let parent = TaskJournal::begin(
        &fixture.root,
        &fixture.input("report-parent", None),
        "parent",
        60_000,
    )
    .unwrap();
    let parent_run = fixture
        .runtime
        .child_run(&parent.load_current().unwrap(), parent.clone())
        .unwrap();
    let child = parent
        .repository()
        .accept_task(&fixture.input("report-child", Some(parent.task_id())))
        .unwrap();
    let child = TaskJournal::start_existing(&fixture.root, &child, "child", 60_000).unwrap();
    child
        .save_task_blob("task-output", &json!({"summary":"old report"}))
        .await
        .unwrap();
    let report = json!({"summary":"甲😀".repeat(3000),"evidence":[{"resultId":"fixture-proof"}],"verification":[{"exitCode":0,"current":true,"resultRef":{"resultId":"fixture-test"}}]});
    let artifact = child.save_task_blob("task-output", &report).await.unwrap();
    child.finish(TaskState::Completed, None).await.unwrap();
    let awaiting_review=fixture.runtime.tasks.tool(parent_run.clone(),fixture.context(),"task.wait",
        json!({"taskIds":[child.task_id()],"timeoutMs":100})).await.unwrap();
    assert_eq!(awaiting_review["allCompleted"],false,"A settled child still requires explicit parent acceptance.");
    assert_eq!(awaiting_review["results"][0]["parentAccepted"],false);
    let full=parent.read_blob(&artifact.reference).await.unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&full).unwrap(),report);
    fixture.runtime.tasks.tool(parent_run.clone(),fixture.context(),"task.result",json!({
        "taskId":child.task_id(),"review":{"decision":"accept","expectedVersion":child.load_current().unwrap().version,
            "outputRef":artifact.reference,"reason":"The parent inspected the current immutable full child report."}
    })).await.unwrap();
    let result = fixture
        .runtime
        .tasks
        .tool(
            parent_run,
            fixture.context(),
            "task.wait",
            json!({"taskIds":[child.task_id()],"timeoutMs":100}),
        )
        .await
        .unwrap();
    assert_eq!(result["allCompleted"], true);
    assert_eq!(result["results"][0]["parentAccepted"],true);
    assert_eq!(result["results"][0]["outputRef"], artifact.reference);
    assert!(
        result["results"][0]["summaryPreview"]
            .as_str()
            .unwrap()
            .encode_utf16()
            .count()
            <= 4096
    );
    assert_eq!(result["results"][0]["summaryTruncated"], true);
    assert_eq!(result["results"][0]["verification"][0]["exitCode"], 0);
    let bytes = parent.read_blob(&artifact.reference).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        report
    );
}

#[tokio::test]
async fn trash_stops_work_preserves_records_and_permanent_cleanup_waits_for_late_owner() {
    let fixture = Fixture::new();
    let journal = TaskJournal::begin(
        &fixture.root,
        &fixture.input("trash-parent", None),
        "parent",
        60_000,
    )
    .unwrap();
    let parent = fixture
        .runtime
        .child_run(&journal.load_current().unwrap(), journal.clone())
        .unwrap();
    let reference = journal
        .save_blob("child-context", b"private task body")
        .await
        .unwrap();
    let result = fixture
        .runtime
        .tasks
        .reconcile_deleted_chats(&json!({"Tombstones":[{"Id":fixture.scope.conversation_id}]}))
        .await
        .unwrap();
    assert_eq!(result["recordsPreserved"], true);
    assert_eq!(journal.load_current().unwrap().state, TaskState::Cancelled);
    assert!(*parent.cancellation.borrow());
    assert_eq!(
        journal.read_blob(&reference).await.unwrap(),
        b"private task body"
    );
    assert_eq!(
        journal
            .repository()
            .purge_conversation(&fixture.scope.conversation_id)
            .unwrap_err()
            .code(),
        "TASK_BLOCKED"
    );
    journal.release("late-owner-drained").await.unwrap();
    let workfile = fixture.root.join("Workspace/user-file.md");
    std::fs::create_dir_all(workfile.parent().unwrap()).unwrap();
    std::fs::write(&workfile, "keep user work").unwrap();
    let removed = fixture
        .runtime
        .tasks
        .purge_conversation(&fixture.scope.conversation_id)
        .await
        .unwrap();
    assert_eq!(removed["cleanupPending"], false);
    assert_eq!(journal.load_current().unwrap_err().code, "TASK_NOT_FOUND");
    assert!(
        !fixture
            .root
            .join(reference["relativePath"].as_str().unwrap())
            .exists()
    );
    assert!(workfile.exists());
}

#[tokio::test]
async fn failed_permanent_file_cleanup_keeps_a_retry_plan_and_blocks_recreation() {
    let fixture = Fixture::new();
    let input = fixture.input("retry-delete", None);
    let journal = TaskJournal::begin(&fixture.root, &input, "fixture", 60_000).unwrap();
    let original = b"formal task body";
    let reference = journal.save_blob("child-context", original).await.unwrap();
    let current = journal.load_current().unwrap();
    journal
        .repository()
        .control(journal.task_id(), current.version, TaskControl::Cancel)
        .unwrap();
    journal.release("ended").await.unwrap();
    let plan = journal
        .repository()
        .purge_conversation(&fixture.scope.conversation_id)
        .unwrap();
    let path = fixture
        .root
        .join(reference["relativePath"].as_str().unwrap());
    std::fs::write(&path, "changed unexpectedly").unwrap();
    assert_eq!(
        journal
            .repository()
            .retry_cleanup_files(&plan.cleanup_id)
            .unwrap_err()
            .code(),
        "TASK_CONFLICT"
    );
    assert_eq!(
        journal.repository().pending_cleanups(8).unwrap()[0]
            .last_error
            .as_deref(),
        Some("TASK_CONFLICT")
    );
    assert_eq!(
        journal.repository().accept_task(&input).unwrap_err().code(),
        "TASK_CLEANUP_PENDING"
    );
    std::fs::write(&path, original).unwrap();
    journal
        .repository()
        .retry_cleanup_files(&plan.cleanup_id)
        .unwrap();
    assert!(!path.exists());
    assert!(journal.repository().pending_cleanups(8).unwrap().is_empty());
}
