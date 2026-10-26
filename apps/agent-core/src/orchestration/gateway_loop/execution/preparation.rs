//! Admission, request budgeting and compaction for one root model round.
//! 单轮根模型请求的准入、预算和压缩，保留各自重试及停止出口。

use super::*;

impl Execution {
    pub(super) async fn start(&mut self) -> Result<(), GatewayError> {
    self.prepared.preparation_stages["contextAssemblyMs"] = json!(self.prepared.preparation_stage_started.elapsed().as_millis() as u64);
    self.prepared.state.delivery.model_started = Some(self.rounds.model_started);
    self.run.emit(json!({"type":"generation_timing","replyMessageId":self.prepared.state.assistant["Id"],
        "generationElapsedMs":crate::orchestration::gateway_runtime::generation_elapsed_ms(
            self.prepared.state.delivery.generation_duration_base_ms, self.prepared.state.delivery.model_started),
        "generationActive":true})).await;
    // Roll over bounded messages without ending their owning task.
    // 消息达到存储边界时续段，不结束所属任务。
    if self.prepared.state.needs_message_continuation(self.prepared.first_round) {
        let previous_reply = self.prepared.state.assistant["Id"].clone();
        let reply_id = new_id();
        self.prepared.state.assistant["TaskContinues"] = json!(true);
        self.prepared.state.assistant["ContinuedByReplyMessageId"] = json!(reply_id);
        self.prepared.state.save(&self.runtime, &self.run, "completed", None).await?;
        self.prepared.state.continue_reply_projection(&reply_id);
        self.prepared.first_round = 1;
        self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
        self.run.emit(json!({"type":"reply_anchor","replyMessageId":reply_id,"userMessageId":self.prepared.state.assistant["ReplyTo"],"previousReplyMessageId":previous_reply,"toolStreamProtocol":3})).await;
    }
    self.prepared.ctx.config["activeAssistantMessageId"] = self.prepared.state.assistant["Id"].clone();
    self.prepared.ctx.config["operationNamespace"] = self.prepared.state.assistant["OperationNamespace"].clone();
    self.rounds.segment_initial_calls = count_tool_attempts(&self.prepared.state.assistant);
    self.rounds.segment_initial_generated = self.prepared.state.generated;
    self.rounds.segment_initial_active_ms = self.run.active_elapsed(self.run.created).as_millis() as u64;
    self.rounds.last_round = (self.prepared.first_round + self.prepared.limits["maxRounds"].as_u64().unwrap_or(64) - 1).min(MAX_MESSAGE_ROUNDS);
        Ok(())
    }

    pub(super) async fn check_round(&mut self, round: u64) -> Result<PhaseOutcome<()>, GatewayError> {
        let permission = self.input["permissionMode"].as_str().unwrap_or("smart");
        if *self.prepared.cancellation.borrow() {
            self.rounds.stopped = Some(GatewayError::new(
                "ABORT_ERR",
                "任务已取消，已完成内容保留。",
                499,
            ));
            return Ok(PhaseOutcome::Stop);
        }
        if apply_user_steering(&self.runtime, &self.run, &mut self.prepared.state, &mut self.prepared.ctx).await? > 0 {
            self.rounds.review_pending = false;
            self.prepared.ctx.config["activeAssistantMessageId"] = self.prepared.state.assistant["Id"].clone();
            let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
        }
        if self.prepared.state.assistant["UserTaskDirective"]["stop"] == true {
            // Recovery saves an already accepted stop without another paid model request.
            // 恢复只保存已接受的停止，不再发起计费模型请求。
            self.rounds.final_received = true;
            self.rounds.task_status = json!({"state":"blocked","source":"user-steering","reason":"user-requested-stop"});
            return Ok(PhaseOutcome::Stop);
        }
        if let Some(status) = self.prepared.live_memory.refresh(&self.runtime.config.data_root, &self.run.conversation_id, &self.run.request_id, &mut self.prepared.system).await? {
            memory_refresh_notice(&mut self.prepared.state, &status);
            self.rounds.review_pending = false;
        }
        if self.prepared.connection["routeProjection"]["toolUpdate"] == "addition-only" {
            self.prepared.ctx = tools::checked_configuration(&self.prepared.ctx).await?;
            let mut authorized = tools::builtin_catalog();
            authorized.extend(tools::reusable_mcp_catalog(&self.prepared.ctx));
            self.prepared.all = authorized.into_iter().filter(tools::executable_descriptor)
                .map(|tool|descriptor(&tool,tool["name"].as_str().unwrap_or(""))).collect();
            self.prepared.state.tools.selected = self.prepared.state.tools.selected.iter().filter_map(|previous|
                self.prepared.all.iter().find(|tool|tool["name"]==previous["name"]).cloned()).collect();
            let native = protocol::project_tool_declarations(&self.prepared.connection,
                &model_tool_declarations(&self.prepared.protocol_name,&self.prepared.all), if self.rounds.grammar_recoveries > 0 {"structural"} else {"bounded"})?;
            self.prepared.state.cache.tool_series.reconcile(&self.prepared.connection,&native,&json!([
                runtime_authority(&self.prepared.ctx,permission,&self.prepared.live_memory.block),self.prepared.ctx.config["configurationGeneration"],
                self.prepared.ctx.config["mcpConfigurationGeneration"]]));
            for required in self.prepared.state.cache.tool_series.required_tool_names(&self.prepared.connection) {
                if let Some(tool) = self.prepared.all.iter().find(|tool|crate::models::protocol::wire_name(tool["name"].as_str().unwrap_or("")) == required)
                    && !self.prepared.state.tools.selected.iter().any(|selected|selected["name"]==tool["name"]) {
                    self.prepared.state.tools.selected.push(tool.clone());
                }
            }
        }
        self.prepared.state.refresh_runtime_snapshot(&runtime_authority(&self.prepared.ctx, permission, &self.prepared.live_memory.block), self.prepared.runtime_role, protocol::protocol(&self.prepared.connection) == "anthropic-messages");
        if self.prepared.state.needs_message_continuation(round) {
            self.rounds.stopped = Some(GatewayError::new("TOOL_RUN_MESSAGE_CAPACITY",
                "回复消息已到容量边界，原任务保留上下文与已完成操作自动续接。", 409));
            return Ok(PhaseOutcome::Stop);
        }
        if crate::models::message_attachments::project_text_only_model(&self.prepared.connection, &mut self.prepared.state.context.history) {
            self.rounds.image_fallback = true;
            self.prepared.state.broker_note("The configured local model accepts text only. Original image attachments are preserved; read their text with attachment.read (OCR) when needed. Do not claim to have inspected image pixels.");
        }
        if (self.run.active_elapsed(self.run.created).as_millis() as u64).saturating_sub(self.rounds.segment_initial_active_ms)
            > self.prepared.limits["maxDurationMs"].as_u64().unwrap_or(1800000)
        {
            self.rounds.stopped = Some(GatewayError::new(
                "TOOL_RUN_TIMEOUT",
                "任务总时间已到，已完成内容保留。",
                504,
            ));
            return Ok(PhaseOutcome::Stop);
        }
        Ok(PhaseOutcome::Ready(()))
    }

    pub(super) async fn prepare_request(&mut self, round: u64) -> Result<PhaseOutcome<RequestPreparation>, GatewayError> {
        // Reserve active acceptance before compaction; never summarize it away.
        // 压缩前预留当前验收要求，不能把它当作旧对话删掉。
        let pending_tool_intent = !crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"])
            && crate::orchestration::task::has_unexecuted_tool_intent(&self.prepared.state.completion.review_checkpoint);
        let protocol_notice = pending_tool_intent.then(|| format!(
            "Broker: earlier text proposed these tools without dispatching them: {}. This is navigation, not an added acceptance requirement. Reuse the current requirements and valid receipts; execute a proposal through native tools only if it fills an actual gap. If acceptance is met, answer without extra tools. Never claim a text proposal ran or replay a settled effect.",
            self.prepared.state.completion.review_checkpoint["unexecutedToolIntent"]["names"]));
        append_control_notice(&mut self.prepared.state, "nativeToolProtocolNotice", protocol_notice.as_deref());
        if !self.rounds.review_pending { refresh_acceptance_reminder(&mut self.prepared.state, round); }
        let review_notice = self.rounds.review_pending.then(|| completion_review_instruction(&self.prepared.state));
        let used_calls = count_tool_attempts(&self.prepared.state.assistant);
        let delivery = self.prepared.state
            .work_progress
            .delivery_plan(self.prepared.limits["maxToolCalls"].as_u64().unwrap_or(256), used_calls);
        let verification = completion_review(&self.prepared.state).verification;
        let has_output = self.prepared.state.context.result_sources.iter().any(|source| {
            source["status"] == "completed"
                && source["executed"] != false
                && matches!(
                    source["name"].as_str(),
                    Some("filesystem.write" | "filesystem.edit" | "document.create")
                )
        });
        let delivery_version = json!({"revision":verification["mutationRevision"],
            "signature":verification["codeVersionCoverage"]["latestSnapshot"]["signature"]})
        .to_string();
        // Prefer delivery after verification while retaining the native tool channel for real gaps.
        // 核验后优先交付，仍保留原生工具通道处理真实缺口。
        let synthesize_delivery = !self.rounds.review_pending
            && cached_completion_review(&self.prepared.state).is_none_or(|review| review["action"] != "resume")
            && has_output
            && verification["pendingValidation"] == false
            && (verification["state"] == "checks-passed"
                || verification["documentValidation"]["targetCount"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
                    && delivery.deprioritize_discovery_and_search)
            && self.rounds.synthesized_version.as_ref() != Some(&delivery_version);
        if synthesize_delivery {
            self.rounds.synthesized_version = Some(delivery_version);
        }
        // Tool limits stop dispatch only; answers retain context and the negotiated output budget.
        // 工具限额只停止派发，答复保留上下文和已协商输出额度。
        let tools_closed = delivery.tools_exhausted;
        let declarations_disabled = !self.prepared.supports_tools || tools_closed;
        let mut delivery_notice = (!self.rounds.review_pending && (delivery.deprioritize_discovery_and_search || synthesize_delivery)).then(|| {
            format!(
            "Delivery phase: {}. Reuse successful outputs, saved findings and actual validation receipts. Finish the final answer and report the artifact and actual result. {}",
                serde_json::to_value(&delivery).unwrap_or(Value::Null),
                if declarations_disabled { "No tools are callable in this response. Do not simulate tool calls using DSML, XML or JSON. Give the user-readable final answer from current evidence and completed outputs; state any genuinely unfinished requirement." }
                else if synthesize_delivery { "Recorded output readback or execution checks are available. Compare them with the original user goal and prefer a concise final answer when it is met. Native tools remain available for a genuinely missing action or validation; use the formal protocol rather than simulating DSML, XML or JSON calls as text. An unfinished requirement remains work in the original task." }
                else { "Complete missing delivery steps with current tools; combine compatible authorized actions." }
            )
        });
        let mut document_progress_notice = document_notice(&self.prepared.state);
        let mut strategy_notice = (!self.rounds.review_pending && self.prepared.state.work_progress.needs_strategy_change()).then(||
            format!("{} rounds added no work. Reuse saved analyses and successfully written outputs. Finish the remaining delivery or validation steps; read only an identified missing fact. Stop repeating directory and known-tool listings.",self.prepared.state.work_progress.stagnant_rounds()));
        let tool_phase = !declarations_disabled
            && !self.prepared.state.delivery.segments.last().is_some_and(|segment| segment["isAnswerContinuation"] == true)
            && (!self.prepared.hints.is_empty() || self.prepared.state.transcript.rounds.last().is_some_and(|turn| {
                turn["calls"].as_array().is_some_and(|calls| !calls.is_empty())
            }));
        let mut mandatory_messages = self.prepared.state.context.history
            .iter()
            .enumerate()
            .filter(|(index, message)| {
                let source = self.prepared.state.context.history_sources.get(*index);
                message["role"] == "user"
                    && source.is_none_or(|source| {
                        source["projectionOrigin"].is_null() && source["turnIndex"].is_null()
                    })
            })
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        if let Some(notice) = &review_notice {
            mandatory_messages.push(json!({"role":"user","content":notice}));
        }
        if let Some(snapshot) = &self.prepared.state.context.runtime_snapshot {
            mandatory_messages.push(snapshot["message"].clone());
        }
        // Bound repeated guidance before schema admission and compaction to protect working context.
        // 声明授额和压缩前限制重复指导，保留目标与工作事实空间。
        let mut essential_tools = if declarations_disabled { vec![] } else { self.prepared.state.tools.selected.clone() };
        while trim_optional_tool(&mut essential_tools) {}
        let essential_schema_tokens = crate::models::protocol::estimate_tokens(
            &json!(model_tool_declarations(&self.prepared.protocol_name, &essential_tools)).to_string());
        // Reserve immutable input before output; a small window must not compact an already fitting goal.
        // 先保留不可缩减的输入，再分配输出，避免小窗口把本来放得下的目标判为无法压缩。
        let required_input_tokens = crate::models::protocol::tool_message_tokens(
            &mandatory_messages, &self.prepared.system).saturating_add(essential_schema_tokens);
        let mut output_args = json!({"contextWindowTokens":self.prepared.context_window,
            "requiredInputTokens":required_input_tokens,"phase":if tool_phase {"tool"} else {"answer"}});
        if let Some(limit) = self.prepared.max_output {
            output_args["providerMaxOutputTokens"] = json!(limit);
        }
        if let Some(limit) = self.rounds.provider_input_limit {
            output_args["providerMaxInputTokens"] = json!(limit);
        }
        let output = native(crate::models::output::calculate(
            "context.outputBudget",
            &output_args,
        ))?;
        let input_budget = output["inputBudgetTokens"].as_u64().unwrap_or(4096)
            .min(self.rounds.projected_input_limit.unwrap_or(u64::MAX));
        let remaining_work_budget = input_budget
            .saturating_sub(crate::models::protocol::tool_message_tokens(&mandatory_messages, &self.prepared.system))
            .saturating_sub(essential_schema_tokens)
            .saturating_sub(512);
        let advisory_budget = (input_budget / 6).min(1024).min(remaining_work_budget);
        if document_progress_notice.as_ref().is_some_and(|notice|
            crate::models::protocol::tool_message_tokens(&[json!({"role":"user","content":notice})], "") > advisory_budget / 2)
        {
            document_progress_notice = bounded_advisory_notice(
                Some(compact_document_notice(&self.prepared.state.documents.directory(), &self.prepared.state.context.result_sources, advisory_budget / 2)), "", advisory_budget / 2);
        }
        delivery_notice = bounded_advisory_notice(delivery_notice,
            if declarations_disabled { "Delivery: tools are unavailable this turn. Answer from verified outputs; do not simulate calls. State any unfinished requirement." }
            else if synthesize_delivery { "Delivery: compare verified outputs with the user goal. Native tools remain available for missing actions or validation; use formal calls and continue the original task. Do not simulate calls." }
            else { "Finish the remaining delivery and validation using saved evidence. Reuse successful outputs; do not recreate them." },
            advisory_budget / 4);
        strategy_notice = bounded_advisory_notice(strategy_notice,
            "Repeated work added no progress. Reuse saved findings; perform the next unfinished step or deliver the final answer.", advisory_budget / 8);
        // Budget schemas against mandatory guidance; compact old history afterward.
        // 工具声明按必要指导授额，旧历史随后压缩。
        // Include all mandatory request messages in schema admission, not just system text.
        // 声明准入计入实际发送的全部必需消息，不只计算 system。
        for notice in [
            &document_progress_notice,
            &delivery_notice,
            &strategy_notice,
        ]
        .into_iter()
        .flatten()
        {
            mandatory_messages.push(json!({"role":"user","content":notice}));
        }
        let occupied = crate::models::protocol::tool_message_tokens(&mandatory_messages, &self.prepared.system);
        // Keep saved-work discovery without replacing the model's newly loaded capability.
        // 保留成果发现入口，不替换模型刚加载的下一步能力。
        if !self.prepared.state.documents.directory()["documents"]
            .as_array()
            .is_none_or(|entries| entries.is_empty())
        {
            if !self.prepared.state.tools.selected
                .iter()
                .any(|tool| tool["name"] == "task.document")
            {
                if let Some(tool) = self.prepared.all.iter().find(|tool| tool["name"] == "task.document") {
                    self.prepared.state.tools.selected.push(descriptor(tool, "task.document"));
                }
            }
        }
        if !self.prepared.state.context.result_sources.is_empty()
            && !self.prepared.state.tools.selected
                .iter()
                .any(|tool| tool["name"] == "tool.result.read")
        {
            if let Some(tool) = self.prepared.all.iter().find(|tool| tool["name"] == "tool.result.read") {
                self.prepared.state.tools.selected.push(tool.clone());
            }
        }
        // Delivery advice must not displace the newly loaded capability.
        // 交付建议不能挤掉模型刚加载的能力。
        let mut declared_tools = self.prepared.state.tools.selected.clone();
        let mut pinned_tool_names = self.prepared.state.cache.tool_series.required_tool_names(&self.prepared.connection);
        let mut schemas = if declarations_disabled {
            vec![]
        } else {
            model_tool_declarations(&self.prepared.protocol_name, &self.prepared.state.tools.selected)
        };
        schemas = match protocol::project_tool_declarations(&self.prepared.connection, &schemas, "bounded") {
            Ok(schemas) => schemas,
            Err(error) => {
                self.rounds.stopped = Some(error.into());
                return Ok(PhaseOutcome::Stop);
            }
        };
        if self.rounds.grammar_recoveries > 0 {
            schemas = match protocol::project_tool_declarations(&self.prepared.connection, &schemas, "structural")
            {
                Ok(schemas) => schemas,
                Err(error) => {
                    self.rounds.stopped = Some(error.into());
                    return Ok(PhaseOutcome::Stop);
                }
            };
        }
        // A smaller declared catalog reclaims unused context without hiding discovery capability.
        // 缩小已声明目录以回收上下文，保留完整能力发现入口。
        loop {
            // Review/delivery turns stay tool-free even when the reserved schema budget is zero.
            // 核验及无工具交付轮次即使声明预算为零，也不能在裁剪时重新装回工具。
            if declarations_disabled { break; }
            let loaded_notice_tokens = if declarations_disabled {
                0
            } else {
                bounded_loaded_tools_notice(&self.prepared.state.tools.selected, &declared_tools, advisory_budget / 8).map_or(0, |notice| {
                    crate::models::protocol::tool_message_tokens(
                        &[json!({"role":"user","content":notice})],
                        "",
                    )
                })
            };
            let schema_budget = crate::models::context_control::tool_declaration_budget(
                input_budget,
                occupied.saturating_add(loaded_notice_tokens),
            );
            if crate::models::protocol::estimate_tokens(&json!(schemas).to_string())
                    <= schema_budget
            {
                break;
            }
            if !trim_optional_tool_preserving(&mut declared_tools, &pinned_tool_names) {
                let actual_input = occupied.saturating_add(loaded_notice_tokens)
                    .saturating_add(crate::models::protocol::estimate_tokens(&json!(schemas).to_string())).saturating_add(512);
                if pinned_tool_names.is_empty() || actual_input <= input_budget { break; }
                self.prepared.state.cache.tool_series.reset();
                pinned_tool_names.clear();
                if !trim_optional_tool(&mut declared_tools) { break; }
            }
            schemas = model_tool_declarations(&self.prepared.protocol_name, &declared_tools);
            schemas = match protocol::project_tool_declarations(
                &self.prepared.connection,
                &schemas,
                if self.rounds.grammar_recoveries > 0 {
                    "structural"
                } else {
                    "bounded"
                },
            ) {
                Ok(schemas) => schemas,
                Err(error) => {
                    self.rounds.stopped = Some(error.into());
                    return Ok(PhaseOutcome::Stop);
                }
            };
        }
        let loaded_tool_notice = if declarations_disabled {
            None
        } else {
            bounded_loaded_tools_notice(&self.prepared.state.tools.selected, &declared_tools, advisory_budget / 8)
        };
        Ok(PhaseOutcome::Ready(RequestPreparation {
            output, input_budget, schemas, declared_tools, declarations_disabled, tools_closed,
            document_progress_notice, loaded_tool_notice, delivery_notice, strategy_notice, review_notice,
        }))
    }

    pub(super) async fn compact_request(&mut self, round: u64, request: RequestPreparation) -> Result<PhaseOutcome<PreparedRound>, GatewayError> {
        let RequestPreparation { mut output, mut input_budget, schemas, mut declared_tools,
            declarations_disabled, tools_closed, document_progress_notice, loaded_tool_notice,
            delivery_notice, strategy_notice, review_notice } = request;
        let permission = self.input["permissionMode"].as_str().unwrap_or("smart");
        let activity_round = self.prepared.state.delivery.segments
            .last()
            .and_then(|item| item["round"].as_u64())
            .unwrap_or(round);
        let (compact_sender, compact_events) = CompactionEvents::start(
            &self.run, self.prepared.state.delivery.timeline_order, activity_round,
        );
        let (summary_executor, summary_observations) =
            crate::orchestration::gateway_model_request::summary_executor(
                &self.runtime, &self.run, &self.prepared.ctx, &self.prepared.connection,
                self.prepared.model_name.as_str(), self.prepared.context_window,
            );

        // Append changed controls once; stable controls stay put. Measure the 90% trigger on real input.
        // 变化控制只追加一次，相同提示不移动或重复；90% 压缩阈值按实际输入计量。
        let live_notices = [
            ("documentProgressNotice", document_progress_notice),
            ("loadedToolNotice", loaded_tool_notice),
            ("deliveryBudgetNotice", delivery_notice),
            ("strategyProgressNotice", strategy_notice),
            ("completionReviewNotice", review_notice),
        ];
        for (marker, notice) in &live_notices {
            append_control_notice(&mut self.prepared.state, marker, notice.as_deref());
        }
        let mut conversational_input_budget = input_budget;
        if let Some(snapshot) = self.prepared.state.context.runtime_snapshot.clone() {
            let projected_tokens = crate::models::protocol::tool_message_tokens(&self.prepared.state.context.history, &self.prepared.system)
                .saturating_add(crate::models::protocol::estimate_tokens(&json!(schemas).to_string()));
            if projected_tokens > conversational_input_budget {
                // Only required compaction may move current facts; reserve their tokens first.
                // 仅必要压缩可移动当前事实，先预留其 token 额度。
                crate::models::runtime_context::detach_snapshot(&mut self.prepared.state.context.history, &mut self.prepared.state.context.history_sources,
                    &snapshot["source"]["snapshotHash"]);
                let snapshot_tokens = crate::models::protocol::tool_message_tokens(&[snapshot["message"].clone()], "");
                conversational_input_budget = conversational_input_budget.saturating_sub(snapshot_tokens);
            }
        }
        let remaining_model_calls = self.run
            .journal
            .as_ref()
            .map(|journal| {
                journal.load_current().map(|task| {
                    task.budget
                        .max_model_requests
                        .saturating_sub(task.usage.model_requests)
                })
            })
            .transpose()?;
        let compact = crate::models::gateway_context::compact_request_with_executor(
            &self.prepared.connection,
            self.prepared.model_name.as_str(),
            &self.prepared.state.context.history,
            &self.prepared.system,
            &json!({
                "inputBudgetTokens": conversational_input_budget,
                "contextWindowTokens": self.prepared.context_window,
                "providerMaxInputTokens": self.rounds.provider_input_limit,
                "providerMaxOutputTokens": self.prepared.max_output,
                "remainingModelCalls": remaining_model_calls,
                "declarations": schemas,
                "wireRequestPrefix": crate::models::gateway_context::summary_prefix(&self.prepared.connection, self.prepared.model_name.as_str(),
                    &self.prepared.state.context.history, &self.prepared.system, &schemas, &self.prepared.state.cache.tool_series)?,
                "conversationId": self.run.conversation_id,
                "dataRoot": self.runtime.config.data_root,
                "requestId": self.run.request_id,
                "historySources": self.prepared.state.context.history_sources,
                "summarySources": self.prepared.state.context.summary_sources,
                "previousSummary": self.prepared.state.cache.previous_summary,
                "availableTools": self.prepared.tool_names,
                "resultSources": self.prepared.state.context.result_sources
            }),
            self.prepared.cancellation.clone(),
            Some(compact_sender),
            Some(summary_executor),
        )
        .await;
        for duration_ms in summary_observations.take_durations_ms().await {
            let _ = self.prepared.state.progress.apply(
                "progress.recordModel",
                &json!({"round":round,"durationMs":duration_ms}),
            );
        }
        compact_events.finish(&mut self.prepared.state.delivery, &mut self.prepared.state.assistant).await;
        let compact = match compact {
            Ok(compact) => compact,
            Err(error) => {
                if ["CONTEXT_IRREDUCIBLE_INPUT", "CONTEXT_INPUT_TOO_LARGE", "TOOL_CONTEXT_BUDGET_EXCEEDED"].contains(&error.code.as_str())
                    && !declarations_disabled && trim_optional_tool(&mut declared_tools)
                {
                    self.prepared.state.cache.tool_series.reset();
                    self.prepared.state.tools.selected = declared_tools;
                    return Ok(PhaseOutcome::Continue);
                }
                // On compaction failure, retain history/effects for a persisted no-tool progress answer.
                // 整理失败时保留历史和副作用，通过持久化链路交付无工具进展答复。
                self.rounds.stopped = Some(error.into());
                return Ok(PhaseOutcome::Stop);
            }
        };
        let budget_exceeded = compact.budget_exceeded;
        apply_compacted_context(&mut self.prepared.state, &mut self.prepared.system, compact, &live_notices);
        self.prepared.state.refresh_runtime_snapshot(&runtime_authority(&self.prepared.ctx, permission, &self.prepared.live_memory.block), self.prepared.runtime_role, protocol::protocol(&self.prepared.connection) == "anthropic-messages");
        // Restored controls include framing absent from the early estimate; fit the complete request before dropping tools.
        // 恢复后的控制提示包含早期估算未计入的包装；先按完整请求重新分配输出，再考虑裁剪工具。
        let projected_input_tokens = crate::models::protocol::tool_message_tokens(
            &self.prepared.state.context.history, &self.prepared.system)
            .saturating_add(crate::models::protocol::estimate_tokens(&json!(schemas).to_string()));
        if budget_exceeded || projected_input_tokens > input_budget {
            let tool_phase = !declarations_disabled
                && !self.prepared.state.delivery.segments.last().is_some_and(|segment| segment["isAnswerContinuation"] == true)
                && (!self.prepared.hints.is_empty() || self.prepared.state.transcript.rounds.last().is_some_and(|turn|
                    turn["calls"].as_array().is_some_and(|calls| !calls.is_empty())));
            let mut output_args = json!({"contextWindowTokens":self.prepared.context_window,
                "requiredInputTokens":projected_input_tokens,"phase":if tool_phase {"tool"} else {"answer"}});
            if let Some(limit) = self.prepared.max_output {
                output_args["providerMaxOutputTokens"] = json!(limit);
            }
            if let Some(limit) = self.rounds.provider_input_limit {
                output_args["providerMaxInputTokens"] = json!(limit);
            }
            if let Ok(reallocated) = crate::models::output::calculate("context.outputBudget", &output_args) {
                let available_input = reallocated["inputBudgetTokens"].as_u64().unwrap_or(0)
                    .min(self.rounds.projected_input_limit.unwrap_or(u64::MAX));
                if projected_input_tokens <= available_input {
                    output = reallocated;
                    input_budget = available_input;
                }
            }
        }
        let summarized_work_changed = publish_compacted_work_summary(
            &mut self.prepared.state.documents, &self.prepared.state.context.history_sources,
            self.run.journal.as_deref(),
        ).await?;
        if projected_input_tokens > input_budget {
            // Keep verified context and trim optional schemas; avoid executor reload or repeated work.
            // 保留已核验上下文，先裁可选声明，避免重载执行器或重复操作。
            if !declarations_disabled && trim_optional_tool(&mut declared_tools) {
                self.prepared.state.cache.tool_series.reset();
                self.prepared.state.tools.selected = declared_tools;
                return Ok(PhaseOutcome::Continue);
            }
            self.rounds.stopped = Some(GatewayError::new(
                "CONTEXT_IRREDUCIBLE_INPUT",
                "当前必要输入仍超过模型窗口，已保留完整原文与回查入口。",
                409,
            ));
            return Ok(PhaseOutcome::Stop);
        }
        // Recheck timeline capacity after compaction without losing its usage or results.
        // 压缩后重核时间线容量，保留压缩用量和成果。
        if self.prepared.state.needs_message_continuation(round) {
            self.rounds.stopped = Some(GatewayError::new("TOOL_RUN_MESSAGE_CAPACITY",
                "回复消息已到容量边界，原任务保留上下文与已完成操作自动续接。", 409));
            return Ok(PhaseOutcome::Stop);
        }
        // Recheck memory revocation after compaction waits, before dispatch.
        // 压缩等待结束后、派发前重核记忆撤回。
        if let Some(status) = self.prepared.live_memory.refresh(&self.runtime.config.data_root, &self.run.conversation_id, &self.run.request_id, &mut self.prepared.system).await? {
            memory_refresh_notice(&mut self.prepared.state, &status);
            self.rounds.review_pending = false;
            return Ok(PhaseOutcome::Continue);
        }
        // Checkpoint the next step and receipts before admission waits; waiting is not terminal.
        // 准入等待前保存下一步和回执，等待不作为任务终态。
        checkpoint_completion_review(&mut self.prepared.state, self.rounds.review_pending, self.rounds.review_evidence_required, self.rounds.review_attempts);
        self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
        Ok(PhaseOutcome::Ready(PreparedRound { output, input_budget, schemas, declared_tools, tools_closed, summarized_work_changed }))
    }
}

/// The compaction owns its sender; drain accepted events before applying its context projection.
/// 发送端由压缩操作持有；应用上下文投影前先收齐已接收事件。
struct CompactionEvents {
    relay: tokio::task::JoinHandle<(u64, Vec<Value>)>,
}

impl CompactionEvents {
    fn start(run: &Arc<RequestRun>, initial_order: u64, activity_round: u64) -> (mpsc::Sender<Value>, Self) {
        let (sender, mut receiver) = mpsc::channel::<Value>(16);
        let run = run.clone();
        let relay = tokio::spawn(async move {
            let mut order = initial_order;
            let mut activities: HashMap<String, Value> = HashMap::new();
            while let Some(mut event) = receiver.recv().await {
                let id = event["tool"]["toolCallId"].as_str().unwrap_or("").to_owned();
                if !activities.contains_key(&id) {
                    order += 1;
                }
                let assigned = activities.get(&id)
                    .map(|item| item["order"].clone()).unwrap_or(json!(order));
                event["tool"]["round"] = json!(activity_round);
                event["tool"]["order"] = assigned;
                event["type"] = json!(if event["tool"]["status"] == "running" {
                    "tool_call"
                } else {
                    "tool_result"
                });
                if event["tool"]["name"] == "context.compact" && event["tool"]["status"] == "completed" {
                    if let Some(journal) = &run.journal {
                        let _ = journal.repository().record_usage(
                            journal.token(), &format!("compaction:{id}"),
                            &crate::storage::task_repository::TaskUsage {
                                compactions: 1, ..Default::default()
                            },
                        );
                    }
                }
                activities.insert(id, event["tool"].clone());
                run.emit(event).await;
            }
            let mut activities = activities.into_values().collect::<Vec<_>>();
            activities.sort_by_key(|item| item["order"].as_u64());
            (order, activities)
        });
        (sender, Self { relay })
    }

    async fn finish(self, delivery: &mut DeliveryState, assistant: &mut Value) {
        if let Ok((order, activities)) = self.relay.await {
            delivery.timeline_order = order;
            assistant["ToolActivities"].as_array_mut().unwrap().extend(activities);
        }
    }
}

/// Apply only the accepted in-memory projection; storage and post-wait checks stay with the caller.
/// 这里只应用已接受的内存投影；存储和等待后的核对仍由调用方协调。
fn apply_compacted_context(
    state: &mut LoopState,
    system: &mut String,
    compact: crate::models::gateway_context::CompactRequest,
    live_notices: &[(&str, Option<String>)],
) {
    if let Some(summary) = compact.summary_update {
        state.cache.previous_summary = summary;
    }
    state.context.history = compact.messages;
    state.context.history_sources = compact.history_sources;
    *system = compact.system;
    for (marker, notice) in live_notices {
        // Restore missing current notes only; unchanged controls retain their position.
        // 仅补回缺失的当前提示，相同控制不移动。
        append_control_notice(state, marker, notice.as_deref());
    }
}

async fn publish_compacted_work_summary(
    documents: &mut DocumentWorkRecords,
    history_sources: &[Value],
    journal: Option<&crate::orchestration::gateway_task_journal::TaskJournal>,
) -> Result<bool, GatewayError> {
    let Some(journal) = journal else { return Ok(false); };
    let accepted = history_sources.iter().rev().find_map(|source| {
        source["checkpointSemanticSummary"].is_object()
            .then(|| source["checkpointSemanticSummary"].clone())
    });
    let Some(summary) = accepted else { return Ok(false); };
    let updated = documents.update_from_summary(journal, &summary).await?;
    Ok(updated["informationAdded"] == true)
}
