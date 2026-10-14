use serde_json::{Value, json};

use super::{connections::local_host, protocol::string};

fn valid_limit(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|value| *value > 0 && *value <= 2_000_000)
}

fn exact_official_endpoint(endpoint: &Value) -> bool {
    endpoint["protocol"] == "https:"
        && ["port", "username", "password", "search", "hash"]
            .iter()
            .all(|key| string(&endpoint[*key]).is_empty())
        && ["", "/", "/v1", "/v1/"].contains(&string(&endpoint["pathname"]))
}

/// Auxiliary summaries may use a declared provider mode; this never establishes proxy quotas or changes normal replies.
/// 辅助摘要可使用明确供应商的专用模式；不据此认定代理额度，也不改变正常回复的思考设置。
fn summary_controls(args: &Value) -> Value {
    let endpoint = &args["endpoint"];
    let known_model = ["deepseek-flash", "deepseek-v4-pro"].contains(&string(&args["model"]));
    let valid_endpoint = ["http:", "https:"].contains(&string(&endpoint["protocol"]))
        && !string(&endpoint["hostname"]).is_empty()
        && ["username", "password", "search", "hash"]
            .iter()
            .all(|key| string(&endpoint[*key]).is_empty());
    let official = exact_official_endpoint(endpoint) && endpoint["hostname"] == "api.deepseek.com";
    let compatible = valid_endpoint && args["providerId"] == "deepseek";
    let applied =
        args["protocol"] == "openai-completions" && known_model && (official || compatible);
    let basis = if !applied {
        Value::Null
    } else if official {
        json!("official-endpoint")
    } else {
        json!("configured-compatible-provider")
    };
    json!({
        "parameters":if applied {json!({"thinking":{"type":"disabled"}})} else {json!({})},
        "audit":{"purpose":"context-summary","applied":applied,"basis":basis,
            "control":if applied {json!("thinking-disabled")} else {Value::Null},
            "quotaInferred":false}
    })
}
fn official(host: &str, model: &str) -> Option<Value> {
    let (window, output, input, source) = match host {
        "api.deepseek.com" if ["deepseek-flash", "deepseek-v4-pro"].contains(&model) => (
            1_048_576,
            Some(393_216),
            None,
            "https://api-docs.deepseek.com/api/list-models/".to_owned(),
        ),
        "api.moonshot.cn" if model == "kimi-k3" => (
            1_048_576,
            Some(1_048_576),
            None,
            "https://platform.kimi.com/docs/guide/kimi-k3-quickstart".to_owned(),
        ),
        "api.moonshot.cn"
            if ["kimi-k2.7-code", "kimi-k2.7-code-highspeed", "kimi-k2.6"].contains(&model) =>
        {
            (
                262_144,
                None,
                None,
                "https://platform.kimi.com/docs/pricing/chat".to_owned(),
            )
        }
        "api.anthropic.com" => {
            let latest = [
                "opus-5-5",
                "sonnet-5-5",
                "fable-5-1",
                "opus-5",
                "sonnet-5",
                "fable-5",
                "opus-4-8",
                "opus-4-7",
                "opus-4-6",
                "sonnet-4-6",
            ];
            if let Some(suffix) = latest
                .iter()
                .find(|suffix| model == format!("claude-{suffix}"))
            {
                (
                    1_000_000,
                    Some(128_000),
                    None,
                    format!("https://platform.claude.com/docs/en/models/{suffix}/overview"),
                )
            } else {
                let (suffix, _) = [
                    ("haiku-4-5", "20251001"),
                    ("opus-4-5", "20251101"),
                    ("sonnet-4-5", "20250929"),
                ]
                .into_iter()
                .find(|(suffix, snapshot)| {
                    model == format!("claude-{suffix}")
                        || model == format!("claude-{suffix}-{snapshot}")
                })?;
                (
                    200_000,
                    Some(64_000),
                    None,
                    format!("https://platform.claude.com/docs/en/models/{suffix}/overview"),
                )
            }
        }
        "api.openai.com" => {
            let rows: &[(&str, u64, u64, Option<&str>, Option<u64>)] = &[
                ("gpt-6-astra", 1050000, 128000, None, Some(922000)),
                ("gpt-6.1-sol", 1050000, 128000, None, Some(922000)),
                ("gpt-6-sol", 1050000, 128000, None, Some(922000)),
                ("gpt-6-luna", 1050000, 128000, None, Some(922000)),
                ("gpt-5.6-sol", 1050000, 128000, None, Some(922000)),
                ("gpt-5.6-terra", 1050000, 128000, None, Some(922000)),
                ("gpt-5.6-luna", 1050000, 128000, None, Some(922000)),
                ("gpt-5.5", 1050000, 128000, Some("2026-04-23"), None),
                ("gpt-5.5-pro", 1050000, 128000, Some("2026-04-23"), None),
                ("gpt-5.4", 1050000, 128000, Some("2026-03-05"), None),
                ("gpt-5.4-pro", 1050000, 128000, Some("2026-03-05"), None),
                (
                    "gpt-5.4-mini",
                    400000,
                    128000,
                    Some("2026-03-17"),
                    Some(272000),
                ),
                (
                    "gpt-5.4-nano",
                    400000,
                    128000,
                    Some("2026-03-17"),
                    Some(272000),
                ),
                ("gpt-5.3-codex", 400000, 128000, None, Some(272000)),
                ("gpt-5.2", 400000, 128000, Some("2025-12-11"), None),
                ("gpt-5.2-pro", 400000, 128000, Some("2025-12-11"), None),
                ("gpt-5.1", 400000, 128000, Some("2025-11-13"), None),
                ("gpt-5", 400000, 128000, Some("2025-08-07"), Some(272000)),
                (
                    "gpt-5-mini",
                    400000,
                    128000,
                    Some("2025-08-07"),
                    Some(272000),
                ),
                (
                    "gpt-5-nano",
                    400000,
                    128000,
                    Some("2025-08-07"),
                    Some(272000),
                ),
                ("gpt-5-pro", 400000, 272000, Some("2025-10-06"), None),
                ("o3", 200000, 100000, Some("2025-04-16"), None),
                ("o3-pro", 200000, 100000, Some("2025-06-10"), None),
                ("gpt-4.1", 1047576, 32768, Some("2025-04-14"), None),
                ("gpt-4.1-mini", 1047576, 32768, Some("2025-04-14"), None),
                ("gpt-4o", 128000, 16384, Some("2024-08-06"), None),
                ("gpt-4o-mini", 128000, 16384, Some("2024-07-18"), None),
                ("chat-latest", 400000, 128000, None, Some(272000)),
            ];
            let (name, window, output, _, input) =
                rows.iter().find(|(name, _, _, snapshot, _)| {
                    *name == model
                        || snapshot.is_some_and(|snapshot| model == format!("{name}-{snapshot}"))
                })?;
            (
                *window,
                Some(*output),
                *input,
                format!("https://developers.openai.com/api/docs/models/{name}"),
            )
        }
        _ => return None,
    };
    let mut value = json!({"contextWindowTokens": window, "source": source});
    if let Some(output) = output {
        value["maxOutputTokens"] = json!(output);
    }
    if let Some(input) = input {
        value["maxInputTokens"] = json!(input);
    }
    Some(value)
}

/// Exact snapshot capability declarations do not infer quotas for proxies, aliases or credential holders.
/// 精确快照能力声明不推测代理、未知别名或凭据拥有者的可用额度；保留现有已核验表。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    let endpoint = &args["endpoint"];
    if operation == "models.capabilities.summaryControls" {
        return Ok(summary_controls(args));
    }
    if operation == "models.capabilities.lookup" {
        if !exact_official_endpoint(endpoint) || !args["model"].is_string() {
            return Ok(json!({}));
        }
        return Ok(
            official(string(&endpoint["hostname"]), string(&args["model"])).unwrap_or(json!({})),
        );
    }
    if operation != "models.capabilities.automaticContext" {
        return Err("AGENT_CORE_UNKNOWN_OPERATION".into());
    }
    let local = local_host(string(&endpoint["hostname"]));
    let valid_endpoint = ["http:", "https:"].contains(&string(&endpoint["protocol"]))
        && ["username", "password", "search", "hash"]
            .iter()
            .all(|key| string(&endpoint[*key]).is_empty())
        && (endpoint["protocol"] == "https:" || local);
    let local_model = &args["localModel"];
    let capabilities = &args["capabilities"];
    let trusted_llama = valid_endpoint
        && local
        && local_model["backend"] == "llama.cpp"
        && local_model["source"] == "llama-cpp-props"
        && local_model["observationOnly"] == true
        && local_model["endpointOrigin"] == endpoint["origin"];
    let trusted_local =
        valid_endpoint && local && (local_model["backend"] == "ollama" || trusted_llama);
    let invalid = trusted_local
        && [
            "runtimeContextTokens",
            "configuredContextTokens",
            "modelMaximumContextTokens",
        ]
        .iter()
        .any(|key| !local_model[*key].is_null() && valid_limit(&local_model[*key]).is_none());
    let mut candidates: Vec<(&str, u64)> = Vec::new();
    if trusted_local {
        if let Some(tokens) = valid_limit(&local_model["runtimeContextTokens"]) {
            candidates.push(("local-runtime", tokens));
        } else if let Some(tokens) = valid_limit(&local_model["configuredContextTokens"]) {
            candidates.push(("local-configured", tokens));
        }
        if let Some(tokens) = valid_limit(&local_model["modelMaximumContextTokens"]) {
            candidates.push(("local-model-metadata", tokens));
        }
    }
    let official = valid_limit(&capabilities["contextWindowTokens"]);
    if let Some(tokens) = official {
        candidates.push(("official-capability", tokens));
    }
    if invalid {
        candidates.push(("conservative-fallback", 32768));
    }
    let configured = &args["connection"]["contextWindowTokens"];
    if candidates.is_empty() && valid_endpoint {
        if let Some(tokens) = valid_limit(configured).filter(|tokens| *tokens >= 2048) {
            candidates.push(("configured-unverified", tokens));
        }
    }
    let selected = candidates.iter().copied().reduce(|current, candidate| {
        if candidate.1 < current.1 {
            candidate
        } else {
            current
        }
    });
    let (source, tokens) = selected.unwrap_or(("conservative-fallback", 32768));
    let reason = if invalid {
        json!("invalid-local-capability-metadata")
    } else if source == "configured-unverified" {
        json!("unverified-configured-context")
    } else if selected.is_some() {
        Value::Null
    } else if trusted_local {
        json!("local-window-unavailable")
    } else {
        json!("unknown-model-capability")
    };
    let limits: serde_json::Map<String, Value> = candidates
        .into_iter()
        .map(|(source, tokens)| (source.into(), json!(tokens)))
        .collect();
    let mut value = json!({"mode": "automatic", "effectiveTokens": tokens, "source": source, "verified": !invalid && !["configured-unverified", "conservative-fallback"].contains(&source),
        "reason": reason, "legacyConfiguredTokens": configured, "legacyFieldIgnored": source != "configured-unverified", "limits": limits});
    if official.is_some() {
        if let Some(source) = capabilities.get("source") {
            value["capabilitySource"] = source.clone();
        }
    }
    Ok(value)
}
