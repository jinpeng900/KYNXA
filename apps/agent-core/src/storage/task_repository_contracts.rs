//! Durable work identities, separate from replaceable agents and execution attempts.
//! 持久工作身份与可更换的代理、执行尝试分离；这些类型不授予工具权限。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Queued,
    Running,
    WaitingNetwork,
    WaitingConfig,
    WaitingApproval,
    WaitingVerification,
    WaitingChildren,
    Paused,
    Cancelled,
    Failed,
    Completed,
    Partial,
}

impl TaskState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::Failed | Self::Completed | Self::Partial
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskScope {
    pub project_id: Option<String>,
    pub conversation_id: String,
    pub permission_snapshot: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskBudget {
    pub max_model_requests: u64,
    pub max_known_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_execution_ms: Option<u64>,
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            max_model_requests: 256,
            max_known_tokens: None,
            max_execution_ms: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskUsage {
    pub model_requests: u64,
    pub failed_requests: u64,
    pub retries: u64,
    pub compactions: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub unknown_token_requests: u64,
    #[serde(default)]
    pub model_duration_ms: u64,
    #[serde(default)]
    pub tool_duration_ms: u64,
}

impl TaskUsage {
    pub fn is_exhausted(&self, budget: &TaskBudget) -> bool {
        self.model_requests >= budget.max_model_requests
            || budget
                .max_known_tokens
                .is_some_and(|limit| self.input_tokens.saturating_add(self.output_tokens) >= limit)
            || budget.max_execution_ms.is_some_and(|limit| {
                self.model_duration_ms.saturating_add(self.tool_duration_ms) >= limit
            })
    }
    pub(crate) fn add(&mut self, delta: &Self) {
        self.model_requests = self.model_requests.saturating_add(delta.model_requests);
        self.failed_requests = self.failed_requests.saturating_add(delta.failed_requests);
        self.retries = self.retries.saturating_add(delta.retries);
        self.compactions = self.compactions.saturating_add(delta.compactions);
        self.input_tokens = self.input_tokens.saturating_add(delta.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(delta.output_tokens);
        self.unknown_token_requests = self
            .unknown_token_requests
            .saturating_add(delta.unknown_token_requests);
        self.model_duration_ms = self
            .model_duration_ms
            .saturating_add(delta.model_duration_ms);
        self.tool_duration_ms = self.tool_duration_ms.saturating_add(delta.tool_duration_ms);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewTask {
    pub request_id: String,
    pub goal: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    pub scope: TaskScope,
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub dependency_ids: Vec<String>,
    #[serde(default)]
    pub budget: TaskBudget,
    pub input_reference: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub task_id: String,
    pub request_id: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_in_task_list: Option<bool>,
    pub acceptance: Vec<String>,
    pub scope: TaskScope,
    pub parent_task_id: Option<String>,
    pub dependency_ids: Vec<String>,
    pub budget: TaskBudget,
    pub usage: TaskUsage,
    pub input_reference: Option<Value>,
    pub state: TaskState,
    pub version: u64,
    pub execution_version: u64,
    pub current_attempt_id: Option<String>,
    pub event_sequence: u64,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub paused_by_task_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptToken {
    pub task_id: String,
    pub attempt_id: String,
    pub execution_version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskAttempt {
    pub token: AttemptToken,
    pub executor_id: String,
    pub lease_expires_at_ms: i64,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub stop_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskControl {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEvent {
    pub task_id: String,
    pub sequence: u64,
    pub event_id: String,
    pub kind: String,
    pub payload: Value,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OperationState {
    Pending,
    Dispatched,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OperationEffect {
    Observation,
    FileMutation,
    Irreversible,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationIntent {
    pub operation_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub input_sha256: String,
    pub input_reference: Option<Value>,
    pub effect: OperationEffect,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOperation {
    pub task_id: String,
    pub attempt_id: String,
    pub intent: OperationIntent,
    pub state: OperationState,
    pub result_reference: Option<Value>,
    pub verification_reference: Option<Value>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskCheckpoint {
    pub checkpoint_id: String,
    pub through_event_sequence: u64,
    pub context_reference: Value,
    pub next_step: Value,
    #[serde(default)]
    pub source_versions: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskArtifact {
    pub artifact_id: String,
    pub kind: String,
    pub reference: Value,
    pub sha256: String,
    pub size_bytes: u64,
    pub source_version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelReservationState {
    Reserved,
    Unknown,
    Settled,
    Released,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequestReservation {
    pub task_id: String,
    pub reservation_id: String,
    pub attempt_id: String,
    pub state: ModelReservationState,
    pub dispatched: Option<bool>,
    pub usage: Option<TaskUsage>,
    pub verification_reference: Option<Value>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
