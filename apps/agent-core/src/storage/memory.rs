use std::collections::HashSet;

use serde_json::{Value, json};

use super::{
    ContractError, ContractResult, MAX_SAFE_INTEGER, hash, identity, keys, member, object,
    safe_integer, trim, utf16_length,
};

const CANDIDATE_ALGORITHM: &str = "conservative-extractive-v1";
const PROPOSAL_ALGORITHM: &str = "main-model-tool-v1";
const TRIGGERS: &[&str] = &[
    "remember",
    "correction",
    "decision",
    "task-complete",
    "ordinary-batch",
];

fn failure(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_MEMORY", 400)
}
fn corrupt(message: &str) -> ContractError {
    ContractError::new(message, "CORRUPT_MEMORY", 500)
}
fn record<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a serde_json::Map<String, Value>, ContractError> {
    object(
        value,
        &format!("{label}格式无效，原文件已保留。"),
        "CORRUPT_MEMORY",
        500,
    )
}
fn valid_text(value: &Value, maximum: usize) -> bool {
    value.as_str().is_some_and(|text| {
        !trim(text).is_empty() && utf16_length(text) <= maximum && !text.contains('\0')
    })
}
fn scope(value: &Value) -> ContractResult {
    if !member(value, &["chat", "project", "user"]) {
        return Err(failure("记忆作用域无效。"));
    }
    Ok(value.clone())
}
fn memory_id(value: &Value) -> ContractResult {
    identity(value).map(|value| json!(value.as_str().unwrap().to_ascii_lowercase()))
}
fn content(value: &Value) -> ContractResult {
    if !valid_text(value, 4000) {
        return Err(failure("记忆内容须为 1–4000 个字符。"));
    }
    Ok(json!(trim(value.as_str().unwrap())))
}
fn kind(value: Option<&Value>, scope: &Value) -> ContractResult {
    let value = value
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| {
            json!(if scope == "user" {
                "preference"
            } else {
                "fact"
            })
        });
    if !member(&value, &["fact", "preference", "decision"]) {
        return Err(failure("记忆类型无效。"));
    }
    Ok(value)
}
fn revision(value: Option<&Value>, required: bool) -> ContractResult {
    let Some(value) = value else {
        return if required {
            Err(failure("请提供有效的记忆版本 expectedRevision。"))
        } else {
            Ok(Value::Null)
        };
    };
    if !safe_integer(value, 0, MAX_SAFE_INTEGER) {
        return Err(failure("请提供有效的记忆版本 expectedRevision。"));
    }
    Ok(value.clone())
}
fn status(value: Option<&Value>) -> ContractResult {
    let value = value.cloned().unwrap_or_else(|| json!("confirmed"));
    if !member(&value, &["draft", "confirmed"]) {
        return Err(failure("记忆状态无效。"));
    }
    Ok(value)
}
fn settings(value: &Value, previous: &Value) -> ContractResult {
    let allowed = [
        "enabled",
        "minTurns",
        "maxTurns",
        "minTokens",
        "maxTokens",
        "maxCandidates",
    ];
    if !keys(value, &allowed) {
        return Err(failure("自动记忆候选设置无效。"));
    }
    let mut result = previous.as_object().cloned().unwrap_or_default();
    result.extend(value.as_object().unwrap().clone());
    let result = Value::Object(result);
    if !result["enabled"].is_boolean()
        || !safe_integer(&result["minTurns"], 6, 12)
        || !safe_integer(
            &result["maxTurns"],
            result["minTurns"].as_u64().unwrap_or(13),
            12,
        )
        || !safe_integer(&result["minTokens"], 2048, 4096)
        || !safe_integer(
            &result["maxTokens"],
            result["minTokens"].as_u64().unwrap_or(4097),
            4096,
        )
        || !safe_integer(&result["maxCandidates"], 1, 8)
    {
        return Err(failure(
            "自动候选须使用 6–12 完整轮次、2K–4K 估算 token 和最多 8 个候选。",
        ));
    }
    Ok(result)
}
fn defaults() -> Value {
    json!({"enabled": true, "minTurns": 6, "maxTurns": 12, "minTokens": 2048, "maxTokens": 4096, "maxCandidates": 8})
}
fn source(value: &Value) -> ContractResult {
    record(value, "记忆来源")?;
    let source_type = value
        .get("type")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| {
            json!(
                if value.get("messageId").is_some_and(|value| !value.is_null()) {
                    "user-message"
                } else {
                    "manual"
                }
            )
        });
    let role = value
        .get("role")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| json!("user"));
    if !member(&source_type, &["user-message", "manual"]) || role != "user" {
        return Err(failure("记忆须由用户明确确认，来源格式无效。"));
    }
    let mut result = json!({"type": source_type, "role": "user"});
    if source_type == "user-message"
        || value
            .get("conversationId")
            .is_some_and(|value| !value.is_null())
    {
        result["conversationId"] = memory_id(&value["conversationId"])?;
    }
    if source_type == "user-message" {
        result["messageId"] = memory_id(&value["messageId"])?;
    } else if value.get("messageId").is_some_and(|value| !value.is_null()) {
        return Err(failure("手动记忆不能伪造消息来源。"));
    }
    Ok(result)
}
fn target(value: &Value) -> ContractResult {
    if !keys(
        value,
        &[
            "id",
            "scope",
            "scopeId",
            "expectedRevision",
            "entryRevision",
            "identity",
        ],
    ) || !safe_integer(&value["entryRevision"], 1, MAX_SAFE_INTEGER)
        || !hash(&value["identity"])
    {
        return Err(failure("修改建议须绑定已读取的完整目标身份。"));
    }
    let scope = scope(&value["scope"])?;
    if scope == "user" && value["scopeId"] != "user" {
        return Err(failure("全局记忆范围身份无效。"));
    }
    Ok(json!({"id": memory_id(&value["id"])?, "scope": scope,
        "scopeId": if scope == "user" { json!("user") } else { memory_id(&value["scopeId"])? },
        "expectedRevision": revision(value.get("expectedRevision"), true)?, "entryRevision": value["entryRevision"], "identity": value["identity"]}))
}
fn timestamp(value: &Value, timestamps: &Value) -> ContractResult {
    if !value.is_string()
        || timestamps
            .get(value.as_str().unwrap_or(""))
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(corrupt("记忆时间格式无效，原文件已保留。"));
    }
    Ok(value.clone())
}
fn quotes(value: &Value, source: &Value, proposal: bool) -> ContractResult {
    let mut ids = HashSet::new();
    let mut result = Vec::new();
    for quote in value.as_array().unwrap() {
        if proposal {
            if !keys(quote, &["messageId", "text"]) || !valid_text(&quote["text"], 4000) {
                return Err(failure("记忆建议须引用完整、有界的用户原话。"));
            }
        } else {
            record(quote, "候选原话")?;
        }
        let id = memory_id(&quote["messageId"])?;
        if !ids.insert(id.as_str().unwrap().to_owned()) {
            return Err(failure(if proposal {
                "记忆建议不能重复引用同一原话。"
            } else {
                "候选原话须引用完整、有界的用户消息。"
            }));
        }
        if !proposal && (!keys(quote, &["messageId", "text"]) || !valid_text(&quote["text"], 4000))
        {
            return Err(failure("候选原话须引用完整、有界的用户消息。"));
        }
        result.push(json!({"messageId": id, "text": quote["text"]}));
    }
    if !ids.contains(source["messageId"].as_str().unwrap_or("")) {
        return Err(failure(if proposal {
            "建议主来源不属于原话记录。"
        } else {
            "候选主来源不属于原话记录。"
        }));
    }
    Ok(json!(result))
}
fn candidate(value: &Value, source: &Value) -> ContractResult {
    record(value, "记忆候选")?;
    if !keys(
        value,
        &[
            "algorithm",
            "trigger",
            "fingerprint",
            "quotes",
            "batchThroughMessageId",
        ],
    ) || value["algorithm"] != CANDIDATE_ALGORITHM
        || !member(&value["trigger"], TRIGGERS)
        || !hash(&value["fingerprint"])
        || source["type"] != "user-message"
        || !value["quotes"]
            .as_array()
            .is_some_and(|quotes| !quotes.is_empty() && quotes.len() <= 12)
    {
        return Err(failure("记忆候选来源格式无效。"));
    }
    let quotes = quotes(&value["quotes"], source, false)?;
    if value.get("batchThroughMessageId").is_some() && value["trigger"] != "ordinary-batch" {
        return Err(failure("批次游标仅属于完整轮次候选。"));
    }
    let mut result = json!({"algorithm": value["algorithm"], "trigger": value["trigger"], "fingerprint": value["fingerprint"], "quotes": quotes});
    if let Some(cursor) = value.get("batchThroughMessageId") {
        result["batchThroughMessageId"] = memory_id(cursor)?;
    }
    Ok(result)
}
fn proposal(value: &Value, source: &Value, timestamps: &Value) -> ContractResult {
    if !keys(
        value,
        &[
            "algorithm",
            "action",
            "quotes",
            "reason",
            "isInference",
            "conflicts",
            "target",
            "fingerprint",
            "appliedAt",
        ],
    ) || value["algorithm"] != PROPOSAL_ALGORITHM
        || !member(&value["action"], &["add", "update", "delete"])
        || source["type"] != "user-message"
        || !value["isInference"].is_boolean()
        || !valid_text(&value["reason"], 1000)
        || !hash(&value["fingerprint"])
        || !value["quotes"]
            .as_array()
            .is_some_and(|quotes| !quotes.is_empty() && quotes.len() <= 12)
        || !value["conflicts"].as_array().is_some_and(|conflicts| {
            conflicts.len() <= 8 && conflicts.iter().all(|conflict| valid_text(conflict, 500))
        })
    {
        return Err(failure("记忆动作建议的原话、理由或不确定性格式无效。"));
    }
    let quotes = quotes(&value["quotes"], source, true)?;
    let target = value.get("target").map(target).transpose()?;
    if (value["action"] != "add") != target.is_some() {
        return Err(failure("更新或删除须指定目标；新增不能伪造目标。"));
    }
    if value.get("appliedAt").is_some() && value["action"] != "update" {
        return Err(failure("只有已确认更新保留执行时间。"));
    }
    let mut conflicts = Vec::new();
    for value in value["conflicts"].as_array().unwrap() {
        let value = json!(trim(value.as_str().unwrap()));
        if !conflicts.contains(&value) {
            conflicts.push(value);
        }
    }
    let mut result = json!({"algorithm": value["algorithm"], "action": value["action"], "quotes": quotes,
        "reason": trim(value["reason"].as_str().unwrap()), "isInference": value["isInference"], "conflicts": conflicts, "fingerprint": value["fingerprint"]});
    if let Some(target) = target {
        result["target"] = target;
    }
    if let Some(applied_at) = value.get("appliedAt") {
        result["appliedAt"] = timestamp(applied_at, timestamps)?;
    }
    Ok(result)
}
fn document(value: &Value, args: &Value) -> ContractResult {
    record(value, "记忆文件")?;
    if value["schemaVersion"] != 1 {
        return Err(ContractError::new(
            "此记忆文件版本不受当前程序支持，原文件已保留。",
            "UNSUPPORTED_MEMORY_VERSION",
            409,
        ));
    }
    let scope = &args["scope"];
    let scope_id = &args["scopeId"];
    let timestamps = &args["timestamps"];
    if &value["scope"] != scope
        || &value["scopeId"] != scope_id
        || !safe_integer(&value["revision"], 0, MAX_SAFE_INTEGER)
        || !value["entries"]
            .as_array()
            .is_some_and(|entries| entries.len() <= 1000)
    {
        return Err(corrupt("记忆文件归属或版本格式无效，原文件已保留。"));
    }
    let mut ids = HashSet::new();
    let mut entries = Vec::new();
    for entry in value["entries"].as_array().unwrap() {
        record(entry, "记忆条目")?;
        let id = memory_id(&entry["id"])?;
        if !ids.insert(id.as_str().unwrap().to_owned())
            || &entry["scope"] != scope
            || &entry["scopeId"] != scope_id
            || !member(&entry["status"], &["draft", "confirmed"])
            || !safe_integer(&entry["revision"], 1, MAX_SAFE_INTEGER)
        {
            return Err(corrupt("记忆条目归属、状态或版本格式无效，原文件已保留。"));
        }
        let source = source(&entry["source"])?;
        let candidate = entry
            .get("candidate")
            .map(|value| candidate(value, &source))
            .transpose()?;
        let proposal = entry
            .get("proposal")
            .map(|value| proposal(value, &source, timestamps))
            .transpose()?;
        if candidate.is_some() && proposal.is_some()
            || entry["status"] == "draft" && candidate.is_none() && proposal.is_none()
        {
            return Err(corrupt("草稿来源缺失或候选类型冲突。"));
        }
        if let Some(proposal) = &proposal {
            if proposal
                .get("target")
                .is_some_and(|target| &target["scope"] != scope || &target["scopeId"] != scope_id)
                || entry["status"] == "draft" && proposal.get("appliedAt").is_some()
                || entry["status"] == "confirmed"
                    && (proposal["action"] == "delete"
                        || proposal["action"] == "update" && proposal.get("appliedAt").is_none())
            {
                return Err(corrupt("记忆建议范围或确认动作无效。"));
            }
        }
        let mut result = json!({"id": id, "scope": scope, "scopeId": scope_id,
            "content": content(&entry["content"])?, "kind": kind(entry.get("kind"), scope)?, "status": entry["status"],
            "source": source, "revision": entry["revision"], "createdAt": timestamp(&entry["createdAt"], timestamps)?,
            "updatedAt": timestamp(&entry["updatedAt"], timestamps)?});
        if let Some(candidate) = candidate {
            result["candidate"] = candidate;
        }
        if let Some(proposal) = proposal {
            result["proposal"] = proposal;
        }
        entries.push(result);
    }
    let dismissed = match value.get("dismissedSources") {
        None => vec![],
        Some(Value::Array(sources)) => sources.clone(),
        _ => return Err(corrupt("记忆撤销来源格式无效，原文件已保留。")),
    };
    let mut dismissed_sources = Vec::new();
    for source in dismissed {
        record(&source, "记忆撤销来源")?;
        for (field, message) in [
            ("candidateFingerprint", "候选撤销身份格式无效。"),
            ("proposalFingerprint", "记忆建议撤销身份格式无效。"),
        ] {
            if source.get(field).is_some_and(|value| !hash(value)) {
                return Err(corrupt(message));
            }
        }
        let mut result = json!({"conversationId": memory_id(&source["conversationId"])?, "messageId": memory_id(&source["messageId"])?,
            "deletedAt": timestamp(&source["deletedAt"], timestamps)?});
        for field in ["candidateFingerprint", "proposalFingerprint"] {
            if let Some(value) = source.get(field) {
                result[field] = value.clone();
            }
        }
        if let Some(cursor) = source.get("candidateBatchThroughMessageId") {
            result["candidateBatchThroughMessageId"] = memory_id(cursor)?;
        }
        dismissed_sources.push(result);
    }
    let mut result = json!({"schemaVersion": 1, "scope": scope, "scopeId": scope_id, "revision": value["revision"], "entries": entries, "dismissedSources": dismissed_sources});
    if value.get("candidateSettings").is_some() || value.get("candidateSettingsRevision").is_some()
    {
        if scope != "user"
            || value.get("candidateSettings").is_none()
            || !safe_integer(&value["candidateSettingsRevision"], 0, MAX_SAFE_INTEGER)
        {
            return Err(corrupt("自动候选配置归属或版本无效。"));
        }
        result["candidateSettings"] = settings(&value["candidateSettings"], &defaults())?;
        result["candidateSettingsRevision"] = value["candidateSettingsRevision"].clone();
    }
    Ok(result)
}
fn instruction(args: &Value) -> ContractResult {
    let Some(message) = args["message"].as_str() else {
        return Ok(Value::Null);
    };
    let trimmed = trim(message);
    let Some((prefix, remaining)) = [
        "全局记住",
        "聊天记住",
        "项目记住",
        "工作记住",
        "记住这个",
        "记住",
    ]
    .iter()
    .find_map(|prefix| {
        trimmed
            .strip_prefix(prefix)
            .map(|remaining| (*prefix, remaining))
    }) else {
        return Ok(Value::Null);
    };
    let remaining = remaining.trim_start_matches(super::js_space);
    let Some(remaining) = remaining.strip_prefix([':', '：']) else {
        return Ok(Value::Null);
    };
    let text = trim(remaining);
    if text.is_empty() {
        return Ok(Value::Null);
    }
    let has_project = args["hasProject"].as_bool().unwrap_or(false);
    let scope = match prefix {
        "全局记住" => "user",
        "聊天记住" => "chat",
        "项目记住" | "工作记住" => "project",
        _ if has_project => "project",
        _ => "chat",
    };
    if scope == "project" && !has_project {
        return Err(failure(
            "此聊天未关联项目，请使用“聊天记住：”或先选择一个项目。",
        ));
    }
    content(&json!(text))?;
    let start = text.as_ptr() as usize - message.as_ptr() as usize;
    Ok(
        json!({"scope": scope, "kind": if scope == "user" { "preference" } else { "fact" },
        "startOffset": utf16_length(&message[..start]), "endOffset": utf16_length(&message[..start]) + utf16_length(text)}),
    )
}

/// Candidate and proposal authority stays versioned; matching a quotation never confirms a write.
/// 候选和提案仍绑定版本；原话匹配不能自动确认写入。
pub fn apply(operation: &str, args: &Value) -> ContractResult {
    let value = &args["value"];
    match operation {
        "storage.memory.extract" => super::candidates::extract(args),
        "storage.memory.scope" => scope(value),
        "storage.memory.content" => content(value),
        "storage.memory.kind" => kind(args.get("value"), &args["scope"]),
        "storage.memory.revision" => revision(
            args.get("value"),
            args["required"].as_bool().unwrap_or(false),
        ),
        "storage.memory.status" => status(args.get("value")),
        "storage.memory.settings" => settings(value, args.get("previous").unwrap_or(&defaults())),
        "storage.memory.source" => source(value),
        "storage.memory.target" => target(value),
        "storage.memory.candidate" => candidate(value, &args["source"]),
        "storage.memory.proposal" => proposal(value, &args["source"], &args["timestamps"]),
        "storage.memory.document" => document(value, args),
        "storage.memory.instruction" => instruction(args),
        "storage.memory.updateInput" => {
            if keys(
                value,
                &[
                    "scope",
                    "content",
                    "kind",
                    "status",
                    "expectedRevision",
                    "proposalAction",
                ],
            ) {
                Ok(Value::Null)
            } else {
                Err(failure("记忆更新输入无效。"))
            }
        }
        "storage.memory.confirmation" => {
            let action = &args["entry"]["proposal"]["action"];
            if member(action, &["update", "delete"]) && action != &args["proposalAction"] {
                Err(ContractError::new(
                    "此建议需要明确确认更新或删除动作，请使用支持动作展示的确认入口。",
                    "MEMORY_PROPOSAL_CONFIRMATION_REQUIRED",
                    409,
                ))
            } else {
                Ok(Value::Null)
            }
        }
        _ => Err(ContractError::new(
            "Unknown memory contract.",
            "AGENT_CORE_UNKNOWN_OPERATION",
            400,
        )),
    }
}
