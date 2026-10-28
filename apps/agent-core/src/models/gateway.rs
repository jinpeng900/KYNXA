use std::{
    fmt,
    future::Future,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
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

/// The request owner measures visible text independently of provider usage and keeps it through cancellation.
/// 请求拥有者独立测量可见正文，不信任供应商同名计量；取消或断流后仍保留已观测结果。
#[derive(Default)]
struct ClientTiming {
    dispatched_at: OnceLock<Instant>,
    first_visible_text_ms: OnceLock<u64>,
}

impl ClientTiming {
    fn mark_dispatched(&self) {
        self.dispatched_at.get_or_init(Instant::now);
    }

    fn observe_public_text(&self, event: &Value) {
        if event["type"] != "text_delta"
            || !event["delta"].as_str().is_some_and(|text| !text.trim().is_empty())
            || self.first_visible_text_ms.get().is_some()
        {
            return;
        }
        if let Some(started) = self.dispatched_at.get() {
            let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let _ = self.first_visible_text_ms.set(elapsed);
        }
    }

    fn attach(&self, usage: &mut Value) {
        if !usage.is_object() { *usage = json!({}); }
        usage["_kynxaFirstVisibleTextMs"] = json!(self.first_visible_text_ms.get().copied());
    }
}

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
    fn decoded_tool_truncation_keeps_its_recovery_code_and_finish_diagnostics() {
        let error = json!({"code":"MODEL_TOOL_OUTPUT_TRUNCATED","message":"output limit","type":"interrupted",
            "finishReason":"max_tokens","outputTruncated":true,"hasToolCalls":true,"estimatedGeneratedTokens":2176});
        let failure = ModelError::decoded(&error);
        assert_eq!(failure.code, "MODEL_TOOL_OUTPUT_TRUNCATED");
        assert_eq!(failure.details["decoderCode"], error["code"]);
        for field in ["type", "finishReason", "outputTruncated", "hasToolCalls", "estimatedGeneratedTokens"] {
            assert_eq!(failure.details[field], error[field], "{field}");
        }
        for code in ["MODEL_TOOL_ARGUMENT_INVALID", "MODEL_TOOL_IDENTITY_INVALID", "MODEL_TOOL_CALL_LIMIT"] {
            let failure = ModelError::decoded(&json!({"code":code,"message":"invalid tool","type":"error"}));
            assert_eq!(failure.code, "MODEL_TOOL_CALL_INVALID");
            assert_eq!(failure.details["decoderCode"], code);
        }
    }

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
    async fn incomplete_http_stream_preserves_already_received_numeric_usage_without_response_text() {
        let payload = format!("data: {}\n\n", json!({"choices":[],"usage":{
            "prompt_tokens":100,"completion_tokens":5,"prompt_cache_miss_tokens":20,
            "prompt_tokens_details":{"cached_tokens":80,"cache_write_tokens":10},
            "private_response":"never-copy-this-body"}}));
        let (connection, server) = fixture(payload, "text/event-stream", StatusCode::OK, Duration::ZERO).await;
        let (_owner, cancel) = watch::channel(false);
        let error = request(&connection, json!({"stream":true}), cancel, None).await.unwrap_err();
        assert_eq!(error.details["requestDispatched"], true);
        assert_eq!(error.details["reportedUsage"]["prompt_tokens"], 100);
        assert_eq!(error.details["reportedUsage"]["completion_tokens"], 5);
        assert_eq!(error.details["reportedUsage"]["prompt_tokens_details"]["cached_tokens"], 80);
        assert_eq!(error.details["reportedUsage"]["prompt_tokens_details"]["cache_write_tokens"], 10);
        assert_eq!(error.details["reportedUsage"]["prompt_cache_miss_tokens"], 20);
        assert!(!error.details.to_string().contains("never-copy-this-body"));
        server.abort();
    }

    #[test]
    fn numeric_usage_observer_merges_partial_cache_details_without_copying_response_content() {
        let reported = std::sync::Mutex::new(json!({}));
        observe_provider_usage(&reported, &json!({"input_tokens":100,
            "input_tokens_details":{"cached_tokens":80,"private":"never-copy"}}));
        observe_provider_usage(&reported, &json!({"input_tokens_details":{"cache_write_tokens":10},
            "prompt_cache_miss_tokens":20,"private":"never-copy"}));
        let reported = reported.into_inner().unwrap();
        assert_eq!(reported["input_tokens"],100);
        assert_eq!(reported["input_tokens_details"]["cached_tokens"],80);
        assert_eq!(reported["input_tokens_details"]["cache_write_tokens"],10);
        assert_eq!(reported["prompt_cache_miss_tokens"],20);
        assert!(!reported.to_string().contains("never-copy"));
    }

    #[test]
    fn client_timing_counts_only_first_visible_text_after_actual_dispatch() {
        let timing = ClientTiming::default();
        timing.observe_public_text(&json!({"type":"text_delta","delta":"before dispatch"}));
        let mut projected = json!({"_kynxaFirstVisibleTextMs":999999});
        timing.attach(&mut projected);
        assert!(projected["_kynxaFirstVisibleTextMs"].is_null());
        timing.mark_dispatched();
        for event in [json!({"type":"reasoning_delta","delta":"private thinking"}),
            json!({"type":"tool_call_delta","delta":"arguments"}),
            json!({"type":"completed","delta":"terminal"}),
            json!({"type":"content_snapshot","content":"complete body"}),
            json!({"type":"text_delta","delta":" \n\t"}),
            json!({"type":"text_delta","delta":""})] {
            timing.observe_public_text(&event);
        }
        timing.attach(&mut projected);
        assert!(projected["_kynxaFirstVisibleTextMs"].is_null());
        timing.observe_public_text(&json!({"type":"text_delta","delta":"可见正文"}));
        timing.attach(&mut projected);
        assert!(projected["_kynxaFirstVisibleTextMs"].is_u64());
        let first = projected.clone();
        timing.observe_public_text(&json!({"type":"text_delta","delta":"later text"}));
        timing.attach(&mut projected);
        assert_eq!(projected, first);
    }

    #[tokio::test]
    async fn incomplete_visible_stream_keeps_measured_timing_and_partial_numeric_usage() {
        let payload = sse(&[("",json!({"choices":[{"index":0,"delta":{"reasoning_content":"private thinking"}}]})),
            ("",json!({"choices":[{"index":0,"delta":{"content":"visible output"}}],
                "usage":{"prompt_tokens":100,"prompt_tokens_details":{"cached_tokens":80},"_kynxaFirstVisibleTextMs":999999}})),
            ("",json!({"choices":[],"usage":{"prompt_tokens_details":{"cache_write_tokens":10},"private":"never-copy"}}))]);
        let (connection, server) = fixture(payload, "text/event-stream", StatusCode::OK, Duration::from_millis(25)).await;
        let (_owner, cancel) = watch::channel(false);
        let error = request(&connection,json!({"stream":true}),cancel,None).await.unwrap_err();
        let observed = &error.details["reportedUsage"];
        let first = observed["_kynxaFirstVisibleTextMs"].as_u64().unwrap();
        assert!((20..999999).contains(&first));
        assert_eq!(observed["prompt_tokens"],100);
        assert_eq!(observed["prompt_tokens_details"]["cached_tokens"],80);
        assert_eq!(observed["prompt_tokens_details"]["cache_write_tokens"],10);
        assert!(!error.details.to_string().contains("private thinking"));
        assert!(!error.details.to_string().contains("visible output"));
        assert!(!error.details.to_string().contains("never-copy"));
        server.abort();
    }

    #[tokio::test]
    async fn non_streaming_snapshots_and_tool_only_streams_have_unknown_first_visible_text_time() {
        let non_stream = json!({"choices":[{"message":{"role":"assistant","content":"complete body"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":5,"_kynxaFirstVisibleTextMs":999999}}).to_string();
        let tool_stream = format!("{}data: [DONE]\n\n",sse(&[("",json!({"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"id":"call-1","type":"function","function":{"name":"read_data","arguments":"{}"}}]}}]})),
            ("",json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":3,"completion_tokens":5,"_kynxaFirstVisibleTextMs":999999}}))]));
        for (payload, content_type, streaming) in [(non_stream,"application/json",false),(tool_stream,"text/event-stream",true)] {
            let (connection,server)=fixture(payload,content_type,StatusCode::OK,Duration::ZERO).await;
            let (_owner,cancel)=watch::channel(false);
            let body=json!({"stream":streaming,"tools":[{"type":"function","function":{"name":"read_data","parameters":{"type":"object","properties":{}}}}]});
            let response=request(&connection,body,cancel,None).await.unwrap();
            assert!(response.usage["_kynxaFirstVisibleTextMs"].is_null());
            assert_eq!(response.raw["usage"]["_kynxaFirstVisibleTextMs"],999999);
            if streaming { assert_eq!(response.calls.len(),1); }
            server.abort();
        }
    }

    #[tokio::test]
    async fn cancellation_after_visible_text_keeps_the_observed_time_without_reissuing_a_request() {
        let payload=sse(&[("",json!({"choices":[{"index":0,"delta":{"content":"visible before cancellation"}}]}))]);
        let server=Router::new().fallback(move || {
            let payload=payload.clone();
            async move {
                let stream=futures_util::stream::once(async move { Ok::<_,std::convert::Infallible>(payload) })
                    .chain(futures_util::stream::pending::<Result<String,std::convert::Infallible>>());
                HttpResponse::builder().status(StatusCode::OK).header(CONTENT_TYPE,"text/event-stream")
                    .body(Body::from_stream(stream)).unwrap()
            }
        });
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection=json!({"protocol":"openai-completions","baseUrl":format!("http://{}",listener.local_addr().unwrap())});
        let server=tokio::spawn(async move { axum::serve(listener,server).await.unwrap(); });
        let (owner,cancel)=watch::channel(false);
        let (events,mut received)=mpsc::channel(8);
        let operation=request(&connection,json!({"stream":true}),cancel,Some(events));
        tokio::pin!(operation);
        let event=tokio::select! {
            event=received.recv()=>event.unwrap(),
            result=&mut operation=>panic!("request ended before cancellation: {result:?}"),
        };
        assert_eq!(event["type"],"text_delta");
        owner.send_replace(true);
        let error=operation.await.unwrap_err();
        assert_eq!(error.code,"ABORT_ERR");
        assert_eq!(error.details["requestDispatched"],true);
        assert!(error.details["reportedUsage"]["_kynxaFirstVisibleTextMs"].is_u64());
        assert!(!error.details.to_string().contains("visible before cancellation"));
        server.abort();
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
                fixture(payload, content_type, StatusCode::OK, Duration::from_millis(25)).await;
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
            assert!(response.usage["_kynxaFirstVisibleTextMs"].as_u64().is_some_and(|time| time >= 20));
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
    async fn an_active_stream_outlives_the_start_deadline_but_a_stalled_one_stops() {
        // Each chunk arrives within idleTimeoutMs while the whole stream lasts longer than timeoutMs.
        // 每块都在 idleTimeoutMs 内到达，但整段流长于 timeoutMs；持续有效的输出不能被开始期限截断。
        async fn stream_fixture(gap: Duration) -> (Value, tokio::task::JoinHandle<()>) {
            let server = Router::new().fallback(move || async move {
                let chunks = (0..6).map(move |index| {
                    let data = if index < 5 {
                        format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":"a"}}]}))
                    } else {
                        format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}))
                    };
                    (data, gap)
                });
                let body = futures_util::stream::iter(chunks).then(|(data, gap)| async move {
                    tokio::time::sleep(gap).await;
                    Ok::<_, std::convert::Infallible>(data)
                });
                HttpResponse::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(body))
                    .unwrap()
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let connection = json!({"providerId":"fixture","protocol":"openai-completions",
                "baseUrl":format!("http://{}",listener.local_addr().unwrap()),"apiKey":"fixture-secret",
                "timeoutMs":300,"idleTimeoutMs":250});
            let handle = tokio::spawn(async move {
                axum::serve(listener, server).await.unwrap();
            });
            (connection, handle)
        }
        let (connection, server) = stream_fixture(Duration::from_millis(100)).await;
        let (_owner, cancel) = watch::channel(false);
        let response = request(&connection, json!({"model":"fixture","stream":true}), cancel, None)
            .await
            .unwrap();
        assert_eq!(response.content, "aaaaa");
        server.abort();
        let (connection, server) = stream_fixture(Duration::from_millis(400)).await;
        let (_owner, cancel) = watch::channel(false);
        let error = request(&connection, json!({"model":"fixture","stream":true}), cancel, None)
            .await
            .unwrap_err();
        assert!(matches!(error.code.as_str(), "MODEL_RESPONSE_TIMEOUT" | "MODEL_IDLE_TIMEOUT"), "{}", error.code);
        server.abort();
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
        // Output-limit recovery needs its precise code; other invalid calls retain their shared recovery class.
        // 输出额度恢复需要精确错误码；其他无效调用仍保持原有的统一恢复类别。
        let mut result = Self::new(
            if decoder_code.starts_with("MODEL_TOOL_") && decoder_code != "MODEL_TOOL_OUTPUT_TRUNCATED" {
                "MODEL_TOOL_CALL_INVALID"
            } else {
                decoder_code
            },
            error["message"].as_str().unwrap_or("模型回复未完整结束。"),
            502,
        );
        result.details = json!({"type":error["type"],"decoderCode":decoder_code,"contractCode":error["contractCode"],"estimatedGeneratedTokens":error["estimatedGeneratedTokens"],"requestDispatched":false});
        for field in ["finishReason", "outputTruncated", "hasToolCalls"] {
            if let Some(value) = error.get(field) {
                result.details[field] = value.clone();
            }
        }
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
            // Cancel the request when its owner closes, so the HTTP body is not orphaned.
            // 所有者关闭时取消请求，避免 HTTP 正文无人处理。
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
    let streaming = AtomicBool::new(false);
    let reported_usage = std::sync::Mutex::new(Value::Null);
    let client_timing = ClientTiming::default();
    // timeoutMs covers startup; an active stream uses cancellation and per-chunk idle checks.
    // timeoutMs 只约束响应开始；流启动后按取消和逐块闲置检查处理，不截断持续输出。
    let deadline = async {
        tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        if streaming.load(Ordering::Acquire) {
            std::future::pending::<()>().await;
        }
    };
    let result = tokio::select! {
        biased;
        _ = cancelled(&mut lifetime) => Err(ModelError::cancelled()),
        result = execute_request(connection, body, &mut cancellation, events, catalog, &dispatched, &streaming, &reported_usage, &client_timing) => result,
        _ = deadline => Err(ModelError::new("MODEL_RESPONSE_TIMEOUT", "模型请求超时，已保留先前执行记录。", 504)),
    };
    let observed = reported_usage.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    match result {
        Ok(mut response) => {
            // Later usage may omit cache counts; retain observed whitelisted numbers, without copying content.
            // 后续计量可能省略缓存数量；保留已观测的白名单数值，不复制正文。
            if !response.usage.is_object() { response.usage = json!({}); }
            for (key, value) in observed.as_object().into_iter().flatten() {
                if matches!(key.as_str(), "prompt_tokens_details" | "input_tokens_details") {
                    if !response.usage[key].is_object() { response.usage[key] = json!({}); }
                    for (field, value) in value.as_object().into_iter().flatten() {
                        response.usage[key][field] = value.clone();
                    }
                } else { response.usage[key] = value.clone(); }
            }
            client_timing.attach(&mut response.usage);
            Ok(response)
        }
        Err(mut error) => {
            if !error.details.is_object() { error.details = json!({}); }
            error.details["requestDispatched"] = json!(dispatched.load(Ordering::Relaxed));
            let mut usage = observed;
            client_timing.attach(&mut usage);
            error.details["reportedUsage"] = usage;
            Err(error)
        }
    }
}

/// Retain observed numeric usage through cancellation or failure; exclude response content.
/// 取消或失败后保留已观测的数值计量；诊断不保存响应正文。
fn observe_provider_usage(target: &std::sync::Mutex<Value>, usage: &Value) {
    if !usage.is_object() { return; }
    let mut selected = target.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for name in ["prompt_tokens", "completion_tokens", "input_tokens", "output_tokens", "cache_read_input_tokens",
        "cache_creation_input_tokens", "prompt_cache_hit_tokens", "prompt_cache_miss_tokens", "prompt_eval_count", "eval_count"] {
        if let Some(value) = usage[name].as_u64() {
            if !selected.is_object() { *selected = json!({}); }
            selected[name] = json!(value);
        }
    }
    for name in ["prompt_tokens_details", "input_tokens_details"] {
        for field in ["cached_tokens", "cache_write_tokens"] {
            if let Some(value) = usage[name][field].as_u64() {
                if !selected.is_object() { *selected = json!({}); }
                if !selected[name].is_object() { selected[name] = json!({}); }
                selected[name][field] = json!(value);
            }
        }
    }
}

async fn execute_request(
    connection: &Value,
    mut body: Value,
    cancellation: &mut watch::Receiver<bool>,
    events: Option<mpsc::Sender<Value>>,
    catalog: &[Value],
    dispatched: &AtomicBool,
    streaming: &AtomicBool,
    reported_usage: &std::sync::Mutex<Value>,
    client_timing: &ClientTiming,
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
    // Count locally with the real template/tokenizer; oversized requests return for compaction before generation.
    // 用实际模板和分词器本地计数；超限先返回压缩，不发起生成。
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
            &mut body,
            measured.input_tokens,
            measured.context_window_tokens,
        )?;
    }
    let client = client()?;
    let mut request = authenticated(&client, connection, Method::POST, &url)
        .header(header::ACCEPT, "text/event-stream, application/json");
    if let Some(beta) = super::tool_updates::beta_header(connection, &body) {
        request = request.header("anthropic-beta", beta);
    }
    let request = request
        .json(&body)
        .build()
        .map_err(|_| ModelError::new("INVALID_MODEL_REQUEST", "模型请求参数或连接头无效。", 400))?;
    let sending = client.execute(request);
    tokio::pin!(sending);
    // Count an HTTP attempt only when its future is polled, including failed network attempts.
    // 只有 HTTP future 实际被轮询才计一次尝试；网络失败仍属于尝试，不表示供应商收费。
    let sending = std::future::poll_fn(|context| {
        dispatched.store(true, Ordering::Relaxed);
        client_timing.mark_dispatched();
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
        observe_provider_usage(reported_usage, &rejected["usage"]);
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
        observe_provider_usage(reported_usage, if protocol == "ollama" { &raw } else { &raw["usage"] });
        let result = gateway_protocol::decode_response(protocol, raw, catalog, Value::Null)?;
        observe_provider_usage(reported_usage, &result.usage);
        if let Some(events) = events {
            events.send(json!({"type":"content_snapshot","content":result.content,"reasoning":result.reasoning})).await
            .map_err(|_| ModelError::cancelled())?;
        }
        return Ok(result);
    }
    let mut frames = FrameDecoder::default();
    let mut decoder = ProviderStream::new(protocol)?;
    let mut stream = response.bytes_stream();
    streaming.store(true, Ordering::Release);
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
            let decoded = decoder.frame(&frame);
            observe_provider_usage(reported_usage, &decoder.usage);
            let decoded = decoded?;
            for event in decoded["events"].as_array().into_iter().flatten() {
                client_timing.observe_public_text(event);
                if let Some(events) = &events {
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
