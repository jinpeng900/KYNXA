//! Persist the accepted transport input once; recovery reads this request, never every chat log.
//! 接受时保存一次传输输入；恢复仅读取本次请求，不能扫描全部聊天日志。

use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    storage::{gateway as data, managed_files, task_repository::*},
};
use serde_json::{Value, json};
use std::path::Path;

pub async fn accept(
    config: &GatewayConfig,
    input: &Value,
    original: Option<&str>,
    fingerprint: &str,
) -> Result<TaskRecord, GatewayError> {
    let root = config.data_root.clone();
    let input = input.clone();
    let original = original.map(str::to_owned);
    let fingerprint = fingerprint.to_owned();
    tokio::task::spawn_blocking(move || {
        let conversation = input["conversationId"].as_str().unwrap_or("");
        let request = input["requestId"].as_str().unwrap_or("");
        let limits = super::budget::apply("budget.limits", &json!({"input":input.get("runLimits").filter(|limits|!limits.is_null()).cloned().unwrap_or(json!({}))}))
            .map_err(|_| GatewayError::new("INVALID_TOOL_RUN_LIMITS", "任务执行额度无效。", 400))?;
        let repository = TaskRepository::open(&root)?;
        if let Some(existing) = repository.find_by_request_id(conversation, request)? {
            let reference = existing.input_reference.as_ref().ok_or_else(|| invalid("TASK_INPUT_MISSING"))?;
            let stored = read_sync(&root, reference)?;
            if stored.2 != fingerprint { return Err(invalid("CHAT_REQUEST_CONFLICT")); }
            let relationship = data::relationship(&root, conversation)?;
            let snapshot = &existing.scope.permission_snapshot["bindingSnapshot"];
            if !crate::storage::workspace_mounts::validate_snapshot(snapshot, &relationship) {
                return Err(invalid("TASK_SCOPE_CHANGED"));
            }
            return Ok(existing);
        }
        let title: String = input["message"].as_str().unwrap_or("").chars().take(80).collect();
        data::ensure_conversation(&root, conversation, &title)?;
        let relationship = data::relationship(&root, conversation)?;
        let permission = input["permissionMode"].as_str().unwrap_or("smart");
        if !["ask", "smart", "full"].contains(&permission) { return Err(invalid("INVALID_PERMISSION_MODE")); }
        let bytes = serde_json::to_vec(&json!({"input":input,"originalMessageRaw":original,"fingerprint":fingerprint}))
            .map_err(|_| invalid("INVALID_TASK_INPUT"))?;
        let hash = managed_files::digest(&bytes);
        let relative = format!("Agent/task-inputs/{request}-{hash}.json");
        managed_files::atomic_write(&root, &root.join(&relative), &bytes, 32 * 1024 * 1024)?;
        let task = NewTask {
            request_id: request.into(), goal: input["message"].as_str().filter(|text| !text.trim().is_empty()).unwrap_or("处理用户附件 / Process user attachments").chars().take(4096).collect(),
            acceptance: vec!["Return the requested result with actual execution and verification status. / 返回请求结果及真实执行、验证状态。".into()],
            scope: TaskScope { conversation_id: conversation.into(), project_id: relationship["projectId"].as_str().map(str::to_owned),
                permission_snapshot: json!({"permissionMode":permission,"bindingSnapshot":crate::storage::workspace_mounts::snapshot(&relationship),"requestFingerprint":fingerprint}) },
            parent_task_id: None, dependency_ids: vec![], budget: TaskBudget {
                max_model_requests: limits["maxModelRequests"].as_u64().unwrap_or_else(|| TaskBudget::default().max_model_requests),
                max_known_tokens: limits["maxKnownTokens"].as_u64(),
                // Keep the documented 30-minute default and allow the validated 60-minute
                // override through runLimits.maxDurationMs.
                // 保留文档约定的 30 分钟默认值，并允许通过 runLimits.maxDurationMs 使用已校验的 60 分钟上限。
                max_execution_ms: limits["maxDurationMs"].as_u64(),
            },
            input_reference: Some(json!({"kind":"task-input","relativePath":relative,"sha256":hash,"bytes":bytes.len()})),
        };
        Ok(repository.accept_task(&task)?)
    }).await.map_err(|_| invalid("TASK_INPUT_WORKER_FAILED"))?
}

pub async fn read(
    root: &Path,
    reference: &Value,
) -> Result<(Value, Option<String>, String), GatewayError> {
    let root = root.to_owned();
    let reference = reference.clone();
    tokio::task::spawn_blocking(move || read_sync(&root, &reference))
        .await
        .map_err(|_| invalid("TASK_INPUT_WORKER_FAILED"))?
}

fn read_sync(
    root: &Path,
    reference: &Value,
) -> Result<(Value, Option<String>, String), GatewayError> {
    let relative = reference["relativePath"]
        .as_str()
        .ok_or_else(|| invalid("INVALID_TASK_INPUT_REFERENCE"))?;
    if reference["kind"] != "task-input"
        || !relative.starts_with("Agent/task-inputs/")
        || relative.contains("..")
        || relative.contains('\\')
    {
        return Err(invalid("INVALID_TASK_INPUT_REFERENCE"));
    }
    let bytes = managed_files::read(root, &root.join(relative), 32 * 1024 * 1024)?
        .ok_or_else(|| invalid("TASK_INPUT_MISSING"))?;
    if reference["sha256"].as_str() != Some(managed_files::digest(&bytes).as_str()) {
        return Err(invalid("TASK_INPUT_CHANGED"));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid("INVALID_TASK_INPUT"))?;
    Ok((
        value["input"].clone(),
        value["originalMessageRaw"].as_str().map(str::to_owned),
        value["fingerprint"]
            .as_str()
            .ok_or_else(|| invalid("INVALID_TASK_INPUT"))?
            .into(),
    ))
}
fn invalid(code: &str) -> GatewayError {
    GatewayError::new(code, "任务输入缺失、发生变化或与正式请求不匹配。", 409)
}
