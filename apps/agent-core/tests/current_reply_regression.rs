// Local regression harness: pure production functions below are extracted verbatim.
// The source snapshot tests reject stale extraction after a production source edit.
mod control {
use serde_json::{Value, json};
pub(super) const MAX_COMPLETION_REFERENCES_PER_ITEM: usize = 64;


fn user_work_disposition(value: &Value) -> Result<Option<&str>, ()> {
    let stop = value.get("stop").map(|value| value.as_bool().ok_or(())).transpose()?;
    match value.get("disposition") {
        Some(disposition) => {
            let disposition = disposition.as_str().filter(|disposition|
                matches!(*disposition, "continue" | "deliver-existing" | "stop")).ok_or(())?;
            if stop.is_some_and(|stop| stop != (disposition != "continue")) { return Err(()); }
            Ok(Some(disposition))
        }
        None => Ok(stop.map(|stop| if stop { "stop" } else { "continue" })),
    }
}


fn string_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}


pub(crate) fn review_format_repair(checkpoint: &mut Value, text: &str, output_truncated: bool, output_tokens: u64) -> Value {
    control_format_repair(checkpoint, text, output_truncated, output_tokens, None)
}

pub(crate) fn steering_format_repair(checkpoint: &mut Value, text: &str, output_truncated: bool, output_tokens: u64) -> Value {
    control_format_repair(checkpoint, text, output_truncated, output_tokens, Some("unresolved-user-steering"))
}

fn control_format_repair(checkpoint: &mut Value, text: &str, output_truncated: bool, output_tokens: u64, required_issue: Option<&str>) -> Value {
    let (stage, mut issues, characters) = review_format_diagnostics(text);
    if let Some(issue) = required_issue { issues.push(issue.to_owned()); }
    if output_truncated { issues.push("provider-output-truncated".into()); }
    if issues.is_empty() {
        if let Some(fields) = checkpoint.as_object_mut() { fields.remove("reviewFormatRepair"); }
        return json!({"action":"valid","reason":"Control format is valid."});
    }
    let previous = &checkpoint["reviewFormatRepair"];
    let previous_issues = previous["issues"].as_array().cloned().unwrap_or_default();
    let changed_format = stage > previous["stage"].as_u64().unwrap_or(0)
        || stage == previous["stage"].as_u64().unwrap_or(0)
            && issues.len() < previous_issues.len()
            && issues.iter().all(|issue| previous_issues.contains(&json!(issue)));
    let smaller_footer = characters > 16384 && previous["characters"].as_u64()
        .is_some_and(|before| before > characters);
    let longer_truncated_output = output_truncated && output_tokens > previous["outputTokens"].as_u64().unwrap_or(0);
    let first = !previous.is_object();
    let may_repair = first || changed_format || smaller_footer || longer_truncated_output;
    let high_water_tokens = output_tokens.max(previous["outputTokens"].as_u64().unwrap_or(0));
    let mut saved = previous.clone();
    if first || changed_format || smaller_footer {
        saved = json!({"stage":stage,"issues":issues,"characters":characters});
    }
    saved["outputTokens"] = json!(high_water_tokens);
    if !checkpoint.is_object() { *checkpoint = json!({}); }
    checkpoint["reviewFormatRepair"] = saved;
    let reason = format!("Review control format: {}. {}", issues.join(", "),
        if may_repair { "Return one standalone single-line footer with valid fields; preserve saved work and receipts." }
        else { "The previous correction did not improve this format or its truncated output; saved work awaits user continuation." });
    json!({"action":if may_repair {"repair"} else {"wait"},"reason":reason})
}

/// Put a parseable final control block on its own compact line; a formatting slip should not reject a clear answer.
fn normalize_control_footer(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(start) = text.rfind("<kynxa_task>") else { return text.into(); };
    let (prefix, after) = (&text[..start], &text[start + "<kynxa_task>".len()..]);
    let Some(end) = after.find("</kynxa_task>") else { return text.into(); };
    let body = &after[..end];
    let standalone = prefix.rsplit('\n').next().unwrap_or("").bytes().all(|byte| matches!(byte, b' ' | b'\t'));
    if prefix.rsplit('\n').next().unwrap_or("").trim_start().starts_with('>') { return text.into(); }
    if (standalone && !body.contains('\n')) || in_fence(prefix)
        || !after[end + "</kynxa_task>".len()..].trim_matches(javascript_whitespace).is_empty() { return text.into(); }
    match serde_json::from_str::<Value>(body) {
        Ok(value) if value.is_object() => format!("{}\n<kynxa_task>{value}</kynxa_task>", prefix.trim_end_matches(javascript_whitespace)).into(),
        _ => text.into(),
    }
}

fn review_format_diagnostics(text: &str) -> (u64, Vec<String>, u64) {
    let text = normalize_control_footer(text);
    let text = text.as_ref();
    let Some(start) = text.rfind("<kynxa_task>") else { return (0, vec!["missing-footer".into()], 0); };
    let prefix = &text[..start];
    let after = &text[start + "<kynxa_task>".len()..];
    let Some(end) = after.find("</kynxa_task>") else { return (1, vec!["unclosed-footer".into()], 0); };
    let body = &after[..end];
    let characters = body.chars().count() as u64;
    let mut issues = Vec::new();
    if !prefix.rsplit('\n').next().unwrap_or("").bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        issues.push("footer-not-standalone".into());
    }
    if in_fence(prefix) { issues.push("footer-inside-code-fence".into()); }
    if !after[end + "</kynxa_task>".len()..].trim_matches(javascript_whitespace).is_empty() {
        issues.push("trailing-content-after-footer".into());
    }
    if body.contains('\n') { issues.push("multiline-footer".into()); }
    if characters > 16384 { issues.push("footer-too-large".into()); }
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        issues.push("invalid-json".into()); return (2, issues, characters);
    };
    let Some(object) = value.as_object() else {
        issues.push("footer-not-object".into()); return (3, issues, characters);
    };
    let state = string_field(&value, "state");
    let valid_state = matches!(state, "complete" | "continue" | "blocked");
    if !valid_footer(&value) {
        // Reuse the real schema for each field; prose changes cannot invent repair progress.
        // 逐字段复用真实格式校验；改写正文不能制造格式修复进展。
        for key in ["state", "steeringRevision", "action", "met", "unmet", "blocker", "feasibility", "next", "reason"] {
            if let Some(field) = object.get(key) {
                let mut probe = json!({"state":if valid_state {state} else {"complete"},"next":"current step","reason":"observed condition"});
                probe[key] = field.clone();
                if !valid_footer(&probe) { issues.push(format!("invalid-{key}")); }
            }
        }
        if !valid_state && !object.contains_key("state") { issues.push("missing-state".into()); }
        if state == "continue" && string_field(&value, "next").trim_matches(javascript_whitespace).is_empty() {
            issues.push("missing-next".into());
        }
        if state == "blocked" && string_field(&value, "reason").trim_matches(javascript_whitespace).is_empty() {
            issues.push("missing-reason".into());
        }
        if user_work_disposition(&value).is_err() { issues.push("invalid-user-control".into()); }
    }
    let stage = if !valid_state {4} else if valid_footer(&value) {6} else {5};
    (stage, issues, characters)
}

fn valid_footer(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let state = string_field(value, "state");
    matches!(state, "continue" | "complete" | "blocked")
        // Unknown extension fields cannot erase recognized control; only known fields are consumed.
        // 扩展字段不能让已识别的控制失效，执行端只消费已知字段。
        && user_work_disposition(value).is_ok()
        && object.get("steeringRevision").is_none_or(Value::is_object)
        && object.get("action").is_none_or(|action| {
            state == "continue" && action.as_object().is_some_and(|fields| {
                fields.len() == 2 && action["arguments"].is_object()
                    && action["name"].as_str().is_some_and(|name| !name.is_empty() && name.len() <= 256)
            })
        })
        && object.get("met").is_none_or(valid_met)
        && object.get("unmet").is_none_or(|unmet| {
            unmet.as_array().is_some_and(|items| {
                items.len() <= 128
                    && items.iter().all(|item| {
                        item.as_str()
                            .is_some_and(|text| text.encode_utf16().count() <= 256)
                    })
            })
        })
        && object.get("blocker").is_none_or(|blocker| {
            matches!(
                blocker.as_str(),
                Some("user-input" | "authorization" | "external" | "resource" | "difficulty" | "impossible" | "unsupported")
            )
        })
        && object.get("feasibility").is_none_or(|assessment| assessment.is_object()
            && ["basis", "alternatives"].iter().all(|field| assessment[*field].as_str()
                .is_some_and(|text| !text.trim().is_empty() && text.encode_utf16().count() <= 2048)))
        && ["next", "reason"].iter().all(|key| {
            object.get(*key).is_none_or(|item| {
                item.as_str()
                    .is_some_and(|text| text.encode_utf16().count() <= 512)
            })
        })
        && (state != "continue"
            || !string_field(value, "next")
                .trim_matches(javascript_whitespace)
                .is_empty())
        && (state != "blocked"
            || !string_field(value, "reason")
                .trim_matches(javascript_whitespace)
                .is_empty())
}

/// Each claimed requirement names its evidence: tool call IDs or artifact paths from this task.
/// 每项已满足的要求都要列出证据：本任务真实执行的调用 ID 或产物路径。
fn valid_met(met: &Value) -> bool {
    met.as_array().is_some_and(|items| {
        items.len() <= 128
            && items.iter().all(|item| {
                item.as_object().is_some_and(|object| {
                    object.keys().all(|key| matches!(key.as_str(), "item" | "evidence" | "requirementId"))
                        && object.get("requirementId").is_none_or(|id| id.as_str().is_some_and(|id| {
                            id.strip_prefix("requirement-").and_then(|number| number.parse::<usize>().ok()).is_some_and(|number| number > 0)
                        }))
                        && item["item"].as_str().is_some_and(|text| text.encode_utf16().count() <= 256)
                        && item["evidence"].as_array().is_some_and(|evidence| {
                            evidence.len() <= MAX_COMPLETION_REFERENCES_PER_ITEM
                                && evidence.iter().all(|entry| {
                                    entry.as_str().is_some_and(|text| text.encode_utf16().count() <= 512)
                                })
                        })
                })
            })
    })
}


fn in_fence(content: &str) -> bool {
    content
        .split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .filter(|line| {
            let line = line.trim_start_matches([' ', '\t']);
            line.starts_with("```") || line.starts_with("~~~")
        })
        .count()
        % 2
        != 0
}


fn javascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}


#[test]
fn unresolved_steering_requires_binding_even_when_footer_syntax_is_valid() {
    let mut checkpoint = json!({});
    let text = "请松开按键后再告诉我。\n<kynxa_task>{\"state\":\"complete\",\"unmet\":[]}</kynxa_task>";
    assert_eq!(steering_format_repair(&mut checkpoint, text, false, 40)["action"], "repair");
    assert!(checkpoint["reviewFormatRepair"]["issues"].as_array().unwrap().contains(&json!("unresolved-user-steering")));
    let repeated = text.replace("请松开按键后再告诉我。", "松开按键后回我一句，我立刻继续画。");
    assert_eq!(steering_format_repair(&mut checkpoint, &repeated, false, 80)["action"], "wait");
    assert!(checkpoint["reviewFormatRepair"]["issues"].as_array().unwrap().contains(&json!("unresolved-user-steering")));
}

#[test]
fn steering_syntax_improvement_repairs_without_treating_prose_changes_as_work() {
    let mut checkpoint = json!({});
    assert_eq!(steering_format_repair(&mut checkpoint, "已有进度保留。", false, 20)["action"], "repair");
    let valid = "已有进度保留。\n<kynxa_task>{\"state\":\"complete\",\"disposition\":\"continue\"}</kynxa_task>";
    assert_eq!(steering_format_repair(&mut checkpoint, valid, false, 40)["action"], "repair");
    assert_eq!(checkpoint["reviewFormatRepair"]["issues"], json!(["unresolved-user-steering"]));
    assert_eq!(steering_format_repair(&mut checkpoint, valid, false, 40)["action"], "wait");
    // Once the caller has bound the user directive, ordinary format validation clears the repair state.
    assert_eq!(review_format_repair(&mut checkpoint, valid, false, 40)["action"], "valid");
    assert!(checkpoint.get("reviewFormatRepair").is_none());
}
}


#[test]
fn extracted_control_source_matches_the_current_production_snapshot() {
    use sha2::{Digest, Sha256};
    assert_eq!(format!("{:x}", Sha256::digest(include_str!("../src/orchestration/task.rs").as_bytes())), "baf911b19e6eda8b61c61b347dfc97934cd66a86271d14a226f2bd04a9016328");
}

mod delivery {
use kynxa_agent_core_native::{gateway_contract::GatewayError, orchestration::progress::RunProgress};
use serde_json::{Value, json};
use std::time::Instant;
#[derive(Default)]
struct DeliveryState {
    segments: Vec<Value>,
    pending_delivery: String,
    delivery_continuing: bool,
    last_progress_ms: u64,
    model_started: Option<Instant>,
    generation_duration_base_ms: Option<u64>,
    timeline_order: u64,
}


fn current_public_reply(error: &GatewayError, assessment: &Value, delivery: &DeliveryState, rounds: &[Value]) -> Option<usize> {
    if !delivery.pending_delivery.is_empty() || delivery.delivery_continuing { return None; }
    let is_public_reply = |segment: &Value, round: &Value| {
        segment["phase"] == "commentary" && segment["status"] == "completed"
            && segment["isAnswerContinuation"] != true
            && segment["round"].is_u64() && segment["round"] == round["round"]
            && segment["content"].as_str().is_some_and(|text| !text.trim().is_empty()
                && round["text"].as_str() == Some(text))
            && round["calls"].as_array().is_some_and(Vec::is_empty)
            && round.get("unexecutedCallCount").is_none_or(|count| count.as_u64() == Some(0))
    };
    if error.code == "TASK_COMPLETION_REVIEW_INVALID" {
        // Internal review can follow a public reply, but cannot replace it or become the reply itself.
        // 内部核验可在公开回答之后继续；不能用核验文字或空段替换已公开的正常回答。
        delivery.segments.iter().enumerate().rev().find_map(|(index, segment)| {
            let round = rounds.iter().rev().find(|round| round["round"] == segment["round"])?;
            (is_public_reply(segment, round) && round["reviewing"] == false
                && round["outputTruncated"] == false && round["taskState"] == "unknown"
                && round["taskStateSource"] == "legacy-response").then_some(index)
        })
    } else if error.code == "TASK_CONTINUATION_NO_PROGRESS"
        && assessment["state"] == "unknown" && assessment["source"] == "legacy-response" {
        let index = delivery.segments.len().checked_sub(1)?;
        let (segment, round) = (&delivery.segments[index], rounds.last()?);
        (is_public_reply(segment, round) && segment["content"] == assessment["content"]
            && round["reviewing"] != true && round["outputTruncated"] != true).then_some(index)
    } else { None }
}


    fn published_explanation() -> (Value, DeliveryState, Vec<Value>) {
        let text = "画板绘制尚未完成。请松开按键，再告诉我，我会继续。";
        (json!({"state":"unknown","source":"legacy-response","content":text}),
            DeliveryState { segments:vec![json!({"id":"current-reply","round":28,"phase":"commentary",
                "status":"completed","content":text})], ..Default::default() },
            vec![json!({"round":28,"text":text,"calls":[],"reviewing":false,"outputTruncated":false,
                "taskState":"unknown","taskStateSource":"legacy-response"})])
    }

    #[test]
    fn current_public_explanation_survives_stopped_work_without_certifying_completion() {
        for code in ["TASK_CONTINUATION_NO_PROGRESS", "TASK_COMPLETION_REVIEW_INVALID"] {
            let error = GatewayError::new(code, "后续处理未能继续推进，已有结果和进度已保留。", 409);
            let (assessment, delivery, mut rounds) = published_explanation();
            assert_eq!(current_public_reply(&error, &assessment, &delivery, &rounds), Some(0));
            rounds[0]["unexecutedCallCount"] = json!(0);
            assert_eq!(current_public_reply(&error, &assessment, &delivery, &rounds), Some(0));
            let completion = RunProgress::default().apply("progress.completion", &json!({
                "interrupted":true,"code":code,"modelTaskState":assessment,
                "requiresVerification":true,"acceptanceReviewed":false})).unwrap();
            assert_eq!(completion["completionState"], "partial");
            assert_eq!(completion["acceptanceReviewed"], false);
            assert_eq!(completion["correctnessCertified"], false);
        }
    }

    #[test]
    fn stopped_work_does_not_promote_buffered_stale_tool_or_truncated_drafts() {
        for boundary in ["buffered", "continuing", "truncated", "interrupted", "stale", "tools", "complete", "review",
            "internal", "output-limit", "unexecuted", "declared-complete", "declared-source"] {
            let (mut assessment, mut delivery, mut rounds) = published_explanation();
            match boundary {
                "buffered" => delivery.pending_delivery = "未经验收的旧完成草稿".into(),
                "continuing" => delivery.delivery_continuing = true,
                "truncated" => delivery.segments[0]["isAnswerContinuation"] = json!(true),
                "interrupted" => delivery.segments[0]["status"] = json!("interrupted"),
                "stale" => rounds[0]["round"] = json!(29),
                "tools" => rounds[0]["calls"] = json!([{"name":"computer.click"}]),
                "complete" => assessment["state"] = json!("complete"),
                "review" => assessment["source"] = json!("model-task-state"),
                "internal" => rounds[0]["reviewing"] = json!(true),
                "output-limit" => rounds[0]["outputTruncated"] = json!(true),
                "unexecuted" => rounds[0]["unexecutedCallCount"] = json!(1),
                "declared-complete" => rounds[0]["taskState"] = json!("complete"),
                "declared-source" => rounds[0]["taskStateSource"] = json!("model-task-state"),
                _ => unreachable!(),
            }
            let code = if matches!(boundary, "complete" | "review") { "TASK_CONTINUATION_NO_PROGRESS" }
                else { "TASK_COMPLETION_REVIEW_INVALID" };
            let error = GatewayError::new(code, "控制格式未能修复。", 409);
            assert_eq!(current_public_reply(&error, &assessment, &delivery, &rounds), None, "{boundary}");
        }
        let (assessment, delivery, rounds) = published_explanation();
        let decoder_error = GatewayError::new("MODEL_TOOL_OUTPUT_TRUNCATED", "回复达到模型输出上限。", 502);
        assert_eq!(current_public_reply(&decoder_error, &assessment, &delivery, &rounds), None);
    }

    #[test]
    fn invalid_control_review_reuses_public_answer_before_an_empty_review_segment() {
        let error = GatewayError::new("TASK_COMPLETION_REVIEW_INVALID", "控制格式未能修复。", 409);
        let (mut assessment, mut delivery, mut rounds) = published_explanation();
        assessment["content"] = json!("Internal review control text must not replace the public answer.");
        delivery.segments.push(json!({"id":"empty-review","round":29,"phase":"commentary",
            "status":"completed","content":""}));
        rounds.push(json!({"round":29,"text":assessment["content"],"calls":[],
            "reviewing":true,"outputTruncated":false}));
        assert_eq!(current_public_reply(&error, &assessment, &delivery, &rounds), Some(0));
        assert_eq!(delivery.segments[0]["content"], "画板绘制尚未完成。请松开按键，再告诉我，我会继续。");
        delivery.segments[1]["content"] = assessment["content"].clone();
        assert_eq!(current_public_reply(&error, &assessment, &delivery, &rounds), Some(0));
        let no_progress = GatewayError::new("TASK_CONTINUATION_NO_PROGRESS", "尚未取得新进展。", 409);
        assert_eq!(current_public_reply(&no_progress, &assessment, &delivery, &rounds), None);
    }

    
}

#[test]
fn extracted_delivery_source_matches_the_current_production_snapshot() {
    use sha2::{Digest, Sha256};
    assert_eq!(format!("{:x}", Sha256::digest(include_str!("../src/orchestration/gateway_loop/delivery.rs").as_bytes())), "c0c5339d03e02bdb5e987d609469876ac0d7f7f5b171a35bf834cdd063ac4d2d");
    assert_eq!(format!("{:x}", Sha256::digest(include_str!("../src/orchestration/gateway_loop.rs").as_bytes())), "d1f09be6f0673c5a2f06bee9359f2719f73848aed096a35e630ca9abf5cec063");
}
