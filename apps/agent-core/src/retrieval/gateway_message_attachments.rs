//! Attachment extraction reuses document decoders and a conversation-owned cache, without indexing private uploads globally.
//! 附件提取复用文档解析器和会话自有缓存；上传不会自动共享到全局索引或记忆。

use crate::{
    gateway_contract::GatewayConfig,
    storage::{ContractResult, message_attachments},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) fn extract(
    config: &GatewayConfig,
    conversation: &str,
    message: &str,
    attachment: &str,
    cancel: &Arc<AtomicBool>,
) -> ContractResult {
    let (metadata, bytes) =
        message_attachments::read(&config.data_root, conversation, message, attachment)?;
    let sha = metadata["sha256"].as_str().unwrap();
    if let Some(cached) =
        message_attachments::cached_text(&config.data_root, conversation, message, attachment)?
    {
        if cached["sourceVersion"] == sha && cached["parserVersion"] == "message-attachment-v1" {
            return Ok(cached);
        }
    }
    let limits = json!({"maximumSourceBytes":message_attachments::MAXIMUM_FILE_BYTES,
        "maximumDocumentInputBytes":message_attachments::MAXIMUM_FILE_BYTES,"maximumDocumentOutputBytes":32*1024*1024,"maximumPdfPages":100});
    let parsed = if bytes.is_empty() {
        Ok(json!({"text":""}))
    } else {
        super::gateway_documents::parse_bytes(
            Some(config),
            Path::new(metadata["name"].as_str().unwrap()),
            &bytes,
            &limits,
            cancel,
        )
    };
    let extracted = match parsed {
        Ok(parsed) => {
            let mut text = String::new();
            if let Some(windows) = parsed["documentWindows"].as_array() {
                for window in windows {
                    text.push_str(window["text"].as_str().unwrap_or(""));
                }
            } else {
                text.push_str(parsed["text"].as_str().unwrap_or(""));
            }
            json!({"parserVersion":"message-attachment-v1","sourceVersion":sha,"text":text,
                "extraction":parsed["extraction"],"complete":parsed["documentComplete"] != false && parsed["extraction"]["complete"] != false})
        }
        Err(error) => {
            if error.code() == "ABORT_ERR" {
                return Err(error);
            }
            json!({"parserVersion":"message-attachment-v1","sourceVersion":sha,"text":"","complete":false,"error":error.message(),"errorCode":error.code()})
        }
    };
    message_attachments::save_extraction(
        &config.data_root,
        conversation,
        message,
        attachment,
        sha,
        &extracted,
    )?;
    Ok(extracted)
}

/// UTF-16 positions match the desktop and existing document.read paging contract.
/// UTF-16 偏移与桌面及现有 document.read 分页合同一致，续读不会切断代理对。
pub(crate) fn page(extracted: &Value, offset: usize, max_chars: usize) -> Value {
    let text = extracted["text"].as_str().unwrap_or("");
    let mut start = 0;
    let mut body = String::new();
    let mut end = 0;
    let mut started = false;
    for character in text.chars() {
        let next = end + character.len_utf16();
        if !started && next > offset {
            start = end;
            started = true;
        }
        if started {
            if end - start + character.len_utf16() > max_chars && !body.is_empty() {
                break;
            }
            body.push(character);
        }
        end = next;
    }
    let total = text.encode_utf16().count();
    if !started {
        start = total;
        end = total;
    }
    let mut result = json!({"text":body,"offset":start,"nextOffset":end,"totalChars":total,"hasMore":end<total,
        "offsetUnit":"utf16-code-units","sourceVersion":extracted["sourceVersion"],"extraction":extracted["extraction"],"complete":extracted["complete"]});
    for field in ["error", "errorCode"] {
        if let Some(value) = extracted.get(field) {
            result[field] = value.clone();
        }
    }
    result
}
