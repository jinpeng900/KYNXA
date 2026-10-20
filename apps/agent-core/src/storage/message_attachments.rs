//! Immutable user-selected attachments belong to one saved user message and move with its conversation.
//! 用户选择的附件归属于已保存用户消息；不可变快照随会话移动、删除，不把二进制写入 JSONL。

use super::{
    ContractError, ContractResult, conversation_history, conversation_logs, gateway, managed_files,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub const MAXIMUM_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: u64 = 256 * 1024;
/// Work-level listings default to one bounded page; the ceiling keeps one request from scanning unbounded snapshots.
/// 工作级附件目录默认返回一页；上限避免单次请求枚举无界附件快照。
pub const DEFAULT_PROJECT_PAGE_SIZE: usize = 50;
pub const MAXIMUM_PROJECT_PAGE_SIZE: usize = 200;

fn invalid(message: &str) -> ContractError {
    gateway::error(message, "INVALID_MESSAGE_ATTACHMENT", 400)
}
fn id(value: &Value) -> Result<String, ContractError> {
    Ok(super::identity(value)?
        .as_str()
        .unwrap()
        .to_ascii_lowercase())
}

/// Store only identity and display metadata in the formal message; local paths and data URLs stay outside it.
/// 正式消息仅保存身份和显示元信息；本机路径、Data URL 和解析内容不进入附件描述符。
pub(crate) fn normalize_descriptors(message: &mut Value) -> Result<(), ContractError> {
    let Some(items) = message
        .get("Attachments")
        .or_else(|| message.get("attachments"))
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };
    let items = items
        .as_array()
        .ok_or_else(|| invalid("附件列表格式无效。"))?;
    if message["Role"] != "user" || items.len() > 128 {
        return Err(invalid("附件只能属于用户消息，单条消息最多 128 个附件。"));
    }
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for item in items {
        let attachment_id = id(item.get("Id").unwrap_or(&item["id"]))?;
        let name = item
            .get("Name")
            .unwrap_or(&item["name"])
            .as_str()
            .filter(|value| valid_name(value))
            .ok_or_else(|| invalid("附件名称无效。"))?;
        let size = item
            .get("SizeBytes")
            .unwrap_or(&item["sizeBytes"])
            .as_u64()
            .filter(|size| *size <= MAXIMUM_FILE_BYTES)
            .ok_or_else(|| invalid("附件大小无效或超过 256 MiB。"))?;
        if !seen.insert(attachment_id.clone()) {
            return Err(invalid("附件 ID 重复。"));
        }
        normalized.push(json!({"Id":attachment_id,"Name":name,"ContentType":content_type(name),"SizeBytes":size}));
    }
    message["Attachments"] = json!(normalized);
    message.as_object_mut().unwrap().remove("attachments");
    Ok(())
}

pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.encode_utf16().count() <= 512
        && !name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
        && name != "."
        && name != ".."
}

pub fn content_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown",
        "txt" | "mmd" | "mermaid" => "text/plain",
        "json" => "application/json",
        "csv" => "text/csv",
        "html" | "htm" | "xhtml" => "text/html",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ppt" => "application/vnd.ms-powerpoint",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xls" => "application/vnd.ms-excel",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

pub(crate) fn validate_repeated_descriptors(
    previous: &Value,
    current: &Value,
) -> Result<(), ContractError> {
    let entries = |message: &Value| {
        message["Attachments"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    if previous["Role"] == "user" && entries(previous) != entries(current) {
        return Err(gateway::error(
            "此消息 ID 已绑定其他附件，请重新发送。",
            "ATTACHMENT_CONFLICT",
            409,
        ));
    }
    Ok(())
}

/// Only submitted attachment messages require an identity lookup; unrelated old histories remain untouched.
/// 仅为提交了附件的消息查身份；不为校验新附件而读取其他旧聊天或全量正文。
pub(crate) fn validate_catalog_submission(
    root: &Path,
    document: &Value,
    submitted: &[Value],
) -> Result<(), ContractError> {
    for location in submitted {
        let incoming = location["Chat"]["Messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| {
                message["Role"] == "user"
                    && (message.get("Attachments").is_some()
                        || message.get("attachments").is_some())
            })
            .map(|message| gateway::normalize_message(message))
            .collect::<Result<Vec<_>, _>>()?;
        if incoming.is_empty() {
            continue;
        }
        let Some(previous_location) = gateway::locations(document)
            .into_iter()
            .find(|previous| previous["Id"] == location["Id"])
        else {
            continue;
        };
        let ids = incoming
            .iter()
            .map(|message| id(&message["Id"]))
            .collect::<Result<HashSet<_>, _>>()?;
        let read = conversation_history::read_latest(root, &previous_location, Some(&ids), false)?;
        for raw in read.messages {
            let previous: Value = serde_json::from_str(&super::raw_json::scalar_projection(&raw))
                .map_err(|_| invalid("消息日志格式无效。"))?;
            let previous = gateway::normalize_message(&previous)?;
            if let Some(current) = incoming
                .iter()
                .find(|current| current["Id"] == previous["Id"])
            {
                validate_repeated_descriptors(&previous, current)?;
            }
        }
    }
    Ok(())
}

fn scope(
    root: &Path,
    document: &Value,
    conversation: &str,
    message: &str,
) -> Result<(PathBuf, Value), ContractError> {
    let location = gateway::locate(document, conversation)?;
    if location["Chat"]["IsArchived"] == true {
        return Err(gateway::error("聊天已归档。", "CONVERSATION_ARCHIVED", 409));
    }
    let message_id = id(&json!(message))?;
    let selected = HashSet::from([message_id.clone()]);
    let read = conversation_history::read_latest(root, &location, Some(&selected), false)?;
    let original = read
        .messages
        .first()
        .ok_or_else(|| gateway::error("附件所属消息不存在。", "MESSAGE_NOT_FOUND", 404))?;
    let original: Value = serde_json::from_str(&super::raw_json::scalar_projection(original))
        .map_err(|_| invalid("附件所属消息格式无效。"))?;
    let original = gateway::normalize_message(&original)?;
    if original["Role"] != "user" {
        return Err(invalid("附件只能属于用户消息。"));
    }
    let directory = conversation_logs::directory(root, &location)?
        .join("attachments")
        .join(message_id);
    Ok((directory, original))
}

fn descriptor(message: &Value, attachment: &str) -> ContractResult {
    let attachment = id(&json!(attachment))?;
    message["Attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["Id"] == attachment)
        .cloned()
        .ok_or_else(|| gateway::error("此消息没有该附件。", "ATTACHMENT_NOT_FOUND", 404))
}
fn blob_name(name: &str) -> String {
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| value.len() <= 16 && value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .unwrap_or("bin");
    format!("content.{}", extension.to_ascii_lowercase())
}
fn manifest(
    root: &Path,
    directory: &Path,
    attachment: &str,
) -> Result<Option<Value>, ContractError> {
    let path = directory.join(attachment).join("manifest.json");
    Ok(gateway::read_json(root, &path, MAXIMUM_MANIFEST_BYTES)?.0)
}
fn public_metadata(
    conversation: &str,
    message: &str,
    item: &Value,
    saved: Option<&Value>,
) -> Value {
    let mut result = json!({"id":item["Id"],"name":item["Name"],"contentType":item["ContentType"],"sizeBytes":item["SizeBytes"],
        "status":saved.map(|value| value["status"].clone()).unwrap_or(json!("pending"))});
    if let Some(saved) = saved {
        for field in ["sha256", "error", "errorCode"] {
            if let Some(value) = saved.get(field) {
                result[field] = value.clone();
            }
        }
        let url = format!(
            "/api/conversations/{conversation}/attachments/{}/content?messageId={message}",
            item["Id"].as_str().unwrap()
        );
        result["contentUrl"] = json!(url);
        if item["ContentType"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("image/") && kind != "image/svg+xml")
        {
            result["thumbnailUrl"] = result["contentUrl"].clone();
        }
    }
    result
}

pub fn list(root: &Path, conversation: &str, message: &str) -> ContractResult {
    gateway::with_catalog(root, |document, _| {
        let (directory, saved_message) = scope(root, document, conversation, message)?;
        let mut items = Vec::new();
        for item in saved_message["Attachments"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let saved = manifest(root, &directory, item["Id"].as_str().unwrap())?;
            items.push(public_metadata(conversation, message, item, saved.as_ref()));
        }
        Ok(json!({"attachments":items}))
    })
}

/// A single chat's saved attachments feed the workspace tree's virtual read-only folder.
/// 单个聊天的已保存附件为工作区树的只读虚拟目录提供内容；不复制字节。
pub fn conversation_list(root: &Path, conversation: &str) -> ContractResult {
    gateway::with_catalog(root, |document, _| {
        let location = gateway::locate(document, conversation)?;
        let directory = conversation_logs::directory(root, &location)?.join("attachments");
        let selected = message_directories(root, &directory)?
            .into_iter()
            .collect::<HashSet<_>>();
        let mut items = Vec::new();
        if selected.is_empty() {
            return Ok(json!({"attachments":items}));
        }
        let read = conversation_history::read_latest(root, &location, Some(&selected), true)?;
        for raw in read.messages {
            let message: Value = serde_json::from_str(&super::raw_json::scalar_projection(&raw))
                .map_err(|_| invalid("消息日志格式无效。"))?;
            let message = gateway::normalize_message(&message)?;
            if message["Role"] != "user" {
                continue;
            }
            let message_id = message["Id"].as_str().unwrap_or("").to_ascii_lowercase();
            let created_at = message["CreatedAt"].as_str().unwrap_or("").to_owned();
            for item in message["Attachments"].as_array().into_iter().flatten() {
                let attachment = item["Id"].as_str().unwrap_or("").to_ascii_lowercase();
                let message_directory = directory.join(&message_id);
                // Only snapshots that actually exist on disk belong in the mirror.
                // 只有实际存在于磁盘的快照才进入镜像。
                let Some(saved) = manifest(root, &message_directory, &attachment)? else {
                    continue;
                };
                if saved["name"] != item["Name"] || saved["sizeBytes"] != item["SizeBytes"] {
                    continue;
                }
                let mut value = public_metadata(conversation, &message_id, item, Some(&saved));
                value["createdAt"] = json!(created_at);
                value["conversationId"] = json!(conversation);
                value["messageId"] = json!(message_id);
                items.push(value);
            }
        }
        // Recent uploads first so the virtual folder reads like an activity list.
        // 最近上传排在前面，虚拟目录读起来像活动列表。
        items.sort_by(|left, right| right["createdAt"].as_str().cmp(&left["createdAt"].as_str()));
        Ok(json!({"attachments":items}))
    })
}

/// One saved attachment inside a work directory page, keeping its sort identity beside the public shape.
/// 工作目录分页中的单个已保存附件；排序身份与公开返回结构分开保存。
struct ProjectAttachmentRow {
    created_millis: i64,
    conversation: String,
    message: String,
    attachment: String,
    value: Value,
}

/// Only directories named after a message identity can hold saved snapshots for that message.
/// 只有以消息身份命名的目录才可能保存该消息的附件快照；其他名称不参与枚举。
fn message_directories(root: &Path, directory: &Path) -> Result<Vec<String>, ContractError> {
    if !managed_files::inspect(root, directory, false, false, u64::MAX)? {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(directory).map_err(conversation_logs::io_error)? {
        let entry = entry.map_err(conversation_logs::io_error)?;
        if !entry
            .file_type()
            .map_err(conversation_logs::io_error)?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // A linked or malformed entry cannot name a formal message, so it is skipped rather than guessed at.
        // 链接或格式异常的目录项无法对应正式消息，直接跳过而不猜测归属。
        if let Ok(value) = super::identity(&json!(name)) {
            ids.push(value.as_str().unwrap().to_ascii_lowercase());
        }
    }
    Ok(ids)
}

/// Listing is scoped by the work relationship: a chat contributes only its own formally referenced snapshots.
/// 以工作归属限定范围：每个聊天只贡献被本聊天正式消息引用且实际落盘的快照。
fn collect_chat_attachments(
    root: &Path,
    project: &str,
    chat: &Value,
    rows: &mut Vec<ProjectAttachmentRow>,
) -> Result<(), ContractError> {
    let conversation = chat["Id"].as_str().unwrap_or("");
    if conversation.is_empty() {
        return Ok(());
    }
    let title = chat["Title"].as_str().unwrap_or("").to_owned();
    let location = json!({"Id":chat["Id"],"ProjectId":project});
    let directory = conversation_logs::directory(root, &location)?.join("attachments");
    let selected = message_directories(root, &directory)?
        .into_iter()
        .collect::<HashSet<_>>();
    if selected.is_empty() {
        return Ok(());
    }
    let read = conversation_history::read_latest(root, &location, Some(&selected), true)?;
    for raw in read.messages {
        let message: Value = serde_json::from_str(&super::raw_json::scalar_projection(&raw))
            .map_err(|_| invalid("消息日志格式无效。"))?;
        let message = gateway::normalize_message(&message)?;
        if message["Role"] != "user" {
            continue;
        }
        let message_id = message["Id"].as_str().unwrap_or("").to_ascii_lowercase();
        let created_at = message["CreatedAt"].as_str().unwrap_or("").to_owned();
        let created_millis = chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|value| value.timestamp_millis())
            .unwrap_or(0);
        for item in message["Attachments"].as_array().into_iter().flatten() {
            let attachment = item["Id"].as_str().unwrap_or("").to_ascii_lowercase();
            // A descriptor without a matching manifest was never durably saved, so it is not part of this directory.
            // 缺少对应清单的描述符从未真正落盘，不属于实际保存的附件目录。
            let message_directory = directory.join(&message_id);
            let Some(saved) = manifest(root, &message_directory, &attachment)? else {
                continue;
            };
            if saved["name"] != item["Name"] || saved["sizeBytes"] != item["SizeBytes"] {
                continue;
            }
            let mut value = public_metadata(conversation, &message_id, item, Some(&saved));
            value["createdAt"] = json!(created_at);
            value["conversationId"] = json!(conversation);
            value["conversationTitle"] = json!(title);
            value["messageId"] = json!(message_id);
            rows.push(ProjectAttachmentRow {
                created_millis,
                conversation: conversation.to_ascii_lowercase(),
                message: message_id.clone(),
                attachment,
                value,
            });
        }
    }
    Ok(())
}

/// A work directory summarises its own chats' saved attachments without copying bytes or reading linked folders.
/// 工作目录汇总本工作各聊天已保存的附件：不复制二进制，不读取工作关联文件夹，也不跨工作取数。
pub fn project_list(root: &Path, project: &str, offset: usize, limit: usize) -> ContractResult {
    if limit == 0 || limit > MAXIMUM_PROJECT_PAGE_SIZE {
        return Err(gateway::error(
            "分页大小必须在 1–200 之间。",
            "INVALID_ATTACHMENT_PAGE",
            400,
        ));
    }
    gateway::with_catalog(root, |document, _| {
        let relation = super::catalog::apply(
            "storage.catalog.project",
            &json!({"current":document,"id":project}),
        )?;
        let project_id = relation["projectId"].as_str().unwrap_or("");
        let selected = document["Projects"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|candidate| {
                candidate["Id"]
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case(project_id))
            })
            .ok_or_else(|| gateway::error("工作不存在。", "PROJECT_NOT_FOUND", 404))?;
        let mut rows = Vec::new();
        for chat in selected["Chats"].as_array().into_iter().flatten() {
            collect_chat_attachments(root, project_id, chat, &mut rows)?;
        }
        // Newest first, then stable identity order so equal timestamps keep one reproducible page boundary.
        // 时间倒序，再用稳定身份排序；时间相同时分页边界仍然可复现。
        rows.sort_by(|left, right| {
            right
                .created_millis
                .cmp(&left.created_millis)
                .then_with(|| left.conversation.cmp(&right.conversation))
                .then_with(|| left.message.cmp(&right.message))
                .then_with(|| left.attachment.cmp(&right.attachment))
        });
        let total = rows.len();
        let page = rows
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|row| row.value)
            .collect::<Vec<_>>();
        let has_more = offset.saturating_add(page.len()) < total;
        Ok(json!({"attachments":page,"offset":offset,"limit":limit,"total":total,"hasMore":has_more}))
    })
}

/// Exact retries reuse the immutable snapshot; reusing an attachment ID for different bytes is a conflict.
/// 同字节重试复用不可变快照；同一附件 ID 不能替换成另一份文件。
pub fn save(
    root: &Path,
    conversation: &str,
    message: &str,
    attachment: &str,
    name: &str,
    bytes: &[u8],
) -> ContractResult {
    gateway::with_catalog(root, |document, _| {
        let attachment_id = id(&json!(attachment))?;
        if !valid_name(name) || bytes.len() as u64 > MAXIMUM_FILE_BYTES {
            return Err(invalid("附件名称或大小无效。"));
        }
        let (directory, item) = match scope(root, document, conversation, message) {
            Ok((directory, saved_message)) => (directory, descriptor(&saved_message, attachment)?),
            Err(error) if error.code() == "MESSAGE_NOT_FOUND" => {
                // Steering uploads may reserve bytes before its user message is committed; reads still require that message.
                // 引导上传可在用户消息提交前预留字节；读取和模型使用仍必须核对正式消息引用。
                let location = gateway::locate(document, conversation)?;
                let directory = conversation_logs::directory(root, &location)?
                    .join("attachments")
                    .join(id(&json!(message))?);
                (
                    directory,
                    json!({"Id":attachment_id,"Name":name,"ContentType":content_type(name),"SizeBytes":bytes.len()}),
                )
            }
            Err(error) => return Err(error),
        };
        if item["Name"] != name || item["SizeBytes"].as_u64() != Some(bytes.len() as u64) {
            return Err(invalid("选择的文件与消息附件不一致，请重新选择后发送。"));
        }
        let attachment = item["Id"].as_str().unwrap();
        let sha = managed_files::digest(bytes);
        if let Some(saved) = manifest(root, &directory, attachment)? {
            if saved["sha256"] != sha {
                return Err(gateway::error(
                    "附件已存在且内容不同。",
                    "ATTACHMENT_CONFLICT",
                    409,
                ));
            }
            return Ok(public_metadata(conversation, message, &item, Some(&saved)));
        }
        let folder = directory.join(attachment);
        managed_files::atomic_write(
            root,
            &folder.join(blob_name(name)),
            bytes,
            MAXIMUM_FILE_BYTES,
        )?;
        let direct_image = matches!(
            content_type(name),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        );
        let saved = json!({"version":1,"id":attachment,"messageId":message,"name":name,"sha256":sha,"sizeBytes":bytes.len(),"status":if direct_image {"ready"} else {"pending"}});
        gateway::commit_json(
            root,
            &folder.join("manifest.json"),
            &saved,
            None,
            MAXIMUM_MANIFEST_BYTES,
        )?;
        Ok(public_metadata(conversation, message, &item, Some(&saved)))
    })
}

pub fn read(
    root: &Path,
    conversation: &str,
    message: &str,
    attachment: &str,
) -> Result<(Value, Vec<u8>), ContractError> {
    gateway::with_catalog(root, |document, _| {
        let (directory, saved_message) = scope(root, document, conversation, message)?;
        let item = descriptor(&saved_message, attachment)?;
        let attachment = item["Id"].as_str().unwrap();
        let saved = manifest(root, &directory, attachment)?
            .ok_or_else(|| gateway::error("附件尚未导入。", "ATTACHMENT_NOT_READY", 409))?;
        if saved["name"] != item["Name"] || saved["sizeBytes"] != item["SizeBytes"] {
            return Err(gateway::error(
                "正式消息与附件快照不一致。",
                "ATTACHMENT_CONFLICT",
                409,
            ));
        }
        let bytes = managed_files::read(
            root,
            &directory
                .join(attachment)
                .join(blob_name(item["Name"].as_str().unwrap())),
            MAXIMUM_FILE_BYTES,
        )?
        .ok_or_else(|| gateway::error("附件文件不存在。", "ATTACHMENT_NOT_FOUND", 404))?;
        if saved["sha256"] != managed_files::digest(&bytes) {
            return Err(gateway::error(
                "附件快照已变化。",
                "ATTACHMENT_VERSION_CHANGED",
                409,
            ));
        }
        Ok((
            public_metadata(conversation, message, &item, Some(&saved)),
            bytes,
        ))
    })
}

pub fn cached_text(
    root: &Path,
    conversation: &str,
    message: &str,
    attachment: &str,
) -> Result<Option<Value>, ContractError> {
    gateway::with_catalog(root, |document, _| {
        let (directory, saved_message) = scope(root, document, conversation, message)?;
        let item = descriptor(&saved_message, attachment)?;
        Ok(gateway::read_json(
            root,
            &directory
                .join(item["Id"].as_str().unwrap())
                .join("extracted.json"),
            64 * 1024 * 1024,
        )?
        .0)
    })
}

pub fn save_extraction(
    root: &Path,
    conversation: &str,
    message: &str,
    attachment: &str,
    expected_sha: &str,
    extracted: &Value,
) -> ContractResult {
    gateway::with_catalog(root, |document, _| {
        let (directory, saved_message) = scope(root, document, conversation, message)?;
        let item = descriptor(&saved_message, attachment)?;
        let attachment = item["Id"].as_str().unwrap();
        let folder = directory.join(attachment);
        let (saved, manifest_hash) =
            gateway::read_json(root, &folder.join("manifest.json"), MAXIMUM_MANIFEST_BYTES)?;
        let mut saved =
            saved.ok_or_else(|| gateway::error("附件尚未导入。", "ATTACHMENT_NOT_READY", 409))?;
        if saved["sha256"] != expected_sha {
            return Err(gateway::error(
                "附件版本已变化。",
                "ATTACHMENT_VERSION_CHANGED",
                409,
            ));
        }
        let cache_hash =
            gateway::read_json(root, &folder.join("extracted.json"), 64 * 1024 * 1024)?.1;
        gateway::commit_json(
            root,
            &folder.join("extracted.json"),
            extracted,
            cache_hash.as_deref(),
            64 * 1024 * 1024,
        )?;
        saved["status"] = json!(if extracted.get("error").is_some() {
            "error"
        } else {
            "ready"
        });
        for field in ["error", "errorCode"] {
            if let Some(value) = extracted.get(field) {
                saved[field] = value.clone();
            }
        }
        gateway::commit_json(
            root,
            &folder.join("manifest.json"),
            &saved,
            manifest_hash.as_deref(),
            MAXIMUM_MANIFEST_BYTES,
        )?;
        Ok(public_metadata(conversation, message, &item, Some(&saved)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (PathBuf, String, String, String) {
        let root =
            std::env::temp_dir().join(format!("kynxa-message-attachments-{}", gateway::new_id()));
        let conversation = gateway::new_id();
        let message = gateway::new_id();
        let attachment = gateway::new_id();
        gateway::ensure_conversation(&root, &conversation, "Attachment fixture").unwrap();
        (root, conversation, message, attachment)
    }

    #[test]
    fn conversation_list_returns_saved_attachments_for_one_chat_only() {
        let (root, conversation, message, attachment) = setup();
        gateway::upsert_message(&root, &conversation, &json!({"Id":message,"Role":"user","Content":"上传",
            "Attachments":[{"Id":attachment,"Name":"清单.txt","SizeBytes":4}]})).unwrap();
        save(&root, &conversation, &message, &attachment, "清单.txt", b"data").unwrap();
        let foreign_chat = gateway::new_id();
        gateway::ensure_conversation(&root, &foreign_chat, "Other chat").unwrap();
        let foreign_message = gateway::new_id();
        let foreign_attachment = gateway::new_id();
        gateway::upsert_message(&root, &foreign_chat, &json!({"Id":foreign_message,"Role":"user","Content":"上传",
            "Attachments":[{"Id":foreign_attachment,"Name":"他人.txt","SizeBytes":4}]})).unwrap();
        save(&root, &foreign_chat, &foreign_message, &foreign_attachment, "他人.txt", b"data").unwrap();
        let listed = conversation_list(&root, &conversation).unwrap();
        let items = listed["attachments"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], attachment);
        assert!(listed.to_string().contains("清单.txt"), "{listed}");
        assert!(!listed.to_string().contains("他人.txt"), "{listed}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn attachment_snapshots_bind_saved_messages_and_reject_foreign_scope_or_replacement() {
        let (root, conversation, message, attachment) = setup();
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap();
        // A steering upload can precede the user message, but is not readable until the formal reference exists.
        // 引导可先上传附件；正式消息引用提交前，不能读取该预留附件。
        save(
            &root,
            &conversation,
            &message,
            &attachment,
            "图像.png",
            &png,
        )
        .unwrap();
        assert!(read(&root, &conversation, &message, &attachment).is_err());
        gateway::upsert_message(&root, &conversation, &json!({"Id":message,"Role":"user","Content":"",
            "Attachments":[{"id":attachment,"name":"图像.png","contentType":"image/png","sizeBytes":png.len(),"status":"ready","thumbnailUrl":"file:///private"}]})).unwrap();
        let messages = gateway::read_messages(&root, &conversation, true).unwrap();
        assert_eq!(
            messages[0]["Attachments"][0],
            json!({"Id":attachment,"Name":"图像.png","ContentType":"image/png","SizeBytes":png.len()})
        );
        let (metadata, restored) = read(&root, &conversation, &message, &attachment).unwrap();
        assert_eq!(restored, png);
        assert_eq!(metadata["sha256"], managed_files::digest(&png));
        assert!(
            metadata["thumbnailUrl"]
                .as_str()
                .unwrap()
                .contains(&message)
        );
        let foreign = gateway::new_id();
        gateway::ensure_conversation(&root, &foreign, "Other chat").unwrap();
        assert!(read(&root, &foreign, &message, &attachment).is_err());
        let different = vec![1; png.len()];
        assert_eq!(
            save(
                &root,
                &conversation,
                &message,
                &attachment,
                "图像.png",
                &different
            )
            .unwrap_err()
            .code(),
            "ATTACHMENT_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_docx_is_extracted_from_snapshot_and_paged_without_an_index() {
        let (root, conversation, message, attachment) = setup();
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/documents/account-guide.docx"),
        )
        .unwrap();
        gateway::upsert_message(&root, &conversation, &json!({"Id":message,"Role":"user","Content":"Read this document",
            "Attachments":[{"Id":attachment,"Name":"文档.docx","ContentType":"application/octet-stream","SizeBytes":bytes.len()}]})).unwrap();
        save(
            &root,
            &conversation,
            &message,
            &attachment,
            "文档.docx",
            &bytes,
        )
        .unwrap();
        let config = crate::gateway_contract::GatewayConfig {
            data_root: root.clone(),
            data_home: root.join("Models"),
            extension_root: root.join("Extensions"),
            application_root: root.clone(),
            toolhost_exe: root.join("tool-host.exe"),
            inference_library: root.join("onnxruntime.dll"),
            vector_extension: root.join("vec0.dll"),
            port: 0,
        };
        let parsed = crate::retrieval::gateway_message_attachments::extract(
            &config,
            &conversation,
            &message,
            &attachment,
            &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        assert!(
            parsed["text"]
                .as_str()
                .unwrap()
                .contains("How to reset an account password")
        );
        let first = crate::retrieval::gateway_message_attachments::page(&parsed, 0, 40);
        let second = crate::retrieval::gateway_message_attachments::page(
            &parsed,
            first["nextOffset"].as_u64().unwrap() as usize,
            40,
        );
        assert_eq!(first["hasMore"], true);
        assert_ne!(first["text"], second["text"]);
        assert_eq!(
            list(&root, &conversation, &message).unwrap()["attachments"][0]["status"],
            "ready"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Publish works and folderless chats through the same catalog route the desktop uses.
    /// 通过与桌面相同的目录接口发布工作与无归属聊天，避免测试另造一份正式存储。
    fn publish(root: &Path, projects: Value, chats: Value) {
        let catalog = gateway::load_catalog(root).unwrap();
        let (status, body) = gateway::handle(
            "PUT",
            "/api/conversations/catalog",
            &[],
            &json!({"Revision":catalog["Revision"],"Chats":chats,"Projects":projects}),
            root,
        )
        .unwrap()
        .unwrap();
        assert!(status < 400, "{body}");
    }

    fn saved_chat(chat: &str, title: &str, message: &str, attachment: &str, name: &str, created: &str) -> Value {
        json!({"Id":chat,"Title":title,"Draft":"","Messages":[
            {"Id":message,"Role":"user","Content":"上传附件","CreatedAt":created,
             "Attachments":[{"Id":attachment,"Name":name,"ContentType":"text/plain","SizeBytes":4}]}]})
    }

    #[test]
    fn project_list_stays_empty_without_saved_attachments() {
        let (root, _, _, _) = setup();
        let project = gateway::new_id();
        let chat = gateway::new_id();
        let message = gateway::new_id();
        publish(
            &root,
            json!([{"Id":project,"Name":"Empty work","FolderPath":null,"IsFolderlessWorkspace":true,
                "Chats":[{"Id":chat,"Title":"Chat without files","Draft":"","Messages":[
                    {"Id":message,"Role":"user","Content":"没有附件","CreatedAt":"2026-10-12T00:00:00Z"}]}]}]),
            json!([]),
        );
        let page = project_list(&root, &project, 0, 50).unwrap();
        assert_eq!(page["attachments"], json!([]));
        assert_eq!(page["total"], 0);
        assert_eq!(page["hasMore"], false);
        assert_eq!(page["offset"], 0);
        assert_eq!(page["limit"], 50);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_list_pages_saved_attachments_newest_first_without_overlap() {
        let (root, _, _, _) = setup();
        let project = gateway::new_id();
        let mut chats = Vec::new();
        let mut saved = Vec::new();
        for index in 0..4 {
            let chat = gateway::new_id();
            let message = gateway::new_id();
            let attachment = gateway::new_id();
            let name = format!("文件{index}.txt");
            chats.push(saved_chat(
                &chat,
                &format!("Chat {index}"),
                &message,
                &attachment,
                &name,
                &format!("2026-10-1{index}T00:00:00Z"),
            ));
            saved.push((chat, message, attachment, name));
        }
        publish(
            &root,
            json!([{"Id":project,"Name":"Paged work","FolderPath":null,"IsFolderlessWorkspace":true,"Chats":chats}]),
            json!([]),
        );
        for (chat, message, attachment, name) in &saved {
            save(&root, chat, message, attachment, name, b"data").unwrap();
        }
        let first = project_list(&root, &project, 0, 2).unwrap();
        assert_eq!(first["total"], 4);
        assert_eq!(first["hasMore"], true);
        assert_eq!(first["attachments"][0]["name"], "文件3.txt");
        assert_eq!(first["attachments"][1]["name"], "文件2.txt");
        let second = project_list(&root, &project, 2, 2).unwrap();
        assert_eq!(second["total"], 4);
        assert_eq!(second["hasMore"], false);
        assert_eq!(second["attachments"][0]["name"], "文件1.txt");
        let mut ids = first["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["attachments"].as_array().unwrap())
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4, "分页不能重复或遗漏附件");
        let empty = project_list(&root, &project, 9, 2).unwrap();
        assert_eq!(empty["attachments"], json!([]));
        assert_eq!(empty["hasMore"], false);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_list_never_reaches_another_works_attachments() {
        let (root, _, _, _) = setup();
        let first_project = gateway::new_id();
        let second_project = gateway::new_id();
        let folderless_chat = gateway::new_id();
        let first = (gateway::new_id(), gateway::new_id(), gateway::new_id());
        let second = (gateway::new_id(), gateway::new_id(), gateway::new_id());
        let loose = (gateway::new_id(), gateway::new_id(), gateway::new_id());
        publish(
            &root,
            json!([
                {"Id":first_project,"Name":"First work","FolderPath":null,"IsFolderlessWorkspace":true,
                 "Chats":[saved_chat(&first.0, "First chat", &first.1, &first.2, "工作一.txt", "2026-10-12T00:00:00Z")]},
                {"Id":second_project,"Name":"Second work","FolderPath":null,"IsFolderlessWorkspace":true,
                 "Chats":[saved_chat(&second.0, "Second chat", &second.1, &second.2, "工作二.txt", "2026-10-12T00:00:00Z")]}
            ]),
            json!([saved_chat(&folderless_chat, "No work", &loose.1, &loose.2, "无归属.txt", "2026-10-12T00:00:00Z")]),
        );
        for (chat, message, attachment, name) in [
            (&first.0, &first.1, &first.2, "工作一.txt"),
            (&second.0, &second.1, &second.2, "工作二.txt"),
            (&folderless_chat, &loose.1, &loose.2, "无归属.txt"),
        ] {
            save(&root, chat, message, attachment, name, b"data").unwrap();
        }
        let first_page = project_list(&root, &first_project, 0, 50).unwrap();
        assert_eq!(first_page["total"], 1);
        assert_eq!(first_page["attachments"][0]["id"], first.2);
        assert_eq!(first_page["attachments"][0]["conversationId"], first.0);
        let second_page = project_list(&root, &second_project, 0, 50).unwrap();
        assert_eq!(second_page["total"], 1);
        assert_eq!(second_page["attachments"][0]["id"], second.2);
        assert_eq!(
            project_list(&root, &gateway::new_id(), 0, 50).unwrap_err().code(),
            "PROJECT_NOT_FOUND"
        );
        assert_eq!(
            project_list(&root, &first_project, 0, 0).unwrap_err().code(),
            "INVALID_ATTACHMENT_PAGE"
        );
        assert_eq!(
            project_list(&root, &first_project, 0, MAXIMUM_PROJECT_PAGE_SIZE + 1).unwrap_err().code(),
            "INVALID_ATTACHMENT_PAGE"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
