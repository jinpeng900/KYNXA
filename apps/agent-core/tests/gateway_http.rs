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
}
async fn mock_provider(State(state): State<Provider>, Json(body): Json<Value>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.requests.lock().await.push(body.clone());
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
    if state.cancel {
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
    let message = if state.mcp {
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
    Json(json!({"id":"fixture-response","choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response()
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
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&fixture.request("读取文件 fixture.txt 并告诉我内容。"))
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
