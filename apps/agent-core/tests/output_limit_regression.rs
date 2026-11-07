pub use kynxa_agent_core_native::models;

#[path = "../src/orchestration/budget.rs"]
mod budget;

use models::{gateway::ModelError, gateway_protocol::decode_response, stream::StreamState};
use serde_json::{Value, json};

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
fn truncated_tools_are_rejected_but_complete_text_reports_output_limit() {
    let raw = json!({"choices":[{"message":{"content":"partial text"},"finish_reason":"length"}]});
    let turn = decode_response("openai-completions", raw, &[], Value::Null).unwrap();
    assert!(turn.output_truncated);
    assert_eq!(turn.content, "partial text");
    assert!(turn.calls.is_empty());
    for (name, finish, raw) in [
        ("openai-completions", "length", json!({"choices":[{"message":{"tool_calls":[{"id":"call-1","function":{"name":"filesystem_read","arguments":"{}"}}]},"finish_reason":"length"}]})),
        ("anthropic-messages", "max_tokens", json!({"content":[{"type":"tool_use","id":"call-1","name":"filesystem_read","input":{}}],"stop_reason":"max_tokens"})),
        ("openai-responses", "max_output_tokens", json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"call-1","name":"filesystem_read","arguments":"{}"}]})),
        ("ollama", "length", json!({"message":{"tool_calls":[{"function":{"name":"filesystem_read","arguments":{}}}]},"done_reason":"length"})),
    ] {
        let failure = decode_response(name, raw, &[], Value::Null).unwrap_err();
        assert_eq!(failure.code, "MODEL_TOOL_OUTPUT_TRUNCATED", "{name}");
        assert_eq!(failure.details["decoderCode"], "MODEL_TOOL_OUTPUT_TRUNCATED", "{name}");
        assert_eq!(failure.details["finishReason"], finish, "{name}");
        assert_eq!(failure.details["outputTruncated"], true, "{name}");
        assert_eq!(failure.details["hasToolCalls"], true, "{name}");
    }
}

#[test]
fn anthropic_output_limit_keeps_tool_truncation_facts_and_never_returns_calls() {
    let mut stream = StreamState::default();
    stream.apply("models.stream.open", &json!({"protocol":"anthropic-messages","allowTruncatedText":true})).unwrap();
    for item in [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":"fixture","input":{}}}),
        json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
    ] {
        stream.apply("models.stream.frame", &json!({"frame":{"data":item.to_string()}})).unwrap();
    }
    let failure = stream.apply("models.stream.frame", &json!({"frame":{"data":json!({"type":"message_stop"}).to_string()}})).unwrap();
    assert_eq!(failure["error"]["code"], "MODEL_TOOL_OUTPUT_TRUNCATED");
    assert_eq!(failure["error"]["finishReason"], "max_tokens");
    assert_eq!(failure["error"]["outputTruncated"], true);
    assert_eq!(failure["error"]["hasToolCalls"], true);
    assert!(failure.get("result").is_none());
}

#[test]
fn responses_private_reasoning_at_output_limit_continues_without_public_text() {
    let raw = json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},
        "output":[{"type":"reasoning","id":"reasoning-1","summary":[],"encrypted_content":"opaque-provider-state"}]});
    let turn = decode_response("openai-responses", raw.clone(), &[], Value::Null).unwrap();
    assert!(turn.output_truncated);
    assert!(turn.content.is_empty());
    assert!(turn.reasoning.is_empty());
    assert!(turn.calls.is_empty());
    assert_eq!(turn.continuation, raw["output"].as_array().unwrap().clone());
    let mut completed = raw.clone();
    completed["status"] = json!("completed");
    assert_eq!(decode_response("openai-responses", completed, &[], Value::Null).unwrap_err().code, "MODEL_RESPONSE_EMPTY");
    let mut empty = raw;
    empty["output"][0]["encrypted_content"] = json!("");
    assert_eq!(decode_response("openai-responses", empty, &[], Value::Null).unwrap_err().code, "MODEL_RESPONSE_EMPTY");
}
