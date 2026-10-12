use std::collections::HashSet;

use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use super::intent::{js_trim, matches, utf16_length};

fn invalid() -> String {
    "INVALID_AGENT_CONFIG".to_owned()
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}
fn key_count(value: &Value) -> usize {
    match value {
        Value::Object(value) => value.len(),
        Value::Array(value) => value.len(),
        Value::String(value) => utf16_length(value),
        _ => 0,
    }
}
fn optional<'a>(value: &'a Value, key: &str, fallback: &'a Value) -> &'a Value {
    value
        .get(key)
        .filter(|value| !value.is_null())
        .unwrap_or(fallback)
}
fn string_limit(value: &Value, maximum: usize, no_newlines: bool) -> bool {
    value.as_str().is_some_and(|text| {
        utf16_length(text) <= maximum
            && !text.contains('\0')
            && (!no_newlines || !text.contains(['\r', '\n']))
    })
}

fn environment_map(value: &Value, kind: &str) -> Result<Value, String> {
    if value.is_null() {
        return Ok(json!({}));
    }
    let entries = value.as_object().ok_or("INVALID_TOOL_ARGUMENTS")?;
    if entries.len() > 64 {
        return Err(invalid());
    }
    let mut seen = HashSet::new();
    for (key, value) in entries {
        let pattern = if kind == "header" {
            r"^[!#$%&'*+.^_`|~a-zA-Z0-9-]{1,100}$"
        } else {
            ENV_NAME
        };
        let lower = key.to_lowercase();
        if !matches(pattern, key)
            || kind == "header"
                && (OWNED_HEADERS.contains(&lower.as_str()) || seen.contains(&lower))
            || !string_limit(value, 16384, true)
            || kind != "constant" && !matches(ENV_NAME, value.as_str().unwrap_or(""))
            || kind == "constant" && matches("KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL", key)
        {
            return Err(invalid());
        }
        seen.insert(lower);
    }
    Ok(value.clone())
}

fn mcp_url(arguments: &Value) -> Result<Value, String> {
    let raw = &arguments["value"];
    let url = &arguments["url"];
    let issuer = arguments["issuer"] == true;
    let hostname = url["hostname"].as_str().unwrap_or("");
    let loopback = ["localhost", "[::1]"].contains(&hostname)
        || matches(r"^127(?:\.[0-9]{1,3}){3}$", hostname);
    if !string_limit(raw, 4096, true)
        || !url.is_object()
        || truthy(&url["username"])
        || truthy(&url["password"])
        || truthy(&url["search"])
        || truthy(&url["hash"])
        || url["protocol"] != "https:" && !(url["protocol"] == "http:" && loopback && !issuer)
    {
        return Err(invalid());
    }
    Ok(if issuer {
        raw.clone()
    } else {
        url["href"].clone()
    })
}

pub(super) fn normalize_connection(arguments: &Value) -> Result<Value, String> {
    let server = &arguments["server"];
    // JSON cannot represent NaN or infinities; preserve their rejection instead of using the null default.
    // JSON 无法表示 NaN 和无穷值；保留拒绝状态，不能将其作为 null 使用默认超时。
    let special = |key: &str| arguments["nonFiniteNumbers"][key].is_string();
    let field_truthy = |key: &str| {
        if special(key) {
            arguments["nonFiniteNumbers"][key] != "nan"
        } else {
            truthy(&server[key])
        }
    };
    if special("transport") || special("startupTimeoutMs") {
        return Err(invalid());
    }
    let default_transport = json!("stdio");
    let transport = optional(server, "transport", &default_transport)
        .as_str()
        .ok_or_else(invalid)?;
    if !["stdio", "streamable-http","sse"].contains(&transport) {
        return Err(invalid());
    }
    let timeout =
        server["startupTimeoutMs"]
            .as_f64()
            .unwrap_or(if server["startupTimeoutMs"].is_null() {
                15000.0
            } else {
                f64::NAN
            });
    if !timeout.is_finite() || timeout.fract() != 0.0 || !(1000.0..=120000.0).contains(&timeout) {
        return Err(invalid());
    }
    if transport == "stdio" {
        let command = server["command"]
            .as_str()
            .filter(|value| !js_trim(value).is_empty())
            .ok_or_else(invalid)?;
        let args = server["args"].as_array().ok_or_else(invalid)?;
        if !string_limit(&server["command"], 4096, true)
            || args.len() > 64
            || args.iter().any(|value| !string_limit(value, 16384, false))
        {
            return Err(invalid());
        }
        let env = if special("env") {
            return Err("INVALID_TOOL_ARGUMENTS".to_owned());
        } else {
            environment_map(&server["env"], "constant")?
        };
        let references = if special("envRefs") {
            return Err("INVALID_TOOL_ARGUMENTS".to_owned());
        } else {
            environment_map(&server["envRefs"], "reference")?
        };
        if env
            .as_object()
            .unwrap()
            .keys()
            .any(|key| references.get(key).is_some())
            || field_truthy("url")
            || field_truthy("auth")
            || key_count(&server["headerEnv"]) > 0
        {
            return Err(invalid());
        }
        let mut value = json!({"transport":transport,"command":js_trim(command),"args":args,"env":env,"envRefs":references,"startupTimeoutMs":timeout as u64});
        if special("cwd") || !server["cwd"].is_null() && server["cwd"] != "" {
            if !string_limit(&server["cwd"], 4096, true)
                || arguments["cwdAbsolute"] != true
                || !arguments["cwdResolved"].is_string()
            {
                return Err(invalid());
            }
            value["cwd"] = arguments["cwdResolved"].clone();
        }
        return Ok(value);
    }
    if field_truthy("cwd") || key_count(&server["env"]) > 0 || key_count(&server["envRefs"]) > 0 {
        return Err(invalid());
    }
    let url = mcp_url(&json!({"value":server["url"],"url":arguments["url"]}))?;
    let headers = if special("headerEnv") {
        return Err("INVALID_TOOL_ARGUMENTS".to_owned());
    } else {
        environment_map(&server["headerEnv"], "header")?
    };
    let mut value = json!({"transport":transport,"command":"","args":[],"url":url,"headerEnv":headers,"startupTimeoutMs":timeout as u64});
    let auth = &server["auth"];
    if special("auth") || !auth.is_null() {
        if !auth.is_object() {
            return Err("INVALID_TOOL_ARGUMENTS".to_owned());
        }
        let normalized = if auth["type"] == "bearer-env"
            && matches(ENV_NAME, auth["tokenEnv"].as_str().unwrap_or(""))
        {
            json!({"type":"bearer-env","tokenEnv":auth["tokenEnv"]})
        } else if auth["type"] == "oauth-client-credentials"
            && matches(ENV_NAME, auth["clientSecretEnv"].as_str().unwrap_or(""))
            && string_limit(&auth["clientId"], 256, true)
            && auth["clientId"]
                .as_str()
                .is_some_and(|value| !js_trim(value).is_empty())
            && !special("auth.scope")
            && (auth["scope"].is_null() || string_limit(&auth["scope"], 2000, true))
        {
            let issuer = mcp_url(
                &json!({"value":auth["issuer"],"url":arguments["issuerUrl"],"issuer":true}),
            )?;
            let mut normalized = json!({"type":"oauth-client-credentials","clientId":js_trim(auth["clientId"].as_str().unwrap()),"clientSecretEnv":auth["clientSecretEnv"],"issuer":issuer});
            if truthy(&auth["scope"]) {
                normalized["scope"] = auth["scope"].clone();
            }
            normalized
        } else {
            return Err(invalid());
        };
        if headers
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.eq_ignore_ascii_case("authorization"))
        {
            return Err(invalid());
        }
        value["auth"] = normalized;
    }
    Ok(value)
}

fn connection_configuration(server: &Value) -> Value {
    if !truthy(server) {
        return Value::Null;
    }
    let mut connection = server.clone();
    if let Some(object) = connection.as_object_mut() {
        object.remove("name");
        object.remove("disabledTools");
    }
    connection
}

fn server_by_id<'a>(config: &'a Value, descriptor: &Value) -> &'a Value {
    config["mcpServers"]
        .as_array()
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server["id"] == descriptor["serverId"])
        })
        .unwrap_or(&Value::Null)
}

fn configured_enabled(config: &Value, descriptor: &Value) -> bool {
    if !descriptor["source"]
        .as_str()
        .is_some_and(|source| source.starts_with("mcp:"))
    {
        return descriptor["enabled"] != false;
    }
    let server = server_by_id(config, descriptor);
    server["enabled"] == true
        && !server["disabledTools"]
            .as_array()
            .is_some_and(|names| names.iter().any(|name| name == &descriptor["toolName"]))
}

fn configuration_changed(arguments: &Value) -> bool {
    let before = &arguments["previous"];
    let after = &arguments["current"];
    let descriptor = &arguments["descriptor"];
    if !truthy(before) || !truthy(after) || !truthy(descriptor) {
        return false;
    }
    if descriptor["source"]
        .as_str()
        .is_some_and(|source| source.starts_with("mcp:"))
    {
        let previous = server_by_id(before, descriptor);
        let current = server_by_id(after, descriptor);
        return !truthy(&current["enabled"])
            || current["disabledTools"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == &descriptor["toolName"]))
            || connection_configuration(previous) != connection_configuration(current);
    }
    descriptor["name"] == "skill.run"
        && (before["skillDirectories"] != after["skillDirectories"]
            || after["disabledSkills"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == &arguments["args"]["id"])))
}

fn changed_server_ids(arguments: &Value) -> Value {
    let before = arguments["previous"]["mcpServers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let after = arguments["current"]["mcpServers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut ids = Vec::<Value>::new();
    for server in before.iter().chain(after.iter()) {
        if !ids.contains(&server["id"]) {
            ids.push(server["id"].clone());
        }
    }
    ids.retain(|id| {
        let previous = before
            .iter()
            .rev()
            .find(|server| &server["id"] == id)
            .unwrap_or(&Value::Null);
        let current = after
            .iter()
            .rev()
            .find(|server| &server["id"] == id)
            .unwrap_or(&Value::Null);
        connection_configuration(previous) != connection_configuration(current)
    });
    json!(ids)
}

fn skill_header(values: &Value) -> Option<Value> {
    let name = values["name"].as_str()?;
    let description = values["description"].as_str()?;
    if js_trim(name).is_empty()
        || utf16_length(name) > 100
        || js_trim(description).is_empty()
        || utf16_length(description) > 2000
    {
        return None;
    }
    for field in ["license", "compatibility"] {
        if !values[field].is_null()
            && !values[field]
                .as_str()
                .is_some_and(|value| utf16_length(value) <= 2000)
        {
            return None;
        }
    }
    if !values["metadata"].is_null() && !values["metadata"].is_object() {
        return None;
    }
    let allowed = &values["allowed-tools"];
    if !allowed.is_null()
        && !allowed.is_string()
        && !allowed
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    {
        return None;
    }
    let mut extra = values.as_object()?.clone();
    extra.remove("name");
    extra.remove("description");
    let mut header = json!({"name":js_trim(name),"description":js_trim(description)});
    if !extra.is_empty() {
        header["metadata"] = json!(extra);
    }
    Some(header)
}

fn validate_skill(arguments: &Value) -> Value {
    let values = &arguments["values"];
    let header = skill_header(values);
    let mut diagnostics = Vec::<Value>::new();
    let mut error = |code: &str, field: &str, message: &str| {
        diagnostics.push(json!({"code":code,"field":field,"severity":"error","message":message}))
    };
    if !values.is_object() {
        error(
            "SKILL_FRONTMATTER_INVALID",
            "frontmatter",
            "Skill frontmatter must be a bounded, safe YAML mapping.",
        );
        return json!({"valid":false,"header":null,"diagnostics":diagnostics});
    }
    let name = values["name"]
        .as_str()
        .map(|value| js_trim(value).nfkc().collect::<String>())
        .unwrap_or_default();
    if name.is_empty() || name.chars().count() > 64 {
        error(
            "SKILL_NAME_LENGTH",
            "name",
            "Skill name must contain 1–64 characters.",
        );
    }
    if !name.is_empty()
        && (name != name.to_lowercase()
            || !matches(r"^[\p{L}\p{N}-]+$", &name)
            || name.starts_with('-')
            || name.ends_with('-')
            || name.contains("--"))
    {
        error(
            "SKILL_NAME_FORMAT",
            "name",
            "Skill name must use lowercase letters, numbers and single internal hyphens.",
        );
    }
    if !name.is_empty()
        && arguments["directoryName"]
            .as_str()
            .is_some_and(|directory| directory.nfkc().collect::<String>() != name)
    {
        error(
            "SKILL_DIRECTORY_NAME",
            "name",
            "The parent directory must match the skill name.",
        );
    }
    if !values["description"]
        .as_str()
        .is_some_and(|value| !js_trim(value).is_empty() && value.chars().count() <= 1024)
    {
        error(
            "SKILL_DESCRIPTION_LENGTH",
            "description",
            "Skill description must contain 1–1024 characters.",
        );
    }
    if values.get("compatibility").is_some()
        && !values["compatibility"]
            .as_str()
            .is_some_and(|value| !js_trim(value).is_empty() && value.chars().count() <= 500)
    {
        error(
            "SKILL_COMPATIBILITY_LENGTH",
            "compatibility",
            "Compatibility, when provided, must contain 1–500 characters.",
        );
    }
    if values.get("license").is_some() && !values["license"].is_string() {
        error(
            "SKILL_LICENSE_FORMAT",
            "license",
            "License must be a string.",
        );
    }
    if values.get("metadata").is_some()
        && !values["metadata"]
            .as_object()
            .is_some_and(|object| object.values().all(Value::is_string))
    {
        error(
            "SKILL_METADATA_FORMAT",
            "metadata",
            "Metadata must map string keys to string values.",
        );
    }
    if values.get("allowed-tools").is_some() && !values["allowed-tools"].is_string() {
        error(
            "SKILL_ALLOWED_TOOLS_FORMAT",
            "allowed-tools",
            "Allowed tools must be a space-separated string.",
        );
    }
    for field in arguments["fieldOrder"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(Value::as_str)
    {
        if ![
            "name",
            "description",
            "license",
            "compatibility",
            "metadata",
            "allowed-tools",
        ]
        .contains(&field)
        {
            error(
                "SKILL_UNKNOWN_FIELD",
                field,
                "Put additional frontmatter fields inside metadata.",
            );
        }
    }
    drop(error);
    if header.is_none() && diagnostics.is_empty() {
        diagnostics.push(json!({"code":"SKILL_FRONTMATTER_LIMIT","field":"frontmatter","severity":"error","message":"Skill metadata exceeds the application safety limits."}));
    }
    json!({"valid":diagnostics.is_empty(),"header":header,"diagnostics":diagnostics})
}

pub(super) fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    match operation {
        "tools.mcpUrl" => mcp_url(arguments),
        "tools.normalizeMcpConnection" => normalize_connection(arguments),
        "tools.connectionConfiguration" => Ok(connection_configuration(&arguments["server"])),
        "tools.configuredEnabled" => Ok(json!(configured_enabled(
            &arguments["config"],
            &arguments["descriptor"]
        ))),
        "tools.configurationChanged" => Ok(json!(configuration_changed(arguments))),
        "tools.changedMcpServerIds" => Ok(changed_server_ids(arguments)),
        "tools.skillHeader" => Ok(skill_header(&arguments["values"]).unwrap_or(Value::Null)),
        "tools.validateSkillHeader" => Ok(validate_skill(arguments)),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    }
}

const ENV_NAME: &str = r"^[a-zA-Z_][a-zA-Z0-9_]{0,127}$";
const OWNED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "accept",
    "origin",
    "mcp-session-id",
    "mcp-protocol-version",
];
