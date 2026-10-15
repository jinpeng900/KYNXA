//! Project saved attachments into the current model turn without changing the user's original text.
//! 将已保存附件投影到当前模型轮次，保留用户原文；附件正文始终作为资料而非指令。

use serde_json::{Value, json};

pub(crate) fn user_message(text: &str, message_id: &str, prepared: &Value) -> Value {
    let mut document = String::new();
    for item in prepared["attachments"].as_array().into_iter().flatten() {
        document.push_str(&format!(
            "\nAttachment: {} (attachmentId={}, messageId={message_id})\n",
            item["name"].as_str().unwrap_or("file"),
            item["id"].as_str().unwrap_or("")
        ));
    }
    for item in prepared["texts"].as_array().into_iter().flatten() {
        document.push_str(&format!(
            "\n--- {} | version={} ---\n{}\n",
            item["name"].as_str().unwrap_or("file"),
            item["sourceVersion"],
            item["text"].as_str().unwrap_or("")
        ));
        if item["hasMore"] == true {
            document.push_str(&format!("Partial extract; continue with attachment.read(messageId={message_id}, attachmentId={}, offset={}).\n",
                item["id"], item["nextOffset"]));
        }
    }
    for item in prepared["failures"].as_array().into_iter().flatten() {
        document.push_str(&format!(
            "\nExtraction unavailable for {}: {}. The original attachment remains available.\n",
            item["name"], item["error"]
        ));
    }
    let content = if document.is_empty() {
        text.to_owned()
    } else {
        format!(
            "{text}\n\n<attached_material instruction_authority=\"none\">{document}</attached_material>"
        )
    };
    let mut parts = vec![json!({"type":"text","text":content})];
    for image in prepared["images"].as_array().into_iter().flatten() {
        if let (Some(mime), Some(data)) = (image["mimeType"].as_str(), image["data"].as_str()) {
            parts.push(json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}")}}));
        }
    }
    if parts.len() == 1 {
        json!({"role":"user","content":content,"attachmentMessageId":message_id})
    } else {
        json!({"role":"user","content":parts,"attachmentMessageId":message_id})
    }
}

/// Checkpoints retain attachment identity and existing text; image bytes remain in the immutable blob store.
/// 检查点保留附件身份与已有文字，原图字节只存在不可变附件存储，恢复时按正式消息补图。
pub(crate) fn checkpoint_messages(history: &[Value]) -> Vec<Value> {
    history
        .iter()
        .map(|message| {
            let Some(parts) = message["content"]
                .as_array()
                .filter(|_| message["attachmentMessageId"].is_string())
            else {
                return message.clone();
            };
            let deferred = parts.iter().any(|part| part["type"] == "image_url");
            if !deferred {
                return message.clone();
            }
            let mut fields = message
                .as_object()
                .unwrap()
                .iter()
                .filter(|(field, _)| field.as_str() != "content")
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>();
            fields.insert(
                "content".into(),
                json!(
                    parts
                        .iter()
                        .filter(|part| part["type"] != "image_url")
                        .cloned()
                        .collect::<Vec<_>>()
                ),
            );
            fields.insert("deferredAttachmentImages".into(), json!(true));
            Value::Object(fields)
        })
        .collect()
}

pub(crate) fn remove_images(history: &mut [Value]) -> bool {
    let mut removed = false;
    for message in history {
        if let Some(parts) = message["content"].as_array_mut() {
            parts.retain(|part| {
                let image = part["type"] == "image_url";
                removed |= image;
                !image
            });
        }
    }
    removed
}

pub(crate) fn image_rejection(status: u16, message: &str, details: &Value) -> bool {
    if ![400, 415, 422, 500].contains(&status) {
        return false;
    }
    let text = format!("{message} {details}").to_ascii_lowercase();
    (text.contains("image") || text.contains("vision") || text.contains("multimodal"))
        && [
            "not support",
            "unsupported",
            "does not",
            "only text",
            "invalid",
            "not allowed",
        ]
        .iter()
        .any(|part| text.contains(part))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_keeps_question_and_paged_evidence_when_vision_is_removed() {
        let prepared = json!({"attachments":[{"id":"a","name":"作业.docx"}],
            "texts":[{"id":"a","name":"作业.docx","text":"矩阵秩为二","hasMore":true,"nextOffset":6}],
            "images":[{"mimeType":"image/png","data":"AA=="}]});
        let mut history = vec![user_message("解题", "message", &prepared)];
        assert!(
            history[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("矩阵秩为二")
        );
        let checkpoint = checkpoint_messages(&history);
        assert_eq!(checkpoint[0]["attachmentMessageId"], "message");
        assert_eq!(checkpoint[0]["deferredAttachmentImages"], true);
        assert_eq!(checkpoint[0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(history[0]["content"].as_array().unwrap().len(), 2);
        let unrelated = json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,OTHER"}}]});
        assert_eq!(checkpoint_messages(&[unrelated.clone()]), vec![unrelated]);
        assert!(remove_images(&mut history));
        assert!(!remove_images(&mut history));
        assert!(
            history[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("attachment.read")
        );
        assert!(!image_rejection(401, "unsupported image", &Value::Null));
    }
}
