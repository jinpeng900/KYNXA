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
    if provider.recovery_final.load(Ordering::Acquire) {
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"Original receipt verified; recovered task complete.\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Original result verified, no operation repeated\"}</kynxa_task>"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
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
    let message = if results < 3 {
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
    Json(
        json!({"choices":[{"index":0,"finish_reason":if message["tool_calls"].is_array() {"tool_calls"} else {"stop"},"message":message}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}}),
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
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 4);
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
        4
    );
    assert_eq!(
        repository
            .load(fixture.parent.journal.as_ref().unwrap().task_id())
            .unwrap()
            .usage
            .model_requests,
        4
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
    checkpoint(&journal, &state, "restart-fixture")
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
    assert!(!child_tool_allowed("task.delegate"));
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
    checkpoint(&journal, &state, "model-received").await.unwrap();
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
    journal.repository().append_events(journal.token(), &[("model-receipt:request-before-crash".into(), "task.model.receipt".into(),
        json!({"reservationId":"request-before-crash","attemptId":journal.token().attempt_id,"dispatched":true,"usage":usage}))]).unwrap();
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
async fn incomplete_model_stream_preserves_text_and_actual_retry_cost_without_calls() {
    let fixture = Fixture::new().await;
    fixture.provider.interrupted.store(true, Ordering::Release);
    let result = execute(
        fixture.runtime.clone(),
        fixture.parent.clone(),
        fixture.child.clone(),
        fixture.context.clone(),
        fixture.input(),
    )
    .await
    .unwrap();
    assert!(
        result["summary"]
            .as_str()
            .unwrap()
            .contains("已保存部分子任务正文。"),
        "{result}"
    );
    assert_eq!(result["status"], "waiting-network", "{result}");
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
    assert_eq!(current.usage.retries, current.usage.model_requests - 1);
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
