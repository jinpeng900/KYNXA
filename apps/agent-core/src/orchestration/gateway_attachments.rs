//! Native attachment HTTP and model preparation reuse immutable message storage and existing decoders.
//! 原生附件 HTTP 与模型准备复用不可变消息存储和现有解析器；单个附件失败不终止整条聊天。

use super::gateway_runtime::GatewayRuntime;
use crate::{gateway_contract::GatewayError, storage::message_attachments};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) struct AttachmentContent {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub sha256: String,
}

pub(crate) async fn import(
    runtime: &GatewayRuntime,
    conversation: &str,
    body: &Value,
) -> Result<Value, GatewayError> {
    let message = body["messageId"].as_str().unwrap_or("").to_owned();
    let files = body["files"]
        .as_array()
        .filter(|files| !files.is_empty() && files.len() <= 128)
        .ok_or_else(|| {
            GatewayError::new("INVALID_MESSAGE_ATTACHMENT", "请选择 1–128 个附件。", 400)
        })?;
    let context = crate::gateway::workspace_tool_context(runtime, conversation).await?;
    let mut attachments = Vec::new();
    for file in files {
        let id = file["id"].as_str().unwrap_or("").to_owned();
        let name = file["name"].as_str().unwrap_or("").to_owned();
        let requested = file["path"].as_str().unwrap_or("");
        if !Path::new(requested).is_absolute() || !message_attachments::valid_name(&name) {
            return Err(GatewayError::new(
                "INVALID_MESSAGE_ATTACHMENT",
                "附件路径或名称无效。",
                400,
            ));
        }
        let (path, _) = crate::tools::gateway::canonical_authorized(&context, requested, false)?;
        if crate::tools::gateway::protected_path(&context, &path) {
            return Err(GatewayError::new(
                "PROTECTED_APP_DATA",
                "不能附加应用私有数据或工具目录。",
                403,
            ));
        }
        let root = runtime.config.data_root.clone();
        let conversation = conversation.to_owned();
        let message = message.clone();
        let selected = tokio::task::spawn_blocking(move || {
            // The native picker authorizes this exact selected file; it does not grant its parent directory.
            // 原生选择器授权本次选中的精确文件，不因此登记或授权整个父目录。
            let (bytes, _) = crate::tools::service_source_reading::read_binary_stable(
                &path,
                message_attachments::MAXIMUM_FILE_BYTES,
                &None,
            )
            .map_err(|error| GatewayError::new(error.code, error.message, error.status_code))?;
            message_attachments::save(&root, &conversation, &message, &id, &name, &bytes)
                .map_err(GatewayError::from)
        })
        .await
        .map_err(|_| GatewayError::new("ATTACHMENT_WORKER_FAILED", "附件保存线程结束。", 500))??;
        attachments.push(selected);
    }
    Ok(json!({"attachments":attachments}))
}

pub(crate) async fn list(
    runtime: &GatewayRuntime,
    conversation: &str,
    message: &str,
) -> Result<Value, GatewayError> {
    let root = runtime.config.data_root.clone();
    let conversation = conversation.to_owned();
    let message = message.to_owned();
    tokio::task::spawn_blocking(move || message_attachments::list(&root, &conversation, &message))
        .await
        .map_err(|_| GatewayError::new("ATTACHMENT_WORKER_FAILED", "附件读取线程结束。", 500))?
        .map_err(Into::into)
}

pub(crate) async fn content(
    runtime: &GatewayRuntime,
    conversation: &str,
    message: &str,
    attachment: &str,
) -> Result<AttachmentContent, GatewayError> {
    let root = runtime.config.data_root.clone();
    let conversation = conversation.to_owned();
    let message = message.to_owned();
    let attachment = attachment.to_owned();
    let (metadata, bytes) = tokio::task::spawn_blocking(move || {
        message_attachments::read(&root, &conversation, &message, &attachment)
    })
    .await
    .map_err(|_| GatewayError::new("ATTACHMENT_WORKER_FAILED", "附件读取线程结束。", 500))??;
    Ok(AttachmentContent {
        bytes,
        mime_type: metadata["contentType"]
            .as_str()
            .unwrap_or("application/octet-stream")
            .to_owned(),
        sha256: metadata["sha256"].as_str().unwrap_or("").to_owned(),
    })
}

pub(crate) async fn text(
    runtime: &GatewayRuntime,
    conversation: &str,
    message: &str,
    attachment: &str,
    offset: usize,
    maximum: usize,
) -> Result<Value, GatewayError> {
    text_with_cancellation(
        runtime, conversation, message, attachment, offset, maximum,
        Arc::new(AtomicBool::new(false)),
    )
    .await
}

/// Model preparation shares the owning task's cancellation and waits for its parser to settle.
/// 模型准备复用所属任务取消令牌，并等待解析器实际退出；暂停后不遗留失去归属的解析工作。
async fn text_with_cancellation(
    runtime: &GatewayRuntime,
    conversation: &str,
    message: &str,
    attachment: &str,
    offset: usize,
    maximum: usize,
    cancellation: Arc<AtomicBool>,
) -> Result<Value, GatewayError> {
    check_cancelled(&cancellation)?;
    let config = runtime.config.clone();
    let conversation = conversation.to_owned();
    let message = message.to_owned();
    let attachment = attachment.to_owned();
    tokio::task::spawn_blocking(move || {
        check_cancelled(&cancellation)?;
        let extracted = crate::retrieval::gateway_message_attachments::extract(
            &config,
            &conversation,
            &message,
            &attachment,
            &cancellation,
        )?;
        check_cancelled(&cancellation)?;
        Ok(crate::retrieval::gateway_message_attachments::page(
            &extracted,
            offset,
            maximum.clamp(1, 1_048_576),
        ))
    })
    .await
    .map_err(|_| GatewayError::new("ATTACHMENT_WORKER_FAILED", "附件解析线程结束。", 500))?
}

pub(crate) async fn prepare_context(
    runtime: &GatewayRuntime,
    conversation: &str,
    message_id: &str,
    max_text_chars: usize,
    include_images: bool,
    cancellation: Arc<AtomicBool>,
) -> Result<Value, GatewayError> {
    check_cancelled(&cancellation)?;
    let metadata = list(runtime, conversation, message_id).await?;
    let mut texts = Vec::new();
    let mut images = Vec::new();
    let mut image_bytes: u64 = 0;
    let mut failures = Vec::new();
    let mut remaining = max_text_chars;
    let items = metadata["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (index, item) in items.iter().enumerate() {
        check_cancelled(&cancellation)?;
        let id = item["id"].as_str().unwrap();
        let image = matches!(
            item["contentType"].as_str(),
            Some("image/png" | "image/jpeg" | "image/webp" | "image/gif")
        );
        let fits_image_context = images.len() < 16
            && image_bytes.saturating_add(item["sizeBytes"].as_u64().unwrap_or(u64::MAX))
                <= 32 * 1024 * 1024;
        if include_images && image && fits_image_context {
            match content(runtime, conversation, message_id, id).await {
                Ok(content) => {
                    image_bytes += content.bytes.len() as u64;
                    images.push(json!({"id":id,"mimeType":content.mime_type,"data":STANDARD.encode(content.bytes)}));
                }
                Err(error) => failures.push(
                    json!({"id":id,"name":item["name"],"error":error.message,"code":error.code}),
                ),
            }
            continue;
        }
        if remaining == 0 {
            if include_images && image && !fits_image_context {
                failures.push(
                    json!({"id":id,"name":item["name"],"code":"ATTACHMENT_IMAGE_CONTEXT_LIMIT",
                    "error":"本轮图片输入已满；原图已保存，可通过 attachment.read 读取文字。"}),
                );
            }
            continue;
        }
        let per_file = remaining
            .div_ceil(items.len().saturating_sub(index).max(1))
            .max(1);
        match text_with_cancellation(
            runtime, conversation, message_id, id, 0, per_file, cancellation.clone(),
        )
        .await
        {
            Ok(mut value) => {
                if value.get("error").is_some() {
                    failures.push(json!({"id":id,"name":item["name"],"error":value["error"],"code":value["errorCode"]}));
                } else {
                    remaining = remaining.saturating_sub(
                        value["text"].as_str().unwrap_or("").encode_utf16().count(),
                    );
                    value["id"] = json!(id);
                    value["name"] = item["name"].clone();
                    texts.push(value);
                }
            }
            Err(error) if error.code == "ABORT_ERR" => return Err(error),
            Err(error) => failures
                .push(json!({"id":id,"name":item["name"],"error":error.message,"code":error.code})),
        }
    }
    check_cancelled(&cancellation)?;
    Ok(json!({"attachments":items,"texts":texts,"images":images,"failures":failures}))
}

fn check_cancelled(cancellation: &AtomicBool) -> Result<(), GatewayError> {
    if cancellation.load(Ordering::Acquire) {
        Err(GatewayError::new("ABORT_ERR", "附件准备已取消。", 499))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_contract::{GatewayConfig, new_id};

    #[tokio::test]
    async fn cancelled_attachment_preparation_does_not_read_or_initialize_storage() {
        let root = std::env::temp_dir().join(format!("kynxa-cancelled-attachment-{}", new_id()));
        let runtime = GatewayRuntime::new(GatewayConfig {
            data_home: root.join("Models"),
            data_root: root.join("Data"),
            extension_root: root.join("Extensions"),
            application_root: root.join("Application"),
            port: 0,
            toolhost_exe: root.join("absent.exe"),
            vector_extension: root.join("absent.dll"),
            inference_library: root.join("absent-onnx.dll"),
        });
        let cancellation = Arc::new(AtomicBool::new(true));
        assert_eq!(
            prepare_context(&runtime, "missing", "missing", 4096, true, cancellation.clone())
                .await.unwrap_err().code,
            "ABORT_ERR",
        );
        assert_eq!(
            text_with_cancellation(&runtime, "missing", "missing", "missing", 0, 4096, cancellation)
                .await.unwrap_err().code,
            "ABORT_ERR",
        );
        assert!(!root.exists());
    }
}
