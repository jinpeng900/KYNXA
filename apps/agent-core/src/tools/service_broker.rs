use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use url::Url;

use super::service_error::{ServiceError, ServiceResult};
use super::service_web::{automatic_browser, provider_kind, public_fetch};
use super::{configuration, contracts, discovery, intent, policy};
use crate::models::protocol::{array, string};

fn rule(operation: &str, arguments: &Value) -> ServiceResult<Value> {
    let result = if operation.starts_with("tools.browser")
        || operation == "tools.backgroundBrowserConnection"
    {
        intent::calculate(operation, arguments)
    } else if ["tools.configuredEnabled", "tools.configurationChanged"].contains(&operation) {
        configuration::calculate(operation, arguments)
    } else if operation == "tools.nativeAvailability" {
        policy::calculate(operation, arguments)
    } else {
        contracts::calculate(operation, arguments)
    };
    result.map_err(|code| ServiceError::new(&code, "工具服务规则调用失败。", 400))
}

pub(super) fn connection_facts(server: &Value) -> ServiceResult<(Value, Value)> {
    let mut keys = HashSet::new();
    for field in ["env", "envRefs"] {
        if let Some(values) = server[field].as_object() {
            keys.extend(values.keys().cloned());
        }
    }
    let projection = json!({"transport":server["transport"],"command":server["command"],"args":server["args"],"envKeys":keys});
    let connection = rule("tools.browserConnection", &json!({"server":projection}))?;
    Ok((projection, connection))
}

fn bounded(value: &Value, fallback: usize, minimum: usize, maximum: usize) -> ServiceResult<usize> {
    if value.is_null() {
        return Ok(fallback);
    }
    value
        .as_u64()
        .filter(|value| (*value >= minimum as u64) && (*value <= maximum as u64))
        .map(|value| value as usize)
        .ok_or_else(|| {
            ServiceError::new(
                "INVALID_TOOL_ARGUMENTS",
                &format!("整数参数须在 {minimum}–{maximum} 范围内。"),
                400,
            )
        })
}

fn identity_fact(fact: &Value) -> ServiceResult<&str> {
    if let Some(error) = fact.get("error") {
        return Err(ServiceError::new(
            string(&error["code"]),
            string(&error["message"]),
            error["statusCode"].as_u64().unwrap_or(400) as u16,
        ));
    }
    Ok(string(&fact["value"]))
}

fn browser_allowed(context: &Value, server: &Value) -> ServiceResult<bool> {
    if context.is_null() || server.is_null() {
        return Ok(true);
    }
    let (projection, facts) = connection_facts(server)?;
    let connection = &facts["connection"];
    if connection.is_null() {
        return Ok(true);
    }
    let browser_intent = context.get("browserTaskIntent").cloned().unwrap_or(rule(
        "tools.browserIntent",
        &json!({"message":context["message"]}),
    )?);
    let executable = rule(
        "tools.browserExecutable",
        &json!({"args":server["args"],"engine":connection["engine"]}),
    )?;
    let endpoint = Url::parse(string(&facts["endpoint"])).ok().map(|url| json!({"protocol":format!("{}:",url.scheme()),"hostname":url.host_str().unwrap_or("")}));
    let background = rule(
        "tools.backgroundBrowserConnection",
        &json!({"server":projection,"endpointUrl":endpoint}),
    )?;
    Ok(rule(
        "tools.browserAllowed",
        &json!({"intent":browser_intent,"executable":executable,"background":background}),
    )? == true)
}

#[derive(Default)]
pub struct BrokerState {
    context: Value,
    builtins: Vec<Value>,
    snapshot: Option<Value>,
    inventory: HashMap<String, Value>,
    descriptors: HashMap<String, Value>,
    revoked: HashSet<String>,
    generation: u64,
    discovery_refreshed: bool,
    evidence_budget: Option<Value>,
    work_transition: Option<Value>,
    configuration_generation: u64,
    live_configuration: Option<Value>,
    stages: HashSet<String>,
}

impl BrokerState {
    fn enabled(&self, descriptor: &Value, config: &Value) -> ServiceResult<bool> {
        Ok(rule(
            "tools.configuredEnabled",
            &json!({"config":config,"descriptor":descriptor}),
        )? == true)
    }

    fn changed(
        &self,
        descriptor: &Value,
        previous: &Value,
        current: &Value,
    ) -> ServiceResult<bool> {
        Ok(rule(
            "tools.configurationChanged",
            &json!({"previous":previous,"current":current,"descriptor":descriptor}),
        )? == true)
    }

    fn search(&self, arguments: &Value) -> ServiceResult<Value> {
        let descriptors = array(&arguments["descriptors"]);
        let mut seen = HashSet::new();
        let mut filtered = Vec::new();
        let mut original = Vec::new();
        for (index, tool) in descriptors.iter().enumerate() {
            if !seen.insert(string(&tool["name"]).to_owned()) {
                continue;
            }
            if !self.enabled(tool, &arguments["currentConfig"])?
                || self.changed(
                    tool,
                    &arguments["previousConfig"],
                    &arguments["currentConfig"],
                )?
                || tool["webAvailable"] == false
            {
                continue;
            }
            let mut projection = tool.clone();
            projection["wireName"] =
                json!(crate::models::protocol::wire_name(string(&tool["name"])));
            filtered.push(projection);
            original.push(index);
        }
        let indices = discovery::calculate(
            "tools.searchTools",
            &json!({"descriptors":filtered,"query":arguments["query"]}),
        )
        .map_err(|code| ServiceError::new(&code, "工具搜索失败。", 400))?;
        let headers = array(&arguments["headers"]);
        let header_indices = discovery::calculate(
            "tools.searchTools",
            &json!({"descriptors":headers,"query":arguments["query"]}),
        )
        .map_err(|code| ServiceError::new(&code, "服务搜索失败。", 400))?;
        let offset = bounded(&arguments["offset"], 0, 0, 100000)?;
        let limit = bounded(&arguments["limit"], 10, 1, 20)?;
        let selected = array(&arguments["selectedNames"]);
        let tools=array(&indices).iter().skip(offset).take(limit).filter_map(Value::as_u64).map(|index|{
            let index=original[index as usize];let tool=&descriptors[index];json!({"index":index,"schemaState":if tool["discoveryOnly"]==true{"needs-connection"}else if selected.contains(&tool["name"]){"loaded"}else{"deferred"},"discoveryOnly":tool["discoveryOnly"]==true})
        }).collect::<Vec<_>>();
        let total = array(&indices).len();
        let server_total = array(&header_indices).len();
        let signals = discovery::calculate(
            "tools.selectionSignals",
            &json!({"message":arguments["query"]}),
        )
        .map_err(|code| ServiceError::new(&code, "工具搜索线索失败。", 400))?;
        let mut result = json!({"tools":tools,"offset":offset,"nextOffset":total.min(offset+limit),"total":total,"hasMore":offset+limit<total,
            "capabilityCheck":signals["deviceState"]==true||self.context["toolIntent"]["deviceState"]==true});
        if server_total > 0 {
            result["servers"] = json!(
                array(&header_indices)
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .collect::<Vec<_>>()
            );
            result["serverTotal"] = json!(server_total);
            result["serverNextOffset"] = json!(server_total.min(offset + limit));
            result["serverHasMore"] = json!(offset + limit < server_total);
        }
        Ok(result)
    }

    fn load_connections(&self, arguments: &Value) -> ServiceResult<Value> {
        let headers = array(&arguments["headers"]);
        let descriptors = array(&arguments["descriptors"]);
        let connections = array(&arguments["connections"]);
        let mut servers = Vec::new();
        let mut heads = Vec::new();
        for name in array(&arguments["names"]) {
            let existing = descriptors.iter().find(|tool| tool["name"] == *name);
            if existing.is_some_and(|tool| {
                self.enabled(tool, &arguments["currentConfig"])
                    .unwrap_or(false)
                    == false
            }) {
                continue;
            }
            let server = headers.iter().find(|header| {
                header["name"] == *name
                    || array(&header["tools"])
                        .iter()
                        .any(|tool| tool["name"] == *name)
                    || existing.is_some_and(|tool| tool["serverId"] == header["id"])
            });
            let live = connections
                .iter()
                .find(|connection| connection["name"] == *name && connection["closed"] != true)
                .map(|connection| &connection["descriptor"]);
            let matches = existing.zip(live).is_some_and(|(existing, live)| {
                !live.is_null()
                    && [
                        "operation",
                        "toolName",
                        "inputSchema",
                        "originalInputSchema",
                    ]
                    .iter()
                    .all(|field| existing[*field] == live[*field])
            });
            let unchanged = existing
                .map(|tool| {
                    self.changed(
                        tool,
                        &arguments["previousConfig"],
                        &arguments["currentConfig"],
                    )
                })
                .transpose()?
                .unwrap_or(false)
                == false;
            if server.is_none() || matches && unchanged {
                continue;
            }
            let server = server.unwrap();
            if !servers.contains(&server["id"]) {
                servers.push(server["id"].clone());
            }
            if server["name"] == *name {
                heads.push(name.clone());
            }
        }
        Ok(json!({"servers":servers,"heads":heads}))
    }

    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        if operation.starts_with("broker.preflight.") {
            return super::service_broker_preflight::apply(operation, arguments);
        }
        if operation.starts_with("broker.completion.") {
            return super::service_broker_completion::apply(operation, arguments);
        }
        match operation {
            "broker.stageAdd" => {
                if let Some(stage) = arguments["stage"].as_str() {
                    self.stages.insert(stage.to_owned());
                }
                Ok(Value::Null)
            }
            "broker.stageTake" => Ok(json!(self.stages.drain().collect::<Vec<_>>())),
            "broker.contextPreflight" => {
                if arguments["closed"] == true {
                    return Err(ServiceError::new(
                        "TOOL_SERVICE_CLOSED",
                        "工具服务已关闭。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "broker.closedReceipt" => {
                Ok(ServiceError::new("TOOL_SERVICE_CLOSED", "工具服务已关闭。", 409).value())
            }
            "broker.permissionMode" => {
                if !["ask", "smart", "full"].contains(&string(&arguments["permissionMode"])) {
                    return Err(ServiceError::new(
                        "INVALID_TOOL_ARGUMENTS",
                        "工具权限模式无效。",
                        400,
                    ));
                }
                Ok(Value::Null)
            }
            "broker.ownershipCatalog" => {
                let conversation_id = string(&arguments["conversationId"]);
                for project in array(&arguments["projects"]) {
                    let mut matched = false;
                    for identity in array(&project["chatIds"]) {
                        if identity_fact(identity)? == conversation_id {
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        continue;
                    }
                    let project_id = identity_fact(&project["projectId"])?;
                    let facts = &project["pathFacts"];
                    let workspace = if project["folderless"] == true || facts["absolute"] != true {
                        Value::Null
                    } else {
                        facts["resolved"].clone()
                    };
                    let managed = !workspace.is_null()
                        && facts["parentRelative"] == ""
                        && facts["reverseRelative"] == ""
                        && facts["basenameIdentity"]["value"] == project_id;
                    return Ok(
                        json!({"projectId":project_id,"workspaceRoot":workspace,"managedWorkspace":managed}),
                    );
                }
                for identity in array(&arguments["chatIds"]) {
                    if identity_fact(identity)? == conversation_id {
                        return Ok(
                            json!({"projectId":null,"workspaceRoot":null,"managedWorkspace":false}),
                        );
                    }
                }
                Ok(Value::Null)
            }
            "broker.contentRange" => {
                let length = arguments["length"].as_u64().unwrap_or(0);
                let maximum = 65_536_u64;
                let marker = "\n[Tool result truncated]";
                Ok(
                    json!({"fits":length<=maximum,"end":if length<=maximum{length}else{maximum-marker.encode_utf16().count() as u64},
                    "marker":if length<=maximum{""}else{marker}}),
                )
            }
            "broker.configuration" => Ok(json!({"generation":self.configuration_generation})),
            "broker.configurationUpdate" => {
                let mut previous = arguments["previous"].clone();
                let mut current = arguments["current"].clone();
                let expected = arguments["expectedRevision"] == previous["revision"];
                previous["revision"] = json!(0);
                current["revision"] = json!(0);
                if expected && previous == current {
                    return Ok(json!({"changed":false,"servers":[]}));
                }
                self.configuration_generation = self.configuration_generation.saturating_add(1);
                self.live_configuration = Some(arguments["current"].clone());
                let servers = configuration::calculate("tools.changedMcpServerIds", arguments)
                    .map_err(|code| ServiceError::new(&code, "MCP 配置变化分析失败。", 400))?;
                Ok(json!({"changed":true,"servers":servers}))
            }
            "broker.evidenceSet" => {
                self.evidence_budget = Some(
                    arguments["maximumTokens"]
                        .as_f64()
                        .map(|tokens| json!(tokens.floor().clamp(0.0, 32768.0)))
                        .unwrap_or(json!({"invalidNumber":true})),
                );
                Ok(Value::Null)
            }
            "broker.evidenceGet" => Ok(self.evidence_budget.clone().unwrap_or(Value::Null)),
            "broker.evidenceArguments" => {
                let mut input = arguments["input"].clone();
                if let Some(budget) = &self.evidence_budget {
                    let proposed = input["maximumTokens"].as_f64().unwrap_or(32768.0);
                    input["maximumTokens"] = budget
                        .as_f64()
                        .map(|remaining| json!(proposed.min(remaining)))
                        .unwrap_or(budget.clone());
                }
                Ok(input)
            }
            "broker.evidenceAudit" => {
                let mut audit = arguments["audit"].clone();
                if !audit.is_object() {
                    return Ok(audit);
                }
                let input = &arguments["input"];
                audit["modelRequested"] =
                    json!({"limit":input["limit"],"maximumTokens":input["maximumTokens"]});
                audit["availableContextTokens"] =
                    self.evidence_budget.clone().unwrap_or(Value::Null);
                if let Some(remaining) = self.evidence_budget.as_ref().and_then(Value::as_f64) {
                    let proposed = input
                        .get("maximumTokens")
                        .filter(|value| !value.is_null())
                        .unwrap_or(&audit["configured"]["maximumTokens"])
                        .clone();
                    if proposed.as_f64().is_some_and(|tokens| tokens > remaining) {
                        if let Some(reasons) = audit["earlyCutReasons"].as_array_mut() {
                            reasons.push(json!({"reason":"remaining-context","field":"maximumTokens","proposed":proposed,"approved":arguments["approved"],"unit":"tokens"}));
                        }
                    }
                }
                Ok(audit)
            }
            "broker.workBinding" => Ok(self
                .work_transition
                .as_ref()
                .map(|transition| transition["workspaceRoot"].clone())
                .unwrap_or_else(|| self.context["linkedWorkspaceRoot"].clone())),
            "broker.workRequest" => {
                if !crate::models::protocol::truthy(&arguments["context"]["projectId"]) {
                    return Err(ServiceError::new(
                        "WORK_REQUIRED",
                        "当前聊天不属于工作，请先创建工作后关联目录。",
                        409,
                    ));
                }
                if super::intent::js_trim(string(&arguments["input"]["reason"])).is_empty() {
                    return Err(ServiceError::new(
                        "WORK_BINDING_REASON_REQUIRED",
                        "请说明关联目录的长期工作用途。",
                        400,
                    ));
                }
                if arguments["absolute"] != true {
                    return Err(ServiceError::new(
                        "WORK_BINDING_PATH_REQUIRED",
                        "关联工作目录需要明确的绝对路径。",
                        400,
                    ));
                }
                Ok(Value::Null)
            }
            "broker.workTarget" => {
                if arguments["owned"] == true || arguments["readOnly"] == true {
                    return Err(ServiceError::new(
                        "PROTECTED_APP_DATA",
                        "应用存储和扩展目录不能作为外部工作目录。",
                        403,
                    ));
                }
                Ok(Value::Null)
            }
            "broker.workDirectory" => {
                if arguments["directory"] != true {
                    return Err(ServiceError::new(
                        "WORK_BINDING_NOT_DIRECTORY",
                        "指定的工作路径不是文件夹。",
                        400,
                    ));
                }
                Ok(Value::Null)
            }
            "broker.workBound" => {
                self.work_transition = Some(json!({"workspaceRoot":arguments["workspaceRoot"]}));
                Ok(Value::Null)
            }
            "broker.ownership" => {
                let current = &arguments["current"];
                let own_transition = self.work_transition.as_ref().is_some_and(|transition| {
                    current["projectId"] == self.context["projectId"]
                        && current["workspaceRoot"] == transition["workspaceRoot"]
                });
                if current["projectId"] != self.context["projectId"]
                    || !own_transition
                        && current["workspaceRoot"] != self.context["linkedWorkspaceRoot"]
                {
                    return Err(ServiceError::new(
                        "WORKSPACE_CHANGED",
                        "聊天工作范围已变化，此工具调用已停止。",
                        409,
                    ));
                }
                Ok(json!({"requiresWorkspaceCheck":!own_transition}))
            }
            "broker.workspaceCheck" => {
                if arguments["isolatedWorkspace"] != self.context["isolatedWorkspace"] {
                    return Err(ServiceError::new(
                        "WORKSPACE_CHANGED",
                        "聊天工作范围已变化，此工具调用已停止。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "broker.init" => {
                self.context = arguments["context"].clone();
                self.builtins = array(&arguments["builtins"]).to_vec();
                Ok(Value::Null)
            }
            "broker.allowedServers" => Ok(json!(
                array(&arguments["servers"])
                    .iter()
                    .enumerate()
                    .map(|(index, server)| Ok((index, browser_allowed(&self.context, server)?)))
                    .collect::<ServiceResult<Vec<_>>>()?
                    .into_iter()
                    .filter(|(_, allowed)| *allowed)
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            )),
            "broker.search" => self.search(arguments),
            "broker.loadConnections" => self.load_connections(arguments),
            "broker.refresh" => {
                let refresh = !self.discovery_refreshed
                    && arguments["hasSnapshot"] == true
                    && (arguments["snapshotGeneration"] != arguments["generation"]
                        || arguments["hasErrors"] == true);
                if refresh {
                    self.discovery_refreshed = true;
                }
                Ok(json!(refresh))
            }
            "broker.retained" => Ok(json!(
                array(&arguments["selected"])
                    .iter()
                    .filter(|name| array(&arguments["available"]).contains(name))
                    .collect::<Vec<_>>()
            )),
            "broker.skillPage" => {
                let skills = array(&arguments["skills"]);
                let offset = bounded(&arguments["offset"], 0, 0, 128)?;
                let limit = bounded(&arguments["limit"], 32, 1, 128)?;
                let mut indices = Vec::new();
                let mut characters = 2_u64;
                for index in offset..skills.len().min(offset + limit) {
                    let size = arguments["serializedLengths"][index]
                        .as_u64()
                        .unwrap_or(u64::MAX);
                    let next = characters
                        .saturating_add(size)
                        .saturating_add(u64::from(!indices.is_empty()));
                    if next > 64536 {
                        break;
                    }
                    characters = next;
                    indices.push(index);
                }
                Ok(
                    json!({"indices":indices,"offset":offset,"nextOffset":offset+indices.len(),"hasMore":offset+indices.len()<skills.len(),"total":skills.len()}),
                )
            }
            "broker.catalog" => {
                let config = &arguments["config"];
                let effective = &arguments["effectiveConfig"];
                let settings = &arguments["retrievalSettings"];
                let servers = array(&effective["mcpServers"]);
                let previous = self.descriptors.clone();
                let mut changes = Vec::new();
                let mut selected = Vec::new();
                self.inventory.clear();
                self.descriptors.clear();
                for (index, original) in array(&arguments["descriptors"]).iter().enumerate() {
                    let mut descriptor = original.clone();
                    if descriptor["source"] == "builtin" {
                        let availability = rule(
                            "tools.nativeAvailability",
                            &json!({"name":descriptor["name"],"context":self.context}),
                        )?;
                        if let Some(availability) = availability.as_object() {
                            for (key, value) in availability {
                                descriptor[key] = value.clone();
                            }
                        }
                    } else {
                        descriptor["enabled"] = json!(
                            rule(
                                "tools.configuredEnabled",
                                &json!({"config":effective,"descriptor":descriptor})
                            )? == true
                                && rule(
                                    "tools.configurationChanged",
                                    &json!({"previous":config,"current":effective,"descriptor":descriptor})
                                )? != true
                        );
                    }
                    let server = servers.iter().find(|server| {
                        server["id"] == descriptor["serverId"] && server["enabled"] != false
                    });
                    let owned = descriptor["serverId"].as_str().is_some_and(|id| {
                        descriptor["source"] == format!("mcp:{id}")
                            && string(&descriptor["name"]).starts_with(&format!("mcp.{id}."))
                    }) && servers.len() <= 64;
                    let connection = if owned {
                        server
                            .map(connection_facts)
                            .transpose()?
                            .map(|(_, facts)| facts["connection"].clone())
                    } else {
                        None
                    };
                    descriptor["executionEnvironment"] = rule(
                        "tools.resolveEnvironment",
                        &json!({"descriptor":{"name":descriptor["name"],"source":descriptor["source"]},
                        "server":if owned{server.map(|server|json!({"transport":server["transport"]}))}else{None},"connection":connection,"sandbox":self.context["sandboxCapabilities"]}),
                    )?;
                    let name = string(&descriptor["name"]).to_owned();
                    let enabled = descriptor["enabled"] != false
                        && descriptor["available"] != false
                        && browser_allowed(&self.context, server.unwrap_or(&Value::Null))?
                        && (!name.starts_with("knowledge.")
                            || arguments["hasRetrieval"] == true
                                && settings["local"]["enabled"] != false)
                        && (!name.starts_with("memory.") || arguments["hasMemory"] == true)
                        && (settings["web"]["browserRead"] != "off"
                            || !automatic_browser(&self.context, &descriptor))
                        && (settings["web"]["mode"] != "off"
                            || !(name.starts_with("web.")
                                || provider_kind(&descriptor).is_some()
                                || public_fetch(&descriptor)));
                    if enabled {
                        selected.push(index);
                        self.descriptors.insert(name.clone(), descriptor.clone());
                    }
                    self.inventory.insert(name, descriptor.clone());
                    changes.push(json!({"index":index,"enabled":descriptor["enabled"],"availability":descriptor.get("available"),"unavailableCode":descriptor.get("unavailableCode"),"executionEnvironment":descriptor["executionEnvironment"]}));
                }
                for (name, descriptor) in previous {
                    if string(&descriptor["source"]).starts_with("mcp:")
                        && rule(
                            "tools.configuredEnabled",
                            &json!({"config":effective,"descriptor":descriptor}),
                        )? != true
                    {
                        self.revoked.insert(name);
                    }
                }
                for name in self.descriptors.keys() {
                    self.revoked.remove(name);
                }
                self.snapshot = Some(config.clone());
                self.generation = arguments["generation"].as_u64().unwrap_or(0);
                Ok(json!({"changes":changes,"selected":selected,"revokedNames":self.revoked}))
            }
            "broker.assertConfiguration" => {
                if self.snapshot.is_none() || arguments["generation"] == self.generation {
                    return Ok(Value::Null);
                }
                if rule(
                    "tools.configurationChanged",
                    &json!({"previous":self.snapshot,"current":arguments["config"],"descriptor":arguments["descriptor"],"args":arguments["arguments"]}),
                )? == true
                {
                    let mut error = ServiceError::new(
                        "AGENT_CONFIG_CHANGED",
                        "此工具的配置或权限已变化，请重新发现该能力；无关工具可以继续。",
                        409,
                    )
                    .value();
                    error["toolConfigurationRevoked"] = json!(true);
                    return Ok(json!({"failure":error}));
                }
                Ok(Value::Null)
            }
            "broker.resolve" => {
                let name = string(&arguments["name"]);
                if self.descriptors.contains_key(name) {
                    return Ok(json!({"name":name,"builtin":false}));
                }
                if self
                    .builtins
                    .iter()
                    .any(|descriptor| descriptor["name"] == name)
                {
                    return Ok(json!({"name":name,"builtin":true}));
                }
                // An injected SDK schema can preserve a denial diagnosis, but never gains native catalog authority.
                // 注入的 SDK schema 只能保留拒绝诊断，不能获得原生目录执行权限。
                let candidate = &arguments["candidate"];
                if candidate["name"] == name {
                    if let Some(server_id) = candidate["serverId"].as_str() {
                        if candidate["source"] == format!("mcp:{server_id}")
                            && name.starts_with(&format!("mcp.{server_id}."))
                        {
                            if let Some(server) = self
                                .snapshot
                                .as_ref()
                                .and_then(|config| config["mcpServers"].as_array())
                                .and_then(|servers| {
                                    servers.iter().find(|server| server["id"] == server_id)
                                })
                            {
                                if !browser_allowed(&self.context, server)? {
                                    return Err(ServiceError::new(
                                        "BROWSER_TASK_NOT_AUTHORIZED",
                                        "当前用户任务没有授权此浏览器操作。",
                                        403,
                                    ));
                                }
                            }
                        }
                    }
                }
                if self.revoked.contains(name) {
                    return Ok(
                        json!({"failure":{"code":"AGENT_CONFIG_CHANGED","message":"此工具曾可用，但当前配置已撤销；请发现其他能力。","statusCode":409,"toolConfigurationRevoked":true}}),
                    );
                }
                Err(ServiceError::new(
                    "TOOL_NOT_FOUND",
                    "工具不存在或尚未发现。",
                    404,
                ))
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown tool broker operation.",
                400,
            )),
        }
    }
}
