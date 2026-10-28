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

/// An image without recognized text still has its immutable original available for pixel access.
/// 图片未识别出文字不代表原件失效；像素仍可按正式附件身份读取。
fn normalize_image_text_state(extracted: &mut Value, image: bool) -> bool {
    if !image { return false; }
    let empty_ocr = extracted["errorCode"] == "DOCUMENT_TEXT_UNAVAILABLE";
    if (extracted.get("error").is_some() || extracted.get("errorCode").is_some()) && !empty_ocr { return false; }
    let text_state = if empty_ocr || extracted["text"].as_str().unwrap_or("").trim().is_empty() { "empty" } else { "available" };
    let changed = empty_ocr || extracted["textState"] != text_state || extracted["originalAvailable"] != true;
    if empty_ocr {
        let object = extracted.as_object_mut().unwrap();
        object.remove("error");
        object.remove("errorCode");
        extracted["text"] = json!("");
        extracted["complete"] = json!(true);
        extracted["extraction"] = json!({"format":"image","rawContentHash":extracted["sourceVersion"],
            "textState":"empty","complete":true});
    }
    extracted["textState"] = json!(text_state);
    extracted["originalAvailable"] = json!(true);
    changed
}

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
    let image = crate::document_formats::detect(Path::new(metadata["name"].as_str().unwrap()))
        .is_some_and(|format| format.family == crate::document_formats::ParserFamily::Image);
    if let Some(mut cached) =
        message_attachments::cached_text(&config.data_root, conversation, message, attachment)?
    {
        if cached["sourceVersion"] == sha && cached["parserVersion"] == "message-attachment-v1" {
            if normalize_image_text_state(&mut cached, image) {
                message_attachments::save_extraction(&config.data_root, conversation, message, attachment, sha, &cached)?;
            }
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
    let mut extracted = match parsed {
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
    normalize_image_text_state(&mut extracted, image);
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
    for field in ["error", "errorCode", "textState", "originalAvailable"] {
        if let Some(value) = extracted.get(field) {
            result[field] = value.clone();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_without_ocr_text_keeps_original_version_and_pages_as_empty_text() {
        let mut extracted = json!({"parserVersion":"message-attachment-v1","sourceVersion":"original-sha",
            "text":"","complete":false,"error":"No recognized text","errorCode":"DOCUMENT_TEXT_UNAVAILABLE"});
        assert!(normalize_image_text_state(&mut extracted, true));
        let page = page(&extracted, 0, 24000);
        assert_eq!(page["sourceVersion"], "original-sha");
        assert_eq!(page["textState"], "empty");
        assert_eq!(page["originalAvailable"], true);
        assert_eq!(page["text"], "");
        assert_eq!(page["totalChars"], 0);
        assert_eq!(page["hasMore"], false);
        assert_eq!(page["complete"], true);
        assert!(page.get("error").is_none() && page.get("errorCode").is_none());
        assert!(!normalize_image_text_state(&mut extracted, true));
    }

    #[test]
    fn image_text_classification_preserves_other_failures_and_document_contracts() {
        for (image, code) in [(false, "DOCUMENT_TEXT_UNAVAILABLE"), (true, "OCR_UNAVAILABLE"), (true, "IMAGE_INVALID_CONTENT")] {
            let original = json!({"sourceVersion":"saved-sha","text":"","complete":false,
                "error":"Original extraction error","errorCode":code});
            let mut extracted = original.clone();
            assert!(!normalize_image_text_state(&mut extracted, image));
            assert_eq!(extracted, original);
        }
        let mut available = json!({"sourceVersion":"saved-sha","text":"visible text","complete":true});
        assert!(normalize_image_text_state(&mut available, true));
        assert_eq!(page(&available, 0, 24000)["textState"], "available");
        assert_eq!(available["text"], "visible text");
    }
}
