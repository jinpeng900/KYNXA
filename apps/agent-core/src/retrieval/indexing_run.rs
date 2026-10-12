use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Default)]
struct Run {
    requested: bool,
    profile_id: Value,
    discovered: u64,
    counts: HashMap<String, i64>,
    diagnostics: Vec<String>,
    known_sources: HashSet<String>,
    states: HashMap<String, Value>,
    entries: HashMap<String, Value>,
    order: VecDeque<String>,
    truncated: bool,
}

#[derive(Default)]
pub struct IndexingRunState {
    run: Option<Run>,
    structure: super::service_lifecycle::StructureState,
    transport: super::service_lifecycle::TransportState,
}

fn diagnostic(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_uppercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}
fn count(run: &Run, field: &str) -> i64 {
    run.counts.get(field).copied().unwrap_or(0)
}
fn increment(run: &mut Run, field: &str, delta: i64) {
    *run.counts.entry(field.to_owned()).or_default() += delta;
}
fn metadata_size(value: &Value) -> usize {
    value
        .as_u64()
        .unwrap_or(2 * 1024 * 1024)
        .min(2 * 1024 * 1024) as usize
}

fn embedding_batch(arguments: &Value) -> Value {
    let status = &arguments["status"];
    let suggestion = status
        .get("batchSuggestions")
        .unwrap_or(&status["resourceReservation"]["batchSuggestions"]);
    let suggested_size = suggestion["batchSize"]
        .as_u64()
        .filter(|value| *value > 0)
        .unwrap_or(32);
    let admission_size = status["requestLimits"]["maxBatchDocuments"]
        .as_u64()
        .filter(|value| *value > 0)
        .unwrap_or(128);
    let maximum_chunks = 128_u64.min(suggested_size).min(admission_size).max(1) as usize;
    let maximum_input = status["maxInputTokens"]
        .as_u64()
        .filter(|value| *value > 0)
        .unwrap_or(512);
    let token_budget = suggestion["batchTokenBudget"]
        .as_u64()
        .filter(|value| *value >= maximum_input)
        .unwrap_or(32 * maximum_input);
    let byte_budget = status["requestLimits"]["maxRequestBytes"]
        .as_u64()
        .unwrap_or(256 * 1024)
        .min(256 * 1024);
    let mut selected = 0;
    let mut bytes = 0;
    let mut tokens = 0;
    let mut length_bound = maximum_chunks;
    for chunk in arguments["chunks"].as_array().into_iter().flatten() {
        let size = chunk["bytes"].as_u64().unwrap_or(0) + 64;
        let demand = if arguments["fitting"] == true {
            size
        } else {
            chunk["tokenCount"]
                .as_u64()
                .unwrap_or(maximum_input.min(size))
        };
        let length = chunk["characters"].as_u64().unwrap_or(0);
        let tier = if length > 4096 {
            32
        } else if length > 2048 {
            64
        } else {
            128
        };
        let next_bound = length_bound.min(tier);
        if selected > 0
            && (selected >= next_bound
                || bytes + size > byte_budget
                || tokens + demand > token_budget)
        {
            break;
        }
        if selected >= length_bound {
            break;
        }
        selected += 1;
        bytes += size;
        tokens += demand;
        length_bound = next_bound;
    }
    json!(selected)
}

fn publication_batch(arguments: &Value) -> Result<Value, String> {
    let requested_size = arguments["batchSize"]
        .as_u64()
        .filter(|value| *value >= 1 && *value <= 128)
        .ok_or("INVALID_RETRIEVAL_SETTINGS")?;
    let mut count = 0;
    let mut characters = 0;
    for source in arguments["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .take(requested_size.min(100) as usize)
    {
        let measured = source
            .get("textCharacters")
            .or_else(|| source.get("windowCharacters"))
            .or_else(|| source.get("storedBytes"))
            .unwrap_or(&Value::Null);
        let size = metadata_size(measured);
        if count > 0 && characters + size > 8 * 1024 * 1024 {
            break;
        }
        count += 1;
        characters += size;
    }
    Ok(json!(count))
}

fn report(run: &Run) -> Value {
    let failed = count(run, "failed");
    let skipped = count(run, "skipped");
    let partial = count(run, "partial");
    let vectors = count(run, "vectorChunks");
    let total = count(run, "totalChunks");
    let state = if !run.requested {
        "disabled"
    } else if failed > 0 || skipped > 0 {
        if vectors > 0 {
            "partial"
        } else {
            "unavailable"
        }
    } else if vectors == total {
        "complete"
    } else if vectors > 0 {
        "partial"
    } else {
        "unavailable"
    };
    json!({"coverage": {"discovered": run.discovered, "lexical": count(run,"lexical"), "semantic": count(run,"semantic"), "failed": failed, "skipped": skipped, "partial": partial, "complete": failed == 0 && skipped == 0 && partial == 0, "sources": run.order.iter().filter_map(|id| run.entries.get(id)).collect::<Vec<_>>(), "reportTruncated": run.truncated},
        "semantic": {"requested": run.requested, "profileId": run.profile_id, "state": state, "totalChunks": total, "vectorChunks": vectors, "cachedChunks": count(run,"cachedChunks"), "skippedSources": count(run,"skippedSources"), "skippedChunks": count(run,"skippedChunks"), "diagnosticCodes": run.diagnostics}})
}

/// One bounded native run owns coverage transitions, exact progress and publication/inference batch policy.
/// 单个有界原生运行拥有覆盖状态转换、真实进度和发布及推理组批策略。
pub fn apply(operation: &str, arguments: &Value) -> Result<Value, String> {
    if operation == "retrieval.indexer-embedding-batch" {
        return Ok(embedding_batch(arguments));
    }
    if operation == "retrieval.indexer-publication-batch" {
        return publication_batch(arguments);
    }
    if operation == "retrieval.indexer-compatible-contract" {
        return Ok(json!(
            [
                "profileId",
                "modelVersion",
                "inputProjectionVersion",
                "embeddingSpaceId",
                "dimensions"
            ]
            .iter()
            .all(|field| arguments["expected"].get(field).is_none()
                || arguments["actual"].get(field).is_none()
                || arguments["expected"][field] == arguments["actual"][field])
        ));
    }
    Err("AGENT_CORE_UNKNOWN_OPERATION".into())
}

impl IndexingRunState {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> Result<Value, String> {
        if let Some(operation) = operation.strip_prefix("retrieval.indexing.structure.") {
            return Ok(super::service_lifecycle::structure(
                &mut self.structure,
                operation,
                arguments,
            ));
        }
        if let Some(operation) = operation.strip_prefix("retrieval.indexing.transport.") {
            return Ok(super::service_lifecycle::transport(
                &mut self.transport,
                operation,
                arguments,
            ));
        }
        if operation == "retrieval.indexing.open" {
            if self.run.is_some() {
                return Err("AGENT_CORE_INDEX_RUN_ALREADY_OPEN".into());
            }
            let source_ids: Vec<String> = arguments["sourceIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            self.run = Some(Run {
                requested: arguments["semanticRequested"] == true,
                profile_id: arguments["profileId"].clone(),
                discovered: arguments["sourceCount"]
                    .as_u64()
                    .unwrap_or(source_ids.len() as u64),
                known_sources: source_ids.into_iter().collect(),
                ..Run::default()
            });
            return Ok(Value::Null);
        }
        if operation == "retrieval.indexing.close" {
            self.run = None;
            return Ok(Value::Null);
        }
        let run = self.run.as_mut().ok_or("AGENT_CORE_INDEX_RUN_CLOSED")?;
        match operation {
            "retrieval.indexing.report" => Ok(report(run)),
            "retrieval.indexing.entry" => Ok(run
                .entries
                .get(arguments["sourceId"].as_str().unwrap_or(""))
                .cloned()
                .unwrap_or(Value::Null)),
            "retrieval.indexing.increment" => {
                for (field, delta) in arguments["delta"].as_object().into_iter().flatten() {
                    if [
                        "totalChunks",
                        "vectorChunks",
                        "cachedChunks",
                        "skippedSources",
                        "skippedChunks",
                        "failed",
                    ]
                    .contains(&field.as_str())
                    {
                        increment(run, field, delta.as_i64().unwrap_or(0));
                    }
                }
                Ok(Value::Null)
            }
            "retrieval.indexing.diagnostic" => {
                let code = arguments["code"]
                    .as_str()
                    .filter(|value| diagnostic(value))
                    .unwrap_or(arguments["fallback"].as_str().unwrap_or("EMBEDDING_FAILED"));
                if run.diagnostics.len() < 8 && !run.diagnostics.iter().any(|value| value == code) {
                    run.diagnostics.push(code.to_owned());
                }
                Ok(Value::Null)
            }
            "retrieval.indexing.coverage" => {
                let entry = &arguments["entry"];
                let source_id = entry["sourceId"]
                    .as_str()
                    .ok_or("INVALID_RETRIEVAL_INPUT")?
                    .to_owned();
                if arguments["discover"] == true && run.known_sources.insert(source_id.clone()) {
                    run.discovered += 1;
                }
                if let Some(old) = run.states.get(&source_id).cloned() {
                    if old["lexical"] == "ready" {
                        increment(run, "lexical", -1);
                    }
                    if old["semantic"] == "ready" {
                        increment(run, "semantic", -1);
                    }
                    if let Some(status) = old["status"]
                        .as_str()
                        .filter(|value| ["failed", "skipped", "partial"].contains(value))
                    {
                        increment(run, status, -1);
                    }
                }
                if entry["lexical"] == "ready" {
                    increment(run, "lexical", 1);
                }
                if entry["semantic"] == "ready" {
                    increment(run, "semantic", 1);
                }
                if let Some(status) = entry["status"]
                    .as_str()
                    .filter(|value| ["failed", "skipped", "partial"].contains(value))
                {
                    increment(run, status, 1);
                }
                let mut entry = entry.clone();
                if let Some(code) = entry.get("errorCode") {
                    if !code.as_str().is_some_and(diagnostic) {
                        entry["errorCode"] = json!("RETRIEVAL_SOURCE_FAILED");
                    }
                }
                run.states.insert(source_id.clone(), json!({"status": entry["status"], "lexical": entry["lexical"], "semantic": entry["semantic"]}));
                if !run.entries.contains_key(&source_id) {
                    run.order.push_back(source_id.clone());
                }
                run.entries.insert(source_id, entry);
                let mut evicted = Vec::new();
                while run.entries.len() > 1000 {
                    if let Some(id) = run.order.pop_front() {
                        run.entries.remove(&id);
                        evicted.push(id);
                        run.truncated = true;
                    }
                }
                Ok(json!(evicted))
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
