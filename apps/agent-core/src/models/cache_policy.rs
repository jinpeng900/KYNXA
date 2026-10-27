//! Provider cache controls are pure request projections; they never own model execution or a cache ledger.
//! 供应商缓存控制仅投影请求，不接管模型执行，也不创建缓存账本。

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderCache {
    OpenAiLegacy,
    OpenAiModern,
    Anthropic,
    Automatic,
    Unknown,
}

fn openai_generation(model: &str) -> Option<(u32, u32)> {
    let version = model.strip_prefix("gpt-")?.split('-').next()?;
    let mut fields = version.split('.');
    let major = fields.next()?.parse().ok()?;
    let minor = fields.next().map_or(Some(0), |field| field.parse().ok())?;
    fields.next().is_none().then_some((major, minor))
}

fn provider(connection: &Value, model: &str, protocol: &str) -> (ProviderCache, &'static str) {
    let endpoint = url::Url::parse(connection["baseUrl"].as_str().unwrap_or("")).ok();
    let Some(endpoint) = endpoint.filter(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.port_or_known_default() == Some(443)
    }) else {
        return (ProviderCache::Unknown, "unknown");
    };
    match endpoint.host_str() {
        Some("api.openai.com") if matches!(protocol, "openai-completions" | "openai-responses") => {
            let generation = openai_generation(model);
            if protocol == "openai-responses"
                && generation.is_some_and(|(major, minor)| major == 6 || major == 5 && minor >= 6)
            {
                return (ProviderCache::OpenAiModern, "openai");
            }
            let legacy = generation
                .is_some_and(|(major, minor)| matches!(major, 5 | 6) || major == 4 && minor == 1)
                || model == "gpt-4o"
                || model.starts_with("gpt-4o-")
                || ["o1", "o3", "o4"]
                    .iter()
                    .any(|family| model == *family || model.starts_with(&format!("{family}-")));
            (
                if legacy {
                    ProviderCache::OpenAiLegacy
                } else {
                    ProviderCache::Unknown
                },
                "openai",
            )
        }
        Some("api.anthropic.com")
            if protocol == "anthropic-messages"
                && [
                    "claude-3",
                    "claude-sonnet-",
                    "claude-opus-",
                    "claude-haiku-",
                ]
                .iter()
                .any(|family| model.starts_with(family)) =>
        {
            (ProviderCache::Anthropic, "anthropic")
        }
        Some("api.deepseek.com")
            if matches!(
                protocol,
                "openai-completions" | "openai-responses" | "anthropic-messages"
            ) && model.starts_with("deepseek-") =>
        {
            (ProviderCache::Automatic, "deepseek")
        }
        Some("generativelanguage.googleapis.com")
            if protocol == "openai-completions"
                && ["gemini-2.5", "gemini-3", "gemini-4"]
                    .iter()
                    .any(|family| model.starts_with(family)) =>
        {
            (ProviderCache::Automatic, "gemini-openai-compatible")
        }
        _ => (ProviderCache::Unknown, "unknown"),
    }
}

fn scope_key(connection: &Value, model: &str, protocol: &str, scope: &str) -> String {
    // Keep accounting scopes stable without sending disk paths, user identifiers, queries or credentials.
    // 保持计量范围稳定，不能把盘符路径、用户标识、查询或凭据直接发送给供应商。
    let domain = json!([
        "kynxa.prompt-cache.v1",
        scope,
        connection["providerId"],
        connection["baseUrl"],
        model,
        protocol
    ]);
    let digest = format!("{:x}", Sha256::digest(domain.to_string().as_bytes()));
    format!("kx1_{}", &digest[..56])
}

fn marker_count(body: &Value) -> usize {
    usize::from(body.get("cache_control").is_some())
        + body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|tool| tool.get("cache_control").is_some())
            .count()
        + body["system"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block.get("cache_control").is_some())
            .count()
        + body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .filter(|block| block.get("cache_control").is_some())
            .count()
}

fn cache_mark(block: &mut Value, available: &mut usize) -> bool {
    if *available == 0 || block.get("cache_control").is_some() || !block.is_object() {
        return false;
    }
    block["cache_control"] = json!({"type":"ephemeral"});
    *available -= 1;
    true
}

fn anthropic(body: &mut Value, stable_system: &str) -> (usize, usize) {
    // Preserve existing TTL order; a 5m marker before a 1h marker is invalid.
    // 保留现有 TTL 顺序；5m 标记放在 1h 前会使请求无效。
    if marker_count(body) > 0 {
        return (0, 0);
    }
    let mut slots = 4;
    let mut written = 0;
    if let Some(tool) = body["tools"]
        .as_array_mut()
        .and_then(|tools| tools.last_mut())
        .filter(|tool| tool["name"].is_string() && tool.get("input_schema").is_some())
    {
        written += usize::from(cache_mark(tool, &mut slots));
    }
    let mut prefix_bytes = 0;
    if let Some(system) = body["system"].as_str().map(str::to_owned) {
        if !stable_system.trim().is_empty() && system.starts_with(stable_system) {
            let suffix = &system[stable_system.len()..];
            let suffix_is_only_spacing = suffix.trim().is_empty();
            let stable_text = if suffix_is_only_spacing {
                system.as_str()
            } else {
                stable_system
            };
            let mut stable = json!({"type":"text","text":stable_text});
            written += usize::from(cache_mark(&mut stable, &mut slots));
            let mut blocks = vec![stable];
            if !suffix_is_only_spacing {
                blocks.push(json!({"type":"text","text":suffix}));
            }
            body["system"] = json!(blocks);
            prefix_bytes = stable_text.len();
        }
    }
    // Reuse completed turns; time, memory and permission changes still invalidate the prefix.
    // 复用已完成轮次；时间、记忆和权限变化仍使前缀失效。
    if slots > 0 {
        if let Some(messages) = body["messages"].as_array_mut() {
            'history: for message in messages.iter_mut().rev() {
                if !matches!(message["role"].as_str(), Some("user" | "assistant")) {
                    continue;
                }
                if let Some(text) = message["content"]
                    .as_str()
                    .filter(|text| !text.trim().is_empty())
                    .map(str::to_owned)
                {
                    let mut block = json!({"type":"text","text":text});
                    written += usize::from(cache_mark(&mut block, &mut slots));
                    message["content"] = json!([block]);
                    break;
                }
                if let Some(blocks) = message["content"].as_array_mut() {
                    for block in blocks.iter_mut().rev() {
                        let eligible = match block["type"].as_str() {
                            Some("text") => block["text"]
                                .as_str()
                                .is_some_and(|text| !text.trim().is_empty()),
                            Some("tool_result") => block["tool_use_id"]
                                .as_str()
                                .is_some_and(|id| !id.is_empty()),
                            _ => false,
                        };
                        if eligible && cache_mark(block, &mut slots) {
                            written += 1;
                            break 'history;
                        }
                    }
                }
            }
        }
    }
    (written, prefix_bytes)
}

/// Apply confirmed provider controls without changing instruction authority, history roles or continuation lifetimes.
/// 仅应用已确认的供应商控制；不能改变指令优先级、历史角色或续传生命周期。
pub(crate) fn apply(
    connection: &Value,
    model: &str,
    protocol: &str,
    body: &mut Value,
    scope: &str,
    stable_system: &str,
) -> Value {
    let (policy, provider_name) = provider(connection, model, protocol);
    let mut mode = "unavailable";
    let mut reason = "unrecognized-provider-model-or-protocol";
    let (mut breakpoints, mut prefix_bytes) = (0, 0);
    if !body.is_object() {
        reason = "request-body-unavailable";
    } else if !scope.is_empty() {
        match policy {
            ProviderCache::OpenAiLegacy | ProviderCache::OpenAiModern => {
                body["prompt_cache_key"] = json!(scope_key(connection, model, protocol, scope));
                mode = "implicit";
                reason = "official-automatic-cache";
                if policy == ProviderCache::OpenAiModern {
                    // Responses instructions replace prior guidance; caching must never turn them into retained input items.
                    // Responses instructions 替换旧指导；缓存不能把它转成会被继承和累积的输入项。
                    if body.get("prompt_cache_options").is_none() {
                        body["prompt_cache_options"] = json!({"mode":"implicit","ttl":"30m"});
                        reason = "official-implicit-cache-preserves-instructions";
                    } else {
                        mode = "unchanged";
                        reason = "existing-request-cache-policy-preserved";
                    }
                    prefix_bytes = body["instructions"].as_str()
                        .filter(|instructions| !stable_system.is_empty() && instructions.starts_with(stable_system))
                        .map_or(0, |_| stable_system.len());
                }
            }
            ProviderCache::Anthropic => {
                (breakpoints, prefix_bytes) = anthropic(body, stable_system);
                mode = if breakpoints > 0 {
                    "explicit-prefix"
                } else {
                    "unchanged"
                };
                reason = if breakpoints > 0 {
                    "stable-system-or-tool-boundary"
                } else {
                    "no-eligible-boundary-or-free-slot"
                };
            }
            ProviderCache::Automatic => {
                mode = "automatic";
                reason = "provider-managed-no-request-fields";
            }
            ProviderCache::Unknown => {}
        }
    } else {
        reason = "verified-scope-unavailable";
    }
    include_stream_usage(connection, model, protocol, body);
    canonicalize_request(body);
    let hash = format!(
        "{:x}",
        Sha256::digest(
            format!("kynxa.cache-policy.v1/{provider_name}/{policy:?}/{mode}").as_bytes()
        )
    );
    json!({"provider":provider_name,"mode":mode,"reason":reason,"breakpoints":breakpoints,
        "stablePrefixBytes":prefix_bytes,"policyHash":&hash[..20]})
}

/// Request streaming usage only from verified protocols to avoid rejected fields.
/// 仅向已核验协议请求流式计量，避免兼容服务拒绝额外字段。
pub(crate) fn include_stream_usage(
    connection: &Value,
    model: &str,
    protocol: &str,
    body: &mut Value,
) {
    if protocol != "openai-completions" || body["stream"] != true {
        return;
    }
    let (policy, provider_name) = provider(connection, model, protocol);
    // The pinned local llama.cpp stream reports its measured KV reuse only when usage is requested.
    // 固定本地 llama.cpp 仅在请求 usage 时回传实测 KV 复用量；这不改变缓存或推理策略。
    let managed_local = super::gateway_local_engine::is_managed(connection);
    if !matches!(
        policy,
        ProviderCache::OpenAiLegacy | ProviderCache::OpenAiModern
    ) && provider_name != "deepseek" && !managed_local
    {
        return;
    }
    if body.get("stream_options").is_none_or(Value::is_null) {
        body["stream_options"] = json!({});
    }
    if let Some(options) = body["stream_options"].as_object_mut() {
        options.entry("include_usage").or_insert(json!(true));
    }
}

/// Sort object keys; preserve history, tool pairs, array order and string contents.
/// 固定对象键顺序；保留历史、工具配对、数组顺序和字符串原文。
pub(crate) fn canonicalize_request(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(canonicalize_request),
        Value::Object(fields) => {
            let mut sorted: Vec<_> = std::mem::take(fields).into_iter().collect();
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, mut value) in sorted {
                canonicalize_request(&mut value);
                fields.insert(key, value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(host: &str, protocol: &str) -> Value {
        json!({"providerId":"configured","baseUrl":format!("https://{host}/v1"),"protocol":protocol,"apiKey":"never-key-material"})
    }

    #[test]
    fn owned_local_streams_request_reported_usage_without_changing_generation_or_cache_policy() {
        let local = json!({"providerId":"kynxa-local-test", "baseUrl":"http://127.0.0.1:59123/v1", "protocol":"openai-completions"});
        let original = json!({"model":"test", "stream":true, "max_tokens":32,
            "messages":[{"role":"user","content":"hello"}], "tools":[]});
        let mut body = original.clone();
        include_stream_usage(&local, "test", "openai-completions", &mut body);
        assert_eq!(body["stream_options"]["include_usage"], true);
        body.as_object_mut().unwrap().remove("stream_options");
        assert_eq!(body, original);

        let mut external = local.clone(); external["providerId"] = json!("configured-local-api");
        include_stream_usage(&external, "test", "openai-completions", &mut body);
        assert_eq!(body, original);
        body["stream"] = json!(false);
        include_stream_usage(&local, "test", "openai-completions", &mut body);
        assert!(body.get("stream_options").is_none());
        body["stream"] = json!(true); body["stream_options"] = json!({"include_usage":false,"other":true});
        include_stream_usage(&local, "test", "openai-completions", &mut body);
        assert_eq!(body["stream_options"], json!({"include_usage":false,"other":true}));
    }

    #[test]
    fn unknown_compatible_endpoints_and_local_models_receive_no_provider_cache_fields() {
        for (host, protocol, model) in [
            ("proxy.invalid", "openai-responses", "gpt-6.1-sol"),
            (
                "api.openai.com.proxy.invalid",
                "openai-completions",
                "gpt-5.4",
            ),
            ("127.0.0.1", "anthropic-messages", "claude-sonnet-4-5"),
            ("api.openai.com", "openai-responses", "unknown-model"),
            (
                "api.anthropic.com",
                "openai-completions",
                "claude-sonnet-4-5",
            ),
        ] {
            let mut body = json!({"instructions":"stable volatile","input":[{"role":"user","content":"hello"}]});
            let before = body.clone();
            let result = apply(
                &connection(host, protocol),
                model,
                protocol,
                &mut body,
                "work/chat",
                "stable",
            );
            assert_eq!(body, before);
            assert_eq!(result["mode"], "unavailable");
        }
    }

    #[test]
    fn official_openai_generation_controls_are_separate_and_preserve_scope_and_input() {
        let source = json!({"instructions":"stable\nclock=1","input":[{"role":"user","content":"hello"}],"tools":[]});
        let mut modern = source.clone();
        let endpoint = connection("api.openai.com", "openai-responses");
        let result = apply(
            &endpoint,
            "gpt-6.1-sol",
            "openai-responses",
            &mut modern,
            "private-disk/work/chat",
            "stable",
        );
        assert_eq!(result["mode"], "implicit");
        assert_eq!(modern["prompt_cache_options"]["mode"], "implicit");
        assert_eq!(modern["prompt_cache_options"]["ttl"], "30m");
        assert!(modern.get("prompt_cache_retention").is_none());
        assert_eq!(modern["instructions"], source["instructions"]);
        assert_eq!(modern["input"], source["input"]);
        assert_eq!(result["breakpoints"], 0);
        let key = modern["prompt_cache_key"].as_str().unwrap();
        assert!(
            key.len() <= 64 && !key.contains("private-disk") && !key.contains("never-key-material")
        );
        let mut repeated = modern.clone();
        apply(
            &endpoint,
            "gpt-6.1-sol",
            "openai-responses",
            &mut repeated,
            "private-disk/work/chat",
            "stable",
        );
        assert_eq!(modern, repeated);
        let mut other = source.clone();
        apply(
            &endpoint,
            "gpt-6.1-sol",
            "openai-responses",
            &mut other,
            "other-work/chat",
            "stable",
        );
        assert_ne!(modern["prompt_cache_key"], other["prompt_cache_key"]);
        let mut older = source.clone();
        apply(
            &endpoint,
            "gpt-5.4",
            "openai-responses",
            &mut older,
            "private-disk/work/chat",
            "stable",
        );
        assert!(older.get("prompt_cache_key").is_some());
        assert!(older.get("prompt_cache_options").is_none());
        assert_eq!(older["instructions"], source["instructions"]);
        let mut chat = json!({"messages":[{"role":"system","content":"stable"}]});
        apply(
            &connection("api.openai.com", "openai-completions"),
            "gpt-5.6",
            "openai-completions",
            &mut chat,
            "scope",
            "stable",
        );
        assert!(chat.get("prompt_cache_options").is_none());
    }

    #[test]
    fn invalidated_prefixes_skip_explicit_markers_without_errors_or_context_padding() {
        let mut body = json!({"instructions":"compacted instructions","input":[{"role":"user","content":"current"}]});
        apply(
            &connection("api.openai.com", "openai-responses"),
            "gpt-5.6",
            "openai-responses",
            &mut body,
            "scope",
            "old prefix",
        );
        assert_eq!(body["instructions"], "compacted instructions");
        assert_eq!(body["prompt_cache_options"]["mode"], "implicit");
        let mut unscoped = body.clone();
        unscoped.as_object_mut().unwrap().remove("prompt_cache_key");
        let before = unscoped.clone();
        apply(
            &connection("api.openai.com", "openai-responses"),
            "gpt-5.6",
            "openai-responses",
            &mut unscoped,
            "",
            "compacted",
        );
        assert_eq!(unscoped, before);
    }

    #[test]
    fn stateful_responses_preserve_replacement_instructions_without_adding_developer_history() {
        let endpoint = connection("api.openai.com", "openai-responses");
        let source = json!({"instructions":"stable\nclock=2; memory=revoked","store":false,
            "input":[{"type":"reasoning","encrypted_content":"opaque-signature"},
                {"role":"user","content":"continue"}]});
        for state in [
            json!({"previous_response_id":"resp-previous"}),
            json!({"conversation":"conv-existing"}),
            json!({"store":true}),
            json!({"input":[{"type":"item_reference","id":"msg-existing"},
                {"role":"user","content":"continue"}]}),
        ] {
            let mut body = source.clone();
            for (key, value) in state.as_object().unwrap() {
                body[key] = value.clone();
            }
            let instructions = body["instructions"].clone();
            let history = body["input"].clone();
            let policy = apply(
                &endpoint,
                "gpt-6.1-sol",
                "openai-responses",
                &mut body,
                "scope",
                "stable",
            );
            assert_eq!(policy["mode"], "implicit");
            assert_eq!(body["instructions"], instructions);
            assert_eq!(body["input"], history);
            assert!(body.get("prompt_cache_key").is_some());
            assert_eq!(body["prompt_cache_options"]["mode"], "implicit");
            let first = body.clone();
            apply(
                &endpoint,
                "gpt-6.1-sol",
                "openai-responses",
                &mut body,
                "scope",
                "stable",
            );
            assert_eq!(body, first);
        }
    }

    #[test]
    fn responses_cache_preserves_instruction_structure_and_every_existing_message_role() {
        let mut body = json!({"instructions":"stable\ncurrent permissions and identity","store":false,
            "input":[{"role":"developer","content":[{"type":"input_text","text":"existing developer guidance"}]},
                {"role":"user","content":"continue"},
                {"type":"reasoning","encrypted_content":"opaque-signature"},
                {"type":"function_call","call_id":"c1","name":"reader","arguments":"{\"path\":\"a\"}"},
                {"type":"function_call_output","call_id":"c1","output":"observed"},
                {"role":"assistant","content":[{"type":"output_text","text":"saved output"}]}]});
        let original = body.clone();
        apply(&connection("api.openai.com", "openai-responses"), "gpt-6.1-sol", "openai-responses",
            &mut body, "scope", "stable");
        assert_eq!(body["instructions"], original["instructions"]);
        assert_eq!(body["input"], original["input"]);
        assert_eq!(body["store"], false);
        assert!(!body.to_string().contains("prompt_cache_breakpoint"));
    }

    #[test]
    fn anthropic_caches_stable_blocks_and_history_without_freezing_a_changing_system_suffix() {
        let mut body = json!({"system":"fixed\nclock=1","tools":[{"name":"reader","input_schema":{"type":"object"}}],
            "messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"opaque","signature":"signed"}]},
            {"role":"user","content":[{"type":"image","source":{"type":"base64","data":"opaque"}},{"type":"text","text":"question"}]}]});
        let messages = body["messages"].clone();
        let result = apply(
            &connection("api.anthropic.com", "anthropic-messages"),
            "claude-sonnet-4-5",
            "anthropic-messages",
            &mut body,
            "scope",
            "fixed",
        );
        assert_eq!(result["breakpoints"], 3);
        assert_eq!(
            body["messages"][1]["content"][1]["cache_control"]["type"],
            "ephemeral"
        );
        let mut clean_messages = body["messages"].clone();
        clean_messages[1]["content"][1]
            .as_object_mut()
            .unwrap()
            .remove("cache_control");
        assert_eq!(clean_messages, messages);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["system"][1].get("cache_control").is_none());
        assert!(body["tools"][0].get("cache_control").is_some());
        assert!(body.get("cache_control").is_none());
        let before = body.clone();
        apply(
            &connection("api.anthropic.com", "anthropic-messages"),
            "claude-sonnet-4-5",
            "anthropic-messages",
            &mut body,
            "scope",
            "fixed",
        );
        assert_eq!(body, before);
    }

    #[test]
    fn anthropic_long_task_history_anchor_advances_across_tool_turns_and_keeps_refreshed_context() {
        let base = json!({"system":"fixed\nclock=1; permission=read","tools":[{"name":"reader","input_schema":{"type":"object"}}],
            "messages":[{"role":"user","content":[{"type":"text","text":"finish the task"}]},
            {"role":"assistant","content":[{"type":"thinking","thinking":"opaque","signature":"signed"},
                {"type":"tool_use","id":"call-1","name":"reader","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"first result"}]}]});
        let connection = connection("api.anthropic.com", "anthropic-messages");
        let mut first = base.clone();
        apply(
            &connection,
            "claude-sonnet-4-5",
            "anthropic-messages",
            &mut first,
            "scope",
            "fixed",
        );
        assert_eq!(marker_count(&first), 3);
        assert_eq!(
            first["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        let mut next = base.clone();
        next["system"] = json!("fixed\nclock=2; permission=restricted; memory=revoked");
        next["messages"].as_array_mut().unwrap().extend([
            json!({"role":"assistant","content":[{"type":"tool_use","id":"call-2","name":"reader","input":{}}]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call-2","content":"second result"}]})]);
        let original_history = next["messages"].clone();
        apply(
            &connection,
            "claude-sonnet-4-5",
            "anthropic-messages",
            &mut next,
            "scope",
            "fixed",
        );
        assert_eq!(marker_count(&next), 3);
        assert!(
            next["messages"][2]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(
            next["messages"][4]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        let mut clean_history = next["messages"].clone();
        clean_history[4]["content"][0]
            .as_object_mut()
            .unwrap()
            .remove("cache_control");
        assert_eq!(clean_history, original_history);
        assert_eq!(
            next["system"]
                .as_array()
                .unwrap()
                .iter()
                .map(|block| block["text"].as_str().unwrap())
                .collect::<String>(),
            "fixed\nclock=2; permission=restricted; memory=revoked"
        );
    }

    #[test]
    fn anthropic_never_exceeds_four_breakpoints_or_marks_thinking_and_image_blocks() {
        let mut full = json!({"system":"stable","tools":(0..4).map(|n|json!({"name":format!("tool{n}"),
            "input_schema":{"type":"object"},"cache_control":{"type":"ephemeral"}})).collect::<Vec<_>>(),
            "messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"opaque","signature":"signed"}]}]});
        let original = full.clone();
        apply(
            &connection("api.anthropic.com", "anthropic-messages"),
            "claude-opus-4-6",
            "anthropic-messages",
            &mut full,
            "scope",
            "stable",
        );
        assert_eq!(full, original);
        assert_eq!(marker_count(&full), 4);
        assert!(
            full["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(full["messages"][0]["content"][0]["signature"], "signed");
        for kind in ["thinking", "redacted_thinking", "image"] {
            let mut body = json!({"system":"stable","messages":[{"role":"assistant","content":[{"type":kind,"data":"opaque"}]}]});
            apply(
                &connection("api.anthropic.com", "anthropic-messages"),
                "claude-opus-4-6",
                "anthropic-messages",
                &mut body,
                "scope",
                "stable",
            );
            assert!(
                body["messages"][0]["content"][0]
                    .get("cache_control")
                    .is_none()
            );
        }
        let mut extended = json!({"system":[{"type":"text","text":"stable","cache_control":{"type":"ephemeral","ttl":"1h"}}],
            "tools":[{"name":"reader","input_schema":{"type":"object"}}],"messages":[{"role":"user","content":"question"}]});
        let before = extended.clone();
        apply(
            &connection("api.anthropic.com", "anthropic-messages"),
            "claude-opus-4-6",
            "anthropic-messages",
            &mut extended,
            "scope",
            "stable",
        );
        assert_eq!(extended, before);
        let mut automatic = json!({"system":"stable","cache_control":{"type":"ephemeral"},
            "tools":[{"name":"reader","input_schema":{"type":"object"}}],"messages":[{"role":"user","content":"question"}]});
        let before = automatic.clone();
        apply(
            &connection("api.anthropic.com", "anthropic-messages"),
            "claude-opus-4-6",
            "anthropic-messages",
            &mut automatic,
            "scope",
            "stable",
        );
        assert_eq!(automatic, before);
    }

    #[test]
    fn automatic_providers_need_no_anthropic_or_openai_extension_fields() {
        for (host, model) in [
            ("api.deepseek.com", "deepseek-v4-flash"),
            ("generativelanguage.googleapis.com", "gemini-2.5-pro"),
        ] {
            let mut body = json!({"messages":[{"role":"system","content":"stable"},{"role":"user","content":"question"}]});
            let before = body.clone();
            assert_eq!(
                apply(
                    &connection(host, "openai-completions"),
                    model,
                    "openai-completions",
                    &mut body,
                    "scope",
                    "stable"
                )["mode"],
                "automatic"
            );
            assert_eq!(body, before);
        }
    }

    #[test]
    fn canonical_objects_keep_history_tool_pair_order_and_signed_strings_exact() {
        let mut first: Value = serde_json::from_str(r#"{"z":1,"input":[{"role":"assistant","content":[{"signature":"opaque\\nbytes","type":"thinking"},{"id":"c1","type":"tool_use"}]},{"role":"user","content":[{"tool_use_id":"c1","type":"tool_result"}]}],"a":{"z":1,"a":2}}"#).unwrap();
        let mut reordered: Value = serde_json::from_str(r#"{"a":{"a":2,"z":1},"input":[{"content":[{"type":"thinking","signature":"opaque\\nbytes"},{"type":"tool_use","id":"c1"}],"role":"assistant"},{"content":[{"type":"tool_result","tool_use_id":"c1"}],"role":"user"}],"z":1}"#).unwrap();
        let history = first["input"].clone();
        canonicalize_request(&mut first);
        canonicalize_request(&mut reordered);
        assert_eq!(first.to_string(), reordered.to_string());
        assert_eq!(first["input"], history);
        assert_eq!(
            first.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["a", "input", "z"]
        );
    }

    #[test]
    fn official_chat_stream_usage_is_requested_without_changing_unknown_proxy_or_non_streaming_requests()
     {
        for (host, model) in [
            ("api.openai.com", "gpt-5.4"),
            ("api.deepseek.com", "deepseek-v4-flash"),
        ] {
            let mut body = json!({"stream":true,"messages":[{"role":"user","content":"hello"}]});
            include_stream_usage(
                &connection(host, "openai-completions"),
                model,
                "openai-completions",
                &mut body,
            );
            assert_eq!(body["stream_options"]["include_usage"], true);
        }
        for (host, stream) in [("proxy.invalid", true), ("api.openai.com", false)] {
            let mut body = json!({"stream":stream,"messages":[]});
            let before = body.clone();
            include_stream_usage(
                &connection(host, "openai-completions"),
                "gpt-5.4",
                "openai-completions",
                &mut body,
            );
            assert_eq!(body, before);
        }
    }
}
