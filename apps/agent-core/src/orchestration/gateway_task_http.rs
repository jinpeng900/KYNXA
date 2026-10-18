//! Task HTTP is a scoped projection of the existing Hub, never a second task store.
//! 任务 HTTP 是现有 Hub 的范围投影，不另建任务状态存储。

use super::gateway_runtime::{GatewayRuntime, checked_id};
use crate::{
    gateway_contract::GatewayError,
    storage::{gateway as data, task_repository::TaskControl},
};
use serde_json::Value;
use std::sync::Arc;

pub async fn handle(
    runtime: &Arc<GatewayRuntime>,
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
) -> Option<Result<(u16, Value), GatewayError>> {
    if path != "/api/tasks" && !path.starts_with("/api/tasks/") {
        return None;
    }
    Some(execute(runtime, method, path, query, body).await)
}
fn parameter<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}
async fn execute(
    runtime: &Arc<GatewayRuntime>,
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
) -> Result<(u16, Value), GatewayError> {
    let conversation = checked_id(
        &if method == "POST" {
            body["conversationId"].clone()
        } else {
            Value::String(parameter(query, "conversationId").unwrap_or("").into())
        },
        "conversationId",
    )?;
    let root = runtime.config.data_root.clone();
    let chat = conversation.clone();
    let relationship = tokio::task::spawn_blocking(move || data::relationship(&root, &chat))
        .await
        .map_err(|_| GatewayError::new("TASK_SCOPE_UNAVAILABLE", "任务范围读取失败。", 500))??;
    let project = relationship["projectId"].as_str();
    if path == "/api/tasks/recoverable" && method == "GET" {
        if parameter(query, "projectId") != project { return Err(denied()); }
        let limit = parameter(query, "limit").and_then(|value| value.parse::<usize>().ok()).unwrap_or(50).clamp(1, 100);
        return runtime.tasks.recoverable(&conversation, project, parameter(query, "afterTaskId"), limit)
            .await.map(|value| (200, value));
    }
    if path == "/api/tasks/paused" && method == "GET" {
        // Recovery always stays in the exact saved chat, even when its ordinary sidebar lists the whole work.
        // 恢复始终限定当前正式聊天；普通侧栏可列整个工作的语义不被更改。
        if parameter(query, "projectId") != project {
            return Err(denied());
        }
        return runtime.tasks.paused(&conversation, project).await.map(|value| (200, value));
    }
    if path == "/api/tasks" && method == "GET" {
        let selected_project = parameter(query, "projectId");
        if selected_project.is_some() && selected_project != project {
            return Err(denied());
        }
        let limit = parameter(query, "limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(50)
            .clamp(1, 100);
        return runtime
            .tasks
            .list(
                if selected_project.is_some() {
                    None
                } else {
                    Some(&conversation)
                },
                selected_project,
                parameter(query, "afterTaskId"),
                limit,
            )
            .await
            .map(|value| (200, value));
    }
    let parts: Vec<&str> = path.trim_start_matches("/api/tasks/").split('/').collect();
    let task_id = checked_id(
        &Value::String(parts.first().copied().unwrap_or("").into()),
        "taskId",
    )?;
    let detail = runtime.tasks.detail(&task_id).await?;
    let scope = &detail["task"]["scope"];
    if scope["conversationId"] != conversation
        && !(project.is_some() && scope["projectId"].as_str() == project)
    {
        return Err(denied());
    }
    match (method, parts.get(1).copied()) {
        ("GET", None) => Ok((200, detail)),
        ("GET", Some("events")) => {
            let after = parameter(query, "afterSequence")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let limit = parameter(query, "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(128)
                .clamp(1, 512);
            runtime
                .tasks
                .events(&task_id, after, limit)
                .await
                .map(|value| (200, value))
        }
        ("POST", Some("control")) => {
            let action = match body["action"].as_str() {
                Some("pause") => TaskControl::Pause,
                Some("resume") => TaskControl::Resume,
                Some("cancel") => TaskControl::Cancel,
                _ => {
                    return Err(GatewayError::new(
                        "INVALID_TASK_CONTROL",
                        "任务控制动作无效。",
                        400,
                    ));
                }
            };
            let version = body["expectedVersion"].as_u64().ok_or_else(|| {
                GatewayError::new("TASK_VERSION_REQUIRED", "任务控制需要当前版本。", 400)
            })?;
            runtime
                .tasks
                .control(&task_id, version, action)
                .await
                .map(|value| (200, value))
        }
        ("POST", Some("steer")) => {
            if scope["conversationId"] != conversation {
                return Err(denied());
            }
            let version = body["expectedVersion"].as_u64().ok_or_else(|| {
                GatewayError::new("TASK_VERSION_REQUIRED", "插话需要当前任务版本。", 400)
            })?;
            let message_id = checked_id(&body["userMessageId"], "userMessageId")?;
            let reply_id = body
                .get("replyMessageId")
                .filter(|value| !value.is_null())
                .map(|value| checked_id(value, "replyMessageId"))
                .transpose()?;
            runtime
                .tasks
                .steer(
                    &task_id,
                    version,
                    &message_id,
                    reply_id.as_deref(),
                    body["message"].as_str().unwrap_or(""),
                    body["action"].as_str().unwrap_or(""),
                    &body["attachments"],
                )
                .await
                .map(|value| (200, value))
        }
        _ => Err(GatewayError::new("NOT_FOUND", "任务接口不存在。", 404)),
    }
}
fn denied() -> GatewayError {
    GatewayError::new("TASK_SCOPE_DENIED", "不能访问其他工作的任务记录。", 403)
}
