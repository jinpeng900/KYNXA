use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json};

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
fn key(value: &Value) -> String {
    value.to_string()
}
fn stopped(stop: Option<&Value>, scope: &Value) -> bool {
    stop.is_some_and(|stop| {
        !truthy(&stop["automaticRebuildScope"]) || stop["automaticRebuildScope"] == *scope
    })
}
fn summary(coverage: &Value) -> Value {
    let mut result = coverage.clone();
    if !result.is_object() {
        result = json!({});
    }
    for field in ["sources", "failures"] {
        let values = array(&coverage[field]);
        result[field] = json!(&values[values.len().saturating_sub(50)..]);
    }
    result["reportTruncated"] = json!(
        truthy(&coverage["reportTruncated"])
            || array(&coverage["sources"]).len() > 50
            || array(&coverage["failures"]).len() > 50
    );
    result
}
#[derive(Default)]
struct Priority {
    revision: u64,
    paths: VecDeque<String>,
    recent: VecDeque<String>,
}
impl Priority {
    fn value(&self) -> Value {
        json!({"revision": self.revision, "paths": self.paths, "recentPaths": self.recent})
    }
}
#[derive(Default)]
struct JobOwner {
    value: Value,
    planned: HashSet<String>,
    completed: HashSet<String>,
    prior_codes: Vec<String>,
    previous_codes: Vec<String>,
    stale_passes: u64,
    acknowledged: usize,
}

/// Queue admission, durable cancellation and per-pass publication decisions belong to one native owner.
/// 队列准入、持久取消和逐轮发布决策由统一原生拥有者维护；实际作业写入/回执及 AbortController 留给适配器。
#[derive(Default)]
pub struct SourceJobState {
    closed: bool,
    sequence: u64,
    active: HashMap<String, JobOwner>,
    active_order: Vec<String>,
    stops: HashMap<String, Value>,
    priorities: HashMap<u64, Priority>,
    priority_keys: HashMap<String, u64>,
    priority_order: VecDeque<String>,
    failure_seen: bool,
}
impl SourceJobState {
    fn priority(&mut self, project: &Value) -> u64 {
        if let Some(id) = self
            .active_order
            .iter()
            .filter_map(|id| self.active.get(id))
            .find(|job| job.value["projectId"] == *project && job.value["aborted"] != true)
            .and_then(|job| job.value["priorityId"].as_u64())
        {
            return id;
        }
        let project = key(project);
        if let Some(id) = self.priority_keys.get(&project) {
            return *id;
        }
        self.sequence += 1;
        let id = self.sequence;
        self.priorities.insert(id, Priority::default());
        self.priority_keys.insert(project.clone(), id);
        self.priority_order.push_back(project);
        while self.priority_order.len() > 32 {
            if let Some(project) = self.priority_order.pop_front() {
                self.priority_keys.remove(&project);
            }
        }
        id
    }
    fn semantic(&self, id: &str, semantic: &Value) -> Value {
        let mut semantic = semantic.clone();
        if !semantic.is_object() {
            semantic = json!({});
        }
        semantic["priorDiagnosticCodes"] = json!(self.active[id].prior_codes);
        semantic
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let id = string(&args["jobId"]);
        match operation {
            "orchestration.sourceManagement.jobs.status" => Ok(json!({"closed": self.closed})),
            "orchestration.sourceManagement.jobs.failure" => {
                let first = !self.failure_seen;
                self.failure_seen = true;
                Ok(json!(first))
            }
            "orchestration.sourceManagement.jobs.active" => Ok(json!(
                self.active_order
                    .iter()
                    .filter(|id| self.active.contains_key(*id))
                    .collect::<Vec<_>>()
            )),
            "orchestration.sourceManagement.jobs.field" => {
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                let property = string(&args["property"]);
                if args["set"] == true {
                    job.value[property] = args["value"].clone();
                }
                Ok(job.value[property].clone())
            }
            "orchestration.sourceManagement.jobs.facts" => {
                for fact in array(&args["facts"]) {
                    if let Some(job) = self.active.get_mut(string(&fact["jobId"])) {
                        job.value["aborted"] = fact["aborted"].clone();
                    }
                }
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.priorities" => {
                let priority_id = args["priorityId"]
                    .as_u64()
                    .unwrap_or_else(|| self.priority(&args["projectId"]));
                Ok(
                    json!({"priorityId": priority_id, "value": self.priorities.get(&priority_id).map(Priority::value)}),
                )
            }
            "orchestration.sourceManagement.jobs.prioritize" => {
                let path = string(&args["path"]);
                if self.closed || path.is_empty() {
                    return Ok(Value::Null);
                }
                let priority_id = self.priority(&args["projectId"]);
                let priority = self.priorities.get_mut(&priority_id).unwrap();
                let recent = args["recent"] == true;
                let paths = if recent {
                    &mut priority.recent
                } else {
                    &mut priority.paths
                };
                if recent || !paths.iter().any(|candidate| candidate == path) {
                    paths.retain(|candidate| candidate != path);
                    paths.push_back(path.into());
                    while paths.len() > 16 {
                        paths.pop_front();
                    }
                    priority.revision += 1;
                }
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.supersede" => {
                let mut actions = Vec::new();
                for (id, job) in &mut self.active {
                    if job.value["projectId"] == args["projectId"]
                        && job.value["cancelledByUser"] != true
                    {
                        job.value["superseded"] = json!(true);
                        job.value["suspend"] = json!(false);
                        job.value["acceptingRefresh"] = json!(false);
                        job.value["aborted"] = json!(true);
                        actions
                            .push(json!({"jobId": id, "queued": job.value["hasStarted"] != true}));
                    }
                }
                Ok(json!(actions))
            }
            "orchestration.sourceManagement.jobs.initialize" => {
                let history = array(&args["history"]);
                let mut order: Vec<usize> = (0..history.len()).collect();
                order.sort_by(|left, right| {
                    string(&history[*left]["createdAt"]).cmp(string(&history[*right]["createdAt"]))
                });
                let mut latest = HashMap::new();
                for index in order {
                    let job = &history[index];
                    latest.insert(key(&job["projectId"]), index);
                    if job["automaticRebuildBlocked"] == true {
                        self.stops.insert(key(&job["projectId"]), job.clone());
                    }
                }
                let mut patches = Vec::new();
                for index in latest.values() {
                    let job = &history[*index];
                    if job["status"] == "cancelled"
                        && job["error"] == "INDEX_CANCELLED"
                        && job.get("automaticRebuildBlocked").is_none()
                    {
                        self.stops.insert(key(&job["projectId"]), job.clone());
                        patches.push(json!({"jobId": job["jobId"], "patch": {"automaticRebuildBlocked": true}}));
                    }
                }
                Ok(json!(patches))
            }
            "orchestration.sourceManagement.jobs.recover" => {
                let job = &args["job"];
                Ok(
                    json!({"closed": self.closed, "blocked": stopped(self.stops.get(&key(&job["projectId"])), &job["automaticRebuildScope"]),
                    "patch": {"status": "cancelled", "error": "INDEX_CANCELLED", "automaticRebuildBlocked": true, "finishedAt": args["now"]}}),
                )
            }
            "orchestration.sourceManagement.jobs.stopAutomatic" => {
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                if job.value["cancelMarkerRequested"] == true {
                    return Ok(json!(false));
                }
                job.value["cancelMarkerRequested"] = json!(true);
                self.stops.insert(key(&job.value["projectId"]), json!({"jobId": id, "projectId": job.value["projectId"], "automaticRebuildScope": job.value["automaticRebuildScope"]}));
                Ok(json!(true))
            }
            "orchestration.sourceManagement.jobs.admit" => {
                let job = &args["job"];
                let id = string(&job["jobId"]);
                let priority_id = self.priority(&job["projectId"]);
                let value = json!({"jobId": id, "projectId": job["projectId"], "acceptingRefresh": true, "automaticRebuildScope": job["automaticRebuildScope"],
                    "needsRefresh": false, "recovered": args["recovered"], "completedSources": job["completedSources"], "totalSources": job["totalSources"],
                    "suspend": false, "cancelledByUser": false, "hasStarted": false, "priorityId": priority_id, "aborted": self.closed});
                self.active.insert(
                    id.into(),
                    JobOwner {
                        value,
                        ..JobOwner::default()
                    },
                );
                self.active_order.retain(|candidate| candidate != id);
                self.active_order.push(id.into());
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.queueStart" => {
                let Some(job) = self.active.get_mut(id) else {
                    return Ok(json!({"cancelled": true}));
                };
                let cancelled = job.value["queuedCancellationRequested"] == true;
                if !cancelled {
                    job.value["hasStarted"] = json!(true);
                }
                Ok(json!({"cancelled": cancelled}))
            }
            "orchestration.sourceManagement.jobs.retire" => {
                self.active.remove(id);
                self.active_order.retain(|candidate| candidate != id);
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.cancelQueued" => {
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                job.value["queuedCancellationRequested"] = json!(true);
                job.value["acceptingRefresh"] = json!(false);
                Ok(
                    json!({"status": "cancelled", "error": if job.value["superseded"] == true && job.value["cancelledByUser"] != true { "INDEX_SUPERSEDED" } else { "INDEX_CANCELLED" }, "finishedAt": args["now"]}),
                )
            }
            "orchestration.sourceManagement.jobs.rebuildPlan" => {
                if self.closed {
                    return Err("RETRIEVAL_CLOSED".into());
                }
                let project = &args["projectId"];
                let automatic = args["automatic"] == true;
                if automatic && stopped(self.stops.get(&key(project)), &args["scope"]) {
                    return Ok(
                        json!({"kind": "blocked", "jobId": self.stops[&key(project)]["jobId"]}),
                    );
                }
                // Select against immutable metadata before mutating the selected owner's refresh marker.
                // 先按不可变元信息选择，再修改该拥有者的刷新标记。
                let selected = self
                    .active_order
                    .iter()
                    .find(|id| {
                        self.active.get(*id).is_some_and(|job| {
                            job.value["projectId"] == *project
                                && job.value["acceptingRefresh"] == true
                                && job.value["aborted"] != true
                        })
                    })
                    .cloned();
                if let Some(id) = selected {
                    if args["dirty"] == true {
                        self.active.get_mut(&id).unwrap().value["needsRefresh"] = json!(true);
                    }
                    return Ok(json!({"kind": "current", "jobId": id}));
                }
                let waits: Vec<String> = if automatic {
                    vec![]
                } else {
                    self.active_order
                        .iter()
                        .filter(|id| {
                            self.active.get(*id).is_some_and(|job| {
                                job.value["projectId"] == *project
                                    && job.value["cancelledByUser"] == true
                                    && job.value["aborted"] == true
                            })
                        })
                        .cloned()
                        .collect()
                };
                let mut options = json!({"resumeAutomatic": !automatic});
                if let Some(scope) = args.get("scope") {
                    options["automaticRebuildScope"] = scope.clone();
                }
                Ok(json!({"kind": "create", "waitIds": waits, "options": options}))
            }
            "orchestration.sourceManagement.jobs.created" => {
                if args["automatic"] != true {
                    self.stops.remove(&key(&args["projectId"]));
                }
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.automaticState" => {
                let project = &args["projectId"];
                let scope = &args["scope"];
                if stopped(self.stops.get(&key(project)), scope) {
                    return Ok(
                        json!({"state": "blocked", "reason": "cancelled-by-user", "jobId": self.stops[&key(project)]["jobId"]}),
                    );
                }
                let active = self
                    .active_order
                    .iter()
                    .filter_map(|id| self.active.get(id))
                    .find(|job| {
                        job.value["projectId"] == *project
                            && job.value["aborted"] != true
                            && job.value["automaticRebuildScope"] == *scope
                    });
                Ok(active
                    .map(|job| json!({"state": "scheduled", "jobId": job.value["jobId"]}))
                    .unwrap_or(json!({"state": "unchanged"})))
            }
            "orchestration.sourceManagement.jobs.passStart" => {
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                job.value["needsRefresh"] = json!(false);
                job.acknowledged = 0;
                for code in &job.previous_codes {
                    if job.prior_codes.len() < 8 && !job.prior_codes.contains(code) {
                        job.prior_codes.push(code.clone());
                    }
                }
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.checkpointPlan" => {
                let job = &mut self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?.value;
                let recovered = &job["recovered"];
                if recovered.is_null() {
                    return Ok(Value::Null);
                }
                let saved = &recovered["checkpoint"];
                let current = &args["checkpoint"];
                if current.is_null()
                    || [
                        "checkpointId",
                        "settingsSignature",
                        "root",
                        "bindingRevision",
                        "preparationVersion",
                    ]
                    .iter()
                    .any(|field| saved[*field] != current[*field])
                {
                    return Err("INDEX_CHECKPOINT_STALE".into());
                }
                Ok(saved.clone())
            }
            "orchestration.sourceManagement.jobs.restored" => {
                self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?.value["recovered"] =
                    Value::Null;
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.planned" => {
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                for source in array(&args["sources"]) {
                    job.planned.insert(string(&source["sourceId"]).into());
                }
                for failure in array(&args["failures"]) {
                    if truthy(&failure["sourceId"]) && !truthy(&failure["directory"]) {
                        job.planned.insert(string(&failure["sourceId"]).into());
                    }
                }
                job.value["totalSources"] = json!(
                    job.value["totalSources"]
                        .as_u64()
                        .unwrap_or(0)
                        .max(job.planned.len() as u64)
                );
                Ok(json!({"totalSources": job.value["totalSources"]}))
            }
            "orchestration.sourceManagement.jobs.progress" => {
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                let progress = &args["progress"];
                let completed = args["completedSources"].as_u64().unwrap_or(0) as usize;
                if progress["processedSourceIds"].is_array() {
                    for source in array(&progress["processedSourceIds"]) {
                        job.completed.insert(string(source).into());
                    }
                } else {
                    for source in array(&args["sources"])
                        .iter()
                        .skip(job.acknowledged)
                        .take(completed.saturating_sub(job.acknowledged))
                    {
                        job.completed.insert(string(&source["sourceId"]).into());
                    }
                }
                job.acknowledged = completed;
                job.value["completedSources"] = json!(
                    job.value["completedSources"]
                        .as_u64()
                        .unwrap_or(0)
                        .max(job.completed.len() as u64)
                );
                let mut patch = json!({"completedSources": job.value["completedSources"]});
                if !progress["coverage"].is_null() {
                    patch["coverage"] = summary(&progress["coverage"]);
                }
                if !progress["semantic"].is_null() {
                    let mut semantic = progress["semantic"].clone();
                    semantic["priorDiagnosticCodes"] = json!(job.prior_codes);
                    patch["semantic"] = semantic;
                }
                Ok(
                    json!({"patch": patch, "commit": !args["checkpoint"].is_null() && !array(&progress["checkpointSources"]).is_empty()}),
                )
            }
            "orchestration.sourceManagement.jobs.committed" => {
                self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?.value["hasCheckpoint"] =
                    json!(true);
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.jobs.coverage" => {
                let report = &args["report"];
                let scan = &args["scan"];
                if report["coverage"].is_null() && scan["coverage"].is_null() {
                    return Ok(Value::Null);
                }
                let mut coverage = report.get("coverage").filter(|coverage| !coverage.is_null()).cloned().unwrap_or(json!({"discovered": args["sourceCount"], "lexical": 0, "semantic": 0, "failed": 0, "skipped": 0, "partial": 0, "complete": false, "sources": []}));
                for field in ["discovered", "failed", "skipped"] {
                    let fallback = if field == "failed" {
                        array(&scan["failures"]).len() as u64
                    } else {
                        0
                    };
                    coverage[field] = json!(
                        coverage[field]
                            .as_u64()
                            .unwrap_or(0)
                            .max(scan["coverage"][field].as_u64().unwrap_or(fallback))
                    );
                }
                coverage["partial"] = coverage.get("partial").cloned().unwrap_or(json!(0));
                coverage["failures"] =
                    json!(array(&scan["failures"]).iter().take(50).collect::<Vec<_>>());
                let sources = array(&coverage["sources"]);
                let truncated = sources.len() > 50
                    || array(&scan["failures"]).len() > 50
                    || truthy(&coverage["reportTruncated"]);
                coverage["sources"] = json!(&sources[sources.len().saturating_sub(50)..]);
                coverage["complete"] = json!(
                    truthy(&coverage["complete"])
                        && scan["coverage"]["complete"] != false
                        && array(&scan["failures"]).is_empty()
                        && !truthy(&scan["truncated"])
                        && !truthy(&scan["backgroundPending"])
                );
                for field in ["limit", "effectiveLimits"] {
                    if truthy(&scan["coverage"][field]) {
                        coverage[field] = scan["coverage"][field].clone();
                    }
                }
                if truthy(&scan["coverage"]["limits"]) {
                    coverage["limits"] = json!(
                        array(&scan["coverage"]["limits"])
                            .iter()
                            .take(50)
                            .collect::<Vec<_>>()
                    );
                }
                coverage["reportTruncated"] = json!(truncated);
                self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?.value["coverage"] =
                    coverage.clone();
                Ok(json!({"coverage": coverage}))
            }
            "orchestration.sourceManagement.jobs.semantic" => {
                let semantic = &args["semantic"];
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                job.previous_codes = array(&semantic["diagnosticCodes"])
                    .iter()
                    .map(|code| string(code).into())
                    .collect();
                let refresh = job
                    .previous_codes
                    .iter()
                    .any(|code| code == "STALE_RETRIEVAL_SOURCE");
                if refresh {
                    job.stale_passes += 1;
                    job.value["needsRefresh"] = json!(true);
                } else {
                    job.stale_passes = 0;
                }
                Ok(
                    json!({"patch": {"semantic": self.semantic(id, semantic)}, "refresh": refresh, "failure": if self.active[id].stale_passes > 2 { json!("INDEX_SOURCE_UNSTABLE") } else { Value::Null }}),
                )
            }
            "orchestration.sourceManagement.jobs.continue" => Ok(json!(
                self.active
                    .get(id)
                    .is_some_and(|job| job.value["needsRefresh"] == true)
                    && args["aborted"] != true
            )),
            "orchestration.sourceManagement.jobs.terminal" => {
                let job = self.active.get_mut(id).ok_or("INDEX_JOB_NOT_OWNED")?;
                job.value["acceptingRefresh"] = json!(false);
                let coverage = &job.value["coverage"];
                let partial = !coverage.is_null()
                    && (!truthy(&coverage["complete"])
                        || truthy(&coverage["failed"])
                        || truthy(&coverage["skipped"]));
                Ok(
                    json!({"status": if partial { "partial" } else { "completed" }, "finishedAt": args["now"]}),
                )
            }
            "orchestration.sourceManagement.jobs.failed" => {
                let job = &self.active.get(id).ok_or("INDEX_JOB_NOT_OWNED")?.value;
                let aborted = args["aborted"] == true;
                let superseded = job["superseded"] == true && job["cancelledByUser"] != true;
                let suspended = aborted
                    && job["cancelledByUser"] != true
                    && job["suspend"] == true
                    && (job["hasCheckpoint"] == true || !job["recovered"].is_null());
                Ok(
                    json!({"status": if suspended { "paused" } else if aborted { "cancelled" } else { "failed" },
                    "error": if superseded { "INDEX_SUPERSEDED" } else if suspended { "INDEX_SUSPENDED" } else if aborted { "INDEX_CANCELLED" } else { args["code"].as_str().unwrap_or("INDEX_FAILED") }, "finishedAt": args["now"]}),
                )
            }
            "orchestration.sourceManagement.jobs.cancel" => {
                if let Some(job) = self.active.get_mut(id) {
                    job.value["cancelledByUser"] = json!(true);
                    job.value["suspend"] = json!(false);
                    job.value["aborted"] = json!(true);
                    Ok(json!({"owned": true, "queued": job.value["hasStarted"] != true}))
                } else if args["job"]["status"] == "paused" {
                    let job = &args["job"];
                    self.stops.insert(key(&job["projectId"]), job.clone());
                    Ok(
                        json!({"owned": false, "patch": {"status": "cancelled", "error": "INDEX_CANCELLED", "automaticRebuildBlocked": true, "finishedAt": args["now"]}}),
                    )
                } else {
                    Ok(json!({"owned": false}))
                }
            }
            "orchestration.sourceManagement.jobs.close" => {
                self.closed = true;
                self.priority_keys.clear();
                self.priority_order.clear();
                for job in self.active.values_mut() {
                    job.value["suspend"] = json!(job.value["cancelledByUser"] != true);
                    job.value["aborted"] = json!(true);
                }
                Ok(json!(self.active_order))
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
