use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use super::{ContractError, ContractResult, trim, utf16_length};

const SPACE: &str = r"[\x09-\x0d\x20\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000\ufeff]";

fn regex(pattern: &str) -> Regex {
    let non_space = format!("[^{}]", &SPACE[1..SPACE.len() - 1]);
    let pattern = pattern
        .replace(r"[\s\S]", r"(?s:.)")
        .replace(r"\s", SPACE)
        .replace(r"\S", &non_space);
    Regex::new(&pattern).expect("static memory expression")
}
fn instruction(message: &str) -> Option<(&'static str, &'static str)> {
    static COMPLETE: OnceLock<Regex> = OnceLock::new();
    static CHINESE: OnceLock<Regex> = OnceLock::new();
    static ENGLISH: OnceLock<Regex> = OnceLock::new();
    let complete = COMPLETE.get_or_init(|| regex(r"^\s*(?:(全局|用户|聊天|项目|工作)\s*)?(?:任务(?:已)?完成|已完成任务|这项任务完成)(?:\s*[:：，,]?\s*[\s\S]*)$"));
    let chinese = CHINESE.get_or_init(|| regex(r"^\s*(?:(全局|用户|聊天|项目|工作)\s*)?(记住(?:这个)?|更正|纠正|修正|决定|决策|约定|我们决定|最终决定|任务完成|任务已完成|这项任务完成|已完成任务|记忆更正)\s*[:：，,]?\s*(\S[\s\S]*)$"));
    let english = ENGLISH.get_or_init(|| regex(r"(?i)^\s*(remember|correction|correct this|decision|we decided|task completed)\s*[:：,]?\s*(\S[\s\S]*)$"));
    let scope = |scope: &str| match scope {
        "全局" | "用户" => "user",
        "项目" | "工作" => "project",
        _ => "chat",
    };
    if let Some(captures) = complete.captures(message) {
        return Some((
            scope(captures.get(1).map(|value| value.as_str()).unwrap_or("")),
            "task-complete",
        ));
    }
    if let Some(captures) = chinese.captures(message) {
        let action = captures.get(2).unwrap().as_str();
        return Some((
            scope(captures.get(1).map(|value| value.as_str()).unwrap_or("")),
            if action.contains("记住") {
                "remember"
            } else if ["更正", "纠正", "修正"]
                .iter()
                .any(|value| action.contains(value))
            {
                "correction"
            } else if action.contains("完成") {
                "task-complete"
            } else {
                "decision"
            },
        ));
    }
    english.captures(message).map(|captures| {
        let action = captures.get(1).unwrap().as_str().to_ascii_lowercase();
        (
            "chat",
            if action.contains("remember") {
                "remember"
            } else if action.contains("correct") {
                "correction"
            } else if action.contains("completed") {
                "task-complete"
            } else {
                "decision"
            },
        )
    })
}
fn eligible(message: &Value) -> bool {
    static EXCLUDED: OnceLock<Regex> = OnceLock::new();
    let pattern = EXCLUDED.get_or_init(|| regex(r"(?im)(?:```|~~~|^\s*>|\[KYNXA_|https?://|(?:网页|工具|模型|助手)(?:说|回复|输出|返回)|(?:quoted|tool output|web page)\s*[:：])"));
    message["eligibleLength"] == true
        && message["text"].as_str().is_some_and(|text| {
            !trim(text).is_empty()
                && utf16_length(text) <= 4000
                && !text.contains('\0')
                && !pattern.is_match(text)
        })
}
fn proposed(
    scope: &str,
    trigger: &str,
    indices: &[usize],
    messages: &[Value],
    checkpoint: Option<&Value>,
) -> Value {
    static PREFERENCE: OnceLock<Regex> = OnceLock::new();
    let preference = PREFERENCE.get_or_init(|| regex(r"(?i)(?:偏好|喜欢|习惯|prefer|usually)"));
    let content = indices
        .iter()
        .map(|index| trim(messages[*index]["text"].as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut result = json!({"scope": scope, "trigger": trigger, "indices": indices,
        "kind": if trigger == "decision" { "decision" } else if preference.is_match(&content) { "preference" } else { "fact" }});
    if let Some(checkpoint) = checkpoint {
        result["batchThroughMessageId"] = checkpoint.clone();
    }
    result
}

/// Only saved user-message indices are returned; original quotes and their byte-identical hashes remain in the adapter.
/// 只返回已保存用户消息的下标；原话及其字节一致的哈希仍留在适配器，助手内容仅用于批次节奏。
pub fn extract(args: &Value) -> ContractResult {
    let phase = args["phase"].as_str().unwrap_or("completed");
    let error = |message| ContractError::new(message, "INVALID_MEMORY", 400);
    if !["user", "completed"].contains(&phase) {
        return Err(error("候选提取阶段无效。"));
    }
    if !args["taskCompleted"].is_boolean() {
        return Err(error("任务阶段完成标记无效。"));
    }
    let settings = &args["settings"];
    if settings["enabled"] == false {
        return Ok(
            json!({"candidates": [], "reason": "disabled", "completeTurns": 0, "estimatedTokens": 0}),
        );
    }
    let messages = args["messages"]
        .as_array()
        .ok_or_else(|| error("候选来源须为已保存的用户消息。"))?;
    let users = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message["role"] == "user")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    // Completed assistant events omit their user identity or send null; both select the latest saved user.
    // 助手完成事件可缺省用户身份或发送 null；两者均选择最后一条已保存用户消息。
    let current_id = args.get("userMessageId").filter(|id| !id.is_null());
    let current = current_id
        .map(|id| {
            users
                .iter()
                .copied()
                .find(|index| &messages[*index]["id"] == id)
        })
        .unwrap_or_else(|| users.last().copied());
    if current_id.is_some() && current.is_none() {
        return Err(error("候选来源须为已保存的用户消息。"));
    }
    let skipped_overlong_messages = users
        .iter()
        .filter(|index| {
            let message = &messages[**index];
            message["textLengthUtf16"]
                .as_u64()
                .unwrap_or_else(|| utf16_length(message["text"].as_str().unwrap_or("")) as u64)
                > 4000
        })
        .count();
    let consumed: HashSet<&str> = args["consumedMessageIds"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let allowed = |index: usize| {
        !consumed.contains(messages[index]["id"].as_str().unwrap_or(""))
            && eligible(&messages[index])
    };
    let mut candidates = Vec::new();
    if let Some(index) = current {
        if allowed(index) {
            if let Some((scope, trigger)) =
                instruction(messages[index]["text"].as_str().unwrap_or(""))
            {
                if scope != "project" || args["hasProject"] == true {
                    candidates.push(proposed(scope, trigger, &[index], messages, None));
                }
            }
        }
    }
    if phase == "user" {
        return Ok(
            json!({"reason": if candidates.is_empty() { "no-trigger" } else { "explicit-user-trigger" }, "candidates": candidates, "completeTurns": 0, "estimatedTokens": 0, "skippedOverlongMessages": skipped_overlong_messages}),
        );
    }
    let mut complete = Vec::new();
    let mut pending: Option<(usize, bool, u64)> = None;
    for (index, message) in messages.iter().enumerate() {
        if message["role"] == "user" {
            if let Some((index, true, tokens)) = pending {
                complete.push((index, tokens));
            }
            pending = Some((
                index,
                false,
                message["estimatedTokens"].as_u64().unwrap_or(0),
            ));
        } else if let Some((index, completed, tokens)) = pending.as_mut() {
            if message["role"] == "assistant"
                && message["status"] == "completed"
                && message["hasContent"] == true
            {
                *completed = true;
                *tokens += message["estimatedTokens"].as_u64().unwrap_or(0);
                let _ = index;
            }
        }
    }
    if let Some((index, true, tokens)) = pending {
        complete.push((index, tokens));
    }
    if let Some(index) = current {
        if args["taskCompleted"] == true
            && candidates.is_empty()
            && allowed(index)
            && complete.iter().any(|(user, _)| *user == index)
        {
            candidates.push(proposed("chat", "task-complete", &[index], messages, None));
        }
    }
    let staged: HashSet<usize> = candidates
        .iter()
        .flat_map(|candidate| {
            candidate["indices"]
                .as_array()
                .unwrap()
                .iter()
                .map(|index| index.as_u64().unwrap() as usize)
        })
        .collect();
    let checkpoint = args.get("afterMessageId").and_then(|id| {
        complete
            .iter()
            .position(|(index, _)| &messages[*index]["id"] == id)
    });
    let rounds = &complete[checkpoint.map(|position| position + 1).unwrap_or(0)..];
    let maximum = settings["maxTurns"].as_u64().unwrap_or(12) as usize;
    let rounds = &rounds[rounds.len().saturating_sub(maximum)..];
    let tokens: u64 = rounds.iter().map(|(_, tokens)| tokens).sum();
    if rounds.len() < settings["minTurns"].as_u64().unwrap_or(6) as usize
        || tokens < settings["minTokens"].as_u64().unwrap_or(2048) && rounds.len() < maximum
    {
        return Ok(
            json!({"candidates": candidates, "reason": "batch-not-due", "completeTurns": rounds.len(), "estimatedTokens": tokens, "skippedOverlongMessages": skipped_overlong_messages}),
        );
    }
    static ORDINARY: OnceLock<Regex> = OnceLock::new();
    let ordinary = ORDINARY.get_or_init(|| regex(r"(?i)^\s*(?:我(?:更|通常|一直)?(?:喜欢|偏好|习惯|使用|希望|需要)|请(?:以后|始终|默认)|我们的?(?:约定|目标|要求|限制|计划)|(?:约定|目标|要求|限制|计划|下一步)\s*[:：]|I (?:prefer|usually|always|use|need)|We (?:use|plan|need)|My (?:preference|goal))"));
    let max_candidates = settings["maxCandidates"].as_u64().unwrap_or(8) as usize;
    let mut indices = Vec::new();
    let mut used_tokens = 0;
    let mut characters = 0;
    let mut skipped_token_budget_messages = 0;
    let batch_checkpoint = rounds.last().map(|(index, _)| &messages[*index]["id"]);
    for (index, _) in rounds {
        let message = &messages[*index];
        let text = message["text"].as_str().unwrap_or("");
        if !allowed(*index)
            || staged.contains(index)
            || instruction(text).is_some()
            || !ordinary.is_match(text)
        {
            continue;
        }
        let tokens = message["estimatedTokens"].as_u64().unwrap_or(0);
        if used_tokens + tokens > settings["maxTokens"].as_u64().unwrap_or(4096) {
            skipped_token_budget_messages += 1;
            continue;
        }
        let length = utf16_length(trim(text));
        if characters + length + 2 > 4000 {
            if !indices.is_empty() && candidates.len() < max_candidates {
                candidates.push(proposed(
                    "chat",
                    "ordinary-batch",
                    &indices,
                    messages,
                    batch_checkpoint,
                ));
            }
            indices.clear();
            characters = 0;
        }
        if candidates.len() >= max_candidates {
            break;
        }
        if !indices.is_empty() {
            characters += 2;
        }
        indices.push(*index);
        characters += length;
        used_tokens += tokens;
    }
    if !indices.is_empty() && candidates.len() < max_candidates {
        candidates.push(proposed(
            "chat",
            "ordinary-batch",
            &indices,
            messages,
            batch_checkpoint,
        ));
    }
    Ok(
        json!({"candidates": candidates, "reason": "complete-turn-batch", "completeTurns": rounds.len(), "estimatedTokens": tokens, "skippedOverlongMessages": skipped_overlong_messages, "skippedTokenBudgetMessages": skipped_token_budget_messages}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed_batch() -> Value {
        let messages = (0..12)
            .flat_map(|index| {
                [
                    json!({"id": format!("user-{index}"), "role": "user", "text": format!("我喜欢第 {index} 项约定。"), "eligibleLength": true, "estimatedTokens": 10}),
                    json!({"role": "assistant", "status": "completed", "hasContent": true, "estimatedTokens": 10}),
                ]
            })
            .collect::<Vec<_>>();
        json!({"messages": messages, "phase": "completed", "taskCompleted": false, "settings": {}, "hasProject": false})
    }

    #[test]
    fn completed_null_identity_matches_omission_and_rejects_non_null_invalid_sources() {
        let omitted = completed_batch();
        let mut nullable = omitted.clone();
        nullable["userMessageId"] = Value::Null;
        let result = extract(&omitted).unwrap();
        assert_eq!(result, extract(&nullable).unwrap());
        assert_eq!(
            result["candidates"][0]["indices"].as_array().unwrap().len(),
            12
        );
        assert_eq!(result["candidates"][0]["batchThroughMessageId"], "user-11");
        for invalid in [json!(""), json!("missing"), json!(7), json!({})] {
            nullable["userMessageId"] = invalid;
            assert_eq!(extract(&nullable).unwrap_err().code(), "INVALID_MEMORY");
        }
    }

    #[test]
    fn overlong_and_over_budget_sources_do_not_block_later_complete_turns() {
        let mut args = completed_batch();
        args["messages"][0]["text"] = json!(format!("我喜欢{}", "长".repeat(4000)));
        args["messages"][2]["estimatedTokens"] = json!(5000);
        let result = extract(&args).unwrap();
        assert_eq!(result["skippedOverlongMessages"], 1);
        assert_eq!(result["skippedTokenBudgetMessages"], 1);
        let indices = result["candidates"][0]["indices"].as_array().unwrap();
        assert_eq!(indices.len(), 10);
        assert_eq!(indices.first().unwrap(), 4);
        assert_eq!(indices.last().unwrap(), 22);
        assert_eq!(result["candidates"][0]["batchThroughMessageId"], "user-11");
    }

    #[test]
    fn incomplete_and_unquoted_assistant_output_never_becomes_a_candidate() {
        let mut args = completed_batch();
        args["messages"][23]["status"] = json!("interrupted");
        assert_eq!(extract(&args).unwrap()["reason"], "batch-not-due");
        args["messages"][23]["status"] = json!("completed");
        for message in args["messages"].as_array_mut().unwrap() {
            if message["role"] == "user" {
                message["text"] = json!("请分析这道题。");
            } else {
                message["text"] = json!("我喜欢使用这个结论。");
            }
        }
        assert!(
            extract(&args).unwrap()["candidates"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
