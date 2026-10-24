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
    /// Effective wall time, measured once by this task's executor; descendant costs remain separate.
    /// 本任务执行者独立测量的有效墙钟；后代模型／工具成本仍单独聚合，不重复累计并行时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_duration_ms: Option<u64>,
}

impl TaskUsage {
    pub fn is_exhausted(&self, budget: &TaskBudget) -> bool {
        self.model_requests >= budget.max_model_requests
            || budget
                .max_known_tokens
                .is_some_and(|limit| self.input_tokens.saturating_add(self.output_tokens) >= limit)
            || budget.max_execution_ms.is_some_and(|limit| {
                self.active_duration_ms.is_some_and(|active| active >= limit)
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

/// Caller-proven implicit budget floors are applied only inside the fenced takeover transaction.
/// 调用方核实的隐式预算下限，只在带版本校验的接管事务内应用。
#[derive(Clone, Debug)]
pub struct TaskAttemptBudgetExtension {
    pub task_id: String,
    pub expected_version: u64,
    pub min_model_requests: u64,
    pub min_execution_ms: u64,
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
    #[serde(default)]
    pub active_duration_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskControl {
    Pause,
    Resume,
    Revise,
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

/// Provider counters and explicitly separated client observations; no response bodies or credentials.
/// 供应商计数与明确分离的网关观测；缺失保持未知，不保留响应正文或凭据。
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_metrics: Option<ModelRequestClientMetrics>,
}

/// Observed once per original dispatch and recovered with its existing reservation, never by replay.
/// 每次原始派发只观测一次，随已有预约恢复；不能为补计量重发模型或重做操作。
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRequestClientMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_visible_text_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_policy: Option<Value>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_usage: Option<ProviderTokenUsage>,
    pub verification_reference: Option<Value>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
