use super::{ContractResult, conversation_logs, managed_files, raw_json};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
struct Transaction<'a> {
    #[serde(rename = "NextCatalog", borrow)]
    next: &'a RawValue,
    #[serde(rename = "Writes", borrow)]
    writes: Vec<Write<'a>>,
}

#[derive(Deserialize)]
struct Write<'a> {
    #[serde(rename = "Location")]
    location: Value,
    #[serde(rename = "Message", borrow)]
    message: &'a RawValue,
}

/// The durable intent owns directory moves and canonical publication, including restart replay.
/// 持久事务意图拥有目录移动和正式发布，包括进程重启后的重放。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let _guard = conversation_logs::CONVERSATION_TRANSACTIONS
        .lock()
        .map_err(|_| {
            conversation_logs::error("会话存储锁失效。", "CONVERSATION_STORAGE_FAILED", 500)
        })?;
    conversation_logs::check_cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let catalog_path = root.join("catalog.json");
    let journal_path = root.join(".catalog-transaction.json");
    let raw = arguments["rawTransaction"].as_str().ok_or_else(|| {
        conversation_logs::error(
            "会话存储恢复文件格式无效。",
            "INVALID_CONVERSATION_DATA",
            400,
        )
    })?;
    let projection: Value =
        serde_json::from_str(&raw_json::scalar_projection(raw)).map_err(|_| {
            conversation_logs::error(
                "会话存储恢复文件格式无效。",
                "INVALID_CONVERSATION_DATA",
                400,
            )
        })?;
    if projection["Version"] != 1
        || !projection["Moves"].is_array()
        || !projection["Writes"].is_array()
        || projection["NextCatalog"]["Version"] != 1
        || !projection["NextCatalog"]["Revision"].is_u64()
    {
        return Err(conversation_logs::error(
            "会话存储恢复文件格式无效。",
            "INVALID_CONVERSATION_DATA",
            400,
        ));
    }
    let transaction: Transaction<'_> = serde_json::from_str(raw).map_err(|_| {
        conversation_logs::error(
            "会话存储恢复文件格式无效。",
            "INVALID_CONVERSATION_DATA",
            400,
        )
    })?;
    // Validate every move and append destination before writing durable intent or touching a directory.
    // 写入持久意图或改动任何目录前，先验证全部移动及追加目标。
    for item in projection["Moves"].as_array().unwrap() {
        for location in [&item["From"], &item["To"]] {
            let path = conversation_logs::directory(root, location)?;
            managed_files::inspect(root, &path, false, false, u64::MAX)?;
        }
    }
    for item in &transaction.writes {
        managed_files::inspect(
            root,
            &conversation_logs::directory(root, &item.location)?,
            false,
            false,
            u64::MAX,
        )?;
    }
    let journal = managed_files::read(root, &journal_path, u64::MAX)?;
    if operation == "storage.conversations.commit" {
        if journal.is_some() {
            return Err(conversation_logs::error(
                "先前目录事务尚未完成，请先恢复。",
                "STORAGE_CONFLICT",
                409,
            ));
        }
        let current = managed_files::read(root, &catalog_path, u64::MAX)?;
        if current
            .as_ref()
            .map(|bytes| managed_files::digest(bytes))
            .as_deref()
            != arguments["expectedHash"].as_str()
        {
            return Err(conversation_logs::error(
                "聊天目录已更新，请重新读取后重试。",
                "CATALOG_CONFLICT",
                409,
            ));
        }
        managed_files::atomic_write(root, &journal_path, raw.as_bytes(), u64::MAX)?;
    } else if operation == "storage.conversations.apply" {
        if journal
            .as_ref()
            .map(|bytes| managed_files::digest(bytes))
            .as_deref()
            != arguments["expectedJournalHash"].as_str()
        {
            return Err(conversation_logs::error(
                "会话恢复意图已改变，原文件已保留。",
                "STORAGE_CONFLICT",
                409,
            ));
        }
        if journal.is_none() {
            return Err(conversation_logs::error(
                "会话恢复意图不存在。",
                "STORAGE_CONFLICT",
                409,
            ));
        }
    } else {
        return Err(conversation_logs::error(
            "未知会话事务操作。",
            "INVALID_CONVERSATION_DATA",
            400,
        ));
    }
    for item in projection["Moves"].as_array().unwrap() {
        conversation_logs::check_cancelled()?;
        let from = conversation_logs::directory(root, &item["From"])?;
        let to = conversation_logs::directory(root, &item["To"])?;
        if from == to || !managed_files::inspect(root, &from, false, false, u64::MAX)? {
            continue;
        }
        if managed_files::inspect(root, &to, false, false, u64::MAX)? {
            return Err(conversation_logs::error(
                "聊天记录目标目录已存在，已保留两处数据。",
                "STORAGE_CONFLICT",
                500,
            ));
        }
        managed_files::inspect(root, to.parent().unwrap(), true, false, u64::MAX)?;
        fs::rename(from, to).map_err(conversation_logs::io_error)?;
    }
    for item in &transaction.writes {
        conversation_logs::check_cancelled()?;
        conversation_logs::append_unlocked(
            root,
            &item.location,
            &raw_json::compact(item.message.get()),
            false,
        )?;
    }
    let catalog_bytes = arguments["rawCatalog"]
        .as_str()
        .unwrap_or(transaction.next.get())
        .as_bytes()
        .to_vec();
    managed_files::atomic_write(root, &catalog_path, &catalog_bytes, u64::MAX)?;
    // A completed canonical replacement is the receipt. Cleanup never turns it into a cancelled write.
    // 正式替换完成即为提交回执；清理不能将其错误报告为已取消写入。
    let cleanup_pending = match fs::remove_file(&journal_path) {
        Ok(()) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    };
    Ok(json!({"hash": managed_files::digest(&catalog_bytes), "cleanupPending": cleanup_pending}))
}
