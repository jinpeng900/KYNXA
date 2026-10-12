use super::{
    ContractError, MAX_SAFE_INTEGER, document_coverage, hash, keys, member, safe_integer,
    utf16_length,
};
use serde_json::Value;

fn invalid(message: &str, checkpoint: bool) -> ContractError {
    ContractError::new(
        message,
        if checkpoint {
            "INVALID_RETRIEVAL_CHECKPOINT"
        } else {
            "INVALID_RETRIEVAL_JOB_UPDATE"
        },
        400,
    )
}

fn text(value: &Value, maximum: usize, nonempty: bool) -> bool {
    value
        .as_str()
        .is_some_and(|value| (!nonempty || !value.is_empty()) && utf16_length(value) <= maximum)
}

fn nullable_text(value: &Value, maximum: usize) -> bool {
    value.is_null() || text(value, maximum, false)
}
fn quantity(value: &Value) -> bool {
    safe_integer(value, 0, MAX_SAFE_INTEGER)
}
fn diagnostic(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        !value.is_empty()
            && value.len() <= 128
            && value.as_bytes()[0].is_ascii_uppercase()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

pub fn checkpoint(value: &Value, timestamps: &Value) -> Result<(), ContractError> {
    if !keys(
        value,
        &[
            "version",
            "kind",
            "checkpointId",
            "settingsSignature",
            "root",
            "bindingRevision",
            "preparationVersion",
            "updatedAt",
        ],
    ) || value["version"] != 1
        || value["kind"] != "source-index"
        || !hash(&value["checkpointId"])
        || !hash(&value["settingsSignature"])
        || value
            .get("root")
            .is_none_or(|value| !nullable_text(value, 4096))
        || !quantity(&value["bindingRevision"])
        || value
            .get("preparationVersion")
            .is_none_or(|value| !nullable_text(value, 512))
        || value["updatedAt"]
            .as_str()
            .is_none_or(|timestamp| timestamps[timestamp] != true)
    {
        return Err(invalid("索引恢复检查点无效。", true));
    }
    Ok(())
}

pub fn checkpoint_source(value: &Value) -> Result<(), ContractError> {
    if !keys(
        value,
        &[
            "sourceId",
            "inputSignature",
            "fingerprint",
            "semantic",
            "preparationVersion",
            "derivationSignature",
            "embeddingInputSignature",
            "chunkCount",
            "vectorChunks",
            "embeddingProfileId",
            "embeddingModelVersion",
            "embeddingSpaceId",
            "vectorDimensions",
        ],
    ) || !text(&value["sourceId"], 256, true)
        || [
            "inputSignature",
            "fingerprint",
            "derivationSignature",
            "embeddingInputSignature",
        ]
        .iter()
        .any(|field| !hash(&value[field]))
        || !value["semantic"].is_boolean()
        || !text(&value["preparationVersion"], 512, true)
        || !quantity(&value["chunkCount"])
        || !quantity(&value["vectorChunks"])
        || value["vectorChunks"].as_f64() > value["chunkCount"].as_f64()
        || value
            .get("vectorDimensions")
            .is_none_or(|value| !(value.is_null() || safe_integer(value, 1, 4096)))
        || [
            "embeddingProfileId",
            "embeddingModelVersion",
            "embeddingSpaceId",
        ]
        .iter()
        .any(|field| {
            value
                .get(field)
                .is_none_or(|value| !nullable_text(value, 512))
        })
    {
        return Err(invalid("索引批次回执无效。", true));
    }
    Ok(())
}

pub fn semantic(value: &Value) -> Result<(), ContractError> {
    if !keys(
        value,
        &[
            "requested",
            "profileId",
            "state",
            "totalChunks",
            "vectorChunks",
            "cachedChunks",
            "diagnosticCodes",
            "priorDiagnosticCodes",
            "skippedSources",
            "skippedChunks",
        ],
    ) || !value["requested"].is_boolean()
        || value
            .get("profileId")
            .is_none_or(|value| !nullable_text(value, 256))
        || !member(
            &value["state"],
            &["disabled", "complete", "partial", "unavailable"],
        )
        || ["totalChunks", "vectorChunks", "cachedChunks"]
            .iter()
            .any(|field| !quantity(&value[field]))
        || ["skippedSources", "skippedChunks"]
            .iter()
            .any(|field| value.get(field).is_some_and(|value| !quantity(value)))
        || ["diagnosticCodes", "priorDiagnosticCodes"]
            .iter()
            .any(|field| {
                if *field == "priorDiagnosticCodes" && value.get(field).is_none() {
                    return false;
                }
                value[field].as_array().is_none_or(|codes| {
                    codes.len() > 8 || codes.iter().any(|code| !diagnostic(code))
                })
            })
    {
        return Err(invalid("索引语义进度无效。", false));
    }
    Ok(())
}

fn coverage_limit(value: &Value) -> bool {
    text(&value["dimension"], 64, false)
        && safe_integer(&value["limit"], 1, MAX_SAFE_INTEGER)
        && quantity(&value["observed"])
}

pub fn coverage(value: &Value) -> Result<(), ContractError> {
    if !keys(
        value,
        &[
            "discovered",
            "files",
            "lexical",
            "semantic",
            "failed",
            "skipped",
            "partial",
            "complete",
            "sources",
            "failures",
            "reportTruncated",
            "limit",
            "limits",
            "effectiveLimits",
        ],
    ) || !value["complete"].is_boolean()
        || ["discovered", "lexical", "semantic", "failed", "skipped"]
            .iter()
            .any(|field| !quantity(&value[field]))
        || ["files", "partial"]
            .iter()
            .any(|field| value.get(field).is_some_and(|value| !quantity(value)))
        || value
            .get("reportTruncated")
            .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid("索引覆盖进度无效。", false));
    }
    if let Some(sources) = value.get("sources") {
        if sources.as_array().is_none_or(|sources| {
            sources.len() > 1000
                || sources.iter().any(|source| {
                    !text(&source["sourceId"], 256, false)
                        || !text(&source["relativePath"], 4096, false)
                        || ["status", "lexical", "semantic", "parser"]
                            .iter()
                            .any(|field| {
                                !member(
                                    &source[field],
                                    &[
                                        "pending",
                                        "ready",
                                        "partial",
                                        "failed",
                                        "skipped",
                                        "unverified",
                                        "disabled",
                                    ],
                                )
                            })
                        || source
                            .get("errorCode")
                            .is_some_and(|value| !diagnostic(value))
                })
        }) {
            return Err(invalid("索引来源覆盖记录无效。", false));
        }
    }
    if let Some(failures) = value.get("failures") {
        if failures.as_array().is_none_or(|failures| {
            failures.len() > 1000
                || failures.iter().any(|entry| {
                    !text(&entry["relativePath"], 4096, false) || !diagnostic(&entry["errorCode"])
                })
        }) {
            return Err(invalid("索引来源失败记录无效。", false));
        }
    }
    for entry in ["sources", "failures"]
        .iter()
        .flat_map(|field| value[field].as_array().into_iter().flatten())
    {
        if let Some(coverage) = entry.get("documentCoverage") {
            document_coverage::validate(coverage)?;
        }
    }
    if value
        .get("limit")
        .is_some_and(|value| !value.is_null() && value != false && !coverage_limit(value))
        || value.get("limits").is_some_and(|value| {
            value.as_array().is_none_or(|limits| {
                limits.len() > 50 || limits.iter().any(|value| !coverage_limit(value))
            })
        })
        || value.get("effectiveLimits").is_some_and(|value| {
            !keys(
                value,
                &[
                    "maximumFiles",
                    "maximumSourceBytes",
                    "maximumBytes",
                    "maximumEntries",
                    "maximumDocumentInputBytes",
                    "maximumDocumentOutputBytes",
                    "maximumPdfPages",
                ],
            ) || value.as_object().is_none_or(|limits| {
                limits
                    .values()
                    .any(|value| !safe_integer(value, 1, MAX_SAFE_INTEGER))
            })
        })
    {
        return Err(invalid("索引覆盖限额无效。", false));
    }
    Ok(())
}

pub fn patch(
    value: &Value,
    job: &Value,
    timestamps: &Value,
    batch: bool,
) -> Result<(), ContractError> {
    if !value.is_object() {
        return Err(invalid("索引任务更新格式无效。", false));
    }
    if !batch {
        if value
            .get("jobId")
            .is_some_and(|value| *value != job["jobId"])
            || value
                .get("projectId")
                .is_some_and(|value| *value != job["projectId"])
        {
            return Err(ContractError::new(
                "索引任务身份不能修改。",
                "INVALID_RETRIEVAL_JOB_UPDATE",
                409,
            ));
        }
        if let Some(status) = value.get("status") {
            let allowed: &[&str] = match job["status"].as_str().unwrap_or("") {
                "queued" => &["queued", "running", "cancelled", "failed"],
                "running" => &[
                    "running",
                    "paused",
                    "completed",
                    "partial",
                    "cancelled",
                    "failed",
                ],
                "paused" => &["paused", "running", "cancelled", "failed"],
                "completed" => &["completed"],
                "partial" => &["partial"],
                "cancelled" => &["cancelled"],
                "failed" => &["failed"],
                _ => &[],
            };
            if !member(status, allowed) {
                return Err(ContractError::new(
                    "索引任务已结束，不能恢复运行。",
                    "RETRIEVAL_JOB_STATE_CONFLICT",
                    409,
                ));
            }
        }
        if let Some(checkpoint_value) = value.get("checkpoint") {
            checkpoint(checkpoint_value, timestamps)?;
        }
        if value
            .get("automaticRebuildBlocked")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(invalid("索引取消策略无效。", false));
        }
    }
    for field in ["completedSources", "totalSources"] {
        if value.get(field).is_some_and(|value| {
            !quantity(value) || field == "completedSources" && value.as_f64() < job[field].as_f64()
        }) {
            return Err(invalid("索引任务进度无效。", false));
        }
    }
    if let Some(value) = value.get("semantic") {
        semantic(value)?;
    }
    if let Some(value) = value.get("coverage") {
        coverage(value)?;
    }
    Ok(())
}
