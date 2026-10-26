//! Explicit history evidence over authorized formal chat logs; work history is never injected automatically.
//! 授权正式聊天日志上的显式历史取证；工作内其他聊天不自动注入上下文。

use crate::{
    gateway_contract::GatewayConfig,
    storage::{self, ContractError, ContractResult, conversation_history, gateway},
    tools::service_history::HistoryState,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

const PAGE_BYTES: usize = 8 * 1024 * 1024;
const SCAN_BYTES: usize = 128 * 1024 * 1024;
const SCAN_TIME: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct OriginalContent<'a> {
    #[serde(rename = "Content", borrow)]
    content: &'a serde_json::value::RawValue,
}

#[derive(Default, Serialize, Deserialize)]
struct SearchCursor {
    // Wire fields are serialized continuation state; rename only through a compatible cursor migration.
    // 字段会序列化为续查游标；改名必须经过兼容迁移，不能当作局部变量整理。
    identity: String,
    conversation: usize,
    page: Option<String>,
    page_version: Value,
    skip: usize,
    matched: usize,
    examined: usize,
    failed: Vec<Value>,
}

fn invalid(message: &str) -> ContractError {
    gateway::error(message, "INVALID_TOOL_ARGUMENTS", 400)
}

fn same_scope_id(left: &Value, right: &Value) -> bool {
    left.is_null() && right.is_null()
        || left
            .as_str()
            .zip(right.as_str())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn apply_history_operation(
    history: &mut HistoryState,
    operation: &str,
    arguments: Value,
) -> ContractResult {
    history.apply(operation, &arguments).map_err(|error| {
        let code = match error.code.as_str() {
            "WORKSPACE_CHANGED" => "WORKSPACE_CHANGED",
            "CONVERSATION_HISTORY_ARCHIVED" => "CONVERSATION_HISTORY_ARCHIVED",
            "CONVERSATION_HISTORY_MESSAGE_NOT_FOUND" => "CONVERSATION_HISTORY_MESSAGE_NOT_FOUND",
            _ => "INVALID_TOOL_ARGUMENTS",
        };
        gateway::error(&error.message, code, error.status_code)
    })
}

fn conversation_relationship(catalog: &Value, id: &Value) -> ContractResult {
    storage::catalog::apply(
        "storage.catalog.relationship",
        &json!({ "current": catalog, "id": id }),
    )
}

fn check_context(catalog: &Value, context: &Value) -> ContractResult {
    // Resolve formal ownership before scanning; the request's expected scope remains an immutable boundary.
    // 扫描前核验正式归属；请求携带的预期范围仍是不可静默扩大或切换的边界。
    storage::identity(&context["conversationId"])?;
    let relationship = conversation_relationship(catalog, &context["conversationId"])?;
    let expected = context
        .get("projectId")
        .or_else(|| context.get("expectedProjectId"));
    let expected = expected.ok_or_else(|| {
        gateway::error(
            "缺少程序确认的聊天归属快照。",
            "CONVERSATION_HISTORY_SCOPE_REQUIRED",
            403,
        )
    })?;
    if context
        .get("expectedProjectId")
        .is_some_and(|snapshot| !same_scope_id(snapshot, expected))
    {
        return Err(gateway::error(
            "聊天工作范围已变化。",
            "WORKSPACE_CHANGED",
            409,
        ));
    }
    apply_history_operation(
        &mut HistoryState::default(),
        "history.ownership",
        json!({
            "relationship": relationship,
            "context": {
                "conversationId": context["conversationId"],
                "projectId": expected
            }
        }),
    )?;
    Ok(relationship)
}

fn source_allowed(catalog: &Value, context: &Value, source: &Value) -> ContractResult {
    let current = check_context(catalog, context)?;
    let source = storage::identity(source)?;
    let target = conversation_relationship(catalog, &source)?;
    if !same_scope_id(&source, &current["conversationId"])
        && (current["projectId"].is_null()
            || current["isFolderlessWorkspace"] == true
            || !same_scope_id(&target["projectId"], &current["projectId"]))
    {
        return Err(gateway::error(
            "该历史来源不在当前授权工作内。",
            "CONVERSATION_HISTORY_SCOPE_FORBIDDEN",
            403,
        ));
    }
    apply_history_operation(
        &mut HistoryState::default(),
        "history.ownership",
        json!({
            "relationship": target,
            "context": {
                "conversationId": source,
                "projectId": current["projectId"]
            }
        }),
    )?;
    gateway::locate(catalog, source.as_str().unwrap())
}

fn scope_locations(
    catalog: &Value,
    context: &Value,
    scope: &str,
) -> Result<(Vec<Value>, usize), ContractError> {
    let current = check_context(catalog, context)?;
    if scope == "chat" {
        return Ok((
            vec![gateway::locate(
                catalog,
                current["conversationId"].as_str().unwrap(),
            )?],
            0,
        ));
    }
    if current["projectId"].is_null() || current["isFolderlessWorkspace"] == true {
        return Err(gateway::error(
            "当前聊天没有可查询的共享工作范围。",
            "CONVERSATION_HISTORY_SCOPE_REQUIRED",
            403,
        ));
    }
    let project = catalog["Projects"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|project| same_scope_id(&project["Id"], &current["projectId"]))
        .unwrap();
    let mut excluded = 0;
    let mut locations = Vec::new();
    for chat in project["Chats"].as_array().into_iter().flatten() {
        if chat["IsArchived"] == true {
            excluded += 1;
            continue;
        }
        locations.push(json!({"Id":chat["Id"],"ProjectId":project["Id"]}));
    }
    locations
        .sort_unstable_by_key(|location| location["Id"].as_str().unwrap().to_ascii_lowercase());
    Ok((locations, excluded))
}

fn scope_identity(
    context: &Value,
    locations: &[Value],
    input: &Value,
    scope: &str,
    role: &str,
) -> String {
    storage::managed_files::digest(
        json!([
            context["conversationId"],
            context["projectId"],
            locations
                .iter()
                .map(|location| json!([location["Id"], location["ProjectId"]]))
                .collect::<Vec<_>>(),
            input["query"],
            scope,
            role,
            input["includeTools"] == true
        ])
        .to_string()
        .as_bytes(),
    )
}

/// Public message versions survive unrelated appends but reject changes to the referenced source or its owner.
/// 公开消息版本允许无关消息追加，但拒绝被引用正文或归属变化；工具进展不能使旧用户原文自失效。
fn message_version(
    location: &Value,
    message: &Value,
    text: &str,
    include_tools: bool,
    raw: &str,
) -> String {
    // Display repair must not merge different original UTF-16 source identities.
    // 展示用标量修复不能合并不同原始 UTF-16 正文的版本身份。
    let original = serde_json::from_str::<OriginalContent<'_>>(raw)
        .map(|fields| storage::managed_files::digest(fields.content.get().as_bytes()))
        .unwrap_or_default();
    storage::managed_files::digest(
        json!([
            location["Id"],
            location["ProjectId"],
            message["Id"],
            message["Role"],
            message["Status"],
            message["CreatedAt"],
            include_tools,
            text,
            original
        ])
        .to_string()
        .as_bytes(),
    )
}

fn load_message(raw: &str) -> ContractResult {
    let parsed: Value = serde_json::from_str(&storage::raw_json::scalar_projection(raw))
        .map_err(|_| gateway::error("历史消息损坏。", "CORRUPT_CONVERSATION", 500))?;
    gateway::normalize_message(&parsed)
}

fn message_text(message: &Value, include_tools: bool) -> Result<String, ContractError> {
    if include_tools && message["Role"] == "assistant" {
        crate::models::gateway_protocol::public_history_text(message).map_err(|error| {
            gateway::error(&error.message, "INVALID_MODEL_HISTORY", error.status_code)
        })
    } else {
        Ok(message["Content"].as_str().unwrap_or("").to_owned())
    }
}

fn ingest(history: &mut HistoryState, message: &Value, text: &str) -> ContractResult {
    let plan = apply_history_operation(
        history,
        "history.message",
        json!({
            "message": {
                "Id": message["Id"],
                "Role": message["Role"],
                "Status": message["Status"],
                "CreatedAt": message["CreatedAt"]
            },
            "contentIsString": message["Content"].is_string()
        }),
    )?;
    if !plan.is_null() {
        let units = text.encode_utf16().collect::<Vec<_>>();
        for chunk in units.chunks(16_384) {
            storage::sqlite_store::cancelled()?;
            apply_history_operation(
                history,
                "history.append",
                json!({ "messageIndex": plan["messageIndex"], "units": chunk }),
            )?;
        }
    }
    Ok(plan)
}

fn checked_scope(
    config: &GatewayConfig,
    context: &Value,
    scope: &str,
    input: &Value,
    role: &str,
    expected: &str,
    versions: &mut [(Value, conversation_history::HistoryRead)],
) -> Result<(), ContractError> {
    // Recheck ownership and observed log snapshots after I/O, without holding the catalog lock throughout scanning.
    // I/O 完成后再次核验归属与已观察日志快照；扫描期间不长时间持有目录锁。
    gateway::with_catalog(&config.data_root, |catalog, _| {
        let (locations, _) = scope_locations(catalog, context, scope)?;
        if scope_identity(context, &locations, input, scope, role) != expected {
            return Err(gateway::error(
                "工作历史名单已变化，请重新查询。",
                "WORKSPACE_CHANGED",
                409,
            ));
        }
        for (location, snapshot) in versions {
            source_allowed(catalog, context, &location["Id"])?;
            snapshot.verify()?;
        }
        Ok(())
    })
}

fn search(config: &GatewayConfig, input: &Value, context: &Value) -> ContractResult {
    let scope = input["scope"].as_str().unwrap_or("chat");
    let role = input["role"].as_str().unwrap_or("all");
    if !["chat", "work"].contains(&scope)
        || !["all", "user", "assistant"].contains(&role)
        || !input["query"].is_string()
        || input["query"]
            .as_str()
            .is_some_and(|query| query.contains('\0'))
    {
        return Err(invalid("历史查询、范围或角色无效。"));
    }
    let limit = input["limit"].as_u64().unwrap_or(10) as usize;
    let offset = input["offset"].as_u64().unwrap_or(0) as usize;
    if !(1..=20).contains(&limit)
        || offset > 1_000_000
        || input.get("limit").is_some_and(|value| !value.is_u64())
        || input.get("offset").is_some_and(|value| !value.is_u64())
    {
        return Err(invalid("历史查询分页位置或数量无效。"));
    }
    let (locations, excluded) = gateway::with_catalog(&config.data_root, |catalog, _| {
        scope_locations(catalog, context, scope)
    })?;
    let identity = scope_identity(context, &locations, input, scope, role);
    let mut cursor = if let Some(encoded) = input["cursor"].as_str() {
        let invalid_cursor = || {
            gateway::error(
                "历史查询游标不属于本次范围或查询。",
                "CONVERSATION_HISTORY_CURSOR_INVALID",
                400,
            )
        };
        if encoded.len() > 16_384 || offset != 0 {
            return Err(invalid_cursor());
        }
        let cursor: SearchCursor = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| invalid_cursor())?,
        )
        .map_err(|_| invalid_cursor())?;
        if cursor.identity != identity
            || cursor.conversation >= locations.len()
            || cursor.skip > 128
            || cursor.failed.len() > locations.len()
        {
            return Err(invalid_cursor());
        }
        cursor
    } else if input["cursor"].is_null() {
        SearchCursor {
            identity: identity.clone(),
            ..Default::default()
        }
    } else {
        return Err(invalid("历史查询游标须为字符串。"));
    };
    let started = Instant::now();
    let mut bytes = 0usize;
    let mut rows = Vec::new();
    let mut versions = Vec::new();
    let include_tools = input["includeTools"] == true;
    // Cancellation and scan ceilings bound each call; the cursor retains progress for a later authorized continuation.
    // 每次调用受取消和扫描额度约束；游标保留进度，供后续授权续查使用。
    'scan: while cursor.conversation < locations.len() {
        storage::sqlite_store::cancelled()?;
        if !versions.is_empty() && (bytes >= SCAN_BYTES || started.elapsed() >= SCAN_TIME) {
            break;
        }
        let location = &locations[cursor.conversation];
        let page = conversation_history::read_page(
            &json!({
                "root": config.data_root,
                "location": location,
                "limit": 128,
                "maxBytes": PAGE_BYTES,
                "cursor": cursor.page
            }),
        );
        let (page, snapshot) = match page {
            Ok(page) => page,
            Err(error)
                if scope == "work"
                    && ![
                        "CONVERSATION_HISTORY_CHANGED",
                        "CANCELLED",
                        "REQUEST_CANCELLED",
                    ]
                    .contains(&error.code()) =>
            {
                cursor
                    .failed
                    .push(json!({"conversationId":location["Id"],"code":error.code()}));
                cursor.conversation += 1;
                cursor.page = None;
                cursor.skip = 0;
                cursor.page_version = Value::Null;
                continue;
            }
            Err(error) => return Err(error),
        };
        if !cursor.page_version.is_null() && cursor.page_version != page["snapshot"] {
            return Err(gateway::error(
                "历史已更新，请从最新页重新查询。",
                "CONVERSATION_HISTORY_CHANGED",
                409,
            ));
        }
        versions.push((location.clone(), snapshot));
        bytes += page["returnedBytes"].as_u64().unwrap_or(0) as usize;
        let raw_messages = page["rawMessages"].as_array().unwrap();
        // Newest-first traversal is stable across bounded calls; archived/private/streaming records never become user evidence.
        // 有界调用按最近消息优先继续；归档、私有续接与未完成流不作为用户提问证据。
        for (position, raw) in raw_messages.iter().rev().enumerate().skip(cursor.skip) {
            let message = load_message(raw.as_str().unwrap())?;
            cursor.examined += 1;
            cursor.skip = position + 1;
            if role != "all" && message["Role"] != role {
                continue;
            }
            let text = message_text(&message, include_tools)?;
            let mut history = HistoryState::default();
            if ingest(&mut history, &message, &text)?.is_null() {
                continue;
            }
            let found = apply_history_operation(
                &mut history,
                "history.search",
                json!({
                    "conversationId": location["Id"],
                    "input": { "query": input["query"], "offset": 0, "limit": 1 }
                }),
            )?;
            if let Some(row) = found["messages"].as_array().unwrap().first() {
                cursor.matched += 1;
                if cursor.matched <= offset {
                    continue;
                }
                let mut row = row.clone();
                row["conversationId"] = location["Id"].clone();
                row["projectId"] = location["ProjectId"].clone();
                row["sourceVersion"] = json!(message_version(
                    location,
                    &message,
                    &text,
                    include_tools,
                    raw.as_str().unwrap()
                ));
                rows.push(row);
                if rows.len() == limit {
                    cursor.page_version = page["snapshot"].clone();
                    break 'scan;
                }
            }
        }
        cursor.skip = 0;
        cursor.page_version = Value::Null;
        cursor.page = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.page.is_none() {
            cursor.conversation += 1;
        }
    }
    checked_scope(config, context, scope, input, role, &identity, &mut versions)?;
    let scan_complete = cursor.conversation == locations.len();
    let next_cursor = if scan_complete {
        Value::Null
    } else {
        let encoded = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&cursor).map_err(|_| invalid("无法生成历史继续查询入口。"))?,
        );
        if encoded.len() > 16_384 {
            return Err(gateway::error(
                "历史继续查询状态超过当前额度。",
                "CONVERSATION_HISTORY_CURSOR_LIMIT",
                413,
            ));
        }
        json!(encoded)
    };
    Ok(json!({
        "conversationId": context["conversationId"],
        "projectId": context["projectId"],
        "scope": scope,
        "role": role,
        "messages": rows,
        "offset": offset,
        "nextOffset": cursor.matched,
        "total": cursor.matched,
        "totalIsExact": scan_complete && cursor.failed.is_empty(),
        "hasMore": !scan_complete,
        "nextCursor": next_cursor,
        "coverage": {
            "isComplete": scan_complete && cursor.failed.is_empty(),
            "scanComplete": scan_complete,
            "eligibleConversations": locations.len(),
            "scannedConversations": cursor.conversation,
            "pendingConversations": locations.len() - cursor.conversation,
            "excludedArchivedConversations": excluded,
            "failedConversations": cursor.failed,
            "messagesExamined": cursor.examined,
            "consistency": "verified-per-call",
            "scopeNote": "只核查当前授权范围内的公开消息；未覆盖、未命中、记忆为空均不能证明从未提问。"
        }
    }))
}

fn read(config: &GatewayConfig, input: &Value, context: &Value) -> ContractResult {
    // Read one referenced message, then recheck ownership and the original snapshot before returning its text.
    // 只读取被引用消息；返回正文前再次核验归属和原始快照。
    let source = input
        .get("sourceConversationId")
        .unwrap_or(&context["conversationId"]);
    let location = gateway::with_catalog(&config.data_root, |catalog, _| {
        source_allowed(catalog, context, source)
    })?;
    let id = storage::identity(&input["messageId"])?;
    let ids = HashSet::from([id.as_str().unwrap().to_ascii_lowercase()]);
    let mut snapshot =
        conversation_history::read_latest(&config.data_root, &location, Some(&ids), false)?;
    let mut history = HistoryState::default();
    let mut version = None;
    for raw in &snapshot.messages {
        let message = load_message(raw)?;
        let text = message_text(&message, input["includeTools"] == true)?;
        if !ingest(&mut history, &message, &text)?.is_null() {
            version = Some(message_version(
                &location,
                &message,
                &text,
                input["includeTools"] == true,
                raw,
            ));
        }
    }
    if let Some(expected) = input.get("sourceVersion") {
        if expected.as_str() != version.as_deref() {
            return Err(gateway::error(
                "引用消息已变化，请重新检索该来源。",
                "CONVERSATION_HISTORY_SOURCE_CHANGED",
                409,
            ));
        }
    }
    let mut result = apply_history_operation(
        &mut history,
        "history.read",
        json!({"conversationId":location["Id"],"input":input,"messageId":id}),
    )?;
    if let Some(units) = result.get("units").and_then(Value::as_array) {
        result["text"] = json!(String::from_utf16_lossy(
            &units
                .iter()
                .map(|unit| unit.as_u64().unwrap() as u16)
                .collect::<Vec<_>>()
        ));
        result.as_object_mut().unwrap().remove("units");
    }
    result["projectId"] = location["ProjectId"].clone();
    result["sourceVersion"] = json!(version);
    gateway::with_catalog(&config.data_root, |catalog, _| {
        source_allowed(catalog, context, source)?;
        snapshot.verify()?;
        Ok(result)
    })
}

pub(crate) fn handle(
    config: &GatewayConfig,
    name: &str,
    input: &Value,
    context: &Value,
) -> ContractResult {
    match name {
        "conversation.history.search" => search(config, input, context),
        "conversation.history.read" => read(config, input, context),
        _ => Err(invalid("未知历史工具。")),
    }
}
