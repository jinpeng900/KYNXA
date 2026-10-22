use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    context, context_control, history,
    protocol::{
        array, characters, estimate_tokens, output_excerpt, quote, string, tool_message_tokens,
        wire_name,
    },
};

fn raw_field(entry: &mut Value, path: Value, wire: &Value) {
    if wire.is_string() {
        if entry["rawFields"].is_null() {
            entry["rawFields"] = json!([]);
        }
        entry["rawFields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path": path, "wire": wire}));
    }
}

const STATES: &[&str] = &[
    "completed",
    "error",
    "cancelled",
    "denied",
    "unknown",
    "running",
];

fn valid_ref(value: &Value) -> bool {
    Regex::new("(?i)^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$")
        .unwrap()
        .is_match(string(&value["id"]))
        && Regex::new("(?i)^[a-f0-9]{64}$")
            .unwrap()
            .is_match(string(&value["sha256"]))
        && value["bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes <= 9_007_199_254_740_991)
}
fn broker_metadata(activity: &Value, environment: &Value) -> Value {
    if environment.is_null() {
        return Value::Null;
    }
    let mut metadata = json!({"executionEnvironment": environment,
        "status": if STATES.contains(&string(&activity["status"])) { string(&activity["status"]) } else { "unknown" }});
    if activity["executed"] == false {
        metadata["executed"] = json!(false);
    }
    if Regex::new("^[A-Z][A-Z0-9_]{0,99}$")
        .unwrap()
        .is_match(string(&activity["code"]))
    {
        metadata["code"] = activity["code"].clone();
    }
    metadata
}
fn broker_text(output_json: &str, metadata: &Value) -> String {
    if metadata.is_null() {
        return output_json.into();
    }
    let mut fields = Vec::new();
    for key in ["executionEnvironment", "status", "executed", "code"] {
        if let Some(value) = metadata.get(key) {
            fields.push(format!("{}:{value}", quote(key)));
        }
    }
    fields.push(format!("\"output\":{output_json}"));
    format!("{{{}}}", fields.join(","))
}
fn activity_index(assistant: &Value, round: &Value, call: &Value) -> Option<usize> {
    array(&assistant["ToolActivities"])
        .iter()
        .position(|activity| {
            activity["toolCallId"] == call["id"]
                && activity["name"] == call["name"]
                && (activity["round"]
                    .as_u64()
                    .filter(|round| *round <= 9_007_199_254_740_991)
                    .is_none()
                    || activity["round"] == *round)
        })
}
fn strip_reference(value: &mut Value, invalid_id: &str) {
    match value {
        Value::Array(items) => {
            for item in items {
                strip_reference(item, invalid_id);
            }
        }
        Value::Object(object) => {
            if object
                .get("resultRef")
                .is_some_and(|reference| reference["id"] == invalid_id)
            {
                object.remove("resultRef");
            }
            for value in object.values_mut() {
                strip_reference(value, invalid_id);
            }
        }
        _ => {}
    }
}

/// Formal history rules are separate from Data's public-output projection callbacks and raw JSON bytes.
/// 正式历史规则独立于 Data 的公开输出投影回调及原始 JSON 字节；业务输出不能覆盖 broker 事实。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "models.history.extractCheckpoint" => {
            let extracted = history::calculate("models.history.extractSummary", args)?;
            if extracted.is_null() {
                return Ok(json!({}));
            }
            let turns = array(&args["turns"]);
            let all_turns = array(&args["allTurns"]);
            let conversation = string(&args["conversationId"]);
            let digest = |serialized: &str| format!("{:x}", Sha256::digest(serialized.as_bytes()));
            let source_hash = digest(&format!(
                "[{},{}]",
                quote(&conversation.to_lowercase()),
                string(&args["allSourcesJson"])
            ));
            let covered_hash = digest(string(&args["coveredSourcesJson"]));
            let request_hash = digest(string(&args["requestJson"]));
            let through = &turns.last().unwrap()["assistant"]["Id"];
            let previous = &args["previous"];
            let same_id = |left: &Value, right: &Value| {
                left.as_str()
                    .zip(right.as_str())
                    .is_some_and(|(left, right)| left.to_lowercase() == right.to_lowercase())
            };
            let reused = previous["schemaVersion"] == 2
                && previous["algorithm"] == "extractive-v2"
                && same_id(&previous["conversationId"], &args["conversationId"])
                && previous["coveredTurnCount"] == turns.len()
                && previous["historyTurnCount"] == all_turns.len()
                && same_id(&previous["coveredThroughAssistantId"], through)
                && previous["sourceHash"] == source_hash
                && previous["coveredSourceHash"] == covered_hash
                && previous["requestHash"] == request_hash
                && previous["excerptBudgetTokens"] == args["budget"]
                && previous["content"] == extracted["content"]
                && string(&args["previousSelectedSourcesJson"])
                    == extracted["selectedSources"].to_string()
                && previous["createdAt"].is_string()
                && args["previousDateValid"] == true;
            if reused {
                return Ok(json!({"reused": true, "value": previous}));
            }
            let mut value = json!({"schemaVersion": 2, "algorithm": "extractive-v2", "conversationId": conversation,
                "coveredTurnCount": turns.len(), "historyTurnCount": all_turns.len(), "coveredThroughAssistantId": through,
                "sourceHash": source_hash, "coveredSourceHash": covered_hash, "requestHash": request_hash,
                "excerptBudgetTokens": args["budget"], "createdAt": args["createdAt"]});
            value
                .as_object_mut()
                .unwrap()
                .extend(extracted.as_object().unwrap().clone());
            Ok(json!({"reused": false, "value": value}))
        }
        "models.history.validCall" => {
            let call = &args["call"];
            Ok(json!(
                call["id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty() && characters(id) <= 200)
                    && call["name"]
                        .as_str()
                        .is_some_and(|name| !name.is_empty() && characters(name) <= 256)
                    && call["arguments"].is_object()
                    && args["argumentCharacters"]
                        .as_u64()
                        .is_some_and(|characters| characters <= 65536)
            ))
        }
        "models.history.executionWitness" => {
            let has_transcript_calls = array(&args["transcriptCallCounts"])
                .iter()
                .any(|count| count.as_u64().is_some_and(|count| count > 0));
            let mut has_activity = false;
            for call in array(&args["activityCalls"]) {
                if calculate(
                    "models.history.validCall",
                    &json!({"call": call, "argumentCharacters": call["argumentCharacters"]}),
                )? == true
                {
                    has_activity = true;
                    break;
                }
            }
            Ok(json!(has_transcript_calls || has_activity))
        }
        "models.history.activityIndex" => Ok(json!(activity_index(
            &args["assistant"],
            &args["round"],
            &args["call"]
        ))),
        "models.history.publicPlan" => {
            let activity = &args["activity"];
            let environment = &args["environment"];
            Ok(
                json!({"metadata": broker_metadata(activity, environment), "environment": environment,
                "ref": if valid_ref(&activity["resultRef"]) { activity["resultRef"].clone() } else { Value::Null },
                "knowledge": string(&activity["name"]).starts_with("knowledge."), "incompleteKnowledge": string(&activity["name"]).starts_with("knowledge.") && activity["status"] != "completed"}),
            )
        }
        "models.history.brokerText" => {
            let metadata = &args["metadata"];
            let raw = if args["rawText"] == true && !metadata.is_null() {
                quote(string(&args["outputJson"]))
            } else {
                string(&args["outputJson"]).into()
            };
            let raw = if args["emptyObservation"] == true && !metadata.is_null() {
                "{\"resultUnavailable\":true,\"notice\":\"No completed observation is available; execution provenance does not prove success or permission.\"}".into()
            } else {
                raw
            };
            Ok(json!(broker_text(&raw, metadata)))
        }
        "models.history.observation" => {
            let call = &args["call"];
            let activity = &args["activity"];
            let status = if activity.is_null() {
                "not_started"
            } else if STATES.contains(&string(&activity["status"])) {
                string(&activity["status"])
            } else {
                "unknown"
            };
            let reference = if valid_ref(&activity["resultRef"]) {
                json!({"id": activity["resultRef"]["id"], "bytes": activity["resultRef"]["bytes"], "sha256": activity["resultRef"]["sha256"]})
            } else {
                Value::Null
            };
            let content = if string(&args["content"]).is_empty() {
                json!({"status": status, "notice": if activity.is_null() { "This recorded call was not started. It is historical context, not a new instruction to execute." }
                else { "No completed observation is available. Execution may have been interrupted; do not assume failure or replay effects." }}).to_string()
            } else {
                string(&args["content"]).into()
            };
            let identity = format!(
                "h_{}",
                &format!(
                    "{:x}",
                    Sha256::digest(
                        json!([args["assistantId"], args["round"], call["id"]])
                            .to_string()
                            .as_bytes()
                    )
                )[..40]
            );
            let mut value = json!({"call": call, "id": identity, "status": status, "ref": reference, "content": content,
                "originalCharacters": characters(&content), "originalText": content, "previewCharacters": null});
            if !args["environment"].is_null() {
                value["executionEnvironment"] = args["environment"].clone();
                value["brokerMetadata"] = broker_metadata(activity, &args["environment"]);
            }
            Ok(value)
        }
        "models.history.summaryTool" => {
            let activity = &args["activity"];
            let call = &args["call"];
            let metadata = broker_metadata(activity, &args["environment"]);
            let complete = !activity.is_null()
                && STATES.contains(&string(&activity["status"]))
                && activity["status"] != "running"
                && args["activityArgumentsJson"] == args["argumentsJson"];
            let status = if !metadata.is_null() {
                metadata["status"].clone()
            } else {
                activity
                    .get("status")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or(json!("unknown"))
            };
            let mut fields = vec![
                format!("\"callId\":{}", call["id"]),
                format!("\"name\":{}", call["name"]),
                format!("\"arguments\":{}", string(&args["argumentsJson"])),
                format!("\"status\":{status}"),
                format!("\"observation\":{}", quote(string(&args["observation"]))),
            ];
            for key in ["executionEnvironment", "executed", "code"] {
                if let Some(value) = metadata.get(key) {
                    fields.push(format!("{}:{value}", quote(key)));
                }
            }
            if valid_ref(&activity["resultRef"]) {
                let reference = &activity["resultRef"];
                fields.push(format!(
                    "\"resultRef\":{{\"id\":{},\"bytes\":{},\"sha256\":{}}}",
                    reference["id"], reference["bytes"], reference["sha256"]
                ));
            }
            Ok(json!({"complete": complete, "wireTool": format!("{{{}}}", fields.join(","))}))
        }
        "models.history.summarySource" => {
            #[derive(serde::Deserialize)]
            struct Base {
                user: Box<serde_json::value::RawValue>,
                assistant: Box<serde_json::value::RawValue>,
            }
            let base: Base = serde_json::from_str(string(&args["baseSourceJson"]))
                .map_err(|_| "INVALID_SUMMARY_SOURCE")?;
            let complete = ["completed", "error", "interrupted"]
                .contains(&string(&args["assistantStatus"]))
                && array(&args["rounds"]).iter().all(|round| {
                    array(&round["toolCompleteness"])
                        .iter()
                        .all(|complete| *complete == true)
                });
            let rounds = format!(
                "[{}]",
                array(&args["rounds"])
                    .iter()
                    .map(|round| format!(
                        "{{\"round\":{},\"text\":{},\"tools\":[{}]}}",
                        round["round"],
                        round["textJson"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| round["text"].to_string()),
                        array(&round["toolsWire"])
                            .iter()
                            .map(string)
                            .collect::<Vec<_>>()
                            .join(",")
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            Ok(
                json!({"wireSource": format!("{{\"user\":{},\"assistant\":{},\"rounds\":{},\"checkpointComplete\":{complete}}}", base.user.get(), base.assistant.get(), rounds)}),
            )
        }
        "models.history.publicChronology" => {
            let message = &args["message"];
            let rounds: Vec<Value> = array(&args["rounds"]).iter().map(|round| json!({"round": round["round"], "text": round["text"],
                "tools": array(&round["observations"]).iter().map(|item| {
                    let mut tool = json!({"name": item["call"]["name"], "arguments": item["call"]["arguments"], "status": item["status"]});
                    if let Some(metadata) = item["brokerMetadata"].as_object() { tool.as_object_mut().unwrap().extend(metadata.clone()); }
                    if !item["executionEnvironment"].is_null() { tool["executionEnvironment"] = item["executionEnvironment"].clone(); }
                    if !item["ref"].is_null() { tool["resultRef"] = item["ref"].clone(); }
                    tool["observation"] = json!(output_excerpt(string(&item["content"]), 4096)); tool
                }).collect::<Vec<_>>() })).collect();
            let completion = history::task_completion(&message["taskCompletion"]);
            let content = if message["Status"] == "completed" {
                message["contentJson"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| message["Content"].to_string())
            } else {
                quote("")
            };
            Ok(
                json!({"wireText": format!("{{\"messageId\":{},\"status\":{},\"finalText\":{content},\"rounds\":{}{}}}",
                message["Id"], message["Status"], json!(rounds),
                if completion.is_null() { String::new() } else { format!(",\"taskCompletion\":{completion}") })}),
            )
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}

/// One opaque owner manages previews, archive admission, caches, compaction and source tags.
/// 一个不透明拥有者统一管理预览、归档准入、缓存、压缩与回源标签；不改写正式日志。
#[derive(Default)]
pub struct HistoryProjectionState {
    records: Vec<Value>,
    protocol: String,
    available_tools: HashSet<String>,
    input_budget_tokens: f64,
    archive_reads: u64,
    archive_read_bytes: u64,
    unavailable_archives: u64,
    compacted_results: u64,
    compacted_rounds: u64,
    observation_reasons: HashMap<String, String>,
    source_tags: HashMap<u64, Value>,
    next_source_tag: u64,
}

impl HistoryProjectionState {
    fn tag(&mut self, source: Value) -> Result<u64, String> {
        self.next_source_tag = self
            .next_source_tag
            .checked_add(1)
            .ok_or("MODEL_HISTORY_TAG_LIMIT")?;
        self.source_tags.insert(self.next_source_tag, source);
        Ok(self.next_source_tag)
    }
    fn push_entry(
        &mut self,
        messages: &mut Vec<Value>,
        message: Value,
        public_text: &Value,
        extra: Value,
        blocks: Vec<Value>,
        tags: bool,
    ) -> Result<(), String> {
        let mut entry = json!({"message": message});
        if tags {
            let mut metadata = json!({"publicText": public_text, "modelHistory": true});
            metadata
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().cloned().unwrap_or_default());
            entry["sourceTag"] = json!(self.tag(metadata.clone())?);
            if !blocks.is_empty() {
                let ids: Vec<u64> = blocks
                    .into_iter()
                    .map(|extra| {
                        let mut metadata = metadata.clone();
                        metadata
                            .as_object_mut()
                            .unwrap()
                            .extend(extra.as_object().cloned().unwrap_or_default());
                        self.tag(metadata)
                    })
                    .collect::<Result<_, _>>()?;
                entry["blockTags"] = json!(ids);
            }
        }
        messages.push(entry);
        Ok(())
    }
    /// Append a bounded result notice once, without duplicating formal assistant or tool text.
    /// 只追加一次受限结果说明，不复制正式助手正文或工具原文。
    fn append_task_notice(
        &mut self,
        messages: &mut Vec<Value>,
        turn: &Value,
        tags: bool,
    ) -> Result<(), String> {
        let completion = history::task_completion(&turn["taskCompletion"]);
        let notice = history::task_completion_notice(&turn["assistant"]["Id"], &completion);
        if !notice.is_empty() {
            self.push_entry(
                messages,
                json!({"role": "assistant", "content": notice}),
                &turn["assistant"]["Content"],
                json!({"taskCompletion": completion}),
                vec![],
                tags,
            )?;
        }
        Ok(())
    }
    fn project(&mut self, index: usize, tags: bool) -> Result<Value, String> {
        let record = self
            .records
            .get(index)
            .ok_or("MODEL_HISTORY_RECORD_NOT_FOUND")?
            .clone();
        let turn = &record["turn"];
        let mut messages =
            vec![json!({"message": {"role": "user", "content": turn["user"]["Content"]}})];
        raw_field(
            &mut messages[0],
            json!(["content"]),
            &turn["user"]["contentJson"],
        );
        if array(&record["rounds"]).is_empty() {
            self.push_entry(
                &mut messages,
                json!({"role": "assistant", "content": turn["assistant"]["Content"]}),
                &turn["assistant"]["Content"],
                json!({}),
                vec![],
                tags,
            )?;
            raw_field(
                messages.last_mut().unwrap(),
                json!(["content"]),
                &turn["assistant"]["contentJson"],
            );
            if let Some(tag) = messages.last().unwrap()["sourceTag"]
                .as_u64()
                .and_then(|tag| self.source_tags.get_mut(&tag))
            {
                tag["publicTextJson"] = turn["assistant"]["contentJson"].clone();
            }
            self.append_task_notice(&mut messages, turn, tags)?;
            return Ok(json!(messages));
        }
        for (round_index, round) in array(&record["rounds"]).iter().enumerate() {
            let start = messages.len();
            // Only an origin-checked native projection may replay broker context before its original model turn.
            // 仅完整来源已核验的原生投影可在对应模型轮之前恢复程序上下文，不能从正文猜测来源。
            if round["runtimeReplayAllowed"] == true
                && super::transcript::valid_context_before(&round["contextBefore"])
            {
                for contextual in array(&round["contextBefore"]) {
                    self.push_entry(
                        &mut messages,
                        contextual["message"].clone(),
                        &contextual["message"]["content"],
                        contextual["source"].clone(),
                        vec![],
                        tags,
                    )?;
                }
            }
            if !string(&round["compacted"]).is_empty() {
                self.push_entry(
                    &mut messages,
                    json!({"role": "assistant", "content": round["compacted"]}),
                    &turn["assistant"]["Content"],
                    json!({}),
                    vec![],
                    tags,
                )?;
            } else {
                let observations = array(&round["observations"]);
                let calls: Vec<Value> = observations
                    .iter()
                    .map(|item| {
                        let mut call = item["call"].clone();
                        call["id"] = item["id"].clone();
                        call["wireName"] = json!(wire_name(string(&call["name"])));
                        call["available"] =
                            json!(self.available_tools.contains(string(&call["name"])));
                        call["argumentsText"] = call
                            .get("argumentsJson")
                            .cloned()
                            .unwrap_or(json!(call["arguments"].to_string()));
                        call
                    })
                    .collect();
                let results: Vec<Value> = observations.iter().map(|item| json!({"id": item["id"], "name": item["call"]["name"], "status": item["status"], "content": item["content"], "ref": item["ref"]})).collect();
                let metadata: Vec<Value> = observations.iter().map(|item| {
                    let mut metadata = json!({"historicalResult": {"callId": item["id"], "name": item["call"]["name"], "resultRef": item["ref"], "status": item["status"],
                        "originalCharacters": item["originalCharacters"], "originalExcerpt": output_excerpt(string(&item["originalText"]), 4096), "observationCompacted": self.observation_reasons.contains_key(string(&item["id"]))}});
                    if let Some(identity) = item.get("identity") { metadata["historicalResult"]["observationIdentity"] = identity.clone(); } metadata
                }).collect();
                let calls_text = format!(
                    "[{}]",
                    calls
                        .iter()
                        .map(|call| format!(
                            "{{\"id\":{},\"name\":{},\"arguments\":{}}}",
                            call["id"],
                            call["name"],
                            string(&call["argumentsText"])
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                );
                let observations_text = json!(results.iter().map(|item| { let mut value = json!({"toolCallId": item["id"], "name": item["name"], "status": item["status"], "observation": item["content"]}); if !item["ref"].is_null() { value["resultRef"] = item["ref"].clone(); } value }).collect::<Vec<_>>()).to_string();
                let plan = history::calculate(
                    "models.history.projectRound",
                    &json!({"protocol": self.protocol, "assistantMessageId": turn["assistant"]["Id"],
                    "round": {"round": round["round"], "text": round["text"], "calls": calls, "results": results, "callsText": calls_text, "observationsText": observations_text}}),
                )?;
                for entry in array(&plan) {
                    let extra = entry["resultIndex"]
                        .as_u64()
                        .map(|index| metadata[index as usize].clone())
                        .unwrap_or(json!({}));
                    let blocks: Vec<Value> = array(&entry["resultBlockIndexes"])
                        .iter()
                        .filter_map(Value::as_u64)
                        .map(|index| {
                            let mut metadata = metadata[index as usize].clone();
                            metadata["field"] = entry["field"].clone();
                            metadata
                        })
                        .collect();
                    let mut extra = extra;
                    if entry.get("resultIndex").is_some() {
                        extra["field"] = entry["field"].clone();
                    }
                    self.push_entry(
                        &mut messages,
                        entry["message"].clone(),
                        &turn["assistant"]["Content"],
                        extra,
                        blocks,
                        tags,
                    )?;
                    let rendered = messages.last_mut().unwrap();
                    if calls.iter().all(|call| call["available"] == true) {
                        if let Some(index) = entry["resultIndex"].as_u64() {
                            let item = &observations[index as usize];
                            if item["previewCharacters"].is_null()
                                && !self.observation_reasons.contains_key(string(&item["id"]))
                            {
                                raw_field(rendered, json!([entry["field"]]), &item["contentJson"]);
                            }
                        } else if rendered["message"]["role"] == "assistant" {
                            if self.protocol == "anthropic-messages"
                                || self.protocol == "openai-responses"
                            {
                                if rendered["message"]["content"][0]["type"] == "text"
                                    || rendered["message"]["content"][0]["type"] == "output_text"
                                {
                                    raw_field(
                                        rendered,
                                        json!(["content", 0, "text"]),
                                        &round["textJson"],
                                    );
                                }
                            } else {
                                raw_field(rendered, json!(["content"]), &round["textJson"]);
                            }
                        }
                        for (block, index) in array(&entry["resultBlockIndexes"]).iter().enumerate()
                        {
                            let item = &observations[index.as_u64().unwrap() as usize];
                            if item["previewCharacters"].is_null()
                                && !self.observation_reasons.contains_key(string(&item["id"]))
                            {
                                raw_field(
                                    rendered,
                                    json!(["content", block, "content"]),
                                    &item["contentJson"],
                                );
                            }
                        }
                    }
                }
            }
            let values: Vec<Value> = messages[start..]
                .iter()
                .map(|entry| entry["message"].clone())
                .collect();
            self.records[index]["rounds"][round_index]["cost"] =
                json!(tool_message_tokens(&values, ""));
        }
        let last = array(&record["rounds"]).last().unwrap();
        if turn["assistant"]["Status"] != "completed"
            || !array(&last["calls"]).is_empty()
            || last["text"] != turn["assistant"]["Content"]
        {
            self.push_entry(
                &mut messages,
                json!({"role": "assistant", "content": turn["assistant"]["Content"]}),
                &turn["assistant"]["Content"],
                json!({}),
                vec![],
                tags,
            )?;
        }
        if let Some(message) = messages.last_mut() {
            if message["message"]["role"] == "assistant"
                && message["message"]["content"].is_string()
                && message["message"]["content"] == turn["assistant"]["Content"]
            {
                raw_field(
                    message,
                    json!(["content"]),
                    &turn["assistant"]["contentJson"],
                );
            }
        }
        if tags {
            for message in &messages {
                if let Some(tag) = message["sourceTag"]
                    .as_u64()
                    .and_then(|tag| self.source_tags.get_mut(&tag))
                {
                    if tag["runtimeContext"] != true {
                        tag["publicTextJson"] = turn["assistant"]["contentJson"].clone();
                    }
                }
                for tag in array(&message["blockTags"])
                    .iter()
                    .filter_map(Value::as_u64)
                {
                    if let Some(tag) = self.source_tags.get_mut(&tag) {
                        tag["publicTextJson"] = turn["assistant"]["contentJson"].clone();
                    }
                }
            }
        }
        self.append_task_notice(&mut messages, turn, tags)?;
        Ok(json!(messages))
    }
    fn metrics(&self, total: u64) -> Value {
        json!({"compactedResultCount": self.compacted_results, "compactedRoundCount": self.compacted_rounds, "archiveReads": self.archive_reads,
            "estimatedHistoryTokens": total, "observationCompaction": {"duplicateCount": self.observation_reasons.values().filter(|reason| *reason == "duplicate-observation").count(),
                "supersededCount": self.observation_reasons.values().filter(|reason| *reason == "superseded-version").count(),
                "verifiedArchiveCount": self.archive_reads, "unavailableArchiveCount": self.unavailable_archives, "archiveReadBytes": self.archive_read_bytes}})
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "models.lifecycle.history.init" => {
                self.protocol = string(&args["protocol"]).into();
                self.input_budget_tokens = args["inputBudgetTokens"].as_f64().unwrap_or(0.0);
                self.available_tools = array(&args["availableTools"])
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                Ok(Value::Null)
            }
            "models.lifecycle.history.addRecord" => {
                self.records.push(args["record"].clone());
                Ok(json!(self.records.len() - 1))
            }
            "models.lifecycle.history.record" => {
                let mut value = self
                    .records
                    .get(args["recordIndex"].as_u64().unwrap_or(0) as usize)
                    .ok_or("MODEL_HISTORY_RECORD_NOT_FOUND")?;
                if let Some(round) = args["roundIndex"].as_u64() {
                    value = &value["rounds"][round as usize];
                }
                if let Some(observation) = args["observationIndex"].as_u64() {
                    value = &value["observations"][observation as usize];
                }
                if let Some(property) = args["property"].as_str() {
                    value = &value[property];
                }
                Ok(if args["count"] == true {
                    json!(array(value).len())
                } else {
                    value.clone()
                })
            }
            "models.lifecycle.history.tools" => {
                if args["clear"] == true {
                    self.available_tools.clear();
                }
                if let Some(name) = args["add"].as_str() {
                    self.available_tools.insert(name.into());
                }
                if let Some(name) = args["delete"].as_str() {
                    self.available_tools.remove(name);
                }
                Ok(if let Some(name) = args["has"].as_str() {
                    json!(self.available_tools.contains(name))
                } else {
                    json!(self.available_tools)
                })
            }
            "models.lifecycle.history.archivePlan" => {
                let mut remaining = (self.input_budget_tokens * 4.0).floor().max(0.0);
                let metadata_bytes = args["metadataBytes"].as_u64().unwrap_or(0);
                let mut plan = Vec::new();
                for (record_index, record) in self.records.iter().enumerate().rev() {
                    for (round_index, round) in array(&record["rounds"]).iter().enumerate().rev() {
                        for (observation_index, item) in
                            array(&round["observations"]).iter().enumerate().rev()
                        {
                            if item["ref"].is_null() {
                                continue;
                            }
                            let bytes = item["ref"]["bytes"]
                                .as_u64()
                                .unwrap_or(0)
                                .saturating_add(metadata_bytes);
                            if bytes as f64 > remaining {
                                continue;
                            }
                            remaining -= bytes as f64;
                            self.archive_read_bytes += bytes;
                            plan.push(json!({"recordIndex": record_index, "roundIndex": round_index, "observationIndex": observation_index,
                                "reference": item["ref"], "owner": {"requestId": record["turn"]["assistant"]["Id"], "toolCallId": item["call"]["id"], "toolName": item["call"]["name"]}}));
                        }
                    }
                }
                Ok(json!(plan))
            }
            "models.lifecycle.history.archiveOutcome" => {
                let record = args["recordIndex"].as_u64().unwrap_or(0) as usize;
                let round = args["roundIndex"].as_u64().unwrap_or(0) as usize;
                let observation = args["observationIndex"].as_u64().unwrap_or(0) as usize;
                let item = &mut self.records[record]["rounds"][round]["observations"][observation];
                if args["success"] == true {
                    for key in [
                        "content",
                        "contentJson",
                        "executionEnvironment",
                        "brokerMetadata",
                        "identity",
                    ] {
                        if let Some(value) = args.get(key) {
                            item[key] = value.clone();
                        }
                    }
                    item["originalText"] = item["content"].clone();
                    item["originalCharacters"] = json!(characters(string(&item["content"])));
                    self.archive_reads += 1;
                } else {
                    self.unavailable_archives += 1;
                    if args["errorCode"] == "TOOL_RESULT_REFERENCE_MISMATCH" {
                        let invalid = string(&item["ref"]["id"]).to_owned();
                        item["ref"] = Value::Null;
                        item.as_object_mut().unwrap().remove("contentJson");
                        if let Ok(mut content) =
                            serde_json::from_str::<Value>(string(&item["content"]))
                        {
                            strip_reference(&mut content, &invalid);
                            item["content"] = json!(content.to_string());
                        }
                        item["originalText"] = item["content"].clone();
                        item["originalCharacters"] = json!(characters(string(&item["content"])));
                    }
                    if string(&item["content"]).is_empty() {
                        let mut content = json!({"status": item["status"], "resultRef": item["ref"], "resultUnavailable": true,
                            "notice": "Stored result is unavailable; do not replay the operation just to recover its output."});
                        if !item["executionEnvironment"].is_null() {
                            content["executionEnvironment"] = item["executionEnvironment"].clone();
                        }
                        item["content"] = json!(content.to_string());
                    }
                }
                Ok(Value::Null)
            }
            "models.lifecycle.history.archiveMetadata" => {
                let item = &self.records[args["recordIndex"].as_u64().unwrap_or(0) as usize]["rounds"]
                    [args["roundIndex"].as_u64().unwrap_or(0) as usize]["observations"]
                    [args["observationIndex"].as_u64().unwrap_or(0) as usize];
                let environment = args
                    .get("environment")
                    .filter(|value| !value.is_null())
                    .unwrap_or(&item["executionEnvironment"])
                    .clone();
                let mut metadata = item["brokerMetadata"].clone();
                if !metadata.is_null() {
                    metadata["executionEnvironment"] = environment.clone();
                } else if !environment.is_null() {
                    metadata = json!({"executionEnvironment": environment});
                }
                Ok(json!({"environment": environment, "metadata": metadata}))
            }
            "models.lifecycle.history.project" => {
                self.project(args["recordIndex"].as_u64().unwrap_or(0) as usize, true)
            }
            "models.lifecycle.history.source" => Ok(self
                .source_tags
                .get(&args["sourceTag"].as_u64().unwrap_or(0))
                .cloned()
                .unwrap_or(Value::Null)),
            "models.lifecycle.history.releaseTag" => {
                self.source_tags
                    .remove(&args["sourceTag"].as_u64().unwrap_or(0));
                Ok(Value::Null)
            }
            "models.lifecycle.history.sourceProjection" => {
                let patches: Vec<Value> = array(&args["messages"]).iter().map(|message| {
                    let Some(metadata) = message["sourceTag"].as_u64().and_then(|id| self.source_tags.get(&id)) else { return Value::Null; };
                    let mut patch = metadata.clone();
                    if self.protocol == "anthropic-messages" {
                        let blocks: Vec<Value> = array(&message["blockTags"]).iter().enumerate().filter_map(|(block_index, id)| id.as_u64().and_then(|id| self.source_tags.get(&id))
                            .filter(|source| source.get("historicalResult").is_some()).map(|source| json!({"blockIndex": block_index, "source": source}))).collect();
                        if !blocks.is_empty() { patch["historicalResultBlocks"] = json!(blocks); }
                    }
                    patch
                }).collect();
                Ok(json!(patches))
            }
            "models.lifecycle.history.compact" => {
                let mut positions = Vec::new();
                let mut observations = Vec::new();
                for (record_index, record) in self.records.iter().enumerate() {
                    for (round_index, round) in array(&record["rounds"]).iter().enumerate() {
                        for (observation_index, item) in
                            array(&round["observations"]).iter().enumerate()
                        {
                            positions.push((record_index, round_index, observation_index));
                            observations.push(item.clone());
                        }
                    }
                }
                let identities: Vec<Value> = observations.iter().enumerate().map(|(index, item)| json!({"objectIdentity": index, "archiveVerified": item["identity"]["archiveVerified"],
                    "hasResultRef": !item["ref"].is_null(), "targetKey": item["identity"]["targetKey"], "contentFingerprint": item["identity"]["contentFingerprint"],
                    "versionJson": item["identity"]["version"].to_string()})).collect();
                let duplicates = context_control::apply(
                    "context.observationPlan",
                    &json!({"observations": identities}),
                )?;
                for action in array(&duplicates) {
                    let source_index = action["sourceIndex"].as_u64().unwrap() as usize;
                    let replacement_index = action["replacementIndex"].as_u64().unwrap() as usize;
                    let source = &observations[source_index];
                    let replacement = &observations[replacement_index];
                    if self.observation_reasons.contains_key(string(&source["id"])) {
                        continue;
                    }
                    let mut compacted = json!({"contextCompacted": true, "observationCompacted": true, "reason": action["reason"],
                        "toolCallId": source["id"], "tool": source["call"]["name"], "status": source["status"], "target": source["identity"]["target"],
                        "observedVersion": source["identity"]["version"], "page": source["identity"]["page"], "originalCharacters": source["originalCharacters"],
                        "resultRef": source["ref"], "replacedBy": {"toolCallId": replacement["id"], "resultRef": replacement["ref"], "observedVersion": replacement["identity"]["version"]},
                        "navigation": {"tool": "tool.result.read", "arguments": {"id": source["ref"]["id"], "offset": 0, "limit": 4096}},
                        "notice": "Earlier archived read observation; its receipt remains recorded. Use the later verified observation or read this saved result for the old version. Never repeat a completed effect to recover context."});
                    for metadata in [
                        &source["brokerMetadata"],
                        &json!({"executionEnvironment": source["executionEnvironment"]}),
                    ] {
                        if let Some(metadata) = metadata.as_object() {
                            for (key, value) in metadata {
                                if !value.is_null() {
                                    compacted[key] = value.clone();
                                }
                            }
                        }
                    }
                    let text = compacted.to_string();
                    if estimate_tokens(&text) >= estimate_tokens(string(&source["content"])) {
                        continue;
                    }
                    let (record, round, observation) = positions[source_index];
                    let item =
                        &mut self.records[record]["rounds"][round]["observations"][observation];
                    if item["previewCharacters"].is_null() {
                        self.compacted_results += 1;
                    }
                    item["content"] = json!(text);
                    item["previewCharacters"] = json!(0);
                    self.observation_reasons.insert(
                        string(&source["id"]).into(),
                        string(&action["reason"]).into(),
                    );
                }
                let mut costs = Vec::new();
                for index in 0..self.records.len() {
                    let projection = self.project(index, false)?;
                    costs.push(tool_message_tokens(
                        &array(&projection)
                            .iter()
                            .map(|entry| entry["message"].clone())
                            .collect::<Vec<_>>(),
                        "",
                    ));
                }
                let plans: Vec<Value> = self.records.iter().enumerate().map(|(index, record)| json!({"assistantId": record["turn"]["assistant"]["Id"], "cost": costs[index],
                    "rounds": array(&record["rounds"]).iter().map(|round| {
                        let quoted = array(&round["observations"]).iter().any(|item| !self.available_tools.contains(string(&item["call"]["name"])));
                        json!({"round": round["round"], "hasCalls": !array(&round["calls"]).is_empty(), "compacted": !string(&round["compacted"]).is_empty(), "cost": round["cost"],
                            "textExcerpt": output_excerpt(string(&round["text"]), 256), "quotedResults": quoted,
                            "observations": array(&round["observations"]).iter().map(|item| json!({"name": item["call"]["name"], "status": item["status"], "ref": item["ref"],
                                "brokerMetadata": item["brokerMetadata"], "executionEnvironment": item["executionEnvironment"], "originalExcerpt": output_excerpt(string(&item["originalText"]), 4096),
                                "originalCharacters": item["originalCharacters"], "previewCharacters": item["previewCharacters"], "tokens": estimate_tokens(&if quoted { quote(string(&item["content"])) } else { string(&item["content"]).into() })})).collect::<Vec<_>>()})
                    }).collect::<Vec<_>>()})).collect();
                let plan = context::calculate(
                    "models.history.compaction",
                    &json!({"inputBudgetTokens": args["inputBudgetTokens"], "records": plans}),
                )?;
                for action in array(&plan["actions"]) {
                    let record = action["recordIndex"].as_u64().unwrap() as usize;
                    let round = action["roundIndex"].as_u64().unwrap() as usize;
                    if action["type"] == "result" {
                        let item = &mut self.records[record]["rounds"][round]["observations"]
                            [action["observationIndex"].as_u64().unwrap() as usize];
                        if item["previewCharacters"].is_null() {
                            self.compacted_results += 1;
                        }
                        item["content"] = action["text"].clone();
                        item["previewCharacters"] = action["previewCharacters"].clone();
                    } else {
                        self.records[record]["rounds"][round]["compacted"] = action["text"].clone();
                        self.compacted_rounds += 1;
                    }
                }
                let mut total = 0;
                for index in 0..self.records.len() {
                    let projected = self.project(index, false)?;
                    total += tool_message_tokens(
                        &array(&projected)
                            .iter()
                            .map(|entry| entry["message"].clone())
                            .collect::<Vec<_>>(),
                        "",
                    );
                }
                Ok(self.metrics(total))
            }
            "models.lifecycle.history.metrics" => Ok(self.metrics(0)),
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

#[cfg(test)]
mod runtime_context_tests {
    use super::HistoryProjectionState;
    use serde_json::{Value, json};

    fn context(content: &str, hash: &str) -> Value {
        json!({"message":{"role":"user","content":content},"source":{
            "projectionOrigin":"broker-control","runtimeContext":true,"snapshotHash":hash.repeat(64),
            "authorityHash":"c".repeat(64),"publicText":content,"instructionAuthority":false,
            "runtimeKind":"runtime-snapshot"}})
    }

    fn record(allowed: bool) -> Value {
        json!({"turn":{"user":{"Id":"user-original","Content":"Original authorized goal",
                "contentJson":"\"Original authorized goal\""},
            "assistant":{"Id":"assistant-original","Status":"completed","Content":"Delivered answer",
                "contentJson":"\"Delivered answer\""}},
            "rounds":[{"round":1,"text":"Reading source","calls":[{"id":"read-1","name":"filesystem.read","arguments":{}}],
                "runtimeReplayAllowed":allowed,"contextBefore":[context("Runtime before reading", "a")],
                "observations":[{"id":"read-1","call":{"id":"read-1","name":"filesystem.read","arguments":{}},
                    "status":"completed","content":"Original saved observation","originalText":"Original saved observation",
                    "originalCharacters":26}]},
            {"round":2,"text":"Delivered answer","calls":[],"observations":[],
                "runtimeReplayAllowed":allowed,"contextBefore":[context("Updated runtime before delivery", "b")]}]})
    }

    fn state(record: Value, protocol: &str) -> HistoryProjectionState {
        let mut state = HistoryProjectionState::default();
        state.apply("models.lifecycle.history.init", &json!({"protocol":protocol,
            "availableTools":["filesystem.read"]})).unwrap();
        state.apply("models.lifecycle.history.addRecord", &json!({"record":record})).unwrap();
        state
    }

    #[test]
    fn runtime_context_replays_in_original_order_with_broker_provenance_and_intact_tool_pairs() {
        for protocol in ["openai-completions", "anthropic-messages", "openai-responses"] {
            let mut state = state(record(true), protocol);
            let entries = state.apply("models.lifecycle.history.project", &json!({"recordIndex":0})).unwrap();
            let entries = entries.as_array().unwrap();
            assert_eq!(entries[0]["message"]["content"], "Original authorized goal");
            let snapshots = entries.iter().enumerate().filter(|(_, entry)| entry["message"]["content"] == "Runtime before reading"
                || entry["message"]["content"] == "Updated runtime before delivery").collect::<Vec<_>>();
            assert_eq!(snapshots.len(), 2, "{protocol}: {entries:?}");
            assert_eq!(snapshots[0].0, 1);
            assert!(snapshots[1].0 > snapshots[0].0 + 2, "The original tool turn stays between snapshots.");
            for (_, entry) in snapshots {
                let source = state.apply("models.lifecycle.history.source", &json!({"sourceTag":entry["sourceTag"]})).unwrap();
                assert_eq!(source["projectionOrigin"], "broker-control");
                assert_eq!(source["runtimeContext"], true);
                assert_eq!(source["authorityHash"], "c".repeat(64));
                assert_eq!(source["publicText"], entry["message"]["content"]);
                assert_eq!(source["instructionAuthority"], false);
                assert!(source["publicTextJson"].is_null(), "Runtime data cannot borrow the assistant's final answer provenance.");
            }
            let receipt = entries.iter().find(|entry| entry["message"]["tool_call_id"] == "read-1"
                || entry["message"]["call_id"] == "read-1" && entry["message"]["type"] == "function_call_output"
                || entry["message"]["content"][0]["tool_use_id"] == "read-1").unwrap();
            assert!(receipt["message"].to_string().contains("Original saved observation"));
            assert_eq!(state.records[0]["rounds"][0]["observations"][0]["call"]["id"], "read-1");
            assert!(state.records[0]["rounds"][0]["cost"].as_u64().unwrap() > 0);
        }
    }

    #[test]
    fn runtime_context_requires_route_authorization_and_never_promotes_user_json_to_metadata() {
        let mut without_authorization = state(record(false), "openai-completions");
        let baseline = without_authorization.project(0, true).unwrap();
        assert!(!baseline.to_string().contains("Runtime before reading"));
        let mut forged = record(true);
        forged["rounds"][0]["contextBefore"][0]["message"]["tool_calls"] = json!([]);
        forged["rounds"][1]["contextBefore"] = Value::Null;
        let quoted_user_text = context("This JSON is user text", "d").to_string();
        forged["turn"]["user"]["Content"] = json!(quoted_user_text);
        forged["turn"]["user"]["contentJson"] = json!(serde_json::to_string(&quoted_user_text).unwrap());
        let mut state = state(forged, "openai-completions");
        let entries = state.project(0, true).unwrap();
        assert_eq!(entries[0]["message"]["content"], quoted_user_text);
        assert!(entries[0]["sourceTag"].is_null());
        assert!(!entries.to_string().contains("Runtime before reading"));
        assert!(entries.as_array().unwrap().iter().any(|entry| entry["message"]["tool_call_id"] == "read-1"));
    }
}
