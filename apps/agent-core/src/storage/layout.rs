use super::{ContractError, ContractResult, conversation_index, identity, managed_files};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const ROOT_DIRECTORIES: &[&str] = &[
    "Projects",
    "Chats",
    "Memory",
    "Knowledge",
    "Retrieval",
    "Index",
    "Trash",
    "Backups",
    "Agent",
    "Skills",
];
fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_DATA_LAYOUT", 409)
}
fn inspect_path(root: &Path, path: &Path, directory: bool) -> Result<bool, ContractError> {
    managed_files::inspect(root, path, false, !directory, u64::MAX)
        .map_err(|_| invalid("数据目录结构无效或包含链接，请检查存储位置。"))
}
fn id(value: &Value) -> Result<String, ContractError> {
    identity(value)
        .map(|value| value.as_str().unwrap().to_ascii_lowercase())
        .map_err(|_| invalid("项目或聊天 ID 无效。"))
}
fn inspect_session(root: &Path, session: &Path) -> Result<(), ContractError> {
    for path in [
        session.to_path_buf(),
        session.join("attachments"),
        session.join("Memory"),
    ] {
        inspect_path(root, &path, true)?;
    }
    for path in [session.join("context.json"), session.join("events.jsonl")] {
        inspect_path(root, &path, false)?;
    }
    Ok(())
}

/// Inspect the selected canonical scope afresh; preparation caches never authorize its actual paths.
/// 每次即时检查所选正式范围；目录准备缓存不能替代实际路径授权。
pub(crate) fn inspect_conversation(root: &Path, location: &Value) -> Result<(), ContractError> {
    let chat_id = id(&location["Id"])?;
    let session = if location["ProjectId"].is_null() {
        root.join("Chats").join(chat_id)
    } else {
        let folder = root.join("Projects").join(id(&location["ProjectId"])?);
        for path in [folder.clone(), folder.join("Sessions"), folder.join("Memory")] {
            inspect_path(root, &path, true)?;
        }
        inspect_path(root, &folder.join("project.json"), false)?;
        folder.join("Sessions").join(chat_id)
    };
    inspect_session(root, &session)
}

pub(crate) fn inspect(
    root: &Path,
    document: Option<&Value>,
) -> Result<(Option<String>, Option<String>), ContractError> {
    inspect_path(root, root, true)?;
    let settings_path = root.join("settings.json");
    inspect_path(root, &settings_path, false)?;
    let bytes = managed_files::read(root, &settings_path, u64::MAX)?;
    let raw = bytes
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    if let Some(raw) = &raw {
        let projection =
            super::raw_json::scalar_projection(raw.strip_prefix('\u{feff}').unwrap_or(raw));
        let value: Value = serde_json::from_str(&projection)
            .map_err(|_| invalid("存储设置文件格式无效，原文件已保留。"))?;
        if !value.is_object() {
            return Err(invalid("存储设置格式无效，原文件已保留。"));
        }
        if let Some(storage) = value.get("Storage") {
            if !storage.is_object() {
                return Err(invalid("存储版本设置格式无效。"));
            }
            if storage
                .get("LayoutVersion")
                .is_some_and(|version| *version != 1)
            {
                return Err(invalid(
                    "此数据目录的结构版本不受当前程序支持，请使用兼容版本打开。",
                ));
            }
        }
    }
    for directory in ROOT_DIRECTORIES {
        inspect_path(root, &root.join(directory), true)?;
    }
    conversation_index::inspect(root)?;
    if let Some(document) = document {
        for project in document["Projects"].as_array().into_iter().flatten() {
            let folder = root.join("Projects").join(id(&project["Id"])?);
            for path in [
                folder.clone(),
                folder.join("Sessions"),
                folder.join("Memory"),
            ] {
                inspect_path(root, &path, true)?;
            }
            inspect_path(root, &folder.join("project.json"), false)?;
            for chat in project["Chats"].as_array().into_iter().flatten() {
                inspect_session(root, &folder.join("Sessions").join(id(&chat["Id"])?))?;
            }
        }
        for chat in document["Chats"].as_array().into_iter().flatten() {
            inspect_session(root, &root.join("Chats").join(id(&chat["Id"])?))?;
        }
    }
    Ok((
        raw,
        bytes.as_ref().map(|bytes| managed_files::digest(bytes)),
    ))
}

fn plan(arguments: &Value) -> ContractResult {
    let document = &arguments["document"];
    let current = &arguments["current"];
    let mut settings = if current.is_null() {
        json!({})
    } else {
        current.clone()
    };
    let mut storage = settings.get("Storage").cloned().unwrap_or(json!({}));
    storage["LayoutVersion"] = json!(1);
    if storage.get("StoreId").is_none_or(Value::is_null) {
        storage["StoreId"] = arguments["storeId"].clone();
    }
    if storage.get("CreatedAt").is_none_or(Value::is_null) {
        storage["CreatedAt"] = arguments["now"].clone();
    }
    settings["Storage"] = storage;
    let manifests = document["Projects"].as_array().into_iter().flatten().enumerate().map(|(order, project)| json!({"id": project["Id"], "document": {"Version": 1, "Source": "../../catalog.json", "Id": project["Id"], "Name": project["Name"], "FolderPath": project["FolderPath"], "Mounts":project["Mounts"],"DefaultMountId":project["DefaultMountId"], "Order": order, "IsPinned": project["IsPinned"] == true, "IsArchived": project["IsArchived"] == true, "IsFolderlessWorkspace": project["IsFolderlessWorkspace"] == true, "Sessions": project["Chats"].as_array().into_iter().flatten().map(|chat| chat["Id"].clone()).collect::<Vec<_>>()}})).collect::<Vec<_>>();
    Ok(json!({"settings": settings, "manifests": manifests}))
}

fn create_session(root: &Path, session: PathBuf) -> Result<(), ContractError> {
    for path in [
        &session,
        &session.join("attachments"),
        &session.join("Memory"),
    ] {
        managed_files::inspect(root, path, true, false, u64::MAX)?;
    }
    Ok(())
}

/// Layout version is checked before any scaffolding or canonical transaction effects.
/// 任何目录框架或正式事务发生实际效应前，都先检查数据布局版本。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    if operation == "storage.layout.plan" {
        return plan(arguments);
    }
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let document = arguments.get("document");
    let (raw, hash) = inspect(root, document)?;
    if operation == "storage.layout.inspect" {
        return Ok(json!({"raw": raw, "hash": hash}));
    }
    if operation != "storage.layout.ensure" {
        return Err(invalid("未知数据布局操作。"));
    }
    managed_files::inspect(root, root, true, false, u64::MAX)?;
    for name in ROOT_DIRECTORIES {
        managed_files::inspect(root, &root.join(name), true, false, u64::MAX)?;
    }
    let document = document.ok_or_else(|| invalid("会话目录无效。"))?;
    for project in document["Projects"].as_array().into_iter().flatten() {
        let folder = root.join("Projects").join(id(&project["Id"])?);
        for path in [&folder, &folder.join("Sessions"), &folder.join("Memory")] {
            managed_files::inspect(root, path, true, false, u64::MAX)?;
        }
        for chat in project["Chats"].as_array().into_iter().flatten() {
            create_session(root, folder.join("Sessions").join(id(&chat["Id"])?))?;
        }
    }
    for chat in document["Chats"].as_array().into_iter().flatten() {
        create_session(root, root.join("Chats").join(id(&chat["Id"])?))?;
    }
    for manifest in arguments["manifests"].as_array().into_iter().flatten() {
        let path = root
            .join("Projects")
            .join(id(&manifest["id"])?)
            .join("project.json");
        let raw = manifest["raw"]
            .as_str()
            .ok_or_else(|| invalid("项目清单编码无效。"))?;
        let previous = managed_files::read(root, &path, u64::MAX)?;
        if previous
            .as_ref()
            .is_none_or(|bytes| super::trim(&String::from_utf8_lossy(bytes)) != raw)
        {
            managed_files::atomic_write(root, &path, raw.as_bytes(), u64::MAX)?;
        }
    }
    Ok(Value::Null)
}
