//! 追加运行事实的原生角色取决于已核验端点、协议和模型，与显示名称和代理标签无关。

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
    // o1 的旧 preview/mini 模板早于 developer 支持；未知的未来代际也不套用当前规则。
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

/// 仅选择代理自身文本事实的角色。
/// 权限与记忆撤销仍由当前顶层系统指导和执行核验负责。
/// Messages 适配器要把原生 system 更新放在 user/工具结果轮之后，工具调用配对内部不插入。
pub fn runtime_message_role(connection: &Value, model: &str) -> &'static str {
    let Some(endpoint) = verified_endpoint(connection) else {
        return "user";
    };
    let protocol = connection["protocol"]
        .as_str()
        .unwrap_or("openai-completions");
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
            // DeepSeek Responses 将 developer 当作 user，须使用其明确支持的 system 角色。
            "system"
        }
        (Some("api.deepseek.com"), "anthropic-messages")
            if matches!(path, "/anthropic" | "/anthropic/v1") && model == "deepseek-flash" =>
        {
            // 固定官方 Harness 687f282 的默认目录仅为此模型声明历史内 system 更新，V4 Pro 不适用。
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
            // 官方会话中途 system 消息约定明确排除 Sonnet 5。
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
            for model in [
                "gpt-6.1-sol",
                "gpt-6-astra",
                "gpt-5.6",
                "gpt-5-mini",
                "gpt-4.1",
                "o3",
            ] {
                assert_eq!(
                    runtime_message_role(&connection, model),
                    "developer",
                    "{model}"
                );
            }
            assert_eq!(runtime_message_role(&connection, "gpt-4o"), "system");
            for model in [
                "o1-mini",
                "o1-preview-2024-09-12",
                "gpt-6.2",
                "gpt-7",
                "custom-model",
            ] {
                assert_eq!(runtime_message_role(&connection, model), "user", "{model}");
            }
        }
    }

    #[test]
    fn deepseek_roles_follow_each_routes_actual_wire_grammar() {
        for protocol in ["openai-completions", "openai-responses"] {
            let connection = connection("https://api.deepseek.com", protocol);
            assert_eq!(
                runtime_message_role(&connection, "deepseek-flash"),
                "system"
            );
            assert_eq!(
                runtime_message_role(&connection, "deepseek-v4-pro"),
                "system"
            );
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
            "claude-opus-4-8",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "claude-opus-4-8-20260601",
        ] {
            assert_eq!(
                runtime_message_role(&connection, model),
                "system",
                "{model}"
            );
        }
        for model in [
            "claude-sonnet-5",
            "claude-sonnet-5-20260601",
            "claude-opus-4-7",
            "claude-opus-4-80",
            "claude-opus-4-9",
            "claude-opus-5-1",
            "claude-sonnet-6",
        ] {
            assert_eq!(runtime_message_role(&connection, model), "user", "{model}");
        }
    }

    #[test]
    fn unverified_hosts_templates_protocols_and_endpoint_metadata_stay_user_data() {
        for base in [
            "http://api.openai.com/v1",
            "https://api.openai.com:444/v1",
            "https://api.openai.com.proxy.invalid/v1",
            "https://proxy.invalid/v1",
            "http://127.0.0.1:8080/v1",
            "https://user:secret@api.openai.com/v1",
            "https://api.openai.com/v1?key=private",
            "https://api.openai.com/v1#fragment",
            "https://api.openai.com/custom",
            "not-an-endpoint",
        ] {
            let connection = connection(base, "openai-responses");
            assert_eq!(
                runtime_message_role(&connection, "gpt-6.1-sol"),
                "user",
                "{base}"
            );
        }
        for protocol in ["ollama", "anthropic-messages", "unknown"] {
            let connection = connection("https://api.openai.com/v1", protocol);
            assert_eq!(
                runtime_message_role(&connection, "gpt-6.1-sol"),
                "user",
                "{protocol}"
            );
        }
        assert_eq!(runtime_message_role(&Value::Null, "gpt-6.1-sol"), "user");
        assert_eq!(
            runtime_message_role(&json!({"providerId":"openai"}), "gpt-6.1-sol"),
            "user"
        );
    }
}
