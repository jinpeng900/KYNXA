//! Root model rounds retain their prepared guards through final delivery.
//! 根任务模型轮次保留准备阶段守卫，直到最终交付结束。

use super::*;
use super::preparation::RootExecution;

mod preparation;
mod response;
mod streaming;

enum PhaseOutcome<T> {
    Continue,
    Stop,
    Ready(T),
}

struct Execution {
    runtime: Arc<GatewayRuntime>,
    run: Arc<RequestRun>,
    input: Value,
    prepared: RootExecution,
    rounds: RoundState,
}

struct RoundState {
    task_status: Value,
    stopped: Option<GatewayError>,
    model_started: Instant,
    final_received: bool,
    final_evidence_validation: Option<Value>,
    freshness_recoveries: u64,
    output_continuations: u64,
    review_pending: bool,
    review_evidence_required: bool,
    review_attempts: u64,
    synthesized_version: Option<String>,
    grammar_recoveries: u64,
    context_recoveries: u64,
    image_fallback: bool,
    provider_input_limit: Option<u64>,
    projected_input_limit: Option<u64>,
    rejected_request: Option<String>,
    retry_next_request: bool,
    empty_control_recoveries: u64,
    history_echo_recoveries: u64,
    minimum_active_ms: u64,
    segment_initial_calls: u64,
    segment_initial_generated: u64,
    segment_initial_active_ms: u64,
    last_round: u64,
}

struct RequestPreparation {
    output: Value,
    input_budget: u64,
    schemas: Vec<Value>,
    declared_tools: Vec<Value>,
    declarations_disabled: bool,
    tools_closed: bool,
    document_progress_notice: Option<String>,
    loaded_tool_notice: Option<String>,
    delivery_notice: Option<String>,
    strategy_notice: Option<String>,
    review_notice: Option<String>,
}

struct PreparedRound {
    output: Value,
    input_budget: u64,
    schemas: Vec<Value>,
    declared_tools: Vec<Value>,
    tools_closed: bool,
    summarized_work_changed: bool,
}

struct ModelRound {
    response: protocol::ModelResponse,
    segment: String,
    reviewing: bool,
    buffered_delivery: bool,
    schemas: Vec<Value>,
    declared_tools: Vec<Value>,
    tools_closed: bool,
    summarized_work_changed: bool,
}

struct ObservedRound {
    model: ModelRound,
    inspected: Value,
    echo_stripped: Option<String>,
    prefix_fingerprint: String,
    continuation_ref: Value,
}

pub(super) async fn run(
    runtime: Arc<GatewayRuntime>,
    run: Arc<RequestRun>,
    input: Value,
    prepared: RootExecution,
) -> Result<Value, GatewayError> {
    let rounds = RoundState::new(&prepared);
    let mut execution = Execution { runtime, run, input, prepared, rounds };
    execution.start().await?;
    for round in execution.prepared.first_round..=execution.rounds.last_round {
        match execution.advance(round).await? {
            PhaseOutcome::Stop => break,
            PhaseOutcome::Continue | PhaseOutcome::Ready(()) => {}
        }
    }
    execution.finish().await
}

impl RoundState {
    fn new(prepared: &RootExecution) -> Self {
        let mut provider_input_limit = prepared.window["providerMaxInputTokens"]
            .as_u64().or_else(|| prepared.connection["maxInputTokens"].as_u64());
        if let Some(limit) = prepared.observed_limits["providerMaxInputTokens"].as_u64() {
            provider_input_limit = Some(provider_input_limit.unwrap_or(limit).min(limit));
        }
        Self {
            task_status: json!({"state":"unknown"}), stopped: None,
            model_started: Instant::now(), final_received: false, final_evidence_validation: None,
            freshness_recoveries: 0, output_continuations: 0,
            review_pending: prepared.steering_applied == 0 && prepared.state.completion.review_checkpoint["pending"] == true,
            review_evidence_required: prepared.state.completion.review_checkpoint["evidenceRequired"] == true,
            review_attempts: prepared.state.completion.review_checkpoint["attempts"].as_u64().unwrap_or(0),
            synthesized_version: None, grammar_recoveries: 0, context_recoveries: 0, image_fallback: false,
            provider_input_limit,
            projected_input_limit: prepared.observed_limits["estimatedInputBudgetTokens"].as_u64(),
            rejected_request: None, retry_next_request: false,
            empty_control_recoveries: 0, history_echo_recoveries: 0,
            minimum_active_ms: prepared.state.assistant["UserTaskDirective"]["minimumActiveMs"]
                .as_u64().unwrap_or_else(|| prepared.limits["minActiveMs"].as_u64().unwrap_or(0)),
            segment_initial_calls: 0, segment_initial_generated: 0, segment_initial_active_ms: 0,
            last_round: 0,
        }
    }
}

impl Execution {
    async fn advance(&mut self, round: u64) -> Result<PhaseOutcome<()>, GatewayError> {
        match self.check_round(round).await? {
            PhaseOutcome::Ready(()) => {}
            PhaseOutcome::Continue => return Ok(PhaseOutcome::Continue),
            PhaseOutcome::Stop => return Ok(PhaseOutcome::Stop),
        }
        let request = match self.prepare_request(round).await? {
            PhaseOutcome::Ready(value) => value,
            PhaseOutcome::Continue => return Ok(PhaseOutcome::Continue),
            PhaseOutcome::Stop => return Ok(PhaseOutcome::Stop),
        };
        let request = match self.compact_request(round, request).await? {
            PhaseOutcome::Ready(value) => value,
            PhaseOutcome::Continue => return Ok(PhaseOutcome::Continue),
            PhaseOutcome::Stop => return Ok(PhaseOutcome::Stop),
        };
        let response = match self.request_round(round, request).await? {
            PhaseOutcome::Ready(value) => value,
            PhaseOutcome::Continue => return Ok(PhaseOutcome::Continue),
            PhaseOutcome::Stop => return Ok(PhaseOutcome::Stop),
        };
        let mut observed = match self.inspect_response(round, response).await? {
            PhaseOutcome::Ready(value) => value,
            PhaseOutcome::Continue => return Ok(PhaseOutcome::Continue),
            PhaseOutcome::Stop => return Ok(PhaseOutcome::Stop),
        };
        if observed.model.reviewing {
            return self.review_response(round, observed).await;
        }
        let broker_planned_call = self.plan_continuation(round, &mut observed).await?;
        if observed.model.response.calls.is_empty() {
            return self.complete_response(round, observed).await;
        }
        self.dispatch_tools(round, observed, broker_planned_call).await
    }

    async fn finish(self) -> Result<Value, GatewayError> {
        let Self { runtime, run, prepared, rounds, .. } = self;
        let RootExecution { state, ctx, cancellation,
            preparation_stages, _queue, _request_tools, .. } = prepared;
        let RoundState { model_started, stopped, final_received,
            final_evidence_validation, task_status, review_pending, review_evidence_required,
            review_attempts, .. } = rounds;
        delivery::FinalDelivery {
            runtime: &runtime, run: &run, state, ctx, cancellation,
            model_started, preparation_stages, stopped,
            final_received, final_evidence_validation, task_status, review_pending,
            review_evidence_required, review_attempts,
        }.finish().await
    }
}
