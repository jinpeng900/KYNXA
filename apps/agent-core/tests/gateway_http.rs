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
    transient_http_failures: Arc<AtomicUsize>,
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
    skill_pagination_cycle: Arc<std::sync::atomic::AtomicBool>,
    memory_approval_cycle: Arc<std::sync::atomic::AtomicBool>,
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

fn is_completion_review_request(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|message| {
            message["content"].as_str().is_some_and(|content| {
                content.starts_with("Broker: compare the original requested work")
            })
        })
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
    if state.transient_http_failures.fetch_update(Ordering::SeqCst, Ordering::SeqCst,
        |remaining| remaining.checked_sub(1)).is_ok() {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":{"message":"fixture temporary provider failure"}}))).into_response();
    }
    if state.skill_pagination_cycle.load(Ordering::Acquire) {
        // Follow the actual failure receipt before correcting the page; no synthetic tool success.
        // 先读取真实失败回执再纠正分页，不伪造工具成功结果。
        let has_receipt = |id: &str| body["messages"].as_array().into_iter().flatten()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == id);
        let message = if is_completion_review_request(&body) || has_receipt("skill-page-corrected") {
            json!({"role":"assistant","content":"已按声明纠正分页并完成技能列表读取，原有上下文保留。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"已核对真实分页回执\"}</kynxa_task>"})
        } else {
            let correcting = has_receipt("skill-page-invalid");
            let arguments = if correcting { json!({"offset":0,"limit":1}) }
                else { json!({"offset":-1,"limit":1}) };
            json!({"role":"assistant","content":"继续当前技能读取任务。","tool_calls":[{
                "id":if correcting {"skill-page-corrected"}else{"skill-page-invalid"},"type":"function",
                "function":{"name":kynxa_agent_core_native::models::protocol::wire_name("skill.list"),"arguments":arguments.to_string()}}]})
        };
        let has_calls = message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else{"stop"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.memory_approval_cycle.load(Ordering::Acquire) {
        let replanning = body["messages"].as_array().into_iter().flatten()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "withdrawn-memory-delete");
        let message = if replanning || is_completion_review_request(&body) {
            json!({"role":"assistant","content":"已保留先前保存的文件，并根据当前记忆重新规划，未执行过期删除。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"保存结果保留，过期计划未派发\"}</kynxa_task>"})
        } else {
            // One completed effect precedes approval; a second stale call must not run after revocation either.
            // 审批前已有一个成功副作用；撤回记忆后，同批余下旧调用也不得执行。
            let calls = [
                ("saved-before-approval", "filesystem.write", json!({"path":"saved-before-approval.txt","content":"successful-output-kept","expectedHash":null})),
                ("withdrawn-memory-delete", "filesystem.delete", json!({"path":"fixture.txt",
                    "expectedHash":kynxa_agent_core_native::retrieval::gateway_index::hash("native-file-body")})),
                ("stale-batch-tail", "filesystem.write", json!({"path":"stale-batch-tail.txt","content":"must-not-be-written","expectedHash":null})),
            ].into_iter().map(|(id, name, arguments)| json!({"id":id,"type":"function",
                "function":{"name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}})).collect::<Vec<_>>();
            json!({"role":"assistant","content":"先保留成功输出，再执行工作目录维护。","tool_calls":calls})
        };
        let has_calls = message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else{"stop"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
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
    if state.steering_cycle.load(Ordering::Acquire) && is_completion_review_request(&body) {
        // Steered replies receive the real independent completion review, rather than another chat answer.
        // 被引导的回复仍经过真实独立完成审核；夹具按审核协议返回标记，不重复普通聊天正文。
        return Json(json!({"id":"steering-review-fixture","choices":[{"index":0,"message":{"role":"assistant","content":"<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"The final guided answer is complete\"}</kynxa_task>"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.cancel
        && (!state.steering_cycle.load(Ordering::Acquire)
            || state.calls.load(Ordering::SeqCst) <= 2)
    {
        let mut delta = json!({"content":"已开始，保留这段正文。"});
        if state.steering_cycle.load(Ordering::Acquire) {
            delta["tool_calls"] = json!([{"index":0,"id":"obsolete-partial-write","type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.write"),"arguments":"{\"path\":\"obsolete.txt\",\"content\":\""}}]);
        }
        let chunk = json!({"id":"cancel-fixture","choices":[{"index":0,"delta":delta,"finish_reason":null}]}).to_string();
        if state.steering_cycle.load(Ordering::Acquire) {
            let completed = json!({"id":"cancel-fixture","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}}).to_string();
            let events = futures_util::stream::unfold(
                (Some(format!("data: {chunk}\n\n")), Some(format!("data: {completed}\n\ndata: [DONE]\n\n"))),
                |(first, last)| async move {
                    if let Some(first) = first {
                        return Some((Ok::<_, std::convert::Infallible>(first), (None, last)));
                    }
                    if let Some(last) = last {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return Some((Ok::<_, std::convert::Infallible>(last), (None, None)));
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
        if count < 2 {
            json!({"role":"assistant","content":"Check the authorized file metadata and continue.","tool_calls":[{"id":format!("unloaded-{count}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.stat"),"arguments":json!({"path":"fixture.txt"}).to_string()}}]})
        } else {
            json!({"role":"assistant","content":"Tool loading recovered and the authorized file metadata was read."})
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
            transient_http_failures: Default::default(),
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
            skill_pagination_cycle: Default::default(),
            memory_approval_cycle: Default::default(),
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
async fn message_attachments_reach_model_and_keep_original_user_message() {
    use kynxa_agent_core_native::storage::gateway as data;
    use base64::Engine;
    let fixture = Fixture::new(false, false).await;
    let doc = std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/documents/account-guide.docx")).unwrap();
    let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJesAAAAASUVORK5CYII=").unwrap();
    let message = new_id();
    let document_id = new_id();
    let image_id = new_id();
    let files = [(document_id.clone(), "作业.docx", doc), (image_id.clone(), "图.png", png)];
    let clipboard_directory = fixture.root.join("Temp").join("AttachmentDrafts");
    std::fs::create_dir_all(&clipboard_directory).unwrap();
    let source_paths = [fixture.root.join("work").join(files[0].1), clipboard_directory.join(files[1].1)];
    for ((_, _, bytes), path) in files.iter().zip(&source_paths) { std::fs::write(path, bytes).unwrap(); }
    data::upsert_message(&fixture.runtime.config.data_root, &fixture.conversation,
        &json!({"Id":message,"Role":"user","Content":"","Attachments":files.iter().map(|(id,name,bytes)|json!({"Id":id,"Name":name,"SizeBytes":bytes.len()})).collect::<Vec<_>>()})).unwrap();
    // Clipboard sources must be external temporary files; application-private sources remain forbidden.
    // 剪贴板来源必须使用外部临时文件，应用私有数据来源仍然禁止附加。
    let private_directory = fixture.runtime.config.data_root.join("Desktop").join("Temp").join("AttachmentDrafts");
    std::fs::create_dir_all(&private_directory).unwrap();
    let private_image = private_directory.join(files[1].1);
    std::fs::write(&private_image, &files[1].2).unwrap();
    let denied = fixture.client.post(format!("{}/api/conversations/{}/attachments/import", fixture.url, fixture.conversation))
        .json(&json!({"messageId":message,"files":[{"id":image_id,"name":files[1].1,"path":private_image}]}))
        .send().await.unwrap();
    assert_eq!(denied.status(), 403);
    assert_eq!(denied.json::<Value>().await.unwrap()["code"], "PROTECTED_APP_DATA");
    let response = fixture.client.post(format!("{}/api/conversations/{}/attachments/import", fixture.url, fixture.conversation))
        .json(&json!({"messageId":message,"files":files.iter().zip(&source_paths).map(|((id,name,_),path)|json!({"id":id,"name":name,"path":path})).collect::<Vec<_>>()}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let mut input = fixture.request("");
    input["userMessageId"] = json!(message);
    input["attachmentIds"] = json!([document_id, image_id]);
    let response: Value = fixture.client.post(format!("{}/api/chat", fixture.url)).json(&input).send().await.unwrap().json().await.unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    let requests = fixture.provider.requests.lock().await;
    let wire = requests.first().unwrap();
    assert!(wire.to_string().contains("How to reset an account password"), "{wire}");
    assert!(!wire.to_string().contains("attachmentMessageId"));
    assert!(!wire.to_string().contains("deferredAttachmentImages"));
    assert!(wire["messages"].as_array().unwrap().iter().any(|message| message["content"].as_array()
        .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))));
    drop(requests);
    let stored = data::read_messages(&fixture.runtime.config.data_root, &fixture.conversation, true).unwrap();
    let original = stored.iter().find(|item| item["Id"] == message).unwrap();
    assert_eq!(original["Content"], "");
    assert_eq!(original["Attachments"].as_array().unwrap().len(), 2);
    assert!(!original.to_string().contains("base64"));
    let markup_message = new_id();
    let markup = [(new_id(), "图表.svg", "<svg xmlns='http://www.w3.org/2000/svg'><rect width='20' height='10' fill='red'/></svg>"),
        (new_id(), "页面.html", "<!doctype html><style>body{background:#fafafa}h1{color:#146e49}</style><h1>预览样式</h1><button onclick='this.textContent=Number(this.textContent)+1'>0</button><script>document.body.dataset.chart='ready'</script>")];
    data::upsert_message(&fixture.runtime.config.data_root, &fixture.conversation,
        &json!({"Id":markup_message,"Role":"user","Content":"preview markup","Attachments":markup.iter().map(|(id,name,text)|json!({"Id":id,"Name":name,"SizeBytes":text.len()})).collect::<Vec<_>>()})).unwrap();
    for (id, name, text) in markup {
        std::fs::write(fixture.root.join("work").join(name), text).unwrap();
        let imported: Value = fixture.client.post(format!("{}/api/conversations/{}/attachments/import", fixture.url, fixture.conversation))
            .json(&json!({"messageId":markup_message,"files":[{"id":id,"name":name,"path":fixture.root.join("work").join(name)}]})).send().await.unwrap().json().await.unwrap();
        let sha = imported["attachments"][0]["sha256"].as_str().unwrap();
        let content_url = format!("{}/api/conversations/{}/attachments/{id}/content?messageId={markup_message}", fixture.url, fixture.conversation);
        let response = fixture.client.get(format!("{content_url}&version={sha}")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let policy = response.headers().get("Content-Security-Policy").unwrap().to_str().unwrap();
        assert!(policy.contains("sandbox") && policy.contains("style-src 'unsafe-inline'"));
        assert!(!policy.contains("allow-same-origin") && !policy.contains("allow-top-navigation"));
        if name.ends_with(".html") { assert!(policy.contains("sandbox allow-scripts") && policy.contains("script-src 'unsafe-inline'") && policy.contains("connect-src 'none'")); }
        else { assert!(!policy.contains("allow-scripts")); }
        assert_eq!(response.text().await.unwrap(), text);
        assert_eq!(fixture.client.get(format!("{content_url}&version=stale")).send().await.unwrap().status(), 409);
    }
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
    let stages = &timing["preparationStages"];
    assert_eq!(stages["automaticEvidenceSkipped"], true);
    assert_eq!(stages["preparationParallel"], true);
    assert!(stages["automaticEvidenceMs"].is_u64());
    assert!(stages["historyAndAttachmentsMs"].is_u64());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_preparation_keeps_authorized_local_evidence_in_the_first_model_request() {
    use kynxa_agent_core_native::retrieval::gateway_index;

    let fixture = Fixture::new(false, false).await;
    let settings: Value = fixture.client.get(format!("{}/api/retrieval/settings", fixture.url))
        .send().await.unwrap().json().await.unwrap();
    let saved = fixture.client.patch(format!("{}/api/retrieval/settings", fixture.url))
        .json(&json!({"expectedRevision":settings["revision"],
            "patch":{"local":{"semantic":"off","rerankProfileId":null}}}))
        .send().await.unwrap();
    assert!(saved.status().is_success(), "{}", saved.text().await.unwrap());
    let mut catalog: Value = fixture.client
        .get(format!("{}/api/conversations/catalog?includeMessages=false", fixture.url))
        .send().await.unwrap().json().await.unwrap();
    let private_chat = new_id();
    std::fs::create_dir(fixture.root.join("other-work")).unwrap();
    catalog["Projects"].as_array_mut().unwrap().push(json!({
        "Id":new_id(),"Name":"Other isolated work","FolderPath":fixture.root.join("other-work"),
        "Chats":[{"Id":private_chat,"Title":"Private fixture","Messages":[{
            "Id":new_id(),"Role":"user","Content":"Private fixture conversation",
            "CreatedAt":"2026-10-16T00:00:00Z"}]}]
    }));
    let saved = fixture.client.put(format!("{}/api/conversations/catalog", fixture.url))
        .json(&catalog).send().await.unwrap();
    assert!(saved.status().is_success(), "{}", saved.text().await.unwrap());
    // Use real indexed fragments in distinct formal scopes; no embedding model or synthetic search receipt is needed.
    // 在不同正式范围发布真实索引片段；此检查不运行嵌入模型，也不伪造搜索回执。
    let prepared = [
        (&fixture.conversation, "authorized-local", "auroraaccel 的索引队列采用 LOCAL_EVIDENCE_431 方案。"),
        (&private_chat, "private-local", "auroraaccel 的索引队列采用 PRIVATE_EVIDENCE_972 方案。"),
    ].into_iter().map(|(chat, source, text)| {
        gateway_index::prepare_source(&json!({"sourceId":source,
            "scopeKey":format!("chat:{chat}"),"sourceType":"document",
            "title":"auroraaccel 本地资料","text":text}), None).unwrap()
    }).collect::<Vec<_>>();
    gateway_index::publish_prepared(&fixture.runtime.config, &prepared).unwrap();
    let mut input = fixture.request("根据本地文档，auroraaccel 的索引队列采用什么方案？");
    input["userMessageId"] = json!(new_id());
    let response: Value = fixture.client.post(format!("{}/api/chat", fixture.url))
        .json(&input).send().await.unwrap().json().await.unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    let requests = fixture.provider.requests.lock().await;
    let wire = requests.first().unwrap().to_string();
    assert!(wire.contains("LOCAL_EVIDENCE_431"), "{wire}");
    assert!(!wire.contains("PRIVATE_EVIDENCE_972"), "{wire}");
    drop(requests);
    let messages = fixture.messages().await;
    let assistant = messages.iter().find(|message| message["Id"] == input["requestId"]).unwrap();
    assert!(assistant["RetrievalResultRef"].is_object(), "{assistant}");
    let stages = &assistant["ToolRun"]["diagnostics"]["requestTiming"]["preparationStages"];
    assert_eq!(stages["automaticEvidenceSkipped"], false);
    assert_eq!(stages["preparationParallel"], true);
    assert!(stages["automaticEvidenceMs"].is_u64());
    assert!(stages["historyAndAttachmentsMs"].is_u64());
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
async fn runtime_model_identity_tracks_switches_without_rewriting_history_or_confirmed_memory() {
    use kynxa_agent_core_native::storage::gateway as data;

    let fixture = Fixture::new(false, false).await;
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
    let synthetic_key = "runtime-identity-fixture-private-key";
    profile["displayName"] = json!("Anthropic Claude stale fixture label");
    profile["models"] = json!(["fixture-model", "second-model"]);
    profile["apiKey"] = json!(synthetic_key);
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);

    // Stale identities stay as historical data; switching must update the trusted request metadata.
    // 旧身份保留为历史资料；换模必须更新本次请求可信元数据，不能靠篡改原文消除冲突。
    let old_answer = "我是 Anthropic 的 Claude，这是一条旧模型的合成回复。";
    let old_assistant = new_id();
    data::upsert_message(
        &fixture.runtime.config.data_root,
        &fixture.conversation,
        &json!({"Id":old_assistant,"Role":"assistant","Status":"completed","Content":old_answer,
            "Provider":"obsolete-fixture","Model":"obsolete-claude"}),
    )
    .unwrap();
    let memory_content = "底层模型是 Anthropic Claude，继续沿用这个身份。";
    let memory_user = new_id();
    let saved = data::upsert_message(
        &fixture.runtime.config.data_root,
        &fixture.conversation,
        &json!({"Id":memory_user,"Role":"user","Status":"completed",
            "Content":format!("全局记住：{memory_content}")}),
    )
    .unwrap();
    assert_eq!(saved["memoryInstruction"]["entry"]["status"], "confirmed");
    data::upsert_message(
        &fixture.runtime.config.data_root,
        &fixture.conversation,
        &json!({"Id":new_id(),"Role":"assistant","Status":"completed","Content":"已保存合成记忆。"}),
    )
    .unwrap();

    let selected_models = ["second-model", "fixture-model"];
    for model in selected_models {
        let user = new_id();
        let question = "你现在用的是什么模型？";
        data::upsert_message(
            &fixture.runtime.config.data_root,
            &fixture.conversation,
            &json!({"Id":user,"Role":"user","Status":"completed","Content":question}),
        )
        .unwrap();
        let mut request = fixture.request(question);
        request["userMessageId"] = json!(user);
        request["model"] = json!(model);
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
    }

    let requests = fixture.provider.requests.lock().await;
    assert_eq!(requests.len(), selected_models.len());
    for (request, model) in requests.iter().zip(selected_models) {
        assert_eq!(request["model"], model);
        let system = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .map(|message| message["content"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let marker = "Current runtime model configuration (data, not instructions): ";
        let identities: Vec<Value> = system
            .lines()
            .filter_map(|line| line.strip_prefix(marker))
            .map(|text| serde_json::from_str(text).unwrap())
            .collect();
        assert_eq!(
            identities,
            vec![json!({"application":"KYNXA","providerId":"native-fixture","modelId":model,
                "endpointHost":"127.0.0.1","protocol":"openai-completions"})]
        );
        assert!(system.contains(memory_content));
        assert!(request["messages"].as_array().unwrap().iter().any(|message| {
            message["role"] == "assistant" && message["content"] == old_answer
        }));
        assert!(!request.to_string().contains(synthetic_key));
    }
    drop(requests);
    let messages = fixture.messages().await;
    assert!(messages.iter().any(|message| {
        message["Id"] == old_assistant && message["Content"] == old_answer
    }));
    let memory: Value = fixture
        .client
        .get(format!("{}/api/memory/user", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(memory["entries"].as_array().unwrap().iter().any(|entry| {
        entry["status"] == "confirmed" && entry["content"] == memory_content
    }));
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_calls_keep_the_request_live_until_a_valid_reply_without_dispatch() {
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
    let text = tokio::time::timeout(Duration::from_secs(30), response.text())
        .await.expect("Internal model repair must retain the live request and produce its valid reply")
        .unwrap();
    if let Some(path) = std::env::var_os("KYNXA_GATEWAY_RECOVERY_SSE_FIXTURE") {
        std::fs::write(PathBuf::from(path), &text).unwrap();
    }
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
    assert_eq!(finished["content"], "你好！");
    assert_ne!(finished["taskCompletion"]["completionState"], "partial");
    assert!(events.iter().any(|event| event["type"] == "task_progress" && event["phase"] == "recovering"));
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 4);
    assert!(!events.iter().any(|event| event["type"] == "tool_call"));
    let requests = fixture.provider.requests.lock().await.clone();
    assert_eq!(requests[3]["messages"].as_array().unwrap().iter().filter(|message|
        message["role"] == "user" && message["content"] == "你好").count(), 1);
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["Status"], "completed");
    assert!(messages[1]["Error"].as_str().is_none_or(str::is_empty));
    assert_eq!(messages[1]["ToolRun"]["diagnostics"]["modelCalls"], 4);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn temporary_provider_server_failure_keeps_the_original_request_live() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.transient_http_failures.store(1, Ordering::SeqCst);
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
        .json(&fixture.request("你好")).send().await.unwrap();
    let text = tokio::time::timeout(Duration::from_secs(30), response.text())
        .await.expect("A transient provider failure must not terminate the request").unwrap();
    let events = event_values(&mut text.as_bytes().to_vec());
    assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))));
    assert!(events.iter().any(|event| event["type"] == "task_progress" && event["phase"] == "recovering"));
    let completed = events.iter().find(|event| event["type"] == "completed").unwrap();
    assert_eq!(completed["content"], "你好！");
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.messages().await.len(), 2);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_startup_preparation_never_generates_or_blocks_chat() {
    let fixture = Fixture::new(false, false).await;
    let health = fixture
        .client
        .get(format!("{}/health", fixture.url))
        .send()
        .await
        .unwrap();
    assert!(health.status().is_success());
    // Core readiness schedules preparation immediately, even before the desktop submits its model selection.
    // 核心就绪后立即安排准备，不等待桌面提交模型选择，也不借用 MCP 的启动延迟。
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.runtime.model_preparation.snapshot().await["models"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Startup model preparation must not wait for the old three/five-second delays");
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
async fn skill_pagination_correction_keeps_context_and_request_live_with_compatible_run_limits() {
    for run_limits in [Value::Null, json!({"maxRounds":"invalid","maxDurationMs":null})] {
        let fixture = Fixture::new(false, false).await;
        fixture.provider.skill_pagination_cycle.store(true, Ordering::Release);
        let original = "列出可用技能，参数有误时在当前任务内部纠正并继续。";
        let mut input = fixture.request(original);
        // A new user message owns a new ID; the fixture already persists its initial greeting.
        // 夹具已保存最初的问候，新消息使用新 ID，不改写已有正式消息。
        input["userMessageId"] = json!(new_id());
        input["runLimits"] = run_limits;
        let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
            .json(&input).send().await.unwrap();
        assert!(response.status().is_success());
        let text = response.text().await.unwrap();
        let events = event_values(&mut text.as_bytes().to_vec());
        assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{text}");
        assert!(events.iter().any(|event| event["type"] == "completed"
            && event["content"].as_str().is_some_and(|text| text.contains("纠正分页"))), "{text}");
        let requests = fixture.provider.requests.lock().await.clone();
        let correction = requests.iter().find(|request| request["messages"].as_array().into_iter().flatten()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "skill-page-invalid")).unwrap();
        let invalid_receipt = correction["messages"].as_array().unwrap().iter()
            .find(|message| message["tool_call_id"] == "skill-page-invalid").unwrap()["content"].as_str().unwrap();
        assert!(invalid_receipt.contains("INVALID_SKILL_LIST_PAGINATION"), "{invalid_receipt}");
        assert!(invalid_receipt.contains("\"executed\":false"), "{invalid_receipt}");
        assert!(correction["messages"].as_array().unwrap().iter()
            .any(|message| message["role"] == "user" && message["content"].as_str() == Some(original)));
        let messages = fixture.messages().await;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["Content"], "你好");
        assert_eq!(messages[1]["Content"], original);
        let activities = messages[2]["ToolActivities"].as_array().unwrap();
        let pages = activities.iter().filter(|activity| activity["name"] == "skill.list").collect::<Vec<_>>();
        assert_eq!(pages.len(), 2, "{activities:?}");
        assert_eq!(pages[0]["status"], "error");
        assert_eq!(pages[1]["status"], "completed");
        let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(&fixture.root.join("Data")).unwrap();
        let task = repository.find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
            .unwrap().unwrap();
        assert_eq!(task.budget.max_execution_ms, Some(1_800_000));
        let persisted: Value = serde_json::from_slice(&std::fs::read(fixture.root.join("Data")
            .join(task.input_reference.as_ref().unwrap()["relativePath"].as_str().unwrap())).unwrap()).unwrap();
        assert_eq!(persisted["input"]["runLimits"], input["runLimits"]);
        assert_eq!(persisted["input"]["message"], original);
        assert_eq!(persisted["resolvedRunLimits"]["maxDurationMs"], 1_800_000);
        let calls_before_replay = fixture.provider.calls.load(Ordering::SeqCst);
        let replay: Value = fixture.client.post(format!("{}/api/chat", fixture.url))
            .json(&input).send().await.unwrap().json().await.unwrap();
        assert_eq!(replay["type"], "completed", "{replay}");
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), calls_before_replay);
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_revoked_during_approval_skips_stale_batch_and_preserves_completed_effects() {
    for scope in ["once", "task"] {
        let fixture = Fixture::new(false, false).await;
        fixture.provider.memory_approval_cycle.store(true, Ordering::Release);
        let marker = "MEMORY_APPROVAL_SENTINEL_521";
        let saved = fixture.client.post(format!("{}/api/conversations/{}/memory", fixture.url, fixture.conversation))
            .json(&json!({"scope":"chat","kind":"preference","content":marker,"expectedRevision":0}))
            .send().await.unwrap();
        assert!(saved.status().is_success());
        let memory: Value = saved.json().await.unwrap();
        let original = "完成工作文件维护并核对保存结果。";
        let mut input = fixture.request(original);
        input["userMessageId"] = json!(new_id());
        let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
            .json(&input).send().await.unwrap();
        assert!(response.status().is_success());
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut events = Vec::new();
        let mut decisions = 0;
        tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(chunk) = stream.next().await {
                buffer.extend_from_slice(&chunk.unwrap());
                for event in event_values(&mut buffer) {
                    if event["type"] == "approval_required" {
                        decisions += 1;
                        assert_eq!(decisions, 1, "Unexpected repeated approval: {event}");
                        assert_eq!(event["tool"]["toolCallId"], "withdrawn-memory-delete");
                        assert_eq!(std::fs::read_to_string(fixture.root.join("work/saved-before-approval.txt")).unwrap(), "successful-output-kept");
                        // Revoke through the real gateway while its original SSE request is still waiting.
                        // 原始 SSE 请求仍在等待审批时，通过真实网关撤回记忆。
                        let removed = fixture.client.delete(format!("{}/api/conversations/{}/memory/{}",
                            fixture.url, fixture.conversation, memory["entries"][0]["id"].as_str().unwrap()))
                            .json(&json!({"expectedRevision":memory["revision"]})).send().await.unwrap();
                        assert!(removed.status().is_success(), "{}", removed.text().await.unwrap());
                        let approved = fixture.client.post(format!("{}/api/agent/approvals", fixture.url))
                            .json(&json!({"conversationId":fixture.conversation,"requestId":input["requestId"],
                                "toolCallId":event["tool"]["toolCallId"],"approvalId":event["tool"]["approvalId"],
                                "approved":true,"scope":scope})).send().await.unwrap();
                        assert!(approved.status().is_success(), "{}", approved.text().await.unwrap());
                    }
                    events.push(event);
                }
            }
        }).await.unwrap();
        assert_eq!(decisions, 1);
        assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{events:?}");
        assert!(events.iter().any(|event| event["type"] == "completed"), "{events:?}");
        let skipped = events.iter().find(|event| event["type"] == "tool_result"
            && event["tool"]["toolCallId"] == "withdrawn-memory-delete").unwrap();
        assert_eq!(skipped["tool"]["code"], "TASK_MEMORY_UPDATED");
        assert_eq!(skipped["tool"]["executed"], false);
        assert_eq!(std::fs::read_to_string(fixture.root.join("work/fixture.txt")).unwrap(), "native-file-body");
        assert!(!fixture.root.join("work/stale-batch-tail.txt").exists());
        assert_eq!(events.iter().filter(|event| event["type"] == "tool_result"
            && event["tool"]["toolCallId"] == "saved-before-approval"
            && event["tool"]["status"] == "completed").count(), 1);
        let requests = fixture.provider.requests.lock().await.clone();
        assert!(system_text(&requests[0]).contains(marker));
        let replanned = requests.iter().find(|request| request["messages"].as_array().into_iter().flatten()
            .any(|message| message["tool_call_id"] == "withdrawn-memory-delete")).unwrap();
        assert!(!system_text(replanned).contains(marker));
        for id in ["withdrawn-memory-delete", "stale-batch-tail"] {
            let receipt: Value = serde_json::from_str(replanned["messages"].as_array().unwrap().iter()
                .find(|message| message["tool_call_id"] == id).unwrap()["content"].as_str().unwrap()).unwrap();
            assert_eq!(receipt["executed"], false, "{receipt}");
            assert_eq!(receipt["requestDispatched"], false, "{receipt}");
            assert_eq!(receipt["code"], "TASK_MEMORY_UPDATED");
            assert!(receipt.get("correction").is_none());
        }
        let messages = fixture.messages().await;
        assert_eq!(messages[0]["Content"], "你好");
        assert_eq!(messages[1]["Content"], original);
        assert!(messages[2]["Content"].as_str().unwrap().contains("先保留成功输出"));
        assert!(messages[2]["Content"].as_str().unwrap().contains("根据当前记忆重新规划"));
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloaded_tool_definition_loads_once_and_continues_with_original_call() {
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
        events.iter().filter(|event| event["type"] == "tool_result"
            && event["tool"]["name"] == "filesystem.stat"
            && event["tool"]["status"] == "completed").count() == 2,
        "{text}"
    );
    assert!(!text.contains("TOOL_NOT_LOADED"), "{text}");
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
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 3);
    let requests = fixture.provider.requests.lock().await;
    let wire_name = kynxa_agent_core_native::models::protocol::wire_name("filesystem.stat");
    assert!(!requests[0]["tools"].as_array().unwrap().iter().any(|tool| tool["function"]["name"] == wire_name));
    assert!(requests[1]["tools"].as_array().unwrap().iter().any(|tool| tool["function"]["name"] == wire_name));
    let result = requests[1]["messages"].as_array().unwrap().iter().find(|message| message["role"] == "tool").unwrap();
    assert_eq!(result["tool_call_id"], "unloaded-0");
    assert!(result["content"].as_str().unwrap().contains("fixture.txt"), "{result}");
    assert!(!requests.iter().any(|request| request["messages"].as_array().unwrap().iter().any(|message| {
        message["tool_calls"].as_array().is_some_and(|calls| calls.iter().any(|call| call["function"]["name"] == kynxa_agent_core_native::models::protocol::wire_name("tool.load")))
    })));
    drop(requests);
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
async fn user_steering_anchors_the_new_reply_after_formal_input_without_restarting_attempt() {
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
    let initial_attempt = repository.current_attempt(&task_id).unwrap().unwrap();
    let message_id = new_id();
    let reply_id = new_id();
    let amendment = "更新方案：保留原范围，只解释结果，不再继续写入。";
    let steering_input = json!({"conversationId":fixture.conversation,"expectedVersion":before.version,"userMessageId":message_id,"replyMessageId":reply_id,"message":amendment,"action":"update"});
    let steering_started = std::time::Instant::now();
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
    assert_eq!(accepted["reconnectRequired"], false);
    assert_eq!(accepted["resumeRequired"], false);
    assert_eq!(accepted["task"]["state"], "running");
    assert_eq!(accepted["task"]["taskId"], task_id);
    assert_eq!(accepted["affectedTaskIds"][0], task_id);
    assert_eq!(accepted["replyMessageId"], reply_id);
    let mut events = Vec::new();
    while !events.iter().any(|event:&Value|event["type"]=="text_delta" && event["replyMessageId"]==reply_id) {
        let bytes=tokio::time::timeout(Duration::from_secs(8),stream.next()).await.unwrap().unwrap().unwrap();
        buffer.extend_from_slice(&bytes);
        events.extend(event_values(&mut buffer));
    }
    assert!(steering_started.elapsed()<Duration::from_secs(8),"Guidance must not wait for the 30-second obsolete generation.");
    let latest_message_id = new_id();
    let latest_reply_id = new_id();
    let latest_amendment = "再补充：用简短结论回答，接着当前工作。";
    let latest_input = json!({"conversationId":fixture.conversation,"expectedVersion":repository.load(&task_id).unwrap().version,"userMessageId":latest_message_id,"replyMessageId":latest_reply_id,"message":latest_amendment,"action":"update"});
    let latest: Value = fixture.client.post(format!("{}/api/tasks/{task_id}/steer",fixture.url)).json(&latest_input).send().await.unwrap().json().await.unwrap();
    assert_eq!(latest["replyMessageId"],latest_reply_id,"{latest}");
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .unwrap()
    {
        buffer.extend_from_slice(&bytes.unwrap());
        events.extend(event_values(&mut buffer));
    }
    assert_eq!(events.last().unwrap()["type"], "completed");
    assert_eq!(events.last().unwrap()["requestId"], input["requestId"]);
    assert_eq!(events.last().unwrap()["replyMessageId"], latest_reply_id);
    let anchor = events.iter().rposition(|event|event["type"]=="reply_anchor").unwrap();
    assert_eq!(events[anchor]["replyMessageId"],latest_reply_id);
    assert_eq!(events[anchor]["userMessageId"],latest_message_id);
    assert!(events[anchor]["supersededReplyMessageId"]==input["requestId"] || events[anchor]["supersededReplyMessageId"]==reply_id);
    assert!(events[anchor+1..].iter().all(|event|event["replyMessageId"]==latest_reply_id));
    assert!(!events.last().unwrap()["content"].as_str().unwrap().contains("已开始，保留这段正文。"));
    let completed_model_calls = fixture.provider.calls.load(Ordering::Acquire);
    assert_eq!(
        completed_model_calls,
        4,
        "Two superseded generations, the final guided answer, and one independent review are required."
    );
    let actual = fixture.provider.requests.lock().await;
    assert_eq!(actual.len(), 4);
    assert_eq!(
        actual.iter().filter(|request| is_completion_review_request(request)).count(),
        1,
        "The guided answer must receive exactly one completion review."
    );
    assert!(is_completion_review_request(&actual[3]));
    assert!(actual[3]["tools"].as_array().is_none_or(Vec::is_empty));
    assert!(
        actual[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "user" && message["content"] == amendment)
    );
    // Runtime guidance must not impersonate a later user request and obscure the real steering text.
    // 调度说明不能冒充更晚的用户请求，覆盖用户实际插话。
    assert_eq!(actual[1]["messages"].as_array().unwrap().last().unwrap()["content"], amendment);
    drop(actual);
    let settled=repository.load(&task_id).unwrap();
    assert_eq!(settled.usage.model_requests,4);
    assert_eq!(settled.usage.failed_requests,0,"User guidance is not a failed model request.");
    assert_eq!(settled.usage.retries,0);
    assert!(!fixture.root.join("work/obsolete.txt").exists(),"Partial tool arguments must never execute.");
    assert!(repository.operations(&task_id).unwrap().is_empty());
    assert_eq!(
        repository.load(&task_id).unwrap().state,
        TaskState::Completed
    );
    assert_eq!(repository.user_steering(&task_id).unwrap().len(), 2);
    assert_eq!(
        repository.current_attempt(&task_id).unwrap().unwrap().token,
        initial_attempt.token,
        "steering must retain the original execution fence and receipt owner"
    );
    let messages = fixture.messages().await;
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["Id"] == message_id && message["Content"] == amendment)
            .count(),
        1
    );
    let old = messages.iter().find(|message|message["Id"]==input["requestId"]).unwrap();
    assert!(old["SupersededByUserMessageId"]==message_id || old["SupersededByUserMessageId"]==latest_message_id);
    assert_eq!(old["Status"],"interrupted");
    assert!(old["Content"].as_str().unwrap().contains("已开始，保留这段正文。"));
    let user_position = messages.iter().position(|message|message["Id"]==latest_message_id).unwrap();
    let reply_position = messages.iter().position(|message|message["Id"]==latest_reply_id).unwrap();
    assert!(reply_position>user_position,"{messages:?}");
    let reply=&messages[reply_position];
    assert_eq!(reply["RequestId"],input["requestId"]);
    assert_eq!(reply["ReplyTo"],latest_message_id);
    assert_eq!(reply["InReplyToUserMessageId"],latest_message_id);
    assert!(reply["SupersededByUserMessageId"].is_null());
    assert!(!reply["Content"].as_str().unwrap().is_empty());
    assert!(!reply["Content"].as_str().unwrap().contains("已开始，保留这段正文。"));
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
    assert_eq!(
        fixture.provider.calls.load(Ordering::Acquire),
        completed_model_calls,
        "Replaying accepted guidance must not dispatch another model request."
    );
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
    // Callable schemas are the contract; a redundant prose name list is no longer required.
    // 可调用 schema 才是能力合同；精简提示词后不再要求重复列出工具名的旧文案。
    assert!(
        last_request["tools"].as_array().unwrap().iter().any(|tool|
            tool["function"]["name"] == kynxa_agent_core_native::models::protocol::wire_name("filesystem.read")),
        "The already-loaded reader was lost during compaction: {last_request}"
    );
    // Actual read receipts, restored analysis and the final file above verify reentry and delivery.
    // 上面的真实读取回执、恢复分析及最终文件已验证接续交付，不依赖已移除的强制收尾提示文案。
    drop(requests);
    fixture.close().await;
}
