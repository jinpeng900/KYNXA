use super::{ContractError, ContractResult, hash, job_contracts};
use serde_json::{Value, json};
use std::collections::HashSet;

fn active(job: &Value) -> bool {
    matches!(
        job["status"].as_str(),
        Some("queued" | "running" | "paused")
    )
}
fn missing() -> ContractError {
    ContractError::new("索引任务不存在。", "RETRIEVAL_JOB_NOT_FOUND", 404)
}

/// Persisted job policy owns lifecycle, monotonic progress and history retention independently of worker effects.
/// 持久作业策略独立于 worker 执行，拥有生命周期、单调进度与历史保留规则。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let mut document = arguments["document"].clone();
    let timestamps = &arguments["timestamps"];
    if document["schemaVersion"] != 1 || !document["jobs"].is_array() {
        return Err(ContractError::new(
            "索引任务文件版本无效。",
            "UNSUPPORTED_RETRIEVAL_SCHEMA",
            409,
        ));
    }
    for job in document["jobs"].as_array().unwrap() {
        if let Some(coverage) = job.get("coverage") {
            job_contracts::coverage(coverage)?;
        }
        if job
            .get("automaticRetryAttempt")
            .is_some_and(|value| !super::safe_integer(value, 0, 10))
            || job
                .get("semanticRetry")
                .is_some_and(|value| !value.is_boolean())
            || job
                .get("automaticRebuildBlocked")
                .is_some_and(|value| !value.is_boolean())
            || job
                .get("automaticRebuildScope")
                .is_some_and(|value| !hash(value))
        {
            return Err(ContractError::new(
                "索引取消策略记录无效。",
                "INVALID_RETRIEVAL_JOB_UPDATE",
                400,
            ));
        }
    }
    let mut changed = true;
    let mut retired = Vec::new();
    let mut pending = Vec::new();
    let mut selected_id = arguments["jobId"].clone();
    match operation {
        "storage.jobs.read" => changed = false,
        "storage.jobs.get" => {
            return document["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["jobId"] == selected_id)
                .cloned()
                .ok_or_else(missing);
        }
        "storage.jobs.create" => {
            let job = &arguments["job"];
            if job
                .get("automaticRebuildScope")
                .is_some_and(|value| !hash(value))
                || !arguments["resumeAutomatic"].is_boolean()
            {
                return Err(ContractError::new(
                    "索引自动恢复范围无效。",
                    "INVALID_RETRIEVAL_JOB_UPDATE",
                    400,
                ));
            }
            selected_id = job["jobId"].clone();
            let jobs = document["jobs"].as_array_mut().unwrap();
            if arguments["resumeAutomatic"] == true {
                for previous in jobs.iter_mut() {
                    if previous["projectId"] == job["projectId"]
                        && (previous["automaticRebuildBlocked"] == true
                            || previous["status"] == "cancelled"
                                && previous["error"] == "INDEX_CANCELLED")
                    {
                        previous["automaticRebuildBlocked"] = json!(false);
                    }
                }
            }
            let mut retained: Vec<Value> = jobs
                .iter()
                .filter(|job| active(job) || job["automaticRebuildBlocked"] == true)
                .cloned()
                .collect();
            let terminal: Vec<Value> = jobs
                .iter()
                .filter(|job| !active(job) && job["automaticRebuildBlocked"] != true)
                .cloned()
                .collect();
            retained.extend(
                terminal
                    .into_iter()
                    .rev()
                    .take(95)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
            retained.push(job.clone());
            let identities: HashSet<String> = retained
                .iter()
                .map(|job| job["jobId"].to_string())
                .collect();
            retired = jobs
                .iter()
                .filter(|job| !identities.contains(&job["jobId"].to_string()))
                .map(|job| job["jobId"].clone())
                .collect();
            *jobs = retained;
        }
        "storage.jobs.update" | "storage.jobs.commitBatch" => {
            let index = document["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .position(|job| job["jobId"] == selected_id);
            if operation == "storage.jobs.commitBatch"
                && index.is_none_or(|index| document["jobs"][index]["status"] != "running")
            {
                return Err(ContractError::new(
                    "索引任务未运行，不能记录批次。",
                    "RETRIEVAL_JOB_STATE_CONFLICT",
                    409,
                ));
            }
            let index = index.ok_or_else(missing)?;
            let undefined_fields = arguments
                .get("undefinedFields")
                .map(|value| {
                    value
                        .as_array()
                        .filter(|fields| fields.len() <= 6)
                        .ok_or_else(|| {
                            ContractError::new(
                                "索引字段清除回执无效。",
                                "INVALID_RETRIEVAL_JOB_UPDATE",
                                400,
                            )
                        })
                })
                .transpose()?;
            let mut cleared = HashSet::new();
            for field in undefined_fields.into_iter().flatten() {
                let field = field
                    .as_str()
                    .filter(|field| {
                        [
                            "error",
                            "startedAt",
                            "finishedAt",
                            "checkpoint",
                            "semantic",
                            "coverage",
                        ]
                        .contains(field)
                    })
                    .ok_or_else(|| {
                        ContractError::new(
                            "不能清除索引任务身份、状态或进度。",
                            "INVALID_RETRIEVAL_JOB_UPDATE",
                            400,
                        )
                    })?;
                if arguments["patch"].get(field).is_some() || !cleared.insert(field) {
                    return Err(ContractError::new(
                        "索引字段清除回执与更新冲突。",
                        "INVALID_RETRIEVAL_JOB_UPDATE",
                        400,
                    ));
                }
            }
            job_contracts::patch(
                &arguments["patch"],
                &document["jobs"][index],
                timestamps,
                operation == "storage.jobs.commitBatch",
            )?;
            if operation == "storage.jobs.commitBatch" {
                job_contracts::checkpoint(&arguments["checkpoint"], timestamps)?;
                let sources = arguments["sources"]
                    .as_array()
                    .filter(|sources| sources.len() <= 128)
                    .ok_or_else(|| {
                        ContractError::new(
                            "索引批次回执过大。",
                            "INVALID_RETRIEVAL_CHECKPOINT",
                            400,
                        )
                    })?;
                for source in sources {
                    job_contracts::checkpoint_source(source)?;
                }
            }
            document["jobs"][index]
                .as_object_mut()
                .unwrap()
                .extend(arguments["patch"].as_object().unwrap().clone());
            // Only native-admitted optional fields may be cleared; identity, state and monotonic progress remain intact.
            // 仅清除原生准入的可选字段；身份、状态及单调进度仍由原生校验保持。
            for field in cleared {
                document["jobs"][index]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            }
            if operation == "storage.jobs.commitBatch" {
                document["jobs"][index]["checkpoint"] = arguments["checkpoint"].clone();
            }
        }
        "storage.jobs.recover" => {
            changed = false;
            for job in document["jobs"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .filter(|job| active(job))
            {
                if job["automaticRebuildBlocked"] == true {
                    job["status"] = json!("cancelled");
                    job["error"] = json!("INDEX_CANCELLED");
                } else if arguments["resumable"] == true
                    && job.get("checkpoint").is_some()
                    && job_contracts::checkpoint(&job["checkpoint"], timestamps).is_ok()
                {
                    pending.push(job.clone());
                    continue;
                } else {
                    job["status"] = json!("failed");
                    job["error"] = json!("INDEX_JOB_INTERRUPTED");
                }
                job["finishedAt"] = arguments["now"].clone();
                changed = true;
            }
        }
        _ => {
            return Err(ContractError::new(
                "未知索引作业操作。",
                "INVALID_RETRIEVAL_JOB_UPDATE",
                400,
            ));
        }
    }
    Ok(
        json!({"document": document, "changed": changed, "retired": retired, "pending": pending, "jobId": selected_id}),
    )
}
