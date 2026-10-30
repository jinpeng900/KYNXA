//! Attached documents use their formal message identity, never a model-supplied private storage path.
//! 附件回读使用正式消息身份，不让模型通过私有数据路径读取其他会话文件。

use super::gateway::{GatewayToolContext, WorkerCancellation, check_cancelled, failure};
use crate::gateway_contract::{GatewayConfig, GatewayError};
use serde_json::{Value, json};
use std::{path::{Path, PathBuf}, sync::atomic::{AtomicBool, Ordering}};

/// Synchronous attachment I/O runs inside the image reader's existing bounded worker.
/// 附件同步读取由图像读取器现有的受限工作线程承载，不另开线程或公开私有路径。
pub(super) fn read_image_original(root: &Path, conversation: &str, message: &str, attachment: &str,
    maximum_bytes: u64, cancellation: &AtomicBool) -> Result<(Value, Vec<u8>), GatewayError> {
    let active = || {
        if cancellation.load(Ordering::Acquire) { Err(failure("TOOL_CANCELLED", "图片读取已取消。", 409)) }
        else { Ok(()) }
    };
    active()?;
    let listed = crate::storage::message_attachments::list(root, conversation, message)?;
    let descriptor = listed["attachments"].as_array().and_then(|items| items.iter().find(|item|
        item["id"].as_str().is_some_and(|id| id.eq_ignore_ascii_case(attachment))))
        .ok_or_else(|| failure("ATTACHMENT_NOT_FOUND", "该消息中没有这个附件。", 404))?;
    if !descriptor["sizeBytes"].as_u64().is_some_and(|size| size > 0 && size <= maximum_bytes) {
        return Err(failure("IMAGE_FILE_TOO_LARGE", "图片必须非空且不超过 32 MiB。", 400));
    }
    active()?;
    let (metadata, bytes) = crate::storage::message_attachments::read(root, conversation, message, attachment)?;
    if metadata["sizeBytes"].as_u64() != Some(bytes.len() as u64) {
        return Err(failure("ATTACHMENT_CONFLICT", "正式消息与附件原件大小不一致。", 409));
    }
    active()?;
    Ok((metadata, bytes))
}

#[cfg(test)]
pub(super) fn test_reserve_image_attachment(context: &GatewayToolContext, name: &str, size: u64) -> (String, String) {
    let message = uuid::Uuid::new_v4().to_string();
    let attachment = uuid::Uuid::new_v4().to_string();
    crate::storage::gateway::ensure_conversation(&context.data_home, &context.conversation_id, "Image fixture").unwrap();
    crate::storage::gateway::upsert_message(&context.data_home, &context.conversation_id,
        &json!({"Id":message,"Role":"user","Content":"",
            "Attachments":[{"Id":attachment,"Name":name,"SizeBytes":size}]})).unwrap();
    (message, attachment)
}

#[cfg(test)]
pub(super) fn test_save_image_attachment(context: &GatewayToolContext, name: &str, bytes: &[u8]) -> (String, String) {
    let (message, attachment) = test_reserve_image_attachment(context, name, bytes.len() as u64);
    crate::storage::message_attachments::save(&context.data_home, &context.conversation_id,
        &message, &attachment, name, bytes).unwrap();
    (message, attachment)
}

/// Export the verified immutable upload through the ordinary authorized binary-write gateway.
/// 原件按正式消息身份读取，再复用普通二进制写入网关；不公开附件私有存储路径。
pub(super) async fn export(
    arguments: Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    check_cancelled(context)?;
    let message = arguments["messageId"].as_str().filter(|value| !value.trim().is_empty())
        .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "请提供附件所属消息 ID。", 400))?.to_owned();
    let attachment = arguments["attachmentId"].as_str().filter(|value| !value.trim().is_empty())
        .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "请提供附件 ID。", 400))?.to_owned();
    if !arguments["path"].as_str().is_some_and(|path| !path.trim().is_empty()) {
        return Err(failure("INVALID_TOOL_ARGUMENTS", "请提供附件导出目标路径。", 400));
    }
    let expected = arguments["expectedSha256"].as_str().map(str::to_owned);
    if !arguments["expectedSha256"].is_null()
        && !expected.as_ref().is_some_and(|value| value.len() == 64
            && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))) {
        return Err(failure("INVALID_TOOL_ARGUMENTS", "expectedSha256 必须是完整的小写 SHA-256。", 400));
    }
    let source_id = format!("attachment:{}:{message}:{attachment}", context.conversation_id);
    let root = context.data_home.clone();
    let conversation = context.conversation_id.clone();
    let (metadata, bytes) = tokio::task::spawn_blocking(move || {
        let (metadata, bytes) = crate::storage::message_attachments::read(
            &root, &conversation, &message, &attachment,
        )?;
        if expected.as_ref().is_some_and(|expected| metadata["sha256"] != expected.as_str()) {
            return Err(failure("ATTACHMENT_VERSION_CHANGED", "附件版本已变化，请重新读取。", 409));
        }
        if metadata["sizeBytes"].as_u64() != Some(bytes.len() as u64) {
            return Err(failure("ATTACHMENT_CONFLICT", "正式消息与附件原件大小不一致。", 409));
        }
        Ok::<_, GatewayError>((metadata, bytes))
    }).await.map_err(|_| failure("ATTACHMENT_WORKER_FAILED", "附件原件读取线程结束。", 500))??;
    check_cancelled(context)?;
    let mut result = super::gateway::filesystem_binary_export(arguments, bytes, context).await?;
    if result["requiresApproval"] == true {
        return Ok(result);
    }
    result["sourceId"] = json!(source_id);
    result["sourceVersion"] = metadata["sha256"].clone();
    result["sha256"] = metadata["sha256"].clone();
    result["sizeBytes"] = metadata["sizeBytes"].clone();
    result["name"] = metadata["name"].clone();
    Ok(result)
}

pub(super) async fn read(
    arguments: Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    check_cancelled(context)?;
    let application_root = context.config["officialToolsRoot"]
        .as_str()
        .and_then(|root| std::path::Path::new(root).parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            context
                .toolhost_exe
                .parent()
                .unwrap_or(&context.workdir)
                .to_owned()
        });
    let config = GatewayConfig {
        data_root: context.data_home.clone(),
        data_home: context.data_home.join("Models"),
        extension_root: context.extension_root.clone(),
        toolhost_exe: context.toolhost_exe.clone(),
        vector_extension: application_root.join("runtime/native/vec0.dll"),
        inference_library: application_root.join("runtime/native/onnxruntime.dll"),
        application_root,
        port: 0,
    };
    let conversation = context.conversation_id.clone();
    let cancellation_guard = WorkerCancellation::new(context);
    let cancellation = cancellation_guard.signal.clone();
    let result = tokio::task::spawn_blocking(move || {
        let message = arguments["messageId"].as_str().unwrap_or("");
        let attachment = arguments["attachmentId"].as_str().unwrap_or("");
        let extracted = crate::retrieval::gateway_message_attachments::extract(
            &config,
            &conversation,
            message,
            attachment,
            &cancellation,
        )?;
        if arguments["expectedSha256"]
            .as_str()
            .is_some_and(|expected| extracted["sourceVersion"] != expected)
        {
            return Err(failure(
                "ATTACHMENT_VERSION_CHANGED",
                "附件版本已变化，请重新读取。",
                409,
            ));
        }
        let mut page = crate::retrieval::gateway_message_attachments::page(
            &extracted,
            arguments["offset"].as_u64().unwrap_or(0) as usize,
            arguments["maxChars"].as_u64().unwrap_or(24000) as usize,
        );
        page["messageId"] = json!(message);
        page["attachmentId"] = json!(attachment);
        page["sourceId"] = json!(format!("attachment:{conversation}:{message}:{attachment}"));
        Ok(page)
    })
    .await
    .map_err(|_| failure("ATTACHMENT_WORKER_FAILED", "附件解析线程结束。", 500));
    drop(cancellation_guard);
    check_cancelled(context)?;
    result?
}
