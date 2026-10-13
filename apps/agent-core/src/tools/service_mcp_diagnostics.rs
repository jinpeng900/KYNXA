use std::collections::HashMap;

use serde_json::{Value, json};

use super::service_error::{ServiceError, ServiceResult};

struct DiagnosticRecord {
    value: Value,
    revision: u64,
}

/// Diagnosis revisions prevent late SDK successes from erasing newer failures or reviving replaced connections.
/// 诊断修订号防止晚到 SDK 成功清除新故障，或恢复已替换连接。
#[derive(Default)]
pub struct McpDiagnostics {
    records: HashMap<u64, DiagnosticRecord>,
    states: Vec<(String, u64)>,
    errors: Vec<(String, Value)>,
    next_id: u64,
    connection_generation: u64,
    server_generations: HashMap<String, u64>,
}

impl McpDiagnostics {
    fn install(&mut self, server_id: &str, value: Value) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.records
            .insert(id, DiagnosticRecord { value, revision: 0 });
        if let Some((_, current)) = self.states.iter_mut().find(|(key, _)| key == server_id) {
            *current = id;
        } else {
            self.states.push((server_id.into(), id));
        }
        id
    }

    fn error(&mut self, server_id: &str, value: Value) {
        if let Some((_, current)) = self.errors.iter_mut().find(|(key, _)| key == server_id) {
            *current = value;
        } else {
            self.errors.push((server_id.into(), value));
        }
    }

    fn owns(&self, arguments: &Value) -> bool {
        arguments["runtimeOwned"] == true
            && arguments["stateId"].as_u64().is_some_and(|id| {
                self.states.iter().any(|(key, current)| {
                    key == arguments["serverId"].as_str().unwrap_or("") && *current == id
                })
            })
    }

    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        let server_id = arguments["serverId"].as_str().unwrap_or("");
        let state_id = arguments["stateId"].as_u64().unwrap_or(0);
        match operation {
            "mcp.diagnosis.generation" => Ok(json!(self.connection_generation)),
            "mcp.diagnosis.resetGeneration" => {
                self.connection_generation = self.connection_generation.saturating_add(1);
                Ok(json!(self.connection_generation))
            }
            "mcp.diagnosis.serverGeneration" => Ok(json!(
                self.server_generations.get(server_id).copied().unwrap_or(0)
            )),
            "mcp.diagnosis.resetServer" => {
                let generation = self.server_generations.entry(server_id.into()).or_default();
                *generation = generation.saturating_add(1);
                Ok(json!(*generation))
            }
            "mcp.diagnosis.assertGeneration" => {
                if arguments["closed"] == true {
                    return Err(ServiceError::new(
                        "TOOL_SERVICE_CLOSED",
                        "MCP 客户端已关闭。",
                        409,
                    ));
                }
                if arguments["resetting"] == true
                    || arguments["generation"].as_u64() != Some(self.connection_generation)
                {
                    return Err(ServiceError::new(
                        "AGENT_CONFIG_CHANGED",
                        "MCP 连接在发现期间已重置，请开始新请求。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.assertServer" => {
                if arguments["resetting"] == true
                    || arguments["generation"].as_u64()
                        != Some(self.server_generations.get(server_id).copied().unwrap_or(0))
                {
                    return Err(ServiceError::new(
                        "MCP_CATALOG_CHANGED",
                        "此 MCP 服务目录在发现期间变化，请重新准备此服务调用。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.get" => {
                if arguments["kind"] == "state" {
                    Ok(self
                        .states
                        .iter()
                        .find(|(key, _)| key == server_id)
                        .map(|(_, id)| json!({"id":id}))
                        .unwrap_or(Value::Null))
                } else {
                    Ok(self
                        .errors
                        .iter()
                        .find(|(key, _)| key == server_id)
                        .map(|(_, value)| json!({"value":value}))
                        .unwrap_or(Value::Null))
                }
            }
            // Queries return only the requested fact without cloning the full diagnostic records.
            // 查询仅返回所需事实，不克隆完整诊断记录，状态仍由原生账本拥有。
            "mcp.diagnosis.has" => Ok(json!(if arguments["kind"] == "state" {
                self.states.iter().any(|(key, _)| key == server_id)
            } else {
                self.errors.iter().any(|(key, _)| key == server_id)
            })),
            "mcp.diagnosis.size" => Ok(json!(if arguments["kind"] == "state" {
                self.states.len()
            } else {
                self.errors.len()
            })),
            "mcp.diagnosis.entries" => {
                if arguments["kind"] == "state" {
                    Ok(json!(self.states))
                } else {
                    Ok(json!(self.errors))
                }
            }
            "mcp.diagnosis.set" => {
                if arguments["kind"] == "state" {
                    Ok(json!(self.install(server_id, arguments["value"].clone())))
                } else {
                    self.error(server_id, arguments["value"].clone());
                    Ok(Value::Null)
                }
            }
            "mcp.diagnosis.delete" => {
                let existed = if arguments["kind"] == "state" {
                    let previous_size = self.states.len();
                    self.states.retain(|(key, _)| key != server_id);
                    self.states.len() != previous_size
                } else {
                    let previous_size = self.errors.len();
                    self.errors.retain(|(key, _)| key != server_id);
                    self.errors.len() != previous_size
                };
                Ok(json!(existed))
            }
            "mcp.diagnosis.clear" => {
                if arguments["kind"] == "state" {
                    self.states.clear();
                } else {
                    self.errors.clear();
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.field" => {
                let value = self
                    .records
                    .get(&state_id)
                    .and_then(|record| record.value.get(arguments["field"].as_str().unwrap_or("")));
                Ok(json!({"present":value.is_some(),"value":value}))
            }
            "mcp.diagnosis.patch" => {
                if let Some(record) = self.records.get_mut(&state_id) {
                    if let Some(fields) = arguments["fields"].as_object() {
                        for (field, value) in fields {
                            record.value[field] = value.clone();
                        }
                    }
                    for field in arguments["deleteFields"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        if let Some(value) = record.value.as_object_mut() {
                            value.remove(field);
                        }
                    }
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.drop" => {
                if !self.states.iter().any(|(_, id)| *id == state_id) {
                    self.records.remove(&state_id);
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.snapshot" => Ok(json!(
                self.states
                    .iter()
                    .filter_map(|(_, id)| self.records.get(id).map(|record| record.value.clone()))
                    .collect::<Vec<_>>()
            )),
            "mcp.diagnosis.begin" => {
                let generation = self
                    .states
                    .iter()
                    .find(|(key, _)| key == server_id)
                    .and_then(|(_, id)| self.records.get(id))
                    .and_then(|record| record.value["generation"].as_u64())
                    .unwrap_or(0)
                    .saturating_add(1);
                let id = self.install(server_id,json!({"serverId":server_id,"transport":arguments["transport"].as_str().unwrap_or("stdio"),"state":"connecting","toolCount":0,"resourceCapabilities":{"resources":false,"templates":false},"generation":generation}));
                Ok(json!(id))
            }
            "mcp.diagnosis.revision" => Ok(json!(
                self.records
                    .get(&state_id)
                    .map(|record| record.revision)
                    .unwrap_or(0)
            )),
            "mcp.diagnosis.owns" => Ok(json!(self.owns(arguments))),
            "mcp.diagnosis.failure" => {
                if !self.owns(arguments) {
                    return Ok(Value::Null);
                }
                if let Some(record) = self.records.get_mut(&state_id) {
                    record.revision = record.revision.saturating_add(1);
                    record.value["code"] = arguments["code"].clone();
                    if arguments["code"] == "MCP_AUTH_REQUIRED" {
                        record.value["state"] = json!("auth-required");
                    }
                }
                self.error(server_id, arguments["code"].clone());
                Ok(Value::Null)
            }
            "mcp.diagnosis.recovery" => {
                if !self.owns(arguments)
                    || self.records.get(&state_id).map(|record| record.revision)
                        != arguments["revision"].as_u64()
                {
                    return Ok(Value::Null);
                }
                if let Some(record) = self.records.get_mut(&state_id) {
                    record.value["state"] = json!("ready");
                    record.value.as_object_mut().unwrap().remove("code");
                }
                self.errors.retain(|(key, _)| key != server_id);
                Ok(Value::Null)
            }
            "mcp.diagnosis.refresh" => {
                if let Some(record) = self.records.get_mut(&state_id) {
                    record.value["toolCount"] = arguments["toolCount"].clone();
                    record.value["generation"] = json!(
                        record.value["generation"]
                            .as_u64()
                            .unwrap_or(0)
                            .saturating_add(1)
                    );
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.resourceChanged" => {
                if let Some(record) = self.records.get_mut(&state_id) {
                    if record.value["state"] == "ready" {
                        record.value["generation"] = json!(
                            record.value["generation"]
                                .as_u64()
                                .unwrap_or(0)
                                .saturating_add(1)
                        );
                    }
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.connected" => {
                if let Some(record) = self.records.get_mut(&state_id) {
                    record.value["state"] = json!("ready");
                    record.value["toolCount"] = arguments["toolCount"].clone();
                    record.value["lastConnectedAt"] = arguments["connectedAt"].clone();
                    record.value["resourceCapabilities"] = json!({"resources":arguments["resources"]==true,"templates":arguments["resources"]==true});
                    record.value.as_object_mut().unwrap().remove("code");
                }
                if self
                    .states
                    .iter()
                    .any(|(key, id)| key == server_id && *id == state_id)
                {
                    self.errors.retain(|(key, _)| key != server_id);
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.disconnected" => {
                if let Some(record) = self.records.get_mut(&state_id) {
                    record.value["state"] = json!("disconnected");
                    record.value["toolCount"] = json!(0);
                    if arguments["unexpected"] == true {
                        record.value["code"] = json!("MCP_CONNECTION_LOST");
                    } else {
                        record.value.as_object_mut().unwrap().remove("code");
                    }
                }
                if arguments["unexpected"] == true {
                    self.error(server_id, json!("MCP_CONNECTION_LOST"));
                } else {
                    self.errors.retain(|(key, _)| key != server_id);
                }
                Ok(Value::Null)
            }
            "mcp.diagnosis.startupFailure" | "mcp.diagnosis.cleanupFailure" => {
                let cleanup = operation == "mcp.diagnosis.cleanupFailure";
                let code = if cleanup {
                    json!("MCP_PROCESS_CLEANUP_FAILED")
                } else {
                    arguments["code"].clone()
                };
                if let Some(record) = self.records.get_mut(&state_id) {
                    record.value["state"] = json!(if code == "MCP_AUTH_REQUIRED" {
                        "auth-required"
                    } else {
                        "error"
                    });
                    record.value["code"] = code.clone();
                    if cleanup {
                        record.value["toolCount"] = json!(0);
                    }
                }
                self.error(
                    server_id,
                    if code.is_null() {
                        json!("MCP_CONNECTION_FAILED")
                    } else {
                        code
                    },
                );
                Ok(Value::Null)
            }
            "mcp.diagnosis.lostFailure" => {
                if self
                    .states
                    .iter()
                    .any(|(key, id)| key == server_id && *id == state_id)
                {
                    if let Some(record) = self.records.get_mut(&state_id) {
                        if record.value["state"] == "disconnected" {
                            record.value["state"] = json!("error");
                            record.value["code"] = arguments["code"].clone();
                            self.error(server_id, arguments["code"].clone());
                        }
                    }
                }
                Ok(Value::Null)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown MCP diagnosis operation.",
                400,
            )),
        }
    }
}
