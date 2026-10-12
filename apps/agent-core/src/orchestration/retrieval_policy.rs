use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn metadata_mismatch(expected: &Value, result: &Value) -> bool {
    ["modelVersion", "inputProjectionVersion"]
        .iter()
        .any(|field| {
            expected.get(*field).is_some()
                && result.get(*field).is_some()
                && expected[*field] != result[*field]
        })
}

/// Retrieval policy validates live SDK receipts without substituting similarity for dependency proof.
/// 检索策略核验实时 SDK 回执，不把相似性替换为依赖关系证明。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "orchestration.retrievalGapValid" => Ok(json!(
            args.get("gap").is_none()
                || args["gap"]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()
                        && value.encode_utf16().count() <= 1000
                        && !value.contains('\0'))
        )),
        "orchestration.retrievalProvidedOriginal" => {
            let item = &args["item"];
            let reference = &args["reference"];
            Ok(json!(
                args["hasOriginalText"] == true
                    && args["textCharacters"]
                        .as_u64()
                        .is_some_and(|length| length > 0 && length <= 65536)
                    && item["navigationOnly"] != true
                    && reference["chunkHash"]
                        .as_str()
                        .is_some_and(|hash| !hash.is_empty())
                    && [
                        "chunkHash",
                        "chunkId",
                        "sourceId",
                        "scopeKey",
                        "contentHash",
                        "sourceRevision",
                        "derivationSignature"
                    ]
                    .iter()
                    .all(|key| reference[*key] == item[*key])
                    && args["originalTextHash"] == reference["chunkHash"]
            ))
        }
        "orchestration.retrievalRerankerExecution" => {
            let local = &args["local"];
            let mut profile = local["rerankProfileId"].clone();
            if [
                "builtin-multilingual-reranker",
                "builtin-multilingual-reranker-dml-q8",
            ]
            .contains(&text(&profile))
            {
                if local["rerankDevicePolicy"] == "cpu" {
                    profile = json!("builtin-multilingual-reranker");
                } else if local["rerankDevicePolicy"] == "gpu" {
                    profile = json!("builtin-multilingual-reranker-dml-q8");
                }
            }
            Ok(
                json!({"profileId":profile,"devicePreference":if local["rerankDevicePolicy"]=="cpu" {"cpu"}else{"auto"}}),
            )
        }
        "orchestration.retrievalMetadataMismatch" => {
            Ok(json!(metadata_mismatch(&args["expected"], &args["result"])))
        }
        "orchestration.retrievalEmbeddingValid" => {
            let status = &args["status"];
            let result = &args["result"];
            let profile = &args["profile"];
            let registered = status["embeddingSpaceId"] == profile["embeddingSpaceId"]
                || result["embeddingSpaceId"] == profile["embeddingSpaceId"]
                || status["modelVersion"] == profile["modelVersion"]
                || result["modelVersion"] == profile["modelVersion"];
            let dimensions = if registered {
                profile.get("dimensions")
            } else {
                status.get("dimensions")
            };
            let valid = result["profileId"] == args["profileId"]
                && (!status
                    .get("embeddingSpaceId")
                    .is_some_and(|v| !v.is_null() && v != "")
                    || result["embeddingSpaceId"] == status["embeddingSpaceId"])
                && !metadata_mismatch(status, result)
                && args["validVectorType"] == true
                && !dimensions.is_some_and(|d| status.get("dimensions").is_some_and(|s| s != d))
                && !dimensions.is_some_and(|d| {
                    args["vectorLength"] != *d || result.get("dimensions").is_some_and(|r| r != d)
                })
                && !result
                    .get("dimensions")
                    .is_some_and(|d| args["vectorLength"] != *d);
            Ok(json!(valid))
        }
        "orchestration.retrievalRerankOrder" => {
            let originals = items(&args["candidates"]);
            let mut selected = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut scored = 0;
            for item in items(&args["items"]) {
                let Some(index) = originals
                    .iter()
                    .position(|original| original["sourceRef"] == item["sourceRef"])
                else {
                    continue;
                };
                if seen.contains(text(&item["sourceRef"]))
                    || item["hasScore"] == true && item["validScore"] != true
                {
                    continue;
                }
                seen.insert(text(&item["sourceRef"]).to_owned());
                let rank = if item["hasScore"] == true {
                    scored += 1;
                    Some(scored)
                } else {
                    None
                };
                selected.push(json!({"index":index,"rank":rank,"score":item["rerankScore"]}));
            }
            for (index, item) in originals.iter().enumerate() {
                if !seen.contains(text(&item["sourceRef"])) {
                    selected.push(json!({"index":index,"rank":null}));
                }
            }
            Ok(json!({"selection":selected,"scoredCount":scored}))
        }
        "orchestration.retrievalQueryValid" => {
            let valid = args["query"]
                .as_str()
                .is_some_and(|v| !v.trim().is_empty() && v.encode_utf16().count() <= 4000)
                && args["limit"]
                    .as_u64()
                    .is_some_and(|v| (1..=60).contains(&v))
                && args["maximumTokens"].as_u64().is_some_and(|v| v <= 32768);
            Ok(json!(valid))
        }
        "orchestration.retrievalOriginalFallback" => {
            let original = text(&args["originalQuery"]);
            let intent = &args["intent"];
            Ok(json!(
                args["empty"] == true
                    && args["prepared"] == true
                    && intent["domain"] == "mixed"
                    && text(&intent["path"]).is_empty()
                    && text(&intent["symbol"]).is_empty()
                    && !original.is_empty()
                    && original.encode_utf16().count() <= 4000
                    && original.nfkc().collect::<String>()
                        != text(&args["query"]).nfkc().collect::<String>()
            ))
        }
        "orchestration.retrievalSourceCurrent" => {
            let source = &args["source"];
            let item = &args["item"];
            Ok(json!(
                !source.is_null()
                    && source["contentHash"] == item["contentHash"]
                    && source["sourceRevision"] == item["sourceRevision"]
                    && source["bindingRevision"].as_u64().unwrap_or(0)
                        == item["bindingRevision"].as_u64().unwrap_or(0)
            ))
        }
        "orchestration.retrievalContinuation" => {
            let item = &args["item"];
            let next = item["nextOffset"].as_u64();
            let total = item["totalCharacters"].as_u64();
            Ok(
                if next.zip(total).is_some_and(|(next, total)| next < total) {
                    json!({"tool":"knowledge.read","arguments":{"sourceRef":args["sourceRef"],"mode":"page","offset":next,"limit":args["limit"]},"offsetUnit":"utf16-code-units","remainingRange":{"startOffset":next,"endOffset":total}})
                } else {
                    Value::Null
                },
            )
        }
        "orchestration.conversationSourceSelection" => {
            let relationship = &args["relationship"];
            let scopes = super::evidence_projection::calculate(
                "orchestration.visibleScopes",
                &json!({"relationship":relationship}),
            )?;
            let mut selected = Vec::new();
            for (index, entry) in items(&args["memoryEntries"]).iter().enumerate() {
                let scope = if entry["scope"] == "user" {
                    "user".into()
                } else {
                    format!(
                        "{}:{}",
                        text(&entry["scope"]),
                        text(&entry["scopeId"]).to_lowercase()
                    )
                };
                if entry["status"] == "confirmed"
                    && entry["active"] != false
                    && items(&scopes).iter().any(|v| v == &scope)
                {
                    selected.push(json!({"kind":"memory","index":index,"scopeKey":scope}));
                }
            }
            for (index, message) in items(&args["messages"]).iter().enumerate() {
                if message["Status"] == "completed"
                    && ["user", "assistant"].contains(&text(&message["Role"]))
                    && !text(&message["Content"]).trim().is_empty()
                {
                    selected.push(json!({"kind":"message","index":index}));
                }
            }
            Ok(json!(selected))
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
