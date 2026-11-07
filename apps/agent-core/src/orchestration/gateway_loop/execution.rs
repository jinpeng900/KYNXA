//! 根任务模型轮次保留准备阶段守卫，直到最终交付结束。

use super::preparation::RootExecution;
use super::*;

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
    closing_answer: Option<GatewayError>,
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
    let mut execution = Execution {
        runtime,
        run,
        input,
        prepared,
        rounds,
    };
    execution.start().await?;
    let mut round = execution.prepared.first_round;
    while round <= execution.rounds.last_round || execution.close_round_limit().await? {
        match execution.advance(round).await? {
            PhaseOutcome::Stop => {
                if !execution.begin_closing_answer().await? {
                    break;
                }
            }
            PhaseOutcome::Continue | PhaseOutcome::Ready(()) => {}
        }
        round += 1;
    }
    execution.finish().await
}

impl RoundState {
    fn new(prepared: &RootExecution) -> Self {
        let mut provider_input_limit = prepared.window["providerMaxInputTokens"]
            .as_u64()
            .or_else(|| prepared.connection["maxInputTokens"].as_u64());
        if let Some(limit) = prepared.observed_limits["providerMaxInputTokens"].as_u64() {
            provider_input_limit = Some(provider_input_limit.unwrap_or(limit).min(limit));
        }
        Self {
            task_status: json!({"state":"unknown"}),
            stopped: None,
            model_started: Instant::now(),
            final_received: false,
            final_evidence_validation: None,
            freshness_recoveries: 0,
            output_continuations: 0,
            review_pending: prepared.steering_applied == 0
                && prepared.state.completion.review_checkpoint["pending"] == true,
            review_evidence_required: prepared.state.completion.review_checkpoint["evidenceRequired"]
                == true,
            review_attempts: prepared.state.completion.review_checkpoint["attempts"]
                .as_u64()
                .unwrap_or(0),
            synthesized_version: None,
            grammar_recoveries: 0,
            context_recoveries: 0,
            image_fallback: false,
            provider_input_limit,
            projected_input_limit: prepared.observed_limits["estimatedInputBudgetTokens"].as_u64(),
            rejected_request: None,
            retry_next_request: false,
            empty_control_recoveries: 0,
            history_echo_recoveries: 0,
            closing_answer: None,
            minimum_active_ms: prepared.state.assistant["UserTaskDirective"]["minimumActiveMs"]
                .as_u64()
                .unwrap_or_else(|| prepared.limits["minActiveMs"].as_u64().unwrap_or(0)),
            segment_initial_calls: 0,
            segment_initial_generated: 0,
            segment_initial_active_ms: 0,
            last_round: 0,
        }
    }
}

impl Execution {
    /// 轮次用完时同样由模型收尾；只多给一轮。
    async fn close_round_limit(&mut self) -> Result<bool, GatewayError> {
        if self.rounds.closing_answer.is_some() || self.rounds.last_round >= MAX_MESSAGE_ROUNDS {
            return Ok(false);
        }
        self.rounds.stopped = Some(GatewayError::new(
            "TOOL_RUN_ROUND_LIMIT",
            "本次任务轮次已用完。",
            409,
        ));
        self.begin_closing_answer().await
    }

    /// 网关决定停下时关闭工具，让模型根据已有结果写完回答；已有回答时不再多写一遍。
    async fn begin_closing_answer(&mut self) -> Result<bool, GatewayError> {
        let state = &self.prepared.state;
        let writes_ending = self.rounds.closing_answer.is_none()
            && !self.rounds.final_received
            && !*self.prepared.cancellation.borrow()
            && state.delivery.pending_delivery.trim().is_empty()
            && !state.delivery.delivery_continuing
            && self.rounds.stopped.as_ref().is_some_and(|error| {
                delivery::writes_closing_answer(&error.code, self.run.journal.is_some())
                    && !delivery::answer_outgrew_review(
                        &error.code,
                        &state.transcript.rounds,
                        &state.content(),
                    )
            });
        if !writes_ending {
            return Ok(false);
        }
        let error = self.rounds.stopped.take().unwrap();
        self.prepared.state.broker_note(&format!(
            "Broker: work on this request ends here ({}). No tools are callable. Write the final answer for the user now in plain text from the saved results: what was done and, honestly, anything unfinished and why. Do not mention this note, internal checks or tool availability, and do not output status markers.",
            error.message
        ));
        self.rounds.closing_answer = Some(error);
        self.rounds.review_pending = false;
        self.prepared
            .state
            .save(&self.runtime, &self.run, "streaming", None)
            .await?;
        Ok(true)
    }

    /// 收尾回答写好后结束；任务状态仍按原来的停止原因记录。
    fn finish_closing_answer(&mut self, round: u64, observed: &ObservedRound) {
        let text = observed.inspected["content"].as_str().unwrap_or("");
        let answered = observed.model.response.calls.is_empty() && !text.trim().is_empty();
        self.prepared
            .state
            .push_transcript_round(json!({"round":round,"text":if answered { text } else { "" },"calls":[],
                "prefixFingerprint":observed.prefix_fingerprint,"nativeContinuationRef":observed.continuation_ref,
                "outputTruncated":observed.model.response.output_truncated,"reviewing":false}));
        self.rounds.stopped = if answered {
            self.rounds.closing_answer.take()
        } else {
            Some(GatewayError::new(
                "MODEL_RESPONSE_EMPTY",
                "模型未返回可见正文，请重试。",
                502,
            ))
        };
    }

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
        if self.rounds.closing_answer.is_some() {
            self.finish_closing_answer(round, &observed);
            return Ok(PhaseOutcome::Stop);
        }
        if observed.model.reviewing {
            return self.review_response(round, observed).await;
        }
        let broker_planned_call = self.plan_continuation(round, &mut observed).await?;
        if observed.model.response.calls.is_empty() {
            return self.complete_response(round, observed).await;
        }
        self.dispatch_tools(round, observed, broker_planned_call)
            .await
    }

    async fn finish(self) -> Result<Value, GatewayError> {
        let Self {
            runtime,
            run,
            prepared,
            rounds,
            ..
        } = self;
        let RootExecution {
            state,
            ctx,
            cancellation,
            preparation_stages,
            _queue,
            _request_tools,
            ..
        } = prepared;
        let RoundState {
            model_started,
            stopped,
            final_received,
            final_evidence_validation,
            task_status,
            review_pending,
            review_evidence_required,
            review_attempts,
            ..
        } = rounds;
        delivery::FinalDelivery {
            runtime: &runtime,
            run: &run,
            state,
            ctx,
            cancellation,
            model_started,
            preparation_stages,
            stopped,
            final_received,
            final_evidence_validation,
            task_status,
            review_pending,
            review_evidence_required,
            review_attempts,
        }
        .finish()
        .await
    }
}
