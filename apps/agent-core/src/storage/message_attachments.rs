//! Immutable user-selected attachments belong to one saved user message and move with its conversation.
//! 用户选择的附件归属于已保存用户消息；不可变快照随会话移动、删除，不把二进制写入 JSONL。

use super::{
    ContractError, ContractResult, conversation_history, conversation_logs, gateway, managed_files,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub const MAXIMUM_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: u64 = 256 * 1024;

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
}
