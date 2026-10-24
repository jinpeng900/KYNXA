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
        // Resolve execution settings before persistence without changing the fingerprinted transport input.
        // 写入前解析执行配置，但不得改写参与指纹计算的原始传输输入。
        let resolved = super::budget::resolve_input_limits(&input);
        let message = input["message"].as_str().unwrap_or("");
        let goal: String = message.trim().chars().take(4096).collect();
        let acceptance = super::task::acceptance_requirements(message);
        // New requests own only their explicit goal; continuation uses the original task control and checkpoint.
        // 新请求只拥有本次明确目标；续作通过原任务控制和检查点接续，不默认继承最近旧任务。
        let limits = &resolved["limits"];
        let duration_ms = limits["maxDurationMs"].as_u64().unwrap_or(1_800_000);
        let record_acceptance = compact_record_acceptance(&acceptance);
        let bytes = serde_json::to_vec(&json!({"input":input,"originalMessageRaw":original,
            "fingerprint":fingerprint,"resolvedRunLimits":limits,"runLimitCorrections":resolved["corrections"],
            "acceptanceRequirements":acceptance}))
            .map_err(|_| invalid("INVALID_TASK_INPUT"))?;
        let hash = managed_files::digest(&bytes);
        let relative = format!("Agent/task-inputs/{request}-{hash}.json");
        managed_files::atomic_write(&root, &root.join(&relative), &bytes, 32 * 1024 * 1024)?;
        let task = NewTask {
            request_id: request.into(), goal: if goal.is_empty() { "处理用户附件 / Process user attachments".into() } else { goal },
            acceptance: record_acceptance,
            scope: TaskScope { conversation_id: conversation.into(), project_id: relationship["projectId"].as_str().map(str::to_owned),
                permission_snapshot: json!({"permissionMode":permission,"bindingSnapshot":crate::storage::workspace_mounts::snapshot(&relationship),"requestFingerprint":fingerprint}) },
            // Active wall time is checkpointed separately from overlapping model/tool costs.
            // 有效墙钟独立于重叠的模型、工具成本写入检查点，不再乘以并行执行器数量。
            parent_task_id: None, dependency_ids: vec![], budget: TaskBudget {
                max_model_requests: super::budget::model_request_allowance(duration_ms),
                max_execution_ms: Some(duration_ms),
                ..Default::default()
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

/// Reuse limits saved at acceptance, including inherited minimum duration.
/// 执行和恢复沿用接受时保存的额度，包括继承的最低时长。
pub(crate) fn read_resolved_limits(root: &Path, reference: &Value) -> Option<Value> {
    let value = read_input_document(root, reference)?;
    value.get("resolvedRunLimits").filter(|limits| limits.is_object()).cloned()
}

/// Keep full acceptance criteria in the hash-verified input, separate from short task labels.
/// 完整验收项保存在哈希核验后的输入中，任务短标签不影响要求。
pub(crate) fn read_acceptance_requirements(root: &Path, reference: &Value) -> Option<Vec<String>> {
    let value = read_input_document(root, reference)?;
    value.get("acceptanceRequirements")?.as_array()?.iter().map(|item| item.as_str().map(str::to_owned)).collect()
}

fn read_input_document(root: &Path, reference: &Value) -> Option<Value> {
    let relative = reference["relativePath"].as_str()?;
    if reference["kind"] != "task-input" || !relative.starts_with("Agent/task-inputs/") || relative.contains("..") || relative.contains('\\') {
        return None;
    }
    let bytes = managed_files::read(root, &root.join(relative), 32 * 1024 * 1024).ok()??;
    if reference["sha256"].as_str() != Some(managed_files::digest(&bytes).as_str()) {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Bound only row metadata; the full list above is retained for review, continuation and recovery.
/// 仅约束任务行元数据；上方完整清单仍用于核对、续作和恢复。
fn compact_record_acceptance(requirements: &[String]) -> Vec<String> {
    const RECORD_ACCEPTANCE_BYTES: usize = 8 * 1024;
    if serde_json::to_vec(requirements).is_ok_and(|bytes| bytes.len() <= RECORD_ACCEPTANCE_BYTES) {
        return requirements.to_vec();
    }
    let item_bytes = RECORD_ACCEPTANCE_BYTES / requirements.len().max(1) - 4;
    requirements.iter().enumerate().map(|(index, requirement)| {
        let mut label = format!("requirement-{}: task-input #{} / ", index + 1, index + 1);
        let mut json_bytes = label.len();
        for character in requirement.chars() {
            // Reserve JSON escaping expansion as well as UTF-8 bytes, including embedded newlines.
            // 同时预留 JSON 转义扩展与 UTF-8 字节，包括要求中的换行。
            let added = match character { '\\' | '"' => 2, character if character.is_control() => 6, character => character.len_utf8() };
            if json_bytes + added > item_bytes { break; }
            label.push(character);
            json_bytes += added;
        }
        label
    }).collect()
}

pub(crate) fn read_sync(
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
