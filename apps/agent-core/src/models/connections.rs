use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};

use regex::Regex;
use serde_json::{Value, json};

use super::{
    output,
    protocol::{array, string, truthy},
};

/// Identify application-managed local providers without starting an engine.
/// 仅判断自管本地连接，不触发引擎或进程操作。
pub fn is_managed_local(connection: &Value) -> bool {
    connection["providerId"].as_str().is_some_and(|id| id.starts_with("kynxa-local-"))
}

fn error(message: &str) -> Value {
    json!({"error": {"message": message}})
}
fn private_v4(ip: Ipv4Addr) -> bool {
    let [first, second, _, _] = ip.octets();
    first == 127
        || first == 10
        || first == 172 && (16..=31).contains(&second)
        || first == 192 && second == 168
}
pub fn local_host(hostname: &str) -> bool {
    let hostname = hostname.trim_matches(['[', ']']).to_lowercase();
    if hostname == "localhost" || hostname == "::1" {
        return true;
    }
    match hostname.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => private_v4(ip),
        Ok(IpAddr::V6(ip)) => ip
            .to_ipv4_mapped()
            .map(private_v4)
            .unwrap_or_else(|| ip.segments()[0] & 0xfe00 == 0xfc00),
        _ => false,
    }
}
fn public_provider(provider: &Value) -> Value {
    let mut result = provider.clone();
    let Some(object) = result.as_object_mut() else {
        return error("模型连接记录格式无效，原记录已保留。");
    };
    let api_key = object.remove("apiKey").unwrap_or(Value::Null);
    result["hasApiKey"] = json!(truthy(&api_key));
    result["protocol"] = provider
        .get("protocol")
        .filter(|protocol| !protocol.is_null())
        .cloned()
        .unwrap_or(json!("openai-completions"));
    result["contextWindowMode"] = json!("automatic");
    // Public legacy defaults keep the desktop integer contract; runtime capacity remains automatic.
    // 公共旧字段默认值保持桌面整数合同；运行时额度仍由自动能力与观察决定，不改写原配置。
    result["contextWindowTokens"] = provider
        .get("contextWindowTokens")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or(json!(32768));
    result["maxOutputTokens"] = provider
        .get("maxOutputTokens")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or(json!(262144));
    result
}
pub(crate) fn valid_model(value: &Value) -> bool {
    value.as_str().is_some_and(|id| {
        !id.is_empty()
            && id.encode_utf16().count() <= 160
            && !id.chars().any(|character| {
                character.is_whitespace() && character != '\u{0085}'
                    || character == '\u{feff}'
                    || character <= '\u{001f}'
            })
    })
}

/// Validate credentials, endpoints and retained defaults here; adapters only persist them.
/// 此处校验凭据、端点和保留的默认值；适配器只负责持久化。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    let value = match operation {
        "models.connections.localEndpoint" => json!(local_host(string(&args["hostname"]))),
        "models.connections.validate" => {
            let input = &args["input"];
            if !input.is_object() {
                return Ok(error("连接信息不能为空。"));
            }
            if input.get("maxOutputTokens").is_some() {
                output::calculate(
                    "context.outputValidate",
                    &json!({"value": input["maxOutputTokens"]}),
                )?;
            }
            if input.get("contextWindowTokens").is_some()
                && input["contextWindowTokens"]
                    .as_u64()
                    .is_none_or(|value| !(2048..=2000000).contains(&value))
            {
                return Ok(error("模型上下文预算须为 2048–2000000 的整数。"));
            }
            if ![
                "openai-completions",
                "openai-responses",
                "anthropic-messages",
                "ollama",
            ]
            .contains(&string(&input["protocol"]))
            {
                return Ok(error("模型接口协议无效。"));
            }
            let provider = string(&input["providerId"]);
            if !Regex::new("^[a-z][a-z0-9-]{1,39}$")
                .unwrap()
                .is_match(provider)
            {
                return Ok(error(
                    "Provider ID 须为 2–40 位小写字母、数字或连字符，且以字母开头。",
                ));
            }
            let display = string(&input["displayName"]);
            if display.is_empty() || display.encode_utf16().count() > 80 {
                return Ok(error("连接名称须为 1–80 个字符。"));
            }
            let endpoint = &args["endpoint"];
            if endpoint.is_null() {
                return Ok(error("Base URL 格式无效。"));
            }
            if !["http:", "https:"].contains(&string(&endpoint["protocol"]))
                || ["username", "password", "search", "hash"]
                    .iter()
                    .any(|key| truthy(&endpoint[*key]))
            {
                return Ok(error(
                    "Base URL 只能是 HTTP(S) 地址，不能包含账号、查询参数或片段。",
                ));
            }
            if endpoint["protocol"] != "https:" && !local_host(string(&endpoint["hostname"])) {
                return Ok(error(
                    "公网连接必须使用 HTTPS；本机或局域网 IP 可使用 HTTP。",
                ));
            }
            let mut seen = HashSet::new();
            let models: Vec<Value> = array(&input["models"])
                .iter()
                .filter(|model| seen.insert(string(model).to_owned()))
                .cloned()
                .collect();
            if args["requireModels"] != false && models.is_empty()
                || models.len() > 10000
                || models.iter().any(|id| !valid_model(id))
            {
                return Ok(error("至少填写一个有效 Model ID，最多 10000 个。"));
            }
            if let Some(api_key) = input.get("apiKey") {
                let api_key = string(api_key);
                if api_key.encode_utf16().count() > 8192 || api_key.contains(['\r', '\n']) {
                    return Ok(error("API Key 格式无效。"));
                }
            }
            let mut result = input.clone();
            result["models"] = json!(models);
            result
        }
        "models.connections.document" => {
            let document = &args["document"];
            if document["version"] != 1 || !document["providers"].is_array() {
                return Ok(error("模型配置文件无效，请从备份恢复。"));
            }
            document.clone()
        }
        "models.connections.list" => json!(
            array(&args["document"]["providers"])
                .iter()
                .map(public_provider)
                .collect::<Vec<_>>()
        ),
        "models.connections.find" => {
            let provider = array(&args["document"]["providers"])
                .iter()
                .find(|provider| provider["providerId"] == args["providerId"]);
            if let Some(provider) = provider {
                json!({"found": true, "value": provider})
            } else {
                json!({"found": false})
            }
        }
        "models.connections.savedKey" => {
            let provider = array(&args["document"]["providers"])
                .iter()
                .find(|provider| {
                    provider["providerId"] == args["providerId"]
                        && provider["baseUrl"] == args["baseUrl"]
                });
            provider
                .and_then(|provider| provider.get("apiKey"))
                .map(|key| json!({"found": true, "value": key}))
                .unwrap_or(json!({"found": false}))
        }
        "models.connections.removePlan" => {
            let mut document = args["document"].clone();
            let providers = array(&document["providers"])
                .iter()
                .filter(|provider| provider["providerId"] != args["providerId"])
                .cloned()
                .collect::<Vec<_>>();
            document["providers"] = json!(providers);
            json!({"document": document})
        }
        "models.connections.savePlan" => {
            let connection = &args["connection"];
            let mut document = args["document"].clone();
            let previous = array(&document["providers"])
                .iter()
                .find(|provider| provider["providerId"] == connection["providerId"]);
            let api_key = connection
                .get("apiKey")
                .filter(|key| truthy(key))
                .or_else(|| {
                    previous
                        .filter(|previous| previous["baseUrl"] == connection["baseUrl"])
                        .and_then(|previous| previous.get("apiKey"))
                });
            if api_key.is_none_or(|key| !truthy(key)) && args["localEndpoint"] != true {
                return Ok(error("此连接需要 API Key。"));
            }
            let mut provider = connection.clone();
            if let Some(key) = api_key {
                provider["apiKey"] = key.clone();
            }
            if !connection
                .as_object()
                .unwrap()
                .contains_key("contextWindowTokens")
            {
                if let Some(value) =
                    previous.and_then(|previous| previous.get("contextWindowTokens"))
                {
                    provider["contextWindowTokens"] = value.clone();
                }
            }
            provider["maxOutputTokens"] = connection
                .get("maxOutputTokens")
                .filter(|value| !value.is_null())
                .or_else(|| {
                    previous
                        .and_then(|previous| previous.get("maxOutputTokens"))
                        .filter(|value| !value.is_null())
                })
                .cloned()
                .unwrap_or(json!(262144));
            let mut providers: Vec<Value> = array(&document["providers"])
                .iter()
                .filter(|item| item["providerId"] != connection["providerId"])
                .cloned()
                .collect();
            providers.push(provider.clone());
            document["providers"] = json!(providers);
            json!({"document": document, "provider": public_provider(&provider)})
        }
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    };
    Ok(json!({"value": value}))
}
