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
        .filter(|value| (512..=2_000_000).contains(value))
}

/// Retain only bounded numeric observations, narrowing each limit without copying provider payloads.
/// 仅保留有界数值观察，各项额度只收紧；不复制供应商正文、凭据或其他请求字段。
pub(crate) fn retain_numeric_limits(previous: &Value, observed: &Value) -> Value {
    let mut retained = serde_json::Map::new();
    for field in ["contextWindowTokens", "providerMaxInputTokens", "providerMaxOutputTokens", "estimatedInputBudgetTokens"] {
        let minimum = if field == "contextWindowTokens" { 512 } else { 1 };
        let valid = |value: &Value| value.as_u64().filter(|value| (minimum..=2_000_000).contains(value));
        let limit = match (valid(&previous[field]), valid(&observed[field])) {
            (Some(previous), Some(observed)) => Some(previous.min(observed)),
            (previous, observed) => previous.or(observed),
        };
        if let Some(limit) = limit { retained.insert(field.to_owned(), json!(limit)); }
    }
    Value::Object(retained)
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
    static LLAMA_CONTEXT: OnceLock<Regex> = OnceLock::new();
    static INPUT: OnceLock<Regex> = OnceLock::new();
    static OUTPUT: OnceLock<Regex> = OnceLock::new();
    let context = CONTEXT.get_or_init(|| expression(r"(?i)(?:maximum|max)\s+context\s+(?:length|window)(?:\s+(?:is|of))?\s*[:=]?\s*([0-9,]+)\s*tokens?"));
    let llama_context = LLAMA_CONTEXT.get_or_init(|| expression(r"(?i)\brequest\s*\(\s*([0-9,]+)\s+tokens?\s*\)\s+exceeds\s+the\s+available\s+context\s+size\s*\(\s*([0-9,]+)\s+tokens?\s*\)"));
    let input = INPUT.get_or_init(|| expression(r"(?i)(?:prompt|input)\s+is\s+too\s+long\s*:\s*[0-9,]+\s*tokens?\s*>\s*([0-9,]+)\s*(?:maximum|max)"));
    let output = OUTPUT.get_or_init(|| expression(r"(?i)(?-u:\b)max_(?:output_tokens|completion_tokens|tokens)(?-u:\b)[^\n]{0,120}?(?:at most|less than or equal to|between\s+[0-9]+\s+and|<=)\s*([0-9,]+)"));
    let captured = |pattern: &Regex| {
        pattern
            .captures(message)
            .and_then(|captures| captures[1].replace(',', "").parse::<u64>().ok())
    };
    // llama.cpp reports the rendered prompt count; retain only verified numeric limits, never its body.
    // llama.cpp 报告的是完整模板输入计数；仅保留可信数值与额度，不把上游正文带入诊断。
    if let Some(captures) = llama_context.captures(message) {
        let request_tokens = captures[1].replace(',', "").parse::<u64>().ok();
        let maximum = captures[2].replace(',', "").parse::<u64>().ok();
        if let (Some(request_tokens), Some(maximum)) = (request_tokens, maximum) {
            if (512..=2_000_000).contains(&maximum) && request_tokens > maximum {
                return json!({"kind":"context","contextWindowTokens":maximum,
                    "requestTokens":request_tokens,"verified":true});
            }
        }
    }
    if let Some(maximum) = captured(output).filter(|maximum| (1..=2_000_000).contains(maximum)) {
        return json!({"kind":"output","providerMaxOutputTokens":maximum,"verified":true});
    }
    if let Some(maximum) = captured(input).filter(|maximum| (1..=2_000_000).contains(maximum)) {
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
        .or_else(|| captured(context).filter(|maximum| (512..=2_000_000).contains(maximum)));
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
            let Some(maximum) = rejection["providerMaxInputTokens"].as_u64().filter(|maximum| (1..=2_000_000).contains(maximum)) else {
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
                .filter(|maximum| (1..=2_000_000).contains(maximum))
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
            if !(512..=2_000_000).contains(&maximum) {
                return Value::Null;
            }
            if maximum >= previous {
                let rejected_input_limit = rejection["inputLimitTokens"].as_u64()
                    .filter(|limit| (1..=maximum).contains(limit))
                    .unwrap_or(maximum);
                let Some(request_tokens) = rejection["requestTokens"]
                    .as_u64()
                    .filter(|tokens| *tokens > rejected_input_limit && rejection["verified"] == true)
                else {
                    return Value::Null;
                };
                let previous_input = current["inputBudgetTokens"]
                    .as_u64()
                    .or_else(|| current["providerMaxInputTokens"].as_u64())
                    .unwrap_or(maximum)
                    .min(maximum);
                let estimated_input = rejection["estimatedInputTokens"].as_u64()
                    .filter(|tokens| *tokens > 0 && *tokens <= previous_input)
                    .unwrap_or(previous_input);
                let allowed_input = maximum
                    .saturating_sub(current["maxOutputTokens"].as_u64().unwrap_or(0))
                    .min(rejected_input_limit)
                    .min(current["providerMaxInputTokens"].as_u64().unwrap_or(maximum));
                // Equal windows can still reject underestimated template input; narrow the estimated budget.
                // 窗口相同仍可能低估模板输入；按实际计数收紧估算预算，不能提高实际窗口或重发相同请求。
                let input_limit = ((u128::from(estimated_input) * u128::from(allowed_input) * 9)
                    / u128::from(request_tokens) / 10) as u64;
                if input_limit == 0 || input_limit >= previous_input {
                    return Value::Null;
                }
                return json!({"kind":"context","contextWindowTokens":maximum,
                    "estimatedInputBudgetTokens":input_limit,"requestTokens":request_tokens,
                    "budgetAdjusted":true,"verified":true});
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
            if let Some(input_limit) = limits["estimatedInputBudgetTokens"].as_u64() {
                cache_entry.insert("estimatedInputBudgetTokens".to_owned(), json!(input_limit));
            }
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
    fn tiny_verified_runtime_limits_survive_parsing_recovery_and_cache_retention() {
        for window in [512,1024] {
            let rejection = context_rejection(&json!({"error":{"message":format!("request (2049 tokens) exceeds the available context size ({window} tokens)")}}));
            assert_eq!(rejection["contextWindowTokens"],window);
            let mut args = plan_arguments();
            args["rejection"] = rejection;
            let plan = context_recovery_plan(&args);
            assert_eq!(plan["limits"]["contextWindowTokens"],window);
            let retained = retain_numeric_limits(&Value::Null,&json!({"contextWindowTokens":window,"providerMaxInputTokens":384,"providerMaxOutputTokens":64}));
            assert_eq!(retained["contextWindowTokens"],window);
            assert_eq!(retained["providerMaxOutputTokens"],64);
        }
    }

    #[test]
    fn retained_numeric_limits_never_restore_a_rejected_budget_or_provider_payload() {
        let first = retain_numeric_limits(&Value::Null, &json!({"contextWindowTokens":8192,
            "estimatedInputBudgetTokens":4800,"providerMaxOutputTokens":1024,"apiKey":"fixture-secret"}));
        let next = retain_numeric_limits(&first, &json!({"contextWindowTokens":16384,
            "estimatedInputBudgetTokens":3600,"providerMaxOutputTokens":2048,"providerMaxInputTokens":0,
            "message":"fixture-private"}));
        assert_eq!(next, json!({"contextWindowTokens":8192,"estimatedInputBudgetTokens":3600,"providerMaxOutputTokens":1024}));
        assert_eq!(retain_numeric_limits(&next, &json!({"estimatedInputBudgetTokens":9999999})), next);
        assert!(!next.to_string().contains("fixture"));
    }

    #[test]
    fn llama_rendered_context_rejection_keeps_only_verified_numeric_observations() {
        for message in [
            "request (12163 tokens) exceeds the available context size (8192 tokens), try increasing it fixture-secret",
            "request (12,163 tokens) exceeds the available context size (8,192 tokens)",
        ] {
            let rejection = context_rejection(&json!({"error":{"message":message}}));
            assert_eq!(rejection, json!({"kind":"context","contextWindowTokens":8192,
                "requestTokens":12163,"verified":true}));
            assert!(!rejection.to_string().contains("fixture-secret"));
        }
        for message in [
            "request (4096 tokens) exceeds the available context size (8192 tokens)",
            "request (12163 tokens) exceeds the available context size (0 tokens)",
            "invalid tools parameter; context size is 8192 tokens",
            "request (fixture tokens) exceeds the available context size (8192 tokens)",
        ] {
            assert!(context_rejection(&json!({"error":{"message":message}})).is_null());
        }
    }

    #[test]
    fn same_actual_window_narrows_underestimated_input_without_changing_the_window() {
        let mut args = plan_arguments();
        args["current"] = json!({"contextWindowTokens":8192,"inputBudgetTokens":5039,
            "maxOutputTokens":1474});
        args["rejection"] = json!({"kind":"context","contextWindowTokens":8192,
            "requestTokens":12163,"verified":true});
        args["cacheEntry"] = json!({"providerMaxInputTokens":8192});
        let plan = context_recovery_plan(&args);
        let input_limit = 5039_u64 * (8192 - 1474) * 9 / 12163 / 10;
        assert_eq!(plan["limits"]["contextWindowTokens"], 8192);
        assert_eq!(plan["limits"]["estimatedInputBudgetTokens"], input_limit);
        assert!(plan["limits"].get("providerMaxInputTokens").is_none());
        assert!(input_limit < 5039);
        assert_eq!(plan["limits"]["budgetAdjusted"], true);
        assert_eq!(plan["cacheEntry"]["contextWindowTokens"], 8192);
        assert_eq!(plan["cacheEntry"]["estimatedInputBudgetTokens"], input_limit);
        assert_eq!(plan["cacheEntry"]["providerMaxInputTokens"], 8192);
        for (field, value) in [
            ("verified", json!(false)),
            ("requestTokens", Value::Null),
            ("requestTokens", json!(8192)),
        ] {
            let mut unverified = args.clone();
            unverified["rejection"][field] = value;
            assert!(context_recovery_plan(&unverified).is_null());
        }
        args["rejection"]["requestTokens"] = json!(7000);
        args["rejection"]["inputLimitTokens"] = json!(6000);
        args["rejection"]["estimatedInputTokens"] = json!(4500);
        let measured = context_recovery_plan(&args);
        assert_eq!(measured["limits"]["estimatedInputBudgetTokens"], 4500 * 6000 * 9 / 7000 / 10);
        assert_eq!(measured["limits"]["contextWindowTokens"], 8192);
        args["rejection"]["requestTokens"] = json!(6000);
        assert!(context_recovery_plan(&args).is_null());
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
