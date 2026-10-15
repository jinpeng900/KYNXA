//! Attached documents use their formal message identity, never a model-supplied private storage path.
//! 附件回读使用正式消息身份，不让模型通过私有数据路径读取其他会话文件。

use super::gateway::{GatewayToolContext, WorkerCancellation, check_cancelled, failure};
use crate::gateway_contract::{GatewayConfig, GatewayError};
use serde_json::{Value, json};
use std::path::PathBuf;

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
