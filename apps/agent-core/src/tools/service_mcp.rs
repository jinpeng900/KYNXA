use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json};

use super::intent::matches;
use super::local_files::sha256;
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, slice, string, truthy};

const CACHE_ENTRIES: usize = 64;
const CACHE_BYTES: usize = 8 * 1024 * 1024;

fn invalid_catalog() -> ServiceError {
    ServiceError::new("MCP_INVALID_CATALOG", "MCP 工具定义无效。", 400)
}

fn cleanup_failure() -> ServiceError {
    ServiceError::new(
        "MCP_PROCESS_CLEANUP_FAILED",
        "MCP 自有进程未安全关闭，请重启模型服务；未重新连接。",
        502,
    )
}

/// SDK callbacks report object ownership and negotiated facts; native policy selects refusals and recovery steps.
/// SDK 回调报告对象所有权及协商事实；原生政策选择拒绝码及恢复步骤。
fn runtime_guard(arguments: &Value) -> ServiceResult<Value> {
    let phase = string(&arguments["phase"]);
    let resetting = arguments["resetting"] == true;
    let failed = arguments["failedClosures"].as_u64().unwrap_or(0) > 0;
    let updating = || {
        ServiceError::new(
            "MCP_CATALOG_CHANGED",
            "此 MCP 服务配置正在更新，请重新准备此服务调用。",
            409,
        )
    };
    match phase {
        "connect" => {
            if resetting {
                return Err(updating());
            }
            if failed {
                return Err(cleanup_failure());
            }
        }
        "validate" => {
            if failed {
                return Err(cleanup_failure());
            }
            if resetting {
                return Err(updating());
            }
        }
        "cleanup" => {
            if failed
                || array(&arguments["statuses"])
                    .iter()
                    .any(|status| status == "rejected")
            {
                return Err(cleanup_failure());
            }
        }
        "pending" if arguments["present"] != true => {
            return Err(ServiceError::new(
                "MCP_NOT_CONNECTED",
                "MCP 工具尚未连接或配置已变化，请刷新。",
                409,
            ));
        }
        "negotiated" if arguments["expectedVersion"] != arguments["negotiatedVersion"] => {
            return Err(ServiceError::new(
                "MCP_PROTOCOL_MISMATCH",
                "MCP 协商版本与明示配置不一致。",
                409,
            ));
        }
        "reconnect" if !truthy(&arguments["enabled"]) => {
            return Err(ServiceError::new(
                "MCP_NOT_ENABLED",
                "MCP 服务未配置或未启用。",
                409,
            ));
        }
        "approval" if arguments["connectionMatches"] != true => {
            return Err(ServiceError::new(
                "BROWSER_CONNECTION_CHANGED",
                "审批期间浏览器连接已替换，请重新准备调用和审批。",
                409,
            ));
        }
        "result" if arguments["inputRequired"] == true => {
            return Err(ServiceError::new(
                "MCP_INPUT_REQUIRED",
                "MCP 服务需要额外交互；此请求未自动授权或重试。",
                409,
            ));
        }
        "pending" | "negotiated" | "reconnect" | "approval" | "result" => {}
        _ => {
            return Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown MCP runtime phase.",
                400,
            ));
        }
    }
    Ok(Value::Null)
}

fn recovery_start(arguments: &Value) -> ServiceResult<Value> {
    let plan = &arguments["plan"];
    let recovery = &arguments["recovery"];
    if plan.is_null()
        || plan["tabId"] != recovery["tabId"]
        || plan["connectionSelectedTabId"] != recovery["connectionSelectedTabId"]
    {
        return Err(ServiceError::new(
            "BROWSER_TAB_CHANGED",
            "等待期间浏览器目标已变化，请重新读取当前任务页面。",
            409,
        ));
    }
    if arguments["hasTools"] != true {
        return Err(ServiceError::new(
            "MCP_CATALOG_CHANGED",
            "此连接缺少页面发现或快照工具，请刷新工具目录。",
            409,
        ));
    }
    let list_required = recovery["tabId"].is_null();
    let listing_args = if recovery["engine"] == "playwright" {
        json!({"action":"list"})
    } else {
        json!({})
    };
    let snapshot_args = if arguments["snapshotHasPageId"] == true && !list_required {
        let page_id = recovery["tabId"]
            .as_f64()
            .or_else(|| string(&recovery["tabId"]).parse::<f64>().ok())
            .filter(|number| number.is_finite())
            .ok_or_else(|| {
                ServiceError::new(
                    "BROWSER_TAB_CHANGED",
                    "浏览器目标身份无效，请重新读取当前任务页面。",
                    409,
                )
            })?;
        json!({"pageId":page_id})
    } else {
        json!({})
    };
    Ok(
        json!({"listRequired":list_required,"listingArgs":listing_args,"snapshotArgs":snapshot_args}),
    )
}

fn execution_failure(arguments: &Value) -> Value {
    let cancelled = arguments["signalAborted"] == true || arguments["errorName"] == "AbortError";
    if cancelled && arguments["prepared"] != true {
        return json!({"rethrowOriginal":true});
    }
    let failure = if cancelled {
        ServiceError::new(
            "TOOL_CANCELLED",
            "浏览器操作已取消；已发出的动作需要核验。",
            409,
        )
        .value()
    } else {
        arguments["failure"].clone()
    };
    let unknown = !truthy(&arguments["readOnly"])
        && ["MCP_TIMEOUT", "MCP_CONNECTION_LOST", "TOOL_CANCELLED"]
            .contains(&string(&failure["code"]));
    json!({"failure":failure,"browserOutcome":if unknown{"unknown"}else{"failed"},
        "status":if unknown{"unknown"}else if cancelled{"cancelled"}else{"error"}})
}

fn nested_schema(node: &mut Value, mut resource: bool) {
    let Some(object) = node.as_object_mut() else {
        return;
    };
    resource |= object.get("$id").is_some_and(Value::is_string);
    if !resource {
        for keyword in ["$ref", "$dynamicRef", "$recursiveRef"] {
            if let Some(reference) = object.get_mut(keyword) {
                if let Some(text) = reference
                    .as_str()
                    .filter(|text| *text == "#" || text.starts_with("#/"))
                {
                    *reference = json!(format!("#/properties/arguments{}", &text[1..]));
                }
            }
        }
    }
    for keyword in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
        "dependencies",
    ] {
        if let Some(children) = object.get_mut(keyword).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                nested_schema(child, resource);
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for child in children {
                nested_schema(child, resource);
            }
        }
    }
    for keyword in [
        "additionalProperties",
        "unevaluatedProperties",
        "propertyNames",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "items",
        "additionalItems",
        "unevaluatedItems",
        "contentSchema",
    ] {
        if let Some(child) = object.get_mut(keyword) {
            if let Some(children) = child.as_array_mut() {
                for child in children {
                    nested_schema(child, resource);
                }
            } else {
                nested_schema(child, resource);
            }
        }
    }
}

fn call_schema(original: &Value) -> Value {
    let mut nested = original.clone();
    nested_schema(&mut nested, false);
    let mut schema = json!({"type":"object","properties":{"arguments":nested,"policy":{"type":"object","properties":{"reason":{"type":"string","minLength":1,"maxLength":2000,
        "description":"Explain this call to the user-enabled external MCP process. This application reason is never a server argument."}},"required":["reason"],"additionalProperties":false}},"required":["arguments","policy"],"additionalProperties":false});
    if original["$schema"].is_string() {
        schema["$schema"] = original["$schema"].clone();
    }
    schema
}

fn descriptor(
    server: &Value,
    key: &Value,
    name: &str,
    operation: &str,
    schema: Value,
    description: String,
) -> Value {
    json!({"name":format!("mcp.{}.{name}",string(&server["id"])),"description":description,"inputSchema":call_schema(&schema),"originalInputSchema":schema,
        "source":format!("mcp:{}",string(&server["id"])),"serverId":server["id"],"toolName":name,"operation":operation,"key":key})
}

fn resources(arguments: &Value) -> Vec<Value> {
    if !truthy(&arguments["capabilities"]["resources"]) {
        return Vec::new();
    }
    let server = &arguments["server"];
    let key = &arguments["key"];
    let cursor = json!({"type":"string","maxLength":2000});
    let definitions = [
        (
            "kynxa_resources_list",
            "resources/list",
            json!({"type":"object","properties":{"cursor":cursor},"additionalProperties":false}),
        ),
        (
            "kynxa_resources_templates_list",
            "resources/templates/list",
            json!({"type":"object","properties":{"cursor":cursor},"additionalProperties":false}),
        ),
        (
            "kynxa_resources_read",
            "resources/read",
            json!({"type":"object","properties":{"uri":{"type":"string","minLength":1,"maxLength":8192}},"required":["uri"],"additionalProperties":false}),
        ),
    ];
    definitions.into_iter().filter(|(name,_,_)|!array(&arguments["names"]).contains(&json!(name))).map(|(name,operation,schema)|descriptor(server,key,name,operation,schema,
        format!("{}: {operation}. This calls the external MCP service and requires application approval.",string(&server["name"])))).collect()
}

fn descriptors(arguments: &Value) -> ServiceResult<Value> {
    let tools = arguments["listing"]["tools"]
        .as_array()
        .filter(|tools| tools.len() <= 128)
        .ok_or_else(|| ServiceError::new("MCP_INVALID_CATALOG", "MCP 工具目录超限或无效。", 400))?;
    let mut names = Vec::new();
    let mut result = Vec::new();
    let server = &arguments["server"];
    for (index, tool) in tools.iter().enumerate() {
        let name = tool["name"]
            .as_str()
            .filter(|name| matches(r"^[a-zA-Z0-9_.-]{1,128}$", name))
            .ok_or_else(invalid_catalog)?;
        if names.contains(&json!(name))
            || !tool["inputSchema"].is_object()
            || tool["inputSchema"]["type"] != "object"
            || arguments["schemaBytes"][index].as_u64().unwrap_or(u64::MAX) > 32000
        {
            return Err(invalid_catalog());
        }
        names.push(json!(name));
        let description = arguments["descriptions"][index].as_str().unwrap_or(name);
        result.push(descriptor(
            server,
            &arguments["key"],
            name,
            "tools/call",
            tool["inputSchema"].clone(),
            format!(
                "{}: {}",
                string(&server["name"]),
                slice(description, 0, 2000)
            ),
        ));
    }
    let mut resources_arguments = arguments.clone();
    resources_arguments["names"] = json!(names);
    result.extend(resources(&resources_arguments));
    Ok(json!(result))
}

fn server_arguments(input: &Value) -> ServiceResult<Value> {
    fn object(value: &Value) -> ServiceResult<&serde_json::Map<String, Value>> {
        value
            .as_object()
            .ok_or_else(|| ServiceError::new("INVALID_TOOL_ARGUMENTS", "工具参数须为对象。", 400))
    }
    let input_fields = object(input)?;
    object(&input["arguments"])?;
    let policy = object(&input["policy"])?;
    if input_fields
        .keys()
        .any(|key| !["arguments", "policy"].contains(&key.as_str()))
        || policy.keys().any(|key| key != "reason")
    {
        return Err(ServiceError::new(
            "INVALID_MCP_ENVELOPE",
            "MCP 调用须将业务参数和应用审批理由分开。",
            400,
        ));
    }
    if input["policy"]["reason"].as_str().is_none_or(|reason| {
        super::intent::js_trim(reason).is_empty() || reason.encode_utf16().count() > 2000
    }) {
        return Err(ServiceError::new(
            "OUTSIDE_WORKSPACE_REASON_REQUIRED",
            "调用外部 MCP 服务必须提供有效的审批理由。",
            403,
        ));
    }
    Ok(input["arguments"].clone())
}

fn without_metadata(value: &Value) -> Value {
    match value {
        Value::Array(values) => json!(values.iter().map(without_metadata).collect::<Vec<_>>()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| *key != "_meta")
                .map(|(key, value)| (key.clone(), without_metadata(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

// Reuse media labels when artifact hydration appends an image, without resending its base64.
// 附件补读追加图片时复用媒体说明，不再把图片 base64 传回预览计算。
pub(super) fn preview_sections(blocks: &[Value]) -> Vec<String> {
    let mut sections = Vec::new();
    for block in blocks {
        let text = match string(&block["type"]) {
            "text" => string(&block["text"]).to_owned(),
            "resource" => {
                let resource = &block["resource"];
                let label = format!(
                    "[MCP resource; URI: {}; MIME: {}]",
                    resource["uri"].as_str().unwrap_or("unspecified"),
                    resource["mimeType"].as_str().unwrap_or("unspecified")
                );
                format!(
                    "{label}\n{}",
                    resource["text"]
                        .as_str()
                        .unwrap_or("Binary resource retained in the full result for viewing.")
                )
            }
            "resource_link" => format!(
                "[MCP resource link; URI: {}; MIME: {}; name: {}]",
                block["uri"].as_str().unwrap_or("unspecified"),
                block["mimeType"].as_str().unwrap_or("unspecified"),
                block["name"].as_str().unwrap_or("unspecified")
            ),
            "image" | "audio" => format!(
                "[MCP {}; MIME: {}; retained in the full result for attachment/viewing, not sent inline.]",
                string(&block["type"]),
                block["mimeType"].as_str().unwrap_or("unspecified")
            ),
            _ => format!(
                "[MCP unsupported content type: {}; retained in the full result for viewing.]",
                block["type"].as_str().unwrap_or("unspecified")
            ),
        };
        sections.push(text);
    }
    sections
}

fn preview(result: &Value) -> Value {
    let mut projection = json!({"sections":preview_sections(array(&result["content"]))});
    if result.get("structuredContent").is_some() {
        projection["structured"] = without_metadata(&result["structuredContent"]);
    }
    projection
}

struct CatalogMetadata {
    server_id: String,
    tools: Value,
    bytes: usize,
}

/// MCP control owns validated catalog metadata and policy; transport/client lifetimes remain at the SDK boundary.
/// MCP 控制拥有已验证目录元信息与策略；传输和客户端生命周期保留在 SDK 边界。
#[derive(Default)]
pub struct McpState {
    catalog: HashMap<String, CatalogMetadata>,
    order: VecDeque<String>,
    bytes: usize,
    policies: HashMap<String, Value>,
    diagnostics: super::service_mcp_diagnostics::McpDiagnostics,
}

impl McpState {
    fn forget(&mut self, key: &str) {
        if let Some(metadata) = self.catalog.remove(key) {
            self.bytes = self.bytes.saturating_sub(metadata.bytes);
        }
        self.order.retain(|entry| entry != key);
    }
    fn policy(&self, arguments: &Value) -> ServiceResult<Value> {
        let descriptor = &arguments["descriptor"];
        let policy = self
            .policies
            .get(string(&descriptor["serverId"]))
            .unwrap_or(&arguments["fallback"]);
        if policy["enabled"] == false {
            return Err(ServiceError::new(
                "MCP_NOT_ENABLED",
                "此 MCP 服务已停用，请使用当前启用服务。",
                409,
            ));
        }
        if array(&policy["disabledTools"]).contains(&descriptor["toolName"]) {
            return Err(ServiceError::new(
                "TOOL_DISABLED",
                "此 MCP 工具已停用，请使用当前启用工具。",
                409,
            ));
        }
        Ok(Value::Null)
    }
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        if operation.starts_with("mcp.diagnosis.") {
            return self.diagnostics.apply(operation, arguments);
        }
        match operation {
            "mcp.runtimeGuard" => runtime_guard(arguments),
            "mcp.errorActions" => Ok(json!({"retainStartup":arguments["code"]=="MCP_COMMAND_NOT_FOUND",
                "disconnect":arguments["code"]=="MCP_CONNECTION_LOST" && arguments["ownsDiagnosis"]==true})),
            "mcp.recoveryAvailable" => Ok(json!(!arguments["recovery"].is_null() && arguments["hasTools"] == true)),
            "mcp.sdkOptions" => {
                let version = arguments.get("protocolVersion").filter(|version|!version.is_null())
                    .cloned().unwrap_or(json!("2025-11-25"));
                let negotiation = if version == "2026-07-28" { json!({"pin":"2026-07-28"}) } else { json!("legacy") };
                let timeout = arguments.get("startupTimeoutMs").filter(|timeout|!timeout.is_null()).cloned().unwrap_or(json!(15000));
                Ok(json!({"protocolVersion":version,"connectTimeoutMs":timeout,"listRequest":{"timeout":15000,"maxTotalTimeout":15000},
                    "clientOptions":{"capabilities":{},"versionNegotiation":{"mode":negotiation},"inputRequired":{"autoFulfill":false},"listMaxPages":32,
                        "listChanged":{"tools":{"autoRefresh":false,"debounceMs":100},"resources":{"autoRefresh":false,"debounceMs":100}}}}))
            }
            "mcp.requestOptions" => {
                let timeout = arguments.get("timeoutMs").filter(|timeout|!timeout.is_null()).cloned().unwrap_or(json!(30000));
                Ok(json!({"timeout":timeout,"maxTotalTimeout":timeout,"cacheMode":"refresh"}))
            }
            "mcp.recoveryStart" => recovery_start(arguments),
            "mcp.recoveryListing" => {
                let listing = &arguments["listing"];
                let url = &arguments["recovery"]["url"];
                if truthy(&listing["isError"]) || !truthy(&listing["targetVerified"]) || truthy(url) && listing["url"] != *url {
                    return Err(ServiceError::new("BROWSER_TAB_CHANGED", "当前浏览器页面与原快照不一致，请选择任务页面并重新读取。", 409));
                }
                Ok(Value::Null)
            }
            "mcp.recoveryReceipt" => {
                if truthy(&arguments["isError"]) { return Ok(json!({"preserveSnapshot":true})); }
                Ok(json!({"notice":"Browser target and fresh snapshot recovered. The original action was not executed. Select the intended element from this fresh snapshot and call the action again.",
                    "canonicalError":true,"metadata":{"code":"BROWSER_REFERENCE_REFRESHED","originalActionExecuted":false},
                    "result":{"isError":true,"code":"BROWSER_REFERENCE_REFRESHED","outsideWorkspace":true}}))
            }
            "mcp.executionFailure" => Ok(execution_failure(arguments)),
            "mcp.screenshotNotice" => Ok(json!(match string(&arguments["status"]) {
                "available" => "\nScreenshot archived for the local sidebar. No image pixels were sent to this text model.".to_owned(),
                "unavailable" => format!("\nScreenshot preview unavailable ({}). The browser operation result is preserved; do not claim the screenshot was viewed.", string(&arguments["code"])),
                _ => String::new(),
            })),
            "mcp.failure" => {
                let direct = &arguments["code"];
                let code = if direct.is_null() {
                    &arguments["causeCode"]
                } else {
                    direct
                };
                let code = string(code);
                let local = [
                    "MCP_ENV_MISSING",
                    "MCP_AUTH_ORIGIN_REJECTED",
                    "MCP_INVALID_CATALOG",
                    "MCP_PROTOCOL_MISMATCH",
                    "MCP_INPUT_REQUIRED",
                    "MCP_CONNECTION_CAPACITY",
                    "MCP_NOT_CONNECTED",
                    "MCP_NOT_ENABLED",
                    "MCP_CATALOG_CHANGED",
                    "MCP_PROCESS_CLEANUP_FAILED",
                    "MCP_CONFIG_REQUIRED",
                    "MCP_COMMAND_NOT_FOUND",
                ];
                let failure = if local.contains(&string(direct)) {
                    ServiceError::new(
                        string(direct),
                        "MCP 连接或请求未完成。",
                        arguments["statusCode"].as_u64().unwrap_or(409) as u16,
                    )
                } else if arguments["unauthorized"] == true
                    || [
                        "CLIENT_HTTP_AUTHENTICATION",
                        "CLIENT_HTTP_AUTHORIZATION",
                        "invalid_client",
                        "invalid_token",
                        "insufficient_scope",
                    ]
                    .contains(&code)
                {
                    ServiceError::new(
                        "MCP_AUTH_REQUIRED",
                        "MCP 服务需要有效认证，请检查环境变量引用。",
                        401,
                    )
                } else if ["REQUEST_TIMEOUT", "ETIMEDOUT", "UND_ERR_CONNECT_TIMEOUT"]
                    .contains(&code)
                {
                    ServiceError::new("MCP_TIMEOUT", "MCP 请求超时；未自动重试。", 504)
                } else if code == "ENOENT" {
                    ServiceError::new(
                        "MCP_COMMAND_NOT_FOUND",
                        "MCP 启动程序或运行目录不存在，请检查依赖与配置。",
                        409,
                    )
                } else if [
                    "CONNECTION_CLOSED",
                    "NOT_CONNECTED",
                    "SEND_FAILED",
                    "ECONNREFUSED",
                    "ECONNRESET",
                    "EPIPE",
                    "UND_ERR_SOCKET",
                    "ENOTFOUND",
                ]
                .contains(&code)
                {
                    ServiceError::new(
                        "MCP_CONNECTION_LOST",
                        "MCP 连接已中断，请手动重新连接；调用未自动重放。",
                        409,
                    )
                } else {
                    ServiceError::new(
                        arguments["fallback"]
                            .as_str()
                            .unwrap_or("MCP_CONNECTION_FAILED"),
                        "MCP 连接或请求失败；调用未自动重试。",
                        502,
                    )
                };
                Ok(failure.value())
            }
            "mcp.environmentValue" => {
                let value = arguments["value"].as_str().filter(|value| {
                    !value.is_empty()
                        && value.encode_utf16().count() <= 16384
                        && !value.contains(['\0', '\r', '\n'])
                });
                if value.is_none() {
                    return Err(ServiceError::new(
                        "MCP_ENV_MISSING",
                        "MCP 所需的环境变量未配置或无效。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "mcp.transportPlan" => {
                if array(&arguments["server"]["args"])
                    .iter()
                    .any(|argument| string(argument).contains("<absolute-path-to-"))
                {
                    return Err(ServiceError::new(
                        "MCP_CONFIG_REQUIRED",
                        "MCP 配置模板中的文件路径尚未填写。",
                        409,
                    ));
                }
                let explicit = array(&arguments["explicitNames"])
                    .iter()
                    .map(|name| string(name).to_lowercase())
                    .collect::<HashSet<_>>();
                let assignments = array(&arguments["directories"])
                    .iter()
                    .filter(|entry| !explicit.contains(&string(&entry["name"]).to_lowercase()))
                    .collect::<Vec<_>>();
                Ok(
                    json!({"assignments":assignments,"extraEnv":if explicit.contains("uv_link_mode"){json!({})}else{json!({"UV_LINK_MODE":"copy"})},"maxBufferSize":2*1024*1024}),
                )
            }
            "mcp.authOrigin" => {
                if arguments["origin"] != arguments["resourceOrigin"]
                    && arguments["origin"] != arguments["issuerOrigin"]
                {
                    return Err(ServiceError::new(
                        "MCP_AUTH_ORIGIN_REJECTED",
                        "MCP 认证元数据指向未配置的服务。",
                        403,
                    ));
                }
                Ok(Value::Null)
            }
            "mcp.cleanupPlan" => {
                let pid = arguments["pid"]
                    .as_u64()
                    .filter(|pid| *pid > 0 && *pid <= 9_007_199_254_740_991);
                Ok(
                    json!({"kill":pid.is_some(),"pid":pid.map(|pid|pid.to_string()),"timeoutMs":5000,"maxBuffer":32768,"systemRootRequired":pid.is_some()}),
                )
            }
            "mcp.cleanupOutcome" => {
                if arguments["code"].as_i64() != Some(128) {
                    return Err(ServiceError::new(
                        "MCP_PROCESS_CLEANUP_FAILED",
                        "MCP 自有进程清理未完成。",
                        502,
                    ));
                }
                Ok(Value::Null)
            }
            "mcp.connectionCapacity" => {
                if arguments["count"].as_u64().unwrap_or(0) >= 32 {
                    return Err(ServiceError::new(
                        "MCP_CONNECTION_CAPACITY",
                        "MCP 连接已达上限，请刷新连接。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "mcp.catalogPlan" => {
                let mut batches = Vec::new();
                let mut batch = Vec::new();
                let mut connect = Vec::new();
                for (index, server) in array(&arguments["servers"]).iter().enumerate() {
                    if !truthy(&server["enabled"]) {
                        continue;
                    }
                    batch.push(index);
                    if batch.len() == 4 {
                        batches.push(std::mem::take(&mut batch));
                    }
                    if arguments["connect"] == true
                        && (arguments.get("serverIds").is_none()
                            || array(&arguments["serverIds"]).contains(&server["id"]))
                    {
                        connect.push(index);
                    }
                }
                if !batch.is_empty() {
                    batches.push(batch);
                }
                Ok(json!({"batches":batches,"connect":connect}))
            }
            "mcp.key" => Ok(json!(sha256(string(&arguments["text"]).as_bytes()))),
            "mcp.descriptors" => descriptors(arguments),
            "mcp.resources" => Ok(json!(resources(arguments))),
            "mcp.arguments" => server_arguments(&arguments["input"]),
            "mcp.preview" => Ok(preview(&arguments["result"])),
            "mcp.remember" => {
                let key = string(&arguments["key"]);
                self.forget(key);
                let bytes = arguments["bytes"].as_u64().unwrap_or(u64::MAX) as usize;
                if bytes <= CACHE_BYTES {
                    self.catalog.insert(
                        key.into(),
                        CatalogMetadata {
                            server_id: string(&arguments["serverId"]).into(),
                            tools: arguments["tools"].clone(),
                            bytes,
                        },
                    );
                    self.order.push_back(key.into());
                    self.bytes += bytes;
                    while self.catalog.len() > CACHE_ENTRIES || self.bytes > CACHE_BYTES {
                        let Some(key) = self.order.front().cloned() else {
                            break;
                        };
                        self.forget(&key);
                    }
                }
                Ok(Value::Null)
            }
            "mcp.forget" => {
                self.forget(string(&arguments["key"]));
                Ok(Value::Null)
            }
            "mcp.forgetServers" => {
                let keys = self
                    .catalog
                    .iter()
                    .filter(|(_, metadata)| {
                        array(&arguments["ids"]).contains(&json!(metadata.server_id))
                    })
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                for key in keys {
                    self.forget(&key);
                }
                Ok(Value::Null)
            }
            "mcp.clear" => {
                self.catalog.clear();
                self.order.clear();
                self.bytes = 0;
                Ok(Value::Null)
            }
            "mcp.policyUpdate" => {
                let configured = array(&arguments["servers"])
                    .iter()
                    .map(|server| (string(&server["id"]).to_owned(), server.clone()))
                    .collect::<HashMap<_, _>>();
                let mut ids = self.policies.keys().cloned().collect::<Vec<_>>();
                for id in array(&arguments["knownIds"]) {
                    if !ids.contains(&string(id).to_owned()) {
                        ids.push(string(id).into());
                    }
                }
                for id in ids {
                    self.policies.insert(
                        id.clone(),
                        configured
                            .get(&id)
                            .cloned()
                            .unwrap_or(json!({"id":id,"enabled":false})),
                    );
                }
                self.policies.extend(configured);
                Ok(Value::Null)
            }
            "mcp.policyEnsure" => {
                let server = &arguments["server"];
                self.policies
                    .entry(string(&server["id"]).into())
                    .or_insert_with(|| server.clone());
                Ok(Value::Null)
            }
            "mcp.policy" => self.policy(arguments),
            "mcp.observationTools" => {
                let policy = self
                    .policies
                    .get(string(&arguments["serverId"]))
                    .unwrap_or(&arguments["fallback"]);
                if policy["enabled"] == false {
                    return Ok(Value::Null);
                }
                let names = if arguments["engine"] == "playwright" {
                    ["browser_tabs", "browser_snapshot"]
                } else {
                    ["list_pages", "take_snapshot"]
                };
                let indices = names
                    .iter()
                    .map(|name| {
                        array(&arguments["tools"]).iter().position(|tool| {
                            tool["operation"] == "tools/call"
                                && tool["toolName"] == *name
                                && !array(&policy["disabledTools"]).contains(&tool["toolName"])
                        })
                    })
                    .collect::<Option<Vec<_>>>();
                Ok(indices.map(|indices| json!(indices)).unwrap_or(Value::Null))
            }
            "mcp.discovery" => {
                let mut result = Vec::new();
                for record in array(&arguments["records"]) {
                    let server = &record["server"];
                    if !truthy(&server["enabled"]) {
                        continue;
                    }
                    let cached = self.catalog.get(string(&record["key"]));
                    let state = if record["ready"] == true {
                        record["state"].as_str().unwrap_or("ready")
                    } else if record["connecting"] == true {
                        "connecting"
                    } else if record["hasError"] == true {
                        "error"
                    } else {
                        "disconnected"
                    };
                    let tools = cached
                        .map(|cached| array(&cached.tools))
                        .unwrap_or(&[])
                        .iter()
                        .filter(|tool| !array(&server["disabledTools"]).contains(&tool["toolName"]))
                        .map(|tool| {
                            let mut tool = tool.clone();
                            let description = string(&tool["description"]);
                            let suffix = description
                                .split_once(':')
                                .map(|(_, suffix)| suffix)
                                .unwrap_or(description);
                            tool["description"] = json!(format!(
                                "{}: {}",
                                string(&server["name"]),
                                super::intent::js_trim(suffix)
                            ));
                            tool["discoveryOnly"] = json!(true);
                            tool["catalogVerified"] = json!(false);
                            tool["needsConnection"] = json!(true);
                            tool
                        })
                        .collect::<Vec<_>>();
                    result.push(json!({"id":server["id"],"name":format!("mcp.{}",string(&server["id"])),"description":format!("{}: configured MCP service; load this name to connect and verify its current tools.",string(&server["name"])),"transport":server["transport"].as_str().unwrap_or("stdio"),"state":state,"tools":tools}));
                }
                Ok(json!(result))
            }
            "mcp.validateLive" => {
                self.policy(arguments)?;
                let args = server_arguments(&arguments["input"])?;
                if arguments["closed"] == true {
                    return Err(ServiceError::new(
                        "MCP_NOT_CONNECTED",
                        "MCP 连接已中断，请手动重新连接。",
                        409,
                    ));
                }
                let current = &arguments["current"];
                let descriptor = &arguments["descriptor"];
                if arguments["hasCatalog"] == true
                    && (current.is_null()
                        || current["operation"] != descriptor["operation"]
                        || current["toolName"] != descriptor["toolName"]
                        || arguments["currentSchemaText"] != arguments["descriptorSchemaText"])
                {
                    return Err(ServiceError::new(
                        "MCP_CATALOG_CHANGED",
                        "MCP 工具目录已变化，请重新准备调用和审批。",
                        409,
                    ));
                }
                Ok(args)
            }
            "mcp.outcome" => {
                let result = &arguments["result"];
                let navigation=arguments["descriptor"]["toolName"]=="navigate_page"&&array(&result["content"]).iter().any(|block|block["type"]=="text"&&matches(r"(?m)^Unable to (?:navigate(?: back| forward)? in the selected page|reload the selected page): ",string(&block["text"])));
                let timed_out=arguments["prepared"]==true&&(result["isError"]==true||navigation)&&array(&result["content"]).iter().any(|block|block["type"]=="text"&&matches(r"(?i)TimeoutError|(?:timed out|timeout).{0,40}(?:exceeded|after|ms)|Timeout \d+ms exceeded",string(&block["text"])));
                let unknown = timed_out && arguments["readOnly"] != true;
                let failed = result["isError"] == true || navigation;
                let mut receipt = Vec::new();
                if unknown { receipt.push(json!(["status","unknown"])); }
                if timed_out { receipt.push(json!(["code","MCP_TIMEOUT"])); }
                else if navigation { receipt.push(json!(["code","MCP_BROWSER_NAVIGATION_FAILED"])); }
                let mut outcome = json!({"navigationFailed":navigation,"browserTimedOut":timed_out,"outcomeUnknown":unknown,
                    "diagnosticCode":if timed_out{json!("MCP_TIMEOUT")}else{Value::Null},
                    "browserOutcome":if unknown{"unknown"}else if failed{"failed"}else{"completed"},"isError":failed||timed_out,"receipt":receipt});
                if arguments["includePreview"] == true {
                    outcome["preview"] = preview(result);
                }
                Ok(outcome)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown MCP control operation.",
                400,
            )),
        }
    }
}

#[cfg(test)]
mod runtime_policy_tests {
    use super::McpState;
    use serde_json::json;

    #[test]
    fn browser_failure_keeps_read_outcomes_separate_from_uncertain_effects() {
        let mut state = McpState::default();
        let mut facts = json!({"prepared":true,"readOnly":true,"failure":{"code":"MCP_TIMEOUT","message":"timeout"}});
        let read = state.apply("mcp.executionFailure", &facts).unwrap();
        assert_eq!(read["status"], "error");
        assert_eq!(read["browserOutcome"], "failed");
        facts["readOnly"] = json!(false);
        let effect = state.apply("mcp.executionFailure", &facts).unwrap();
        assert_eq!(effect["status"], "unknown");
        assert_eq!(effect["browserOutcome"], "unknown");
        facts["signalAborted"] = json!(true);
        facts["prepared"] = json!(false);
        assert_eq!(
            state.apply("mcp.executionFailure", &facts).unwrap()["rethrowOriginal"],
            true
        );
    }

    #[test]
    fn recovery_refuses_changed_targets_before_selecting_sdk_steps() {
        let mut state = McpState::default();
        let facts = json!({"plan":{"tabId":"1","connectionSelectedTabId":"1"},
            "recovery":{"tabId":"1","connectionSelectedTabId":"2"},"hasTools":true});
        assert_eq!(
            state.apply("mcp.recoveryStart", &facts).unwrap_err().code,
            "BROWSER_TAB_CHANGED"
        );
        let listing = json!({"recovery":{"url":"https://fixture.example/"},
            "listing":{"targetVerified":true,"url":"https://other.example/"}});
        assert_eq!(
            state
                .apply("mcp.recoveryListing", &listing)
                .unwrap_err()
                .code,
            "BROWSER_TAB_CHANGED"
        );
        let negotiated = json!({"phase":"negotiated","expectedVersion":"2026-07-28","negotiatedVersion":"2025-11-25"});
        assert_eq!(
            state
                .apply("mcp.runtimeGuard", &negotiated)
                .unwrap_err()
                .code,
            "MCP_PROTOCOL_MISMATCH"
        );
    }
}
