//! Opt-in request routes that serve a configured connection through a verified alternative endpoint.
//! 可选的请求路线：经核验的替代端点承载已配置连接；保存的连接配置、密钥与模型名不变。

use serde_json::{Value, json};

/// Default to Messages for deepseek-flash's verified tool updates; "0" disables routing, "1" skips the tool-update check.
/// 默认仅为已核验工具更新的 deepseek-flash 改用 Messages；"0" 关闭改道，"1" 不要求工具更新已核验。
const DEEPSEEK_MESSAGES_SWITCH: &str = "KYNXA_DEEPSEEK_MESSAGES_ROUTE";
const DEEPSEEK_MESSAGES_BASE_URL: &str = "https://api.deepseek.com/anthropic";

fn official_deepseek_root(base_url: &str) -> bool {
    url::Url::parse(base_url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("api.deepseek.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && ["", "/", "/v1", "/v1/"].contains(&url.path())
    })
}

/// Select the route once per request series; callers keep the projected connection for the whole series.
/// 每个请求序列只选择一次路线；调用方在整个序列内沿用投影后的连接，执行中不改道。
pub(crate) fn project_connection(connection: &mut Value, model: &str) {
    let enabled = match std::env::var(DEEPSEEK_MESSAGES_SWITCH).as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        // Default: route only the model whose tool additions are verified to keep the cache.
        // 默认只改道已核验新增工具不断缓存的模型。
        _ => model == "deepseek-flash",
    };
    project_connection_with(connection, model, enabled);
}

fn project_connection_with(connection: &mut Value, model: &str, enabled: bool) {
    let base_url = connection["baseUrl"].as_str().unwrap_or("").to_owned();
    if !enabled
        || connection["routeProjection"].is_object()
        || connection["protocol"].as_str().unwrap_or("openai-completions") != "openai-completions"
        || !official_deepseek_root(&base_url)
        || !["deepseek-flash", "deepseek-v4-pro"].contains(&model)
    {
        return;
    }
    // Mid-conversation tool additions are verified for deepseek-flash; other models keep rebuilding declarations.
    // 中途新增工具目前只对 deepseek-flash 核验；其他模型仍按原方式重建声明。
    let tool_update = (model == "deepseek-flash").then_some("addition-only");
    connection["routeProjection"] = json!({"kind":"deepseek-messages","originalProtocol":"openai-completions",
        "originalBaseUrl":base_url,"toolUpdate":tool_update});
    connection["protocol"] = json!("anthropic-messages");
    connection["baseUrl"] = json!(DEEPSEEK_MESSAGES_BASE_URL);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official() -> Value {
        json!({"providerId":"deepseek","baseUrl":"https://api.deepseek.com","protocol":"openai-completions","apiKey":"fixture"})
    }

    #[test]
    fn the_messages_route_applies_only_when_enabled_for_official_known_models() {
        let mut disabled = official();
        project_connection_with(&mut disabled, "deepseek-flash", false);
        assert_eq!(disabled, official(), "The switch off keeps the configured route byte-for-byte.");
        let mut flash = official();
        project_connection_with(&mut flash, "deepseek-flash", true);
        assert_eq!(flash["protocol"], "anthropic-messages");
        assert_eq!(flash["baseUrl"], DEEPSEEK_MESSAGES_BASE_URL);
        assert_eq!(flash["apiKey"], "fixture");
        assert_eq!(flash["routeProjection"]["toolUpdate"], "addition-only");
        assert_eq!(flash["routeProjection"]["originalBaseUrl"], "https://api.deepseek.com");
        let mut again = flash.clone();
        project_connection_with(&mut again, "deepseek-flash", true);
        assert_eq!(again, flash, "Projection is idempotent within one series.");
        let mut pro = official();
        project_connection_with(&mut pro, "deepseek-v4-pro", true);
        assert_eq!(pro["protocol"], "anthropic-messages");
        assert!(pro["routeProjection"]["toolUpdate"].is_null());
        // The routed endpoint keeps the official window, so compaction timing is unchanged.
        // 改道后的端点仍识别官方窗口，压缩时机不因路线改变。
        let endpoint = |path: &str| json!({"protocol":"https:","hostname":"api.deepseek.com","port":"","pathname":path,
            "username":"","password":"","search":"","hash":""});
        let routed = super::super::capabilities::calculate("models.capabilities.lookup",
            &json!({"endpoint":endpoint("/anthropic"),"model":"deepseek-flash"})).unwrap();
        let original = super::super::capabilities::calculate("models.capabilities.lookup",
            &json!({"endpoint":endpoint("/"),"model":"deepseek-flash"})).unwrap();
        assert_eq!(routed, original);
        assert!(!routed.as_object().unwrap().is_empty());
        for (connection, model) in [
            (json!({"baseUrl":"https://proxy.example.com/v1","protocol":"openai-completions"}), "deepseek-flash"),
            (json!({"baseUrl":"https://api.deepseek.com","protocol":"openai-responses"}), "deepseek-flash"),
            (official(), "deepseek-unknown"),
        ] {
            let mut projected = connection.clone();
            project_connection_with(&mut projected, model, true);
            assert_eq!(projected, connection, "{connection} {model}");
        }
    }
}
