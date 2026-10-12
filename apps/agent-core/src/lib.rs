//! Shared native operation contracts and scoped state sessions used by gateway adapters.
//! 网关适配器复用的原生操作合同与范围状态会话；HTTP/SSE 入口由 gateway 模块负责。

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

pub mod file_identity;
pub mod gateway;
pub mod gateway_contract;
pub mod models;
pub mod orchestration;
pub mod resources;
pub mod retrieval;
pub mod service_cancellation;
pub mod storage;
pub mod text_metrics;
pub mod tools;
pub mod transport;

use orchestration::{budget, progress::RunProgress, task::TaskState};
use tools::{effects::EffectLedger, observations::ObservationGuard};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_SESSIONS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub operation: String,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub arguments: Value,
}

struct RunSession {
    task: TaskState,
    effects: EffectLedger,
    observations: ObservationGuard,
    progress: RunProgress,
    stream: models::stream::StreamState,
    evidence: retrieval::evidence::EvidenceLedger,
    tool_service: tools::service::ToolSession,
    memory_lifecycle: storage::memory_lifecycle::MemoryLifecycleState,
    resource_coordination: resources::coordination::CoordinationState,
    inference_admission: resources::admission::AdmissionState,
    inference_reservation: resources::reservation::ReservationState,
    external_admission: resources::external_admission::ExternalAdmissionState,
    external_observer: resources::external::ExternalObserverState,
    indexing_run: retrieval::indexing_run::IndexingRunState,
    ann_session: retrieval::ann_queue::AnnSession,
    model_routing: models::routing::RoutingState,
    inference_lifecycle: models::lifecycle::InferenceLifecycleState,
    workflow: orchestration::workflow::WorkflowState,
    navigation: orchestration::navigation::NavigationState,
    source_management: orchestration::source_management::SourceManagementState,
    evidence_publication: orchestration::evidence_publication::EvidencePublicationState,
    embedding_policy: orchestration::embedding_policy::EmbeddingPolicyState,
    limits: Value,
    rounds: u64,
    tool_calls: u64,
    generated_tokens: u64,
}

impl RunSession {
    fn snapshot(&mut self) -> Result<Value, String> {
        Ok(json!({
            "task": self.task.apply("task.state", &json!({}))?,
            "effects": self.effects.apply("effects.state", &json!({}))?,
            "observations": self.observations.apply("observations.state", &json!({}))?,
            "progress": self.progress.summary(),
            "run": {"limits": self.limits, "rounds": self.rounds,
                "toolCalls": self.tool_calls, "generatedTokens": self.generated_tokens}
        }))
    }

    fn apply(&mut self, operation: &str, arguments: &Value) -> Result<Value, String> {
        if operation.starts_with("retrieval.ann.") {
            return self.ann_session.apply(operation, arguments);
        }
        if operation.starts_with("retrieval.indexing.") {
            return self.indexing_run.apply(operation, arguments);
        }
        if operation.starts_with("storage.memoryLifecycle.") {
            return self.memory_lifecycle.apply(operation, arguments);
        }
        if operation.starts_with("tools.service.") {
            return self.tool_service.apply(operation, arguments);
        }
        if operation.starts_with("resources.coordination.") {
            return self.resource_coordination.apply(operation, arguments);
        }
        if operation.starts_with("resources.admission.") {
            return self.inference_admission.apply(operation, arguments);
        }
        if operation.starts_with("resources.reservation.") {
            return self.inference_reservation.apply(operation, arguments);
        }
        if operation.starts_with("resources.externalAdmission.") {
            return self.external_admission.apply(operation, arguments);
        }
        if operation.starts_with("resources.external.") {
            return self.external_observer.apply(operation, arguments);
        }
        if operation.starts_with("models.routing.") {
            return self.model_routing.apply(operation, arguments);
        }
        if operation.starts_with("models.lifecycle.") {
            return self.inference_lifecycle.apply(operation, arguments);
        }
        if operation.starts_with("orchestration.sourceManagement.") {
            return self.source_management.apply(operation, arguments);
        }
        if operation.starts_with("orchestration.publication.") {
            return self.evidence_publication.apply(operation, arguments);
        }
        if operation.starts_with("orchestration.embedding.") {
            return self.embedding_policy.apply(operation, arguments);
        }
        if operation.starts_with("orchestration.navigation.") {
            return self.navigation.apply(operation, arguments);
        }
        if operation.starts_with("orchestration.workflow.") {
            return self.workflow.apply(operation, arguments);
        }
        if operation.starts_with("models.stream.") {
            return self.stream.apply(operation, arguments);
        }
        if operation.starts_with("retrieval.evidence-") {
            return self.evidence.apply(operation, arguments);
        }
        if operation.starts_with("task.") {
            return self.task.apply(operation, arguments);
        }
        if operation.starts_with("effects.") {
            return self.effects.apply(operation, arguments);
        }
        if operation.starts_with("observations.") {
            return self.observations.apply(operation, arguments);
        }
        if operation.starts_with("progress.") {
            return self.progress.apply(operation, arguments);
        }
        if operation == "run.admit" {
            let number = |name: &str| {
                arguments[name]
                    .as_u64()
                    .ok_or("AGENT_CORE_INVALID_ARGUMENT".to_owned())
            };
            let rounds = number("rounds")?;
            let tool_calls = number("toolCalls")?;
            let generated_tokens = number("generatedTokens")?;
            // Counters only advance. A repaired model step cannot reset the same request's budget.
            // 计数只能向前推进；修复模型步骤不能重置同一请求的执行额度。
            if rounds < self.rounds
                || tool_calls < self.tool_calls
                || generated_tokens < self.generated_tokens
            {
                return Err("AGENT_CORE_COUNTER_REGRESSION".to_owned());
            }
            budget::apply(
                "budget.check",
                &json!({"limits": self.limits,
                "rounds": rounds, "toolCalls": tool_calls, "generatedTokens": generated_tokens}),
            )?;
            self.rounds = rounds;
            self.tool_calls = tool_calls;
            self.generated_tokens = generated_tokens;
            return Ok(Value::Null);
        }
        Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned())
    }
}

/// An engine owns isolated control and native services; adapters authorize and supply live SDK receipts.
/// 引擎拥有隔离控制及原生服务；适配器先授权，再提供实际 SDK 回执。
#[derive(Default)]
pub struct AgentEngine {
    sessions: HashMap<String, RunSession>,
}

impl AgentEngine {
    /// A closing service session signals long-running native scans without blocking JavaScript.
    /// 服务会话关闭时通知长时原生扫描，避免阻塞 JavaScript 线程等待退出。
    pub fn bind_service_cancellation(
        &mut self,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        for session in self.sessions.values_mut() {
            session.tool_service.cancellation = Some(cancelled.clone());
        }
    }
    pub fn service_committed(&self) -> bool {
        self.sessions
            .values()
            .any(|session| session.tool_service.committed)
    }
    pub fn dispatch(&mut self, request: Request) -> Value {
        let id = request.id;
        match self.apply(request) {
            Ok(result) => {
                json!({"version": PROTOCOL_VERSION, "id": id, "ok": true, "result": result})
            }
            Err(code) => json!({"version": PROTOCOL_VERSION, "id": id, "ok": false,
                "error": {"code": code, "message": "Agent service request failed; inspect the domain receipt before retrying an operation."}}),
        }
    }

    fn apply(&mut self, request: Request) -> Result<Value, String> {
        if request.version != PROTOCOL_VERSION {
            return Err("AGENT_CORE_PROTOCOL_MISMATCH".to_owned());
        }
        if request.operation == "core.status" {
            return Ok(
                json!({"engine": "rust", "protocolVersion": PROTOCOL_VERSION,
                "activeSessions": self.sessions.len(), "maxSessions": MAX_SESSIONS,
                "capabilities": ["task-state", "task-transition", "run-admission", "effect-recovery",
                    "observation-progress", "context-capacity", "output-checkpoint-metrics",
                    "verification-progress", "retrieval-calculations", "tool-discovery", "provider-protocols", "storage-contracts"],
                "externalExecution": true, "providerExecution": false, "terminalExecution": false, "durableStorageWriter": true,
                "nativeServices": ["managed-files", "sqlite-index", "sqlite-lexical", "conversation-log", "catalog-transactions", "memory-repository", "memory-lifecycle", "source-library", "source-coverage", "checkpoints", "source-indexing", "ann-control", "inference-lifecycle", "resource-admission", "external-model-coordination", "tool-broker", "filesystem", "skill-resources", "browser-state", "terminal-job-state"]}),
            );
        }
        if request.operation == "task.parse" {
            return TaskState::default().apply("task.inspect", &request.arguments);
        }
        if matches!(
            request.operation.as_str(),
            "observations.fingerprint" | "observations.source"
        ) {
            return tools::observations::calculate(&request.operation, &request.arguments);
        }
        if request.operation == "progress.validationCall" {
            return Ok(json!(orchestration::progress::validation_call(
                &request.arguments["call"]
            )));
        }
        if request.operation.starts_with("retrieval.")
            && !request.operation.starts_with("retrieval.evidence-")
            && !request.operation.starts_with("retrieval.indexing.")
            && !request.operation.starts_with("retrieval.ann.")
        {
            return retrieval::calculate(&request.operation, &request.arguments);
        }
        if request.operation.starts_with("tools.")
            && !request.operation.starts_with("tools.service.")
        {
            return tools::calculate(&request.operation, &request.arguments);
        }
        if request.operation.starts_with("models.")
            && !request.operation.starts_with("models.stream.")
            && !request.operation.starts_with("models.routing.")
            && !request.operation.starts_with("models.lifecycle.")
        {
            return models::calculate(&request.operation, &request.arguments);
        }
        if request.operation.starts_with("storage.")
            && !request.operation.starts_with("storage.memoryLifecycle.")
        {
            return storage::calculate(&request.operation, &request.arguments);
        }
        if request.operation.starts_with("resources.")
            && !request.operation.starts_with("resources.admission.")
            && !request.operation.starts_with("resources.coordination.")
            && !request.operation.starts_with("resources.reservation.")
            && !request.operation.starts_with("resources.external.")
            && !request
                .operation
                .starts_with("resources.externalAdmission.")
        {
            return resources::calculate(&request.operation, &request.arguments);
        }
        if request.operation.starts_with("orchestration.")
            && !request.operation.starts_with("orchestration.workflow.")
            && !request.operation.starts_with("orchestration.navigation.")
            && !request.operation.starts_with("orchestration.embedding.")
            && !request.operation.starts_with("orchestration.publication.")
            && !request
                .operation
                .starts_with("orchestration.sourceManagement.")
        {
            return orchestration::calculate(&request.operation, &request.arguments);
        }
        if matches!(
            request.operation.as_str(),
            "context.outputDefaults"
                | "context.outputValidate"
                | "context.outputBudget"
                | "context.completeTurns"
                | "context.observationReadTool"
                | "context.observationDescribe"
                | "context.observationPlan"
        ) {
            return orchestration::context::apply(&request.operation, &request.arguments);
        }
        // Calculations are independent of sessions and never create permissions or provider requests.
        // 纯计算不依赖会话，也不产生权限或供应商请求。
        if request.operation.starts_with("budget.")
            || request.operation.starts_with("context.")
            || request.operation.starts_with("output.")
        {
            return budget::apply(&request.operation, &request.arguments);
        }
        let session_id = request
            .session
            .ok_or("AGENT_CORE_SESSION_REQUIRED".to_owned())?;
        if session_id.is_empty()
            || session_id.len() > 128
            || session_id.chars().any(char::is_control)
        {
            return Err("AGENT_CORE_INVALID_SESSION".to_owned());
        }
        if request.operation == "session.open" {
            if self.sessions.contains_key(&session_id) {
                return Err("AGENT_CORE_SESSION_EXISTS".to_owned());
            }
            if self.sessions.len() >= MAX_SESSIONS {
                return Err("AGENT_CORE_SESSION_LIMIT".to_owned());
            }
            let limits = budget::apply(
                "budget.limits",
                &json!({"input": request.arguments["limits"]}),
            )?;
            let mut session = RunSession {
                task: TaskState::default(),
                effects: EffectLedger::default(),
                observations: ObservationGuard::default(),
                progress: RunProgress::default(),
                stream: models::stream::StreamState::default(),
                evidence: retrieval::evidence::EvidenceLedger::default(),
                tool_service: tools::service::ToolSession::default(),
                memory_lifecycle: storage::memory_lifecycle::MemoryLifecycleState::default(),
                resource_coordination: resources::coordination::CoordinationState::default(),
                inference_admission: resources::admission::AdmissionState::default(),
                inference_reservation: resources::reservation::ReservationState::default(),
                external_admission: resources::external_admission::ExternalAdmissionState::default(
                ),
                external_observer: resources::external::ExternalObserverState::default(),
                indexing_run: retrieval::indexing_run::IndexingRunState::default(),
                ann_session: retrieval::ann_queue::AnnSession::default(),
                model_routing: models::routing::RoutingState::default(),
                inference_lifecycle: models::lifecycle::InferenceLifecycleState::default(),
                workflow: orchestration::workflow::WorkflowState::default(),
                navigation: orchestration::navigation::NavigationState::default(),
                source_management: orchestration::source_management::SourceManagementState::default(
                ),
                evidence_publication:
                    orchestration::evidence_publication::EvidencePublicationState::default(),
                embedding_policy: orchestration::embedding_policy::EmbeddingPolicyState::default(),
                limits,
                rounds: 0,
                tool_calls: 0,
                generated_tokens: 0,
            };
            let state = session.snapshot()?;
            self.sessions.insert(session_id, session);
            return Ok(json!({"value": null, "state": state}));
        }
        if request.operation == "session.close" {
            self.sessions.remove(&session_id);
            return Ok(Value::Null);
        }
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or("AGENT_CORE_SESSION_NOT_FOUND".to_owned())?;
        let value = if request.operation == "session.state" {
            Value::Null
        } else {
            session.apply(&request.operation, &request.arguments)?
        };
        let state = session.snapshot()?;
        Ok(json!({"value": value, "state": state}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(engine: &mut AgentEngine, operation: &str, session: &str, arguments: Value) -> Value {
        engine.dispatch(Request {
            version: 1,
            id: 1,
            operation: operation.to_owned(),
            session: Some(session.to_owned()),
            arguments,
        })
    }

    #[test]
    fn request_state_isolated_and_closed_sessions_are_not_recreated() {
        let mut engine = AgentEngine::default();
        for id in ["first", "second"] {
            assert_eq!(
                invoke(&mut engine, "session.open", id, json!({"limits": {}}))["ok"],
                true
            );
        }
        invoke(&mut engine, "task.resumed", "first", json!({}));
        let second = invoke(&mut engine, "session.state", "second", json!({}));
        assert_eq!(second["result"]["state"]["task"]["resumes"], 0);
        invoke(&mut engine, "session.close", "first", json!({}));
        assert_eq!(
            invoke(&mut engine, "task.resumed", "first", json!({}))["error"]["code"],
            "AGENT_CORE_SESSION_NOT_FOUND"
        );
    }

    #[test]
    fn failed_admission_does_not_reset_limits_or_accept_counter_regression() {
        let mut engine = AgentEngine::default();
        invoke(
            &mut engine,
            "session.open",
            "run",
            json!({"limits": {"maxRounds": 2}}),
        );
        let counters = json!({"rounds": 2, "toolCalls": 1, "generatedTokens": 1});
        assert_eq!(
            invoke(&mut engine, "run.admit", "run", counters)["ok"],
            true
        );
        assert_eq!(
            invoke(
                &mut engine,
                "run.admit",
                "run",
                json!({"rounds": 1, "toolCalls": 1, "generatedTokens": 1})
            )["error"]["code"],
            "AGENT_CORE_COUNTER_REGRESSION"
        );
        assert_eq!(
            invoke(
                &mut engine,
                "run.admit",
                "run",
                json!({"rounds": 3, "toolCalls": 1, "generatedTokens": 1})
            )["error"]["code"],
            "TOOL_RUN_ROUND_LIMIT"
        );
    }
}
