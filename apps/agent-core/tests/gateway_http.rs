//! Real loopback HTTP/SSE checks against a local deterministic provider, without Node or credentials.
//! 使用本机确定性供应商验证真实 HTTP/SSE、工具续接与持久化，不调用 Node 或真实模型凭据。

use axum::{
    Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::StreamExt;
use kynxa_agent_core_native::{
    gateway,
    gateway_contract::{GatewayConfig, new_id},
    orchestration::gateway_runtime::GatewayRuntime,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Mutex, task::JoinHandle};

#[derive(Clone)]
struct Provider {
    requests: Arc<Mutex<Vec<Value>>>,
    calls: Arc<AtomicUsize>,
    tool: bool,
    cancel: bool,
    mcp: bool,
    malformed_calls: Arc<AtomicUsize>,
    unloaded_cycle: Arc<std::sync::atomic::AtomicBool>,
    archived_read_ids: Arc<Mutex<Vec<String>>>,
    context_pressure: Arc<std::sync::atomic::AtomicBool>,
    pressure_step: Arc<AtomicUsize>,
    pressure_read_reentry: Arc<std::sync::atomic::AtomicBool>,
    steering_cycle: Arc<std::sync::atomic::AtomicBool>,
    output_continuation: Arc<std::sync::atomic::AtomicBool>,
    control_note_cycle: Arc<std::sync::atomic::AtomicBool>,
    cancel_summary: Arc<std::sync::atomic::AtomicBool>,
    summary_requests: Arc<AtomicUsize>,
}
fn system_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "system")
        .map(|message| message["content"].to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Follow the actual broker navigation, so the fixture cannot invent a missing document identity.
/// 使用实际发送的文档导航；夹具不能自行补造压缩后已经丢失的定位。
fn saved_document(request: &Value, path_suffix: &str) -> Value {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| {
            let text = message["content"].as_str()?;
            let marker = "Saved document work: ";
            let start = text.find(marker)? + marker.len();
            let navigation = serde_json::Deserializer::from_str(&text[start..])
                .into_iter::<Value>()
                .next()?
                .ok()?;
            navigation["documents"]
                .as_array()?
                .iter()
                .find(|entry| {
                    entry["source"]
                        .as_str()
                        .is_some_and(|path| path.ends_with(path_suffix))
                })
                .cloned()
        })
        .next_back()
        .unwrap_or_else(|| {
            panic!("The current document is missing from actual broker navigation: {path_suffix}")
        })
}

async fn mock_provider(State(state): State<Provider>, Json(body): Json<Value>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.requests.lock().await.push(body.clone());
    if state.control_note_cycle.load(Ordering::Acquire)
        && system_text(&body)
            .contains("Summarize public evidence for continuation of the same user task")
    {
        state.summary_requests.fetch_add(1, Ordering::SeqCst);
        let payload: Value = serde_json::from_str(
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "user")
                .unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let mut facts = payload["priorFacts"].as_array().unwrap().clone();
        let current = json!({"resultId":payload["source"]["resultId"],"text":"The current authorized file body was read; preserve its original source reference."});
        if !facts
            .iter()
            .any(|fact| fact["resultId"] == current["resultId"])
        {
            facts.push(current);
        }
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":json!({"facts":facts}).to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.cancel_summary.load(Ordering::Acquire)
        && system_text(&body)
            .contains("Summarize public evidence for continuation of the same user task")
    {
        state.summary_requests.fetch_add(1, Ordering::SeqCst);
        // The real HTTP request has arrived, but its response is still incomplete when the user cancels.
        // 真实 HTTP 摘要请求已到达；用户取消时响应仍不完整，不将其记成未调用或成功摘要。
        let chunk = json!({"id":"summary-cancel-fixture","choices":[{"index":0,"delta":{"content":"{\"facts\":["},"finish_reason":null}]}).to_string();
        let events =
            futures_util::stream::unfold(Some(format!("data: {chunk}\n\n")), |chunk| async move {
                if let Some(chunk) = chunk {
                    Some((Ok::<_, std::convert::Infallible>(chunk), None))
                } else {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    None
                }
            });
        return (
            [("content-type", "text/event-stream")],
            axum::body::Body::from_stream(events),
        )
            .into_response();
    }
    if state
        .malformed_calls
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"","tool_calls":[{
            "id":"incomplete-fixture","type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("tool.search"),"arguments":"{"}}]},
            "finish_reason":"tool_calls"}]})).into_response();
    }
    if state.cancel
        && (!state.steering_cycle.load(Ordering::Acquire)
            || state.calls.load(Ordering::SeqCst) == 1)
    {
        let chunk = json!({"id":"cancel-fixture","choices":[{"index":0,"delta":{"content":"已开始，保留这段正文。"},"finish_reason":null}]}).to_string();
        let events = futures_util::stream::unfold(
            (Some(format!("data: {chunk}\n\n")), false),
            |(chunk, delayed)| async move {
                if let Some(chunk) = chunk {
                    return Some((Ok::<_, std::convert::Infallible>(chunk), (None, false)));
                }
                if !delayed {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                None
            },
        );
        return (
            [("content-type", "text/event-stream")],
            axum::body::Body::from_stream(events),
        )
            .into_response();
    }
    let previous_result = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|message| message["role"] == "tool");
    let archive_ids = state.archived_read_ids.lock().await.clone();
    let continuation = state.output_continuation.load(Ordering::Acquire);
    let control_cycle = state.control_note_cycle.load(Ordering::Acquire);
    let control_step = control_cycle.then(|| state.pressure_step.fetch_add(1, Ordering::SeqCst));
    let continuation_first =
        (continuation && state.calls.load(Ordering::Acquire) == 1) || control_step == Some(0);
    let mut message = if let Some(step) = control_step {
        match step {
            0 => {
                json!({"role":"assistant","content":"Initial answer body; the authorized source check remains unfinished."})
            }
            1 | 4 => {
                json!({"role":"assistant","content":"Read the next authorized source.","tool_calls":[{"id":format!("control-source-{step}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.read"),"arguments":json!({"path":if step == 1 {"fixture.txt"}else{"second.txt"},"maxChars":100}).to_string()}}]})
            }
            2 => {
                json!({"role":"assistant","content":"The first current source was read; the second source remains to be checked."})
            }
            3 => {
                json!({"role":"assistant","content":"<kynxa_task>{\"state\":\"continue\",\"next\":\"Read second.txt and finish the original answer\",\"reason\":\"The second current source has not been checked\"}</kynxa_task>"})
            }
            5 => {
                json!({"role":"assistant","content":"Both authorized current sources were checked.\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Both source reads are complete\"}</kynxa_task>"})
            }
            _ => {
                json!({"role":"assistant","content":"<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Both source reads are complete\"}</kynxa_task>"})
            }
        }
    } else if continuation {
        json!({"role":"assistant","content":if continuation_first {"第一段正文：保留这一段。"}
            else {"第二段正文：续写已完成。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"两段正文完成\"}</kynxa_task>"}})
    } else if state.context_pressure.load(Ordering::Acquire) {
        if body["tools"].as_array().is_none_or(Vec::is_empty) {
            json!({"role":"assistant","content":"Public evidence summary; pending target remains second.txt and original receipts remain readable."})
        } else {
            let step = state.pressure_step.fetch_add(1, Ordering::SeqCst);
            let read_reentry = state.pressure_read_reentry.load(Ordering::Acquire);
            if step < if read_reentry { 15 } else { 4 } {
                let (name, arguments) = match step {
                    0 => (
                        "filesystem.read",
                        json!({"path":"fixture.txt","maxChars":16000}),
                    ),
                    1 => (
                        "tool.load",
                        json!({"names":[kynxa_agent_core_native::models::protocol::wire_name("filesystem.read"),"tool.result.read"]}),
                    ),
                    2 => (
                        "filesystem.read",
                        json!({"path":"second.txt","maxChars":100}),
                    ),
                    3 if read_reentry => (
                        "tool.load",
                        json!({"names":["filesystem.write","terminal.host.run","filesystem.edit","filesystem.read","filesystem.list"]}),
                    ),
                    4 | 5 => ("tool.load", json!({"names":["filesystem.write"]})),
                    6 => ("tool.load", json!({"names":["filesystem.read"]})),
                    7 => (
                        "filesystem.read",
                        json!({"path":"second.txt","maxChars":100}),
                    ),
                    8 => ("tool.load", json!({"names":["task.document"]})),
                    9 => (
                        "task.document",
                        json!({"action":"record","resultId":saved_document(&body,"second.txt")["resultId"],"conclusions":["SECOND_CURRENT_FILE is the current source body."],"unresolved":[]}),
                    ),
                    10 => ("task.document", json!({"action":"read","offset":0,"limit":4000})),
                    11 => (
                        "task.document",
                        json!({"action":"read","sourceIdentity":saved_document(&body,"second.txt")["sourceIdentity"],"offset":0,"limit":4000}),
                    ),
                    13 => (
                        "filesystem.write",
                        json!({"path":"pressure-result.txt","content":"SECOND_CURRENT_FILE","expectedHash":null}),
                    ),
                    14 => (
                        "filesystem.read",
                        json!({"path":"pressure-result.txt","maxChars":100}),
                    ),
                    _ => (
                        "tool.load",
                        json!({"names":["filesystem.write","filesystem.read","filesystem.list"]}),
                    ),
                };
                json!({"role":"assistant","content":"Continue using the loaded file reader after context preparation.","tool_calls":[{"id":format!("pressure-{step}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]})
            } else {
                json!({"role":"assistant","content":if read_reentry {
                    "The saved analysis was recovered after context preparation; pressure-result.txt contains SECOND_CURRENT_FILE and was read back.\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Current sources were read, their analysis restored, and the output written and read back\"}</kynxa_task>"
                } else {"The second current file was read after context preparation."}})
            }
        }
    } else if !archive_ids.is_empty() {
        let count = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "tool")
            .count();
        if count == 0 {
            json!({"role":"assistant","content":"Load saved-result paging.","tool_calls":[{"id":"archive-load","type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("tool.load"),"arguments":json!({"names":["tool.result.read"]}).to_string()}}]})
        } else if count == 1 {
            let calls = archive_ids.iter().enumerate().map(|(index,id)| json!({"id":format!("archive-page-{index}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("tool.result.read"),"arguments":json!({"id":id,"offset":0,"limit":6000}).to_string()}})).collect::<Vec<_>>();
            json!({"role":"assistant","content":"Read the public saved result and check archive boundaries.","tool_calls":calls})
        } else {
            json!({"role":"assistant","content":"Public result read; foreign and private archives rejected."})
        }
    } else if state.unloaded_cycle.load(Ordering::Acquire) {
        let count = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "tool")
            .count();
        if count < 3 {
            let (name, arguments) = if count == 1 {
                ("tool.load", json!({"names":["task.status"]}))
            } else {
                ("task.status", json!({}))
            };
            json!({"role":"assistant","content":"Load the declared capability and continue.","tool_calls":[{"id":format!("unloaded-{count}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]})
        } else {
            json!({"role":"assistant","content":"Tool loading recovered and the task status was read."})
        }
    } else if state.mcp {
        let count = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "tool")
            .count();
        if count < 2 {
            let name = if count == 0 {
                "tool.search"
            } else {
                "tool.load"
            };
            let arguments = if count == 0 {
                json!({"query":"offline-fixture"})
            } else {
                json!({"names":["mcp.offline-fixture"]})
            };
            json!({"role":"assistant","content":"检查隔离 MCP 的可用性。","tool_calls":[{"id":format!("mcp-fixture-{count}"),"type":"function","function":{
                "name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]})
        } else {
            json!({"role":"assistant","content":"离线 MCP 不可用，仍能继续完成回答。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"已报告隔离服务不可用\"}</kynxa_task>"})
        }
    } else if state.tool && !previous_result {
        json!({"role":"assistant","content":"先读取已授权文件。","tool_calls":[{"id":"read-fixture","type":"function","function":{
            "name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.read"),"arguments":"{\"path\":\"fixture.txt\"}"}}]})
    } else {
        json!({"role":"assistant","content":if state.tool {"已读取 native-file-body。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"已取得当前原文\"}</kynxa_task>"} else {"你好！"}})
    };
    let has_calls = message["tool_calls"].is_array();
    if (state.cancel_summary.load(Ordering::Acquire) && state.calls.load(Ordering::Acquire) == 1)
        || (control_cycle && matches!(control_step, Some(1 | 4)))
    {
        // An opaque native continuation forces the settled current-step checkpoint after the actual file read.
        // 大原生续传使真实文件读取后的当前步骤进入检查点，摘要仍只读取公开原件，不能依赖伪造压缩回执。
        message["reasoning_content"] = json!("PRIVATE_FIXTURE_CONTINUATION ".repeat(4096));
    }
    Json(json!({"id":"fixture-response","choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else if continuation_first {"length"}else{"stop"}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response()
}

struct Fixture {
    runtime: Arc<GatewayRuntime>,
    client: reqwest::Client,
    url: String,
    conversation: String,
    user_message_id: String,
    provider: Provider,
    servers: Vec<JoinHandle<()>>,
    root: PathBuf,
}
impl Fixture {
    async fn new(tool: bool, cancel: bool) -> Self {
        Self::with_mcp(tool, cancel, false).await
    }
    async fn with_mcp(tool: bool, cancel: bool, mcp: bool) -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-native-http-{}", new_id()));
        for directory in ["Models", "Data", "Extensions", "work"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(root.join("work/fixture.txt"), "native-file-body").unwrap();
        let provider = Provider {
            requests: Default::default(),
            calls: Default::default(),
            tool,
            cancel,
            mcp,
            malformed_calls: Default::default(),
            unloaded_cycle: Default::default(),
            archived_read_ids: Default::default(),
            context_pressure: Default::default(),
            pressure_step: Default::default(),
            pressure_read_reentry: Default::default(),
            steering_cycle: Default::default(),
            output_continuation: Default::default(),
            control_note_cycle: Default::default(),
            cancel_summary: Default::default(),
            summary_requests: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_url = format!("http://{}", listener.local_addr().unwrap());
        let service = Router::new()
            .route("/chat/completions", post(mock_provider))
            .with_state(provider.clone());
        let provider_server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let configuration = GatewayConfig {
            data_home: root.join("Models"),
            data_root: root.join("Data"),
            extension_root: root.join("Extensions"),
            application_root: root.join("Application"),
            port: 0,
            toolhost_exe: root.join("missing-toolhost.exe"),
            vector_extension: root.join("missing-vec0.dll"),
            inference_library: root.join("missing-onnxruntime.dll"),
        };
        let runtime = GatewayRuntime::new(configuration);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let service = gateway::router(runtime.clone());
        let gateway_server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let saved = client.post(format!("{url}/api/models")).json(&json!({"providerId":"native-fixture","displayName":"Isolated fixture",
            "baseUrl":provider_url,"apiKey":"","protocol":"openai-completions","models":["fixture-model"],"contextWindowTokens":32768,"maxOutputTokens":4096})).send().await.unwrap();
        assert!(
            saved.status().is_success(),
            "{}",
            saved.text().await.unwrap()
        );
        let catalog: Value = client
            .get(format!(
                "{url}/api/conversations/catalog?includeMessages=false"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let conversation = new_id();
        let user_message_id = new_id();
        let initial_message = if mcp {
            "检查隔离 MCP；不可用时也继续回答。"
        } else if tool {
            "读取文件 fixture.txt 并告诉我内容。"
        } else if cancel {
            "开始取消验证。"
        } else {
            "你好"
        };
        // A formal work chat is retained only once it has a user message; mimic the desktop send contract.
        // 正式工作聊天有用户消息才保留；夹具遵循桌面发送合同，不假设空聊天会被持久化。
        let saved = client.put(format!("{url}/api/conversations/catalog")).json(&json!({"Revision":catalog["Revision"],"Chats":[],"Projects":[{
            "Id":new_id(),"Name":"Isolated work","FolderPath":root.join("work"),"Chats":[{"Id":conversation,"Title":"Native HTTP fixture","Draft":"","Messages":[{"Id":user_message_id,"Role":"user","Content":initial_message,"CreatedAt":"2026-10-12T00:00:00Z"}]}]}]})).send().await.unwrap();
        assert!(
            saved.status().is_success(),
            "{}",
            saved.text().await.unwrap()
        );
        let saved: Value = saved.json().await.unwrap();
        assert_eq!(
            saved["Projects"][0]["Chats"][0]["Id"], conversation,
            "{saved}"
        );
        Self {
            runtime,
            client,
            url,
            conversation,
            user_message_id,
            provider,
            servers: vec![provider_server, gateway_server],
            root,
        }
    }
    fn request(&self, message: &str) -> Value {
        json!({"conversationId":self.conversation,"requestId":new_id(),"userMessageId":self.user_message_id,"provider":"native-fixture","model":"fixture-model","message":message,"permissionMode":"smart"})
    }
    async fn messages(&self) -> Vec<Value> {
        let response: Value = self
            .client
            .get(format!(
                "{}/api/conversations/{}/messages?limit=200",
                self.url, self.conversation
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        response["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("Unexpected message page: {response}"))
            .clone()
    }
    async fn close(self) {
        self.runtime.shutdown().await;
        for task in self.servers {
            task.abort();
            let _ = task.await;
        }
        // Native SQLite caches can retain test-only handles until process exit; preserve this isolated fixture for diagnostics.
        // 原生 SQLite 测试句柄可能保留到进程退出；仅保留独立临时夹具用于诊断，不触碰日常数据。
        assert!(self.root.starts_with(std::env::temp_dir()));
    }
}

fn event_values(buffer: &mut Vec<u8>) -> Vec<Value> {
    let mut events = Vec::new();
    // Decode only complete frames so a network packet may split a UTF-8 character.
    // 只解码完整事件帧，允许网络分包落在 UTF-8 中文字符内部。
    while let Some(end) = buffer.windows(2).position(|bytes| bytes == b"\n\n") {
        let frame = String::from_utf8(buffer[..end].to_vec()).unwrap();
        buffer.drain(..end + 2);
        for line in frame.lines().filter_map(|line| line.strip_prefix("data: ")) {
            if let Ok(value) = serde_json::from_str(line) {
                events.push(value);
            }
        }
    }
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_output_limit_preserves_marked_body_continuation_in_final_and_formal_log() {
    let fixture = Fixture::new(false, false).await;
    fixture
        .provider
        .output_continuation
        .store(true, Ordering::Release);
    let input = fixture.request("你好");
    let response: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    // The existing continuation contract also performs one completion review after the two text rounds.
    // 既有续写合同在两次正文生成后还执行一次完成核验；核验不新增正文段。
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 3);
    let segments = response["assistantSegments"].as_array().unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0]["phase"], "commentary");
    assert_eq!(segments[0]["isAnswerContinuation"], true);
    assert_eq!(segments[0]["content"], "第一段正文：保留这一段。");
    assert_eq!(segments[1]["phase"], "final_answer");
    assert!(
        segments[1]["content"]
            .as_str()
            .unwrap()
            .contains("第二段正文")
    );
    assert!(!response["content"].as_str().unwrap().contains("kynxa_task"));
    let messages = fixture.messages().await;
    let assistant = messages
        .iter()
        .find(|message| message["Role"] == "assistant")
        .unwrap();
    assert_eq!(
        assistant["AssistantSegments"][0]["isAnswerContinuation"],
        true
    );
    assert!(
        assistant["Content"]
            .as_str()
            .unwrap()
            .contains("第一段正文")
    );
    assert!(
        assistant["Content"]
            .as_str()
            .unwrap()
            .contains("第二段正文")
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_continuation_and_review_notes_do_not_change_compaction_user_goals() {
    let fixture = Fixture::new(true, false).await;
    fixture
        .provider
        .control_note_cycle
        .store(true, Ordering::Release);
    std::fs::write(fixture.root.join("work/second.txt"), "SECOND_CURRENT_FILE").unwrap();
    let config: Value = fixture
        .client
        .get(format!("{}/api/models", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut profile = config["providers"][0].clone();
    profile["contextWindowTokens"] = json!(16384);
    profile["maxOutputTokens"] = json!(4096);
    profile["apiKey"] = json!("");
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let original_goal = "读取文件 fixture.txt 并告诉我内容。";
    let input = fixture.request(original_goal);
    let response: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    assert!(
        response["content"]
            .as_str()
            .unwrap()
            .contains("Both authorized current sources were checked")
    );
    let requests = fixture.provider.requests.lock().await;
    let main_requests: Vec<&Value> = requests
        .iter()
        .filter(|request| {
            !system_text(request)
                .contains("Summarize public evidence for continuation of the same user task")
        })
        .collect();
    assert_eq!(main_requests.len(), 7);
    assert!(
        main_requests[1]
            .to_string()
            .contains("Continue the original authorized task")
    );
    assert!(
        main_requests[3]
            .to_string()
            .contains("Broker: compare the original requested work")
    );
    assert!(
        main_requests[4]
            .to_string()
            .contains("Broker: verification found remaining work")
    );
    let summaries: Vec<Value> = requests
        .iter()
        .filter(|request| {
            system_text(request)
                .contains("Summarize public evidence for continuation of the same user task")
        })
        .map(|request| {
            let message = request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "user")
                .unwrap();
            serde_json::from_str(message["content"].as_str().unwrap()).unwrap()
        })
        .collect();
    // Both opaque continuations force real current-step compaction after actual tool receipts.
    // 两次大原生续传均在真实工具回执之后触发压缩，程序提示只能作为导航，不能变成新用户目标。
    assert!(summaries.len() >= 2, "{requests:?}");
    for summary in &summaries {
        assert_eq!(summary["userGoals"], json!([original_goal]), "{summary}");
    }
    drop(requests);
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(
        &fixture.runtime.config.data_root,
    )
    .unwrap();
    let task = repository
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(task.usage.compactions >= 2);
    assert_eq!(
        repository
            .operations(&task.task_id)
            .unwrap()
            .iter()
            .filter(|operation| operation.intent.tool_name == "filesystem.read"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded)
            .count(),
        2,
        "Program guidance must not restart the source inventory or replay the original reads."
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_http_wraps_media_and_paginates_the_public_utf16_view() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture = Fixture::new(false, false).await;
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJesAAAAASUVORK5CYII=";
    let canonical = json!({"content":[{"type":"image","mimeType":"image/png","data":png}],
        "structuredContent":{"notice":"前😀后"},"isError":false});
    let reference = data::save_tool_result(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &new_id(),
        &json!({"id":"image-fixture","name":"computer.screenshot","arguments":{}}),
        &canonical,
        None,
    )
    .unwrap();
    let endpoint = format!(
        "{}/api/conversations/{}/tool-results/{}",
        fixture.url,
        fixture.conversation,
        reference["id"].as_str().unwrap()
    );
    let response = fixture.client.get(&endpoint).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let complete: Value = response.json().await.unwrap();
    assert!(complete["result"].is_object(), "{complete}");
    assert_eq!(complete["result"]["content"][0]["data"], png);
    assert_eq!(complete["result"]["structuredContent"]["notice"], "前😀后");
    let projected = data::public_tool_result(&canonical, Some(&reference), None, false)
        .unwrap()
        .to_string();
    let total = projected.encode_utf16().count();
    let mut offset = 0;
    let mut assembled = String::new();
    loop {
        let response = fixture
            .client
            .get(&endpoint)
            .query(&[("offset", offset.to_string()), ("limit", "47".into())])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let page: Value = response.json().await.unwrap();
        let text = page["text"].as_str().unwrap();
        assert_eq!(page["id"], reference["id"]);
        assert_eq!(page["resultRef"], reference);
        assert_eq!(page["offset"], offset);
        assert_eq!(page["totalCharacters"], total);
        assert_eq!(page["offsetUnit"], "utf16-code-units");
        assert!(!text.contains('\u{fffd}'));
        let next = page["nextOffset"].as_u64().unwrap() as usize;
        assert_eq!(next, offset + text.encode_utf16().count());
        assert_eq!(page["truncated"], next < total);
        assembled.push_str(text);
        if next == total {
            break;
        }
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(assembled, projected);
    assert!(
        !assembled.contains(png),
        "Paged model view must retain only media references"
    );
    let emoji = projected[..projected.find('😀').unwrap()]
        .encode_utf16()
        .count();
    let response = fixture
        .client
        .get(&endpoint)
        .query(&[("offset", (emoji + 1).to_string()), ("limit", "1".into())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let page: Value = response.json().await.unwrap();
    assert_eq!(page["offset"], emoji);
    assert_eq!(page["nextOffset"], emoji + 2);
    assert_eq!(page["text"], "😀");
    assert_eq!(
        data::load_private_tool_result(
            &fixture.root.join("Data"),
            &fixture.conversation,
            reference["id"].as_str().unwrap()
        )
        .unwrap()["canonical"],
        canonical
    );
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_http_rejects_invalid_pages_foreign_scope_and_private_archives() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture = Fixture::new(false, false).await;
    let root = fixture.root.join("Data");
    let reference = data::save_tool_result(
        &root,
        &fixture.conversation,
        &new_id(),
        &json!({"id":"text-fixture","name":"filesystem.read","arguments":{}}),
        &json!({"text":"SCOPED_RESULT_MUST_NOT_LEAK"}),
        None,
    )
    .unwrap();
    let endpoint = format!(
        "{}/api/conversations/{}/tool-results/{}",
        fixture.url,
        fixture.conversation,
        reference["id"].as_str().unwrap()
    );
    for query in [
        "offset=-1",
        "offset=+1",
        "offset=wrong",
        "offset=18446744073709551616",
        "offset=999999999",
        "limit=0",
        "limit=16001",
        "limit=-1",
        "limit=1.5",
        "limit=",
    ] {
        let response = fixture
            .client
            .get(format!("{endpoint}?{query}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{query}");
    }
    for query in ["offset=0", "limit=5"] {
        let response = fixture
            .client
            .get(format!("{endpoint}?{query}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{query}");
        assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    }
    let invalid = fixture
        .client
        .get(format!(
            "{}/api/conversations/{}/tool-results/not-a-reference",
            fixture.url, fixture.conversation
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);
    let other = new_id();
    data::ensure_conversation(&root, &other, "other isolated chat").unwrap();
    for suffix in ["", "?offset=0&limit=16"] {
        let response = fixture
            .client
            .get(format!(
                "{}/api/conversations/{other}/tool-results/{}{suffix}",
                fixture.url,
                reference["id"].as_str().unwrap()
            ))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("SCOPED_RESULT_MUST_NOT_LEAK")
        );
    }
    let private = data::save_tool_result(
        &root,
        &fixture.conversation,
        &new_id(),
        &json!({"id":"private-fixture","name":"model.continuation","arguments":{}}),
        &json!({"text":"PRIVATE_CONTINUATION_MUST_NOT_LEAK"}),
        None,
    )
    .unwrap();
    for suffix in ["", "?offset=0&limit=16"] {
        let response = fixture
            .client
            .get(format!(
                "{}/api/conversations/{}/tool-results/{}{suffix}",
                fixture.url,
                fixture.conversation,
                private["id"].as_str().unwrap()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("PRIVATE_CONTINUATION_MUST_NOT_LEAK")
        );
    }
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn greeting_reuses_request_and_persists_original_messages() {
    let fixture = Fixture::new(false, false).await;
    let health: Value = fixture
        .client
        .get(format!("{}/health", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["runtime"], "rust");
    assert_eq!(health["nodeRuntime"], false);
    let forbidden = fixture
        .client
        .get(format!("{}/health", fixture.url))
        .header("Origin", "https://untrusted.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
    let input = fixture.request("你好");
    for _ in 0..2 {
        let value: Value = fixture
            .client
            .post(format!("{}/api/chat", fixture.url))
            .json(&input)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(value["type"], "completed", "{value}");
        assert_eq!(value["content"], "你好！");
    }
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["Content"], "你好");
    assert_eq!(messages[1]["Content"], "你好！");
    let timing = &messages[1]["ToolRun"]["diagnostics"]["requestTiming"];
    assert!(timing["acceptedToModelMs"].is_u64());
    assert!(timing["modelLoopMs"].is_u64());
    assert!(
        timing["acceptedToFinalizationMs"].as_u64().unwrap()
            >= timing["acceptedToModelMs"].as_u64().unwrap()
    );
    assert_eq!(timing["includesDesktopCatalogSave"], false);
    assert_eq!(timing["includesFinalPersistence"], false);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_completed_chat_generates_a_source_bound_draft_without_blocking_the_reply() {
    let fixture = Fixture::new(false, false).await;
    let data = fixture.root.join("Data");
    let mut expected_users = Vec::new();
    for index in 0..11 {
        let user = new_id();
        expected_users.push(user.clone());
        kynxa_agent_core_native::storage::gateway::upsert_message(&data, &fixture.conversation, &json!({"Id":user,"Role":"user","Status":"completed","Content":format!("我喜欢第 {index} 种合成展示约定。")})).unwrap();
        kynxa_agent_core_native::storage::gateway::upsert_message(&data, &fixture.conversation, &json!({"Id":new_id(),"Role":"assistant","Status":"completed","Content":"已收到合成历史讨论。"})).unwrap();
    }
    let mut catalog: Value = fixture
        .client
        .get(format!(
            "{}/api/conversations/catalog?includeMessages=false",
            fixture.url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user = new_id();
    let text = "我喜欢最后一项合成约定。";
    expected_users.push(user.clone());
    catalog["Projects"][0]["Chats"][0]["Messages"] =
        json!([{"Id":user,"Role":"user","Content":text,"CreatedAt":"2026-10-13T00:00:00Z"}]);
    let saved = fixture
        .client
        .put(format!("{}/api/conversations/catalog", fixture.url))
        .json(&catalog)
        .send()
        .await
        .unwrap();
    assert!(
        saved.status().is_success(),
        "{}",
        saved.text().await.unwrap()
    );
    let mut request = fixture.request(text);
    request["userMessageId"] = json!(user);
    let completed: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["type"], "completed", "{completed}");
    assert_eq!(completed["content"], "你好！");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let memory = loop {
        let candidate_response = fixture
            .client
            .get(format!("{}/api/memory/candidates", fixture.url))
            .send()
            .await
            .unwrap();
        let candidate_status = candidate_response.status();
        let status: Value = candidate_response.json().await.unwrap();
        assert!(
            candidate_status.is_success(),
            "candidate status {candidate_status}: {status}"
        );
        let memory_response = fixture
            .client
            .get(format!(
                "{}/api/conversations/{}/memory",
                fixture.url, fixture.conversation
            ))
            .send()
            .await
            .unwrap();
        let memory_status = memory_response.status();
        let memory: Value = memory_response.json().await.unwrap();
        assert!(
            memory_status.is_success(),
            "memory status {memory_status}: {memory}"
        );
        let scopes = memory["scopes"]
            .as_array()
            .unwrap_or_else(|| panic!("memory response is missing scopes: {memory}"));
        if scopes.iter().any(|scope| {
            scope["scope"] == "chat" && !scope["entries"].as_array().unwrap().is_empty()
        }) {
            assert_eq!(status["settings"]["enabled"], true);
            break memory;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background candidate did not appear: {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let chat = memory["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["scope"] == "chat")
        .unwrap();
    assert_eq!(chat["entries"].as_array().unwrap().len(), 1);
    let entry = &chat["entries"][0];
    assert_eq!(entry["status"], "draft");
    assert_eq!(entry["source"]["conversationId"], fixture.conversation);
    assert_eq!(entry["source"]["messageId"], user);
    assert_eq!(entry["candidate"]["batchThroughMessageId"], user);
    assert_eq!(entry["candidate"]["quotes"].as_array().unwrap().len(), 12);
    assert!(entry["candidate"]["quotes"].as_array().unwrap().iter().all(|quote| expected_users.contains(&quote["messageId"].as_str().unwrap().to_string())));
    assert_eq!(entry["active"], false);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let requests = fixture.provider.requests.lock().await;
    assert!(!system_text(&requests[0]).contains("我喜欢第 0 种合成展示约定。"));
    assert!(!system_text(&requests[0]).contains(text));
    drop(requests);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_remember_survives_disabled_candidates_replay_and_a_later_memory_rejection() {
    let fixture = Fixture::new(false, false).await;
    let settings: Value = fixture
        .client
        .patch(format!("{}/api/memory/candidates/settings", fixture.url))
        .json(&json!({"expectedRevision":0,"patch":{"enabled":false}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["settings"]["enabled"], false);
    let user = new_id();
    let text = "全局记住：回答使用中文😀。";
    let saved = kynxa_agent_core_native::storage::gateway::upsert_message(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &json!({"Id":user,"Role":"user","Status":"completed","Content":text}),
    )
    .unwrap();
    assert_eq!(saved["memoryInstruction"]["created"], true);
    assert_eq!(saved["memoryInstruction"]["entry"]["status"], "confirmed");
    let mut request = fixture.request(text);
    request["userMessageId"] = json!(user);
    for _ in 0..2 {
        let completed: Value = fixture
            .client
            .post(format!("{}/api/chat", fixture.url))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(completed["type"], "completed", "{completed}");
        assert_eq!(completed["content"], "你好！");
    }
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let rejected_user = new_id();
    let oversized = format!("全局记住：{}", "长".repeat(4001));
    let rejected = kynxa_agent_core_native::storage::gateway::upsert_message(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &json!({"Id":rejected_user,"Role":"user","Status":"completed","Content":oversized}),
    )
    .unwrap();
    assert_eq!(rejected["memoryInstruction"]["reason"], "save-failed");
    assert_eq!(
        rejected["memoryInstruction"]["error"]["code"],
        "INVALID_MEMORY"
    );
    let mut ordinary_reply = fixture.request(&oversized);
    ordinary_reply["userMessageId"] = json!(rejected_user);
    let completed: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&ordinary_reply)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["type"], "completed", "{completed}");
    let memory: Value = fixture
        .client
        .get(format!("{}/api/memory/user", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(memory["entries"].as_array().unwrap().len(), 1);
    assert_eq!(memory["entries"][0]["content"], "回答使用中文😀。");
    assert_eq!(memory["entries"][0]["status"], "confirmed");
    assert_eq!(memory["entries"][0]["source"]["messageId"], user);
    assert_eq!(memory["candidateSettings"]["enabled"], false);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 2);
    let messages = fixture.messages().await;
    assert!(
        messages
            .iter()
            .any(|message| message["Id"] == rejected_user && message["Content"] == oversized)
    );
    let deleted = fixture
        .client
        .delete(format!(
            "{}/api/memory/user/{}",
            fixture.url,
            memory["entries"][0]["id"].as_str().unwrap()
        ))
        .json(&json!({"expectedRevision":memory["revision"]}))
        .send()
        .await
        .unwrap();
    assert!(
        deleted.status().is_success(),
        "{}",
        deleted.text().await.unwrap()
    );
    let follow_up_user = new_id();
    let follow_up_text = "验证撤销后的普通聊天回复。";
    kynxa_agent_core_native::storage::gateway::upsert_message(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &json!({"Id":follow_up_user,"Role":"user","Status":"completed","Content":follow_up_text}),
    )
    .unwrap();
    let mut follow_up = fixture.request(follow_up_text);
    follow_up["userMessageId"] = json!(follow_up_user);
    let completed: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&follow_up)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["type"], "completed", "{completed}");
    let requests = fixture.provider.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(system_text(&requests[0]).contains("回答使用中文😀。"));
    assert!(system_text(&requests[1]).contains("回答使用中文😀。"));
    assert!(!system_text(&requests[2]).contains("回答使用中文😀。"));
    drop(requests);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_calls_exhaust_repair_but_finish_a_truthful_reply_without_dispatch() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.malformed_calls.store(3, Ordering::SeqCst);
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&fixture.request("你好"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    let events = event_values(&mut text.as_bytes().to_vec());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))),
        "{text}"
    );
    let finished = events
        .iter()
        .find(|event| event["type"] == "completed")
        .unwrap();
    assert!(!finished["content"].as_str().unwrap().is_empty());
    assert_eq!(finished["taskCompletion"]["completionState"], "partial");
    assert_eq!(finished["taskCompletion"]["correctnessCertified"], false);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 4);
    assert!(!events.iter().any(|event| event["type"] == "tool_call"));
    let requests = fixture.provider.requests.lock().await.clone();
    assert!(requests[3]["tools"].is_null() || requests[3]["tools"].as_array().unwrap().is_empty());
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["Status"], "completed");
    assert!(messages[1]["Error"].as_str().is_none_or(str::is_empty));
    assert_eq!(messages[1]["ToolRun"]["diagnostics"]["modelCalls"], 4);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_startup_preparation_never_generates_or_blocks_chat() {
    let fixture = Fixture::new(false, false).await;
    let response = fixture
        .client
        .post(format!("{}/api/models/preparation", fixture.url))
        .json(&json!({"providerId":"native-fixture","modelId":"fixture-model"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let accepted: Value = response.json().await.unwrap();
    assert_eq!(accepted["scheduled"], true);
    let (_owner, cancellation) = tokio::sync::watch::channel(false);
    assert!(
        fixture
            .runtime
            .model_preparation
            .wait_idle(cancellation, Duration::from_secs(5))
            .await
    );
    let status: Value = fixture
        .client
        .get(format!("{}/api/models/preparation", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["isPreparing"], false);
    assert_eq!(status["models"][0]["inferenceVerified"], false);
    assert_ne!(status["models"][0]["modelReady"], true);
    assert!(!status.to_string().contains("apiKey"));
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
    let reply: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&fixture.request("你好"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["content"], "你好！");
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.messages().await[1]["Status"], "completed");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_cycle_streams_real_read_receipt_into_provider_and_formal_log() {
    let fixture = Fixture::new(true, false).await;
    let mut input = fixture.request("读取文件 fixture.txt 并告诉我内容。");
    input["runLimits"]["maxToolCalls"] = json!(1);
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    if let Some(path) = std::env::var_os("KYNXA_GATEWAY_SSE_FIXTURE") {
        // The desktop contract check consumes the actual emitted stream, not a handwritten approximation.
        // 桌面合同检查读取实际发出的消息流，不用手写样例代替真实后端输出。
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    let mut buffer = text.as_bytes().to_vec();
    let events = event_values(&mut buffer);
    // The provider fallback emits a raw content snapshot; no public SSE event may expose its footer.
    // 供应商非流式回退发出原始正文快照；公开 SSE 事件不能泄露其内部任务尾部。
    assert!(!text.contains("<kynxa_task>"), "{text}");
    assert!(!text.contains("</kynxa_task>"), "{text}");
    assert!(events.iter().any(|event| {
        event["type"] == "text_delta"
            && event["delta"]
                .as_str()
                .is_some_and(|delta| delta.contains("native-file-body"))
    }));
    assert!(
        events.iter().any(|event| event["type"] == "tool_result"
            && event["tool"]["name"] == "filesystem.read"
            && event["tool"]["status"] == "completed"),
        "{text}"
    );
    assert!(
        events.iter().any(|event| event["type"] == "completed"
            && event["content"]
                .as_str()
                .is_some_and(|text| text.contains("native-file-body"))),
        "{text}"
    );
    let requests = fixture.provider.requests.lock().await.clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["tools"].as_array().is_none_or(Vec::is_empty),
        "A used tool allowance must retain a normal final answer without further declarations.");
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message.to_string().contains("native-file-body"))
    );
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["ToolActivities"][0]["name"], "filesystem.read");
    assert!(
        messages[1]["ToolActivities"][0]["resultRef"]["id"].is_string(),
        "{}",
        messages[1]
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloaded_tool_recovers_without_settling_an_undispatched_operation() {
    let fixture = Fixture::new(false, false).await;
    fixture
        .provider
        .unloaded_cycle
        .store(true, Ordering::Release);
    let request = fixture.request("你好");
    let request_id = request["requestId"].as_str().unwrap().to_owned();
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    let events = event_values(&mut text.as_bytes().to_vec());
    assert!(
        events.iter().any(|event| event["type"] == "tool_result"
            && event["tool"]["status"] == "error"
            && event.to_string().contains("TOOL_NOT_LOADED")),
        "{text}"
    );
    assert!(
        events.iter().any(|event| event["type"] == "completed"
            && event["content"]
                .as_str()
                .is_some_and(|content| content.contains("Tool loading recovered"))),
        "{text}"
    );
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(
        &fixture.runtime.config.data_root,
    )
    .unwrap();
    let task = repository
        .find_by_request_id(&fixture.conversation, &request_id)
        .unwrap()
        .unwrap();
    let operations = repository.operations(&task.task_id).unwrap();
    assert_eq!(operations.len(), 2);
    assert!(operations.iter().all(|operation| operation.state
        == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded));
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 4);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_mcp_load_diagnostic_keeps_streaming_and_persists_final_reply() {
    let mut fixture = Fixture::with_mcp(false, false, true).await;
    // An owned loopback server rejects its handshake; no external MCP, process or credentials participate.
    // 自有本机服务拒绝握手，不连接外部 MCP、不启动脚本进程，也不读取真实凭据。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mcp_url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let service = Router::new().route(
        "/mcp",
        post(|| async {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"isolated unavailable MCP"})),
            )
        }),
    );
    fixture.servers.push(tokio::spawn(async move {
        axum::serve(listener, service).await.unwrap();
    }));
    let response = fixture
        .client
        .get(format!("{}/api/agent/config", fixture.url))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let mut configuration: Value = response.json().await.unwrap();
    configuration["expectedRevision"] = configuration["revision"].clone();
    configuration["mcpServers"] = json!([{"id":"offline-fixture","name":"Offline fixture","enabled":true,
        "transport":"streamable-http","url":mcp_url,"startupTimeoutMs":1000,"disabledTools":[]}]);
    let saved = fixture
        .client
        .put(format!("{}/api/agent/config", fixture.url))
        .json(&configuration)
        .send()
        .await
        .unwrap();
    assert!(
        saved.status().is_success(),
        "{}",
        saved.text().await.unwrap()
    );
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&fixture.request("检查隔离 MCP；不可用时也继续回答。"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    if let Some(path) = std::env::var_os("KYNXA_GATEWAY_MCP_SSE_FIXTURE") {
        let path = PathBuf::from(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    let events = event_values(&mut text.as_bytes().to_vec());
    let status = events
        .iter()
        .position(|event| event["type"] == "mcp_status")
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(events[status]["serverId"], "offline-fixture");
    assert_eq!(events[status]["ready"], false);
    assert!(
        events[status]["code"]
            .as_str()
            .is_some_and(|code| !code.is_empty())
    );
    assert!(
        events[status]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(events[status]["conversationId"], fixture.conversation);
    assert!(
        events[status + 1..]
            .iter()
            .any(|event| event["type"] == "text_delta"
                && event["delta"]
                    .as_str()
                    .is_some_and(|text| text.contains("仍能继续"))),
        "{text}"
    );
    assert!(
        events.iter().any(|event| event["type"] == "completed"
            && event["content"]
                .as_str()
                .is_some_and(|text| text.contains("仍能继续"))),
        "{text}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "tool_call")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "tool_result")
            .count(),
        2
    );
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 3);
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert!(
        messages[1]["Content"]
            .as_str()
            .is_some_and(|text| text.contains("仍能继续"))
    );
    assert_eq!(messages[1]["ToolActivities"].as_array().unwrap().len(), 2);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatched_summary_cancellation_keeps_formal_diagnostics_equal_to_task_usage() {
    use kynxa_agent_core_native::storage::task_repository::TaskRepository;
    let fixture = Fixture::new(true, false).await;
    fixture
        .provider
        .context_pressure
        .store(true, Ordering::Release);
    fixture
        .provider
        .cancel_summary
        .store(true, Ordering::Release);
    std::fs::write(
        fixture.root.join("work/fixture.txt"),
        "Public source body with operative value 431. ".repeat(512),
    )
    .unwrap();
    let config: Value = fixture
        .client
        .get(format!("{}/api/models", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut profile = config["providers"][0].clone();
    profile["contextWindowTokens"] = json!(16384);
    profile["maxOutputTokens"] = json!(4096);
    profile["apiKey"] = json!("");
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let input = fixture.request("读取文件 fixture.txt 并告诉我内容。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture.provider.summary_requests.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("The actual current-step summary request must reach the local HTTP provider.");
    let cancelled: Value = fixture
        .client
        .post(format!("{}/api/chat/cancel", fixture.url))
        .json(&json!({"conversationId":fixture.conversation,"requestId":input["requestId"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    let mut buffer = Vec::new();
    let mut terminal = Value::Null;
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .unwrap()
    {
        buffer.extend_from_slice(&bytes.unwrap());
        for event in event_values(&mut buffer) {
            if ["interrupted", "error"].contains(&event["type"].as_str().unwrap_or("")) {
                terminal = event;
            }
        }
    }
    assert_eq!(terminal["type"], "interrupted", "{terminal}");
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let task = repository
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let calls = fixture.provider.calls.load(Ordering::Acquire) as u64;
    assert_eq!(
        fixture.provider.summary_requests.load(Ordering::Acquire),
        1,
        "The cancelled summary must not be retried."
    );
    assert_eq!(
        calls, 2,
        "Only the source-reading step and the cancelled summary may be dispatched."
    );
    assert_eq!(task.usage.model_requests, calls);
    assert_eq!(task.usage.failed_requests, 1);
    assert_eq!(task.usage.unknown_token_requests, 1);
    let messages = fixture.messages().await;
    let assistant = messages
        .iter()
        .find(|message| message["Role"] == "assistant" && message["Id"] == input["requestId"])
        .unwrap();
    assert_eq!(
        assistant["ToolRun"]["diagnostics"]["modelCalls"], calls,
        "The cancelled, dispatched summary belongs in the persisted message diagnostics: {assistant}"
    );
    assert_eq!(
        assistant["ToolRun"]["diagnostics"]["totalModelMs"],
        task.usage.model_duration_ms
    );
    assert_eq!(
        repository.operations(&task.task_id).unwrap().len(),
        1,
        "Cancellation must not replay the source read or dispatch any additional tool."
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_keeps_partial_stream_and_does_not_repeat_provider_request() {
    let fixture = Fixture::new(false, true).await;
    let input = fixture.request("开始取消验证。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut seen_text = false;
    while !seen_text {
        let bytes = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buffer.extend_from_slice(&bytes);
        seen_text = event_values(&mut buffer)
            .iter()
            .any(|event| event["type"] == "text_delta");
    }
    let cancelled: Value = fixture
        .client
        .post(format!("{}/api/chat/cancel", fixture.url))
        .json(&json!({"conversationId":fixture.conversation,"requestId":input["requestId"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    let mut finished = false;
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .unwrap()
    {
        buffer.extend_from_slice(&bytes.unwrap());
        for event in event_values(&mut buffer) {
            if ["interrupted", "error"].contains(&event["type"].as_str().unwrap_or("")) {
                finished = true;
            }
        }
    }
    assert!(finished);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let messages = fixture.messages().await;
    assert!(
        messages[1]["Content"]
            .as_str()
            .unwrap_or("")
            .contains("保留这段正文"),
        "{}",
        messages[1]
    );
    assert_eq!(messages[1]["Status"], "interrupted");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_disconnect_keeps_task_alive_until_explicit_cancel() {
    use kynxa_agent_core_native::storage::task_repository::{TaskRepository, TaskState};
    let fixture = Fixture::new(false, true).await;
    let input = fixture.request("开始取消验证。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut started = Value::Null;
    while started.is_null() {
        let bytes = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buffer.extend_from_slice(&bytes);
        for event in event_values(&mut buffer) {
            if event["type"] == "started" {
                started = event;
            }
        }
    }
    assert!(started["taskId"].is_string(), "{started}");
    drop(stream);
    let run = fixture.runtime.begin(input.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture.provider.calls.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !*run.cancellation.borrow(),
        "SSE disconnect must not cancel durable work"
    );
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let accepted = repository
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(accepted.task_id, started["taskId"]);
    fixture
        .runtime
        .cancel(&fixture.conversation, input["requestId"].as_str().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !run.finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let cancelled = repository.load(&accepted.task_id).unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(cancelled.usage.model_requests, 1);
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 1);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_http_reads_real_scope_events_and_reuses_completed_request_after_restart() {
    let fixture = Fixture::new(false, false).await;
    let input = fixture.request("你好");
    let reply: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["type"], "completed", "{reply}");
    assert!(reply["persistenceError"].is_null(), "{reply}");
    let list: Value = fixture
        .client
        .get(format!(
            "{}/api/tasks?conversationId={}",
            fixture.url, fixture.conversation
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task = &list["tasks"][0];
    assert_eq!(task["state"], "completed", "{task}");
    assert_eq!(task["usage"]["modelRequests"], 1);
    let events: Value = fixture
        .client
        .get(format!(
            "{}/api/tasks/{}/events?conversationId={}&afterSequence=0",
            fixture.url,
            task["taskId"].as_str().unwrap(),
            fixture.conversation
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(events["throughSequence"].as_u64().unwrap() > 0);
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "task.final")
    );
    let replacement = GatewayRuntime::new(fixture.runtime.config.clone());
    let run = replacement.begin(input.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !run.finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(run.result.lock().await.as_ref().unwrap()["reused"], true);
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 1);
    let mut changed = input;
    changed["message"] = json!("different request");
    assert_eq!(
        replacement.begin(changed).await.err().unwrap().code,
        "CHAT_REQUEST_CONFLICT"
    );
    replacement.shutdown().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_pause_preserves_budget_and_late_receipt_then_cancel_is_explicit() {
    use kynxa_agent_core_native::storage::task_repository::{
        TaskControl, TaskRepository, TaskState,
    };
    let fixture = Fixture::new(false, true).await;
    let input = fixture.request("开始取消验证。");
    let run = fixture.runtime.begin(input).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture.provider.calls.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let journal = run.journal.as_ref().unwrap();
    let task = journal.load_current().unwrap();
    fixture
        .runtime
        .tasks
        .control(&task.task_id, task.version, TaskControl::Pause)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !run.finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let paused = repository.load(&task.task_id).unwrap();
    assert_eq!(paused.state, TaskState::Paused);
    assert_eq!(paused.usage.model_requests, 1);
    assert_eq!(paused.usage.failed_requests, 1);
    assert!(
        repository
            .latest_checkpoint(&task.task_id)
            .unwrap()
            .is_some()
    );
    fixture
        .runtime
        .tasks
        .control(&task.task_id, paused.version, TaskControl::Cancel)
        .await
        .unwrap();
    assert_eq!(
        repository.load(&task.task_id).unwrap().state,
        TaskState::Cancelled
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_steering_pauses_saves_formal_input_and_observes_the_same_resumed_task() {
    use kynxa_agent_core_native::storage::task_repository::{TaskRepository, TaskState};
    let fixture = Fixture::new(false, true).await;
    fixture
        .provider
        .steering_cycle
        .store(true, Ordering::Release);
    let input = fixture.request("开始取消验证。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut task_id = String::new();
    let mut saw_text = false;
    while !saw_text || task_id.is_empty() {
        let bytes = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buffer.extend_from_slice(&bytes);
        for event in event_values(&mut buffer) {
            if event["type"] == "started" {
                task_id = event["taskId"].as_str().unwrap().to_owned();
            }
            saw_text |= event["type"] == "text_delta";
        }
    }
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let before = repository.load(&task_id).unwrap();
    let message_id = new_id();
    let amendment = "更新方案：保留原范围，只解释结果，不再继续写入。";
    let steering_input = json!({"conversationId":fixture.conversation,"expectedVersion":before.version,"userMessageId":message_id,"message":amendment,"action":"update"});
    let response = fixture
        .client
        .post(format!("{}/api/tasks/{task_id}/steer", fixture.url))
        .json(&steering_input)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let accepted: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted["reconnectRequired"], true);
    assert_eq!(accepted["task"]["taskId"], task_id);
    assert_eq!(accepted["affectedTaskIds"][0], task_id);
    let recovered = fixture
        .client
        .get(format!(
            "{}/api/tasks/{task_id}/stream?conversationId={}",
            fixture.url, fixture.conversation
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(recovered.status(), 200);
    let body = tokio::time::timeout(Duration::from_secs(15), recovered.text())
        .await
        .unwrap()
        .unwrap();
    let mut body = body.into_bytes();
    let events = event_values(&mut body);
    assert_eq!(events.last().unwrap()["type"], "completed");
    assert_eq!(events.last().unwrap()["requestId"], input["requestId"]);
    assert_eq!(
        fixture.provider.calls.load(Ordering::Acquire),
        2,
        "observation must not dispatch another model request"
    );
    let actual = fixture.provider.requests.lock().await;
    assert!(
        actual[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "user" && message["content"] == amendment)
    );
    drop(actual);
    assert_eq!(
        repository.load(&task_id).unwrap().state,
        TaskState::Completed
    );
    assert_eq!(repository.user_steering(&task_id).unwrap().len(), 1);
    let messages = fixture.messages().await;
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["Id"] == message_id && message["Content"] == amendment)
            .count(),
        1
    );
    let replay: Value = fixture
        .client
        .post(format!("{}/api/tasks/{task_id}/steer", fixture.url))
        .json(&steering_input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["reused"], true);
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 2);
    drop(stream);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_tool_result_read_pages_the_formal_archive_without_foreign_or_private_data() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture = Fixture::new(false, false).await;
    let root = fixture.root.join("Data");
    let other = new_id();
    data::ensure_conversation(&root, &other, "foreign fixture").unwrap();
    let mut ids = Vec::new();
    for (chat, name, marker) in [
        (
            &fixture.conversation,
            "filesystem.read",
            "PUBLIC_ARCHIVE_中文😀",
        ),
        (&other, "filesystem.read", "FOREIGN_ARCHIVE_MUST_NOT_LEAK"),
        (
            &fixture.conversation,
            "model.continuation",
            "PRIVATE_ARCHIVE_MUST_NOT_LEAK",
        ),
    ] {
        let reference = data::save_tool_result(&root, chat, &new_id(),
            &json!({"id":new_id(),"name":name,"arguments":{}}),
            &json!({"text":marker,"content":[{"type":"image","mimeType":"image/png","data":"MEDIA_BASE64_MUST_NOT_LEAK"}]}), None).unwrap();
        ids.push(reference["id"].as_str().unwrap().to_owned());
    }
    *fixture.provider.archived_read_ids.lock().await = ids;
    let response = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&fixture.request("你好"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["type"], "completed", "{result}");
    let requests = fixture.provider.requests.lock().await;
    let last = requests.last().unwrap();
    let receipts: Vec<Value> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| serde_json::from_str(message["content"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(receipts.len(), 4);
    assert_ne!(receipts[1]["isError"], true);
    assert!(receipts[1].to_string().contains("PUBLIC_ARCHIVE_中文😀"));
    assert_eq!(
        receipts[2]["code"], "TOOL_RESULT_NOT_FOUND",
        "{}",
        receipts[2]
    );
    assert_eq!(
        receipts[3]["code"], "PRIVATE_MODEL_ATTACHMENT",
        "{}",
        receipts[3]
    );
    let model_view = last.to_string();
    for marker in [
        "FOREIGN_ARCHIVE_MUST_NOT_LEAK",
        "PRIVATE_ARCHIVE_MUST_NOT_LEAK",
        "MEDIA_BASE64_MUST_NOT_LEAK",
    ] {
        assert!(!model_view.contains(marker), "{marker}");
    }
    drop(requests);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_pressure_keeps_loaded_tools_callable_and_normalizes_exact_wire_aliases() {
    let mut fixture = Fixture::new(true, false).await;
    // Observe the extended read-to-write flow; this changes only the test client's wait, not task grants.
    // 完整观察读后切换写入的流程；仅调整测试客户端等待，不放宽产品任务或上下文额度。
    fixture.client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    fixture
        .provider
        .context_pressure
        .store(true, Ordering::Release);
    fixture
        .provider
        .pressure_read_reentry
        .store(true, Ordering::Release);
    std::fs::write(
        fixture.root.join("work/fixture.txt"),
        "当前证据与版本规则。".repeat(2000),
    )
    .unwrap();
    std::fs::write(fixture.root.join("work/second.txt"), "SECOND_CURRENT_FILE").unwrap();
    let connection = fixture.provider.requests.lock().await;
    assert!(connection.is_empty());
    drop(connection);
    let config: Value = fixture
        .client
        .get(format!("{}/api/models", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut profile = config["providers"][0].clone();
    profile["contextWindowTokens"] = json!(16384);
    profile["maxOutputTokens"] = json!(4096);
    profile["apiKey"] = json!("");
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let mut input = fixture.request(
        "读取 fixture.txt 和 second.txt，记录分析，写 pressure-result.txt 并读回，最后回复交付结果。",
    );
    input["userMessageId"] = json!(new_id());
    input["permissionMode"] = json!("full");
    let result: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["type"], "completed", "{result}");
    let tasks = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(
        &fixture.root.join("Data"),
    )
    .unwrap();
    let task = tasks
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let operations = tasks.operations(&task.task_id).unwrap();
    assert!(task.usage.compactions > 0, "{result}");
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.intent.tool_name == "filesystem.read"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded)
            .count(),
        4,
        "{operations:?}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("work/pressure-result.txt")).unwrap(),
        "SECOND_CURRENT_FILE"
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.intent.tool_name == "task.document"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded)
            .count(),
        2,
        "The recorded analysis must be read back through its real document identity."
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.intent.tool_name == "task.document"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Failed)
            .count(),
        1,
        "Missing document identity must remain an error; it cannot select another document."
    );
    let final_body = result["content"].as_str().expect("A completed reply must have a final public body.");
    assert!(final_body.contains("pressure-result.txt"), "{result}");
    assert!(final_body.contains("SECOND_CURRENT_FILE"), "{result}");
    assert!(!final_body.contains("<kynxa_task>"), "{result}");
    let requests = fixture.provider.requests.lock().await;
    let reentry_request = requests
        .iter()
        .filter(|request| {
            request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        })
        .nth(7)
        .expect("The model must receive the post-load read reentry request.");
    assert!(
        reentry_request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| {
                tool["function"]["name"]
                    == kynxa_agent_core_native::models::protocol::wire_name("filesystem.read")
            }),
        "Consolidation displaced the reader that was explicitly loaded for the next gap."
    );
    let last_request = requests.last().unwrap();
    let last = last_request.to_string();
    assert!(last.contains("SECOND_CURRENT_FILE"));
    assert!(!last.contains("TOOL_NOT_LOADED"));
    assert!(
        last_request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"]
                == kynxa_agent_core_native::models::protocol::wire_name("filesystem.write")),
        "The requested writer was displaced after compaction: {last_request}"
    );
    assert!(
        last.contains("Tools callable in this request:"),
        "The live capability guidance was lost during compaction: {last_request}"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.to_string().contains("Broker delivery priority:")),
        "The read reentry was not exercised after stalled work entered consolidation."
    );
    drop(requests);
    fixture.close().await;
}
