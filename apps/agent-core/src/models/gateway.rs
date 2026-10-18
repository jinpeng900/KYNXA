use std::{
    fmt,
    future::Future,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{Client, Method, Response, header};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

pub use super::gateway_protocol::{ModelResponse, append_results, build_request};
use super::{
    gateway_protocol,
    gateway_stream::{FrameDecoder, ProviderStream},
    recovery,
};

const MAXIMUM_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 180_000;

#[derive(Clone, Debug)]
pub struct ModelError {
    pub code: String,
    pub message: String,
    pub status_code: u16,
    pub details: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_connection_and_retryable_provider_rejections_remain_distinct() {
        for (status, body, code) in [
            (402, json!({"error":{"message":"private balance detail"}}), "MODEL_CREDIT_EXHAUSTED"),
            (429, json!({"error":{"code":"insufficient_quota"}}), "MODEL_CREDIT_EXHAUSTED"),
            (401, json!({"error":{"message":"private key detail"}}), "MODEL_AUTHENTICATION_FAILED"),
            (429, json!({"error":{"code":"rate_limit_exceeded"}}), "MODEL_RATE_LIMITED"),
            (400, json!({"error":{"message":"Unsupported parameter private detail"}}), "MODEL_REQUEST_REJECTED"),
            (404, json!({}), "MODEL_ENDPOINT_NOT_FOUND"),
        ] {
            let error = upstream_rejection(status, &body);
            assert_eq!(error.code, code);
            assert_eq!(error.details["upstreamStatus"], status);
            assert!(!error.message.contains("private"));
            assert!(!error.details.to_string().contains("private"));
        }
    }
    use axum::{
        Router,
        body::Body,
        http::{StatusCode, header::CONTENT_TYPE},
        response::Response as HttpResponse,
    };
    async fn fixture(
        payload: String,
        content_type: &'static str,
        status: StatusCode,
        delay: Duration,
    ) -> (Value, tokio::task::JoinHandle<()>) {
        let server = Router::new().fallback(move || {
            let payload = payload.clone();
            async move {
                tokio::time::sleep(delay).await;
                HttpResponse::builder()
                    .status(status)
                    .header(CONTENT_TYPE, content_type)
                    .body(Body::from(payload))
                    .unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection = json!({"providerId":"fixture","protocol":"openai-completions","baseUrl":format!("http://{}",listener.local_addr().unwrap()),"apiKey":"fixture-secret"});
        let handle = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        (connection, handle)
    }
    fn sse(events: &[(&str, Value)]) -> String {
        events
            .iter()
            .map(|(name, value)| format!("event: {name}\ndata: {value}\n\n"))
            .collect()
    }
    #[tokio::test]
    async fn actual_http_streams_keep_provider_identity_usage_and_incremental_text() {
        let fixtures = [
            (
                "openai-completions",
                "text/event-stream",
                format!(
                    "{}data: [DONE]\n\n",
                    sse(&[
                        (
                            "",
                            json!({"id":"chat-1","choices":[{"delta":{"content":"hello"},"finish_reason":null}]})
                        ),
                        (
                            "",
                            json!({"id":"chat-1","choices":[{"delta":{},"finish_reason":"stop"}]})
                        ),
                        (
                            "",
                            json!({"id":"chat-1","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":5}})
                        )
                    ])
                ),
            ),
            (
                "anthropic-messages",
                "text/event-stream",
                sse(&[
                    (
                        "message_start",
                        json!({"type":"message_start","message":{"id":"anthropic-1","usage":{"input_tokens":3}}}),
                    ),
                    (
                        "content_block_start",
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    ),
                    (
                        "content_block_delta",
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
                    ),
                    (
                        "content_block_stop",
                        json!({"type":"content_block_stop","index":0}),
                    ),
                    (
                        "message_delta",
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
                    ),
                    ("message_stop", json!({"type":"message_stop"})),
                ]),
            ),
            (
                "openai-responses",
                "text/event-stream",
                sse(&[
                    (
                        "response.output_text.delta",
                        json!({"type":"response.output_text.delta","delta":"hello"}),
                    ),
                    (
                        "response.completed",
                        json!({"type":"response.completed","response":{"id":"responses-1","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}],"usage":{"input_tokens":3,"output_tokens":5}}}),
                    ),
                ]),
            ),
            (
                "ollama",
                "application/x-ndjson",
                format!(
                    "{}\n{}\n",
                    json!({"message":{"role":"assistant","content":"hello"},"done":false}),
                    json!({"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":3,"eval_count":5})
                ),
            ),
        ];
        for (name, content_type, payload) in fixtures {
            let (mut connection, server) =
                fixture(payload, content_type, StatusCode::OK, Duration::ZERO).await;
            connection["protocol"] = json!(name);
            let (_owner, cancel) = watch::channel(false);
            let (events, mut received) = mpsc::channel(32);
            let response = request(
                &connection,
                json!({"model":"temporary","stream":true}),
                cancel,
                Some(events),
            )
            .await
            .unwrap();
            assert_eq!(response.content, "hello");
            assert_eq!(
                response.usage[if name == "openai-completions" || name == "ollama" {
                    "completion_tokens"
                } else {
                    "output_tokens"
                }],
                5
            );
            if name != "ollama" {
                assert!(response.raw["id"].is_string());
            }
            let mut text = String::new();
            while let Ok(event) = received.try_recv() {
                if event["type"] == "text_delta" {
                    text.push_str(event["delta"].as_str().unwrap());
                }
            }
            assert_eq!(text, "hello");
            server.abort();
        }
    }
    #[tokio::test]
    async fn grammar_and_context_http_rejections_have_distinct_recovery_details() {
        for (message, code, field, absent) in [
            (
                "failed to parse grammar: number of repetitions exceeds sane defaults fixture-secret",
                "MODEL_LOCAL_TOOL_GRAMMAR_REJECTED",
                "toolGrammarRejection",
                "contextRejection",
            ),
            (
                "maximum context length is 4096 tokens fixture-secret",
                "MODEL_CONTEXT_LIMIT_REJECTED",
                "contextRejection",
                "toolGrammarRejection",
            ),
        ] {
            let (connection, server) = fixture(
                json!({"error":{"message":message}}).to_string(),
                "application/json",
                StatusCode::BAD_REQUEST,
                Duration::ZERO,
            )
            .await;
            let (_owner, cancel) = watch::channel(false);
            let error = request(
                &connection,
                json!({"model":"fixture","stream":false}),
                cancel,
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, code);
            assert!(error.details[field].is_object());
            assert!(error.details.get(absent).is_none());
            assert_eq!(error.details["upstreamStatus"], 400);
            assert_eq!(error.details["requestDispatched"], true);
            assert!(!error.details.to_string().contains("fixture-secret"));
            assert!(!error.message.contains("fixture-secret"));
            server.abort();
        }
    }

    #[tokio::test]
    async fn llama_context_and_image_http_refusals_keep_safe_recovery_facts() {
        for (message, expected_code) in [
            ("request (12163 tokens) exceeds the available context size (8192 tokens), try increasing it fixture-secret", "MODEL_CONTEXT_LIMIT_REJECTED"),
            ("This model does not support image inputs fixture-secret", "MODEL_IMAGE_INPUT_REJECTED"),
            ("Unsupported parameter: fixture-secret", "MODEL_REQUEST_REJECTED"),
        ] {
            let (connection, server) = fixture(
                json!({"error":{"message":message}}).to_string(),
                "application/json",
                StatusCode::BAD_REQUEST,
                Duration::ZERO,
            ).await;
            let (_owner, cancel) = watch::channel(false);
            let error = request(&connection, json!({"model":"fixture","stream":false}), cancel, None)
                .await.unwrap_err();
            assert_eq!(error.code, expected_code);
            assert_eq!(error.status_code, 502);
            assert_eq!(error.details["upstreamStatus"], 400);
            assert_eq!(error.details["requestDispatched"], true);
            assert!(!error.details.to_string().contains("fixture-secret"));
            assert!(!error.message.contains("fixture-secret"));
            match expected_code {
                "MODEL_CONTEXT_LIMIT_REJECTED" => {
                    assert_eq!(error.details["contextRejection"]["contextWindowTokens"], 8192);
                    assert_eq!(error.details["contextRejection"]["requestTokens"], 12163);
                    assert!(error.details.get("imageRejection").is_none());
                }
                "MODEL_IMAGE_INPUT_REJECTED" => {
                    assert!(super::super::message_attachments::image_rejection(
                        error.status_code, &error.message, &error.details));
                    assert!(error.details.get("contextRejection").is_none());
                }
                _ => {
                    assert!(error.details.get("imageRejection").is_none());
                    assert!(error.details.get("contextRejection").is_none());
                }
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn request_cancel_timeout_and_error_bodies_preserve_private_boundaries() {
        let (connection, server) = fixture(
            "{}".into(),
            "application/json",
            StatusCode::OK,
            Duration::from_secs(5),
        )
        .await;
        let (owner, cancel) = watch::channel(false);
        let task_connection = connection.clone();
        let request =
            tokio::spawn(async move { request(&task_connection, json!({}), cancel, None).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        owner.send(true).unwrap();
        assert_eq!(request.await.unwrap().unwrap_err().code, "ABORT_ERR");
        let mut connection = connection;
        connection["timeoutMs"] = json!(100);
        let (_owner, cancel) = watch::channel(false);
        assert_eq!(
            super::request(&connection, json!({}), cancel, None)
                .await
                .unwrap_err()
                .code,
            "MODEL_RESPONSE_TIMEOUT"
        );
        server.abort();
        let (connection, server) = fixture(
            json!({"error":{"message":"fixture-secret private upstream body"}}).to_string(),
            "application/json",
            StatusCode::UNAUTHORIZED,
            Duration::ZERO,
        )
        .await;
        let (_owner, cancel) = watch::channel(false);
        let failure = super::request(&connection, json!({}), cancel, None)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "MODEL_AUTHENTICATION_FAILED");
        assert_eq!(failure.details["upstreamStatus"], 401);
        assert!(!failure.message.contains("fixture-secret"));
        assert!(!failure.details.to_string().contains("fixture-secret"));
        server.abort();
    }

    #[tokio::test]
    async fn actual_json_http_responses_decode_all_four_protocols() {
        let fixtures = [
            (
                "openai-completions",
                json!({"id":"chat","choices":[{"message":{"content":"hello"},"finish_reason":"stop"}],"usage":{"completion_tokens":5}}),
            ),
            (
                "anthropic-messages",
                json!({"id":"anthropic","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"output_tokens":5}}),
            ),
            (
                "openai-responses",
                json!({"id":"responses","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}],"usage":{"output_tokens":5}}),
            ),
            (
                "ollama",
                json!({"model":"fixture","message":{"role":"assistant","content":"hello"},"done":true,"done_reason":"stop","eval_count":5}),
            ),
        ];
        for (name, payload) in fixtures {
            let (mut connection, server) = fixture(
                payload.to_string(),
                "application/json",
                StatusCode::OK,
                Duration::ZERO,
            )
            .await;
            connection["protocol"] = json!(name);
            let (_owner, cancel) = watch::channel(false);
            let response = super::request(&connection, json!({"stream":false}), cancel, None)
                .await
                .unwrap();
            assert_eq!(response.content, "hello");
            assert_eq!(
                response.usage[if name == "openai-completions" || name == "ollama" {
                    "completion_tokens"
                } else {
                    "output_tokens"
                }],
                5
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn actual_streamed_turns_share_the_durable_call_limit_before_returning() {
        for count in [
            super::super::protocol::MAX_TURN_CALLS,
            super::super::protocol::MAX_TURN_CALLS + 1,
        ] {
            let calls: Vec<Value> = (0..count).map(|index|json!({"id":format!("call-{index}"),"index":index,"type":"function","function":{"name":"filesystem_read","arguments":"{}"}})).collect();
            let anthropic: Vec<_> = (0..count).flat_map(|index|[("content_block_start",json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":format!("call-{index}"),"name":"filesystem_read","input":{}}})),("content_block_stop",json!({"type":"content_block_stop","index":index}))]).chain([("message_delta",json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}})),("message_stop",json!({"type":"message_stop"}))]).collect();
            let responses: Vec<_> = (0..count).map(|index|json!({"type":"function_call","call_id":format!("call-{index}"),"name":"filesystem_read","arguments":"{}"})).collect();
            let ollama: Vec<_> = (0..count).map(|index|json!({"function":{"name":"filesystem_read","arguments":{"index":index}}})).collect();
            let fixtures = [
                (
                    "openai-completions",
                    "text/event-stream",
                    format!(
                        "{}data: [DONE]\n\n",
                        sse(&[
                            (
                                "",
                                json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":null}]})
                            ),
                            (
                                "",
                                json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
                            )
                        ])
                    ),
                ),
                ("anthropic-messages", "text/event-stream", sse(&anthropic)),
                (
                    "openai-responses",
                    "text/event-stream",
                    sse(&[(
                        "response.completed",
                        json!({"type":"response.completed","response":{"status":"completed","output":responses}}),
                    )]),
                ),
                (
                    "ollama",
                    "application/x-ndjson",
                    format!(
                        "{}\n",
                        json!({"message":{"role":"assistant","content":"","tool_calls":ollama},"done":true,"done_reason":"stop"})
                    ),
                ),
            ];
            for (name, content_type, payload) in fixtures {
                let (mut connection, server) =
                    fixture(payload, content_type, StatusCode::OK, Duration::ZERO).await;
                connection["protocol"] = json!(name);
                let (_owner, cancel) = watch::channel(false);
                let catalog = [json!({"name":"filesystem.read","wireName":"filesystem_read"})];
                let turn = request_with_catalog(
                    &connection,
                    json!({"model":"fixture","stream":true}),
                    cancel,
                    None,
                    &catalog,
                )
                .await;
                if count == super::super::protocol::MAX_TURN_CALLS {
                    let turn = turn.unwrap();
                    assert_eq!(turn.calls.len(), count, "{name}");
                    let results:Vec<_>=turn.calls.iter().map(|call|json!({"call":call,"result":{"status":"completed","content":"temporary observation"}})).collect();
                    let continued = append_results(name, &[], &turn, &results).unwrap();
                    assert!(super::super::protocol::complete_pairs(&continued), "{name}");
                } else {
                    let failure = turn.unwrap_err();
                    assert_eq!(failure.code, "MODEL_TOOL_CALL_INVALID", "{name}");
                    assert_eq!(failure.details["requestDispatched"], true, "{name}");
                    assert_eq!(
                        failure.details["decoderCode"], "MODEL_TOOL_CALL_LIMIT",
                        "{name}"
                    );
                }
                server.abort();
            }
        }
    }

    #[tokio::test]
    async fn request_attempts_exclude_local_rejection_and_initial_cancel() {
        let (_owner, cancel) = watch::channel(false);
        let failure = request(
            &json!({"baseUrl":"invalid address"}),
            json!({}),
            cancel,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.code, "INVALID_MODEL_CONNECTION");
        assert_eq!(failure.details["requestDispatched"], false);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection = json!({"protocol":"openai-completions","baseUrl":format!("http://{}",listener.local_addr().unwrap())});
        let mut invalid_headers = connection.clone();
        invalid_headers["apiKey"] = json!("temporary\ninvalid-header");
        let (_owner, cancel) = watch::channel(false);
        let failure = request(&invalid_headers, json!({}), cancel, None)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "INVALID_MODEL_REQUEST");
        assert_eq!(failure.details["requestDispatched"], false);
        let (_owner, cancel) = watch::channel(true);
        let failure = request(&connection, json!({}), cancel, None)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "ABORT_ERR");
        assert_eq!(failure.details["requestDispatched"], false);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );

        let address = listener.local_addr().unwrap();
        let dropped = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
        });
        let (_owner, cancel) = watch::channel(false);
        let failure = request(
            &json!({"protocol":"openai-completions","baseUrl":format!("http://{address}")}),
            json!({}),
            cancel,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.code, "MODEL_NETWORK_FAILED");
        assert_eq!(failure.details["requestDispatched"], true);
        dropped.await.unwrap();
    }
}
impl ModelError {
    pub fn new(code: &str, message: &str, status_code: u16) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            status_code,
            details: json!({"requestDispatched":false}),
        }
    }
    pub fn native(code: String) -> Self {
        Self::new(&code, "模型协议处理失败。", 502)
    }
    pub fn decoded(error: &Value) -> Self {
        let decoder_code = error["code"]
            .as_str()
            .unwrap_or("MODEL_RESPONSE_INCOMPLETE");
        // Invalid model calls are one pre-dispatch recovery class; preserve the precise decoder reason.
        // 无效模型调用统一进入派发前恢复类别；精确解码原因仍保留在诊断中。
        let mut result = Self::new(
            if decoder_code.starts_with("MODEL_TOOL_") {
                "MODEL_TOOL_CALL_INVALID"
            } else {
                decoder_code
            },
            error["message"].as_str().unwrap_or("模型回复未完整结束。"),
            502,
        );
        result.details = json!({"type":error["type"],"decoderCode":decoder_code,"contractCode":error["contractCode"],"estimatedGeneratedTokens":error["estimatedGeneratedTokens"],"requestDispatched":false});
        result
    }
    pub fn cancelled() -> Self {
        Self::new("ABORT_ERR", "模型请求已取消。", 499)
    }
}
impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for ModelError {}

/// Preserve only bounded recovery facts from provider refusals; raw error bodies remain private.
/// 供应商拒绝仅保留受限恢复事实；原始错误正文不进入公开诊断或模型上下文。
pub(crate) fn upstream_rejection(status: u16, rejected: &Value) -> ModelError {
    let provider_code = rejected["error"]["code"].as_str()
        .or_else(|| rejected["error"]["type"].as_str()).unwrap_or("").to_ascii_lowercase();
    let exhausted = status == 402 || matches!(provider_code.as_str(),
        "insufficient_quota" | "insufficient_balance" | "balance_insufficient"
            | "credit_balance_too_low" | "billing_hard_limit_reached");
    let rejection = recovery::calculate(
        "models.recovery.contextRejection",
        &json!({"body":rejected}),
    )
    .unwrap_or(Value::Null);
    let grammar = super::protocol::calculate(
        "models.protocol.grammarRejection",
        &json!({"status":status,"body":rejected}),
    )
    .unwrap_or(Value::Null);
    let image = super::message_attachments::classify_image_rejection(status, rejected);
    let mut error = if exhausted {
        ModelError::new("MODEL_CREDIT_EXHAUSTED", "模型账户余额或可用额度不足。", 402)
    } else if matches!(status, 401 | 403) {
        ModelError::new("MODEL_AUTHENTICATION_FAILED", "模型连接认证未通过，请检查连接配置。", 502)
    } else if status == 404 {
        ModelError::new("MODEL_ENDPOINT_NOT_FOUND", "模型接口或所选模型不可用。", 502)
    } else if !grammar.is_null() {
        ModelError::new(
            "MODEL_LOCAL_TOOL_GRAMMAR_REJECTED",
            "模型无法解析本次工具声明；此步骤未派发工具。",
            502,
        )
    } else if !rejection.is_null() {
        ModelError::new(
            "MODEL_CONTEXT_LIMIT_REJECTED",
            "模型拒绝了超出实际上下文额度的请求。",
            502,
        )
    } else if !image.is_null() {
        ModelError::new(
            "MODEL_IMAGE_INPUT_REJECTED",
            "模型拒绝了本次图片输入，将保留原附件并尝试文字解析。",
            502,
        )
    } else if status == 429 {
        ModelError::new("MODEL_RATE_LIMITED", "模型服务暂时繁忙，等待后继续。", 429)
    } else if matches!(status, 400 | 413 | 415 | 422) {
        ModelError::new("MODEL_REQUEST_REJECTED", "正在调整模型请求的兼容格式。", 502)
    } else {
        ModelError::new(
            "MODEL_HTTP_ERROR",
            &format!("模型接口返回 HTTP {status}。"),
            502,
        )
    };
    error.details["upstreamStatus"] = json!(status);
    if !grammar.is_null() {
        error.details["toolGrammarRejection"] = grammar;
    } else if !rejection.is_null() {
        error.details["contextRejection"] = rejection;
    } else if !image.is_null() {
        error.details["imageRejection"] = image;
    }
    error
}

pub(crate) fn client() -> Result<Client, ModelError> {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(300))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .map_err(|_| ModelError::new("MODEL_HTTP_CLIENT_FAILED", "无法初始化模型通信。", 500))?;
    let _ = CLIENT.set(client.clone());
    Ok(CLIENT.get().cloned().unwrap_or(client))
}

pub(crate) fn authenticated(
    client: &Client,
    connection: &Value,
    method: Method,
    url: &str,
) -> reqwest::RequestBuilder {
    let mut request = client.request(method, url);
    let key = connection["apiKey"].as_str().unwrap_or("");
    if gateway_protocol::protocol(connection) == "anthropic-messages" {
        request = request.header("anthropic-version", "2023-06-01");
        if !key.is_empty() {
            request = request.header("x-api-key", key);
        }
    } else if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    request
}

pub(crate) async fn cancelled(cancellation: &mut watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow() {
            return;
        }
        if cancellation.changed().await.is_err() {
            // Closing the lifecycle owner cancels a request instead of silently orphaning the HTTP body.
            // 生命周期拥有者关闭时取消请求，不能让 HTTP 正文成为无人管理的任务。
            return;
        }
    }
}

pub(crate) async fn bounded_json(
    response: Response,
    maximum_bytes: usize,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<Value, ModelError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let next = tokio::select! { _ = cancelled(cancellation) => return Err(ModelError::cancelled()), next = stream.next() => next };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|_| {
            ModelError::new("MODEL_RESPONSE_READ_FAILED", "读取模型响应失败。", 502)
        })?;
        if bytes.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(ModelError::new(
                "MODEL_RESPONSE_SIZE_LIMIT",
                "模型响应超过容量。",
                502,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| ModelError::new("MODEL_INVALID_JSON", "模型接口返回无效 JSON。", 502))
}

pub async fn request(
    connection: &Value,
    body: Value,
    cancellation: watch::Receiver<bool>,
    events: Option<mpsc::Sender<Value>>,
) -> Result<ModelResponse, ModelError> {
    let catalog = gateway_protocol::body_catalog(&body);
    request_with_catalog(connection, body, cancellation, events, &catalog).await
}

/// One Rust future owns the socket, bounded response and cancellation; no SDK subprocess participates.
/// 一个 Rust future 拥有连接、有界响应和取消生命周期；不经过 SDK 子进程、Node 或 NAPI。
pub async fn request_with_catalog(
    connection: &Value,
    body: Value,
    mut cancellation: watch::Receiver<bool>,
    events: Option<mpsc::Sender<Value>>,
    catalog: &[Value],
) -> Result<ModelResponse, ModelError> {
    let timeout_ms = connection["timeoutMs"]
        .as_u64()
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(100, 3_600_000);
    let mut lifetime = cancellation.clone();
    let dispatched = AtomicBool::new(false);
    let result = tokio::select! {
        biased;
        _ = cancelled(&mut lifetime) => Err(ModelError::cancelled()),
        result = tokio::time::timeout(Duration::from_millis(timeout_ms), execute_request(connection, body, &mut cancellation, events, catalog, &dispatched)) =>
            result.unwrap_or_else(|_| Err(ModelError::new("MODEL_RESPONSE_TIMEOUT", "模型请求超时，已保留先前执行记录。", 504))),
    };
    result.map_err(|mut error| {
        if !error.details.is_object() {
            error.details = json!({});
        }
        error.details["requestDispatched"] = json!(dispatched.load(Ordering::Relaxed));
        error
    })
}

async fn execute_request(
    connection: &Value,
    body: Value,
    cancellation: &mut watch::Receiver<bool>,
    events: Option<mpsc::Sender<Value>>,
    catalog: &[Value],
    dispatched: &AtomicBool,
) -> Result<ModelResponse, ModelError> {
    if *cancellation.borrow() {
        return Err(ModelError::cancelled());
    }
    let protocol = gateway_protocol::protocol(connection);
    let endpoint = connection["baseUrl"]
        .as_str()
        .ok_or_else(|| ModelError::new("INVALID_MODEL_CONNECTION", "模型连接地址缺失。", 400))?;
    let endpoint = url::Url::parse(endpoint)
        .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "模型连接地址无效。", 400))?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(ModelError::new(
            "INVALID_MODEL_CONNECTION",
            "模型连接地址必须为 HTTP 或 HTTPS。",
            400,
        ));
    }
    let url = if protocol == "ollama" {
        endpoint
            .join("/api/chat")
            .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "本地模型地址无效。", 400))?
            .to_string()
    } else {
        format!(
            "{}{}",
            endpoint.as_str().trim_end_matches('/'),
            gateway_protocol::request_path(connection)
        )
    };
    // Local input_tokens uses the same template and tokenizer without running generation or consuming a model call.
    // 本地计数复用实际模板与分词器，不执行生成；超限交回请求工厂压缩，不把错误请求发给生成接口。
    if let Some(measured) = super::gateway_local_engine::count_request_tokens(
        connection,
        body["model"].as_str().unwrap_or(""),
        &body,
        cancellation.clone(),
    )
    .await?
    {
        gateway_protocol::validate_measured_request(
            connection,
            &body,
            measured.input_tokens,
            measured.context_window_tokens,
        )?;
    }
    let client = client()?;
    let request = authenticated(&client, connection, Method::POST, &url)
        .header(header::ACCEPT, "text/event-stream, application/json")
        .json(&body)
        .build()
        .map_err(|_| ModelError::new("INVALID_MODEL_REQUEST", "模型请求参数或连接头无效。", 400))?;
    let sending = client.execute(request);
    tokio::pin!(sending);
    // Count an HTTP attempt only when its future is polled, including failed network attempts.
    // 只有 HTTP future 实际被轮询才计一次尝试；网络失败仍属于尝试，不表示供应商收费。
    let sending = std::future::poll_fn(|context| {
        dispatched.store(true, Ordering::Relaxed);
        sending.as_mut().poll(context)
    });
    let response = tokio::select! {
        biased;
        _ = cancelled(cancellation) => return Err(ModelError::cancelled()),
        response = sending => response.map_err(|_| ModelError::new("MODEL_NETWORK_FAILED", "无法连接模型服务。", 502))?,
    };
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let rejected = match bounded_json(response, 1024 * 1024, cancellation).await {
            Err(error) if error.code == "ABORT_ERR" => return Err(error),
            result => result.unwrap_or(Value::Null),
        };
        return Err(upstream_rejection(status, &rejected));
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if !content_type.contains("text/event-stream")
        && !(protocol == "ollama" && content_type.contains("ndjson"))
    {
        let raw = bounded_json(response, MAXIMUM_RESPONSE_BYTES, cancellation).await?;
        let result = gateway_protocol::decode_response(protocol, raw, catalog, Value::Null)?;
        if let Some(events) = events {
            events.send(json!({"type":"content_snapshot","content":result.content,"reasoning":result.reasoning})).await
            .map_err(|_| ModelError::cancelled())?;
        }
        return Ok(result);
    }
    let mut frames = FrameDecoder::default();
    let mut decoder = ProviderStream::new(protocol)?;
    let mut stream = response.bytes_stream();
    let mut terminal: Option<Value> = None;
    let idle_ms = connection["idleTimeoutMs"]
        .as_u64()
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(100, 3_600_000);
    loop {
        let chunk = tokio::select! {
            _ = cancelled(cancellation) => return Err(ModelError::cancelled()),
            result = tokio::time::timeout(Duration::from_millis(idle_ms), stream.next()) => result
                .map_err(|_| ModelError::new("MODEL_IDLE_TIMEOUT", "模型流长时间未返回数据。", 504))?,
        };
        let end = chunk.is_none();
        let bytes = chunk
            .transpose()
            .map_err(|_| ModelError::new("MODEL_RESPONSE_READ_FAILED", "模型流连接中断。", 502))?
            .unwrap_or_default();
        for frame in frames.push(&bytes, protocol == "ollama", end)? {
            let decoded = decoder.frame(&frame)?;
            if let Some(events) = &events {
                for event in decoded["events"].as_array().into_iter().flatten() {
                    tokio::select! { _ = cancelled(cancellation) => return Err(ModelError::cancelled()),
                    sent = events.send(event.clone()) => sent.map_err(|_| ModelError::cancelled())? };
                }
            }
            if let Some(error) = decoded.get("error") {
                return Err(ModelError::decoded(error));
            }
            if let Some(raw) = decoded.get("result") {
                if protocol == "openai-completions" && frame["data"] != "[DONE]" {
                    terminal = Some(raw.clone());
                    continue;
                }
                return gateway_protocol::decode_response(
                    protocol,
                    raw.clone(),
                    catalog,
                    decoder.usage.clone(),
                );
            }
        }
        if end {
            if let Some(raw) = terminal {
                return gateway_protocol::decode_response(
                    protocol,
                    raw,
                    catalog,
                    decoder.usage.clone(),
                );
            }
            return Err(decoder.end());
        }
    }
}
