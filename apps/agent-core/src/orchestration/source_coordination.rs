use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        _ => true,
    }
}
fn error_code(args: &Value, fallback: &str) -> Value {
    args.get("code")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or(json!(fallback))
}
fn safe_error(args: &Value) -> Value {
    let code = string(&args["code"]);
    let bytes = code.as_bytes();
    json!(if !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        code
    } else {
        "MEMORY_INDEX_FAILED"
    })
}

/// Catalog preparation and memory publication own native ledgers; original JSON wires preserve SDK identities.
/// 目录准备与记忆发布由原生账本拥有；原始 JSON 字节保留 SDK 身份，I/O 关闭不冒充发布完成。
pub struct SourceCoordinationState {
    fields: Value,
    maps: HashMap<String, Vec<(String, String)>>,
    catalog_active: bool,
    memory_active: bool,
    fresh_sequence: u64,
    freshness: HashMap<u64, (HashSet<String>, Value)>,
}
impl Default for SourceCoordinationState {
    fn default() -> Self {
        Self {
            fields: json!({"closed": false, "pendingProjectCatalog": null, "mountedProjectRefreshRequested": false,
        "mountedProjectCatalogRevision": -1, "mountedProjectPreparationDiagnostic": null,
        "memoryIndexState": {"publishedUpdates": 0, "partialUpdates": 0, "skippedUpdates": 0, "deferred": 0, "errorCode": null}}),
            maps: HashMap::new(),
            catalog_active: false,
            memory_active: false,
            fresh_sequence: 0,
            freshness: HashMap::new(),
        }
    }
}
impl SourceCoordinationState {
    fn closed(&self) -> bool {
        self.fields["closed"] == true
    }
    fn increment(&mut self, field: &str) {
        let count = self.fields["memoryIndexState"][field].as_u64().unwrap_or(0);
        self.fields["memoryIndexState"][field] = json!(count + 1);
    }
    fn map(&self, domain: &str) -> &[(String, String)] {
        self.maps.get(domain).map(Vec::as_slice).unwrap_or(&[])
    }
    fn put(&mut self, domain: &str, key: String, wire: String) {
        let rows = self.maps.entry(domain.into()).or_default();
        if let Some(row) = rows.iter_mut().find(|row| row.0 == key) {
            row.1 = wire;
        } else {
            rows.push((key, wire));
        }
    }
    fn remove(&mut self, domain: &str, key: &str) -> bool {
        let rows = self.maps.entry(domain.into()).or_default();
        let length = rows.len();
        rows.retain(|row| row.0 != key);
        rows.len() != length
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let operation = operation
            .strip_prefix("orchestration.sourceManagement.coordinator.")
            .ok_or("UNKNOWN_SOURCE_COORDINATION_OPERATION")?;
        match operation {
            "freshOpen" => {
                self.fresh_sequence += 1;
                self.freshness
                    .insert(self.fresh_sequence, (HashSet::new(), args["pool"].clone()));
                Ok(json!(self.fresh_sequence))
            }
            "freshObserved" => {
                let id = args["id"].as_u64().unwrap_or(0);
                let (stale, pool) = self
                    .freshness
                    .get_mut(&id)
                    .ok_or("UNKNOWN_SOURCE_FRESHNESS")?;
                if let Some(index) = array(&args["checked"])
                    .iter()
                    .position(|row| row["status"] == "rejected")
                {
                    return Ok(json!({"failed": index}));
                }
                let selected = array(&args["selected"]);
                let mut changed = false;
                for (index, row) in array(&args["checked"]).iter().enumerate() {
                    if !truthy(&row["value"]) {
                        if let Some(source) = selected.get(index) {
                            stale.insert(string(&source["sourceId"]).into());
                            changed = true;
                        }
                    }
                }
                Ok(
                    json!({"done": !changed, "count": stale.len(), "pool": array(pool).iter().enumerate().filter(|(_, source)| !stale.contains(string(&source["sourceId"]))).map(|(index, _)| index).collect::<Vec<_>>()}),
                )
            }
            "freshReplacement" => {
                let id = args["id"].as_u64().unwrap_or(0);
                let (stale, _) = self.freshness.get(&id).ok_or("UNKNOWN_SOURCE_FRESHNESS")?;
                let current = array(&args["current"]);
                let references = current
                    .iter()
                    .map(|source| string(&source["sourceRef"]))
                    .collect::<HashSet<_>>();
                Ok(
                    json!({"retained": current.iter().enumerate().filter(|(_, source)| !stale.contains(string(&source["sourceId"]))).map(|(index, _)| index).collect::<Vec<_>>(),
                    "additional": array(&args["available"]).iter().enumerate().filter(|(_, source)| !references.contains(string(&source["sourceRef"]))).map(|(index, _)| index).collect::<Vec<_>>()}),
                )
            }
            "freshClose" => {
                self.freshness.remove(&args["id"].as_u64().unwrap_or(0));
                Ok(Value::Null)
            }
            "field" => {
                let field = string(&args["field"]);
                if args["set"] == true {
                    self.fields[field] = args["value"].clone();
                }
                Ok(self.fields[field].clone())
            }
            "memoryState" => {
                let field = string(&args["field"]);
                if args["set"] == true {
                    self.fields["memoryIndexState"][field] = args["value"].clone();
                }
                Ok(if field.is_empty() {
                    self.fields["memoryIndexState"].clone()
                } else {
                    self.fields["memoryIndexState"][field].clone()
                })
            }
            "map" => {
                let domain = string(&args["domain"]);
                let key = string(&args["key"]);
                match string(&args["action"]) {
                    "get" => Ok(self
                        .map(domain)
                        .iter()
                        .find(|row| row.0 == key)
                        .map(|row| json!(row.1))
                        .unwrap_or(Value::Null)),
                    "has" => Ok(json!(self.map(domain).iter().any(|row| row.0 == key))),
                    "size" => Ok(json!(self.map(domain).len())),
                    "set" => {
                        self.put(domain, key.into(), string(&args["wire"]).into());
                        Ok(Value::Null)
                    }
                    "delete" => Ok(json!(self.remove(domain, key))),
                    "clear" => {
                        self.maps.remove(domain);
                        Ok(Value::Null)
                    }
                    "page" => Ok(json!(
                        self.map(domain)
                            .iter()
                            .skip(args["cursor"].as_u64().unwrap_or(0) as usize)
                            .take(64)
                            .map(|row| json!({"key": row.0, "wire": row.1}))
                            .collect::<Vec<_>>()
                    )),
                    _ => Err("UNKNOWN_SOURCE_COORDINATION_MAP_OPERATION".into()),
                }
            }
            "catalogSchedule" => {
                if self.closed() {
                    return Ok(json!({"closed": true}));
                }
                if args["restart"] != true {
                    if args["hasCatalog"] != true {
                        self.fields["mountedProjectRefreshRequested"] = json!(true);
                    } else {
                        let pending = self.fields["pendingProjectCatalog"]["revision"]
                            .as_i64()
                            .unwrap_or(
                                self.fields["mountedProjectCatalogRevision"]
                                    .as_i64()
                                    .unwrap_or(-1),
                            );
                        if args["revision"].as_i64().unwrap_or(-1) >= pending {
                            self.fields["pendingProjectCatalog"] =
                                json!({"wire": args["wire"], "revision": args["revision"]});
                        }
                    }
                }
                if self.catalog_active {
                    return Ok(json!({"start": false}));
                }
                self.catalog_active = true;
                Ok(json!({"start": true}))
            }
            "catalogClaim" => {
                if self.closed() {
                    return Ok(Value::Null);
                }
                let pending = self.fields["pendingProjectCatalog"].take();
                let refresh = self.fields["mountedProjectRefreshRequested"] == true;
                self.fields["mountedProjectRefreshRequested"] = json!(false);
                if pending.is_null() && !refresh {
                    Ok(Value::Null)
                } else {
                    Ok(json!({"wire": pending["wire"], "read": pending.is_null() && refresh}))
                }
            }
            "catalogPlan" => {
                let catalog = &args["catalog"];
                let revision = catalog["Revision"].as_i64().unwrap_or(-1);
                if self.closed()
                    || revision
                        < self.fields["mountedProjectCatalogRevision"]
                            .as_i64()
                            .unwrap_or(-1)
                {
                    return Ok(json!({"removed": [], "projects": []}));
                }
                self.fields["mountedProjectCatalogRevision"] = json!(revision);
                let projects = array(&catalog["Projects"]);
                let retained: Vec<String> = projects
                    .iter()
                    .map(|project| string(&project["Id"]).to_lowercase())
                    .collect();
                let removed: Vec<Value> = self
                    .map("mountedProjectBindings")
                    .iter()
                    .filter_map(|row| serde_json::from_str::<Value>(&row.0).ok())
                    .filter(|id| !retained.iter().any(|retained| retained == string(id)))
                    .collect();
                for id in &removed {
                    self.remove("mountedProjectBindings", &id.to_string());
                }
                Ok(
                    json!({"removed": removed, "projects": projects.iter().enumerate().map(|(index, project)| json!({"index": index, "projectId": retained[index],
                    "forget": !truthy(&project["FolderPath"]) || truthy(&project["IsArchived"]) || truthy(&project["IsFolderlessWorkspace"])})).collect::<Vec<_>>() }),
                )
            }
            "bindingPlan" => {
                let id = &args["projectId"];
                let previous = self
                    .map("mountedProjectBindings")
                    .iter()
                    .find(|row| row.0 == id.to_string())
                    .and_then(|row| serde_json::from_str::<String>(&row.1).ok());
                let same = previous.as_deref() == args["binding"].as_str();
                let enabled = args["local"]["enabled"] != false;
                Ok(
                    json!({"mode": if same { "unchanged" } else { "replace" }, "enabled": enabled, "forget": !same && previous.is_some(),
                    "result": if enabled { Value::Null } else { json!({"state": "disabled", "reason": "local-retrieval-disabled"}) }}),
                )
            }
            "bindingCommitted" => {
                self.put(
                    "mountedProjectBindings",
                    args["projectId"].to_string(),
                    args["binding"].to_string(),
                );
                if self.fields["mountedProjectPreparationDiagnostic"]["projectId"]
                    == args["projectId"]
                {
                    self.fields["mountedProjectPreparationDiagnostic"] = Value::Null;
                }
                Ok(Value::Null)
            }
            "bindingResult" => Ok(if truthy(&args["job"]["automaticRebuildBlocked"]) {
                json!({"state": "blocked", "reason": args["job"]["automaticRebuildState"], "jobId": args["job"]["jobId"]})
            } else {
                json!({"state": "scheduled", "jobId": args["job"]["jobId"]})
            }),
            "catalogFailure" => {
                let code = error_code(args, "WORK_PREPARATION_FAILED");
                if !self.closed() {
                    self.fields["mountedProjectPreparationDiagnostic"] =
                        if args.get("projectId").is_some() {
                            json!({"code": code, "projectId": args["projectId"]})
                        } else {
                            json!({"code": code})
                        };
                }
                Ok(json!({"state": "failed", "reason": code}))
            }
            "catalogSettled" => {
                self.catalog_active = false;
                Ok(json!(
                    !self.closed()
                        && (!self.fields["pendingProjectCatalog"].is_null()
                            || self.fields["mountedProjectRefreshRequested"] == true)
                ))
            }
            "memoryChangePlan" => {
                let event = &args["event"];
                if self.closed()
                    || event["previousStatus"] != "confirmed" && event["status"] != "confirmed"
                {
                    return Ok(Value::Null);
                }
                let scope = if event["scope"] == "user" {
                    "user".into()
                } else {
                    format!(
                        "{}:{}",
                        string(&event["scope"]),
                        event["scopeId"]
                            .as_str()
                            .map(str::to_lowercase)
                            .unwrap_or("undefined".into())
                    )
                };
                if event["scopeKey"] != scope {
                    return Err("INVALID_MEMORY_CHANGE".into());
                }
                Ok(json!(scope))
            }
            "memoryIdentity" => {
                if args["event"]["sourceId"] != args["identity"] {
                    return Err("INVALID_MEMORY_CHANGE".into());
                }
                Ok(Value::Null)
            }
            "memoryEnqueue" => {
                let event = &args["event"];
                let key = event["sourceId"].to_string();
                if self.closed() || event["status"] != "confirmed" {
                    self.remove("memoryIndexPending", &key);
                    return Ok(Value::Null);
                }
                self.put("memoryIndexPending", key, string(&args["wire"]).into());
                while self.map("memoryIndexPending").len() > 256 {
                    self.maps.get_mut("memoryIndexPending").unwrap().remove(0);
                    self.increment("deferred");
                }
                Ok(Value::Null)
            }
            "memoryStart" => {
                if self.closed() || self.memory_active || self.map("memoryIndexPending").is_empty()
                {
                    return Ok(json!(false));
                }
                self.memory_active = true;
                Ok(json!(true))
            }
            "memorySettled" => {
                self.memory_active = false;
                Ok(Value::Null)
            }
            "memoryClaim" => {
                if self.closed() || self.map("memoryIndexPending").is_empty() {
                    return Ok(Value::Null);
                }
                Ok(json!(
                    self.maps.get_mut("memoryIndexPending").unwrap().remove(0).1
                ))
            }
            "memoryRelationship" => Ok(json!(
                !(args["relationship"]["isArchived"] == true
                    || args["relationship"]["isFolderlessWorkspace"] == true
                        && args["scope"] == "project"
                    || args["relationship"]["projectArchived"] == true && args["scope"] == "chat")
            )),
            "memoryLive" => {
                let event = &args["event"];
                let index = array(&args["entries"])
                    .iter()
                    .position(|entry| entry["id"] == event["memoryId"]);
                Ok(index
                    .filter(|index| {
                        let entry = &args["entries"][*index];
                        entry["status"] == "confirmed"
                            && entry["active"] != false
                            && entry["revision"] == event["entryRevision"]
                    })
                    .map(|index| json!({"index": index}))
                    .unwrap_or(Value::Null))
            }
            "memoryCurrent" => Ok(json!(
                !args["live"].is_null()
                    && args["live"]["contentHash"] == args["current"]["contentHash"]
                    && truthy(&args["effective"]["enabled"])
                    && ["semantic", "embeddingProfileId", "embeddingDevicePolicy"]
                        .iter()
                        .all(|field| args["effective"][*field] == args["initial"][*field])
            )),
            "memoryEnabled" => Ok(json!(!self.closed() && truthy(&args["local"]["enabled"]))),
            "memoryLexical" => {
                let entry = array(&args["report"]["coverage"]["sources"])
                    .iter()
                    .find(|entry| entry["sourceId"] == args["sourceId"]);
                if entry.is_none_or(|entry| entry["lexical"] != "ready") {
                    self.increment("skippedUpdates");
                    self.fields["memoryIndexState"]["errorCode"] = entry
                        .and_then(|entry| entry.get("errorCode"))
                        .filter(|code| !code.is_null())
                        .cloned()
                        .or_else(|| {
                            array(&args["report"]["semantic"]["diagnosticCodes"])
                                .first()
                                .cloned()
                        })
                        .unwrap_or(json!("MEMORY_PUBLICATION_UNVERIFIED"));
                    return Ok(json!({"published": false}));
                }
                Ok(
                    json!({"published": true, "semantic": args["local"]["semantic"] != "off" && args["local"].get("embeddingProfileId") != Some(&Value::Null)}),
                )
            }
            "memoryCompleted" => {
                if args["live"] != true {
                    self.increment("skippedUpdates");
                    return Ok(Value::Null);
                }
                self.increment("publishedUpdates");
                let entry = array(&args["semantic"]["coverage"]["sources"])
                    .iter()
                    .find(|entry| entry["sourceId"] == args["sourceId"]);
                if !args["semantic"].is_null()
                    && entry.is_none_or(|entry| entry["semantic"] != "ready")
                {
                    self.increment("partialUpdates");
                    self.fields["memoryIndexState"]["errorCode"] = entry
                        .and_then(|entry| entry.get("errorCode"))
                        .filter(|code| !code.is_null())
                        .cloned()
                        .or_else(|| {
                            array(&args["semantic"]["semantic"]["diagnosticCodes"])
                                .first()
                                .cloned()
                        })
                        .unwrap_or(json!("MEMORY_SEMANTIC_PENDING"));
                } else {
                    self.fields["memoryIndexState"]["errorCode"] = Value::Null;
                }
                Ok(Value::Null)
            }
            "memoryFailure" => {
                if !self.closed() {
                    self.fields["memoryIndexState"]["errorCode"] = safe_error(args);
                }
                Ok(Value::Null)
            }
            "close" => {
                self.fields["closed"] = json!(true);
                self.maps.remove("memoryIndexPending");
                self.fields["pendingProjectCatalog"] = Value::Null;
                self.fields["mountedProjectRefreshRequested"] = json!(false);
                Ok(Value::Null)
            }
            _ => Err("UNKNOWN_SOURCE_COORDINATION_OPERATION".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(state: &mut SourceCoordinationState, operation: &str, args: Value) -> Value {
        state
            .apply(
                &format!("orchestration.sourceManagement.coordinator.{operation}"),
                &args,
            )
            .unwrap()
    }
    #[test]
    fn late_catalog_and_memory_arrivals_survive_claim_until_actual_settlement() {
        let mut state = SourceCoordinationState::default();
        call(&mut state, "catalogSchedule", json!({}));
        call(&mut state, "catalogClaim", json!({}));
        call(
            &mut state,
            "catalogSchedule",
            json!({"hasCatalog": true, "revision": 2, "wire": "{}"}),
        );
        assert_eq!(call(&mut state, "catalogSettled", json!({})), true);
        assert_eq!(call(&mut state, "catalogClaim", json!({}))["wire"], "{}");
        call(
            &mut state,
            "map",
            json!({"domain": "memoryIndexPending", "action": "set", "key": "id", "wire": "{}"}),
        );
        assert_eq!(call(&mut state, "memoryStart", json!({})), true);
        call(&mut state, "memoryClaim", json!({}));
        call(&mut state, "close", json!({}));
        call(&mut state, "memorySettled", json!({}));
        assert_eq!(call(&mut state, "memoryStart", json!({})), false);
        assert_eq!(
            call(&mut state, "memoryState", json!({}))["publishedUpdates"],
            0
        );
    }
}
