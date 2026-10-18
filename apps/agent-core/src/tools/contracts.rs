use serde_json::{Value, json};

use super::intent::utf16_length;

// Provenance is descriptive, never a grant. All returned fields are bounded fixed enums.
// 执行来源只是说明，不能授予权限；返回字段只包含受限的固定枚举，不包含端点和凭据。
fn normalize_environment(input: &Value) -> Option<Value> {
    let object = input.as_object()?;
    let network = input["network"].as_object()?;
    let includes = |name: &str, values: &[&str]| {
        input[name]
            .as_str()
            .is_some_and(|value| values.contains(&value))
    };
    if input["schemaVersion"] != 1
        || !includes(
            "executorKind",
            &[
                "unknown",
                "builtin-tool",
                "builtin-http",
                "host-terminal",
                "desktop-tool-host",
                "sandbox-terminal",
                "mcp-stdio",
                "mcp-http",
            ],
        )
        || !includes(
            "executorLocation",
            &["unknown", "gateway-host", "remote-service"],
        )
        || !includes("operationLocation", &["unknown", "gateway-host"])
        || !includes(
            "locationScope",
            &[
                "unknown",
                "builtin-execution-policy",
                "configured-transport-process",
                "configured-service-endpoint",
            ],
        )
        || input["gatewayHostMeaning"] != "machine-running-the-gateway"
        || input["userDeviceRelationship"] != "unverified"
        || input["grantsPermission"] != false
        || !network
            .get("requestOrigin")
            .and_then(Value::as_str)
            .is_some_and(|value| {
                [
                    "unknown",
                    "gateway-host",
                    "tool-defined-unknown",
                    "unavailable",
                ]
                .contains(&value)
            })
        || input["network"]["egress"] != "unknown"
        || input["network"]["proxy"] != "unknown"
        || input["network"]["sameEgressDoesNotProveSameMachine"] != true
    {
        return None;
    }
    let mut value = environment(
        input["executorKind"].as_str()?,
        input["executorLocation"].as_str()?,
        input["operationLocation"].as_str()?,
        input["network"]["requestOrigin"].as_str()?,
        input["locationScope"].as_str()?,
    );
    if object.contains_key("isolation") {
        if input["isolation"] != "windows-appcontainer" {
            return None;
        }
        value["isolation"] = input["isolation"].clone();
    }
    if object.contains_key("sandboxVerified") {
        if !input["sandboxVerified"].is_boolean() || value["isolation"] != "windows-appcontainer" {
            return None;
        }
        value["sandboxVerified"] = input["sandboxVerified"].clone();
    }
    if object.contains_key("serviceMachineIdentity") {
        if input["serviceMachineIdentity"] != "unknown" {
            return None;
        }
        value["serviceMachineIdentity"] = json!("unknown");
    }
    if object.contains_key("browserTarget") {
        let browser = input["browserTarget"].as_object()?;
        if !browser
            .get("location")
            .and_then(Value::as_str)
            .is_some_and(|value| ["unknown", "gateway-host", "remote-browser"].contains(&value))
            || !browser
                .get("evidence")
                .and_then(Value::as_str)
                .is_some_and(|value| {
                    [
                        "application-verified-browser-target",
                        "trusted-browser-configuration",
                    ]
                    .contains(&value)
                })
            || browser.contains_key("mode")
                && !browser["mode"].as_str().is_some_and(|value| {
                    [
                        "existing-browser",
                        "independent-browser",
                        "remote-browser",
                        "custom-browser",
                    ]
                    .contains(&value)
                })
            || browser.contains_key("visibility")
                && !browser["visibility"]
                    .as_str()
                    .is_some_and(|value| ["headless", "visible"].contains(&value))
        {
            return None;
        }
        let mut target = json!({"location":browser["location"],"evidence":browser["evidence"]});
        for field in ["mode", "visibility"] {
            if let Some(member) = browser.get(field) {
                target[field] = member.clone();
            }
        }
        value["browserTarget"] = target;
    }
    if serde_json::to_vec(&value).ok()?.len() >= 1024 {
        None
    } else {
        Some(value)
    }
}

fn environment(kind: &str, executor: &str, operation: &str, origin: &str, scope: &str) -> Value {
    json!({"schemaVersion":1,"executorKind":kind,"executorLocation":executor,"operationLocation":operation,"locationScope":scope,
        "gatewayHostMeaning":"machine-running-the-gateway","userDeviceRelationship":"unverified","grantsPermission":false,
        "network":{"requestOrigin":origin,"egress":"unknown","proxy":"unknown","sameEgressDoesNotProveSameMachine":true}})
}

fn resolve_environment(arguments: &Value) -> Value {
    let descriptor = &arguments["descriptor"];
    let name = descriptor["name"].as_str().unwrap_or("");
    if descriptor["source"] == "builtin" {
        let (kind, origin) = if name == "web.fetch" {
            ("builtin-http", "gateway-host")
        } else if name.starts_with("terminal.host.") || name.starts_with("terminal.workspace.") {
            ("host-terminal", "tool-defined-unknown")
        } else if name.starts_with("computer.") {
            ("desktop-tool-host", "unknown")
        } else if ["terminal.run", "skill.run"].contains(&name) {
            ("sandbox-terminal", "unknown")
        } else {
            ("builtin-tool", "unknown")
        };
        let mut value = environment(
            kind,
            "gateway-host",
            "gateway-host",
            origin,
            "builtin-execution-policy",
        );
        if kind == "sandbox-terminal" {
            let sandbox = &arguments["sandbox"];
            let verified = sandbox["available"] == true
                && sandbox["sandbox"] == "appcontainer"
                && sandbox["failClosed"] == true
                && sandbox["checksChildToken"] == true;
            value["isolation"] = json!("windows-appcontainer");
            value["sandboxVerified"] = json!(verified);
            if verified && sandbox["network"] == false {
                value["network"]["requestOrigin"] = json!("unavailable");
            }
        }
        return value;
    }
    let server = &arguments["server"];
    let mut value = match server["transport"].as_str().unwrap_or("stdio") {
        "stdio" if server.is_object() => environment(
            "mcp-stdio",
            "gateway-host",
            "unknown",
            "tool-defined-unknown",
            "configured-transport-process",
        ),
        "streamable-http" | "sse" if server.is_object() => {
            let mut value = environment(
                "mcp-http",
                "remote-service",
                "unknown",
                "tool-defined-unknown",
                "configured-service-endpoint",
            );
            value["serviceMachineIdentity"] = json!("unknown");
            value
        }
        _ => return environment("unknown", "unknown", "unknown", "unknown", "unknown"),
    };
    let verified = &arguments["verifiedTarget"];
    let connection = &arguments["connection"];
    if verified["location"]
        .as_str()
        .is_some_and(|location| ["gateway-host", "remote-browser", "unknown"].contains(&location))
    {
        value["browserTarget"] = json!({"location":verified["location"],"evidence":"application-verified-browser-target"});
    } else if connection.is_object() {
        let mode = connection["mode"].as_str().unwrap_or("");
        let local = ["existing-browser", "independent-browser"].contains(&mode);
        let mut target = json!({"location":if local {"gateway-host"} else {"unknown"},"mode":mode,"evidence":"trusted-browser-configuration"});
        if mode == "independent-browser" {
            target["visibility"] = json!(if connection["headless"] == true {
                "headless"
            } else {
                "visible"
            });
        }
        value["browserTarget"] = target;
    }
    normalize_environment(&value).unwrap_or(Value::Null)
}

fn type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn validate_input(schema: &Value, node: &Value, path: &str, depth: usize) -> Option<Value> {
    let error = |kind: &str, path: &str| Some(json!({"kind":kind,"path":path}));
    if depth > 16 {
        return error("depth", path);
    }
    let kind = node["type"].as_str().unwrap_or("undefined");
    let scalar = &node["value"];
    let types = schema["type"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![schema["type"].clone()]);
    if !schema["type"].is_null()
        && !types.iter().any(|value| {
            value.as_str() == Some(kind)
                || value == "integer"
                    && kind == "number"
                    && scalar.as_f64().is_some_and(|number| {
                        number.is_finite()
                            && number.fract() == 0.0
                            && number.abs() <= 9_007_199_254_740_991.0
                    })
        })
    {
        return error("type", path);
    }
    if let Some(values) = schema["enum"].as_array() {
        let included = values.iter().any(|value| {
            type_of(value) == kind
                && match kind {
                    "number" => value.as_f64() == scalar.as_f64() && scalar.is_number(),
                    "object" | "array" => false,
                    _ => value == scalar,
                }
        });
        if !included {
            return error("enum", path);
        }
    }
    if kind == "string" {
        let text = scalar.as_str().unwrap_or("");
        let length = utf16_length(text) as f64;
        if text.contains('\0')
            || length < schema["minLength"].as_f64().unwrap_or(0.0)
            || length > schema["maxLength"].as_f64().unwrap_or(f64::INFINITY)
        {
            return error("length", path);
        }
    }
    if kind == "number"
        && (node["finite"] != true
            || scalar.as_f64().is_some_and(|number| {
                number < schema["minimum"].as_f64().unwrap_or(f64::NEG_INFINITY)
                    || number > schema["maximum"].as_f64().unwrap_or(f64::INFINITY)
            }))
    {
        return error("range", path);
    }
    if kind == "array" {
        let children = node["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let length = children.len() as f64;
        if length < schema["minItems"].as_f64().unwrap_or(0.0)
            || length > schema["maxItems"].as_f64().unwrap_or(64.0)
        {
            return error("items", path);
        }
        if !schema["items"].is_null() {
            for (index, child) in children.iter().enumerate() {
                // Empty array slots cross JSON as null; explicit null retains a typed projection.
                // 稀疏数组空槽通过 JSON 变为 null；显式 null 保留类型投影，仍执行完整校验。
                if child.is_null() {
                    continue;
                }
                if let Some(error) = validate_input(
                    &schema["items"],
                    child,
                    &format!("{path}[{index}]"),
                    depth + 1,
                ) {
                    return Some(error);
                }
            }
        }
    } else if kind == "object" {
        let entries = node["entries"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        for key in schema["required"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(Value::as_str)
        {
            if !entries.iter().any(|entry| entry[0] == key) {
                return error("required", &format!("{path}.{key}"));
            }
        }
        for entry in entries {
            let key = entry[0].as_str().unwrap_or("");
            let property = schema["properties"].get(key);
            if property.is_none() && schema["additionalProperties"] == false {
                return error("property", &format!("{path}.{key}"));
            }
            if let Some(property) = property {
                if let Some(error) =
                    validate_input(property, &entry[1], &format!("{path}.{key}"), depth + 1)
                {
                    return Some(error);
                }
            }
        }
    }
    None
}

pub(super) fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    Ok(match operation {
        "tools.normalizeEnvironment" => {
            normalize_environment(&arguments["input"]).unwrap_or(Value::Null)
        }
        "tools.resolveEnvironment" => resolve_environment(arguments),
        "tools.environmentServer" => {
            let descriptor = &arguments["descriptor"];
            let servers = arguments["servers"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let server_id = descriptor["serverId"].as_str().unwrap_or("");
            let owned = descriptor["serverId"].is_string()
                && descriptor["source"] == format!("mcp:{server_id}")
                && descriptor["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(&format!("mcp.{server_id}.")));
            if owned && servers.len() <= 64 {
                servers
                    .iter()
                    .position(|server| server["id"] == server_id && server["enabled"] != false)
                    .map(|index| json!(index))
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
        "tools.validateBuiltinInput" => validate_input(
            &arguments["schema"],
            &arguments["input"],
            arguments["name"].as_str().unwrap_or(""),
            0,
        )
        .unwrap_or(Value::Null),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    })
}
