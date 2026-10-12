use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json, value::RawValue};
use sha2::{Digest, Sha256};

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}
fn ordered_version(value: &Value, order: &Value) -> String {
    let scopes = array(&value["scopes"]);
    let mut indexes: Vec<usize> = array(order)
        .iter()
        .filter_map(|index| index.as_u64().map(|index| index as usize))
        .filter(|index| *index < scopes.len())
        .collect();
    if indexes.len() != scopes.len() {
        indexes = (0..scopes.len()).collect();
        indexes.sort_by_key(|index| string(&scopes[*index]["scopeKey"]));
    }
    let scopes: Vec<Value> = indexes
        .into_iter()
        .map(|index| {
            json!([
                scopes[index]["scopeKey"],
                scopes[index]
                    .get("corpusGeneration")
                    .filter(|value| !value.is_null())
                    .unwrap_or(&scopes[index]["generation"])
            ])
        })
        .collect();
    if let Some(epoch) = value.get("indexEpoch") {
        format!("{{\"indexEpoch\":{epoch},\"scopes\":{}}}", json!(scopes))
    } else {
        format!("{{\"scopes\":{}}}", json!(scopes))
    }
}
fn raw_object(value: &Value) -> Result<HashMap<String, Box<RawValue>>, String> {
    serde_json::from_str(string(value)).map_err(|_| "INVALID_SOURCE_SIGNATURE".into())
}
fn raw_field(object: &HashMap<String, Box<RawValue>>, name: &str) -> String {
    object
        .get(name)
        .map(|value| value.get().into())
        .unwrap_or("null".into())
}
fn project_id(snapshot: &Value) -> Value {
    if snapshot["relationship"]["isFolderlessWorkspace"] == true {
        Value::Null
    } else {
        snapshot["relationship"]
            .get("projectId")
            .filter(|value| !value.is_null())
            .or_else(|| snapshot["settings"].get("projectId"))
            .cloned()
            .unwrap_or(Value::Null)
    }
}
pub fn scan_summary(args: &Value) -> Value {
    let registration = &args["registered"]["scan"]["coverage"];
    let mounted = &args["mounted"];
    let discovery = &mounted["scan"]["coverage"];
    if registration.is_null() {
        return mounted["scan"].clone();
    }
    let mut result = mounted["scan"].as_object().cloned().unwrap_or_default();
    result.insert("registeredCoverage".into(), registration.clone());
    result.insert(
        "incomplete".into(),
        json!(truthy(&mounted["scan"]["incomplete"]) || registration["complete"] == false),
    );
    let mut coverage = discovery.as_object().cloned().unwrap_or_default();
    for (field, registered_default, mounted_default) in [
        (
            "discovered",
            args["registered"]["sourceCount"].as_u64().unwrap_or(0),
            mounted["sourceCount"].as_u64().unwrap_or(0),
        ),
        ("failed", 0, 0),
        ("skipped", 0, 0),
    ] {
        coverage.insert(
            field.into(),
            json!(
                registration[field].as_u64().unwrap_or(registered_default)
                    + discovery[field].as_u64().unwrap_or(mounted_default)
            ),
        );
    }
    coverage.insert(
        "complete".into(),
        json!(
            registration["complete"] != false
                && discovery["complete"] != false
                && !truthy(&mounted["scan"]["backgroundPending"])
        ),
    );
    let mut limits = array(&registration["limits"]).to_vec();
    if truthy(&discovery["limit"]) {
        limits.push(discovery["limit"].clone());
    }
    limits.extend(array(&discovery["limits"]).iter().cloned());
    if !limits.is_empty() {
        coverage.insert("limits".into(), json!(limits));
    }
    result.insert("coverage".into(), json!(coverage));
    json!(result)
}

struct CorpusPlan {
    key: String,
    signature: String,
    can_cache: bool,
    complete: bool,
}

/// Durable stores remain authoritative; native state owns reuse proofs, corpus plans and mutation shutdown.
/// 持久存储仍是权威；原生状态拥有复用证明、语料计划及变更关闭账本，不以取消请求撤销已提交资料。
#[derive(Default)]
pub struct SourceManagementState {
    closed: bool,
    sequence: u64,
    operations: HashSet<u64>,
    restored: HashSet<String>,
    restored_order: VecDeque<String>,
    corpus: HashMap<String, Value>,
    corpus_order: VecDeque<String>,
    plans: HashMap<u64, CorpusPlan>,
    checkpoint_error: Value,
    pipeline: super::source_pipeline::SourcePipelineState,
    jobs: super::source_jobs::SourceJobState,
    sync: super::source_sync::SourceSyncState,
    coordination: super::source_coordination::SourceCoordinationState,
    preparation: super::source_preparation::SourcePreparationState,
}
impl SourceManagementState {
    fn cache(&mut self, key: String, value: Value) {
        self.corpus_order.retain(|candidate| candidate != &key);
        self.corpus_order.push_back(key.clone());
        self.corpus.insert(key, value);
        while self.corpus_order.len() > 32 {
            if let Some(key) = self.corpus_order.pop_front() {
                self.corpus.remove(&key);
            }
        }
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation.starts_with("orchestration.sourceManagement.pipeline.")
            || operation.starts_with("orchestration.sourceManagement.publication.")
        {
            return self.pipeline.apply(operation, args);
        }
        if operation.starts_with("orchestration.sourceManagement.jobs.") {
            return self.jobs.apply(operation, args);
        }
        if operation.starts_with("orchestration.sourceManagement.sync.") {
            return self.sync.apply(operation, args);
        }
        if operation.starts_with("orchestration.sourceManagement.preparation.") {
            return self.preparation.apply(operation, args);
        }
        if operation.starts_with("orchestration.sourceManagement.coordinator.") {
            return self.coordination.apply(operation, args);
        }
        match operation {
            "orchestration.sourceManagement.scanSummary" => Ok(scan_summary(args)),
            "orchestration.sourceManagement.corpusKey" => {
                let mut scopes = array(&args["scopes"]).to_vec();
                scopes.sort_by(|left, right| string(left).cmp(string(right)));
                Ok(json!(digest(&json!([scopes]).to_string())))
            }
            "orchestration.sourceManagement.signatureFields" => Ok(json!([
                "sourceId",
                "contentHash",
                "sourceRevision",
                "bindingRevision",
                "scopeKey",
                "sourceType",
                "title",
                "locator",
                "parserVersion",
                "chunkerVersion",
                "tokenizerVersion",
                "embeddingInputVersion",
                "structure"
            ])),
            "orchestration.sourceManagement.status" => Ok(
                json!({"closed": self.closed, "corpusCacheSize": self.corpus.len(), "checkpointError": self.checkpoint_error}),
            ),
            "orchestration.sourceManagement.clearCache" => {
                self.corpus.clear();
                self.corpus_order.clear();
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.checkpointError" => {
                if args["set"] == true {
                    self.checkpoint_error = args["value"].clone();
                }
                Ok(self.checkpoint_error.clone())
            }
            "orchestration.sourceManagement.prepare" => {
                if args["settings"]["local"]["enabled"] == false {
                    return Err("RETRIEVAL_DISABLED".into());
                }
                let project = &args["projectId"];
                Ok(
                    json!({"scopes": if truthy(project) { vec![json!("user"), json!(format!("project:{}", string(project)))] } else { vec![json!("user")] }}),
                )
            }
            "orchestration.sourceManagement.claimReuse" => {
                let checkpoint = string(&args["checkpointId"]);
                if checkpoint.is_empty() || !self.restored.insert(checkpoint.into()) {
                    return Ok(json!(false));
                }
                self.restored_order.push_back(checkpoint.into());
                while self.restored_order.len() > 32 {
                    if let Some(checkpoint) = self.restored_order.pop_front() {
                        self.restored.remove(&checkpoint);
                    }
                }
                Ok(json!(true))
            }
            "orchestration.sourceManagement.reuseCandidate" => {
                let mut candidates: Vec<usize> = array(&args["jobs"])
                    .iter()
                    .enumerate()
                    .filter(|(_, job)| {
                        ["completed", "partial"].contains(&string(&job["status"]))
                            && job["projectId"] == args["projectId"]
                            && job["checkpoint"]["checkpointId"] == args["checkpointId"]
                    })
                    .map(|(index, _)| index)
                    .collect();
                let jobs = array(&args["jobs"]);
                candidates.sort_by(|left, right| {
                    string(&jobs[*right]["createdAt"]).cmp(string(&jobs[*left]["createdAt"]))
                });
                Ok(json!(candidates.first()))
            }
            "orchestration.sourceManagement.finalizePlan" => {
                let snapshot = &args["snapshot"];
                let scan = &snapshot["sourceScan"];
                let complete = scan["coverage"]["complete"] != false
                    && !truthy(&scan["incomplete"])
                    && !truthy(&scan["backgroundPending"]);
                let reconcile = !truthy(&scan["incomplete"])
                    && !truthy(&scan["backgroundPending"])
                    && !truthy(&scan["failureReportTruncated"])
                    && !array(&scan["failures"])
                        .iter()
                        .any(|failure| truthy(&failure["directory"]));
                let mut identities: Vec<Value> = array(&snapshot["sources"])
                    .iter()
                    .map(|source| source["sourceId"].clone())
                    .collect();
                for failure in array(&scan["failures"]) {
                    if truthy(&failure["sourceId"]) && !identities.contains(&failure["sourceId"]) {
                        identities.push(failure["sourceId"].clone());
                    }
                }
                Ok(
                    json!({"prune": complete, "reconcile": reconcile, "sourceIds": identities,
                    "cache": snapshot["preparationFailures"] == args["preparationFailures"], "complete": complete}),
                )
            }
            "orchestration.sourceManagement.checkpoint" => {
                let settings = raw_object(&args["settingsJson"])?;
                let local = raw_object(&json!(raw_field(&settings, "local")))?;
                let project = settings
                    .get("projectIndexing")
                    .and_then(|value| {
                        serde_json::from_str::<HashMap<String, Box<RawValue>>>(value.get()).ok()
                    })
                    .unwrap_or_default();
                let binding = project
                    .get("bindingRevision")
                    .filter(|value| value.get() != "null")
                    .map(|value| value.get())
                    .unwrap_or("0");
                let mounted = project
                    .get("mountedFolder")
                    .filter(|value| value.get() != "null")
                    .map(|value| value.get())
                    .unwrap_or("false");
                let signature = digest(&format!(
                    "[{},{},{},{},{},{mounted},{binding}]",
                    raw_field(&local, "enabled"),
                    raw_field(&local, "semantic"),
                    raw_field(&local, "embeddingProfileId"),
                    raw_field(&local, "vectorBackend"),
                    raw_field(&local, "indexing")
                ));
                let root = if mounted != "false"
                    && mounted != "null"
                    && mounted != "0"
                    && mounted != "\"\""
                {
                    args["resolvedRoot"].clone()
                } else {
                    Value::Null
                };
                let preparation = string(&args["preparationVersionJson"]);
                let preparation = if preparation.is_empty() {
                    "null"
                } else {
                    preparation
                };
                let checkpoint_id = digest(&format!(
                    "[{},{},{root},{binding},{preparation}]",
                    args["projectId"],
                    quote(&signature)
                ));
                Ok(
                    json!({"version": 1, "kind": "source-index", "checkpointId": checkpoint_id, "settingsSignature": signature,
                    "root": root, "bindingRevision": serde_json::from_str::<Value>(binding).unwrap_or(Value::Null),
                    "preparationVersion": serde_json::from_str::<Value>(preparation).unwrap_or(Value::Null), "updatedAt": args["updatedAt"]}),
                )
            }
            "orchestration.sourceManagement.signature" => {
                const FIELDS: &[&str] = &[
                    "sourceId",
                    "contentHash",
                    "sourceRevision",
                    "bindingRevision",
                    "scopeKey",
                    "sourceType",
                    "title",
                    "locator",
                    "parserVersion",
                    "chunkerVersion",
                    "tokenizerVersion",
                    "embeddingInputVersion",
                    "structure",
                ];
                let sources = array(&args["sourcesJson"]);
                let rows: Result<Vec<String>, String> = array(&args["order"])
                    .iter()
                    .map(|index| {
                        let source = sources
                            .get(index.as_u64().unwrap_or(u64::MAX) as usize)
                            .ok_or("INVALID_SOURCE_SIGNATURE")?;
                        let object = raw_object(source)?;
                        Ok(format!(
                            "[{}]",
                            FIELDS
                                .iter()
                                .map(|field| raw_field(&object, field))
                                .collect::<Vec<_>>()
                                .join(",")
                        ))
                    })
                    .collect();
                Ok(json!(digest(&format!(
                    "[{},{},{},[{}]]",
                    string(&args["preparationVersionJson"]),
                    string(&args["localJson"]),
                    args["projectIndexingJson"].as_str().unwrap_or("null"),
                    rows?.join(",")
                ))))
            }
            "orchestration.sourceManagement.version" => {
                Ok(json!(ordered_version(&args["value"], &args["order"])))
            }
            "orchestration.sourceManagement.syncPlan" => {
                let snapshot = &args["snapshot"];
                let sources = array(&snapshot["sources"]);
                let scopes: Vec<Value> = array(&snapshot["scopes"])
                    .iter()
                    .filter(|scope| *scope == "user" || string(scope).starts_with("project:"))
                    .cloned()
                    .collect();
                let corpus: Vec<usize> = sources
                    .iter()
                    .enumerate()
                    .filter(|(_, source)| {
                        ["knowledge", "work-file"].contains(&string(&source["sourceType"]))
                    })
                    .map(|(index, _)| index)
                    .collect();
                let other: Vec<usize> = sources
                    .iter()
                    .enumerate()
                    .filter(|(_, source)| {
                        !["knowledge", "work-file"].contains(&string(&source["sourceType"]))
                    })
                    .map(|(index, _)| index)
                    .collect();
                if args["hasScopeVersion"] != true {
                    return Ok(
                        json!({"mode": "legacy", "corpusScopes": scopes, "corpusIndexes": corpus, "otherIndexes": other}),
                    );
                }
                if args["planned"] != true {
                    return Ok(
                        json!({"corpusScopes": scopes, "corpusIndexes": corpus, "otherIndexes": other}),
                    );
                }
                let key = string(&args["key"]);
                let signature = string(&args["signature"]);
                let version = string(&args["version"]);
                let previous = self.corpus.get(key);
                let cached = previous.is_some_and(|previous| {
                    previous["signature"] == signature && previous["version"] == version
                });
                let mode = if cached {
                    "cached"
                } else if truthy(&snapshot["sourceScan"]["backgroundPending"])
                    || snapshot["sourceScan"]["coverage"]["complete"] == false
                    || corpus.len() > 512
                {
                    "job"
                } else {
                    "inline"
                };
                self.sequence += 1;
                let id = self.sequence;
                let complete = previous.is_none_or(|previous| previous["complete"] != false)
                    && snapshot["sourceScan"]["coverage"]["complete"] != false;
                self.plans.insert(
                    id,
                    CorpusPlan {
                        key: key.into(),
                        signature: signature.into(),
                        can_cache: cached,
                        complete,
                    },
                );
                Ok(
                    json!({"planId": id, "mode": mode, "corpusScopes": scopes, "corpusIndexes": corpus, "otherIndexes": other, "projectId": project_id(snapshot),
                    "invalidate": !cached && mode == "inline" && previous.is_some_and(|previous| previous["version"] != version),
                    "patch": if cached && previous.is_some_and(|previous| previous["complete"] == false) { json!({"indexingPartial": true}) } else { json!({}) }}),
                )
            }
            "orchestration.sourceManagement.syncOutcome" => {
                let plan = self
                    .plans
                    .get_mut(&args["planId"].as_u64().unwrap_or(0))
                    .ok_or("SOURCE_SYNC_PLAN_NOT_FOUND")?;
                if args["inline"] == true {
                    plan.can_cache = !truthy(&args["report"]["semantic"]["skippedSources"])
                        && args["preparationBefore"] == args["preparationAfter"];
                }
                Ok(if args["job"].is_null() {
                    json!({})
                } else if truthy(&args["job"]["automaticRebuildBlocked"]) {
                    json!({"indexingPaused": true, "indexingPartial": true, "indexingDiagnostic": "INDEX_CANCELLED"})
                } else {
                    json!({"indexingPending": true})
                })
            }
            "orchestration.sourceManagement.cacheReady" => Ok(json!(
                self.plans
                    .get(&args["planId"].as_u64().unwrap_or(0))
                    .is_some_and(|plan| plan.can_cache)
            )),
            "orchestration.sourceManagement.finishSync" => {
                let plan = self
                    .plans
                    .remove(&args["planId"].as_u64().unwrap_or(0))
                    .ok_or("SOURCE_SYNC_PLAN_NOT_FOUND")?;
                self.corpus.remove(&plan.key);
                self.corpus_order.retain(|key| key != &plan.key);
                if plan.can_cache {
                    self.cache(plan.key, json!({"signature": plan.signature, "version": args["version"], "complete": plan.complete}));
                }
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.abandonSync" => {
                self.plans.remove(&args["planId"].as_u64().unwrap_or(0));
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.cache" => {
                self.cache(string(&args["key"]).into(), json!({"signature": args["signature"], "version": args["version"], "complete": args["complete"]}));
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.mutationBegin" => {
                if self.closed {
                    return Err("RETRIEVAL_CLOSED".into());
                }
                self.sequence += 1;
                self.operations.insert(self.sequence);
                Ok(json!(self.sequence))
            }
            "orchestration.sourceManagement.mutationSettled" => {
                self.operations.remove(&args["id"].as_u64().unwrap_or(0));
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.followup" => {
                Ok(json!(!self.closed && args["aborted"] != true))
            }
            "orchestration.sourceManagement.ignorableFollowupError" => Ok(json!(
                args["code"] == "RETRIEVAL_CLOSED" && self.closed
                    || args["name"] == "AbortError" && args["aborted"] == true
            )),
            "orchestration.sourceManagement.importResult" => {
                let sources = array(&args["result"]["sources"]);
                let first = sources.first().ok_or("SOURCE_IMPORT_EMPTY")?;
                let scan = &args["scan"];
                let mut result = json!({"id": first["id"], "title": first["title"], "scope": first["scope"], "projectId": first["projectId"], "path": first["originalPath"],
                    "revision": first["revision"], "status": "ready", "importedCount": sources.len(), "sourceCoverage": {"discovered": scan["discoveredFiles"].as_u64().unwrap_or(sources.len() as u64),
                    "failed": scan["failedFiles"].as_u64().unwrap_or(0), "importedWindows": sources.len(), "failures": scan.get("failures").cloned().unwrap_or(json!([])),
                    "complete": !truthy(&scan["incomplete"]) && !truthy(&scan["failedFiles"])}});
                if truthy(&scan["documentWindows"]) {
                    result["sourceCoverage"]["documentWindows"] = scan["documentWindows"].clone();
                }
                if !args["job"].is_null() {
                    result["jobId"] = args["job"]["jobId"].clone();
                } else {
                    result["indexingStatus"] = json!("pending");
                }
                Ok(result)
            }
            "orchestration.sourceManagement.close" => {
                self.closed = true;
                self.corpus.clear();
                self.corpus_order.clear();
                Ok(json!(self.operations.iter().copied().collect::<Vec<_>>()))
            }
            "orchestration.sourceManagement.closeOutcome" => {
                if args["releaseStatus"] == "rejected" {
                    return Ok(json!({"kind": "release"}));
                }
                if args["drainStatus"] == "rejected" {
                    return Ok(json!({"kind": "drain"}));
                }
                Ok(array(&args["work"])
                    .iter()
                    .enumerate()
                    .find(|(_, result)| {
                        result["status"] == "rejected" && result["name"] != "AbortError"
                    })
                    .map(|(index, _)| json!({"kind": "work", "index": index}))
                    .unwrap_or(Value::Null))
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
