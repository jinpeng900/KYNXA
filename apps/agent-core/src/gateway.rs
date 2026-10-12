//! Loopback HTTP/SSE adapter for the native backend; formal state lives in domain services.
//! 原生后端的本机 HTTP/SSE 适配；正式状态仍属于领域服务。

use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    orchestration::gateway_runtime::{GatewayRuntime, RequestRun, terminal_event},
};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::any,
};
use futures_util::stream;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

fn response(result: Result<(u16, Value), GatewayError>) -> Response {
    let (status, value) = match result {
        Ok(result) => result,
        Err(error) => (error.status, error.response()),
    };
    let value = crate::storage::gateway::normalize_transport_numbers(value);
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        axum::Json(value),
    )
        .into_response()
}

fn reply_response(result: Result<Value, GatewayError>) -> Response {
    response(result.and_then(|value| {
        if value["type"] == "error"
            || value["type"] == "interrupted" && !value["content"].is_string()
        {
            let status = value["statusCode"]
                .as_u64()
                .and_then(|status| u16::try_from(status).ok())
                .filter(|status| (400..=599).contains(status))
                .unwrap_or(if value["type"] == "interrupted" {
                    499
                } else {
                    500
                });
            Err(GatewayError::new(
                value["code"].as_str().unwrap_or("CHAT_REPLY_FAILED"),
                value["error"].as_str().unwrap_or("模型回复未完成。"),
                status,
            ))
        } else {
            Ok((200, value))
        }
    }))
}
pub fn health() -> Value {
    json!({
        "service": "kynxa-model-gateway",
        "status": "ok",
        "runtime": "rust",
        "nodeRuntime": false,
        "conversationProtocol": 1,
        "dataLayoutVersion": 1,
        "memoryProtocol": 1,
        "contextProtocol": 3,
        "agentProtocol": 5,
        "officialToolsProtocol": 2,
        "hostTerminalProtocol": 3,
        "browserAutomationProtocol": 2,
        "extensionStorageProtocol": 1,
        "toolStreamProtocol": 3,
        "retrievalProtocol": 1
    })
}

pub fn router(runtime: Arc<GatewayRuntime>) -> Router {
    Router::new()
        .fallback(any(handle))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(runtime)
}

struct StreamOwner(Arc<crate::orchestration::gateway_runtime::RequestRun>);
impl Drop for StreamOwner {
    fn drop(&mut self) {
        if !self.0.finished.load(Ordering::Acquire) {
            self.0.cancel();
        }
    }
}

async fn wait_reply(run: &RequestRun) -> Result<Value, GatewayError> {
    let mut events = run.events.subscribe();
    loop {
        if let Some(result) = run.result.lock().await.clone() {
            return Ok(result);
        }
        match events.recv().await {
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return run.result.lock().await.clone().ok_or_else(|| {
                    GatewayError::new("CHAT_STREAM_CLOSED", "任务事件流已关闭。", 500)
                });
            }
        }
    }
}

struct RequestEventStream {
    run: Arc<RequestRun>,
    receiver: tokio::sync::broadcast::Receiver<Value>,
    pending: std::collections::VecDeque<Value>,
    checkpoint: Option<u64>,
    delivered: u64,
    ended: bool,
}
impl RequestEventStream {
    async fn new(run: Arc<RequestRun>) -> Result<Self, GatewayError> {
        // Subscribe before the atomic snapshot, so every later event remains observable.
        // 先订阅再取得原子快照，确保快照之后的事件仍能读取。
        let receiver = run.events.subscribe();
        let recovery = run.recovery(0).await?;
        Ok(Self {
            run,
            receiver,
            pending: recovery.events,
            checkpoint: Some(recovery.through),
            delivered: 0,
            ended: false,
        })
    }
    async fn recover(&mut self) -> bool {
        // Subscribe before reading the replacement snapshot; its watermark covers the old lagged cursor.
        // 读取替换快照前重新订阅；快照水位覆盖旧落后游标，后续增量仍由新订阅接收。
        self.receiver = self.run.events.subscribe();
        match self.run.recovery(self.delivered).await {
            Ok(recovery) => {
                self.pending = recovery.events;
                self.checkpoint = Some(recovery.through);
                true
            }
            Err(error) => {
                self.pending.push_back(json!({"type":"error","error":error.message,"code":error.code,"conversationId":self.run.conversation_id,"requestId":self.run.request_id,"createdAt":crate::gateway_contract::now_iso8601()}));
                false
            }
        }
    }
    async fn next(&mut self) -> Option<Value> {
        loop {
            if self.ended {
                return None;
            }
            if let Some(event) = self.pending.pop_front() {
                self.ended = terminal_event(&event);
                return Some(event);
            }
            // Replacement snapshots and required controls are applied before skipping their covered deltas.
            // 替换快照及必要控制事件全部应用后，才跳过该水位已包含的旧增量。
            if let Some(through) = self.checkpoint.take() {
                self.delivered = through;
            }
            let received = match self.receiver.try_recv() {
                Ok(event) => Ok(event),
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(count)) => {
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count))
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    if self.run.finished.load(Ordering::Acquire) {
                        if !self.recover().await || !self.pending.is_empty() {
                            continue;
                        }
                        self.ended = true;
                        return None;
                    }
                    self.receiver.recv().await
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    Err(tokio::sync::broadcast::error::RecvError::Closed)
                }
            };
            let event = match received {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    self.recover().await;
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    if self.recover().await && self.pending.is_empty() {
                        self.ended = true;
                        return None;
                    }
                    continue;
                }
            };
            let sequence = event["eventSequence"].as_u64().unwrap_or(0);
            if sequence <= self.delivered {
                continue;
            }
            self.delivered = sequence;
            self.ended = terminal_event(&event);
            return Some(event);
        }
    }
}

async fn handle(
    State(runtime): State<Arc<GatewayRuntime>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    // Browser-origin traffic cannot inherit the native desktop's authority even on loopback.
    // 即便请求来自回环地址，带浏览器 Origin 的流量也不能继承桌面端的本机权限。
    if headers.contains_key(header::ORIGIN) {
        return response(Err(GatewayError::new(
            "BROWSER_ORIGIN_FORBIDDEN",
            "浏览器来源不能直接使用本机管理接口。",
            403,
        )));
    }
    let raw = match std::str::from_utf8(&bytes) {
        Ok(value) => value.to_owned(),
        Err(_) => {
            return response(Err(GatewayError::new(
                "INVALID_JSON",
                "请求不是有效 UTF-8。",
                400,
            )));
        }
    };
    let body = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_str::<Value>(&crate::storage::raw_json::scalar_projection(&raw)) {
            Ok(value) => crate::storage::gateway::normalize_transport_numbers(value),
            Err(_) => {
                return response(Err(GatewayError::new(
                    "INVALID_JSON",
                    "请求 JSON 无效。",
                    400,
                )));
            }
        }
    };
    let path = uri.path();
    let query = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect::<Vec<_>>();
    if path == "/health" && method == Method::GET {
        runtime.schedule_extension_prewarm();
        return response(Ok((200, health())));
    }
    if method == Method::POST && ["/api/chat", "/api/chat/stream"].contains(&path) {
        let original = serde_json::from_str::<HashMap<String, &serde_json::value::RawValue>>(&raw)
            .ok()
            .and_then(|fields| fields.get("message").map(|value| value.get().to_owned()));
        return match runtime.begin_raw(body, original).await {
            Err(error) => response(Err(error)),
            Ok(run) => {
                if path == "/api/chat" {
                    reply_response(wait_reply(&run).await)
                } else {
                    let events = match RequestEventStream::new(run.clone()).await {
                        Ok(events) => events,
                        Err(error) => return response(Err(error)),
                    };
                    let owner = StreamOwner(run.clone());
                    let source =
                        stream::unfold((events, owner), |(mut events, owner)| async move {
                            let event = events.next().await?;
                            let event = crate::storage::gateway::normalize_transport_numbers(event);
                            Some((
                                Ok::<_, Infallible>(Event::default().data(event.to_string())),
                                (events, owner),
                            ))
                        });
                    let mut result = Sse::new(source)
                        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
                        .into_response();
                    result.headers_mut().insert(
                        header::CACHE_CONTROL,
                        header::HeaderValue::from_static("no-cache"),
                    );
                    result
                }
            }
        };
    }
    if path == "/api/chat/cancel" && method == Method::POST {
        return response(
            runtime
                .cancel(
                    body["conversationId"].as_str().unwrap_or(""),
                    body["requestId"].as_str().unwrap_or(""),
                )
                .await
                .map(|value| (200, value)),
        );
    }
    if path == "/api/agent/approvals" && method == Method::POST {
        return response(runtime.approve(&body).await.map(|value| (200, value)));
    }
    if path == "/api/models/preparation" {
        if method == Method::POST {
            return response(runtime.prepare_models(body).await.map(|value| (202, value)));
        }
        if method == Method::GET {
            return response(Ok((200, runtime.model_preparation.snapshot().await)));
        }
    }
    if path.starts_with("/api/models") {
        if let Some(result) = crate::models::gateway_config::handle_models(
            method.as_str(),
            path,
            body.clone(),
            &runtime.config.data_home,
        )
        .await
        {
            return response(result.map_err(Into::into));
        }
    }
    let text_root = runtime.config.data_root.clone();
    let text_method = method.to_string();
    let text_path = path.to_owned();
    let text_query = query.clone();
    let preserved = tokio::task::spawn_blocking(move || {
        crate::storage::gateway::handle_text(&text_method, &text_path, &text_query, &text_root)
    })
    .await;
    match preserved {
        Ok(Some(Ok((status, raw)))) => {
            return (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                raw,
            )
                .into_response();
        }
        Ok(Some(Err(error))) => return response(Err(error.into())),
        Err(_) => {
            return response(Err(GatewayError::new(
                "DATA_WORKER_FAILED",
                "正式数据读取未完成。",
                500,
            )));
        }
        _ => {}
    }
    let data_root = runtime.config.data_root.clone();
    let stored_method = method.to_string();
    let stored_path = path.to_owned();
    let stored_query = query.clone();
    let stored_body = body.clone();
    let stored = tokio::task::spawn_blocking(move || {
        crate::storage::gateway::handle_raw(
            &stored_method,
            &stored_path,
            &stored_query,
            &stored_body,
            &raw,
            &data_root,
        )
    })
    .await;
    match stored {
        Ok(Some(result)) => return response(result.map_err(Into::into)),
        Err(_) => {
            return response(Err(GatewayError::new(
                "DATA_WORKER_FAILED",
                "正式数据操作未完成。",
                500,
            )));
        }
        _ => {}
    }
    let config = runtime.config.clone();
    let retrieved_method = method.to_string();
    let retrieved_path = path.to_owned();
    let retrieved_query = query.clone();
    let retrieved_body = body.clone();
    let retrieval = tokio::task::spawn_blocking(move || {
        crate::retrieval::gateway::handle(
            &retrieved_method,
            &retrieved_path,
            &retrieved_query,
            &retrieved_body,
            &config,
        )
    })
    .await;
    match retrieval {
        Ok(Some(result)) => return response(result.map_err(Into::into)),
        Err(_) => {
            return response(Err(GatewayError::new(
                "RETRIEVAL_WORKER_FAILED",
                "检索操作未完成。",
                500,
            )));
        }
        _ => {}
    }
    if path.starts_with("/api/agent/") || path == "/api/retrieval/providers" {
        let (_owner, cancellation) = tokio::sync::watch::channel(false);
        let conversation = query
            .iter()
            .find(|(key, _)| key == "conversationId")
            .map(|(_, value)| value.as_str());
        let context = if let Some(id) = conversation {
            runtime.tool_context(id, "ask", "", cancellation).await
        } else {
            Ok(crate::tools::gateway::GatewayToolContext {
                data_home: runtime.config.data_root.clone(),
                extension_root: runtime.config.extension_root.clone(),
                workdir: runtime.config.data_root.join("Sandboxes"),
                toolhost_exe: runtime.config.toolhost_exe.clone(),
                permission_mode: "ask".into(),
                conversation_id: String::new(),
                project_id: None,
                authorized_roots: vec![],
                cancellation,
                runtime: runtime.tools.clone(),
                config: json!({"officialToolsRoot":runtime.config.application_root.join("official-tools")}),
                approved: false,
                events: None,
                external_execute: None,
            })
        };
        return response(match context {
            Err(error) => Err(error),
            Ok(mut context) => match crate::tools::gateway::get_config(&context).await {
                Err(error) => Err(error),
                Ok(mut config) => {
                    config["officialToolsRoot"] =
                        json!(runtime.config.application_root.join("official-tools"));
                    context.config = config;
                    if path == "/api/retrieval/providers" && method == Method::GET {
                        let config = runtime.config.clone();
                        let effective = tokio::task::spawn_blocking(move || {
                            crate::retrieval::gateway::effective(&config, None)
                        })
                        .await;
                        match effective {
                            Ok(Ok(settings)) => Ok((
                                200,
                                crate::tools::gateway::providers(
                                    &context,
                                    settings["web"]["providerId"].as_str().unwrap_or(""),
                                )
                                .await,
                            )),
                            Ok(Err(error)) => Err(error.into()),
                            Err(_) => Err(GatewayError::new(
                                "RETRIEVAL_WORKER_FAILED",
                                "检索设置读取失败。",
                                500,
                            )),
                        }
                    } else {
                        match crate::tools::gateway::handle_route(
                            method.as_str(),
                            path,
                            &query.into_iter().collect::<HashMap<_, _>>(),
                            body,
                            &context,
                        )
                        .await
                        {
                            Ok(Some(value)) => Ok(value),
                            Ok(None) => Err(GatewayError::new("NOT_FOUND", "接口不存在。", 404)),
                            Err(error) => Err(error),
                        }
                    }
                }
            },
        });
    }
    response(Err(GatewayError::new("NOT_FOUND", "接口不存在。", 404)))
}

pub async fn serve(config: GatewayConfig) -> Result<(), GatewayError> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.port))
        .await
        .map_err(|_| {
            GatewayError::new(
                "GATEWAY_PORT_UNAVAILABLE",
                "网关端口被占用或无法监听。",
                503,
            )
        })?;
    let _data_owner = own_data_root(&config.data_root)?;
    let runtime = GatewayRuntime::new(config.clone());
    if config.inference_library.is_file() {
        crate::models::gateway_inference::initialize_runtime(&config.inference_library)
            .map_err(GatewayError::from)?;
    }
    let resource = config
        .application_root
        .join("runtime/resource/kynxa-resource-service.exe");
    if !resource.is_file() {
        return Err(GatewayError::new(
            "RESOURCE_RUNTIME_UNAVAILABLE",
            "安装包缺少原生资源服务。",
            503,
        ));
    }
    crate::resources::gateway::install(
        crate::resources::gateway::ResourceService::start(&resource).await?,
    );
    crate::retrieval::gateway_embedding::install_runtime(tokio::runtime::Handle::current());
    let background_config = config.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        crate::retrieval::gateway::start_background(&background_config)
    })
    .await;
    if let Err(error) = prepared
        .as_ref()
        .map_err(|_| GatewayError::new("INDEX_START_FAILED", "后台资料服务未启动。", 503))
        .and_then(|result| {
            result.as_ref().map_err(|error| {
                GatewayError::new(error.code(), error.message(), error.status_code())
            })
        })
    {
        eprintln!(
            "{}",
            json!({"code":error.code,"message":error.message,"lexicalReadAvailable":true})
        );
    }
    println!(
        "{}",
        json!({"service":"kynxa-model-gateway","runtime":"rust","port":listener.local_addr()?.port(),"nodeRuntime":false})
    );
    let shutdown = runtime.clone();
    axum::serve(listener, router(runtime))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.shutdown().await;
        })
        .await?;
    Ok(())
}

pub fn initialize_storage(target: &str) -> Result<Value, GatewayError> {
    let root = std::path::Path::new(target);
    if !root.is_absolute() {
        return Err(GatewayError::new(
            "INVALID_DATA_DIRECTORY",
            "数据目录必须使用绝对路径。",
            400,
        ));
    }
    let catalog = crate::storage::gateway::load_catalog(root)?;
    let projects = catalog["Projects"].as_array().map_or(0, Vec::len);
    let chats = catalog["Chats"].as_array().map_or(0, Vec::len)
        + catalog["Projects"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|project| project["Chats"].as_array().map_or(0, Vec::len))
            .sum::<usize>();
    Ok(json!({"layoutVersion":1,"projects":projects,"chats":chats}))
}

/// An OS-owned lock expires with the process; stale PID files never authorize a second writer.
/// 所有者锁由操作系统随进程释放，过期 PID 文件不能授权第二个正式数据写入者。
fn own_data_root(root: &std::path::Path) -> Result<std::fs::File, GatewayError> {
    // Only trusted application configuration selects this root; validate and prepare every missing ancestor.
    // 此根目录仅由可信应用配置选定；逐层核验并准备缺失祖先，不能只假设最终一层存在。
    let volume_root: std::path::PathBuf = root
        .components()
        .take_while(|part| {
            matches!(
                part,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            )
        })
        .collect();
    crate::storage::managed_files::inspect(&volume_root, root, true, false, u64::MAX)?;
    crate::storage::managed_files::inspect(root, root, false, false, u64::MAX)?;
    let path = root.join(".gateway-owner.lock");
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 {
            return Err(GatewayError::new(
                "INVALID_DATA_OWNER_PATH",
                "数据目录的所有者锁路径无效，原位置已保留。",
                409,
            ));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0).custom_flags(0x00200000);
    }
    // Acquire before inspecting the file handle: a second reader would conflict with this exclusive Windows lock.
    // 先取得锁再核验文件句柄；额外读取句柄本身会与 Windows 独占锁冲突。
    let lock = options.open(&path).map_err(|error| {
        if matches!(error.raw_os_error(), Some(32 | 33)) {
            GatewayError::new(
                "DATA_OWNER_ACTIVE",
                "此数据目录已有网关持有，请关闭旧网关后启动新版本。",
                409,
            )
        } else {
            GatewayError::new(
                "DATA_OWNER_IO_FAILED",
                "无法取得数据目录所有者锁，请检查路径和读写权限。",
                500,
            )
        }
    })?;
    let identity = crate::file_identity::handle_identity(&lock)?;
    if identity.is_link || identity.links != 1 {
        return Err(GatewayError::new(
            "INVALID_DATA_OWNER_PATH",
            "数据目录所有者锁不能是链接或共享文件。",
            409,
        ));
    }
    crate::storage::managed_files::inspect(root, root, false, false, u64::MAX)?;
    Ok(lock)
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn data_owner_lock_supports_first_start_rejects_second_owner_and_reopens_after_exit() {
        let fixture = std::env::temp_dir().join(format!(
            "kynxa-owner-lock-{}",
            crate::gateway_contract::new_id()
        ));
        let root = fixture.join("nested").join("Data");
        let first = own_data_root(&root).unwrap();
        assert!(root.join(".gateway-owner.lock").is_file());
        assert_eq!(own_data_root(&root).unwrap_err().code, "DATA_OWNER_ACTIVE");
        drop(first);
        drop(own_data_root(&root).unwrap());
        std::fs::remove_file(root.join(".gateway-owner.lock")).unwrap();
        std::fs::create_dir(root.join(".gateway-owner.lock")).unwrap();
        assert_eq!(
            own_data_root(&root).unwrap_err().code,
            "INVALID_DATA_OWNER_PATH"
        );
        std::fs::remove_dir(root.join(".gateway-owner.lock")).unwrap();
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_dir(fixture.join("nested")).unwrap();
        std::fs::remove_dir(fixture).unwrap();
    }

    fn segment(id: &str, round: u64, order: u64, status: &str, content: &str) -> Value {
        json!({"id":id,"round":round,"order":order,"phase":"commentary","status":status,"content":content,"reasoning":"","reasoningDurationMs":0})
    }

    fn tool(status: &str) -> Value {
        json!({"toolCallId":"call-1","name":"filesystem.read","arguments":{},"status":status,"summary":"temporary fixture","round":1,"order":2})
    }

    #[tokio::test]
    async fn lagged_stream_restores_boundaries_and_skips_snapshot_covered_deltas() {
        let run = RequestRun::test_run(4);
        let mut stream = RequestEventStream::new(run.clone()).await.unwrap();
        run.emit(json!({"type":"started"})).await;
        assert_eq!(stream.next().await.unwrap()["type"], "started");
        run.emit(json!({"type":"assistant_segment","segment":segment("first",1,1,"streaming",""),"toolStreamProtocol":3})).await;
        assert_eq!(
            stream.next().await.unwrap()["segment"]["status"],
            "streaming"
        );
        for _ in 0..17 {
            run.emit(json!({"type":"text_delta","segmentId":"first","delta":"x"}))
                .await;
        }
        run.emit(json!({"type":"assistant_segment","segment":segment("first",1,1,"completed",&"x".repeat(17)),"toolStreamProtocol":3})).await;
        run.emit(json!({"type":"tool_call","tool":tool("running")}))
            .await;
        let mut approval = tool("approval-required");
        approval["approvalId"] = json!(crate::gateway_contract::new_id());
        run.emit(json!({"type":"approval_required","tool":approval}))
            .await;
        let mut receipt = tool("completed");
        receipt["result"] = json!("temporary observation");
        run.emit(json!({"type":"tool_result","tool":receipt})).await;
        run.emit(json!({"type":"assistant_segment","segment":segment("second",2,3,"streaming",""),"toolStreamProtocol":3})).await;
        for _ in 0..17 {
            run.emit(json!({"type":"text_delta","segmentId":"second","delta":"y"}))
                .await;
        }
        let mut recovered = Vec::new();
        loop {
            let event = stream.next().await.unwrap();
            let snapshot = event["type"] == "content_snapshot";
            recovered.push(event);
            if snapshot {
                break;
            }
        }
        assert_eq!(
            recovered
                .iter()
                .filter(|event| event["type"] == "started")
                .count(),
            0
        );
        assert_eq!(recovered[0]["segment"]["status"], "completed");
        let types: Vec<_> = recovered
            .iter()
            .filter_map(|event| event["type"].as_str())
            .collect();
        assert_eq!(
            types,
            vec![
                "assistant_segment",
                "tool_call",
                "approval_required",
                "tool_result",
                "assistant_segment",
                "assistant_segment",
                "content_snapshot"
            ]
        );
        assert_eq!(recovered[1]["tool"]["round"], recovered[3]["tool"]["round"]);
        assert_eq!(recovered[1]["tool"]["order"], recovered[3]["tool"]["order"]);
        assert_eq!(recovered[5]["segment"]["content"], "y".repeat(17));
        assert_eq!(
            recovered[6]["content"],
            format!("{}{}", "x".repeat(17), "y".repeat(17))
        );
        assert_eq!(recovered[5]["replayThrough"], recovered[6]["replayThrough"]);
        run.emit(json!({"type":"text_delta","segmentId":"second","delta":"z"}))
            .await;
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next["type"], "text_delta");
        assert_eq!(next["delta"], "z");
        assert!(
            next["eventSequence"].as_u64().unwrap()
                > recovered[6]["replayThrough"].as_u64().unwrap()
        );
    }

    #[tokio::test]
    async fn finished_reconnect_starts_first_and_retains_tools_segments_and_task_outcome() {
        let run = RequestRun::test_run(2);
        run.emit(json!({"type":"started"})).await;
        run.emit(json!({"type":"assistant_segment","segment":segment("first",1,1,"streaming",""),"toolStreamProtocol":3})).await;
        for _ in 0..12 {
            run.emit(json!({"type":"text_delta","segmentId":"first","delta":"x"}))
                .await;
        }
        let first = segment("first", 1, 1, "completed", &"x".repeat(12));
        run.emit(json!({"type":"assistant_segment","segment":first,"toolStreamProtocol":3}))
            .await;
        run.emit(json!({"type":"tool_call","tool":tool("running")}))
            .await;
        let mut receipt = tool("completed");
        receipt["result"] = json!("temporary observation");
        run.emit(json!({"type":"tool_result","tool":receipt})).await;
        run.emit(json!({"type":"assistant_segment","segment":segment("second",2,3,"streaming",""),"toolStreamProtocol":3})).await;
        let mut final_segment = segment("second", 2, 3, "completed", "final");
        final_segment["phase"] = json!("final_answer");
        run.emit(
            json!({"type":"assistant_segment","segment":final_segment,"toolStreamProtocol":3}),
        )
        .await;
        run.finish(json!({"type":"completed","content":"final","reasoning":"","assistantSegments":[first,final_segment],"toolStreamProtocol":3,"taskCompletion":{"completionState":"verification-failed"}})).await;
        let mut stream = RequestEventStream::new(run.clone()).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        assert_eq!(events[0]["type"], "started");
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "tool_call")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "tool_result")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "assistant_segment")
                .count(),
            4
        );
        let final_event = events.last().unwrap();
        assert_eq!(final_event["type"], "completed");
        assert_eq!(
            final_event["taskCompletion"]["completionState"],
            "verification-failed"
        );
        assert_eq!(final_event["assistantSegments"][1]["phase"], "final_answer");
        assert_eq!(
            run.result.lock().await.as_ref().unwrap()["eventSequence"],
            final_event["eventSequence"]
        );
    }

    #[tokio::test]
    async fn nonstream_wait_survives_lag_and_terminal_is_ready_before_wakeup() {
        let run = RequestRun::test_run(2);
        let observer = run.clone();
        let waiting = tokio::spawn(async move { wait_reply(&observer).await });
        tokio::task::yield_now().await;
        run.emit(json!({"type":"started"})).await;
        for _ in 0..32 {
            run.emit(json!({"type":"text_delta","delta":"x"})).await;
        }
        let mut receiver = run.events.subscribe();
        run.emit(json!({"type":"completed","content":"final","reasoning":""}))
            .await;
        let event = receiver.recv().await.unwrap();
        assert!(run.finished.load(Ordering::Acquire));
        assert_eq!(
            run.result.lock().await.as_ref().unwrap()["eventSequence"],
            event["eventSequence"]
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap()
                .unwrap()["content"],
            "final"
        );
        run.finish(json!({"type":"completed","content":"duplicate"}))
            .await;
        assert_eq!(
            run.result.lock().await.as_ref().unwrap()["content"],
            "final"
        );
        assert!(receiver.try_recv().is_err());
        let cancellation = RequestRun::test_run(2);
        cancellation.cancel();
        assert!(*cancellation.cancellation.subscribe().borrow());
    }

    #[tokio::test]
    async fn actual_http_nonstream_failures_keep_machine_errors_and_partial_replies() {
        let app = Router::new().fallback(any(|bytes: Bytes| async move {
            reply_response(Ok(serde_json::from_slice(&bytes).unwrap()))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let response=client.post(format!("http://{address}/api/chat")).json(&json!({"type":"error","code":"CHAT_REQUEST_CONFLICT","error":"temporary conflict","statusCode":409})).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 409);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["code"], "CHAT_REQUEST_CONFLICT");
        let response=client.post(format!("http://{address}/api/chat")).json(&json!({"type":"interrupted","content":"saved partial","reasoning":"","taskCompletion":{"completionState":"partial"},"durationMs":1.0})).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["content"], "saved partial");
        assert_eq!(body["taskCompletion"]["completionState"], "partial");
        assert_eq!(body["durationMs"].as_u64(), Some(1));
        server.abort();
    }
}
