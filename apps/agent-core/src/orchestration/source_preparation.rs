use crate::models::protocol::{array, string, truthy};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};

fn count(value: &Value) -> usize {
    value.as_u64().unwrap_or(0) as usize
}
fn nullish(value: &Value, fallback: Value) -> Value {
    if value.is_null() {
        fallback
    } else {
        value.clone()
    }
}
fn contract(expected: &Value, actual: &Value) -> bool {
    [
        "profileId",
        "modelVersion",
        "inputProjectionVersion",
        "embeddingSpaceId",
        "dimensions",
    ]
    .iter()
    .all(|field| {
        expected.get(*field).is_none()
            || actual.get(*field).is_none()
            || expected[*field] == actual[*field]
    })
}
struct CacheEntry {
    id: u64,
    facts: Value,
}
struct Fit {
    status: Value,
    profile: Value,
    offset: usize,
    total: usize,
    batch: usize,
    contexts: Vec<bool>,
    first_version: Value,
}
struct Embedding {
    positions: Vec<usize>,
    rejected: Vec<usize>,
    length: usize,
}

/// Native preparation owns bounded fingerprints and receipt decisions; raw SDK bodies and identity handles stay outside.
/// 原生准备服务拥有有界指纹及回执决策；原始 SDK 正文与对象身份句柄留在外部。
#[derive(Default)]
pub struct SourcePreparationState {
    sequence: u64,
    cache: HashMap<String, CacheEntry>,
    order: VecDeque<String>,
    fields: Value,
    fits: HashMap<u64, Fit>,
    embeddings: HashMap<u64, Embedding>,
}
impl SourcePreparationState {
    fn next_id(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let op = operation
            .strip_prefix("orchestration.sourceManagement.preparation.")
            .ok_or("UNKNOWN_SOURCE_PREPARATION_OPERATION")?;
        let id = args["id"].as_u64().unwrap_or(0);
        match op {
            "fingerprintIndices" => Ok(json!([
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                if truthy(&args["semantic"]) {
                    Some(5)
                } else {
                    None
                },
                if truthy(&args["semantic"]) {
                    Some(6)
                } else {
                    None
                },
                if truthy(&args["semantic"]) {
                    Some(7)
                } else {
                    None
                },
                if truthy(&args["semantic"]) {
                    Some(8)
                } else {
                    None
                }
            ])),
            "metadataPlan" => {
                if args["hasText"] != true
                    && !regex::Regex::new("^[a-f0-9]{64}$")
                        .unwrap()
                        .is_match(string(&args["contentHash"]))
                {
                    return Err("RETRIEVAL_SOURCE_CHANGED".into());
                }
                Ok(json!({"hasText": args["hasText"] == true}))
            }
            "execution" => Ok(
                json!({"devicePreference": if args["settings"]["local"]["embeddingDevicePolicy"] == "cpu" { "cpu" } else { "auto" }, "semanticRequested": truthy(&args["semantic"]) && args["settings"]["local"]["semantic"] != "off" && !args["settings"]["local"]["embeddingProfileId"].is_null()}),
            ),
            "allocate" => Ok(json!(self.next_id())),
            "field" => {
                if !self.fields.is_object() {
                    self.fields = json!({"preparationFailures": 0});
                }
                let field = string(&args["field"]);
                if args["set"] == true {
                    self.fields[field] = args["value"].clone();
                }
                Ok(self.fields[field].clone())
            }
            "incrementFailure" => {
                if !self.fields.is_object() {
                    self.fields = json!({});
                }
                self.fields["preparationFailures"] =
                    json!(count(&self.fields["preparationFailures"]) + 1);
                Ok(Value::Null)
            }
            "map" => {
                let key = string(&args["key"]);
                match string(&args["action"]) {
                    "get" => Ok(self
                        .cache
                        .get(key)
                        .map(|entry| json!({"id": entry.id, "facts": entry.facts}))
                        .unwrap_or(Value::Null)),
                    "has" => Ok(json!(self.cache.contains_key(key))),
                    "size" => Ok(json!(self.cache.len())),
                    "set" => {
                        let id = self.next_id();
                        let mut released = Vec::new();
                        if let Some(old) = self.cache.insert(
                            key.into(),
                            CacheEntry {
                                id,
                                facts: args["facts"].clone(),
                            },
                        ) {
                            released.push(old.id);
                        } else {
                            self.order.push_back(key.into());
                        }
                        while self.cache.len() > 64000 {
                            if let Some(key) = self.order.pop_front() {
                                if let Some(old) = self.cache.remove(&key) {
                                    released.push(old.id);
                                }
                            }
                        }
                        Ok(json!({"id": id, "released": released}))
                    }
                    "delete" => {
                        let old = self.cache.remove(key);
                        self.order.retain(|item| item != key);
                        Ok(json!({"deleted": old.is_some(), "released": old.map(|entry| entry.id)}))
                    }
                    "clear" => {
                        let released = self
                            .cache
                            .values()
                            .map(|entry| entry.id)
                            .collect::<Vec<_>>();
                        self.cache.clear();
                        self.order.clear();
                        Ok(json!({"released": released}))
                    }
                    "page" => Ok(json!(
                        self.order
                            .iter()
                            .skip(count(&args["cursor"]))
                            .take(64)
                            .filter_map(|key| self.cache.get(key).map(
                                |entry| json!({"key": key, "id": entry.id, "facts": entry.facts})
                            ))
                            .collect::<Vec<_>>()
                    )),
                    _ => Err("UNKNOWN_SOURCE_PREPARATION_MAP_OPERATION".into()),
                }
            }
            "sameEntry" => Ok(json!(
                self.cache
                    .get(string(&args["key"]))
                    .is_some_and(|entry| entry.id == id)
            )),
            "reusePlan" => {
                let entry = self.cache.get(string(&args["key"]));
                Ok(json!(
                    args["unavailable"] == true
                        || entry.is_some_and(|entry| truthy(&args["serviceVersion"])
                            && entry.facts["preparation"].is_object()
                            && entry.facts["preparation"]["serviceId"] == args["serviceId"]
                            && entry.facts["preparation"]["serviceVersion"]
                                == args["serviceVersion"]
                            && entry.facts["preparation"]["inputSignature"]
                                == args["inputSignature"]
                            && entry.facts["fingerprint"] == args["fingerprint"])
                ))
            }
            "serviceVersionPlan" => Ok(
                json!({"read": args["structures"] == true && !["unavailable", "closed"].contains(&string(&args["state"]))}),
            ),
            "serviceVersion" => {
                if args["structures"] != true {
                    return Ok(json!(format!(
                        "{}|{}|{}",
                        string(&args["chunkerVersion"]),
                        string(&args["tokenizerVersion"]),
                        string(&args["embeddingVersion"])
                    )));
                }
                Ok(
                    if args["version"].as_str().is_some_and(|version| {
                        !version.is_empty() && version.encode_utf16().count() <= 512
                    }) {
                        args["version"].clone()
                    } else {
                        Value::Null
                    },
                )
            }
            "preparationVersion" => Ok(if !truthy(&args["parserVersion"]) {
                Value::Null
            } else if truthy(&args["fittingVersion"]) {
                json!(format!(
                    "{}|{}|{}",
                    string(&args["parserVersion"]),
                    string(&args["fittingVersion"]),
                    string(&args["fittingIdentity"])
                ))
            } else {
                args["parserVersion"].clone()
            }),
            "unchangedPlan" => {
                let source = &args["source"];
                let row = &args["row"];
                let eligible = ["knowledge", "work-file"].contains(&string(&source["sourceType"]));
                Ok(json!(
                    eligible
                        && (args["beforeRead"] == true
                            || row.is_object()
                                && [
                                    "sourceId",
                                    "scopeKey",
                                    "sourceType",
                                    "contentHash",
                                    "sourceRevision",
                                    "title"
                                ]
                                .iter()
                                .all(|field| source[*field] == row[*field])
                                && nullish(&source["bindingRevision"], json!(0))
                                    == nullish(&row["bindingRevision"], json!(0))
                                && args["sourceLocator"] == args["rowLocator"])
                ))
            }
            "fittedPlan" => {
                if args["beforeRead"] == true {
                    return Ok(json!(args["parseStatus"] != "unavailable"));
                }
                let row = &args["row"];
                let versions = &args["versions"];
                Ok(json!(
                    args["parseStatus"] != "unavailable"
                        && row.is_object()
                        && row["parserVersion"] == versions["parserVersion"]
                        && row["tokenizerVersion"] == versions["tokenizerVersion"]
                        && row["chunkerVersion"]
                            == format!(
                                "{}|{}",
                                string(&args["tokenFittedVersion"]),
                                string(&versions["chunkerVersion"])
                            )
                        && string(&row["embeddingInputVersion"]).starts_with(&format!(
                            "{}|",
                            string(&versions["embeddingInputVersion"])
                        ))
                        && args["rowStructure"] == args["sourceStructure"]
                ))
            }
            "restoreScopes" => {
                let mut seen = HashSet::new();
                Ok(json!(
                    array(&args["sources"])
                        .iter()
                        .filter_map(|source| {
                            let scope = string(&source["scopeKey"]);
                            if seen.insert(scope.to_owned()) {
                                Some(scope)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                ))
            }
            "restorePairs" => {
                let sources = array(&args["sources"]);
                let rows = array(&args["rows"]);
                Ok(json!(
                    array(&args["records"])
                        .iter()
                        .enumerate()
                        .filter_map(|(record_index, record)| {
                            let source = sources
                                .iter()
                                .rposition(|source| source["sourceId"] == record["sourceId"])?;
                            let row = rows
                                .iter()
                                .rposition(|row| row["sourceId"] == record["sourceId"])?;
                            Some(json!({"record": record_index, "source": source, "row": row}))
                        })
                        .collect::<Vec<_>>()
                ))
            }
            "restorePlan" => {
                let source = &args["source"];
                let row = &args["row"];
                let record = &args["record"];
                let status = &args["status"];
                let settings = &args["settings"]["local"];
                let semantic = truthy(&record["semantic"])
                    && settings["semantic"] != "off"
                    && !settings["embeddingProfileId"].is_null();
                let base = truthy(&args["serviceVersion"])
                    && record["preparationVersion"] == args["serviceVersion"]
                    && args["inputSignature"] == record["inputSignature"]
                    && ["contentHash", "sourceRevision", "scopeKey"]
                        .iter()
                        .all(|field| source[*field] == row[*field])
                    && nullish(&source["bindingRevision"], json!(0))
                        == nullish(&row["bindingRevision"], json!(0))
                    && [
                        "derivationSignature",
                        "embeddingInputSignature",
                        "chunkCount",
                        "vectorChunks",
                    ]
                    .iter()
                    .all(|field| row[*field] == record[*field])
                    && (count(&record["vectorChunks"]) == 0
                        || row["embeddingProfileId"] == record["embeddingProfileId"]
                            && row["embeddingModelVersion"] == record["embeddingModelVersion"]
                            && row["embeddingSpaceId"] == record["embeddingSpaceId"]
                            && row["vectorDimensions"] == record["vectorDimensions"]);
                Ok(json!(
                    base && (!semantic
                        || record["vectorChunks"] == record["chunkCount"]
                            && record["embeddingProfileId"] == settings["embeddingProfileId"]
                            && record["embeddingModelVersion"] == args["embeddingVersion"]
                            && record["embeddingSpaceId"] == status["embeddingSpaceId"]
                            && (status.get("dimensions").is_none()
                                || record["vectorDimensions"] == status["dimensions"]))
                        && args["fingerprint"] == record["fingerprint"]
                ))
            }
            "loadedSignature" => {
                if args["expected"] != args["actual"] {
                    return Err("STALE_RETRIEVAL_SOURCE".into());
                }
                Ok(Value::Null)
            }
            "parserFallback" => {
                let source = &args["source"];
                let path = source["locator"]
                    .get("relativePath")
                    .or_else(|| source["locator"].get("path"))
                    .unwrap_or(&source["title"]);
                let code = !["memory", "message"].contains(&string(&source["sourceType"]))
                    && regex::Regex::new("(?i)\\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|pyi?|rs|go)$")
                        .unwrap()
                        .is_match(string(path));
                let error = string(&args["code"]);
                let diagnostic = if regex::Regex::new("^[A-Z][A-Z0-9_]{0,127}$")
                    .unwrap()
                    .is_match(error)
                {
                    error
                } else {
                    "STRUCTURE_PARSE_FAILED"
                };
                Ok(
                    json!({"structure": {"domain": if code { "code" } else { "knowledge" }, "language": null, "parserVersion": "plain-text-v1", "parseStatus": "unavailable", "diagnosticCodes": [diagnostic]}, "parserVersion": "plain-text-v1", "chunkerVersion": args["chunkerVersion"], "embeddingInputVersion": args["embeddingInputVersion"]}),
                )
            }
            "parserFailure" => {
                if args["structure"]["parseStatus"] == "unavailable"
                    && !array(&args["structure"]["diagnosticCodes"])
                        .iter()
                        .any(|code| {
                            ["UNSUPPORTED_CODE_LANGUAGE", "CODE_LANGUAGE_UNSUPPORTED"]
                                .contains(&string(code))
                        })
                {
                    if !self.fields.is_object() {
                        self.fields = json!({});
                    }
                    self.fields["preparationFailures"] =
                        json!(count(&self.fields["preparationFailures"]) + 1);
                }
                Ok(Value::Null)
            }
            "preparationEligible" => Ok(json!(
                args["fittingFailed"] != true && truthy(&args["serviceVersion"])
            )),
            "preparationApproved" => Ok(json!(
                args["serviceVersion"] == args["currentVersion"]
                    && args["sameService"] == true
                    && args["serviceId"] == args["currentServiceId"]
                    && args["parseStatus"] != "unavailable"
            )),
            "derivedReusable" => {
                Ok(json!(self.cache.get(string(&args["key"])).is_some_and(
                    |entry| entry.facts["fingerprint"] == args["fingerprint"]
                )))
            }
            "fittingPolicy" => Ok(
                json!({"required": args["semanticRequested"] == true && args["hasSdk"] == true && truthy(&args["status"]["fittingVersion"]), "failure": if ["ready", "loading"].contains(&string(&args["status"]["state"])) { Value::Null } else { args["status"].get("errorCode").filter(|code| !code.is_null()).cloned().unwrap_or(json!("EMBEDDING_PROFILE_UNAVAILABLE")) }}),
            ),
            "fitOpen" => {
                if count(&args["chunkCount"]) == 0
                    || args["hasSdk"] != true
                    || !truthy(&args["status"]["fittingVersion"])
                {
                    return Ok(Value::Null);
                }
                let id = self.next_id();
                self.fits.insert(
                    id,
                    Fit {
                        status: args["status"].clone(),
                        profile: args["profileId"].clone(),
                        offset: 0,
                        total: 0,
                        batch: 0,
                        contexts: vec![],
                        first_version: Value::Null,
                    },
                );
                Ok(json!({"id": id}))
            }
            "fitBatch" => {
                let fit = self.fits.get_mut(&id).ok_or("UNKNOWN_SOURCE_FIT")?;
                fit.batch = count(&args["count"]);
                fit.contexts = array(&args["contexts"]).iter().map(truthy).collect();
                Ok(Value::Null)
            }
            "fitError" => {
                let fit = self.fits.get_mut(&id).ok_or("UNKNOWN_SOURCE_FIT")?;
                let index = args["index"].as_u64().map(|index| index as usize);
                if args["aborted"] == true
                    || args["code"] != "EMBEDDING_CONTEXT_TOO_LONG"
                    || index.is_none_or(|index| !fit.contexts.get(index).copied().unwrap_or(false))
                {
                    return Ok(json!({"propagate": true}));
                }
                fit.contexts[index.unwrap()] = false;
                Ok(json!({"index": index, "diagnostic": "EMBEDDING_CONTEXT_OMITTED"}))
            }
            "fitReceipt" => {
                let fit = self.fits.get(&id).ok_or("UNKNOWN_SOURCE_FIT")?;
                let mut expected = fit.status.clone();
                expected["profileId"] = fit.profile.clone();
                let receipt = &args["receipt"];
                if !receipt.is_object()
                    || receipt["fittingVersion"] != fit.status["fittingVersion"]
                    || !contract(&expected, receipt)
                    || fit
                        .status
                        .get("maxInputTokens")
                        .is_some_and(|value| *value != receipt["maxInputTokens"])
                {
                    return Err("EMBEDDING_PROFILE_MISMATCH".into());
                }
                Ok(Value::Null)
            }
            "fitAppend" => {
                let fit = self.fits.get_mut(&id).ok_or("UNKNOWN_SOURCE_FIT")?;
                let chunks = array(&args["chunks"]);
                let assignments = chunks.iter().enumerate().map(|(index, chunk)| { let id = string(&chunk["chunkId"]); let base = id.rsplit_once(':').map(|pair| pair.0).unwrap_or(id); json!({"chunkIndex": fit.total + index, "chunkId": format!("{}:{}", base, fit.total + index)}) }).collect::<Vec<_>>();
                if fit.first_version.is_null() {
                    fit.first_version = chunks
                        .first()
                        .map(|chunk| chunk["chunkerVersion"].clone())
                        .unwrap_or(Value::Null);
                }
                fit.total += chunks.len();
                if fit.total > 40000 {
                    return Err("RETRIEVAL_SOURCE_TOO_LARGE".into());
                }
                fit.offset += fit.batch;
                Ok(json!({"assignments": assignments, "offset": fit.offset}))
            }
            "fitFinish" => {
                let fit = self.fits.remove(&id).ok_or("UNKNOWN_SOURCE_FIT")?;
                Ok(
                    json!({"chunkerVersion": nullish(&fit.first_version, args["chunkerVersion"].clone()), "embeddingInputVersion": format!("{}|{}", string(&args["embeddingInputVersion"]), string(&fit.status["fittingVersion"]))}),
                )
            }
            "fitClose" => {
                self.fits.remove(&id);
                Ok(Value::Null)
            }
            "embedOpen" => {
                let length = count(&args["count"]);
                let id = self.next_id();
                self.embeddings.insert(
                    id,
                    Embedding {
                        positions: (0..length).collect(),
                        rejected: vec![],
                        length,
                    },
                );
                Ok(json!(id))
            }
            "embedNext" => {
                let run = self.embeddings.get(&id).ok_or("UNKNOWN_SOURCE_EMBEDDING")?;
                Ok(
                    json!({"positions": run.positions, "rejected": run.rejected, "length": run.length, "done": run.positions.is_empty()}),
                )
            }
            "embedReceipt" => {
                let run = self.embeddings.get(&id).ok_or("UNKNOWN_SOURCE_EMBEDDING")?;
                if args["valid"] != true || count(&args["count"]) != run.positions.len() {
                    return Err("EMBEDDING_PROFILE_MISMATCH".into());
                }
                Ok(
                    json!({"positions": run.positions, "rejected": run.rejected, "length": run.length}),
                )
            }
            "embedError" => {
                let run = self
                    .embeddings
                    .get_mut(&id)
                    .ok_or("UNKNOWN_SOURCE_EMBEDDING")?;
                let index = args["index"].as_u64().map(|index| index as usize);
                if args["code"] != "EMBEDDING_INPUT_TOO_LONG"
                    || index.is_none_or(|index| index >= run.positions.len())
                {
                    return Ok(json!({"propagate": true}));
                }
                let position = run.positions.remove(index.unwrap());
                run.rejected.push(position);
                if !self.fields.is_object() {
                    self.fields = json!({});
                }
                self.fields["lastEmbeddingError"] = args["code"].clone();
                Ok(json!({"diagnostic": args["code"]}))
            }
            "embedClose" => {
                self.embeddings.remove(&id);
                Ok(Value::Null)
            }
            _ => Err("UNKNOWN_SOURCE_PREPARATION_OPERATION".into()),
        }
    }
}
