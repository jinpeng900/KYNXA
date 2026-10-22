//! Loopback model, real file effects and task-owned checkpoint recovery checks.
//! 本机模型协议、真实文件副作用及任务自有检查点恢复验证；不使用用户资料或密钥。

use super::*;
use crate::{
    gateway_contract::GatewayConfig,
    storage::task_repository::{NewTask, TaskBudget, TaskControl, TaskRepository, TaskScope, TaskUsage},
};
use axum::{
    Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone, Default)]
struct Provider {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
    delay: Arc<AtomicBool>,
    interrupted: Arc<AtomicBool>,
    recovery_final: Arc<AtomicBool>,
    recovery_read_requested: Arc<AtomicBool>,
    recovery_unknown: Arc<AtomicBool>,
    echo_history: Arc<AtomicBool>,
    early_completion: Arc<AtomicBool>,
    pure_answer: Arc<AtomicBool>,
    visual_only: Arc<AtomicBool>,
    invalid_reviews: Arc<AtomicUsize>,
    review_requests: Arc<AtomicUsize>,
    parallel_reads: Arc<AtomicBool>,
    large_usage: Arc<AtomicBool>,
}
async fn respond(State(provider): State<Provider>, Json(request): Json<Value>) -> Response {
    provider.calls.fetch_add(1, Ordering::SeqCst);
    provider.requests.lock().unwrap().push(request.clone());
    if provider.delay.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if provider.interrupted.load(Ordering::Acquire) {
        let frame = json!({"id":"child-partial","choices":[{"index":0,"delta":{"content":"已保存部分子任务正文。"},"finish_reason":null}]});
        return (
            [("content-type", "text/event-stream")],
            format!("data: {frame}\n\n"),
        )
            .into_response();
    }
    let assessment = request["messages"].as_array().into_iter().flatten().last()
        .and_then(|message|message["content"].as_str())
        .filter(|text|text.starts_with("Broker: assess the actual saved output")
            && text.contains("Expected acceptance requirements:") && text.contains("Verified completion evidence:"));
    if let Some(assessment) = assessment {
        provider.review_requests.fetch_add(1,Ordering::AcqRel);
        if provider.invalid_reviews.fetch_update(Ordering::AcqRel,Ordering::Acquire,|remaining|remaining.checked_sub(1)).is_ok() {
            return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"The previous assessment needs a repaired structured footer."},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20}})).into_response();
        }
        let requirements: Vec<Value> = assessment.lines().find_map(|line|line.strip_prefix("Expected acceptance requirements: "))
            .and_then(|text|serde_json::from_str(text).ok()).unwrap_or_default();
        let evidence: Vec<Value> = assessment.lines().find_map(|line|line.strip_prefix("Verified completion evidence: "))
            .and_then(|text|serde_json::from_str(text).ok()).unwrap_or_default();
        let met: Vec<Value> = requirements.iter().map(|requirement| json!({"requirementId":requirement["requirementId"],
            "item":requirement["item"].as_str().unwrap_or("").chars().take(100).collect::<String>(),"evidence":evidence.iter().take(8).collect::<Vec<_>>()})).collect();
        let footer = if evidence.is_empty() { json!({"state":"continue","next":"Discover the file writer, create the delegated file and read it back.","unmet":["The actual requested operation has not executed."]}) }
            else { json!({"state":"complete","next":"","unmet":[],"met":met}) };
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":format!("<kynxa_task>{footer}</kynxa_task>")},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20}})).into_response();
    }
    if provider.pure_answer.load(Ordering::Acquire) {
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"你好！"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":3}})).into_response();
    }
    if provider.parallel_reads.load(Ordering::Acquire) {
        let already_read=request["messages"].as_array().into_iter().flatten().any(|message|message["role"]=="tool");
        let message=if already_read { json!({"role":"assistant","content":"Both delegated sources were read. <kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"unmet\":[]}</kynxa_task>"}) }
            else {json!({"role":"assistant","content":"Read the two independent sources.","tool_calls":[
                {"id":"parallel-first","type":"function","function":{"name":crate::models::protocol::wire_name("filesystem.read"),"arguments":json!({"path":"first.txt"}).to_string()}},
                {"id":"parallel-second","type":"function","function":{"name":crate::models::protocol::wire_name("filesystem.read"),"arguments":json!({"path":"second.txt"}).to_string()}}]})};
        return Json(json!({"choices":[{"index":0,"finish_reason":if already_read{"stop"}else{"tool_calls"},"message":message}],"usage":{"prompt_tokens":100,"completion_tokens":20}})).into_response();
    }
    if provider.visual_only.load(Ordering::Acquire) || provider.early_completion.swap(false,Ordering::AcqRel) {
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"The requested result is ready.\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"unmet\":[]}</kynxa_task>"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20}})).into_response();
    }
    if provider.recovery_final.load(Ordering::Acquire) {
        let review = request["messages"].as_array().into_iter().flatten().rev()
            .filter_map(|message|message["content"].as_str())
            .find(|text|text.contains("Expected acceptance requirements:") && text.contains("Verified completion evidence:"));
        let message = if let Some(review) = review {
            // Reviews use only the broker's successful receipt allowlist, never a hard-coded original command ID.
            // 核对只使用 Broker 已验证成功回执清单，不把未知原命令硬编码成成功证据。
            let requirements: Vec<Value> = review.lines().find_map(|line|line.strip_prefix("Expected acceptance requirements: "))
                .and_then(|text|serde_json::from_str(text).ok()).unwrap_or_default();
            let evidence: Vec<Value> = review.lines().find_map(|line|line.strip_prefix("Verified completion evidence: "))
                .and_then(|text|serde_json::from_str(text).ok()).unwrap_or_default();
            let footer = if evidence.is_empty() {
                json!({"state":"blocked","next":"","reason":"No successful observation proves the recovered current state.","blocker":"external","unmet":["Read the current actual state before completion."]})
            } else {
                let met: Vec<Value> = requirements.iter().map(|requirement|json!({"requirementId":requirement["requirementId"],"item":requirement["item"],"evidence":evidence})).collect();
                json!({"state":"complete","next":"","reason":"Successful current-state read verified; no prior effect repeated or reclassified.","unmet":[],"met":met})
            };
            json!({"role":"assistant","content":format!("<kynxa_task>{footer}</kynxa_task>")})
        } else if provider.recovery_unknown.load(Ordering::Acquire) && !provider.recovery_read_requested.swap(true, Ordering::AcqRel) {
            let path = "fixture-service-state.json";
            json!({"role":"assistant","content":"Inspect the current actual state without replaying the earlier operation.","tool_calls":[{"id":"recovery-current-state-read","type":"function","function":{"name":crate::models::protocol::wire_name("filesystem.read"),"arguments":json!({"path":path}).to_string()}}]})
        } else {
            let text = if provider.recovery_unknown.load(Ordering::Acquire) {
                "The fixture state was read. The original start command remains unknown and was not run again; user acknowledgement does not prove it succeeded."
            } else { "The original successful file-write receipt was verified. No write was repeated." };
            json!({"role":"assistant","content":format!("{text}\n<kynxa_task>{{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Actual current-state observation completed\"}}</kynxa_task>")})
        };
        return Json(json!({"choices":[{"index":0,"finish_reason":if message["tool_calls"].is_array(){"tool_calls"}else{"stop"},"message":message}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if request["tools"].as_array().is_none_or(Vec::is_empty) {
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"Partial task history summary; actual receipt references and pending original goal remain authoritative."},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    let results = request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "tool")
        .count();
    let mut message = if results < 3 {
        let (name, arguments) = match results {
            0 => ("tool.load", json!({"names":["filesystem.write"]})),
            1 => (
                "filesystem.write",
                json!({"path":"created.txt","content":"真实子任务产物","expectedHash":null}),
            ),
            _ => ("filesystem.read", json!({"path":"created.txt"})),
        };
        json!({"role":"assistant","content":"Continue delegated work.","tool_calls":[{"id":format!("child-call-{results}"),"type":"function","function":{"name":crate::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]})
    } else {
        json!({"role":"assistant","content":"Read back the actual created file. No test suite was run.\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Creation and source read complete\"}</kynxa_task>"})
    };
    if provider.echo_history.load(Ordering::Acquire) {
        let text = format!("公开进度。\n{{\n  \"historicalToolCalls\" : [{{\"id\":\"CHILD_PRIVATE_ECHO\",\"arguments\":{{\"nested\":\"value\"}}}}]\n}}\n{}",message["content"].as_str().unwrap());
        message["content"] = json!(text);
        if results >= 3 {
            // Split inside every reserved key and nested object boundary; nothing private may leak through live deltas.
            // 在保留键及嵌套对象内部逐字符分块，实时增量也不能先泄露后清理。
            let mut stream = String::new();
            for character in text.chars() {
                let frame = json!({"choices":[{"index":0,"delta":{"content":character.to_string()},"finish_reason":null}]});
                stream.push_str(&format!("data: {frame}\n\n"));
            }
            let terminal = json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}});
            stream.push_str(&format!("data: {terminal}\n\ndata: [DONE]\n\n"));
            return ([("content-type", "text/event-stream")], stream).into_response();
        }
    }
    Json(
        json!({"choices":[{"index":0,"finish_reason":if message["tool_calls"].is_array() {"tool_calls"} else {"stop"},"message":message}],"usage":{"prompt_tokens":100,"completion_tokens":if provider.large_usage.load(Ordering::Acquire){1500}else{20},"total_tokens":120}}),
    ).into_response()
}

struct Fixture {
    runtime: Arc<GatewayRuntime>,
    parent: Arc<RequestRun>,
    child: TaskRecord,
    context: GatewayToolContext,
    root: PathBuf,
    provider: Provider,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-child-worker-{}", new_id()));
        for folder in ["Models", "Data", "Extensions", "work"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        let provider = Provider::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/chat/completions", post(respond))
            .with_state(provider.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let runtime = GatewayRuntime::new(GatewayConfig {
            data_home: root.join("Models"),
            data_root: root.join("Data"),
            extension_root: root.join("Extensions"),
            application_root: root.join("Application"),
            port: 0,
            toolhost_exe: root.join("absent.exe"),
            vector_extension: root.join("absent.dll"),
            inference_library: root.join("absent-onnx.dll"),
        });
        runtime.models.save(json!({"providerId":"child-fixture","displayName":"Child fixture","baseUrl":url,"apiKey":"","protocol":"openai-completions","models":["fixture-model"],"contextWindowTokens":32768,"maxOutputTokens":4096})).await.unwrap();
        let chat = new_id();
        let project = new_id();
        let old = data::load_catalog(&runtime.config.data_root).unwrap();
        data::handle("PUT", "/api/conversations/catalog", &[], &json!({"Revision":old["Revision"],"Chats":[],"Projects":[{"Id":project,"Name":"Worker fixture","FolderPath":root.join("work"),"Chats":[{"Id":chat,"Title":"Parent","Messages":[{"Id":new_id(),"Role":"user","Content":"Create a delegated file.","CreatedAt":"2026-10-13T00:00:00Z"}]}]}]}), &runtime.config.data_root).unwrap().unwrap();
        let (_cancel, receiver) = tokio::sync::watch::channel(false);
        let context = runtime
            .tool_context(&chat, "full", "Create a delegated file.", receiver)
            .await
            .unwrap();
        let scope = TaskScope {
            project_id: context.project_id.clone(),
            conversation_id: chat,
            permission_snapshot: json!({"permissionMode":"full","bindingSnapshot":context.config["bindingSnapshot"]}),
        };
        let parent_journal = TaskJournal::begin(
            &runtime.config.data_root,
            &NewTask {
                request_id: new_id(),
                goal: "Parent goal with PRIVATE_PARENT_HISTORY sentinel".into(),
                acceptance: vec![],
                scope: scope.clone(),
                parent_task_id: None,
                dependency_ids: vec![],
                budget: TaskBudget::default(),
                input_reference: None,
            },
            "fixture-parent",
            60_000,
        )
        .unwrap();
        let parent_record = parent_journal.load_current().unwrap();
        let parent = runtime.child_run(&parent_record, parent_journal).unwrap();
        let repository = TaskRepository::open(&runtime.config.data_root).unwrap();
        let child = repository
            .accept_task(&NewTask {
                request_id: format!("child:{}", new_id()),
                goal: "Create created.txt and read the result.".into(),
                acceptance: vec![],
                scope,
                parent_task_id: Some(parent_record.task_id),
                dependency_ids: vec![],
                budget: TaskBudget::default(),
                input_reference: None,
            })
            .unwrap();
        Self {
            runtime,
            parent,
            child,
            context,
            root,
            provider,
            server,
        }
    }
    fn input(&self) -> Value {
        json!({"provider":"child-fixture","model":"fixture-model","goal":self.child.goal,"contextRefs":[],"writeTargets":["created.txt"],"rootUserConstraints":"Only created.txt may be changed."})
    }
    fn additional_child(&self, goal: &str) -> TaskRecord {
        TaskRepository::open(&self.runtime.config.data_root).unwrap().accept_task(&NewTask {
            request_id:format!("child:{}",new_id()),goal:goal.into(),acceptance:vec![],scope:self.child.scope.clone(),
            parent_task_id:self.child.parent_task_id.clone(),dependency_ids:vec![],budget:TaskBudget::default(),input_reference:None
        }).unwrap()
    }
    async fn close(self) {
        self.server.abort();
        let _ = self.server.await;
        self.runtime.shutdown().await;
        let _ = crate::storage::sqlite_connections::close(
            &self.runtime.config.data_root,
            &self.runtime.config.data_root.join("tasks.sqlite"),
        );
        assert!(self.root.starts_with(std::env::temp_dir()));
        let _ = std::fs::remove_dir_all(self.root);
    }
}

#[tokio::test]
async fn delegated_model_tool_receipt_loop_uses_private_context_and_actual_file() {
    let fixture = Fixture::new().await;
    let result = execute(
        fixture.runtime.clone(),
        fixture.parent.clone(),
        fixture.child.clone(),
        fixture.context.clone(),
        fixture.input(),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("work/created.txt")).unwrap(),
        "真实子任务产物"
    );
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 5);
    assert!(
        fixture
            .provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.to_string().contains("PRIVATE_PARENT_HISTORY"))
    );
    assert!(!result["evidence"].as_array().unwrap().is_empty());
    assert_eq!(result["changedFiles"].as_array().unwrap().len(), 1);
    assert!(result["verification"].as_array().unwrap().is_empty());
    assert!(!result["summary"].as_str().unwrap().contains("kynxa_task"));
    assert!(!result["summary"].as_str().unwrap().contains("Continue delegated work."));
    assert_eq!(result["summarySource"],"latest-delivery");
    let saved_output:Value=serde_json::from_slice(&fixture.parent.journal.as_ref().unwrap().read_blob(&result["outputHistoryRef"]).await.unwrap()).unwrap();
    assert!(saved_output["output"].as_str().unwrap().contains("Continue delegated work."));
    assert_eq!(result["acceptanceReview"]["reviewed"],true);
    assert_eq!(result["acceptanceReview"]["parentAcceptance"],"pending");
    assert!(!result["acceptanceReview"]["readbackRefs"].as_array().unwrap().is_empty());
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    assert_eq!(
        repository.operations(&fixture.child.task_id).unwrap().len(),
        3
    );
    assert_eq!(
        repository
            .load(&fixture.child.task_id)
            .unwrap()
            .usage
            .model_requests,
        5
    );
    assert_eq!(
        repository
            .load(fixture.parent.journal.as_ref().unwrap().task_id())
            .unwrap()
            .usage
            .model_requests,
        5
    );
    let chat = data::read_model_messages(
        &fixture.runtime.config.data_root,
        &fixture.context.conversation_id,
    )
    .unwrap();
    assert_eq!(
        chat.len(),
        1,
        "Child must not fabricate a parent chat turn."
    );
    fixture.close().await;
}

#[tokio::test]
async fn child_soft_segments_renew_without_resetting_cumulative_usage_or_replaying_writes() {
    let fixture=Fixture::new().await;
    fixture.provider.large_usage.store(true,Ordering::Release);
    let journal=TaskJournal::start_existing(&fixture.runtime.config.data_root,&fixture.child,"soft-segment-fixture",60000).unwrap();
    journal.repository().record_active_duration(journal.token(),1_800_001).unwrap();
    journal.finish(TaskState::WaitingConfig,Some("saved-before-soft-segment")).await.unwrap();
    drop(journal);
    let child=TaskRepository::open(&fixture.runtime.config.data_root).unwrap().load(&fixture.child.task_id).unwrap();
    let mut input=fixture.input(); input["runLimits"]=json!({"maxRounds":1,"maxToolCalls":1,"maxGeneratedTokens":1024});
    // This watchdog includes five real HTTP requests and durable checkpoints under concurrent test/build I/O.
    // 此看门狗覆盖五次真实 HTTP 请求及持久检查点；为并行测试和构建的磁盘竞争留余量，不改变生产预算。
    let result=tokio::time::timeout(Duration::from_secs(30),execute(fixture.runtime.clone(),fixture.parent.clone(),child,fixture.context.clone(),input))
        .await.expect("A soft segment boundary must preserve the active completion-review phase.").unwrap();
    assert_eq!(result["status"],"completed","{result}");
    let repository=TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let current=repository.load(&fixture.child.task_id).unwrap();
    assert!(current.usage.active_duration_ms.unwrap()>1_800_001);
    assert_eq!(current.usage.model_requests,5);
    assert_eq!(repository.operations(&fixture.child.task_id).unwrap().iter().filter(|operation|operation.intent.tool_name=="filesystem.write").count(),1);
    let bytes=fixture.parent.journal.as_ref().unwrap().read_blob(&result["contextRef"]).await.unwrap();
    let saved:Value=serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved["rounds"],5);
    assert_eq!(saved["toolCalls"],3);
    assert!(saved["generatedTokens"].as_u64().unwrap()>1024);
    assert!(saved["segmentRoundBase"].as_u64().unwrap()>0);
    assert!(saved["segmentToolBase"].as_u64().unwrap()>0);
    assert!(saved["segmentGeneratedBase"].as_u64().unwrap()>0);
    assert!(saved["segmentActiveBaseMs"].as_u64().unwrap()>=1_800_001);
    fixture.close().await;
}

#[tokio::test]
async fn independent_child_reads_use_real_ordered_archives_without_duplicate_dispatch() {
    let fixture=Fixture::new().await;
    fixture.provider.parallel_reads.store(true,Ordering::Release);
    std::fs::write(fixture.root.join("work/first.txt"),"第一份独立来源").unwrap();
    std::fs::write(fixture.root.join("work/second.txt"),"第二份独立来源").unwrap();
    let child=fixture.additional_child("Read first.txt and second.txt, then summarize the actual content.");
    let mut input=fixture.input(); input["goal"]=json!(child.goal);input["writeTargets"]=json!([]);input["requiresVerification"]=json!(true);
    let result=execute(fixture.runtime.clone(),fixture.parent.clone(),child.clone(),fixture.context.clone(),input).await.unwrap();
    assert_eq!(result["status"],"completed","{result}");
    let repository=TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let operations=repository.operations(&child.task_id).unwrap();
    assert_eq!(operations.len(),2,"Each prefetched read has exactly one formal operation.");
    assert!(operations.iter().all(|operation|operation.state==OperationState::Succeeded));
    assert_eq!(result["evidence"].as_array().unwrap().len(),2);
    let request=fixture.provider.requests.lock().unwrap().iter().find(|request|request["messages"].as_array().into_iter().flatten().any(|message|message["role"]=="tool")).unwrap().clone();
    let paired:Vec<_>=request["messages"].as_array().unwrap().iter().filter(|message|message["role"]=="tool").filter_map(|message|message["tool_call_id"].as_str()).collect();
    assert_eq!(paired,vec!["parallel-first","parallel-second"]);
    fixture.close().await;
}

#[tokio::test]
async fn child_self_review_requires_parent_acceptance_and_sibling_access_is_denied() {
    let fixture=Fixture::new().await;
    let result=execute(fixture.runtime.clone(),fixture.parent.clone(),fixture.child.clone(),fixture.context.clone(),fixture.input()).await.unwrap();
    let repository=TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let current=repository.load(&fixture.child.task_id).unwrap();
    let before=fixture.runtime.tasks.children_acceptance(fixture.parent.journal.as_ref().unwrap().task_id()).await.unwrap();
    assert_eq!(before["allAccepted"],false);
    let review=json!({"taskId":current.task_id,"review":{"decision":"accept","expectedVersion":current.version,"outputRef":result["outputRef"],"reason":"The parent read the actual created file and current receipt-backed acceptance."}});
    let accepted=fixture.runtime.tasks.tool(fixture.parent.clone(),fixture.context.clone(),"task.result",review.clone()).await.unwrap();
    assert_eq!(accepted["decision"],"accept");
    fixture.runtime.tasks.tool(fixture.parent.clone(),fixture.context.clone(),"task.result",review).await.unwrap();
    assert_eq!(fixture.runtime.tasks.children_acceptance(fixture.parent.journal.as_ref().unwrap().task_id()).await.unwrap()["allAccepted"],true);
    let sibling=fixture.additional_child("Read-only sibling task.");
    let journal=TaskJournal::start_existing(&fixture.runtime.config.data_root,&sibling,"sibling-scope",60000).unwrap();
    let run=fixture.runtime.child_run(&sibling,journal.clone()).unwrap();
    let denied=fixture.runtime.tasks.tool(run,fixture.context.clone(),"task.status",json!({"taskId":current.task_id})).await.unwrap_err();
    assert_eq!(denied.code,"TASK_SCOPE_DENIED");
    let sibling_current=journal.load_current().unwrap();
    repository.control(&sibling.task_id,sibling_current.version,TaskControl::Pause).unwrap();
    journal.finish(TaskState::Paused,Some("test-complete")).await.unwrap();
    drop(journal);
    fixture.close().await;
}

#[tokio::test]
async fn waiting_for_150_logical_children_paginates_without_dropping_queued_work() {
    let fixture=Fixture::new().await;
    let children:Vec<_>=(0..150).map(|index|fixture.additional_child(&format!("Independent queued assignment {index}"))).collect();
    let ids:Vec<_>=children.iter().map(|child|child.task_id.clone()).collect();
    let page=fixture.runtime.tasks.tool(fixture.parent.clone(),fixture.context.clone(),"task.wait",
        json!({"taskIds":ids,"timeoutMs":1,"limit":64})).await.unwrap();
    assert_eq!(page["total"],150);
    assert_eq!(page["results"].as_array().unwrap().len(),64);
    assert_eq!(page["nextOffset"],64);
    assert_eq!(page["allCompleted"],false);
    let repository=TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    assert!(children.iter().all(|child|repository.load(&child.task_id).unwrap().state==TaskState::Queued));
    fixture.close().await;
}

#[tokio::test]
async fn nested_hub_delegation_uses_real_root_input_and_cannot_expand_parent_write_targets() {
    let fixture=Fixture::new().await;
    fixture.provider.delay.store(true,Ordering::Release);
    fixture.provider.pure_answer.store(true,Ordering::Release);
    let original=json!({"conversationId":fixture.context.conversation_id,"requestId":new_id(),
        "provider":"child-fixture","model":"fixture-model","permissionMode":"full",
        "message":"Delegate scoped work without accessing unrelated files."});
    let root=super::super::gateway_task_input::accept(&fixture.runtime.config,&original,None,"nested-hub-fixture").await.unwrap();
    let journal=TaskJournal::start_existing(&fixture.runtime.config.data_root,&root,"nested-root",60000).unwrap();
    let parent=fixture.runtime.child_run(&root,journal.clone()).unwrap();
    let child=fixture.runtime.tasks.tool(parent.clone(),fixture.context.clone(),"task.delegate",
        json!({"goal":"Say hello after admission.","writeTargets":["created.txt"]})).await.unwrap();
    let child_id=child["taskId"].as_str().unwrap().to_owned();
    let child_run=tokio::time::timeout(Duration::from_secs(3),async {
        loop { if let Some(run)=fixture.runtime.live_task_run(&child_id){break run;}
            tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.expect("The accepted child must attach through its real worker.");
    let denied=fixture.runtime.tasks.tool(child_run.clone(),fixture.context.clone(),"task.delegate",
        json!({"goal":"Read a scoped result.","writeTargets":["other.txt"]})).await.unwrap_err();
    assert_eq!(denied.code,"CHILD_WRITE_SCOPE_BLOCKED");
    let grandchild=fixture.runtime.tasks.tool(child_run,fixture.context.clone(),"task.delegate",
        json!({"goal":"Say hello after admission.","writeTargets":["created.txt"]})).await.unwrap();
    assert_eq!(grandchild["parentTaskId"],child_id);
    let grandchild_id=grandchild["taskId"].as_str().unwrap().to_owned();
    let repository=TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    assert_eq!(repository.load(&grandchild_id).unwrap().scope.permission_snapshot,root.scope.permission_snapshot);
    tokio::time::timeout(Duration::from_secs(3),async {
        while fixture.runtime.live_task_run(&grandchild_id).is_none(){tokio::time::sleep(Duration::from_millis(5)).await;}
    }).await.expect("The accepted grandchild must attach before testing whole-tree cancellation.");
    // Cancel via the real control API and wait for both child attempts to release before removing fixture data.
    // 使用正式控制接口取消整树，等待两个子尝试释放，再清理隔离夹具数据。
    tokio::time::timeout(Duration::from_secs(3),async {loop {
        let current=repository.load(&root.task_id).unwrap();
        match fixture.runtime.tasks.control(&root.task_id,current.version,TaskControl::Cancel).await {
            Ok(_)=>break,
            Err(error) if error.code=="TASK_CONFLICT" || error.code=="TASK_VERSION_CONFLICT"=>tokio::task::yield_now().await,
            Err(error)=>panic!("Regular cancellation failed: {error:?}"),
        }
    }}).await.expect("Regular cancellation must not spin on version changes.");
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            if [&child_id,&grandchild_id].iter().all(|id|repository.current_attempt(id).unwrap().is_none_or(|attempt|attempt.ended_at_ms.is_some())
                && fixture.runtime.live_task_run(id).is_none()){break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("Cancellation must release every attached child worker.");
    journal.finish(TaskState::Cancelled,Some("nested-test-complete")).await.unwrap();
    drop(parent);drop(journal);
    fixture.close().await;
}

#[tokio::test]
async fn an_early_child_completion_and_three_malformed_reviews_continue_to_real_verified_work() {
    let fixture = Fixture::new().await;
    fixture.provider.early_completion.store(true,Ordering::Release);
    fixture.provider.invalid_reviews.store(3,Ordering::Release);
    let result = execute(fixture.runtime.clone(),fixture.parent.clone(),fixture.child.clone(),fixture.context.clone(),fixture.input()).await.unwrap();
    assert_eq!(result["status"],"completed","{result}");
    assert_eq!(result["acceptanceReview"]["reviewed"],true);
    assert!(fixture.provider.review_requests.load(Ordering::Acquire) >= 5);
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/created.txt")).unwrap(),"真实子任务产物");
    let operations = TaskRepository::open(&fixture.runtime.config.data_root).unwrap().operations(&fixture.child.task_id).unwrap();
    assert_eq!(operations.iter().filter(|operation|operation.intent.tool_name=="filesystem.write").count(),1,
        "Assessment repair must not replay the successful file write.");
    assert!(!result["summary"].as_str().unwrap().contains("repaired structured footer"));
    fixture.close().await;
}

#[tokio::test]
async fn a_delegated_finite_greeting_finishes_in_one_model_request_without_tool_review() {
    let fixture = Fixture::new().await;
    fixture.provider.pure_answer.store(true,Ordering::Release);
    let child = fixture.additional_child("说你好100个小时");
    let mut input = fixture.input();
    input["goal"] = json!(child.goal);
    input["writeTargets"] = json!([]);
    let result = execute(fixture.runtime.clone(),fixture.parent.clone(),child,fixture.context.clone(),input).await.unwrap();
    assert_eq!(result["status"],"completed","{result}");
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire),1);
    assert_eq!(fixture.provider.review_requests.load(Ordering::Acquire),0);
    assert_eq!(result["summary"],"你好！");
    assert_eq!(result["acceptanceReview"]["required"],false);
    fixture.close().await;
}

#[tokio::test]
async fn owned_visual_readback_rehydrates_images_into_the_actual_child_model_request() {
    use base64::Engine;
    let fixture = Fixture::new().await;
    fixture.provider.visual_only.store(true,Ordering::Release);
    let child = fixture.additional_child("Inspect rendered Blender image quality.");
    let journal = TaskJournal::start_existing(&fixture.runtime.config.data_root,&child,"visual-source",60_000).unwrap();
    let run = fixture.runtime.child_run(&child,journal.clone()).unwrap();
    let call = json!({"id":"owned-image-read","name":"filesystem.read","arguments":{"path":"visual.png"}});
    let encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aB9sAAAAASUVORK5CYII=";
    std::fs::write(fixture.root.join("work/visual.png"),base64::engine::general_purpose::STANDARD.decode(encoded).unwrap()).unwrap();
    let canonical = json!({"content":[{"type":"image","mimeType":"image/png","data":encoded}]});
    let receipt = archive_receipt(&fixture.runtime,&run,&call,json!({"status":"completed","isError":false,"executed":true,"content":canonical})).await.unwrap();
    let projected = data::public_tool_result(&receipt["content"],Some(&receipt["resultRef"]),None,false).unwrap();
    assert!(!task::receipt_has_visual_observation(&projected),"A local media placeholder must not certify visible input.");
    let intent = operation_intent(&call,true,0);
    journal.record_operation(&intent).unwrap();
    journal.dispatch_operation(&intent.operation_id).unwrap();
    let owned = journal.save_blob("worker-log",&serde_json::to_vec(&receipt).unwrap()).await.unwrap();
    journal.finish_operation(&intent.operation_id,OperationState::Succeeded,Some(owned),None).unwrap();
    let state = WorkerContext { history:vec![json!({"role":"user","content":child.goal})],pending:Some(protocol::ModelResponse {
        content:String::new(),reasoning:String::new(),calls:vec![call.clone()],continuation:vec![json!({"role":"assistant","content":"","tool_calls":[{"id":call["id"],"type":"function","function":{"name":crate::models::protocol::wire_name("filesystem.read"),"arguments":call["arguments"].to_string()}}]})],
        finish:json!("tool_calls"),usage:json!({}),output_truncated:false,raw:json!({}) }),..Default::default() };
    checkpoint(&run,&journal,&state,"visual-pending").await.unwrap();
    journal.finish(TaskState::WaitingConfig,Some("visual-fixture-restart")).await.unwrap();
    let child=journal.load_current().unwrap();
    drop(run);
    drop(journal);
    let mut input = fixture.input();
    input["goal"] = json!(child.goal);
    input["writeTargets"] = json!([]);
    input["requiresVerification"] = json!(true);
    input["requiresVisualReadback"] = json!(true);
    let result = execute(fixture.runtime.clone(),fixture.parent.clone(),child,fixture.context.clone(),input).await.unwrap();
    assert_eq!(result["status"],"completed","{result}");
    assert_eq!(result["acceptanceReview"]["reviewed"],true);
    assert!(!result["acceptanceReview"]["visualReadbackRefs"].as_array().unwrap().is_empty());
    let requests = fixture.provider.requests.lock().unwrap();
    assert!(requests.iter().filter(|request|crate::models::protocol::image_inputs(request)>0).count() >= 2,
        "Both the actual continuation and visual assessment must carry the rehydrated image.");
    assert!(requests.iter().any(|request|request.to_string().contains(encoded)));
    drop(requests);
    fixture.close().await;
}

#[tokio::test]
async fn delegated_history_echo_is_hidden_in_snapshots_deltas_and_summary_without_losing_receipts() {
    let fixture = Fixture::new().await;
    fixture.provider.echo_history.store(true, Ordering::Release);
    let result = execute(
        fixture.runtime.clone(),
        fixture.parent.clone(),
        fixture.child.clone(),
        fixture.context.clone(),
        fixture.input(),
    ).await.unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    let summary = result["summary"].as_str().unwrap();
    assert!(summary.contains("公开进度。") && summary.contains("Read back the actual created file."), "{summary}");
    assert!(!summary.contains("historicalToolCalls") && !summary.contains("CHILD_PRIVATE_ECHO"), "{summary}");
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    assert_eq!(repository.operations(&fixture.child.task_id).unwrap().len(), 3);
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 5);
    let mut public_events = Vec::new();
    let mut after = 0;
    loop {
        let events = repository.read_events(&fixture.child.task_id, after, 512).unwrap();
        if events.is_empty() { break; }
        after = events.last().unwrap().sequence;
        public_events.extend(events.into_iter().filter(|event| ["text_delta", "content_snapshot"].contains(&event.kind.as_str())).map(|event| event.payload));
    }
    assert!(!public_events.is_empty(), "The test must observe real relayed child text.");
    let live_text: String = public_events.iter().filter_map(|event| event["delta"].as_str().or_else(||event["content"].as_str())).collect();
    assert!(!live_text.contains("historicalToolCalls") && !live_text.contains("CHILD_PRIVATE_ECHO"), "{live_text}");
    assert!(live_text.contains("Read back the actual created file."));
    let saved = repository.latest_checkpoint(&fixture.child.task_id).unwrap().unwrap();
    let bytes = std::fs::read(fixture.runtime.config.data_root.join(saved.context_reference["relativePath"].as_str().unwrap())).unwrap();
    let checkpoint = checkpoint_decode(&bytes).unwrap();
    assert!(checkpoint.history.iter().any(|message| message["content"].as_str().is_some_and(|content|content.contains("CHILD_PRIVATE_ECHO"))), "Raw provider history must survive for native continuation and tool pairing.");
    assert!(!checkpoint.output.contains("CHILD_PRIVATE_ECHO"));
    fixture.close().await;
}

#[tokio::test]
async fn restart_checkpoint_preserves_settled_file_effect_without_replaying() {
    let fixture = Fixture::new().await;
    let journal = TaskJournal::start_existing(
        &fixture.runtime.config.data_root,
        &fixture.child,
        "previous-child",
        60_000,
    )
    .unwrap();
    let run = fixture
        .runtime
        .child_run(&fixture.child, journal.clone())
        .unwrap();
    let call = json!({"id":"restart-write","name":"filesystem.write","arguments":{"path":"created.txt","content":"A","expectedHash":null}});
    let intent = operation_intent(&call, false, 0);
    journal.record_operation(&intent).unwrap();
    journal.dispatch_operation(&intent.operation_id).unwrap();
    std::fs::write(fixture.root.join("work/created.txt"), "A").unwrap();
    let receipt = archive_receipt(
        &fixture.runtime,
        &run,
        &call,
        json!({"status":"completed","isError":false,"executed":true,"content":{"success":true}}),
    )
    .await
    .unwrap();
    let saved_receipt = journal
        .save_blob("worker-log", &serde_json::to_vec(&receipt).unwrap())
        .await
        .unwrap();
    journal
        .finish_operation(
            &intent.operation_id,
            OperationState::Succeeded,
            Some(saved_receipt),
            None,
        )
        .unwrap();
    let state = WorkerContext {
        history: vec![json!({"role":"user","content":"Continue."})],
        pending: Some(protocol::ModelResponse {
            content: String::new(),
            reasoning: String::new(),
            calls: vec![call.clone()],
            continuation: vec![],
            finish: json!("tool_calls"),
            usage: json!({}),
            output_truncated: false,
            raw: json!({}),
        }),
        ..Default::default()
    };
    checkpoint(&run,&journal, &state, "restart-fixture")
        .await
        .unwrap();
    journal
        .finish(TaskState::WaitingConfig, Some("fixture-restart"))
        .await
        .unwrap();
    std::fs::write(fixture.root.join("work/created.txt"), "B").unwrap();
    drop(run);
    drop(journal);
    // A fresh repository and journal model restart ownership without sharing mutable worker context.
    // 新仓储与日志重建执行所有权，不复用内存 worker 状态；已成功回执不能重放。
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let task = repository.load(&fixture.child.task_id).unwrap();
    let resumed = TaskJournal::start_existing(
        &fixture.runtime.config.data_root,
        &task,
        "resumed-child",
        60_000,
    )
    .unwrap();
    let saved = resumed.latest_checkpoint().unwrap().unwrap();
    let restored =
        checkpoint_decode(&resumed.read_blob(&saved.context_reference).await.unwrap()).unwrap();
    assert_eq!(restored.pending.as_ref().unwrap().calls[0], call);
    let previous = previous_operation(&resumed, &call, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(previous["reused"], true);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("work/created.txt")).unwrap(),
        "B"
    );
    assert_eq!(repository.operations(&task.task_id).unwrap().len(), 1);
    resumed
        .finish(TaskState::Partial, Some("fixture-ended"))
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn child_write_scope_and_unknown_dispatch_remain_enforced_on_restart() {
    let fixture = Fixture::new().await;
    let write = json!({"id":"outside-write","name":"filesystem.write","arguments":{"path":"other.txt","content":"X","expectedHash":null}});
    assert_eq!(
        check_write_target(&fixture.context, &fixture.input(), &write)
            .unwrap_err()
            .code,
        "CHILD_WRITE_TARGET_REQUIRED"
    );
    assert!(
        check_write_target(
            &fixture.context,
            &fixture.input(),
            &json!({"name":"terminal.host.run","arguments":{"command":"echo X"}})
        )
        .is_err()
    );
    assert!(child_tool_allowed("task.delegate"),"Nested delegation is allowed only through the Hub's real ancestry and narrowed write grants.");
    assert!(!child_tool_allowed("work.folder.bind"));
    let journal = TaskJournal::start_existing(
        &fixture.runtime.config.data_root,
        &fixture.child,
        "unknown-child",
        60_000,
    )
    .unwrap();
    let intent = operation_intent(&write, false, 0);
    journal.record_operation(&intent).unwrap();
    journal.dispatch_operation(&intent.operation_id).unwrap();
    journal
        .finish(TaskState::WaitingVerification, Some("unknown-effect"))
        .await
        .unwrap();
    let task = journal.repository().load(&fixture.child.task_id).unwrap();
    let recovered = TaskJournal::start_existing(
        &fixture.runtime.config.data_root,
        &task,
        "verify-child",
        60_000,
    )
    .unwrap();
    let receipt = previous_operation(&recovered, &write, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt["status"], "unknown");
    assert_eq!(receipt["executed"], false);
    let mut effects = EffectLedger::default();
    effects.apply("effects.observe", &json!({"call":write,"result":effect_receipt(&receipt),"workspaceRoot":fixture.context.workdir})).unwrap();
    assert_eq!(
        effects.apply("effects.state", &json!({})).unwrap()["hasUnknownEffects"],
        true
    );
    assert!(!fixture.root.join("work/other.txt").exists());
    assert!(recovered.record_operation(&operation_intent(&json!({"id":"second-write","name":"filesystem.write","arguments":{"path":"created.txt"}}), false, 1)).is_err());
    recovered
        .finish(TaskState::WaitingVerification, Some("fixture-ended"))
        .await
        .unwrap();
    fixture.close().await;
}

#[test]
fn child_document_create_respects_exact_write_targets_and_selected_mount() {
    let (mut context, _cancellation) = tools::test_context();
    let input = json!({"writeTargets":["only.docx"]});
    let allowed = json!({"id":"document-create","name":"document.create","arguments":{"path":"only.docx","content":"正文","expectedHash":null}});
    assert!(check_write_target(&context, &input, &allowed).is_ok());
    assert!(matches!(operation_intent(&allowed, false, 1).effect, OperationEffect::FileMutation));
    let mut outside = allowed.clone();
    outside["arguments"]["path"] = json!("other.docx");
    assert_eq!(check_write_target(&context, &input, &outside).unwrap_err().code, "CHILD_WRITE_TARGET_REQUIRED");
    let second = context.workdir.join("second");
    std::fs::create_dir(&second).unwrap();
    context.config["workspaceMounts"] = json!([{"id":"second","path":second}]);
    outside["arguments"]["path"] = json!("only.docx");
    outside["arguments"]["mountId"] = json!("second");
    assert_eq!(check_write_target(&context, &input, &outside).unwrap_err().code, "CHILD_WRITE_TARGET_REQUIRED");
    std::fs::remove_dir_all(context.data_home.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn recovery_reconciles_original_tool_and_model_receipts_once_without_replay() {
    let fixture = Fixture::new().await;
    let journal = TaskJournal::start_existing(&fixture.runtime.config.data_root, &fixture.child, "receipt-before-crash", 60000).unwrap();
    let run = fixture.runtime.child_run(&fixture.child, journal.clone()).unwrap();
    let call = json!({"id":"recovery-write","name":"filesystem.write","arguments":{"path":"created.txt","content":"A","expectedHash":null}});
    let state = WorkerContext {
        pending: Some(protocol::ModelResponse { content:String::new(), reasoning:String::new(), calls:vec![call.clone()],
            continuation:vec![], finish:json!("tool_calls"), usage:json!({}), output_truncated:false, raw:json!({}) }),
        ..Default::default()
    };
    checkpoint(&run,&journal, &state, "model-received").await.unwrap();
    let intent = operation_intent(&call, false, 0);
    journal.record_operation(&intent).unwrap();
    journal.dispatch_operation(&intent.operation_id).unwrap();
    let canonical = tools::execute("filesystem.write", call["arguments"].clone(), &fixture.context).await.unwrap();
    archive_receipt(&fixture.runtime, &run, &call, json!({"status":"completed","isError":false,"content":canonical})).await.unwrap();
    journal.reserve_model_request("request-before-crash").unwrap();
    let connection = fixture.runtime.models.connection_for("child-fixture").await.unwrap().unwrap();
    let response = crate::models::gateway::request(&connection,
        json!({"model":"fixture-model","messages":[{"role":"user","content":"Return one bounded summary."}],"stream":false}),
        run.cancellation.subscribe(), None).await.unwrap();
    let usage = TaskUsage { model_requests:1,
        input_tokens:response.usage["prompt_tokens"].as_u64().unwrap(),
        output_tokens:response.usage["completion_tokens"].as_u64().unwrap(), ..Default::default() };
    // The real response is persisted, but its accounting transaction is interrupted.
    // 真实响应已落盘，但模拟在计量结算事务前中断。
    let provider = crate::storage::task_repository::ProviderTokenUsage {
        input_tokens:Some(usage.input_tokens), output_tokens:Some(usage.output_tokens),
        client_metrics:Some(crate::storage::task_repository::ModelRequestClientMetrics {
            first_visible_text_ms:Some(37),
            cache_policy:Some(json!({"provider":"fixture","mode":"implicit","breakpoints":0})),
        }), ..Default::default()
    };
    journal.repository().append_events(journal.token(), &[("model-receipt:request-before-crash".into(), "task.model.receipt".into(),
        json!({"reservationId":"request-before-crash","attemptId":journal.token().attempt_id,"dispatched":true,"usage":usage,"providerUsage":provider}))]).unwrap();
    journal.finish(TaskState::WaitingVerification, Some("fixture-crash-window")).await.unwrap();
    std::fs::write(fixture.root.join("work/created.txt"), "B").unwrap();
    let task = journal.load_current().unwrap();
    let recovered = TaskJournal::start_existing(&fixture.runtime.config.data_root, &task, "receipt-recovery", 60000).unwrap();
    let report = recovered.reconcile_receipts().await.unwrap();
    assert_eq!(report["resolvedOperations"], 1, "{report}");
    assert_eq!(report["resolvedReservations"], 1, "{report}");
    assert!(report["pending"].as_array().unwrap().is_empty(), "{report}");
    let replay = recovered.reconcile_receipts().await.unwrap();
    assert_eq!(replay["resolvedOperations"], 0);
    assert_eq!(replay["resolvedReservations"], 0);
    assert_eq!(recovered.load_current().unwrap().usage.model_requests, 1);
    assert_eq!(recovered.repository().model_reservations(recovered.task_id()).unwrap()
        .into_iter().find(|reservation| reservation.reservation_id == "request-before-crash").unwrap()
        .provider_usage, Some(provider));
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/created.txt")).unwrap(), "B");
    assert_eq!(previous_operation(&recovered, &call, 0).await.unwrap().unwrap()["reused"], true);
    recovered.finish(TaskState::Partial, Some("fixture-ended")).await.unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn production_resume_reconciles_steering_copies_and_continues_the_same_root() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new().await;
    let request_id = new_id();
    let user_id = new_id();
    let reply_id = new_id();
    let input = json!({"conversationId":fixture.context.conversation_id,"requestId":request_id,"userMessageId":user_id,
        "message":"Confirm the previously created file from its actual receipt, without writing it again.",
        "provider":"child-fixture","model":"fixture-model","permissionMode":"full"});
    let mut identity = Sha256::new();
    for unit in input["message"].as_str().unwrap().encode_utf16() { identity.update(unit.to_le_bytes()); }
    let fingerprint = crate::retrieval::gateway_index::hash(&json!([input, format!("{:x}", identity.finalize())]).to_string());
    let task = super::super::gateway_task_input::accept(&fixture.runtime.config, &input, None, &fingerprint).await.unwrap();
    let journal = TaskJournal::start_existing(&fixture.runtime.config.data_root, &task, "root-before-crash", 60_000).unwrap();
    let call = json!({"id":"original-root-write","name":"filesystem.write","arguments":{"path":"created.txt","content":"A","expectedHash":null}});
    data::upsert_message(&fixture.runtime.config.data_root, &task.scope.conversation_id,
        &json!({"Id":user_id,"Role":"user","Content":input["message"],"CreatedAt":"2026-10-18T00:00:00Z"})).unwrap();
    let connection = fixture.runtime.models.connection_for("child-fixture").await.unwrap().unwrap();
    let origin = json!({"providerId":"child-fixture","model":"fixture-model","protocol":"openai-completions",
        "connectionFingerprint":crate::retrieval::gateway_index::hash(&json!(["child-fixture","fixture-model","openai-completions",connection["baseUrl"],connection["apiKey"].as_str().unwrap_or("")]).to_string())});
    let transcript = json!({"version":1,"origin":origin,"rounds":[{"round":1,"text":"Create the requested file.","calls":[call],"observations":[]}]});
    // A steering reply legitimately contains the same original call as the superseded assistant.
    // 插话后的回复合法保留旧助手的同一原始调用，核验应去重且只更新最新回复。
    for (id, created) in [(&request_id,"2026-10-18T00:00:01Z"),(&reply_id,"2026-10-18T00:00:02Z")] {
        data::upsert_message(&fixture.runtime.config.data_root, &task.scope.conversation_id,
            &json!({"Id":id,"Role":"assistant","RequestId":request_id,"OriginUserMessageId":user_id,"Provider":"child-fixture","Model":"fixture-model",
                "Content":"","CreatedAt":created,"Status":"interrupted","ModelTranscript":transcript})).unwrap();
    }
    let intent = crate::storage::task_repository::OperationIntent {
        operation_id:"round-1-original-root-write".into(), tool_call_id:"original-root-write".into(), tool_name:"filesystem.write".into(),
        input_sha256:managed_files::digest(call["arguments"].to_string().as_bytes()),
        input_reference:Some(json!({"kind":"conversation-tool-call","conversationId":task.scope.conversation_id,"requestId":request_id,"toolCallId":call["id"],"round":1})),
        effect:crate::storage::task_repository::OperationEffect::FileMutation,
    };
    journal.record_operation(&intent).unwrap();
    journal.dispatch_operation(&intent.operation_id).unwrap();
    let canonical = tools::execute("filesystem.write", call["arguments"].clone(), &fixture.context).await.unwrap();
    data::save_tool_result(&fixture.runtime.config.data_root, &task.scope.conversation_id, &request_id, &call, &canonical, None).unwrap();
    journal.reserve_model_request("root-model-before-crash").unwrap();
    let response = crate::models::gateway::request(&connection,
        json!({"model":"fixture-model","messages":[{"role":"user","content":"Return one bounded summary."}],"stream":false}),
        fixture.parent.cancellation.subscribe(), None).await.unwrap();
    let usage = TaskUsage { model_requests:1,input_tokens:response.usage["prompt_tokens"].as_u64().unwrap(),
        output_tokens:response.usage["completion_tokens"].as_u64().unwrap(), ..Default::default() };
    journal.repository().append_events(journal.token(), &[("model-receipt:root-model-before-crash".into(),"task.model.receipt".into(),
        json!({"reservationId":"root-model-before-crash","attemptId":journal.token().attempt_id,"dispatched":true,"usage":usage}))]).unwrap();
    // A read interrupted before its receipt remains observable; it must not block the recovered write.
    // 只读操作中断且缺回执不能阻塞已有真实写入回执的恢复。
    let read = operation_intent(&json!({"id":"old-observation","name":"filesystem.read","arguments":{"path":"created.txt"}}), true, 0);
    journal.record_operation(&read).unwrap();
    journal.dispatch_operation(&read.operation_id).unwrap();
    let before_recovery = journal.load_current().unwrap();
    assert!(journal.repository().transition(journal.token(), before_recovery.version, TaskState::Completed, None).is_err(),
        "A dispatched write and unsettled model cost must still prevent completion.");
    journal.finish(TaskState::WaitingVerification, Some("root-crash-window")).await.unwrap();
    std::fs::write(fixture.root.join("work/created.txt"), "B").unwrap();
    fixture.provider.recovery_final.store(true, Ordering::Release);
    let current = journal.load_current().unwrap();
    let result = fixture.runtime.tasks.control(&task.task_id, current.version, TaskControl::Resume).await.unwrap();
    assert_eq!(result["task"]["taskId"], task.task_id, "{result}");
    assert_ne!(result["task"]["state"], "waiting-verification", "{result}");
    assert_eq!(result["recovery"]["resolvedOperations"], 1, "{result}");
    assert_eq!(result["recovery"]["resolvedReservations"], 1, "{result}");
    assert!(result["recovery"]["pending"].as_array().unwrap().is_empty(), "{result}");
    let formal = data::read_model_messages(&fixture.runtime.config.data_root, &task.scope.conversation_id).unwrap();
    let old = formal.iter().find(|message| message["Id"] == request_id).unwrap();
    let latest = formal.iter().find(|message| message["Id"] == reply_id).unwrap();
    assert!(old["ModelTranscript"]["rounds"][0]["observations"].as_array().unwrap().is_empty());
    assert_eq!(latest["ModelTranscript"]["rounds"][0]["observations"][0]["id"], call["id"]);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = journal.load_current().unwrap();
            if current.state.is_terminal() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("Same root must continue after real receipt settlement.");
    let completed = journal.load_current().unwrap();
    assert_eq!(completed.state, TaskState::Completed, "{completed:?}");
    assert_eq!(completed.task_id, task.task_id);
    assert_eq!(completed.usage.model_requests, fixture.provider.calls.load(Ordering::SeqCst) as u64);
    let reservation = journal.repository().model_reservations(&task.task_id).unwrap().into_iter()
        .find(|reservation| reservation.reservation_id == "root-model-before-crash").unwrap();
    assert_eq!(reservation.usage, Some(usage));
    assert_eq!(journal.repository().operation(&task.task_id, &intent.operation_id).unwrap().unwrap().state, OperationState::Succeeded);
    assert_eq!(journal.repository().operation(&task.task_id, &read.operation_id).unwrap().unwrap().state, OperationState::Unknown);
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/created.txt")).unwrap(), "B");
    fixture.close().await;
}

#[tokio::test]
async fn user_acknowledged_unknown_effect_resumes_the_same_root_without_replay() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new().await;
    let request_id = new_id();
    let user_id = new_id();
    let input = json!({"conversationId":fixture.context.conversation_id,"requestId":request_id,"userMessageId":user_id,
        "message":"Inspect and report the fixture service's current recorded state, preserving the unknown prior start attempt without starting it again.",
        "provider":"child-fixture","model":"fixture-model","permissionMode":"full"});
    let mut identity = Sha256::new();
    for unit in input["message"].as_str().unwrap().encode_utf16() { identity.update(unit.to_le_bytes()); }
    let fingerprint = crate::retrieval::gateway_index::hash(&json!([input, format!("{:x}", identity.finalize())]).to_string());
    let task = super::super::gateway_task_input::accept(&fixture.runtime.config, &input, None, &fingerprint).await.unwrap();
    let journal = TaskJournal::start_existing(&fixture.runtime.config.data_root, &task, "root-before-exit", 60_000).unwrap();
    // The host command is only recorded and dispatched; nothing executes it, so no receipt can exist.
    // 宿主命令只登记并标记派发，测试从不执行它，因此不存在可核验的回执。
    let call = json!({"id":"original-host-run","name":"terminal.host.run","arguments":{"command":"fixture-service --start"}});
    data::upsert_message(&fixture.runtime.config.data_root, &task.scope.conversation_id,
        &json!({"Id":user_id,"Role":"user","Content":input["message"],"CreatedAt":"2026-10-20T00:00:00Z"})).unwrap();
    let connection = fixture.runtime.models.connection_for("child-fixture").await.unwrap().unwrap();
    let origin = json!({"providerId":"child-fixture","model":"fixture-model","protocol":"openai-completions",
        "connectionFingerprint":crate::retrieval::gateway_index::hash(&json!(["child-fixture","fixture-model","openai-completions",connection["baseUrl"],connection["apiKey"].as_str().unwrap_or("")]).to_string())});
    let transcript = json!({"version":1,"origin":origin,"rounds":[{"round":1,"text":"Start the service.","calls":[call],"observations":[]}]});
    data::upsert_message(&fixture.runtime.config.data_root, &task.scope.conversation_id,
        &json!({"Id":request_id,"Role":"assistant","RequestId":request_id,"OriginUserMessageId":user_id,"Provider":"child-fixture","Model":"fixture-model",
            "Content":"","CreatedAt":"2026-10-20T00:00:01Z","Status":"interrupted","ModelTranscript":transcript,
            "ToolActivities":[{"toolCallId":call["id"],"name":call["name"],"arguments":call["arguments"],"round":1,"order":1,"status":"running"}]})).unwrap();
    let intent = crate::storage::task_repository::OperationIntent {
        operation_id:"round-1-original-host-run".into(), tool_call_id:"original-host-run".into(), tool_name:"terminal.host.run".into(),
        input_sha256:managed_files::digest(call["arguments"].to_string().as_bytes()),
        input_reference:Some(json!({"kind":"conversation-tool-call","conversationId":task.scope.conversation_id,"requestId":request_id,"toolCallId":call["id"],"round":1})),
        effect:crate::storage::task_repository::OperationEffect::Irreversible,
    };
    journal.record_operation(&intent).unwrap();
    journal.dispatch_operation(&intent.operation_id).unwrap();
    journal.finish(TaskState::WaitingVerification, Some("executor-exited")).await.unwrap();
    std::fs::write(fixture.root.join("work/fixture-service-state.json"),
        r#"{"service":"fixture-service","recordedState":"stopped","originalStartOutcome":"unknown"}"#).unwrap();
    fixture.provider.recovery_unknown.store(true, Ordering::Release);
    fixture.provider.recovery_final.store(true, Ordering::Release);

    // Ordinary resume cannot settle the unknown outcome and never calls the model.
    // 普通继续无法结算未知结果，也不会调用模型。
    let current = journal.load_current().unwrap();
    let waiting = fixture.runtime.tasks.control(&task.task_id, current.version, TaskControl::Resume).await.unwrap();
    assert_eq!(waiting["task"]["state"], "waiting-verification", "{waiting}");
    assert!(waiting["recovery"]["pending"].as_array().unwrap().iter()
        .any(|item| item["code"] == "TOOL_RECEIPT_EVIDENCE_REQUIRED" && item["operationId"] == intent.operation_id), "{waiting}");
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);

    let current = journal.load_current().unwrap();
    let result = fixture.runtime.tasks
        .acknowledge_unknown(&task.task_id, current.version, vec![intent.operation_id.clone()]).await.unwrap();
    assert_eq!(result["task"]["taskId"], task.task_id, "{result}");
    assert_ne!(result["task"]["state"], "waiting-verification", "{result}");
    assert_eq!(result["acknowledgedOperationIds"][0], intent.operation_id, "{result}");
    assert!(result["recovery"]["pending"].as_array().is_none_or(Vec::is_empty), "{result}");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if journal.load_current().unwrap().state.is_terminal() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("The same root must continue after the user accepts the unknown outcome.");
    let finished = journal.load_current().unwrap();
    assert_eq!(finished.state, TaskState::Completed, "{finished:?}");
    let operations = journal.repository().operations(&task.task_id).unwrap();
    assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == "terminal.host.run").count(), 1,
        "The original host command must not be replayed.");
    let original = operations.iter().find(|operation| operation.intent.operation_id == intent.operation_id).unwrap();
    assert_eq!(original.state, OperationState::Unknown, "Confirmation must not mark the command as succeeded.");
    assert!(original.is_unknown_acknowledged());
    assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == "filesystem.read" && operation.state == OperationState::Succeeded).count(), 1,
        "The resumed model must make a real read-only observation before reporting the recorded current state.");
    let requests = fixture.provider.requests.lock().unwrap().clone();
    assert!(!requests.is_empty());
    let prompt = requests[0]["messages"].to_string();
    assert!(prompt.contains("unknown outcomes") && prompt.contains("original-host-run")
        && prompt.contains("observe the current actual state"), "{prompt}");
    fixture.close().await;
}

#[tokio::test]
async fn production_resume_keeps_unknown_effects_waiting_and_reports_the_missing_evidence() {
    let fixture = Fixture::new().await;
    let journal = TaskJournal::start_existing(&fixture.runtime.config.data_root, &fixture.child, "missing-receipt", 60000).unwrap();
    let call = json!({"id":"unknown-recovery-write","name":"filesystem.write","arguments":{"path":"created.txt","content":"A","expectedHash":null}});
    let intent = operation_intent(&call, false, 0);
    journal.record_operation(&intent).unwrap();
    journal.dispatch_operation(&intent.operation_id).unwrap();
    journal.reserve_model_request("unknown-model-request").unwrap();
    journal.finish(TaskState::WaitingVerification, Some("missing-originals")).await.unwrap();
    fixture.parent.journal.as_ref().unwrap().release("fixture-old-parent-ended").await.unwrap();
    std::fs::write(fixture.root.join("work/created.txt"), "Existing file is not an execution receipt").unwrap();
    let task = journal.load_current().unwrap();
    let result = fixture.runtime.tasks.control(&task.task_id, task.version, TaskControl::Resume).await.unwrap();
    assert_eq!(result["task"]["taskId"], task.task_id);
    assert_eq!(result["task"]["state"], "waiting-verification", "{result}");
    let pending = result["recovery"]["pending"].as_array().unwrap();
    assert!(pending.iter().any(|item| item["code"] == "MODEL_DISPATCH_EVIDENCE_REQUIRED"));
    assert!(pending.iter().any(|item| item["code"] == "TOOL_RECEIPT_EVIDENCE_REQUIRED"));
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(journal.repository().operation(&task.task_id, &intent.operation_id).unwrap().unwrap().state, OperationState::Unknown);
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/created.txt")).unwrap(), "Existing file is not an execution receipt");
    fixture.close().await;
}

#[tokio::test]
async fn steering_releases_parent_child_wait_without_cancelling_child() {
    let fixture = Fixture::new().await;
    let mut context = fixture.context.clone();
    context.config["activeSteeringSequence"] = json!(0);
    let parent = fixture.parent.clone();
    let signal = async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        parent.steering.send_modify(|sequence| *sequence += 1);
    };
    let wait = fixture.runtime.tasks.tool(
        fixture.parent.clone(), context, "task.wait",
        json!({"taskIds":[fixture.child.task_id],"timeoutMs":30_000}),
    );
    let (result, _) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(wait, signal)
    })
    .await
    .expect("User steering must not wait for the child or its 30-second polling budget.");
    let result = result.unwrap();
    assert_eq!(result["steeringPending"], true);
    assert_eq!(result["timedOut"], false);
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    assert_eq!(
        repository.load(&fixture.child.task_id).unwrap().state,
        TaskState::Queued,
    );
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 0);
    assert!(repository.operations(&fixture.child.task_id).unwrap().is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn pause_inflight_keeps_actual_failed_model_cost_and_saves_recoverable_context() {
    let fixture = Fixture::new().await;
    fixture.provider.delay.store(true, Ordering::Release);
    let runtime = fixture.runtime.clone();
    let child = fixture.child.clone();
    let calls = fixture.provider.calls.clone();
    let pause = async move {
        tokio::time::timeout(Duration::from_secs(10), async {
            while calls.load(Ordering::Acquire) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let repository = TaskRepository::open(&runtime.config.data_root).unwrap();
        let current = repository.load(&child.task_id).unwrap();
        repository
            .control(
                &child.task_id,
                current.version,
                crate::storage::task_repository::TaskControl::Pause,
            )
            .unwrap();
        runtime.cancel_task_worker(&child.task_id).await;
    };
    let (result, _) = tokio::join!(
        execute(
            fixture.runtime.clone(),
            fixture.parent.clone(),
            fixture.child.clone(),
            fixture.context.clone(),
            fixture.input()
        ),
        pause
    );
    let result = result.unwrap();
    assert_eq!(result["status"], "paused", "{result}");
    assert!(result["outputRef"].is_object());
    assert!(result["contextRef"].is_object());
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let current = repository.load(&fixture.child.task_id).unwrap();
    assert_eq!(current.state, TaskState::Paused);
    assert_eq!(current.usage.model_requests, 1);
    assert_eq!(current.usage.failed_requests, 1);
    assert_eq!(current.usage.unknown_token_requests, 1);
    assert!(!fixture.root.join("work/created.txt").exists());
    fixture.close().await;
}

#[tokio::test]
async fn compaction_preserves_original_context_through_real_hub_readback() {
    let fixture = Fixture::new().await;
    let journal = TaskJournal::start_existing(
        &fixture.runtime.config.data_root,
        &fixture.child,
        "compacting-child",
        60_000,
    )
    .unwrap();
    let run = fixture
        .runtime
        .child_run(&fixture.child, journal.clone())
        .unwrap();
    let connection = fixture
        .runtime
        .models
        .connection_for("child-fixture")
        .await
        .unwrap()
        .unwrap();
    let original = "原始前段不会丢失 ".repeat(4_000);
    let mut context = WorkerContext {
        history: vec![
            json!({"role":"user","content":fixture.child.goal}),
            json!({"role":"assistant","content":original}),
            json!({"role":"user","content":"Continue from the actual source."}),
        ],
        ..Default::default()
    };
    let catalog = tools::builtin_catalog()
        .iter()
        .find(|tool| tool["name"] == "task.read")
        .map(|tool| vec![gateway_loop::descriptor(tool, "task.read")])
        .unwrap_or_default();
    compact_child(
        &fixture.runtime,
        &run,
        &fixture.context,
        &journal,
        &connection,
        "fixture-model",
        8192,
        "Original delegated scope is fixed.",
        &mut context,
        &catalog,
        4096,
        &json!({"contextWindowTokens":8192,"providerMaxInputTokens":4096,"providerMaxOutputTokens":1024}),
        false,
    )
    .await
    .unwrap();
    let artifacts = journal.repository().artifacts(journal.task_id()).unwrap();
    let original_ref = artifacts
        .iter()
        .filter(|artifact| artifact.kind == "child-context")
        .find_map(|artifact| {
            let path = fixture
                .runtime
                .config
                .data_root
                .join(artifact.reference["relativePath"].as_str()?);
            let bytes = std::fs::read(path).ok()?;
            let saved = checkpoint_decode(&bytes).ok()?;
            saved
                .history
                .iter()
                .any(|message| message["content"] == original)
                .then(|| artifact.reference.clone())
        })
        .expect("The complete original context must remain formally referenced.");
    let read = fixture
        .runtime
        .tasks
        .tool(
            run.clone(),
            fixture.context.clone(),
            "task.read",
            json!({"reference":original_ref,"offset":0,"limit":65_536}),
        )
        .await
        .unwrap();
    assert!(read["text"].as_str().unwrap().contains("原始前段不会丢失"));
    assert_eq!(read["unit"], "utf16");
    let mut forged = original_ref.clone();
    forged["sha256"] = json!("0".repeat(64));
    assert!(
        fixture
            .runtime
            .tasks
            .tool(
                run,
                fixture.context.clone(),
                "task.read",
                json!({"reference":forged})
            )
            .await
            .is_err()
    );
    journal
        .finish(TaskState::Partial, Some("compaction-fixture-finished"))
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn incomplete_model_stream_preserves_text_and_retry_cost_until_explicit_pause() {
    let fixture = Fixture::new().await;
    fixture.provider.interrupted.store(true, Ordering::Release);
    let runtime = fixture.runtime.clone();
    let child = fixture.child.clone();
    let calls = fixture.provider.calls.clone();
    let pause = async move {
        tokio::time::timeout(Duration::from_secs(8),async {
            while calls.load(Ordering::Acquire) < 3 { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.expect("The stream repair must make progress before the fixture pauses it.");
        let repository = TaskRepository::open(&runtime.config.data_root).unwrap();
        let current = repository.load(&child.task_id).unwrap();
        repository.control(&child.task_id,current.version,TaskControl::Pause).unwrap();
        runtime.cancel_task_worker(&child.task_id).await;
    };
    let (result,_) = tokio::join!(execute(
        fixture.runtime.clone(),
        fixture.parent.clone(),
        fixture.child.clone(),
        fixture.context.clone(),
        fixture.input(),
    ),pause);
    let result = result.unwrap();
    assert!(
        result["summary"]
            .as_str()
            .unwrap()
            .contains("已保存部分子任务正文。"),
        "{result}"
    );
    assert_eq!(result["status"], "paused", "{result}");
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    assert!(
        repository
            .operations(&fixture.child.task_id)
            .unwrap()
            .is_empty()
    );
    let current = repository.load(&fixture.child.task_id).unwrap();
    assert_eq!(
        current.usage.model_requests,
        fixture.provider.calls.load(Ordering::Acquire) as u64
    );
    assert!(current.usage.failed_requests > 0);
    assert!(current.usage.model_requests >= 3);
    assert!(current.usage.retries <= current.usage.model_requests - 1);
    let original = repository
        .latest_checkpoint(&fixture.child.task_id)
        .unwrap()
        .unwrap();
    let reference = original.context_reference["relativePath"].as_str().unwrap();
    assert!(
        std::fs::read_to_string(fixture.runtime.config.data_root.join(reference))
            .unwrap()
            .contains("已保存部分子任务正文。")
    );
    assert!(!fixture.root.join("work/created.txt").exists());
    fixture.close().await;
}
