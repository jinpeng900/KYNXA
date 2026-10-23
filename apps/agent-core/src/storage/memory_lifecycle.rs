use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::{ContractError, ContractResult, memory};

fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_MEMORY", 400)
}

/// Manage candidate lifecycle here; keep asynchronous work and quoted bytes with the caller.
/// 本模块管理候选生命周期，异步工作及原话字节由调用方持有。
#[derive(Default)]
pub struct MemoryLifecycleState {
    settings: Value,
    settings_override: bool,
    settings_revision: u64,
    pending: HashMap<String, u64>,
    sequence: u64,
    last_result: Value,
}

impl MemoryLifecycleState {
    fn calculate(&mut self, operation: &str, args: &Value) -> ContractResult {
        match operation {
            "storage.memoryLifecycle.init" => {
                self.settings = args["settings"].clone();
                self.settings_override = args["override"] == true;
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.configure" => {
                self.settings = memory::apply(
                    "storage.memory.settings",
                    &json!({"value":args.get("settings").unwrap_or(&json!({})),"previous":self.settings}),
                )?;
                self.settings_override = true;
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.initialize" => {
                if !self.settings_override {
                    if let Some(settings) = args["document"].get("candidateSettings") {
                        self.settings =
                            memory::apply("storage.memory.settings", &json!({"value":settings}))?;
                    }
                }
                self.settings_revision = args["document"]["candidateSettingsRevision"]
                    .as_u64()
                    .unwrap_or(0);
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.settingsInput" => {
                let input = &args["input"];
                let keys = args["inputKeys"].as_array();
                if !input.is_object()
                    || !keys.is_some_and(|keys| {
                        keys.iter().any(|key| key == "patch")
                            && keys.iter().all(|key| {
                                key.as_str()
                                    .is_some_and(|key| ["patch", "expectedRevision"].contains(&key))
                            })
                    })
                {
                    return Err(invalid("自动候选配置请求无效。"));
                }
                memory::apply(
                    "storage.memory.revision",
                    &json!({"value":input["expectedRevision"],"required":true}),
                )
            }
            "storage.memoryLifecycle.mutateSettings" => {
                let document = &args["document"];
                let expected = args["expected"]
                    .as_u64()
                    .ok_or_else(|| invalid("候选配置版本无效。"))?;
                if document["candidateSettingsRevision"].as_u64().unwrap_or(0) != expected {
                    return Err(ContractError::new(
                        "候选配置已更新，请重新读取。",
                        "MEMORY_CONFLICT",
                        409,
                    ));
                }
                let settings = memory::apply(
                    "storage.memory.settings",
                    &json!({"value":args.get("patch").unwrap_or(&json!({})),"previous":document.get("candidateSettings").unwrap_or(&self.settings)}),
                )?;
                Ok(json!({"candidateSettings":settings,"candidateSettingsRevision":expected+1}))
            }
            "storage.memoryLifecycle.settingsSaved" => {
                self.settings = args["document"]["candidateSettings"].clone();
                self.settings_revision = args["document"]["candidateSettingsRevision"]
                    .as_u64()
                    .unwrap_or(0);
                self.settings_override = false;
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.settings" => Ok(self.settings.clone()),
            "storage.memoryLifecycle.status" => {
                let mut result = json!({"settings":self.settings,"settingsRevision":self.settings_revision,"pendingTasks":self.pending.len(),"algorithm":"conservative-extractive-v1","lastResult":self.last_result});
                if let Some(error) = args.get("changeNotificationError").filter(|v| !v.is_null()) {
                    result["changeNotificationError"] = error.clone();
                }
                Ok(result)
            }
            "storage.memoryLifecycle.schedule" => {
                let id = args["id"]
                    .as_str()
                    .ok_or_else(|| invalid("候选任务身份无效。"))?;
                if self.settings["enabled"] != true {
                    return Ok(
                        json!({"admitted":false,"result":{"created":0,"reason":"disabled"}}),
                    );
                }
                if !self.pending.contains_key(id) && self.pending.len() >= 128 {
                    return Ok(
                        json!({"admitted":false,"result":{"created":0,"reason":"queue-capacity"}}),
                    );
                }
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid("候选任务计数超限。"))?;
                self.pending.insert(id.into(), self.sequence);
                Ok(json!({"admitted":true,"sequence":self.sequence}))
            }
            "storage.memoryLifecycle.settled" => {
                self.last_result = if args["failed"] == true {
                    json!({"created":0,"reason":"failed","code":args.get("code").unwrap_or(&json!("MEMORY_CANDIDATE_FAILED"))})
                } else {
                    args["result"].clone()
                };
                let id = args["id"].as_str().unwrap_or("");
                let sequence = args["sequence"].as_u64();
                if self.pending.get(id).copied() == sequence {
                    self.pending.remove(id);
                    Ok(json!(true))
                } else {
                    Ok(json!(false))
                }
            }
            "storage.memoryLifecycle.capturePlan" => {
                let conversation = &args["conversationId"];
                let mut consumed = HashSet::new();
                let mut checkpoints = HashSet::new();
                for document in args["documents"].as_array().into_iter().flatten() {
                    for entry in document["entries"].as_array().into_iter().flatten() {
                        if entry["source"]["conversationId"] == *conversation {
                            if let Some(id) = entry["source"]["messageId"].as_str() {
                                consumed.insert(id.to_owned());
                            }
                            let quotes = entry["candidate"]
                                .get("quotes")
                                .or_else(|| entry["proposal"].get("quotes"));
                            for quote in quotes.and_then(Value::as_array).into_iter().flatten() {
                                if let Some(id) = quote["messageId"].as_str() {
                                    consumed.insert(id.to_owned());
                                }
                            }
                        }
                        if document["scope"] == "chat" {
                            if let Some(id) = entry["candidate"]["batchThroughMessageId"].as_str() {
                                checkpoints.insert(id.to_owned());
                            }
                        }
                    }
                    for source in document["dismissedSources"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if source["conversationId"] == *conversation {
                            if let Some(id) = source["messageId"].as_str() {
                                consumed.insert(id.to_owned());
                            }
                        }
                        if document["scope"] == "chat" {
                            if let Some(id) = source["candidateBatchThroughMessageId"].as_str() {
                                checkpoints.insert(id.to_owned());
                            }
                        }
                    }
                }
                let after = args["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .rev()
                    .find(|message| {
                        message["role"] == "user"
                            && checkpoints.contains(message["id"].as_str().unwrap_or(""))
                    })
                    .map(|message| message["id"].clone());
                Ok(
                    json!({"consumedMessageIds":consumed.into_iter().collect::<Vec<_>>(),"afterMessageId":after,"hasProject":args["hasProject"]==true&&args["documents"].as_array().is_some_and(|documents|documents.iter().any(|document|document["scope"]=="project"))}),
                )
            }
            "storage.memoryLifecycle.quoteCheck" => {
                let users = &args["usersRaw"];
                let candidate = &args["candidate"];
                if args["quotesRaw"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|quote| {
                        users.get(quote["messageId"].as_str().unwrap_or(""))
                            != Some(&quote["textRaw"])
                    })
                {
                    return Err(invalid("候选只能引用正式保存的完整用户原话。"));
                }
                if let Some(cursor) = candidate
                    .get("batchThroughMessageId")
                    .filter(|v| !v.is_null() && v.as_str() != Some(""))
                {
                    if users.get(cursor.as_str().unwrap_or("")).is_none() {
                        return Err(invalid("完整轮次游标缺少真实用户消息。"));
                    }
                }
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.scopeInput" => {
                let input = &args["input"];
                let scope = &args["scope"];
                if !input.is_object() {
                    return Err(invalid("记忆输入无效。"));
                }
                memory::apply("storage.memory.scope", &json!({"value":scope}))?;
                if scope == "chat" {
                    return Err(invalid("独立记忆管理仅支持工作和用户作用域。"));
                }
                if input.get("scope").is_some_and(|value| value != scope) {
                    return Err(invalid("记忆作用域须与接口一致。"));
                }
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.createInput" => {
                let input = &args["input"];
                if !input.is_object() && !input.is_array() {
                    return Err(invalid("记忆输入无效。"));
                }
                memory::apply("storage.memory.scope", &json!({"value":input["scope"]}))?;
                if input.get("candidate").is_some()
                    || input.get("proposal").is_some()
                    || input
                        .get("status")
                        .is_some_and(|status| status != "confirmed")
                {
                    return Err(invalid(
                        "自动候选由正式用户消息提取；手动创建只接受用户确认记忆。",
                    ));
                }
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.objectInput" => {
                if !args["input"].is_object() && !args["input"].is_array() {
                    return Err(invalid("记忆输入无效。"));
                }
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.manualSource" => {
                if args["source"]["type"] != "manual" {
                    return Err(invalid("独立记忆管理只能创建手动确认记忆。"));
                }
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.sourceOwner" => {
                let mut source = args["source"].clone();
                if source["type"] == "manual" && source.get("conversationId").is_none() {
                    source["conversationId"] = args["conversationId"].clone();
                }
                if source["conversationId"] != args["conversationId"] {
                    return Err(invalid("记忆来源须属于当前聊天。"));
                }
                Ok(source)
            }
            "storage.memoryLifecycle.sourceMessage" => {
                if !args["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|message| message["role"] == "user" && message["id"] == args["messageId"])
                {
                    return Err(invalid("记忆来源须为当前聊天中已保存的真实用户消息。"));
                }
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.proposalScope" => {
                let scope = &args["scope"];
                let document = &args["document"];
                let input = &args["input"];
                if document.is_null()
                    || input
                        .get("scopeId")
                        .is_some_and(|id| id != &document["scopeId"])
                {
                    return Err(ContractError::new(
                        "模型建议只能归属当前聊天可见的记忆范围。",
                        "MEMORY_SCOPE_CHANGED",
                        409,
                    ));
                }
                let target = &args["target"];
                if !target.is_null()
                    && (target["scope"] != *scope || target["scopeId"] != document["scopeId"])
                {
                    return Err(ContractError::new(
                        "目标不能跨越记忆作用域。",
                        "MEMORY_SCOPE_CHANGED",
                        409,
                    ));
                }
                Ok(
                    json!({"targetIndex":document["entries"].as_array().into_iter().flatten().position(|entry|entry["id"]==target["id"])}),
                )
            }
            "storage.memoryLifecycle.contextSelection" => Ok(json!(
                args["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter(|(_, entry)| entry["status"] == "confirmed" && entry["active"] == true)
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            )),
            "storage.memoryLifecycle.snapshotSelection" => {
                let ids: HashSet<&str> = args["includedIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let entries = args["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter(|(_, entry)| ids.contains(entry["id"].as_str().unwrap_or("")))
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                let projection = args["projection"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter(|(_, entry)| ids.contains(entry["memoryId"].as_str().unwrap_or("")))
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                Ok(json!({"entries":entries,"projection":projection}))
            }
            "storage.memoryLifecycle.explicitMessage" => {
                if !args["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|message| {
                        message["role"] == "user"
                            && message["id"] == args["messageId"]
                            && message["contentRaw"] == args["contentRaw"]
                    })
                {
                    return Err(invalid("只能从已保存的真实用户消息提取记忆。"));
                }
                Ok(Value::Null)
            }
            "storage.memoryLifecycle.proposalInput" => {
                if !args["input"].is_object()
                    || !args["inputKeys"].as_array().is_some_and(|keys| {
                        keys.iter().all(|key| {
                            key.as_str().is_some_and(|key| {
                                [
                                    "action",
                                    "scope",
                                    "scopeId",
                                    "content",
                                    "kind",
                                    "quotes",
                                    "reason",
                                    "isInference",
                                    "conflicts",
                                    "target",
                                ]
                                .contains(&key)
                            })
                        })
                    })
                    || !args["input"]["action"]
                        .as_str()
                        .is_some_and(|action| ["add", "update", "delete", "noop"].contains(&action))
                {
                    return Err(invalid("模型记忆建议动作无效。"));
                }
                Ok(Value::Null)
            }
            _ => Err(ContractError::new(
                "Unknown memory lifecycle action.",
                "AGENT_CORE_UNKNOWN_OPERATION",
                400,
            )),
        }
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        Ok(match self.calculate(operation, args) {
            Ok(value) => json!({"value":value}),
            Err(error) => {
                json!({"error":{"code":error.code,"message":error.message,"statusCode":error.status_code}})
            }
        })
    }
}
