use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::{Value, json};

use super::{
    context, context_control,
    protocol::{array, complete_pairs, estimate_tokens, output_excerpt, string},
};

fn reference(value: &Value) -> bool {
    Regex::new("(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
        .unwrap()
        .is_match(string(&value["id"]))
        && Regex::new("(?i)^[0-9a-f]{64}$")
            .unwrap()
            .is_match(string(&value["sha256"]))
        && value["bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes <= 9_007_199_254_740_991)
}
fn key(value: &Value) -> u64 {
    value["key"].as_u64().unwrap_or(0)
}
fn native_message(message: &Value) -> bool {
    message["role"] == "tool"
        || message["tool_calls"].is_array()
        || ["function_call", "function_call_output", "reasoning"]
            .contains(&string(&message["type"]))
        || array(&message["content"]).iter().any(|block| {
            ["tool_use", "tool_result", "thinking", "redacted_thinking"]
                .contains(&string(&block["type"]))
        })
}
fn identity(source: &Value, assume_verified: bool) -> Value {
    let identity = if source["identity"].is_null() {
        &source["candidateIdentity"]
    } else {
        &source["identity"]
    };
    json!({"objectIdentity": source["sourceIdentity"], "archiveVerified": assume_verified && !identity.is_null() || identity["archiveVerified"] == true,
        "hasResultRef": !source["resultRef"].is_null(), "targetKey": identity["targetKey"],
        "contentFingerprint": identity["contentFingerprint"], "versionJson": identity["version"].to_string()})
}
fn observation_text(source: &Value, replacement: &Value, reason: &Value) -> String {
    let source_identity = if source["identity"].is_null() {
        &source["candidateIdentity"]
    } else {
        &source["identity"]
    };
    let replacement_identity = if replacement["identity"].is_null() {
        &replacement["candidateIdentity"]
    } else {
        &replacement["identity"]
    };
    json!({"contextCompacted": true, "observationCompacted": true, "reason": reason,
        "toolCallId": source["callId"], "tool": source["name"], "status": source["status"], "target": source_identity["target"],
        "observedVersion": source_identity["version"], "page": source_identity["page"], "originalCharacters": source["originalCharacters"],
        "resultRef": source["resultRef"], "replacedBy": {"toolCallId": replacement["callId"], "resultRef": replacement["resultRef"], "observedVersion": replacement_identity["version"]},
        "navigation": {"tool": "tool.result.read", "arguments": {"id": source["resultRef"]["id"], "offset": 0, "limit": 4096}},
        "notice": "Earlier archived read observation; its receipt remains recorded. Use the later verified observation or read this saved result for the old version. Never repeat a completed effect to recover context."}).to_string()
}

/// Native state owns projection provenance, archive admission and all compaction decisions.
/// 原生状态拥有投影来源、归档准入与全部压缩决策；JS 只保留 SDK 对象身份和真实 I/O Promise。
#[derive(Default)]
pub struct ToolContextState {
    sequence: u64,
    protocol: String,
    conversation: Value,
    valid_conversation: bool,
    history: HashMap<u64, Value>,
    results: HashMap<u64, Value>,
    receipts: HashMap<String, (String, Option<Value>)>,
    reasons: HashMap<String, String>,
    compacted_results: HashSet<String>,
    compacted_latest: HashSet<String>,
    compacted_turns: HashSet<i64>,
    latest_round: i64,
    latest_history: i64,
    latest_historical_result: i64,
    verified: u64,
    unavailable: u64,
    read_bytes: u64,
}

impl ToolContextState {
    /// Export surviving provenance in current message order after native replacements.
    /// 原生替换完成后，按当前消息顺序导出仍存续的来源，不能沿用压缩前的数组偏移。
    pub fn history_sources(&self, items: &[Value]) -> Vec<Value> {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.history
                    .get(&key(item))
                    .map(|source| {
                        let mut source = source.clone();
                        source["index"] = json!(index);
                        source
                    })
                    .unwrap_or(Value::Null)
            })
            .collect()
    }
    fn allocate(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
    fn metrics(&self) -> Value {
        json!({"duplicateCount": self.reasons.values().filter(|reason| *reason == "duplicate-observation").count(),
            "supersededCount": self.reasons.values().filter(|reason| *reason == "superseded-version").count(),
            "verifiedArchiveCount": self.verified, "unavailableArchiveCount": self.unavailable, "archiveReadBytes": self.read_bytes})
    }
    fn locations(&self, items: &[Value]) -> Vec<Value> {
        let mut locations = Vec::new();
        for (index, message) in items.iter().enumerate() {
            if self.protocol == "anthropic-messages" && message["content"].is_array() {
                for (block_index, block) in array(&message["content"]).iter().enumerate() {
                    if self.results.contains_key(&key(block)) {
                        locations.push(json!({"index": index, "blockIndex": block_index, "key": key(block), "textTokens": block["textTokens"]}));
                    }
                }
            } else if self.results.contains_key(&key(message)) {
                locations.push(json!({"index": index, "key": key(message), "textTokens": message["textTokens"]}));
            }
        }
        locations
    }
    fn register_historical_result(
        &mut self,
        item: &Value,
        metadata: &Value,
        turn: i64,
        round: i64,
    ) {
        let result = &metadata["historicalResult"];
        let field = string(&metadata["field"]);
        let call = item
            .get("tool_call_id")
            .or_else(|| item.get("call_id"))
            .or_else(|| item.get("tool_use_id"));
        if !reference(&result["resultRef"])
            || !["content", "output"].contains(&field)
            || item["field"].as_str() != Some(field)
            || call != Some(&result["callId"])
        {
            return;
        }
        self.latest_round = self.latest_round.max(round);
        self.latest_historical_result = self.latest_historical_result.max(turn);
        self.results.insert(key(item), json!({"sourceIdentity": key(item), "callId": result["callId"], "name": result["name"], "resultRef": result["resultRef"],
            "field": field, "round": round, "historical": true, "turnIndex": turn, "status": result["status"],
            "originalCharacters": result.get("originalCharacters").unwrap_or(&item["characters"]),
            "originalExcerpt": output_excerpt(result["originalExcerpt"].as_str().unwrap_or(string(&item["excerpt"])), 4096),
            "identity": result["observationIdentity"], "observationCompacted": result["observationCompacted"] == true,
            "previewCharacters": if result["observationCompacted"] == true { json!(0) } else { Value::Null }}));
    }
    fn plans(&self, locations: &[Value], assume_verified: bool) -> Result<Vec<Value>, String> {
        let observations: Vec<Value> = locations
            .iter()
            .map(|location| identity(&self.results[&key(location)], assume_verified))
            .collect();
        Ok(array(&context_control::apply(
            "context.observationPlan",
            &json!({"observations": observations}),
        )?)
        .to_vec())
    }
    fn result_action(&mut self, location: &Value, text: &str, metadata: &Value) -> Value {
        let old_key = key(location);
        let new_key = self.allocate();
        let mut source = self.results[&old_key].clone();
        source
            .as_object_mut()
            .unwrap()
            .extend(metadata.as_object().unwrap().clone());
        self.results.insert(new_key, source.clone());
        if let Some(history) = self.history.get(&old_key).cloned() {
            self.history.insert(new_key, history);
        }
        self.compacted_results
            .insert(string(&source["callId"]).into());
        let mut action = json!({"type": "result", "index": location["index"], "key": new_key, "field": source["field"], "text": text});
        if let Some(block) = location.get("blockIndex") {
            action["blockIndex"] = block.clone();
        }
        action
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "models.lifecycle.toolContext.object" => Ok(json!(self.allocate())),
            "models.lifecycle.toolContext.init" => {
                self.protocol = string(&args["protocol"]).into();
                self.conversation = args["conversationId"].clone();
                self.valid_conversation = args["conversationIdValid"] == true;
                self.latest_round = i64::MIN;
                self.latest_history = -1;
                self.latest_historical_result = -1;
                let items = array(&args["items"]);
                let sources = array(&args["sources"]);
                let model_turns: HashSet<Value> = sources
                    .iter()
                    .filter(|source| source["modelHistory"] == true)
                    .map(|source| source["turnIndex"].clone())
                    .collect();
                for (index, item) in items.iter().enumerate() {
                    let Some(source) = sources.get(index) else {
                        continue;
                    };
                    let Some(turn) = source["turnIndex"].as_i64().filter(|turn| *turn >= 0) else {
                        continue;
                    };
                    let role = item["role"].as_str().unwrap_or("assistant");
                    let model_history = model_turns.contains(&source["turnIndex"]);
                    if index + 1 == items.len()
                        || source["messageIdValid"] != true
                        || source["role"] != role
                        || !model_history && (item["contentString"] != true || native_message(item))
                    {
                        continue;
                    }
                    let public_text = if item["role"] == "user"
                        && source["modelHistory"] != true
                        && item["contentString"] == true
                    {
                        string(&item["excerpt"])
                    } else if model_history {
                        string(&source["publicText"])
                    } else {
                        string(&item["excerpt"])
                    };
                    let characters = if model_history
                        && !(item["role"] == "user"
                            && source["modelHistory"] != true
                            && item["contentString"] == true)
                    {
                        public_text.encode_utf16().count() as u64
                    } else {
                        item["characters"].as_u64().unwrap_or(0)
                    };
                    let mut history = source.clone();
                    history["modelHistory"] = json!(model_history);
                    history["originalCharacters"] = json!(characters);
                    history["originalExcerpt"] = json!(output_excerpt(public_text, 768));
                    history["stage"] = json!(0);
                    self.history.insert(key(item), history);
                    self.latest_history = self.latest_history.max(turn);
                    let round = index as i64 - items.len() as i64;
                    self.register_historical_result(item, source, turn, round);
                    for block in array(&source["historicalResultBlocks"]) {
                        if let Some(value) = block["blockIndex"]
                            .as_u64()
                            .and_then(|index| array(&item["content"]).get(index as usize))
                            .filter(|value| value["type"] == "tool_result")
                        {
                            self.register_historical_result(value, &block["source"], turn, round);
                        }
                    }
                }
                Ok(Value::Null)
            }
            "models.lifecycle.toolContext.observe" => {
                let round = args["round"].as_i64().unwrap_or(0);
                self.latest_round = self.latest_round.max(round);
                let result = &args["result"];
                if !reference(&result["resultRef"]) {
                    return Ok(Value::Null);
                }
                let mut source = args["source"].clone();
                source["sourceIdentity"] = args["key"].clone();
                source["round"] = json!(round);
                source["resultRef"] = result["resultRef"].clone();
                source["previewCharacters"] = Value::Null;
                self.results
                    .insert(args["key"].as_u64().unwrap_or(0), source);
                Ok(Value::Null)
            }
            "models.lifecycle.toolContext.alias" => {
                let old = args["oldKey"].as_u64().unwrap_or(0);
                let new = args["key"].as_u64().unwrap_or(0);
                if let Some(history) = self.history.get(&old).cloned() {
                    self.history.insert(new, history);
                }
                Ok(Value::Null)
            }
            "models.lifecycle.toolContext.prepare" => {
                let locations = self.locations(array(&args["items"]));
                let plans = self.plans(&locations, true)?;
                let mut needed = HashSet::new();
                for plan in plans {
                    let index = plan["sourceIndex"].as_u64().unwrap() as usize;
                    let replacement = plan["replacementIndex"].as_u64().unwrap() as usize;
                    let source = &self.results[&key(&locations[index])];
                    if source["observationCompacted"] != true
                        && estimate_tokens(&observation_text(
                            source,
                            &self.results[&key(&locations[replacement])],
                            &plan["reason"],
                        )) < locations[index]["textTokens"].as_u64().unwrap_or(0)
                    {
                        needed.insert(key(&locations[index]));
                        needed.insert(key(&locations[replacement]));
                    }
                }
                let mut remaining = (args["inputBudgetTokens"].as_f64().unwrap_or(0.0) * 4.0)
                    .floor()
                    .max(0.0) as u64;
                remaining = remaining.saturating_sub(self.read_bytes);
                let mut commands = Vec::new();
                for location in locations.iter().rev() {
                    let id = key(location);
                    let source = self.results.get_mut(&id).unwrap();
                    if !needed.contains(&id)
                        || source["identity"]["archiveVerified"] == true
                        || source["archiveChecked"] == true
                        || source["historical"] == true
                        || string(&source["archiveOwner"]["requestId"]).is_empty()
                    {
                        continue;
                    }
                    let reference_id = string(&source["resultRef"]["id"]).to_owned();
                    let receipt_key =
                        json!([source["resultRef"], source["archiveOwner"]]).to_string();
                    if let Some((previous, identity)) = self.receipts.get(&reference_id) {
                        source["archiveChecked"] = json!(true);
                        if previous == &receipt_key {
                            if let Some(identity) = identity {
                                source["identity"] = identity.clone();
                            } else {
                                commands.push(
                                    json!({"type": "wait", "key": id, "receiptId": reference_id}),
                                );
                            }
                        } else {
                            self.unavailable += 1;
                        }
                        continue;
                    }
                    let read_bytes = source["resultRef"]["bytes"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_add(args["metadataBytes"].as_u64().unwrap_or(0));
                    if read_bytes > remaining {
                        continue;
                    }
                    remaining -= read_bytes;
                    self.read_bytes += read_bytes;
                    source["archiveChecked"] = json!(true);
                    commands.push(json!({"type": "read", "key": id, "receiptId": reference_id, "reference": source["resultRef"], "owner": source["archiveOwner"], "name": source["name"], "status": source["status"]}));
                    self.receipts.insert(reference_id, (receipt_key, None));
                }
                Ok(json!(commands))
            }
            "models.lifecycle.toolContext.verified" => {
                let id = args["key"].as_u64().unwrap_or(0);
                let identity = args["identity"].clone();
                if let Some(source) = self.results.get_mut(&id) {
                    source["identity"] = identity.clone();
                }
                if args["read"] == true {
                    if args["failed"] == true {
                        self.unavailable += 1;
                    } else {
                        self.verified += 1;
                    }
                    if let Some((_, previous)) = self.receipts.get_mut(string(&args["receiptId"])) {
                        *previous = Some(identity);
                    }
                }
                Ok(Value::Null)
            }
            "models.lifecycle.toolContext.observations" => {
                let items = array(&args["items"]);
                if !complete_pairs(items) {
                    return Ok(json!([]));
                }
                let locations = self.locations(items);
                let plans = self.plans(&locations, false)?;
                let mut actions = Vec::new();
                for plan in plans {
                    let location = &locations[plan["sourceIndex"].as_u64().unwrap() as usize];
                    let source = self.results[&key(location)].clone();
                    if source["observationCompacted"] == true {
                        continue;
                    }
                    let replacement = &self.results
                        [&key(&locations[plan["replacementIndex"].as_u64().unwrap() as usize])];
                    let text = observation_text(&source, replacement, &plan["reason"]);
                    if estimate_tokens(&text) >= location["textTokens"].as_u64().unwrap_or(0) {
                        continue;
                    }
                    actions.push(self.result_action(
                        location,
                        &text,
                        &json!({"previewCharacters": 0, "observationCompacted": true}),
                    ));
                    self.reasons.insert(
                        string(&source["callId"]).into(),
                        string(&plan["reason"]).into(),
                    );
                }
                Ok(json!(actions))
            }
            "models.lifecycle.toolContext.compaction" => {
                let items = array(&args["items"]);
                let mut groups: HashMap<i64, Vec<Value>> = HashMap::new();
                for item in items {
                    if let Some(turn) = self
                        .history
                        .get(&key(item))
                        .and_then(|history| history["turnIndex"].as_i64())
                    {
                        groups.entry(turn).or_default().push(item.clone());
                    }
                }
                let mut descriptors = Vec::new();
                let locations = self.locations(items);
                for (index, item) in items.iter().enumerate() {
                    let mut history = self.history.get(&key(item)).cloned().unwrap_or(Value::Null);
                    if !history.is_null() {
                        history["contentTokens"] = item["contentTokens"].clone();
                    }
                    let results: Vec<Value> = locations.iter().filter(|location| location["index"] == index).map(|location| {
                        let mut result = json!({"source": self.results[&key(location)], "textTokens": location["textTokens"]});
                        if let Some(block) = location.get("blockIndex") { result["blockIndex"] = block.clone(); } result
                    }).collect();
                    descriptors.push(
                        json!({"cost": item["cost"], "history": history, "results": results}),
                    );
                }
                let groups: Vec<Value> = groups.into_iter().map(|(turn, messages)| json!({"turnIndex": turn, "completePairs": complete_pairs(&messages)})).collect();
                let calculated = context::calculate(
                    "models.context.compaction",
                    &json!({"items": descriptors, "groups": groups,
                    "beforeTokens": args["tokens"], "inputBudgetTokens": args["inputBudgetTokens"], "conversationId": self.conversation,
                    "canCompactHistory": self.valid_conversation, "latestResultRound": self.latest_round,
                    "latestHistoryTurn": self.latest_history, "latestHistoricalResultTurn": self.latest_historical_result}),
                )?;
                let original = items.to_vec();
                let mut projected = original.clone();
                let mut actions = Vec::new();
                for action in array(&calculated["actions"]) {
                    let index = action["index"].as_u64().unwrap() as usize;
                    if action["type"] == "result" {
                        let block = action["blockIndex"].as_u64().map(|block| block as usize);
                        let old_key = block.map_or(key(&projected[index]), |block| {
                            key(&projected[index]["content"][block])
                        });
                        let source = self.results[&old_key].clone();
                        let location = if let Some(block) = block {
                            json!({"index": index, "blockIndex": block, "key": old_key})
                        } else {
                            json!({"index": index, "key": old_key})
                        };
                        let replacement = self.result_action(
                            &location,
                            string(&action["text"]),
                            &json!({"previewCharacters": action["previewCharacters"]}),
                        );
                        if action["latest"] == true {
                            self.compacted_latest
                                .insert(string(&source["callId"]).into());
                        }
                        if let Some(block) = block {
                            projected[index]["content"][block]["key"] = replacement["key"].clone();
                        } else {
                            projected[index]["key"] = replacement["key"].clone();
                        }
                        actions.push(replacement);
                    } else {
                        let mut replacement_items = Vec::new();
                        let mut replacements = Vec::new();
                        for replacement in array(&action["replacements"]) {
                            let original_index =
                                replacement["originalIndex"].as_u64().unwrap() as usize;
                            let mut source = self.history[&key(&original[original_index])].clone();
                            source["stage"] = replacement["stage"].clone();
                            if replacement["modelHistory"] == true {
                                source["originalMessageCount"] =
                                    replacement["originalMessageCount"].clone();
                            }
                            let new_key = self.allocate();
                            self.history.insert(new_key, source.clone());
                            let mut output = replacement.clone();
                            output["key"] = json!(new_key);
                            output["role"] = source["role"].clone();
                            replacements.push(output);
                            let mut item = original[original_index].clone();
                            item["key"] = json!(new_key);
                            replacement_items.push(item);
                        }
                        let count = action["count"].as_u64().unwrap() as usize;
                        projected.splice(index..index + count, replacement_items);
                        let mut action = action.clone();
                        action["replacements"] = json!(replacements);
                        self.compacted_turns
                            .insert(action["turnIndex"].as_i64().unwrap_or(-1));
                        actions.push(action);
                    }
                }
                Ok(json!(actions))
            }
            "models.lifecycle.toolContext.finalize" => {
                let hard = args["inputBudgetTokens"]
                    .as_f64()
                    .map(|value| value.floor().max(0.0));
                let after = args["tokens"].as_u64().unwrap_or(0);
                let before = args["beforeTokens"].as_u64().unwrap_or(0);
                if hard.is_some_and(|hard| after as f64 > hard) {
                    return Err("TOOL_CONTEXT_BUDGET_EXCEEDED".into());
                }
                Ok(if args["changed"] == true || self.read_bytes > 0 {
                    json!({"beforeTokens": before, "estimatedInputTokens": after,
                    "inputBudgetTokens": hard, "schemaTokens": args["schemaTokens"], "reducedTokens": before.saturating_sub(after),
                    "compactedToolResultCount": self.compacted_results.len(), "compactedLatestResultCount": self.compacted_latest.len(),
                    "compactedHistoryTurnCount": self.compacted_turns.len(), "observationCompaction": self.metrics()})
                } else {
                    Value::Null
                })
            }
            "models.lifecycle.toolContext.metrics" => Ok(self.metrics()),
            "models.lifecycle.toolContext.checkpointPlan" => {
                let items = array(&args["items"]);
                if !complete_pairs(items) {
                    return Ok(Value::Null);
                }
                let mut public = Vec::new();
                let mut retained = Vec::new();
                let mut latest_text = None;
                for (index, item) in items.iter().enumerate() {
                    if !self.history.contains_key(&key(item))
                        && item["role"] == "user"
                        && !native_message(item)
                    {
                        retained.push(index);
                    }
                    if item["type"] == "reasoning" {
                        continue;
                    }
                    let fields = if item["type"] == "function_call" {
                        json!(["type", "call_id", "name", "arguments"])
                    } else if item["type"] == "function_call_output" {
                        json!(["type", "call_id", "output"])
                    } else {
                        let mut fields = vec!["role", "content"];
                        if !item["tool_calls"].is_null() {
                            fields.push("tool_calls");
                        }
                        if !string(&item["tool_call_id"]).is_empty() {
                            fields.push("tool_call_id");
                        }
                        json!(fields)
                    };
                    let mut recipe = json!({"index": index, "fields": fields});
                    if item["content"].is_array() {
                        recipe["blocks"] = json!(
                            array(&item["content"])
                                .iter()
                                .enumerate()
                                .filter(|(_, block)| ["text", "tool_use", "tool_result"]
                                    .contains(&string(&block["type"])))
                                .map(|(index, _)| index)
                                .collect::<Vec<_>>()
                        );
                    }
                    if item["role"] == "assistant" && item["contentString"] == true {
                        latest_text = Some(index);
                    }
                    public.push(recipe);
                }
                let receipts: Vec<Value> = self.locations(items).iter().map(|location| { let source = &self.results[&key(location)]; json!({"id": source["callId"], "tool": source["name"], "status": source["status"], "resultRef": source["resultRef"]}) }).collect();
                Ok(
                    json!({"public": public, "retained": retained, "latestTextIndex": latest_text, "receipts": receipts}),
                )
            }
            "models.lifecycle.toolContext.checkpointFinalize" => {
                let plan = &args["plan"];
                let receipts = array(&plan["receipts"]);
                let reference = &args["resultRef"];
                let content = json!({"contextCheckpoint": true, "settledToolBoundary": true, "completedRecordCount": receipts.len(),
                    "recentReceipts": &receipts[receipts.len().saturating_sub(8)..], "resultRef": reference,
                    "navigation": {"tool": "tool.result.read", "arguments": {"id": reference["id"], "offset": 0, "limit": 4096}},
                    "recentPublicText": output_excerpt(string(&args["latestPublicText"]), 512),
                    "notice": "A fresh inference window follows a settled tool boundary. Continue the original task. Before further effects or conclusions relying on omitted history, read the saved public records to recover the relevant original goal, user constraints and evidence; do not guess them. Do not replay completed or unconfirmed operations. A receipt is not proof of task correctness; verify current source versions before relying on it."}).to_string();
                Ok(
                    json!({"message": {"role": "assistant", "content": content}, "metrics": {"currentStepCheckpoint": true,
                    "archivedMessageCount": array(&plan["public"]).len(), "receiptCount": receipts.len(), "resultRef": reference,
                    "opaqueReasoningExposed": false, "toolOperationsReplayed": 0}}),
                )
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
