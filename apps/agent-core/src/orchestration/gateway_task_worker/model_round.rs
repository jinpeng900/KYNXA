//! Child model sessions own request budgets, cache projection, streaming and bounded protocol repair.
//! 子模型会话集中管理请求预算、缓存投影、流读取与有限协议修复。

use super::*;

pub(super) struct ChildModelSession {
    pub connection: Value,
    pub model: String,
    pub window: u64,
    allocation: Value,
    input_budget: u64,
    output_budget: u64,
    projected_input_limit: Option<u64>,
    context_repairs: u64,
    model_repairs: u64,
    retry_next_request: bool,
}

pub(super) struct ModelRound<'a> {
    pub runtime: &'a Arc<GatewayRuntime>,
    pub parent: &'a Arc<RequestRun>,
    pub run: &'a Arc<RequestRun>,
    pub journal: &'a Arc<TaskJournal>,
    pub child: &'a TaskRecord,
    pub ctx: &'a mut GatewayToolContext,
    pub state: &'a mut WorkerContext,
    pub prompt: &'a mut ChildPrompt,
    pub tools: &'a mut ChildTools,
    pub acceptance_query: &'a str,
    pub requires_readback: bool,
    pub requires_visual: bool,
    pub repairs: u64,
    pub limits: &'a Value,
}

struct PreparedRequest {
    body: Value,
    cache_policy: Value,
    delivered_visual_ids: Vec<String>,
}

impl ChildModelSession {
    pub(super) async fn connect(runtime: &Arc<GatewayRuntime>, run: &Arc<RequestRun>, input: &Value) -> Result<Self> {
        let provider = input["provider"].as_str().ok_or_else(|| {
            failure(
                "MODEL_CONFIG_INVALID",
                "Delegation requires the parent's model provider. / 委派须使用父模型供应商。",
            )
        })?;
        let model = input["model"].as_str().ok_or_else(|| {
            failure(
                "MODEL_CONFIG_INVALID",
                "Delegation requires a configured model. / 委派须指定已配置模型。",
            )
        })?;
        let model = model.to_owned();
        let mut connection = runtime
            .models
            .connection_for(provider)
            .await?
            .ok_or_else(|| {
                failure(
                    "MODEL_PROVIDER_NOT_FOUND",
                    "Delegated provider is unavailable. / 委派供应商不可用。",
                )
            })?;
        gateway_model_request::prepare_managed_connection(runtime, &mut connection, &model, run.cancellation.subscribe(),
            !run.is_background_recovery.load(std::sync::atomic::Ordering::Acquire)).await?;
        crate::models::provider_routes::project_connection(&mut connection, &model);
        let local =
            gateway_config::observe_local(&connection, &model, None, run.cancellation.subscribe())
                .await
                .unwrap_or(Value::Null);
        let automatic_context = gateway_config::automatic_context(&connection, &model, &local)?;
        let window =
            automatic_context["effectiveTokens"]
                .as_u64()
                .unwrap_or(32768);
        let mut allocation = json!({"contextWindowTokens":window,"requiredInputTokens":0});
        for (source, target) in [
            ("maxOutputTokens", "providerMaxOutputTokens"),
            ("maxInputTokens", "providerMaxInputTokens"),
        ] {
            if let Some(value) = connection.get(source).filter(|value| !value.is_null()) {
                allocation[target] = value.clone();
            }
            if let Some(observed) = automatic_context[target].as_u64() {
                allocation[target] = json!(allocation[target].as_u64().map_or(observed, |configured| configured.min(observed)));
            }
        }
        let allocated = crate::models::output::calculate("context.outputBudget", &allocation)
            .map_err(|code| failure(&code, "Child model budget is invalid. / 子模型预算无效。"))?;
        let input_budget = allocated["inputBudgetTokens"].as_u64().unwrap_or(4096);
        let output_budget = allocated["maxOutputTokens"].as_u64().unwrap_or(4096);
        Ok(Self { connection, model, window, allocation, input_budget, output_budget,
            projected_input_limit: None, context_repairs: 0, model_repairs: 0, retry_next_request: false })
    }

    pub(super) async fn receive(&mut self, mut round: ModelRound<'_>) -> Result<bool> {
        let prepared = self.prepare_request(&mut round).await?;
        let (response, partial, dispatched) = self.dispatch(&mut round, prepared.body, prepared.cache_policy).await?;
        let ModelRound {run, journal, state, tools, limits, child, ..} = &mut round;
        if response.is_err() && !partial.is_empty() {
            let overlap = budget::apply("output.overlap", &json!({"previous":state.output,"text":partial})).unwrap_or(json!(partial));
            state.output.push_str(overlap.as_str().unwrap_or(""));
            // Partial text is durable; incomplete calls are never dispatched.
            // 残缺正文持久保留，不派发未完成调用。
            state.history.push(json!({"role":"assistant","content":partial}));
            state.history.push(json!({"role":"user","content":"Broker: the previous stream ended before completion. Preserve its partial text; continue the original goal without repeating completed output or effects."}));
            checkpoint(run,journal,state,"partial-model-stream").await?;
        }
        state.rounds += u64::from(dispatched);
        let response = self.recover_response(run, journal, state, &mut tools.effects, child, response).await?;
        let Some(response) = response else {return Ok(false)};
        let delivered_visual_ids = prepared.delivered_visual_ids;
        for saved in &mut state.receipts {
            if saved["call"]["id"].as_str().is_some_and(|id| delivered_visual_ids.iter().any(|delivered| delivered == id)) {
                saved["visualDeliveredToModel"] = json!(true);
            }
        }
        self.context_repairs = 0;
        self.model_repairs = 0;
        state.generated_tokens += response.usage["output_tokens"]
            .as_u64()
            .or(response.usage["completion_tokens"].as_u64())
            .unwrap_or(crate::models::protocol::estimate_tokens(&response.content) as u64);
        if state.generated_tokens.saturating_sub(state.segment_generated_base) > limits["maxGeneratedTokens"].as_u64().unwrap_or(1_048_576) {
            state.segment_generated_base=state.generated_tokens;
        }
        state.pending = Some(response);
        state.memory_plan_invalidated=false;
        state.pending_review = state.review_pending;
        state.pending_results.clear();
        // Persist the model step and continuation signatures before dispatch.
        // 派发前保存完整模型步骤和续传签名。
        checkpoint(run,journal, state, "model-received").await?;
        Ok(true)
    }

    async fn prepare_request(&mut self, round: &mut ModelRound<'_>) -> Result<PreparedRequest> {
        let ModelRound {runtime, run, journal, child, ctx, state, prompt, tools, acceptance_query,
            requires_readback, requires_visual, repairs, limits, ..} = round;
        let ChildPrompt {system, cache_scope, cache_stable_system, live_memory, ..} = prompt;
        let ChildTools {catalog, effects, ..} = tools;
        let connection = &self.connection;
        let model = &self.model;
        let repairs = *repairs;
        let requires_readback = *requires_readback;
        let requires_visual = *requires_visual;
        if state.rounds.saturating_sub(state.segment_round_base) >= limits["maxRounds"].as_u64().unwrap_or(64) {
            state.segment_round_base = state.rounds;
            if !state.review_pending {
                state.history.push(json!({"role":"user","content":"Broker: the previous execution segment is saved. Continue the same delegated task with its unchanged requirements, outputs and operation receipts. Segment rollover does not certify completion and does not authorize replay."}));
            }
            checkpoint(run,journal,state,"round-segment-renewed").await?;
        }
        // Reconcile revocations/upgrades before pinning cached tools; add no child authority or connections.
        // 固定缓存工具前核对撤权和升级，不新增子任务权限或连接。
        **ctx = tools::checked_configuration(ctx).await?;
        let reusable = tools::reusable_mcp_catalog(&ctx);
        catalog.retain(|tool| !tool["name"].as_str().is_some_and(|name| name.starts_with("mcp.")) ||
            reusable.iter().any(|current| current["name"] == tool["name"]));
        for tool in catalog.iter_mut() {
            if let Some(current) = reusable.iter().find(|current| current["name"] == tool["name"]) {
                *tool = current.clone();
            }
        }
        let authority = json!([ctx.permission_mode,ctx.project_id,ctx.authorized_roots,
            ctx.config["configurationGeneration"],ctx.config["mcpConfigurationGeneration"],live_memory.block]);
        let authorized_schemas = gateway_model_request::model_tool_declarations(protocol::protocol(&connection), &catalog);
        let authorized_schemas = if repairs > 0 {
            protocol::project_tool_declarations(&connection,&authorized_schemas,"structural")?
        } else { authorized_schemas };
        state.tool_series.reconcile(&connection,&authorized_schemas,&authority);
        let mut required = state.tool_series.required_tool_names(&connection);
        let mut declared_catalog = catalog.clone();
        declared_catalog.sort_by_key(|tool| {
            if ["tool.search", "tool.load"].contains(&tool["name"].as_str().unwrap_or("")) {
                0
            } else if state
                .loaded_tools
                .iter()
                .any(|loaded| loaded["name"] == tool["name"])
            {
                1
            } else {
                2
            }
        });
        // Preserve the goal and latest instruction, not the whole compressible history, before reserving output.
        // 分配输出前保留目标及最新指令，旧历史仍可正常压缩，不能整段当作不可缩减输入。
        let mut mandatory_messages = vec![json!({"role":"user","content":child.goal})];
        if let Some(message) = state.history.iter().rev().find(|message| message["role"] == "user")
            && message != &mandatory_messages[0]
        {
            mandatory_messages.push(message.clone());
        }
        let review_notice = state.review_pending.then(|| {
            let review = ChildCompletionReview::new(state, &acceptance_query);
            review_instruction(state, &review, requires_readback, requires_visual)
        });
        let review_tokens = review_notice.as_ref().filter(|notice|
            state.history.last().is_none_or(|message| message["content"].as_str() != Some(notice.as_str())))
            .map_or(0, |notice| {
                let message = json!({"role":"user","content":notice});
                let tokens = crate::models::protocol::tool_message_tokens(std::slice::from_ref(&message), "");
                mandatory_messages.push(message);
                tokens
            });
        let essential_schemas = gateway_model_request::model_tool_declarations(
            protocol::protocol(connection), &declared_catalog[..declared_catalog.len().min(2)]);
        self.allocation["requiredInputTokens"] = json!(crate::models::protocol::tool_message_tokens(
            &mandatory_messages, system).saturating_add(crate::models::protocol::estimate_tokens(&json!(essential_schemas).to_string())));
        let allocated = crate::models::output::calculate("context.outputBudget", &self.allocation)
            .map_err(|code| failure(&code, "Child model budget is invalid. / 子模型预算无效。"))?;
        self.input_budget = allocated["inputBudgetTokens"].as_u64().unwrap_or(4096)
            .min(self.projected_input_limit.unwrap_or(u64::MAX));
        self.output_budget = allocated["maxOutputTokens"].as_u64().unwrap_or(4096);
        while declared_catalog.len() > 2
            && crate::models::protocol::estimate_tokens(
                &json!(gateway_model_request::model_tool_declarations(
                    protocol::protocol(&connection),
                    &declared_catalog
                ))
                .to_string(),
            )
            .saturating_add(crate::models::protocol::estimate_tokens(&system))
                > self.input_budget * 7 / 10
        {
            if let Some(position) = declared_catalog.iter().rposition(|tool|
                !required.contains(&crate::models::protocol::wire_name(tool["name"].as_str().unwrap_or("")))) {
                declared_catalog.remove(position);
            } else {
                let fixed = crate::models::protocol::estimate_tokens(&json!(gateway_model_request::model_tool_declarations(
                    protocol::protocol(&connection), &declared_catalog)).to_string())
                    .saturating_add(crate::models::protocol::estimate_tokens(&system));
                if fixed > self.input_budget {
                    // Rebuild declarations for hard-window limits only; soft compaction retains authorized pinned tools.
                    // 仅硬窗口不足时重建声明，普通压缩保留已授权固定工具。
                    state.tool_series.reset();
                    required.clear();
                } else { break; }
            }
        }
        let previous_compactions = state.compactions;
        compact_child(
            runtime,
            run,
            &ctx,
            journal,
            &connection,
            model,
            self.window,
            &system,
            state,
            &declared_catalog,
            self.input_budget.saturating_sub(review_tokens),
            &self.allocation,
            repairs > 0,
        )
        .await?;
        if state.compactions > previous_compactions {
            let _ = effects.apply("effects.protect", &json!({"includeCurrent":true}));
        }
        if let Some(notice) = review_notice {
            if state.history.last().is_none_or(|message| message["content"] != notice) {
                state.history.push(json!({"role":"user","content":notice}));
            }
        }
        let schemas = gateway_model_request::model_tool_declarations(
            protocol::protocol(&connection),
            &declared_catalog,
        );
        let schemas = if repairs > 0 {
            protocol::project_tool_declarations(&connection, &schemas, "structural")?
        } else {
            schemas
        };
        let mut body = protocol::build_request(
            &connection,
            model,
            &state.history,
            &system,
            Some(if state.review_pending {
                budget::review_output_limit(state.acceptance_requirements.len(), state.review_attempts, self.output_budget)
            } else { self.output_budget }),
            &schemas,
            true,
        )?;
        let tool_update = crate::models::tool_updates::project(&connection, &mut body, &mut state.tool_series);
        let mut cache_policy = protocol::apply_prompt_cache(&connection, model, &mut body, &cache_scope, &cache_stable_system);
        if !tool_update.is_null() { cache_policy["toolUpdates"] = tool_update; }
        let body_has_images = crate::models::protocol::image_inputs(&body) > 0;
        let body_hashes=if body_has_images{task::model_visual_hashes(&body)}else{Vec::new()};
        let delivered_visual_ids: Vec<String> = state.receipts.iter().filter(|saved|
            saved["visualAvailable"] == true && saved["visualHashes"].as_array().into_iter().flatten()
                .filter_map(Value::as_str).any(|hash|body_hashes.iter().any(|actual|actual==hash)))
            .filter_map(|saved| saved["call"]["id"].as_str().map(str::to_owned)).collect();
        Ok(PreparedRequest { body, cache_policy, delivered_visual_ids })
    }

    async fn dispatch(&mut self, round: &mut ModelRound<'_>, body: Value, cache_policy: Value)
        -> Result<(std::result::Result<protocol::ModelResponse, crate::models::gateway::ModelError>, String, bool)> {
        let ModelRound {runtime, parent, run, child, ctx, state, tools, ..} = round;
        let connection = &self.connection;
        let model = &self.model;
        let catalog = &tools.catalog;
        parent.emit(json!({"type":"task_progress","taskId":child.task_id,"phase":"model","round":state.rounds+1})).await;
        let (events, receiver) = tokio::sync::mpsc::channel::<Value>(64);
        let stream_reader = tokio::spawn(collect_stream(run.clone(), receiver));
        let model_events = if state.review_pending { drop(events); None } else { Some(events) };
        let ModelRequestOutcome { result: response, dispatched, .. } = gateway_model_request::request_model(
            runtime,
            run,
            &ctx,
            &connection,
            model,
            self.window,
            body,
            &catalog,
            run.cancellation.subscribe(),
            model_events,
            Some(cache_policy),
            std::mem::take(&mut self.retry_next_request),
        )
        .await;
        let partial = stream_reader.await.map_err(|_| {
            failure(
                "CHILD_STREAM_READER_FAILED",
                "Child stream reader ended unexpectedly. / 子任务流读取者意外退出。",
            )
        })?;
        Ok((response, partial, dispatched))
    }

    async fn recover_response(&mut self, run: &Arc<RequestRun>, journal: &Arc<TaskJournal>,
        state: &mut WorkerContext, effects: &mut EffectLedger, child: &TaskRecord,
        response: std::result::Result<protocol::ModelResponse, crate::models::gateway::ModelError>) -> Result<Option<protocol::ModelResponse>> {
        match response {
            Ok(response) => Ok(Some(response)),
            Err(error)
                if error.details["contextRejection"].is_object() && self.context_repairs < 2 =>
            {
                let plan = crate::models::recovery::calculate("models.recovery.contextPlan", &json!({"errorCode":error.code,"isRequestFactory":true,"canRebuildContext":true,"recoveryAttempts":self.context_repairs,"rejection":error.details["contextRejection"],"current":{"contextWindowTokens":self.window,"inputBudgetTokens":self.input_budget,"maxOutputTokens":self.output_budget,"providerMaxInputTokens":self.allocation["providerMaxInputTokens"]},"cacheKey":child.task_id,"cacheKeys":[],"cacheEntry":{},"now":chrono::Utc::now().timestamp_millis()})).map_err(|code| failure(&code, "Child context recovery failed. / 子上下文恢复失败。"))?;
                if plan.is_null() {
                    return Err(error.into());
                }
                self.context_repairs = plan["recoveryAttempts"]
                    .as_u64()
                    .unwrap_or(self.context_repairs + 1);
                self.retry_next_request = true;
                self.window = plan["limits"]["contextWindowTokens"]
                    .as_u64()
                    .unwrap_or(self.window);
                self.allocation["contextWindowTokens"] = json!(self.window);
                for field in ["providerMaxOutputTokens", "providerMaxInputTokens"] {
                    if let Some(value) =
                        plan["limits"].get(field).filter(|value| !value.is_null())
                    {
                        self.allocation[field] = value.clone();
                    }
                }
                if let Some(limit) = plan["limits"]["estimatedInputBudgetTokens"].as_u64() {
                    self.projected_input_limit =
                        Some(self.projected_input_limit.map_or(limit, |previous| previous.min(limit)));
                }
                let narrowed = crate::models::output::calculate("context.outputBudget", &self.allocation).map_err(|code| failure(&code,"Provider context limit is too small for this task. / 供应商上下文限制不足以容纳当前任务。"))?;
                self.input_budget = narrowed["inputBudgetTokens"]
                    .as_u64()
                    .unwrap_or(self.input_budget)
                    .min(self.projected_input_limit.unwrap_or(u64::MAX));
                self.output_budget = narrowed["maxOutputTokens"]
                    .as_u64()
                    .unwrap_or(self.output_budget);
                let _ = effects.apply("effects.protect", &json!({"includeCurrent":true}));
                journal.emit_control(
                    "task.context.recovery",
                    &json!({"limits":plan["limits"],"attempt":self.context_repairs}),
                )?;
                Ok(None)
            }
            Err(error)
                if [
                        "MODEL_TOOL_CALL_INVALID",
                        "MODEL_TOOL_ARGUMENT_INVALID",
                        "MODEL_RESPONSE_INCOMPLETE",
                        "MODEL_TOOL_SCHEMA_REJECTED",
                        "MODEL_LOCAL_TOOL_GRAMMAR_REJECTED",
                    ]
                    .contains(&error.code.as_str()) && self.model_repairs < 2 =>
            {
                self.model_repairs += 1;
                self.retry_next_request = true;
                journal.emit_control(
                    "task.model.recovery",
                    &json!({"code":error.code,"attempt":self.model_repairs}),
                )?;
                let _ = effects.apply("effects.protect", &json!({"includeCurrent":true}));
                state.history.push(json!({"role":"user","content":format!("Broker: previous model step failed validation ({}). Generate the next complete valid step. Do not repeat effects already completed; original receipts remain valid.", error.code)}));
                checkpoint(run,journal, state, "model-recovery").await?;
                let mut repair_cancel = run.cancellation.subscribe();
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis((self.model_repairs * 250).min(2000))) => {}, _ = tools::wait_for_cancellation(&mut repair_cancel) => {} }
                Ok(None)
            }
            Err(mut error) => {
                if error.details["type"] == "interrupted" {
                    error.code = "MODEL_STREAM_INCOMPLETE".into();
                }
                return Err(error.into());
            }
        }
    }
}

async fn collect_stream(streamed_run: Arc<RequestRun>, mut receiver: tokio::sync::mpsc::Receiver<Value>) -> String {
    let mut projection = task::TaskTextProjection::default();
    // The child stream uses the same public projection as the root; raw responses remain in checkpoints.
    // 子任务与根任务使用相同的公开投影；原始模型响应继续保存在检查点中。
    let mut history_filter = task::HistoryEchoFilter::default();
    let mut text = String::new();
    while let Some(mut event) = receiver.recv().await {
        match event["type"].as_str() {
            Some("text_delta") => {
                let delta = history_filter.push(&projection.push(event["delta"].as_str().unwrap_or("")));
                if delta.is_empty() {
                    continue;
                }
                text.push_str(&delta);
                event["delta"] = json!(delta);
            }
            Some("content_snapshot") => {
                projection = task::TaskTextProjection::default();
                history_filter = task::HistoryEchoFilter::default();
                text = history_filter.push(&projection.push(event["content"].as_str().unwrap_or("")));
                text.push_str(&history_filter.push(&projection.finish()));
                text.push_str(&history_filter.finish());
                event["content"] = json!(text);
            }
            _ => {}
        }
        streamed_run.emit(event).await;
    }
    let mut tail = history_filter.push(&projection.finish());
    tail.push_str(&history_filter.finish());
    if !tail.is_empty() {
        text.push_str(&tail);
        streamed_run.emit(json!({"type":"text_delta","delta":tail})).await;
    }
    text
}
