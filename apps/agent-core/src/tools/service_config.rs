use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde_json::{Value, json};
use url::Url;

use super::configuration::normalize_connection;
use super::intent::{js_trim, matches, utf16_length};
use super::local_files::{
    absolute_path, atomic_json, inspect, path_key, path_text, read_checked, sha256,
};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string};

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const OVERRIDE_FIELDS: &[&str] = &[
    "name",
    "command",
    "args",
    "enabled",
    "protocolVersion",
    "disabledTools",
    "transport",
    "cwd",
    "env",
    "envRefs",
    "url",
    "headerEnv",
    "auth",
    "startupTimeoutMs",
    "timeoutMs",
    "trusted",
    "backgroundTask",
    "prewarmPriority",
];
type PathLocks = Mutex<HashMap<String, Weak<Mutex<()>>>>;
static CONFIG_LOCKS: OnceLock<PathLocks> = OnceLock::new();

fn invalid(message: &str) -> ServiceError {
    ServiceError::new("INVALID_AGENT_CONFIG", message, 400)
}

#[cfg(test)]
mod lifecycle_configuration_tests {
    use super::*;

    #[test]
    fn execution_timeout_is_saved_separately_from_startup_and_bounded() {
        let source = json!({"id":"blender","name":"Blender","enabled":true,
            "transport":"stdio","command":"fixture.exe","args":[],"startupTimeoutMs":15000,"timeoutMs":1_800_000});
        let normalized = normalize_server(&source).unwrap();
        assert_eq!(normalized["timeoutMs"], 1_800_000);
        assert_eq!(normalized["startupTimeoutMs"], 15000);
        let mut omitted = source.clone();
        omitted.as_object_mut().unwrap().remove("timeoutMs");
        assert!(normalize_server(&omitted).unwrap().get("timeoutMs").is_none());
        let mut http = json!({"id":"remote","name":"Remote","enabled":true,"transport":"streamable-http",
            "url":"https://example.com/mcp","timeoutMs":600_000});
        assert_eq!(normalize_server(&http).unwrap()["timeoutMs"], 600_000);
        for value in [json!(999), json!(21_600_001), json!("60000"), json!(1.5)] {
            http["timeoutMs"] = value.clone();
            assert!(normalize_server(&http).is_err(), "{value}");
        }
    }

    #[test]
    fn lifecycle_policy_is_explicit_and_survives_config_normalization() {
        let source = json!({"id":"local-fixture","name":"Local fixture","enabled":true,
            "transport":"stdio","command":"fixture.exe","args":[],
            "trusted":true,"backgroundTask":true,"prewarmPriority":37});
        let normalized = normalize_server(&source).unwrap();
        assert_eq!(normalized["trusted"], true);
        assert_eq!(normalized["backgroundTask"], true);
        assert_eq!(normalized["prewarmPriority"], 37);
        let mut unsupported = source.clone();
        unsupported["protocolVersion"] = json!("2026-07-28");
        assert_eq!(
            normalize_server(&unsupported).unwrap()["protocolVersion"],
            "2026-07-28"
        );
        let mut legacy = source.clone();
        legacy["protocolVersion"] = json!("2025-06-18");
        assert_eq!(
            normalize_server(&legacy).unwrap()["protocolVersion"],
            "2025-06-18"
        );
        for protocol in [json!(20260728), json!("newest"), json!("2026-07-28\n")] {
            let mut malformed = source.clone();
            malformed["protocolVersion"] = protocol;
            assert!(normalize_server(&malformed).is_err());
        }
        for (field, value) in [
            ("trusted", json!("true")),
            ("backgroundTask", json!(1)),
            ("prewarmPriority", json!(1001)),
        ] {
            let mut invalid = source.clone();
            invalid[field] = value;
            assert!(normalize_server(&invalid).is_err());
        }
    }

    #[test]
    fn unsupported_mcp_revision_preserves_other_servers_and_config_round_trip() {
        let root = std::env::temp_dir().join(format!("kynxa-config-{}", uuid::Uuid::new_v4()));
        let file = root.join("Agent/config.json");
        let servers = (0..18)
            .map(|index| json!({"id":format!("fixture-{index}"),"name":format!("Fixture {index}"),
                "enabled":true,"transport":"stdio","command":format!("fixture-{index}.exe"),"args":[],
                "protocolVersion":if index == 17 {"2026-07-28"} else {"2025-11-25"}}))
            .collect::<Vec<_>>();
        let original = json!({"version":1,"revision":7,"mcpServers":servers,
            "skillDirectories":[],"disabledSkills":[]});
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let original_bytes = serde_json::to_vec(&original).unwrap();
        std::fs::write(&file, &original_bytes).unwrap();
        let mut repository = ConfigRepository::default();
        let loaded = repository
            .apply("config.initRead", &json!({"root":root}))
            .unwrap();
        assert_eq!(loaded["mcpServers"].as_array().unwrap().len(), 18);
        assert_eq!(loaded["mcpServers"][17]["protocolVersion"], "2026-07-28");
        assert_eq!(std::fs::read(&file).unwrap(), original_bytes);

        let mut mcp = super::super::service_mcp::McpState::default();
        let unsupported = mcp
            .apply("mcp.sdkOptions", &loaded["mcpServers"][17])
            .unwrap_err();
        assert_eq!(unsupported.code, "MCP_UNSUPPORTED_PROTOCOL");
        assert!(mcp.apply("mcp.sdkOptions", &loaded["mcpServers"][0]).is_ok());

        let mut input = loaded;
        input["expectedRevision"] = json!(7);
        let updated = repository
            .apply("config.update", &json!({"input":input}))
            .unwrap();
        assert_eq!(updated["revision"], 8);
        let persisted = repository.apply("config.read", &json!({})).unwrap();
        assert_eq!(persisted, updated);
        assert_eq!(persisted["mcpServers"][17]["protocolVersion"], "2026-07-28");

        let mut malformed = original;
        malformed["mcpServers"][17]["protocolVersion"] = json!(42);
        let malformed_bytes = serde_json::to_vec(&malformed).unwrap();
        std::fs::write(&file, &malformed_bytes).unwrap();
        assert_eq!(
            repository.apply("config.read", &json!({})).unwrap_err().code,
            "CORRUPT_AGENT_CONFIG"
        );
        assert_eq!(std::fs::read(&file).unwrap(), malformed_bytes);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
fn object(value: &Value) -> ServiceResult<()> {
    if value.is_object() {
        Ok(())
    } else {
        Err(ServiceError::new(
            "INVALID_TOOL_ARGUMENTS",
            "工具输入必须为对象。",
            400,
        ))
    }
}
fn text(value: &Value, label: &str, maximum: usize) -> ServiceResult<String> {
    value
        .as_str()
        .filter(|value| {
            !js_trim(value).is_empty()
                && utf16_length(value) <= maximum
                && !value.contains(['\0', '\r', '\n'])
        })
        .map(|value| js_trim(value).to_owned())
        .ok_or_else(|| invalid(&format!("{label}格式无效。")))
}
fn unique(values: &[Value]) -> Vec<Value> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(value) {
            result.push(value.clone());
        }
    }
    result
}
fn ids(value: &Value) -> ServiceResult<Vec<Value>> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid("官方工具启停列表无效。"))?;
    if values.len() > 64
        || values
            .iter()
            .any(|value| !matches(r"^[a-z][a-z0-9-]{1,39}$", string(value)))
    {
        return Err(invalid("官方工具启停列表无效。"));
    }
    Ok(unique(values))
}

fn url_projection(value: &Value) -> Value {
    let Some(value) = value.as_str() else {
        return Value::Null;
    };
    let Ok(url) = Url::parse(value) else {
        return Value::Null;
    };
    json!({"href":url.as_str(),"protocol":format!("{}:",url.scheme()),"hostname":url.host_str().unwrap_or(""),
        "username":url.username(),"password":url.password().unwrap_or(""),
        "search":url.query().map(|value| format!("?{value}")).unwrap_or_default(),
        "hash":url.fragment().map(|value| format!("#{value}")).unwrap_or_default()})
}

pub fn normalize_server(server: &Value) -> ServiceResult<Value> {
    object(server)?;
    let id = text(&server["id"], "MCP ID", 40)?;
    if !matches(r"^[a-z][a-z0-9-]{1,39}$", &id) {
        return Err(invalid("MCP ID 无效或重复。"));
    }
    let cwd = server["cwd"].as_str().map(absolute_path);
    let cwd_absolute = server["cwd"]
        .as_str()
        .is_some_and(|value| std::path::Path::new(value).is_absolute());
    let mut projected = server.clone();
    let mut special = serde_json::Map::new();
    for (key, value) in server.as_object().unwrap() {
        if value.get("invalidNumber") == Some(&json!(true)) {
            special.insert(key.clone(), json!("infinite"));
            projected[key] = Value::Null;
        }
    }
    if server["auth"]["scope"].get("invalidNumber") == Some(&json!(true)) {
        special.insert("auth.scope".into(), json!("infinite"));
        projected["auth"]["scope"] = Value::Null;
    }
    let connection = normalize_connection(&json!({"server":projected,"cwdAbsolute":cwd_absolute,
        "cwdResolved":cwd.as_ref().map(|path| path_text(path)),"url":url_projection(&server["url"]),
        "issuerUrl":url_projection(&server["auth"]["issuer"]),"nonFiniteNumbers":special}))
    .map_err(|code| {
        ServiceError::new(&code, "MCP 连接配置无效；密钥使用环境变量引用，个人 OAuth 使用浏览器登录。", 400)
    })?;
    if !server["enabled"].is_boolean() {
        return Err(invalid("请明确是否启用 MCP 服务。"));
    }
    let protocol = server
        .get("protocolVersion")
        .filter(|value| !value.is_null())
        .unwrap_or(&Value::Null);
    let protocol = if protocol.is_null() {
        "2025-11-25"
    } else {
        protocol.as_str().unwrap_or("")
    };
    // Persist well-formed revisions independently of this client's transport capabilities.
    // 配置保留合法协议日期；连接时再判断实现能力，单个不兼容服务不能使整份配置损坏。
    if !matches(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$", protocol) {
        return Err(invalid("MCP 协议版本须为 YYYY-MM-DD 日期格式。"));
    }
    let disabled = server
        .get("disabledTools")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let disabled = disabled
        .as_array()
        .ok_or_else(|| invalid("MCP 禁用工具列表无效。"))?;
    if disabled.len() > 4096
        || disabled
            .iter()
            .any(|value| !matches(r"^[a-zA-Z0-9_.-]{1,128}$", string(value)))
    {
        return Err(invalid("MCP 禁用工具列表无效。"));
    }
    let mut result = connection;
    result["id"] = json!(id);
    result["name"] = json!(text(&server["name"], "MCP name", 100)?);
    result["enabled"] = server["enabled"].clone();
    result["protocolVersion"] = json!(protocol);
    result["disabledTools"] = json!(unique(disabled));
    // Trust is a saved configuration decision, never inferred from a script name or repository text.
    // 信任来自已保存的明确配置，不从脚本名称、文件存在或仓库文字推断。
    for field in ["trusted", "backgroundTask"] {
        if server.get(field).is_some_and(|value| !value.is_boolean()) {
            return Err(invalid("MCP 信任和后台任务标记必须为布尔值。"));
        }
        if let Some(value) = server.get(field) {
            result[field] = value.clone();
        }
    }
    let priority = server
        .get("prewarmPriority")
        .map_or(Some(0), Value::as_u64)
        .filter(|priority| *priority <= 1000)
        .ok_or_else(|| invalid("MCP 预热优先级须为 0 到 1000 的整数。"))?;
    if server.get("prewarmPriority").is_some() {
        result["prewarmPriority"] = json!(priority);
    }
    Ok(result)
}

pub fn endpoint_identity(server: &Value) -> ServiceResult<String> {
    let normalized = normalize_server(&json!({"id":"identity","name":"identity","enabled":true,
        "transport":server["transport"],"command":server["command"],"args":server["args"],"cwd":server["cwd"],
        "env":server["env"],"envRefs":server["envRefs"],"url":server["url"],"headerEnv":server["headerEnv"],
        "auth":server["auth"],"startupTimeoutMs":server["startupTimeoutMs"]}))?;
    let ordered = |value: &Value| {
        value
            .as_object()
            .map(|values| {
                values
                    .iter()
                    .map(|(key, value)| json!([key, value]))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let identity = if normalized["transport"] == "stdio" {
        json!([
            normalized["transport"],
            normalized["command"],
            normalized["args"],
            normalized["cwd"],
            ordered(&normalized["env"]),
            ordered(&normalized["envRefs"])
        ])
    } else {
        json!([
            normalized["transport"],
            normalized["url"],
            ordered(&normalized["headerEnv"]),
            normalized["auth"]
        ])
    };
    Ok(sha256(serde_json::to_string(&identity).unwrap().as_bytes()))
}

fn validate_config(
    value: &Value,
    reject_duplicates: bool,
    maximum_servers: usize,
) -> ServiceResult<Value> {
    object(value)?;
    if value["version"] != 1 {
        return Err(ServiceError::new(
            "UNSUPPORTED_AGENT_CONFIG",
            "工具配置版本不受支持，原文件已保留。",
            409,
        ));
    }
    let revision = value["revision"]
        .as_u64()
        .filter(|revision| *revision <= 9_007_199_254_740_991)
        .ok_or_else(|| invalid("工具配置格式无效。"))?;
    let servers = value["mcpServers"]
        .as_array()
        .ok_or_else(|| invalid("工具配置格式无效。"))?;
    let directories = value["skillDirectories"]
        .as_array()
        .ok_or_else(|| invalid("工具配置格式无效。"))?;
    if servers.len() > maximum_servers || directories.len() > 32 {
        return Err(invalid("工具配置格式无效。"));
    }
    let mut server_ids = HashSet::new();
    let mut endpoints = HashSet::new();
    let mut normalized_servers = Vec::new();
    for server in servers {
        let server = normalize_server(server)?;
        if !server_ids.insert(server["id"].clone()) {
            return Err(invalid("MCP ID 无效或重复。"));
        }
        let identity = endpoint_identity(&server)?;
        if !endpoints.insert(identity) && reject_duplicates {
            return Err(ServiceError::new(
                "DUPLICATE_MCP_ENDPOINT",
                "同一个 MCP 连接已配置，请编辑原连接。",
                409,
            ));
        }
        normalized_servers.push(server);
    }
    let mut normalized_directories = Vec::new();
    for directory in directories {
        let directory = text(directory, "技能目录", 4096)?;
        if !std::path::Path::new(&directory).is_absolute() {
            return Err(invalid("技能目录须为绝对路径。"));
        }
        let directory = json!(path_text(&absolute_path(&directory)));
        if !normalized_directories.contains(&directory) {
            normalized_directories.push(directory);
        }
    }
    let disabled = value
        .get("disabledSkills")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let disabled = disabled
        .as_array()
        .ok_or_else(|| invalid("禁用技能列表无效。"))?;
    if disabled.len() > 4096
        || disabled
            .iter()
            .any(|value| !matches(r"^[a-f0-9]{24}$", string(value)))
    {
        return Err(invalid("禁用技能列表无效。"));
    }
    let mut result = json!({"version":1,"revision":revision,"mcpServers":normalized_servers,"skillDirectories":normalized_directories,"disabledSkills":unique(disabled)});
    if !value["disabledOfficialMcpServers"].is_null() {
        result["disabledOfficialMcpServers"] = json!(ids(&value["disabledOfficialMcpServers"])?);
    }
    if !value["officialMcpOverrides"].is_null() {
        let overrides = value["officialMcpOverrides"]
            .as_array()
            .filter(|values| values.len() <= 64)
            .ok_or_else(|| invalid("官方工具自定义配置无效。"))?;
        let mut seen = HashSet::new();
        let mut normalized = Vec::new();
        for item in overrides {
            object(item)?;
            let preset = ids(&json!([item["presetId"]]))?.remove(0);
            if !seen.insert(preset.clone()) {
                return Err(invalid("官方工具自定义配置重复。"));
            }
            if !item["server"].is_null() {
                normalized
                    .push(json!({"presetId":preset,"server":normalize_server(&item["server"])?}));
                continue;
            }
            let id = text(&item["id"], "MCP ID", 40)?;
            if !matches(r"^[a-z][a-z0-9-]{1,39}$", &id) {
                return Err(invalid("MCP ID 无效。"));
            }
            object(&item["changes"])?;
            if item["changes"]
                .as_object()
                .unwrap()
                .keys()
                .any(|key| !OVERRIDE_FIELDS.contains(&key.as_str()))
            {
                return Err(invalid("官方工具自定义字段无效。"));
            }
            normalized.push(json!({"presetId":preset,"id":id,"changes":item["changes"]}));
        }
        result["officialMcpOverrides"] = json!(normalized);
    }
    Ok(result)
}

pub fn official_server_id(preset_id: &str, occupied: &HashSet<String>) -> String {
    let base = format!("official-{preset_id}");
    let mut id = base.clone();
    let mut suffix = 2;
    while occupied.contains(&id) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

pub fn configured_preset(preset: &Value, servers: &[Value]) -> Option<usize> {
    if let Some(index) = servers
        .iter()
        .position(|server| server["origin"] == "official" && server["presetId"] == preset["id"])
    {
        return Some(index);
    }
    let identity = endpoint_identity(&preset["server"]).ok();
    if let Some(index) = servers
        .iter()
        .position(|server| endpoint_identity(server).ok() == identity && identity.is_some())
    {
        return Some(index);
    }
    servers.iter().position(|server| {
        if preset["server"]["transport"] == "streamable-http" {
            return server["transport"] == "streamable-http"
                && string(&server["url"]).trim_end_matches('/')
                    == string(&preset["server"]["url"]).trim_end_matches('/');
        }
        if !server["transport"].is_null() && server["transport"] != "stdio"
            || !preset["package"].is_object()
        {
            return false;
        }
        let executable = string(&server["command"])
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_owned();
        let executable = regex::Regex::new(r"(?i)\.(?:exe|cmd|ps1)$")
            .unwrap()
            .replace(&executable, "");
        if executable == string(&preset["package"]["entryPoint"]) {
            return true;
        }
        let package = string(&preset["package"]["name"]);
        array(&server["args"]).iter().any(|argument| {
            let argument = string(argument);
            argument == package
                || argument.starts_with(&format!(
                    "{package}{}",
                    if preset["package"]["registry"] == "npm" {
                        "@"
                    } else {
                        "=="
                    }
                ))
                || argument
                    .replace('\\', "/")
                    .contains(&format!("/node_modules/{package}/"))
                || preset["package"]["registry"] == "pypi" && argument == package.replace('-', "_")
        })
    })
}

fn merge(user: &Value, presets: &[Value], metadata: &Value) -> ServiceResult<Value> {
    let mut servers = array(&user["mcpServers"])
        .iter()
        .map(|server| {
            let mut server = server.clone();
            server["origin"] = json!("user");
            server
        })
        .collect::<Vec<_>>();
    let mut occupied = servers
        .iter()
        .map(|server| string(&server["id"]).to_owned())
        .collect::<HashSet<_>>();
    let hidden = array(&user["disabledOfficialMcpServers"]);
    for preset in presets {
        if hidden.contains(&preset["id"]) {
            continue;
        }
        let override_value = array(&user["officialMcpOverrides"])
            .iter()
            .find(|item| item["presetId"] == preset["id"]);
        if override_value.is_none() {
            if let Some(index) = configured_preset(preset, &servers) {
                if servers[index]["presetId"].is_null() {
                    servers[index]["presetId"] = preset["id"].clone();
                }
                continue;
            }
        }
        let id = override_value
            .and_then(|item| {
                item["id"]
                    .as_str()
                    .or_else(|| item["server"]["id"].as_str())
            })
            .map(str::to_owned)
            .unwrap_or_else(|| official_server_id(string(&preset["id"]), &occupied));
        if occupied.contains(&id) {
            continue;
        }
        let mut source = override_value
            .filter(|item| item["server"].is_object())
            .map(|item| item["server"].clone())
            .unwrap_or_else(|| preset["server"].clone());
        if let Some(changes) = override_value.and_then(|item| item["changes"].as_object()) {
            for (key, value) in changes {
                source[key] = value.clone();
            }
        }
        source["id"] = json!(id);
        let mut server = normalize_server(&source)?;
        let identity = endpoint_identity(&server)?;
        if servers
            .iter()
            .any(|server| endpoint_identity(server).ok().as_ref() == Some(&identity))
        {
            continue;
        }
        occupied.insert(id);
        server["origin"] = json!("official");
        server["presetId"] = preset["id"].clone();
        server["overridden"] = json!(override_value.is_some());
        servers.push(server);
    }
    let mut result = json!({"version":user["version"],"revision":user["revision"],"mcpServers":servers,
        "skillDirectories":user["skillDirectories"],"disabledSkills":user["disabledSkills"],"disabledOfficialMcpServers":hidden});
    if let Some(metadata) = metadata.as_object() {
        for (key, value) in metadata {
            result[key] = value.clone();
        }
    }
    Ok(result)
}

fn split(
    effective: &Value,
    previous: &Value,
    current: &Value,
    presets: &[Value],
) -> ServiceResult<Value> {
    let by_id = array(&current["mcpServers"])
        .iter()
        .filter(|server| server["origin"] == "official")
        .map(|server| {
            (
                string(&server["id"]).to_owned(),
                string(&server["presetId"]).to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    let previous_ids = array(&previous["mcpServers"])
        .iter()
        .map(|server| string(&server["id"]).to_owned())
        .collect::<HashSet<_>>();
    let mut hidden = array(
        effective
            .get("disabledOfficialMcpServers")
            .unwrap_or(&previous["disabledOfficialMcpServers"]),
    )
    .to_vec();
    let selected_ids = array(&effective["mcpServers"])
        .iter()
        .map(|server| string(&server["id"]))
        .collect::<HashSet<_>>();
    for (id, preset) in &by_id {
        if !selected_ids.contains(id.as_str()) && !hidden.contains(&json!(preset)) {
            hidden.push(json!(preset));
        }
    }
    let mut overrides = array(&previous["officialMcpOverrides"])
        .iter()
        .filter(|item| !by_id.values().any(|preset| item["presetId"] == *preset))
        .cloned()
        .collect::<Vec<_>>();
    let mut servers = Vec::new();
    for server in array(&effective["mcpServers"]) {
        let id = string(&server["id"]);
        let preset_id = by_id.get(id).cloned().or_else(|| {
            if previous_ids.contains(id) {
                return None;
            }
            presets
                .iter()
                .find(|preset| official_server_id(string(&preset["id"]), &previous_ids) == id)
                .map(|preset| string(&preset["id"]).to_owned())
        });
        let Some(preset_id) = preset_id else {
            servers.push(server.clone());
            continue;
        };
        let preset = presets
            .iter()
            .find(|preset| preset["id"] == preset_id)
            .ok_or_else(|| invalid("官方工具自定义配置无效。"))?;
        let mut baseline = preset["server"].clone();
        baseline["id"] = json!(id);
        let baseline = normalize_server(&baseline)?;
        hidden.retain(|value| *value != preset_id);
        overrides.retain(|item| item["presetId"] != preset_id);
        if *server != baseline {
            let keys = baseline
                .as_object()
                .unwrap()
                .keys()
                .chain(server.as_object().unwrap().keys())
                .collect::<HashSet<_>>();
            let changes = keys
                .into_iter()
                .filter(|key| key.as_str() != "id" && server[*key] != baseline[*key])
                .map(|key| (key.clone(), server[key].clone()))
                .collect::<serde_json::Map<_, _>>();
            overrides.push(json!({"presetId":preset_id,"id":id,"changes":changes}));
        }
    }
    Ok(
        json!({"version":effective["version"],"revision":effective["revision"],"mcpServers":servers,
        "skillDirectories":effective["skillDirectories"],"disabledSkills":effective["disabledSkills"],
        "officialMcpOverrides":overrides,"disabledOfficialMcpServers":hidden}),
    )
}

#[derive(Default)]
pub struct ConfigRepository {
    root: PathBuf,
    presets: Option<Vec<Value>>,
    metadata: Value,
}

impl ConfigRepository {
    fn read_user(&self) -> ServiceResult<Value> {
        let file = self.root.join("Agent/config.json");
        let Some(metadata) = inspect(&file, true, false)? else {
            return Ok(
                json!({"version":1,"revision":0,"mcpServers":[],"skillDirectories":[],"disabledSkills":[]}),
            );
        };
        let corrupt = || {
            ServiceError::new(
                "CORRUPT_AGENT_CONFIG",
                "工具配置文件损坏，原文件已保留。",
                500,
            )
        };
        if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
            return Err(corrupt());
        }
        let bytes = read_checked(&file, MAX_CONFIG_BYTES, false, "AGENT_CONFIG_CONFLICT")?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| corrupt())?
            .trim_start_matches('\u{feff}');
        let value = serde_json::from_str::<Value>(text).map_err(|_| corrupt())?;
        validate_config(&value, false, 32).map_err(|error| {
            if error.code == "UNSUPPORTED_AGENT_CONFIG" {
                error
            } else {
                corrupt()
            }
        })
    }

    fn effective(&self, user: &Value) -> ServiceResult<Value> {
        match &self.presets {
            Some(presets) => merge(user, presets, &self.metadata),
            None => Ok(user.clone()),
        }
    }

    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "config.init" | "config.initRead" => {
                self.root = absolute_path(string(&arguments["root"]));
                self.presets = arguments["presets"].as_array().cloned();
                self.metadata = arguments["metadata"].clone();
                if operation == "config.initRead" {
                    self.effective(&self.read_user()?)
                } else {
                    Ok(Value::Null)
                }
            }
            "config.read" => self.effective(&self.read_user()?),
            "config.validate" => {
                let mut input = arguments["input"].clone();
                input["revision"] = json!(0);
                validate_config(
                    &input,
                    true,
                    32 + self.presets.as_ref().map(Vec::len).unwrap_or(0),
                )
            }
            "config.update" => {
                let input = &arguments["input"];
                object(input)?;
                let expected = input["expectedRevision"]
                    .as_u64()
                    .filter(|revision| *revision <= 9_007_199_254_740_991)
                    .ok_or_else(|| invalid("请提供工具配置 expectedRevision。"))?;
                let key = path_key(&self.root.join("Agent/config.json"));
                let lock = {
                    let mut locks = CONFIG_LOCKS
                        .get_or_init(|| Mutex::new(HashMap::new()))
                        .lock()
                        .unwrap();
                    locks.retain(|_, lock| lock.strong_count() > 0);
                    let lock = locks
                        .get(&key)
                        .and_then(Weak::upgrade)
                        .unwrap_or_else(|| Arc::new(Mutex::new(())));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                };
                let _guard = lock.lock().unwrap();
                let previous = self.read_user()?;
                if previous["revision"] != expected {
                    return Err(ServiceError::new(
                        "AGENT_CONFIG_CONFLICT",
                        "工具配置已更新，请重新读取。",
                        409,
                    ));
                }
                let current = self.effective(&previous)?;
                let mut updated = input.clone();
                updated["revision"] = json!(expected + 1);
                let effective = validate_config(
                    &updated,
                    true,
                    32 + self.presets.as_ref().map(Vec::len).unwrap_or(0),
                )?;
                let next = if let Some(presets) = &self.presets {
                    validate_config(&split(&effective, &previous, &current, presets)?, true, 32)?
                } else {
                    effective
                };
                if serde_json::to_string_pretty(&next).unwrap().len() as u64 > MAX_CONFIG_BYTES {
                    return Err(invalid("工具配置过大。"));
                }
                for directory in array(&next["skillDirectories"]) {
                    if inspect(&absolute_path(string(directory)), true, false)?
                        .is_some_and(|metadata| !metadata.is_dir())
                    {
                        return Err(invalid("技能路径不是目录。"));
                    }
                }
                let projected = self.effective(&next)?;
                atomic_json(&self.root.join("Agent/config.json"), &next)?;
                Ok(projected)
            }
            "config.preset" => Ok(configured_preset(
                &arguments["preset"],
                array(&arguments["servers"]),
            )
            .map(|index| json!(index))
            .unwrap_or(Value::Null)),
            "config.officialId" => Ok(json!(official_server_id(
                string(&arguments["presetId"]),
                &array(&arguments["occupied"])
                    .iter()
                    .map(|value| string(value).to_owned())
                    .collect()
            ))),
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown configuration operation.",
                400,
            )),
        }
    }
}
