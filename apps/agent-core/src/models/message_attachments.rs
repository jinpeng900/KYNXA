//! Project saved attachments into the current model turn without changing the user's original text.
//! 将已保存附件投影到当前模型轮次，保留用户原文；附件正文始终作为资料而非指令。

use std::sync::OnceLock;

use regex::Regex;
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

/// Adapt only this request to verified local capabilities; preserve API contracts, attachments and history.
/// 仅按已核验本地能力调整请求；保留 API 合同、正式附件和历史。
pub(crate) fn project_text_only_model(connection: &Value, history: &mut [Value]) -> bool {
    let managed = connection["providerId"]
        .as_str()
        .is_some_and(|provider| provider.starts_with("kynxa-local-"));
    let known_text_only = connection["capabilities"]
        .as_array()
        .is_some_and(|capabilities| {
            capabilities.iter().any(|capability| capability == "text")
                && !capabilities.iter().any(|capability| capability == "vision")
        });
    managed && known_text_only && remove_images(history)
}

/// Classify explicit upstream image refusals without publishing their body or arbitrary provider metadata.
/// 仅识别上游明确的图片拒绝；原始正文和任意供应商元数据不进入公开诊断。
pub(crate) fn classify_image_rejection(status: u16, body: &Value) -> Value {
    if ![400, 415, 422, 500].contains(&status) {
        return Value::Null;
    }
    let message = body["error"]
        .as_str()
        .or_else(|| body["error"]["message"].as_str())
        .or_else(|| body["message"].as_str())
        .unwrap_or("");
    if message.len() > 65_536 {
        return Value::Null;
    }
    let code = body["error"]["code"]
        .as_str()
        .or_else(|| body["error"]["type"].as_str())
        .or_else(|| body["code"].as_str())
        .unwrap_or("");
    static UNSUPPORTED: OnceLock<Regex> = OnceLock::new();
    static INVALID: OnceLock<Regex> = OnceLock::new();
    let unsupported = UNSUPPORTED.get_or_init(|| Regex::new(r"(?i)(?:\b(?:does|do)\s+not\s+support\b[^\n]{0,80}\b(?:images?|image_url|vision|multimodal)\b|\b(?:images?|image_url|vision|multimodal)\b[^\n]{0,80}\b(?:not\s+supported|unsupported|not\s+allowed)\b|\bunsupported\s+(?:images?|image_url|vision|multimodal)\b)").unwrap());
    let invalid = INVALID.get_or_init(|| Regex::new(r"(?i)(?:\binvalid\s+(?:images?|image_url|vision|multimodal)\b|\b(?:image|image_url|vision|multimodal)\s+(?:input|format|data|url|content)\b[^\n]{0,60}\binvalid\b|\b(?:failed|unable)\s+to\s+(?:decode|load|parse)\s+(?:the\s+)?image\b)").unwrap());
    let reason = if matches!(
        code,
        "unsupported_image" | "unsupported_image_input" | "vision_not_supported"
    ) || unsupported.is_match(message)
    {
        "unsupported"
    } else if matches!(
        code,
        "invalid_image" | "invalid_image_format" | "invalid_image_url" | "image_parse_error"
    ) || invalid.is_match(message)
    {
        "invalid"
    } else {
        return Value::Null;
    };
    json!({"kind":"image-input","reason":reason,"verified":true})
}

pub(crate) fn image_rejection(status: u16, _message: &str, details: &Value) -> bool {
    // The gateway's public 502 is distinct from the provider rejection that authorizes this text fallback.
    // 网关公开的502与供应商实际拒绝不同；只有已核验的图片分类允许文字回退。
    let upstream_status = details["upstreamStatus"]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(status);
    [400, 415, 422, 500].contains(&upstream_status)
        && details["imageRejection"]["kind"] == "image-input"
        && details["imageRejection"]["verified"] == true
        && matches!(
            details["imageRejection"]["reason"].as_str(),
            Some("unsupported" | "invalid")
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_text_only_projection_preserves_saved_content_and_unknown_routes() {
        let original = vec![json!({"role":"user","attachmentMessageId":"message",
            "content":[{"type":"text","text":"用户问题和现有文档文字"},
                {"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}]})];
        let mut projected = original.clone();
        let text = json!({"providerId":"kynxa-local-fixture","capabilities":["text","tools"]});
        assert!(project_text_only_model(&text, &mut projected));
        assert_eq!(projected[0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(projected[0]["content"][0]["text"], "用户问题和现有文档文字");
        assert_eq!(projected[0]["attachmentMessageId"], "message");
        assert_eq!(original[0]["content"].as_array().unwrap().len(), 2);
        assert!(!project_text_only_model(&text, &mut projected));
        for route in [
            json!({"providerId":"cloud","capabilities":["text"]}),
            json!({"providerId":"external-local","baseUrl":"http://127.0.0.1:18080","capabilities":["text"]}),
            json!({"providerId":"kynxa-local-fixture"}),
            json!({"providerId":"kynxa-local-fixture","capabilities":[]}),
            json!({"providerId":"kynxa-local-fixture","capabilities":["text","vision"]}),
        ] {
            let mut unchanged = original.clone();
            assert!(!project_text_only_model(&route, &mut unchanged));
            assert_eq!(unchanged, original);
        }
    }

    #[test]
    fn upstream_image_classification_is_explicit_private_and_works_through_gateway_status() {
        for (message, reason) in [
            ("This model does not support image inputs. fixture-secret", "unsupported"),
            ("Image inputs are not supported fixture-secret", "unsupported"),
            ("Invalid image format fixture-secret", "invalid"),
            ("failed to decode the image fixture-secret", "invalid"),
        ] {
            let classification = classify_image_rejection(400, &json!({"error":{"message":message}}));
            assert_eq!(classification, json!({"kind":"image-input","reason":reason,"verified":true}));
            assert!(!classification.to_string().contains("fixture-secret"));
            let details = json!({"upstreamStatus":400,"imageRejection":classification});
            assert!(image_rejection(502, "模型拒绝了图片输入。", &details));
            assert!(!image_rejection(502, "", &json!({"upstreamStatus":401,"imageRejection":classification})));
        }
        for body in [
            json!({"error":{"message":"Invalid API key for image model"}}),
            json!({"error":{"message":"request (12163 tokens) exceeds the available context size (8192 tokens)"}}),
            json!({"error":{"message":"Unsupported parameter: tools"}}),
        ] {
            assert!(classify_image_rejection(400, &body).is_null());
        }
        assert!(classify_image_rejection(401, &json!({"error":"unsupported image"})).is_null());
        assert!(!image_rejection(400, "unsupported image", &Value::Null));
        assert!(!image_rejection(400, "", &json!({"imageRejection":{"kind":"image-input","verified":false,"reason":"invalid"}})));
    }

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
