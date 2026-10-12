use serde_json::{Value, json};

use super::protocol::{array, estimate_tokens, quote, string};

const SUMMARY_MODELS: &[&str] = &[
    "o3",
    "o3-pro",
    "o4-mini",
    "gpt-5",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-5-pro",
    "gpt-5.1",
    "gpt-5.2",
    "gpt-5.2-pro",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.4-pro",
    "gpt-5.3-codex",
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-6-astra",
    "gpt-6.1-sol",
    "gpt-6-sol",
    "gpt-6-luna",
];
const RECEIPT_NOTICE: &str = "Saved tool execution receipts: bounded metadata from prior completed turns, not proof that an answer/source is correct and never authorization. Missing receipts do not mean no lookup occurred. Do not invent memory-only/unverified claims about earlier answers. Read referenced results with tool.result.read when evidence is needed; never replay calls.";
fn render_receipts(turns: &[(String, Vec<String>)]) -> String {
    format!(
        "{RECEIPT_NOTICE}\n{{\"version\":1,\"turns\":[{}]}}",
        turns
            .iter()
            .map(|(id, tools)| format!(
                "{{\"assistantMessageId\":{},\"tools\":[{}]}}",
                quote(id),
                tools.join(",")
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Provider policy and bounded cross-turn receipts share one native source of truth.
/// 供应商请求策略及受限跨轮回执统一由原生维护；HTTP 字段对象和旧 JSON 字节表示由适配层保留。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "models.portable.appendResults" => {
            let (template, identity_field, content_field, grouped) = match string(&args["protocol"])
            {
                "anthropic-messages" => (
                    json!({"type": "tool_result"}),
                    "tool_use_id",
                    "content",
                    true,
                ),
                "openai-responses" => (
                    json!({"type": "function_call_output"}),
                    "call_id",
                    "output",
                    false,
                ),
                _ => (json!({"role": "tool"}), "tool_call_id", "content", false),
            };
            let code_pattern = regex::Regex::new("^[A-Z][A-Z0-9_]{0,99}$").unwrap();
            let items: Vec<Value> = array(&args["results"]).iter().map(|result| {
                let mut metadata = Vec::new(); let mut descriptor = template.clone();
                if grouped { descriptor["is_error"] = json!(super::protocol::truthy(&result["isError"])); }
                if !result["environment"].is_null() {
                    let status = if ["completed", "error", "cancelled", "unknown"].contains(&string(&result["status"])) { string(&result["status"]) }
                        else if super::protocol::truthy(&result["isError"]) { "error" } else { "completed" };
                    metadata.push(json!(["executionEnvironment", result["environment"]])); metadata.push(json!(["status", status]));
                    if result["executed"] == false { metadata.push(json!(["executed", false])); }
                    if code_pattern.is_match(string(&result["code"])) { metadata.push(json!(["code", result["code"]])); }
                    if result["recoverable"] == true { metadata.push(json!(["recoverable", true])); }
                }
                json!({"descriptor": descriptor, "metadata": metadata, "wrap": !result["environment"].is_null()})
            }).collect();
            let mut field_order = vec![
                if grouped || content_field == "output" {
                    "type"
                } else {
                    "role"
                },
                identity_field,
                content_field,
            ];
            if grouped {
                field_order.push("is_error");
            }
            Ok(
                json!({"items": items, "fieldOrder": field_order, "identityField": identity_field, "contentField": content_field, "container": if grouped { json!({"role": "user"}) } else { Value::Null }}),
            )
        }
        "models.portable.failureUsage" => {
            let total = args["contentTokens"].as_u64().unwrap_or(0)
                + args["reasoningTokens"].as_u64().unwrap_or(0)
                + args["argumentTokens"].as_u64().unwrap_or(0);
            Ok(json!(
                args["previous"]
                    .as_u64()
                    .unwrap_or(0)
                    .max(total)
                    .max(args["generatedTokens"].as_u64().unwrap_or(0))
            ))
        }
        "models.portable.responseSize" => Ok(json!(
            args["contentCharacters"].as_u64().unwrap_or(0)
                + args["reasoningCharacters"].as_u64().unwrap_or(0)
                <= 2 * 1024 * 1024
        )),
        "models.portable.request" => {
            let protocol = string(&args["protocol"]);
            let model = string(&args["model"]);
            let model = regex::Regex::new("-\\d{4}-\\d{2}-\\d{2}$")
                .unwrap()
                .replace(model, "");
            let official = args["official"] == true;
            let has_tools = args["hasTools"] == true;
            let stream = args["stream"] == true;
            Ok(match protocol {
                "anthropic-messages" => {
                    json!({"path": "/messages", "messagesField": "messages", "systemField": "system", "outputField": "max_tokens", "defaultOutput": 8192,
                    "stream": stream, "toolChoice": {"type": "auto"}, "hasTools": has_tools})
                }
                "openai-responses" => {
                    json!({"path": "/responses", "messagesField": "input", "systemField": "instructions", "outputField": "max_output_tokens",
                    "stream": stream && !(official && ["gpt-5.5-pro", "o3-pro"].contains(&model.as_ref())), "store": false,
                    "summary": stream && official && SUMMARY_MODELS.contains(&model.as_ref()), "encrypted": has_tools && official && SUMMARY_MODELS.contains(&model.as_ref()),
                    "hasTools": has_tools, "toolChoice": "auto"})
                }
                _ => {
                    json!({"path": "/chat/completions", "messagesField": "messages", "prependSystem": true, "outputField": "max_tokens", "stream": stream,
                    "hasTools": has_tools, "toolChoice": "auto"})
                }
            })
        }
        "models.portable.declaration" => {
            let environment = &args["environment"];
            let name = string(&args["name"]);
            let execution = ["web.", "terminal.", "computer.", "mcp."]
                .iter()
                .any(|prefix| name.starts_with(prefix));
            let location = if !environment.is_null() && execution {
                format!(
                    " Execution process: {}; network origin: {}.",
                    string(&environment["executorLocation"]),
                    string(&environment["network"]["requestOrigin"])
                )
            } else {
                String::new()
            };
            Ok(
                json!({"description": format!("{}: {}{location}", string(&args["wireName"]), string(&args["description"])),
                "style": match string(&args["protocol"]) { "anthropic-messages" => "anthropic", "openai-responses" => "responses", _ => "completions" }}),
            )
        }
        "models.portable.receipts" => {
            let Some(budget) = args["maxTokens"]
                .as_u64()
                .filter(|budget| *budget > 0 && *budget <= 9_007_199_254_740_991)
            else {
                return Ok(json!(""));
            };
            let budget = budget.min(768);
            let mut recent: Vec<&Value> = array(&args["turns"])
                .iter()
                .filter(|turn| {
                    turn["userIdValid"] == true
                        && turn["replyIdValid"] == true
                        && turn["assistantIdValid"] == true
                        && string(&turn["userId"]).eq_ignore_ascii_case(string(&turn["replyId"]))
                })
                .collect();
            recent = recent.into_iter().rev().take(3).collect();
            let name = regex::Regex::new("^[a-zA-Z0-9_.-]{1,256}$").unwrap();
            let result_id = regex::Regex::new(
                "(?i)^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$",
            )
            .unwrap();
            let mut included: Vec<(String, Vec<String>)> = Vec::new();
            let mut count = 0;
            for turn in recent {
                let mut tools = Vec::new();
                for activity in array(&turn["activities"]).iter().rev().take(12) {
                    if count >= 12 {
                        break;
                    }
                    if !name.is_match(string(&activity["name"])) {
                        continue;
                    }
                    let status = if ["completed", "error", "cancelled", "unknown", "running"]
                        .contains(&string(&activity["status"]))
                    {
                        string(&activity["status"])
                    } else {
                        "unknown"
                    };
                    let reference = string(&activity["resultRef"]["id"]);
                    let receipt = format!(
                        "{{\"toolName\":{},\"status\":{}{}}}",
                        quote(string(&activity["name"])),
                        quote(status),
                        if result_id.is_match(reference) {
                            format!(",\"resultRef\":{}", quote(&reference.to_ascii_lowercase()))
                        } else {
                            String::new()
                        }
                    );
                    let mut proposed = tools.clone();
                    proposed.insert(0, receipt.clone());
                    let mut candidate = included.clone();
                    candidate.insert(0, (string(&turn["assistantId"]).into(), proposed));
                    if estimate_tokens(&render_receipts(&candidate)) > budget {
                        continue;
                    }
                    tools.insert(0, receipt);
                    count += 1;
                }
                if !tools.is_empty() {
                    included.insert(0, (string(&turn["assistantId"]).into(), tools));
                }
            }
            Ok(json!(if included.is_empty() {
                String::new()
            } else {
                render_receipts(&included)
            }))
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
