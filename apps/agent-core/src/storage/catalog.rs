use super::{ContractError, ContractResult, identity};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_CONVERSATION_DATA", 400)
}
fn key(value: &Value) -> Result<String, ContractError> {
    Ok(identity(value)?.as_str().unwrap().to_ascii_lowercase())
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}

fn locations(document: &Value) -> Vec<Value> {
    let mut locations = Vec::new();
    for chat in document["Chats"].as_array().into_iter().flatten() {
        locations.push(json!({"Id": chat["Id"], "ProjectId": null, "Chat": chat}));
    }
    for project in document["Projects"].as_array().into_iter().flatten() {
        for chat in project["Chats"].as_array().into_iter().flatten() {
            locations.push(json!({"Id": chat["Id"], "ProjectId": project["Id"], "Chat": chat}));
        }
    }
    locations
}

fn validate_unique(document: &Value) -> Result<(), ContractError> {
    let mut projects = HashSet::new();
    for project in document["Projects"]
        .as_array()
        .ok_or_else(|| invalid("项目列表格式无效。"))?
    {
        if !projects.insert(key(&project["Id"])?) {
            return Err(invalid("项目包含重复 ID。"));
        }
    }
    let mut chats = HashSet::new();
    for location in locations(document) {
        if !chats.insert(key(&location["Id"])?) {
            return Err(invalid("项目和普通聊天之间存在重复聊天 ID。"));
        }
    }
    Ok(())
}

fn owner<'a>(document: &'a Value, location: &Value) -> Option<&'a Value> {
    let id = key(&location["ProjectId"]).ok()?;
    document["Projects"]
        .as_array()?
        .iter()
        .find(|project| key(&project["Id"]).ok().as_ref() == Some(&id))
}

fn locate(document: &Value, id: &Value) -> Result<Value, ContractError> {
    let id = key(id)?;
    if document["Tombstones"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|item| key(&item["Id"]).ok().as_ref() == Some(&id))
    {
        return Err(ContractError::new(
            "聊天已删除，不能继续写入。",
            "CONVERSATION_DELETED",
            410,
        ));
    }
    locations(document)
        .into_iter()
        .find(|location| key(&location["Id"]).ok().as_ref() == Some(&id))
        .ok_or_else(|| ContractError::new("聊天不存在。", "CONVERSATION_NOT_FOUND", 404))
}

fn relationship(document: &Value, location: &Value) -> Value {
    let project = owner(document, location);
    json!({"conversationId": location["Id"], "projectId": project.map(|project| project["Id"].clone()), "projectName": project.map(|project| project["Name"].clone()), "isFolderlessWorkspace": project.is_some_and(|project| truthy(&project["IsFolderlessWorkspace"])), "isArchived": truthy(&location["Chat"]["IsArchived"]), "projectArchived": project.is_some_and(|project| truthy(&project["IsArchived"]))})
}

fn previous_source(current: &Value, location: &Value) -> Result<Value, ContractError> {
    let id = key(&location["Id"])?;
    if let Some(previous) = locations(current)
        .into_iter()
        .find(|previous| key(&previous["Id"]).ok().as_ref() == Some(&id))
    {
        return Ok(previous);
    }
    if current["Tombstones"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|item| key(&item["Id"]).ok().as_ref() == Some(&id))
    {
        return Ok(json!({"Id": location["Id"], "Trash": true}));
    }
    Ok(location.clone())
}

fn same_directory(left: &Value, right: &Value) -> Result<bool, ContractError> {
    Ok(key(&left["Id"])? == key(&right["Id"])?
        && truthy(&left["Trash"]) == truthy(&right["Trash"])
        && left["ProjectId"].as_str().map(str::to_ascii_lowercase)
            == right["ProjectId"].as_str().map(str::to_ascii_lowercase))
}

fn slim(location: &Value) -> Value {
    if truthy(&location["Trash"]) {
        json!({"Id": location["Id"], "Trash": true})
    } else {
        json!({"Id": location["Id"], "ProjectId": location["ProjectId"]})
    }
}

fn metadata(chat: &Value) -> Value {
    let mut chat = chat.clone();
    if let Some(chat) = chat.as_object_mut() {
        chat.remove("Messages");
    }
    chat
}

fn is_metadata_only_update(
    location: &Value,
    previous: Option<&Value>,
) -> Result<bool, ContractError> {
    let Some(previous) = previous else {
        return Ok(false);
    };
    if !same_directory(previous, location)? {
        return Ok(false);
    }
    // An unchanged registered location already witnesses admission; empty UI history never clears its log.
    // 已登记且位置不变的正式聊天已经通过准入；界面未提交历史不能解释为清空正式日志。
    Ok(!location["Chat"]["Messages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|message| message["Role"] == "user"))
}

/// Catalog state owns stable ownership, draft admission, undo/deletion and cross-folder transaction planning.
/// 目录状态拥有稳定归属、草稿准入、撤销及删除，以及跨目录事务规划。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let current = &arguments["current"];
    let value = &arguments["value"];
    match operation {
        "storage.catalog.messages" => {
            let entries = arguments["messages"]
                .as_array()
                .filter(|entries| entries.len() <= 2000)
                .ok_or_else(|| invalid("消息批量校验参数无效。"))?;
            // A page validates metadata in one frame; exact bodies remain in the original gateway codec objects.
            // 单页元信息共用一次传输校验，精确正文继续留在网关原始编码对象中，不进行标量改写。
            return entries
                .iter()
                .map(|entry| apply("storage.catalog.message", entry))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array);
        }
        "storage.catalog.record" => {
            let valid = if arguments.get("recordType").is_some() {
                arguments["recordType"] == "object"
                    && arguments["recordIsArray"] != true
                    && arguments["recordIsNull"] != true
            } else {
                value.is_object()
            };
            if !valid {
                return Err(invalid(&format!(
                    "{}格式无效，原文件已保留。",
                    arguments["label"].as_str().unwrap_or("记录")
                )));
            }
            return Ok(Value::Null);
        }
        "storage.catalog.text" => {
            let valid = if arguments.get("value").is_some() {
                value.is_null() || value.is_string() && arguments["rawType"] == "string"
            } else {
                arguments["rawType"] == "string"
            };
            if !valid {
                return Err(invalid("会话文本格式无效，原文件已保留。"));
            }
            return Ok(Value::Null);
        }
        "storage.catalog.message" => {
            if !value.is_object() {
                return Err(invalid("消息格式无效，原文件已保留。"));
            }
            let id = identity(&arguments["id"])?;
            if arguments["rawTypes"]["Role"] != "string"
                || !matches!(
                    value["Role"].as_str(),
                    Some("user" | "assistant" | "system" | "tool")
                )
            {
                return Err(invalid("消息角色格式无效，原文件已保留。"));
            }
            if arguments["hasTranscript"] == true && value["Role"] != "assistant" {
                return Err(ContractError::new(
                    "模型转录只能由助手请求记录。",
                    "INVALID_MODEL_TRANSCRIPT",
                    400,
                ));
            }
            if !value["CreatedAt"].is_null()
                && (!value["CreatedAt"].is_string()
                    || arguments["rawTypes"]["CreatedAt"] != "string")
                || !arguments["parsedDate"].is_number()
            {
                return Err(invalid("消息时间格式无效。"));
            }
            let duration = if value["ReasoningDurationMs"].is_null() {
                json!(0)
            } else {
                value["ReasoningDurationMs"].clone()
            };
            if !value["ReasoningDurationMs"].is_null()
                && arguments["rawTypes"]["ReasoningDurationMs"] != "number"
                || !duration
                    .as_f64()
                    .is_some_and(|duration| duration.is_finite() && duration >= 0.0)
            {
                return Err(invalid("思考耗时格式无效。"));
            }
            for field in [
                "Content",
                "Reasoning",
                "Status",
                "Error",
                "Provider",
                "Model",
            ] {
                if !value[field].is_null()
                    && (!value[field].is_string() || arguments["rawTypes"][field] != "string")
                {
                    return Err(invalid("会话文本格式无效，原文件已保留。"));
                }
            }
            return Ok(json!({"id":id,"reasoningDurationMs":duration}));
        }
        "storage.catalog.chat" => {
            if !value.is_object() {
                return Err(invalid("聊天格式无效，原文件已保留。"));
            }
            identity(&value["Id"])?;
            if arguments["titleType"] != "string"
                || !value["Title"]
                    .as_str()
                    .is_some_and(|value| !value.trim_matches(super::js_space).is_empty())
            {
                return Err(invalid("聊天标题不能为空。"));
            }
            if arguments["hasMessages"] == true && !value["Messages"].is_array() {
                return Err(invalid("聊天消息列表格式无效。"));
            }
            if !value["Draft"].is_null() && !value["Draft"].is_string() {
                return Err(invalid("会话文本格式无效，原文件已保留。"));
            }
            if !value["KeepWorkspace"].is_null() && !value["KeepWorkspace"].is_boolean() {
                return Err(invalid("工作区保留标记必须是布尔值。"));
            }
            return Ok(Value::Null);
        }
        "storage.catalog.uniqueMessages" => {
            let mut seen = HashSet::new();
            for id in arguments["ids"].as_array().into_iter().flatten() {
                if !seen.insert(key(id)?) {
                    return Err(invalid("聊天包含重复消息 ID。"));
                }
            }
            return Ok(Value::Null);
        }
        "storage.catalog.projectValue" => {
            if !value.is_object() {
                return Err(invalid("项目格式无效，原文件已保留。"));
            }
            identity(&value["Id"])?;
            if arguments["nameType"] != "string"
                || !value["Name"]
                    .as_str()
                    .is_some_and(|value| !value.trim_matches(super::js_space).is_empty())
                || !value["Chats"].is_array()
            {
                return Err(invalid("项目列表格式无效。"));
            }
            if !value["FolderPath"].is_null() && !value["FolderPath"].is_string() {
                return Err(invalid("会话文本格式无效，原文件已保留。"));
            }
            super::workspace_mounts::project(value)?;
            return Ok(Value::Null);
        }
        "storage.catalog.unique" => {
            validate_unique(value)?;
            return Ok(Value::Null);
        }
        "storage.catalog.document" => {
            if !value.is_object()
                || value["Version"] != 1
                || !value["Revision"]
                    .as_u64()
                    .is_some_and(|value| value <= 9_007_199_254_740_991)
                || !value["Projects"].is_array()
                || !value["Chats"].is_array()
                || !value["Tombstones"].is_array()
            {
                return Err(invalid("会话目录格式无效，原文件已保留。"));
            }
            validate_unique(value)?;
            let active = locations(value)
                .iter()
                .map(|location| key(&location["Id"]))
                .collect::<Result<HashSet<_>, _>>()?;
            for item in value["Tombstones"].as_array().unwrap() {
                if !item.is_object() {
                    return Err(invalid("已删除聊天格式无效，原文件已保留。"));
                }
                if active.contains(&key(&item["Id"])?) {
                    return Err(invalid("聊天删除状态冲突。"));
                }
            }
            return Ok(Value::Null);
        }
        "storage.catalog.event" => {
            if arguments["blank"] == true
                || value["version"] != 1
                || value["type"] != "message.upsert"
            {
                return Err(invalid("会话事件格式无效。"));
            }
            return Ok(Value::Null);
        }
        "storage.catalog.checkAlive" => {
            let id = key(&arguments["id"])?;
            if current["Tombstones"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| key(&item["Id"]).ok().as_ref() == Some(&id))
            {
                return Err(ContractError::new(
                    "聊天已删除，不能继续写入。",
                    "CONVERSATION_DELETED",
                    410,
                ));
            }
            return Ok(Value::Null);
        }
        "storage.catalog.find" => {
            let id = key(&arguments["id"])?;
            for (index, chat) in current["Chats"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if key(&chat["Id"])? == id {
                    return Ok(json!({"chatIndex":index,"projectIndex":null}));
                }
            }
            for (project_index, project) in current["Projects"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                for (chat_index, chat) in project["Chats"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    if key(&chat["Id"])? == id {
                        return Ok(json!({"chatIndex":chat_index,"projectIndex":project_index}));
                    }
                }
            }
            return Ok(Value::Null);
        }
        "storage.catalog.bindWork" => {
            let location = locate(current, &arguments["id"]).map_err(|_| {
                ContractError::new(
                    "聊天的工作归属已变化，请重新读取当前工作。",
                    "WORKSPACE_CHANGED",
                    409,
                )
            })?;
            let project = owner(current, &location).ok_or_else(|| {
                ContractError::new(
                    "聊天的工作归属已变化，请重新读取当前工作。",
                    "WORKSPACE_CHANGED",
                    409,
                )
            })?;
            if key(&project["Id"])? != key(&arguments["expectedProjectId"])? {
                return Err(ContractError::new(
                    "聊天的工作归属已变化，请重新读取当前工作。",
                    "WORKSPACE_CHANGED",
                    409,
                ));
            }
            if truthy(&project["IsArchived"])
                || truthy(&project["IsFolderlessWorkspace"])
                || truthy(&location["Chat"]["IsArchived"])
            {
                return Err(ContractError::new(
                    "当前工作或聊天不可关联目录。",
                    "WORK_UNAVAILABLE",
                    409,
                ));
            }
            if arguments["resolvedCurrent"] != arguments["resolvedExpected"] {
                return Err(ContractError::new(
                    "工作目录已被修改，请重新读取后再关联。",
                    "WORKSPACE_CHANGED",
                    409,
                ));
            }
            let path = arguments["folderPath"]
                .as_str()
                .filter(|value| {
                    std::path::Path::new(value).is_absolute() && !value.contains(['\0', '\r', '\n'])
                })
                .ok_or_else(|| invalid("工作目录路径无效。"))?;
            let index = current["Projects"]
                .as_array()
                .unwrap()
                .iter()
                .position(|value| key(&value["Id"]).ok() == key(&project["Id"]).ok())
                .unwrap();
            return Ok(
                json!({"projectIndex":index,"projectId":key(&project["Id"])? ,"changed":!truthy(&project["FolderPath"])||arguments["resolvedCurrent"].as_str()!=Some(path)}),
            );
        }
        _ => {}
    }
    if operation == "storage.catalog.relationship" {
        let location = locate(current, &arguments["id"])?;
        return Ok(relationship(current, &location));
    }
    if operation == "storage.catalog.related" {
        let location = locate(current, &arguments["id"])?;
        let project = owner(current, &location);
        let shared = project.is_some_and(|project| {
            !truthy(&project["IsFolderlessWorkspace"]) && !truthy(&project["IsArchived"])
        });
        let mut result = relationship(current, &location);
        result.as_object_mut().unwrap().remove("isArchived");
        result.as_object_mut().unwrap().remove("projectArchived");
        result["relatedConversations"] = if shared {
            json!(project.unwrap()["Chats"].as_array().unwrap().iter().filter(|chat| key(&chat["Id"]).ok() != key(&location["Id"]).ok()).map(|chat| json!({"id": chat["Id"], "title": chat["Title"], "isArchived": truthy(&chat["IsArchived"])})).collect::<Vec<_>>())
        } else {
            json!([])
        };
        result["memoryScopes"] = if shared {
            json!(["chat", "project", "user"])
        } else {
            json!(["chat", "user"])
        };
        return Ok(result);
    }
    if operation == "storage.catalog.project" {
        let id = key(&arguments["id"])?;
        let project = current["Projects"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|project| key(&project["Id"]).ok().as_ref() == Some(&id))
            .ok_or_else(|| ContractError::new("工作不存在。", "PROJECT_NOT_FOUND", 404))?;
        let mut result = super::workspace_mounts::relationship(project)?;
        result["projectId"] = project["Id"].clone();
        result["name"] = project["Name"].clone();
        result["isFolderlessWorkspace"] = json!(truthy(&project["IsFolderlessWorkspace"]));
        result["isArchived"] = json!(truthy(&project["IsArchived"]));
        return Ok(result);
    }
    let mut next = arguments["next"].clone();
    if arguments["expectedRevision"] != current["Revision"] {
        return Err(ContractError::new(
            "聊天目录已更新，请重新读取后重试。",
            "CATALOG_CONFLICT",
            409,
        ));
    }
    validate_unique(&next)?;
    let previous: HashMap<String, Value> = locations(current)
        .into_iter()
        .map(|location| Ok((key(&location["Id"])?, location)))
        .collect::<Result<_, ContractError>>()?;
    if operation == "storage.catalog.logRequests" {
        let mut requests = Vec::new();
        for location in locations(&next) {
            if !is_metadata_only_update(&location, previous.get(&key(&location["Id"])?))? {
                requests.push(json!({"id":location["Id"],"source":slim(&previous_source(current,&location)?)}));
            }
        }
        return Ok(json!(requests));
    }
    if operation != "storage.catalog.savePlan" {
        return Err(invalid("未知目录规划操作。"));
    }
    let logs: HashMap<String, &Value> = arguments["logs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|log| Ok((key(&log["id"])?, log)))
        .collect::<Result<_, ContractError>>()?;
    let mut retained = HashSet::new();
    let mut moves = Vec::new();
    let mut writes = Vec::new();
    for location in locations(&next) {
        let id = key(&location["Id"])?;
        let old = previous.get(&id);
        if is_metadata_only_update(&location, old)? {
            retained.insert(id);
            continue;
        }
        let source = previous_source(current, &location)?;
        let deleted = truthy(&source["Trash"]);
        let messages = logs
            .get(&id)
            .and_then(|log| log["messages"].as_array())
            .ok_or_else(|| invalid("聊天准入需要对应的正式日志身份。"))?;
        let saved: HashSet<String> = messages
            .iter()
            .map(|message| key(&message["Id"]))
            .collect::<Result<_, _>>()?;
        let new_messages: Vec<(usize, &Value)> = location["Chat"]["Messages"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, message)| {
                message["Role"] == "user"
                    && !saved.contains(&key(&message["Id"]).unwrap_or_default())
            })
            .collect();
        if !truthy(&location["Chat"]["IsSample"]) && location["Chat"]["KeepWorkspace"] != true && messages.is_empty() && new_messages.is_empty()
        {
            continue;
        }
        retained.insert(id.clone());
        if (old.is_some() || deleted) && !same_directory(&source, &location)? {
            moves.push(json!({"From": slim(&source), "To": slim(&location)}));
        }
        for (index, _) in new_messages {
            writes.push(json!({"Location": slim(&location), "conversationId": location["Id"], "messageIndex": index}));
        }
    }
    next["Chats"] = json!(
        next["Chats"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|chat| retained.contains(&key(&chat["Id"]).unwrap_or_default()))
            .map(metadata)
            .collect::<Vec<_>>()
    );
    for project in next["Projects"].as_array_mut().unwrap() {
        project["Chats"] = json!(
            project["Chats"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|chat| retained.contains(&key(&chat["Id"]).unwrap_or_default()))
                .map(metadata)
                .collect::<Vec<_>>()
        );
    }
    let mut tombstones: Vec<Value> = next["Tombstones"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| !retained.contains(&key(&item["Id"]).unwrap_or_default()))
        .cloned()
        .collect();
    // Preserve canonical traversal order for deterministic move intent and deletion receipts.
    // 按正式目录遍历顺序产生移动意图和删除回执，保证恢复顺序稳定。
    for location in locations(current) {
        if !retained.contains(&key(&location["Id"])?) {
            moves.push(
                json!({"From": slim(&location), "To": {"Id": location["Id"], "Trash": true}}),
            );
            tombstones.push(json!({"Id": location["Id"], "ProjectId": location["ProjectId"], "DeletedAt": arguments["now"]}));
        }
    }
    next["Tombstones"] = json!(tombstones);
    next["Revision"] = json!(current["Revision"].as_u64().unwrap() + 1);
    Ok(json!({"next": next, "moves": moves, "writes": writes}))
}

#[cfg(test)]
mod save_plan_tests {
    use super::*;

    fn catalog(chats: Value) -> Value {
        json!({"Version":1,"Revision":7,"Projects":[],"Chats":chats,"Tombstones":[]})
    }

    fn plan(operation: &str, current: &Value, next: &Value, logs: Value) -> ContractResult {
        apply(
            operation,
            &json!({"current":current,"next":next,"logs":logs,
            "expectedRevision":current["Revision"],"now":"2026-10-12T08:00:00Z"}),
        )
    }

    #[test]
    fn user_submission_reads_only_its_log_and_retains_unrelated_metadata() {
        let current = catalog(json!([
            {"Id":"old-chat","Title":"Old chat","IsSample":false},
            {"Id":"active-chat","Title":"Active chat"}
        ]));
        let mut next = current.clone();
        next["Chats"][0]["Title"] = json!("Renamed old chat");
        next["Chats"][0]["IsArchived"] = json!(true);
        next["Chats"][0]["Messages"] = json!([]);
        next["Chats"][1]["Messages"] = json!([
            {"Id":"saved-user","Role":"user","Content":"Already saved"},
            {"Id":"new-user","Role":"user","Content":"Hello"},
            {"Id":"ui-assistant","Role":"assistant","Content":"Not authoritative"}
        ]);
        let requests = plan("storage.catalog.logRequests", &current, &next, json!([])).unwrap();
        assert_eq!(
            requests,
            json!([{"id":"active-chat","source":{"Id":"active-chat","ProjectId":null}}])
        );
        let saved = plan(
            "storage.catalog.savePlan",
            &current,
            &next,
            json!([{"id":"active-chat","messages":[{"Id":"saved-user"}]}]),
        )
        .unwrap();
        assert_eq!(saved["next"]["Chats"].as_array().unwrap().len(), 2);
        assert_eq!(saved["next"]["Chats"][0]["Title"], "Renamed old chat");
        assert_eq!(saved["next"]["Chats"][0]["IsArchived"], true);
        assert_eq!(
            saved["writes"],
            json!([{"Location":{"Id":"active-chat","ProjectId":null},
            "conversationId":"active-chat","messageIndex":1}])
        );
        assert_eq!(saved["moves"], json!([]));
        assert_eq!(saved["next"]["Tombstones"], json!([]));
        assert!(
            saved["next"]["Chats"]
                .as_array()
                .unwrap()
                .iter()
                .all(|chat| chat.get("Messages").is_none())
        );
    }

    #[test]
    fn registered_project_chat_keeps_its_admission_without_history_or_sample_flag() {
        let mut current = catalog(json!([]));
        current["Projects"] = json!([{"Id":"work-one","Name":"Work","Chats":[
            {"Id":"registered-chat","Title":"Registered chat","IsSample":false}
        ]}]);
        let mut next = current.clone();
        next["Projects"][0]["Name"] = json!("Renamed work");
        next["Projects"][0]["Chats"][0]["Messages"] = json!([]);
        assert_eq!(
            plan("storage.catalog.logRequests", &current, &next, json!([])).unwrap(),
            json!([])
        );
        let saved = plan("storage.catalog.savePlan", &current, &next, json!([])).unwrap();
        assert_eq!(
            saved["next"]["Projects"][0]["Chats"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(saved["next"]["Projects"][0]["Name"], "Renamed work");
        assert_eq!(saved["writes"], json!([]));
        assert_eq!(saved["moves"], json!([]));
    }

    #[test]
    fn moves_and_trash_restoration_still_read_original_log_identities() {
        let mut current = catalog(json!([{"Id":"moving-chat","Title":"Moving chat"}]));
        current["Tombstones"] = json!([{"Id":"restored-chat","ProjectId":null}]);
        let mut next = current.clone();
        next["Chats"] = json!([]);
        next["Projects"] = json!([{"Id":"target-work","Name":"Target work","Chats":[
            {"Id":"moving-chat","Title":"Moving chat","Messages":[]},
            {"Id":"restored-chat","Title":"Restored chat","Messages":[]}
        ]}]);
        let requests = plan("storage.catalog.logRequests", &current, &next, json!([])).unwrap();
        assert_eq!(
            requests,
            json!([
                {"id":"moving-chat","source":{"Id":"moving-chat","ProjectId":null}},
                {"id":"restored-chat","source":{"Id":"restored-chat","Trash":true}}
            ])
        );
        let saved = plan(
            "storage.catalog.savePlan",
            &current,
            &next,
            json!([
                {"id":"moving-chat","messages":[{"Id":"moving-user"}]},
                {"id":"restored-chat","messages":[{"Id":"restored-user"}]}
            ]),
        )
        .unwrap();
        assert_eq!(
            saved["moves"],
            json!([
                {"From":{"Id":"moving-chat","ProjectId":null},"To":{"Id":"moving-chat","ProjectId":"target-work"}},
                {"From":{"Id":"restored-chat","Trash":true},"To":{"Id":"restored-chat","ProjectId":"target-work"}}
            ])
        );
        assert_eq!(
            saved["next"]["Projects"][0]["Chats"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(saved["next"]["Tombstones"], json!([]));
        assert_eq!(saved["writes"], json!([]));
    }

    #[test]
    fn unsent_drafts_and_ui_assistants_do_not_admit_new_chats() {
        let current = catalog(json!([]));
        let next = catalog(json!([
            {"Id":"draft-chat","Title":"Draft","Draft":"Unsent","Messages":[]},
            {"Id":"assistant-chat","Title":"Assistant snapshot","Messages":[
                {"Id":"ui-assistant","Role":"assistant","Content":"UI only"}]},
            {"Id":"sample-chat","Title":"Sample","IsSample":true,"Messages":[]},
            {"Id":"user-chat","Title":"Sent","Messages":[
                {"Id":"sent-user","Role":"user","Content":"Sent"}]}
        ]));
        let requests = plan("storage.catalog.logRequests", &current, &next, json!([])).unwrap();
        assert_eq!(requests.as_array().unwrap().len(), 4);
        let logs = json!([{"id":"draft-chat","messages":[]},{"id":"assistant-chat","messages":[]},
            {"id":"sample-chat","messages":[]},{"id":"user-chat","messages":[]}]);
        let saved = plan("storage.catalog.savePlan", &current, &next, logs).unwrap();
        assert_eq!(
            saved["next"]["Chats"]
                .as_array()
                .unwrap()
                .iter()
                .map(|chat| chat["Id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["sample-chat", "user-chat"]
        );
        assert_eq!(
            saved["writes"],
            json!([{"Location":{"Id":"user-chat","ProjectId":null},
            "conversationId":"user-chat","messageIndex":0}])
        );
    }

    #[test]
    fn an_explicit_workspace_can_exist_without_synthetic_chat_messages() {
        let current = catalog(json!([]));
        let next = catalog(json!([
            {"Id":"workspace-chat","Title":"Workspace","KeepWorkspace":true,"Messages":[]},
            {"Id":"draft-chat","Title":"Unsent draft","Messages":[]}
        ]));
        let saved = plan("storage.catalog.savePlan", &current, &next,
            json!([{"id":"workspace-chat","messages":[]},{"id":"draft-chat","messages":[]}])).unwrap();
        assert_eq!(saved["next"]["Chats"].as_array().unwrap().len(), 1);
        assert_eq!(saved["next"]["Chats"][0]["Id"], "workspace-chat");
        assert_eq!(saved["writes"], json!([]));
    }

    #[test]
    fn metadata_update_ignores_ui_assistant_but_omission_still_deletes_chat() {
        let current = catalog(json!([
            {"Id":"retained-chat","Title":"Retained"},{"Id":"removed-chat","Title":"Removed"}
        ]));
        let next = catalog(json!([{"Id":"retained-chat","Title":"Renamed","Messages":[
            {"Id":"fake-assistant","Role":"assistant","Content":"Never write"}
        ]}]));
        assert_eq!(
            plan("storage.catalog.logRequests", &current, &next, json!([])).unwrap(),
            json!([])
        );
        let saved = plan("storage.catalog.savePlan", &current, &next, json!([])).unwrap();
        assert_eq!(saved["writes"], json!([]));
        assert_eq!(
            saved["moves"],
            json!([{"From":{"Id":"removed-chat","ProjectId":null},
            "To":{"Id":"removed-chat","Trash":true}}])
        );
        assert_eq!(saved["next"]["Tombstones"][0]["Id"], "removed-chat");
    }

    #[test]
    fn user_submission_requires_log_receipt_and_metadata_keeps_revision_checks() {
        let current = catalog(json!([{"Id":"active-chat","Title":"Active"}]));
        let mut next = current.clone();
        next["Chats"][0]["Messages"] = json!([{"Id":"new-user","Role":"user","Content":"Hello"}]);
        assert_eq!(
            plan("storage.catalog.savePlan", &current, &next, json!([]))
                .unwrap_err()
                .code(),
            "INVALID_CONVERSATION_DATA"
        );
        for operation in ["storage.catalog.logRequests", "storage.catalog.savePlan"] {
            let error = apply(
                operation,
                &json!({"current":current,"next":current,
                "expectedRevision":6,"logs":[]}),
            )
            .unwrap_err();
            assert_eq!(error.code(), "CATALOG_CONFLICT");
        }
    }
}
