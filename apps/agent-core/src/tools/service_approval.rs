use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::local_files::random_identity;
use super::service_error::{ServiceError, ServiceResult};

struct PendingApproval {
    conversation_id: String,
    request_id: String,
    tool_call_id: String,
    deadline: Instant,
}

pub struct ApprovalRegistry {
    pending: HashMap<String, PendingApproval>,
    timeout: Duration,
    closed: bool,
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            timeout: Duration::from_secs(300),
            closed: false,
        }
    }
}

impl ApprovalRegistry {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "approval.init" => {
                if let Some(timeout) = arguments["timeoutMs"].as_u64() {
                    self.timeout = Duration::from_millis(timeout);
                }
                Ok(Value::Null)
            }
            "approval.wait" => {
                if self.closed {
                    return Err(ServiceError::new(
                        "TOOL_SERVICE_CLOSED",
                        "工具服务已关闭。",
                        409,
                    ));
                }
                if self.pending.len() >= 128 {
                    return Err(ServiceError::new(
                        "TOOL_APPROVAL_CAPACITY",
                        "待审批工具已达上限。",
                        409,
                    ));
                }
                let approval_id = random_identity()?;
                self.pending.insert(
                    approval_id.clone(),
                    PendingApproval {
                        conversation_id: arguments["context"]["conversationId"]
                            .as_str()
                            .unwrap_or("")
                            .into(),
                        request_id: arguments["context"]["requestId"]
                            .as_str()
                            .unwrap_or("")
                            .into(),
                        tool_call_id: arguments["toolCallId"].as_str().unwrap_or("").into(),
                        deadline: Instant::now() + self.timeout,
                    },
                );
                Ok(json!({"approvalId":approval_id}))
            }
            "approval.approve" => {
                let input = &arguments["input"];
                let approval_id = input["approvalId"].as_str().unwrap_or("").to_lowercase();
                let pending = self.pending.get(&approval_id).ok_or_else(|| {
                    ServiceError::new(
                        "TOOL_APPROVAL_NOT_FOUND",
                        "此工具审批不存在、已消费或已过期。",
                        404,
                    )
                })?;
                if Instant::now() >= pending.deadline {
                    self.pending.remove(&approval_id);
                    return Ok(
                        json!({"completion":{"approvalId":approval_id,"error":ServiceError::new(
                        "TOOL_APPROVAL_EXPIRED", "工具审批已过期。", 409).value()},
                        "error":ServiceError::new("TOOL_APPROVAL_NOT_FOUND", "此工具审批不存在、已消费或已过期。", 404).value()}),
                    );
                }
                if !input["approved"].is_boolean()
                    || input["conversationId"]
                        .as_str()
                        .map(str::to_lowercase)
                        .as_deref()
                        != Some(&pending.conversation_id)
                    || input["requestId"]
                        .as_str()
                        .map(str::to_lowercase)
                        .as_deref()
                        != Some(&pending.request_id)
                    || input["toolCallId"].as_str() != Some(&pending.tool_call_id)
                {
                    return Err(ServiceError::new(
                        "TOOL_APPROVAL_MISMATCH",
                        "审批身份不匹配，不能改变原工具调用。",
                        409,
                    ));
                }
                self.pending.remove(&approval_id);
                Ok(
                    json!({"approved":input["approved"],"approvalId":approval_id,"toolCallId":input["toolCallId"]}),
                )
            }
            "approval.finish" => {
                let id = arguments["approvalId"].as_str().unwrap_or("");
                let consumed = self.pending.remove(id).is_some();
                Ok(json!({"consumed":consumed}))
            }
            "approval.cancelContext" => {
                let conversation_id = arguments["context"]["conversationId"].as_str();
                let request_id = arguments["context"]["requestId"].as_str();
                let ids = self
                    .pending
                    .iter()
                    .filter(|(_, approval)| {
                        Some(approval.conversation_id.as_str()) == conversation_id
                            && Some(approval.request_id.as_str()) == request_id
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                for id in &ids {
                    self.pending.remove(id);
                }
                Ok(json!(ids))
            }
            "approval.close" => {
                self.closed = true;
                Ok(json!(
                    self.pending.drain().map(|(id, _)| id).collect::<Vec<_>>()
                ))
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown approval operation.",
                400,
            )),
        }
    }
}
