use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use super::protocol::string;

const MAX_CONTEXT_RECOVERY_ATTEMPTS: u64 = 2;
const OBSERVED_CONTEXT_LIMIT_LIFETIME_MS: u64 = 600_000;
const MAX_OBSERVED_CONTEXT_LIMITS: usize = 32;

fn valid_limit(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|value| (2048..=2_000_000).contains(value))
}

fn expression(pattern: &str) -> Regex {
    // JavaScript whitespace excludes NEL and includes BOM; provider-message parsing preserves that contract.
    // JavaScript 空白字符排除 NEL、包含 BOM；供应商错误消息解析保持相同合同。
    Regex::new(&pattern.replace(r"\s", r"[\t\n\r\x0B\x0C \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]")).unwrap()
}

fn context_rejection(body: &Value) -> Value {
    let error = &body["error"];
    let message = error
        .as_str()
        .or_else(|| error["message"].as_str())
        .or_else(|| body["message"].as_str())
        .unwrap_or("");
    let code = error
        .get("code")
        .filter(|value| !value.is_null())
        .or_else(|| error.get("type").filter(|value| !value.is_null()))
        .or_else(|| body.get("code"))
        .map(string)
        .unwrap_or("")
        .to_lowercase();
    static CONTEXT: OnceLock<Regex> = OnceLock::new();
    static INPUT: OnceLock<Regex> = OnceLock::new();
    static OUTPUT: OnceLock<Regex> = OnceLock::new();
    let context = CONTEXT.get_or_init(|| expression(r"(?i)(?:maximum|max)\s+context\s+(?:length|window)(?:\s+(?:is|of))?\s*[:=]?\s*([0-9,]+)\s*tokens?"));
    let input = INPUT.get_or_init(|| expression(r"(?i)(?:prompt|input)\s+is\s+too\s+long\s*:\s*[0-9,]+\s*tokens?\s*>\s*([0-9,]+)\s*(?:maximum|max)"));
    let output = OUTPUT.get_or_init(|| expression(r"(?i)(?-u:\b)max_(?:output_tokens|completion_tokens|tokens)(?-u:\b)[^\n]{0,120}?(?:at most|less than or equal to|between\s+[0-9]+\s+and|<=)\s*([0-9,]+)"));
    let captured = |pattern: &Regex| {
        pattern
            .captures(message)
            .and_then(|captures| captures[1].replace(',', "").parse::<u64>().ok())
    };
    if let Some(maximum) = captured(output).filter(|maximum| (256..=2_000_000).contains(maximum)) {
        return json!({"kind":"output","providerMaxOutputTokens":maximum,"verified":true});
    }
    if let Some(maximum) = captured(input).filter(|maximum| (2048..=2_000_000).contains(maximum)) {
        return json!({"kind":"input","providerMaxInputTokens":maximum,"verified":true});
    }
    let has_context = context.is_match(message);
    if !matches!(
        code.as_str(),
        "context_length_exceeded"
            | "context_window_exceeded"
            | "model_context_window_exceeded"
            | "input_too_long"
            | "prompt_too_long"
    ) && !has_context
    {
        return Value::Null;
    }
    let numeric = error
        .get("maximum_context_length")
        .filter(|value| !value.is_null())
        .unwrap_or(&error["max_context_tokens"]);
    let maximum = valid_limit(numeric)
        .or_else(|| captured(context).filter(|maximum| (2048..=2_000_000).contains(maximum)));
    match maximum {
        Some(maximum) => json!({"kind":"context","contextWindowTokens":maximum,"verified":true}),
        None => json!({"kind":"context","verified":false}),
    }
}

/// Recovery only narrows a verified rejected step; it never discards effects, changes identity or grants capacity.
/// 恢复仅缩小明确被拒绝的步骤；不丢弃副作用、不更换身份，也不自行授予上下文容量。
fn recovery_limits(args: &Value) -> Value {
    let rejection = &args["rejection"];
    let current = &args["current"];
    match string(&rejection["kind"]) {
        "input" => {
            let Some(maximum) = valid_limit(&rejection["providerMaxInputTokens"]) else {
                return Value::Null;
            };
            if current["providerMaxInputTokens"]
                .as_u64()
                .is_some_and(|previous| maximum >= previous)
            {
                return Value::Null;
            }
            json!({"kind":"input","contextWindowTokens":current["contextWindowTokens"],"providerMaxInputTokens":maximum,"verified":rejection["verified"]})
        }
        "output" => {
            let Some(maximum) = rejection["providerMaxOutputTokens"]
                .as_u64()
                .filter(|maximum| (256..=2_000_000).contains(maximum))
            else {
                return Value::Null;
            };
            if maximum as f64 >= current["maxOutputTokens"].as_f64().unwrap_or(f64::INFINITY) {
                return Value::Null;
            }
            json!({"kind":"output","contextWindowTokens":current["contextWindowTokens"],"providerMaxOutputTokens":maximum,"verified":rejection["verified"]})
        }
        "context" => {
            let Some(previous) = current["contextWindowTokens"].as_u64() else {
                return Value::Null;
            };
            let maximum = rejection["contextWindowTokens"]
                .as_u64()
                .unwrap_or(previous / 2)
                .min(previous);
            if !(2048..=2_000_000).contains(&maximum) || maximum >= previous {
                return Value::Null;
            }
            json!({"kind":"context","contextWindowTokens":maximum,"verified":rejection["verified"]==true})
        }
        _ => Value::Null,
    }
}

fn observed_limits(args: &Value) -> Value {
    let entry = &args["entry"];
    let expired = entry["expiresAt"]
        .as_u64()
        .zip(args["now"].as_u64())
        .is_some_and(|(expires_at, now)| expires_at <= now);
    json!({"entry":if expired { &Value::Null } else { entry }, "remove":expired})
}

/// The adapter stores original identity keys; Rust owns retry admission and cache lifetime/capacity.
/// 适配器保留原始身份键；Rust 统一重试准入与缓存寿命/容量，重试只针对已拒绝的模型步骤。
fn context_recovery_plan(args: &Value) -> Value {
    let attempts = args["recoveryAttempts"].as_u64().unwrap_or(0);
    if args["errorCode"] != "MODEL_CONTEXT_LIMIT_REJECTED"
        || args["isRequestFactory"] != true
        || args["canRebuildContext"] != true
        || attempts >= MAX_CONTEXT_RECOVERY_ATTEMPTS
    {
        return Value::Null;
    }
    let limits = recovery_limits(args);
    if limits.is_null() {
        return Value::Null;
    }
    let Some(now) = args["now"].as_u64() else {
        return Value::Null;
    };
    let Some(cache_key) = args["cacheKey"].as_str() else {
        return Value::Null;
    };
    let mut cache_entry = args["cacheEntry"].as_object().cloned().unwrap_or_default();
    match limits["kind"].as_str() {
        Some("context") => {
            cache_entry.insert(
                "contextWindowTokens".to_owned(),
                limits["contextWindowTokens"].clone(),
            );
            cache_entry.insert("verified".to_owned(), limits["verified"].clone());
        }
        Some("input") => {
            cache_entry.insert(
                "providerMaxInputTokens".to_owned(),
                limits["providerMaxInputTokens"].clone(),
            );
        }
        Some("output") => {
            cache_entry.insert(
                "providerMaxOutputTokens".to_owned(),
                limits["providerMaxOutputTokens"].clone(),
            );
        }
        _ => return Value::Null,
    }
    cache_entry.insert(
        "expiresAt".to_owned(),
        json!(now.saturating_add(OBSERVED_CONTEXT_LIMIT_LIFETIME_MS)),
    );
    // Reads do not renew expiry; a successful update becomes the most recent observation.
    // 读取不续期；成功更新移动到最近观察位置，容量淘汰保持既有最近写入顺序。
    let remaining_keys: Vec<&Value> = args["cacheKeys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|key| key.as_str() != Some(cache_key))
        .collect();
    let eviction_count = (remaining_keys.len() + 1).saturating_sub(MAX_OBSERVED_CONTEXT_LIMITS);
    let mut cache_remove_keys = vec![json!(cache_key)];
    cache_remove_keys.extend(remaining_keys.into_iter().take(eviction_count).cloned());
    json!({"limits":limits,"recoveryAttempts":attempts + 1,"cacheEntry":cache_entry,
        "cacheRemoveKeys":cache_remove_keys})
}

pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    Ok(match operation {
        "models.recovery.contextRejection" => context_rejection(&args["body"]),
        "models.recovery.contextLimits" => recovery_limits(args),
        "models.recovery.observedLimits" => observed_limits(args),
        "models.recovery.contextPlan" => context_recovery_plan(args),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_arguments() -> Value {
        json!({"errorCode":"MODEL_CONTEXT_LIMIT_REJECTED","isRequestFactory":true,
            "canRebuildContext":true,"recoveryAttempts":0,"rejection":{"kind":"context","verified":false},
            "current":{"contextWindowTokens":32768},"cacheKey":"provider:model:credential",
            "cacheKeys":[],"now":1000})
    }

    #[test]
    fn context_retry_is_bounded_and_only_admits_rejected_rebuildable_steps() {
        let mut args = plan_arguments();
        for (attempt, expected_window) in [(0, 16384), (1, 8192)] {
            args["recoveryAttempts"] = json!(attempt);
            let result = context_recovery_plan(&args);
            assert_eq!(result["recoveryAttempts"], attempt + 1);
            assert_eq!(result["limits"]["contextWindowTokens"], expected_window);
            args["current"] = result["limits"].clone();
        }
        args["recoveryAttempts"] = json!(2);
        assert!(context_recovery_plan(&args).is_null());
        for (field, value) in [
            ("errorCode", json!("MODEL_HTTP_ERROR")),
            ("isRequestFactory", json!(false)),
            ("canRebuildContext", json!(false)),
        ] {
            let mut rejected = plan_arguments();
            rejected[field] = value;
            assert!(context_recovery_plan(&rejected).is_null());
        }
    }

    #[test]
    fn observed_limits_expire_at_the_boundary_without_renewing_on_read() {
        let entry = json!({"contextWindowTokens":8192,"verified":true,"expiresAt":601000});
        assert_eq!(
            observed_limits(&json!({"entry":entry,"now":600999})),
            json!({"entry":entry,"remove":false})
        );
        assert_eq!(
            observed_limits(&json!({"entry":entry,"now":601000})),
            json!({"entry":null,"remove":true})
        );
        assert_eq!(
            observed_limits(&json!({"now":1000})),
            json!({"entry":null,"remove":false})
        );
    }

    #[test]
    fn recovery_merges_independent_limits_and_evicts_oldest_observations() {
        let mut args = plan_arguments();
        args["cacheKeys"] = json!(
            (0..32)
                .map(|index| format!("provider-{index}"))
                .collect::<Vec<_>>()
        );
        args["cacheEntry"] =
            json!({"providerMaxInputTokens":8192,"providerMaxOutputTokens":4096,"expiresAt":1500});
        let result = context_recovery_plan(&args);
        assert_eq!(result["cacheEntry"]["expiresAt"], 601000);
        assert_eq!(result["cacheEntry"]["providerMaxInputTokens"], 8192);
        assert_eq!(result["cacheEntry"]["providerMaxOutputTokens"], 4096);
        assert_eq!(
            result["cacheRemoveKeys"],
            json!(["provider:model:credential", "provider-0"])
        );
        args["cacheKey"] = json!("provider-0");
        assert_eq!(
            context_recovery_plan(&args)["cacheRemoveKeys"],
            json!(["provider-0"])
        );

        args["rejection"] = json!({"kind":"output","providerMaxOutputTokens":2048,"verified":true});
        args["current"]["maxOutputTokens"] = json!(4096);
        args["cacheEntry"] = result["cacheEntry"].clone();
        let output = context_recovery_plan(&args);
        assert_eq!(output["cacheEntry"]["contextWindowTokens"], 16384);
        assert_eq!(output["cacheEntry"]["providerMaxInputTokens"], 8192);
        assert_eq!(output["cacheEntry"]["providerMaxOutputTokens"], 2048);
    }
}
