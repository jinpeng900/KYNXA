//! Native roles for appended runtime facts depend on the verified endpoint, wire protocol and model.
//! 追加运行事实的原生角色取决于已核验端点、协议和模型，不能从显示名称或代理标签推断。

use serde_json::Value;

fn verified_endpoint(connection: &Value) -> Option<url::Url> {
    let endpoint = url::Url::parse(connection["baseUrl"].as_str()?).ok()?;
    (endpoint.scheme() == "https"
        && endpoint.username().is_empty()
        && endpoint.password().is_none()
        && endpoint.query().is_none()
        && endpoint.fragment().is_none()
        && endpoint.port_or_known_default() == Some(443))
    .then_some(endpoint)
}

fn model_family(model: &str, family: &str) -> bool {
    model == family
        || model
            .strip_prefix(family)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

fn documented_model(model: &str, models: &[&str]) -> bool {
    models.iter().any(|id| {
        model == *id
            || model
                .strip_prefix(*id)
                .and_then(|suffix| suffix.strip_prefix('-'))
                .is_some_and(|suffix| {
                    suffix == "latest"
                        || suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
    })
}

fn openai_role(model: &str) -> &'static str {
    // Preview/mini o1 templates precede developer-role support; do not extrapolate to future generations.
    // o1 的旧 preview/mini 模板早于 developer 支持；也不能把当前规则外推到未知未来代际。
    if model_family(model, "o1-preview") || model_family(model, "o1-mini") {
        return "user";
    }
    if [
        "gpt-4.1", "gpt-5", "gpt-5.1", "gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.5", "gpt-5.6",
        "gpt-6", "gpt-6.1", "o1", "o3", "o4",
    ]
    .iter()
    .any(|family| model_family(model, family))
    {
        "developer"
    } else if model_family(model, "gpt-4o") {
        "system"
    } else {
        "user"
    }
}

/// Choose only the role for broker-owned text facts; this does not grant authority or construct a message.
/// 仅选择代理自身文本事实的角色，不增加授权，也不负责创建消息。
/// Permissions and memory revocation remain in the current top-level system guidance and execution checks.
/// 权限与记忆撤销仍由当前顶层系统指导和执行核验负责，不因此降级或冻结。
/// Messages adapters must place a native system update after a user/tool-result turn, outside tool-call pairs.
/// Messages 适配器必须把原生 system 更新放在 user/工具结果轮之后，不能插入工具调用配对内部。
pub fn runtime_message_role(connection: &Value, model: &str) -> &'static str {
    let Some(endpoint) = verified_endpoint(connection) else {
        return "user";
    };
    let protocol = connection["protocol"].as_str().unwrap_or("openai-completions");
    let path = endpoint.path().trim_end_matches('/');
    match (endpoint.host_str(), protocol) {
        (Some("api.openai.com"), "openai-completions" | "openai-responses")
            if matches!(path, "" | "/v1") =>
        {
            openai_role(model)
        }
        (Some("api.deepseek.com"), "openai-completions" | "openai-responses")
            if matches!(path, "" | "/v1")
                && matches!(
                    model,
                    "deepseek-flash" | "deepseek-v4-pro" | "deepseek-chat" | "deepseek-reasoner"
                ) =>
        {
            // DeepSeek Responses treats developer as user; its documented system role preserves the intended role.
            // DeepSeek Responses 将 developer 当作 user，须使用其明确支持的 system 角色。
            "system"
        }
        (Some("api.deepseek.com"), "anthropic-messages")
            if matches!(path, "/anthropic" | "/anthropic/v1") && model == "deepseek-flash" =>
        {
            // Pinned official Harness 687f282 advertises in-history system updates only for this default model.
            // 固定官方 Harness 687f282 的默认目录仅为此模型声明历史内 system 更新，不能扩展给 V4 Pro。
            "system"
        }
        (Some("api.anthropic.com"), "anthropic-messages")
            if matches!(path, "" | "/v1")
                && documented_model(
                    model,
                    &[
                        "claude-opus-4-8",
                        "claude-opus-5",
                        "claude-opus-5-5",
                        "claude-sonnet-5-5",
                        "claude-haiku-5-5",
                        "claude-fable-5",
                        "claude-fable-5-1",
                        "claude-mythos-5",
                        "claude-mythos-5-1",
                    ],
                ) =>
        {
            // Sonnet 5 is explicitly excluded by the official mid-conversation-system-message contract.
            // 官方会话中途 system 消息合同明确排除 Sonnet 5，不能按版本大小笼统放行。
            "system"
        }
        _ => "user",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn connection(base: &str, protocol: &str) -> Value {
        json!({"baseUrl":base,"protocol":protocol,"providerId":"display-label-is-not-authority"})
    }

    #[test]
    fn official_openai_roles_follow_known_model_templates() {
        for protocol in ["openai-completions", "openai-responses"] {
            let connection = connection("https://api.openai.com/v1", protocol);
            for model in ["gpt-6.1-sol", "gpt-6-astra", "gpt-5.6", "gpt-5-mini", "gpt-4.1", "o3"] {
                assert_eq!(runtime_message_role(&connection, model), "developer", "{model}");
            }
            assert_eq!(runtime_message_role(&connection, "gpt-4o"), "system");
            for model in ["o1-mini", "o1-preview-2024-09-12", "gpt-6.2", "gpt-7", "custom-model"] {
                assert_eq!(runtime_message_role(&connection, model), "user", "{model}");
            }
        }
    }

    #[test]
    fn deepseek_roles_follow_each_routes_actual_wire_grammar() {
        for protocol in ["openai-completions", "openai-responses"] {
            let connection = connection("https://api.deepseek.com", protocol);
            assert_eq!(runtime_message_role(&connection, "deepseek-flash"), "system");
            assert_eq!(runtime_message_role(&connection, "deepseek-v4-pro"), "system");
            assert_eq!(runtime_message_role(&connection, "claude-opus-4-8"), "user");
        }
        let messages = connection("https://api.deepseek.com/anthropic/", "anthropic-messages");
        assert_eq!(runtime_message_role(&messages, "deepseek-flash"), "system");
        assert_eq!(runtime_message_role(&messages, "deepseek-v4-pro"), "user");
        assert_eq!(runtime_message_role(&messages, "claude-opus-4-8"), "user");
        let wrong_path = connection("https://api.deepseek.com", "anthropic-messages");
        assert_eq!(runtime_message_role(&wrong_path, "deepseek-flash"), "user");
    }

    #[test]
    fn anthropic_mid_conversation_system_role_uses_explicit_model_boundaries() {
        let connection = connection("https://api.anthropic.com/v1", "anthropic-messages");
        for model in [
            "claude-opus-4-8", "claude-opus-5-5", "claude-sonnet-5-5",
            "claude-haiku-5-5", "claude-fable-5-1", "claude-mythos-5-1",
            "claude-opus-4-8-20260601",
        ] {
            assert_eq!(runtime_message_role(&connection, model), "system", "{model}");
        }
        for model in [
            "claude-sonnet-5", "claude-sonnet-5-20260601", "claude-opus-4-7",
            "claude-opus-4-80", "claude-opus-4-9", "claude-opus-5-1", "claude-sonnet-6",
        ] {
            assert_eq!(runtime_message_role(&connection, model), "user", "{model}");
        }
    }

    #[test]
    fn unverified_hosts_templates_protocols_and_endpoint_metadata_stay_user_data() {
        for base in [
            "http://api.openai.com/v1", "https://api.openai.com:444/v1",
            "https://api.openai.com.proxy.invalid/v1", "https://proxy.invalid/v1",
            "http://127.0.0.1:8080/v1", "https://user:secret@api.openai.com/v1",
            "https://api.openai.com/v1?key=private", "https://api.openai.com/v1#fragment",
            "https://api.openai.com/custom", "not-an-endpoint",
        ] {
            let connection = connection(base, "openai-responses");
            assert_eq!(runtime_message_role(&connection, "gpt-6.1-sol"), "user", "{base}");
        }
        for protocol in ["ollama", "anthropic-messages", "unknown"] {
            let connection = connection("https://api.openai.com/v1", protocol);
            assert_eq!(runtime_message_role(&connection, "gpt-6.1-sol"), "user", "{protocol}");
        }
        assert_eq!(runtime_message_role(&Value::Null, "gpt-6.1-sol"), "user");
        assert_eq!(runtime_message_role(&json!({"providerId":"openai"}), "gpt-6.1-sol"), "user");
    }
}
