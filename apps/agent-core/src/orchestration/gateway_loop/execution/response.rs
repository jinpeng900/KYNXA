//! Interpret model replies, verify completion and hand authorized calls to tool dispatch.
//! 解释模型回复、核验完成状态，再把已授权调用交给既有工具派发链。

use super::*;

impl Execution {
    pub(super) async fn inspect_response(&mut self, round: u64, model: ModelRound) -> Result<PhaseOutcome<ObservedRound>, GatewayError> {
        let ModelRound { response, segment, mut reviewing, buffered_delivery,
            schemas, declared_tools, tools_closed, summarized_work_changed } = model;
        // A review may discover work; native calls retain the ordinary authorization and receipt path.
        // 核验发现缺口时，正式调用仍走原授权与回执链，不当作无效核验丢弃。
        if reviewing && !response.calls.is_empty() {
            reviewing = false;
            self.rounds.review_pending = false;
            self.prepared.state.completion.acceptance_reviewed = false;
            self.prepared.state.completion.acceptance_review_revision = None;
            self.prepared.state.delivery.pending_delivery.clear();
            self.prepared.state.delivery.delivery_continuing = false;
            checkpoint_completion_review(&mut self.prepared.state, false, self.rounds.review_evidence_required, self.rounds.review_attempts);
            if !buffered_delivery {
                let phase = self.prepared.state.begin_segment(round, &segment);
                self.run.emit(json!({"type":"assistant_segment","segment":phase,"toolStreamProtocol":3})).await;
            }
        }
        // Successful steps reset recovery allowance for later failures.
        // 成功步骤重置恢复次数，供后续故障继续使用。
        self.rounds.context_recoveries = 0;
        self.prepared.state.generated += response.usage["completion_tokens"]
            .as_u64()
            .or_else(|| response.usage["output_tokens"].as_u64())
            .unwrap_or_else(|| crate::models::protocol::estimate_tokens(&response.content));
        // A reply formed before new steering cannot acknowledge or execute that unseen instruction.
        // 新引导到达前生成的回复，不能确认该引导或继续派发旧计划。
        let steering_arrived = steering_pending(&self.run, &self.prepared.ctx)
            && apply_user_steering(&self.runtime, &self.run, &mut self.prepared.state, &mut self.prepared.ctx).await? > 0;
        let inspected = native(
            self.prepared.state
                .task
                .apply("task.inspect", &json!({"text":response.content})),
        )?;
        self.rounds.task_status = inspected.clone();
        let prior_directive = self.prepared.state.assistant["UserTaskDirective"].clone();
        if !steering_arrived {
            apply_steering_revision(&mut self.prepared.state, &inspected, &mut self.rounds.minimum_active_ms);
        }
        if prior_directive != self.prepared.state.assistant["UserTaskDirective"] {
            if let Some(journal) = &self.run.journal {
                journal.emit_control("task.user-directive", &self.prepared.state.assistant["UserTaskDirective"])?;
            }
        }
        if self.prepared.state.assistant["UserTaskDirective"]["stop"] == true {
            // The user withdrew further work; acknowledge without dispatching proposed new calls.
            // 用户已撤销后续工作，确认停止，不再派发本轮提出的新调用。
            let acknowledgement = inspected["content"].as_str().filter(|text| !text.trim().is_empty())
                .unwrap_or("已停止，已有进度和成功回执已保留。");
            self.prepared.state.append(acknowledgement, "", round, &segment);
            self.run.emit(json!({"type":"assistant_segment","segment":self.prepared.state.segment(&segment),"toolStreamProtocol":3})).await;
            self.prepared.state.context.history.push(json!({"role":"assistant","content":acknowledgement}));
            self.prepared.state.push_transcript_round(json!({"round":round,"text":acknowledgement,"calls":[],
                "userStopped":true,"unexecutedCallCount":response.calls.len()}));
            self.rounds.task_status = json!({"state":"blocked","source":"user-steering","reason":"user-requested-stop"});
            // A user stop is a settled response, not an exhausted round that should recover.
            // 用户停止已完成本次答复，不应误报轮次耗尽再恢复执行。
            self.rounds.final_received = true;
            return Ok(PhaseOutcome::Stop);
        }
        if unresolved_user_steering(&self.prepared.state) {
            let work_plan = observe_work_progress(&mut self.prepared.state.work_progress, &mut self.prepared.state.progress,
                &RoundProgress { execution_without_action:true, ..Default::default() });
            // Pair skipped proposals for protocol continuity; they are not execution receipts.
            // 跳过的提议仍配对协议结果，但不能记为执行回执。
            let skipped = response.calls.iter().map(|call| json!({"call":call,"result":{
                "status":"error","executed":false,"isError":true,"code":"TASK_PLAN_UPDATED",
                "content":{"notice":"Not dispatched: resolve the latest verified user instruction before continuing the old plan."}
            }})).collect::<Vec<_>>();
            self.prepared.state.context.history = protocol::append_results(&self.prepared.protocol_name, &self.prepared.state.context.history, &response, &skipped)?;
            self.prepared.state.push_transcript_round(json!({"round":round,"text":inspected["content"],"calls":[],
                "unexecutedCallCount":response.calls.len(),"awaitingUserDirective":true}));
            self.prepared.state.append("", "", round, &segment);
            self.run.emit(json!({"type":"assistant_segment","segment":self.prepared.state.segment(&segment),"toolStreamProtocol":3})).await;
            self.rounds.review_pending = true;
            let notice = completion_review_instruction(&self.prepared.state);
            append_control_notice(&mut self.prepared.state, "completionReviewNotice", Some(&notice));
            self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
            if let Some(error) = crate::orchestration::gateway_tool_execution::progress_interruption(&work_plan) {
                self.rounds.stopped = Some(error);
                return Ok(PhaseOutcome::Stop);
            }
            return Ok(PhaseOutcome::Continue);
        }
        let text = inspected["content"].as_str().unwrap_or(&response.content);
        // Remove echoed internal history from public text; it cannot settle the task.
        // 内部历史复述从公开正文移除，不能据此结算任务。
        let echo_stripped = (!reviewing && crate::orchestration::task::contains_history_record(text))
            .then(|| crate::orchestration::task::strip_history_records(text));
        let text: &str = echo_stripped.as_deref().unwrap_or(text);
        if echo_stripped.is_some() {
            self.rounds.task_status["content"] = json!(text);
        }
        if !reviewing {
            if buffered_delivery {
                if response.calls.is_empty() {
                    if self.prepared.state.delivery.delivery_continuing { self.prepared.state.delivery.pending_delivery.push_str(text); }
                    else { self.prepared.state.delivery.pending_delivery = text.to_owned(); }
                    self.prepared.state.delivery.delivery_continuing = response.output_truncated;
                    self.prepared.state.bind_pending_delivery(&inspected);
                } else {
                    self.prepared.state.delivery.delivery_continuing = false;
                    let brief = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
                    let brief = brief.chars().take(140).collect::<String>();
                    let active_ms = self.run.effective_active_ms();
                    if self.prepared.state.content().is_empty() || active_ms.saturating_sub(self.prepared.state.delivery.last_progress_ms) >= 60_000 {
                        self.prepared.state.append(&brief, "", round, &segment);
                        self.prepared.state.delivery.last_progress_ms = active_ms;
                    } else {
                        // Quiet tool rounds still need a completed same-round commentary anchor.
                        // 静默工具轮仍需同轮已完成的过程锚点。
                        self.prepared.state.append("", "", round, &segment);
                    }
                }
            } else { self.prepared.state.append(text, &response.reasoning, round, &segment); }
            let public_segment = self.prepared.state.segment(&segment);
            if !public_segment.is_null() {
                self.run.emit(json!({"type":"assistant_segment","segment":public_segment,"toolStreamProtocol":3})).await;
            }
            let _ = self.prepared.state.task.apply("task.observe", &json!({"content":text}));
        }
        let prefix_fingerprint = crate::retrieval::gateway_index::hash(
            &json!([self.prepared.system, schemas, self.prepared.state.context.history]).to_string(),
        );
        let archive_root = self.runtime.config.data_root.clone();
        let archive_chat = self.run.conversation_id.clone();
        let archive_request = self.run.request_id.clone();
        let private = json!({"content":[],"_meta":{"modelContinuation":{"version":1,"origin":self.prepared.state.origin,"prefixFingerprint":prefix_fingerprint,"round":round,"continuation":response.continuation}}});
        let continuation_ref = tokio::task::spawn_blocking(move || {
            data::save_tool_result(
                &archive_root,
                &archive_chat,
                &archive_request,
                &json!({"id":format!("model-continuation-{round}"),"name":"model.continuation"}),
                &private,
                None,
            )
        })
        .await
        .map_err(|_| GatewayError::new("DATA_WORKER_FAILED", "模型续接回执保存失败。", 500))??;
        // Retain streamed text, but block stale-memory calls from effects and delivery.
        // 保留流式正文，失效记忆相关调用不得派发副作用或交付。
        if let Some(status) = self.prepared.live_memory.refresh(&self.runtime.config.data_root, &self.run.conversation_id, &self.run.request_id, &mut self.prepared.system).await? {
            self.prepared.state.push_transcript_round(json!({"round":round,"text":text,"calls":[],
                "nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint,
                "discardedToolCalls":"memory-source-changed-before-dispatch"}));
            memory_refresh_notice(&mut self.prepared.state, &status);
            self.rounds.review_pending = false;
            self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
            return Ok(PhaseOutcome::Continue);
        }
        let scope = self.prepared.state.assistant["CurrentWorkScope"].clone();
        if response.calls.iter().any(|call| !crate::orchestration::task::scope_allows_call(&scope,
            call["name"].as_str().unwrap_or(""), crate::tools::policy::is_read_only_call(
                call["name"].as_str().unwrap_or(""), &call["arguments"]))) {
            let skipped = response.calls.iter().map(|call| json!({"call":call,"result":{
                "status":"error","executed":false,"isError":true,"code":"TASK_SCOPE_NOT_AUTHORIZED",
                "content":{"notice":"Not dispatched: this turn permits only the requested plan, explanation or read-only investigation. Answer within that scope; a future deliverable is not current execution authorization."}
            }})).collect::<Vec<_>>();
            self.prepared.state.context.history = protocol::append_results(&self.prepared.protocol_name,
                &self.prepared.state.context.history, &response, &skipped)?;
            self.prepared.state.push_transcript_round(json!({"round":round,"text":text,"calls":[],
                "nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint,
                "undispatchedForWorkScope":true}));
            self.prepared.state.broker_note(&crate::orchestration::task::work_scope_instruction(&scope));
            self.rounds.review_pending = false;
            self.prepared.state.save(&self.runtime,&self.run,"streaming",None).await?;
            return Ok(PhaseOutcome::Continue);
        }
        if !crate::orchestration::task::response_only_scope(&scope)
            && response.calls.is_empty() && inspected["source"] == "unexecuted-tool-intent" {
            let requested = crate::orchestration::task::unexecuted_tool_names(&response.content);
            if retain_unexecuted_tool_intent(&mut self.prepared.state, &requested, &self.prepared.all, round) {
                self.prepared.state.push_transcript_round(json!({"round":round,"text":text,"calls":[],
                    "nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint,
                    "unexecutedToolIntent":self.prepared.state.completion.review_checkpoint["unexecutedToolIntent"]}));
                self.prepared.state.context.history = protocol::append_results(&self.prepared.protocol_name, &self.prepared.state.context.history, &response, &[])?;
                self.rounds.review_pending = false;
                let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
                self.prepared.state.broker_note("Broker: the preceding tool syntax was plain text and nothing ran. Assess the current requirements from saved receipts. Use native tools for a specific missing action; if current acceptance is already met, deliver the answer without executing the proposal. Preserve context, outputs and successful effects.");
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Continue);
            }
        }
        if echo_stripped.is_some() && response.calls.is_empty() {
            self.prepared.state.push_transcript_round(json!({"round":round,"text":text,"calls":[],
                "nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint}));
            if self.rounds.history_echo_recoveries < 2 {
                self.rounds.history_echo_recoveries += 1;
                // Exclude echoes from provider history; retain originals in formal storage.
                // 复述内容不进供应商历史，原输出仍留正式存储。
                self.prepared.state.broker_note("Broker: the previous output repeated an internal historical tool-call record. Those calls ran earlier and were NOT executed now, and the record is not an answer. Discover and load the tools you need, check the current state, and perform the next real step of the original task, or answer truthfully in plain text.");
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                self.rounds.retry_next_request = true;
                return Ok(PhaseOutcome::Continue);
            }
            self.rounds.stopped = Some(GatewayError::new("MODEL_HISTORY_ECHO",
                "模型反复复述内部历史调用记录，没有执行新的操作；已有内容和回执保留。", 502));
            return Ok(PhaseOutcome::Stop);
        }
        let has_replanned_action = self.prepared.state.work_progress.needs_strategy_change()
            && self.rounds.task_status["action"]["arguments"].is_object()
            && self.prepared.state.tools.selected.iter().any(|tool|tool["name"]==self.rounds.task_status["action"]["name"])
            && cached_completion_review(&self.prepared.state).is_some_and(|review|review["action"]=="resume");
        if !reviewing && !has_replanned_action && missing_public_answer(&response.content, text, !response.calls.is_empty()) {
            // Control markers alone are not answers; retain receipts and request text once.
            // 仅有控制标记不算回答，保留回执并补一次正文请求。
            self.prepared.state.push_transcript_round(json!({"round":round,"text":"","calls":[],
                "nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint}));
            if self.rounds.empty_control_recoveries == 0 {
                self.rounds.empty_control_recoveries += 1;
                self.prepared.state.context.history.extend(response.continuation.clone());
                self.prepared.state.broker_note("The previous output contained only an internal status marker and no user-visible answer. Answer the user's actual request now in ordinary visible text. Do not output task markers, status JSON, or repeat external actions.");
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                self.rounds.retry_next_request = true;
                return Ok(PhaseOutcome::Continue);
            }
            self.rounds.stopped = Some(GatewayError::new("MODEL_RESPONSE_EMPTY", "模型未返回可见正文，请重试。", 502));
            return Ok(PhaseOutcome::Stop);
        }
        if !crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"])
            && response.calls.is_empty() && let Some(journal) = &self.run.journal {
            let children = self.runtime.tasks.children_acceptance(journal.task_id()).await?;
            if children["allAccepted"] == false && children["unaccepted"].as_array().is_some_and(|items|
                items.iter().any(|item| matches!(item["state"].as_str(), Some("completed" | "partial")))) {
                self.rounds.review_pending = false;
                self.prepared.state.completion.acceptance_reviewed = false;
                self.prepared.state.broker_note(&format!("Broker: delegated outputs require your parent review before final delivery. Read the actual artifacts, then task.result with review accept or return tied to expectedVersion/outputRef. A child's completion is not the parent's acceptance. {}", children));
                if let Some(tool) = self.prepared.all.iter().find(|tool| tool["name"] == "task.result") {
                    if !self.prepared.state.tools.selected.iter().any(|loaded| loaded["name"] == "task.result") { self.prepared.state.tools.selected.push(tool.clone()); }
                }
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Continue);
            }
        }
        // Recheck evidence versions before review or inline completion settles the task.
        // 独立核验或内联完成结算前，均须重核证据版本。
        if !crate::orchestration::task::response_only_scope(&scope)
            && (reviewing || response.calls.is_empty() && self.rounds.task_status["state"] == "complete")
            && !self.prepared.state.progress.verification()["codeVersionCoverage"]["validatedSnapshot"].is_null() {
            let snapshot = tools::capture_code_version(&self.prepared.ctx).await?;
            native(self.prepared.state.progress.apply("progress.finalVersion", &json!({"snapshot":snapshot})))?;
        }
        Ok(PhaseOutcome::Ready(ObservedRound {
            model: ModelRound { response, segment, reviewing, buffered_delivery, schemas, declared_tools,
                tools_closed, summarized_work_changed },
            inspected, echo_stripped, prefix_fingerprint, continuation_ref,
        }))
    }

    pub(super) async fn review_response(&mut self, round: u64, observed: ObservedRound) -> Result<PhaseOutcome<()>, GatewayError> {
        let ObservedRound { model, inspected, prefix_fingerprint, continuation_ref, .. } = observed;
        let response = model.response;
            if apply_user_steering(&self.runtime, &self.run, &mut self.prepared.state, &mut self.prepared.ctx).await? > 0 {
                self.rounds.review_pending = false;
                let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Continue);
            }
            let _ = self.prepared.state
                .task
                .apply("task.reviewResult", &json!({"value":inspected}));
            self.prepared.state.completion.acceptance_review_revision = Some(acceptance_revision(&self.prepared.state));
            let review = completion_review(&self.prepared.state);
            let decision = self.prepared.state.task.decide_typed(&review.decision_input(&self.prepared.state.completion.acceptance_requirements, TaskDecisionInput {
                reply: inspected.clone(), is_review: true, has_calls: !response.calls.is_empty(),
                response_only: crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"]),
                current_work_scope: self.prepared.state.assistant["CurrentWorkScope"].clone(),
                output_truncated: response.output_truncated, review_attempts: self.rounds.review_attempts,
                remaining_rounds: Some(self.rounds.last_round.saturating_sub(round)), minimum_active_ms: self.rounds.minimum_active_ms,
                active_elapsed_ms: effective_work_ms(&self.run, self.prepared.state.started), evidence_required: self.rounds.review_evidence_required,
                requires_verification: requires_task_verification(&self.prepared.state), ..Default::default()
            })).into_value();
            self.prepared.state.push_transcript_round(json!({"round":round,"text":"","calls":[],"nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint}));
            if decision["action"] == "retry-review" {
                let repair = crate::orchestration::task::review_format_repair(
                    &mut self.prepared.state.completion.review_checkpoint, &response.content,
                    response.output_truncated, response.usage["completion_tokens"].as_u64()
                        .or_else(||response.usage["output_tokens"].as_u64()).unwrap_or(0));
                if repair["action"] == "wait" {
                    self.rounds.stopped = Some(GatewayError::new("TASK_COMPLETION_REVIEW_INVALID",
                        repair["reason"].as_str().unwrap_or("验收格式仍未改善，原任务与回执已保存。"), 409));
                    self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                    return Ok(PhaseOutcome::Stop);
                }
            } else if let Some(fields) = self.prepared.state.completion.review_checkpoint.as_object_mut() {
                fields.remove("reviewFormatRepair");
            }
            if response.calls.is_empty()
                && let Some(error) = self.continuation_interruption(&decision, response.output_truncated) {
                self.rounds.stopped = Some(error);
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Stop);
            }
            match decision["action"].as_str() {
                Some("retry-review") => {
                    self.rounds.review_attempts += 1;
                    let review_instruction = completion_review_instruction(&self.prepared.state);
                    append_control_notice(&mut self.prepared.state, "completionReviewNotice", Some(&review_instruction));
                    self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                    return Ok(PhaseOutcome::Continue);
                }
                Some("resume") => {
                    self.rounds.review_pending = false;
                    checkpoint_completion_review(&mut self.prepared.state, false, self.rounds.review_evidence_required, self.rounds.review_attempts);
                    let revision = acceptance_revision(&self.prepared.state);
                    cache_completion_review(&mut self.prepared.state.completion.review_checkpoint, &revision, &decision, &inspected);
                    self.rounds.output_continuations += 1;
                    let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
                    if decision["minimumActive"] == true {
                        self.prepared.state.broker_note(&format!("Broker: the user required at least {} minutes of effective work; about {} minutes are done. Keep refining and verifying the original task with real actions now; do not idle or pad time.",
                            self.rounds.minimum_active_ms / 60000, effective_work_ms(&self.run, self.prepared.state.started) / 60000));
                    } else {
                        let remaining = decision["remaining"].as_array().filter(|items| !items.is_empty())
                            .map(|items| json!(items).to_string()).unwrap_or_else(|| inspected["next"].to_string());
                    let notice = format!("Broker: remaining current requirements: {remaining}. If they remain feasible and authorized, take the specific missing action or use existing evidence. Do not turn a plan, optional improvement or already satisfied check into new work. If the target is demonstrably impossible or a necessary capability is unavailable after reasonable alternatives, report the supported limitation with feasibility basis and alternatives instead of repeating attempts. Preserve successful effects.");
                        append_control_notice(&mut self.prepared.state, "completionExecutionNotice", Some(&notice));
                    }
                    self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                    return Ok(PhaseOutcome::Continue);
                }
                Some("final") => {
                    self.prepared.state.completion.acceptance_reviewed = true;
                    let config = self.runtime.config.clone();
                    let context =
                        json!({"conversationId":self.run.conversation_id,"requestId":self.run.request_id,
                            "bindingSnapshot":self.prepared.ctx.config["bindingSnapshot"],"workspaceMounts":self.prepared.ctx.config["workspaceMounts"]});
                    let current = if crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"]) {
                        json!({"current":true,"state":"not-required","reason":"current-turn-response-only"})
                    } else { tokio::task::spawn_blocking(move || {
                        crate::retrieval::gateway::final_validate(&config, &context)
                    })
                    .await
                    .map_err(|_| {
                        GatewayError::new("EVIDENCE_WORKER_FAILED", "证据核对退出。", 500)
                    })?? };
                    if current["current"] == false && self.rounds.freshness_recoveries < 2 {
                        self.rounds.freshness_recoveries += 1;
                        self.rounds.review_pending = false;
                        self.prepared.state.completion.review_checkpoint = Value::Null;
                        self.prepared.state.completion.acceptance_review_revision = None;
                        self.prepared.state.completion.acceptance_reviewed = false;
                        self.prepared.state.broker_note(&format!("Broker: sources changed; revise the conclusion after re-reading these sources: {}",current["invalidSources"]));
                        return Ok(PhaseOutcome::Continue);
                    }
                    if current["current"] == false {
                        self.rounds.stopped = Some(GatewayError::new(
                            "EVIDENCE_SOURCE_CHANGED",
                            "部分来源持续变化，结论尚未完成版本核验。",
                            409,
                        ));
                        return Ok(PhaseOutcome::Stop);
                    }
                    self.rounds.final_evidence_validation = Some(current);
                    self.prepared.state.publish_buffered_delivery(&self.run, round, &new_id()).await;
                    if let Some(last) = self.prepared.state.delivery.segments.last_mut() {
                        last["phase"] = json!("final_answer");
                        last["isAnswerContinuation"] = json!(false);
                    }
                    self.rounds.final_received = true;
                    return Ok(PhaseOutcome::Stop);
                }
                _ => {
                    self.rounds.stopped = Some(GatewayError::new(
                        decision["code"].as_str().unwrap_or("TASK_BLOCKED"),
                        inspected["reason"]
                            .as_str()
                            .filter(|reason| !reason.is_empty())
                            .unwrap_or("任务完成核对还有未解决事项。"),
                        409,
                    ));
                    return Ok(PhaseOutcome::Stop);
                }
            }
    }

    pub(super) async fn plan_continuation(&mut self, round: u64, observed: &mut ObservedRound) -> Result<Value, GatewayError> {
        let response = &mut observed.model.response;
        let segment = &observed.model.segment;
        let tools_closed = observed.model.tools_closed;
        // Fill deterministic read gaps through normal dispatch, not another model review.
        // 可确定的读取缺口走原派发链，不再调用模型验收。
        let mut broker_planned_call = Value::Null;
        if response.calls.is_empty() && !response.output_truncated && requires_task_verification(&self.prepared.state)
            && !unresolved_user_steering(&self.prepared.state)
            && self.prepared.supports_tools && !tools_closed && planned_continuation(&self.prepared.protocol_name, &[], &[]).is_some() {
            if let Some(plan) = completion_progress_plan(&mut self.prepared.state, &self.rounds.task_status["action"])
                && crate::orchestration::task::scope_allows_call(&self.prepared.state.assistant["CurrentWorkScope"],
                    plan["name"].as_str().unwrap_or(""), crate::tools::policy::is_read_only_call(
                        plan["name"].as_str().unwrap_or(""), &plan["arguments"])) {
                let call = json!({"id":new_id(),"name":plan["name"],"arguments":plan["arguments"],
                    "unavailable":!self.prepared.state.tools.selected.iter().any(|tool|tool["name"]==plan["name"])});
                response.calls = vec![call];
                response.continuation = planned_continuation(&self.prepared.protocol_name, &response.calls, &response.continuation).unwrap_or_default();
                broker_planned_call = plan;
                self.rounds.review_pending = false;
                self.prepared.state.append("", "", round, &segment);
                self.run.emit(json!({"type":"assistant_segment","segment":self.prepared.state.segment(&segment),"toolStreamProtocol":3})).await;
            }
        }
        Ok(broker_planned_call)
    }

    pub(super) async fn complete_response(&mut self, round: u64, observed: ObservedRound) -> Result<PhaseOutcome<()>, GatewayError> {
        let ObservedRound { model, inspected, echo_stripped, prefix_fingerprint, continuation_ref } = observed;
        let ModelRound { response, segment, buffered_delivery, .. } = model;
        let text = inspected["content"].as_str().unwrap_or(&response.content);
        let text = echo_stripped.as_deref().unwrap_or(text);
            // Apply streaming-time corrections in the next model turn before completion.
            // 流式期间收到的调整须先交下一轮模型，再判定完成。
            self.prepared.state.context.history =
                protocol::append_results(&self.prepared.protocol_name, &self.prepared.state.context.history, &response, &[])?;
            if apply_user_steering(&self.runtime, &self.run, &mut self.prepared.state, &mut self.prepared.ctx).await? > 0 {
                self.prepared.state.push_transcript_round(json!({"round":round,"text":text,"calls":[],"nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint}));
                self.rounds.review_pending = false;
                let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Continue);
            }
            if !crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"])
                && let Some(journal) = &self.run.journal {
                let children = journal.repository().children(journal.task_id())?;
                let pending: Vec<Value> = children
                    .iter()
                    .filter(|task| !task.state.is_terminal())
                    .map(|task| json!(task.task_id))
                    .collect();
                if !pending.is_empty() {
                    let child_results = self.runtime
                        .tasks
                        .tool(
                            self.run.clone(),
                            self.prepared.ctx.clone(),
                            "task.wait",
                            json!({"taskIds":pending,"timeoutMs":30000}),
                        )
                        .await?;
                    // Wait locally for changes without model polling.
                    // 本地等待条件变化，不用模型调用轮询。
                    let mut child_results = child_results;
                    while child_results["timedOut"] == true && !*self.prepared.cancellation.borrow() {
                        child_results = self.runtime
                            .tasks
                            .tool(
                                self.run.clone(),
                                self.prepared.ctx.clone(),
                                "task.wait",
                                json!({"taskIds":pending,"timeoutMs":30000}),
                            )
                            .await?;
                    }
                    self.prepared.state.broker_note(&format!("Broker: delegated task results, use actual artifacts and verification; failed/partial children do not certify completion: {child_results}"));
                    self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                    return Ok(PhaseOutcome::Continue);
                }
            }
            self.prepared.state.push_transcript_round(json!({"round":round,"text":text,"calls":[],"nativeContinuationRef":continuation_ref,"prefixFingerprint":prefix_fingerprint}));
            let mut decision = self.prepared.state.task.decide_typed(&TaskDecisionInput {
                reply: self.rounds.task_status.clone(), has_calls: false, output_truncated: response.output_truncated,
                output_continuations: self.rounds.output_continuations, supports_task_markers: self.prepared.supports_tools,
                steered_task: !self.prepared.state.applied_steering_ids.is_empty(), effectful_work: current_effectful_work(&self.prepared.state),
                execution_intent: crate::orchestration::task::continuation_needs_progress(
                    &self.prepared.state.assistant["CurrentWorkScope"], !self.prepared.state.context.result_sources.is_empty(),
                    requires_task_verification(&self.prepared.state)),
                response_only: crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"]),
                current_work_scope: self.prepared.state.assistant["CurrentWorkScope"].clone(),
                requires_verification: requires_task_verification(&self.prepared.state),
                remaining_rounds: Some(self.rounds.last_round.saturating_sub(round)), ..Default::default()
            }).into_value();
            if matches!(decision["action"].as_str(), Some("review" | "resume"))
                && cached_completion_review(&self.prepared.state).is_some_and(|cached| cached["action"] == "resume" && cached["minimumActive"] != true) {
                // Check inline proof fixes locally; stale reviews cannot schedule another review.
                // 本地核对内联证据修正，旧验收不再触发新验收。
                let review = completion_review(&self.prepared.state);
                let inline = self.prepared.state.task.decide_typed(&review.decision_input(&self.prepared.state.completion.acceptance_requirements, TaskDecisionInput {
                    reply: self.rounds.task_status.clone(), is_review: true,
                    response_only: crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"]),
                    current_work_scope: self.prepared.state.assistant["CurrentWorkScope"].clone(),
                    requires_verification: requires_task_verification(&self.prepared.state), evidence_required: true,
                    ..Default::default()
                })).into_value();
                // Corrected references can shrink gaps without changing any execution receipt.
                // 修正引用即可缩小缺口，无需为了更新核验缓存而重做操作。
                let revision = acceptance_revision(&self.prepared.state);
                cache_completion_review(&mut self.prepared.state.completion.review_checkpoint, &revision, &inline, &self.rounds.task_status);
                match inline["action"].as_str() {
                    Some("final") => {
                        self.prepared.state.completion.acceptance_reviewed = true;
                        let _ = self.prepared.state.task.apply("task.reviewResult", &json!({"value":self.rounds.task_status}));
                        decision = inline;
                    }
                    Some("retry-review") => {
                        self.rounds.review_pending = true;
                        self.rounds.review_evidence_required = true;
                        self.rounds.review_attempts += 1;
                        self.prepared.state.completion.acceptance_reviewed = false;
                        checkpoint_completion_review(&mut self.prepared.state, true, self.rounds.review_evidence_required, self.rounds.review_attempts);
                        append_control_notice(&mut self.prepared.state, "completionExecutionNotice", None);
                        let instruction = completion_review_instruction(&self.prepared.state);
                        append_control_notice(&mut self.prepared.state, "completionReviewNotice", Some(&instruction));
                        self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                        return Ok(PhaseOutcome::Continue);
                    }
                    Some("resume") => {
                        let work_plan = observe_work_progress(&mut self.prepared.state.work_progress, &mut self.prepared.state.progress,
                            &RoundProgress { execution_without_action:true, ..Default::default() });
                        let remaining = &inline["remaining"];
                        let notice = format!("Broker: unchanged evidence still leaves these requirements: {remaining}. Perform the next actual tool action or change the implementation strategy. Do not narrate another assessment. Reuse saved outputs and receipts; a genuine complete inline marker must map each acceptance ID to actual evidence. If native tool dispatch is not being emitted, return a standalone private <kynxa_task>{{\"state\":\"continue\",\"next\":\"concrete step\",\"action\":{{\"name\":\"a currently loaded tool name\",\"arguments\":{{}}}}}}</kynxa_task> with the actual tool arguments. The broker will validate and execute this action through the normal authorization and receipt pipeline; do not replay successful or unknown effects.");
                        append_control_notice(&mut self.prepared.state,"completionExecutionNotice",Some(&notice));
                        self.rounds.review_pending = false;
                        self.prepared.state.save(&self.runtime,&self.run,"streaming",None).await?;
                        if let Some(error) = crate::orchestration::gateway_tool_execution::progress_interruption(&work_plan) {
                            self.rounds.stopped = Some(error);
                            return Ok(PhaseOutcome::Stop);
                        }
                        return Ok(PhaseOutcome::Continue);
                    }
                    _ => decision = inline,
                }
            }
            if let Some(error) = self.continuation_interruption(&decision, response.output_truncated) {
                self.rounds.stopped = Some(error);
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Stop);
            }
            if decision["action"] == "resume" {
                if self.prepared.state.generated.saturating_sub(self.rounds.segment_initial_generated) >= self.prepared.limits["maxGeneratedTokens"].as_u64().unwrap_or(1048576) {
                    self.rounds.stopped = Some(GatewayError::new(
                        "TOOL_RUN_OUTPUT_LIMIT",
                        "本次生成额度已用完，已完成内容保留。",
                        409,
                    ));
                    return Ok(PhaseOutcome::Stop);
                }
                let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
                self.rounds.output_continuations += 1;
                if response.output_truncated {
                    // Only provider-confirmed output-limit prose is answer continuation, not tool commentary or progress.
                    // 仅供应商确认达到输出上限的正文标为续写，工具说明和进度不算。
                    if let Some(continued_segment) = self.prepared.state.delivery.segments.last_mut().filter(|item| item["id"] == segment) {
                        continued_segment["isAnswerContinuation"] = json!(true);
                    }
                }
                self.prepared.state.broker_note("Continue only the current authorized request from saved progress. Preserve settled effects and answer directly when its result is established. Future suggestions and background goals are not new work. A specific supported impossibility or missing capability should be reported honestly rather than repeatedly attempted; ordinary implementation difficulty still calls for a reasonable alternative.");
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Continue);
            }
            if decision["action"] == "review" {
                self.rounds.review_pending = true;
                self.rounds.review_attempts = 0;
                // Require evidence for effect-triggered reviews only; keep steering/continuation rules unchanged.
                // 仅外部操作触发的核验要求证据，插话和续写沿用原规则。
                self.rounds.review_evidence_required = current_effectful_work(&self.prepared.state) || requires_task_verification(&self.prepared.state);
                let _ = self.prepared.state.task.apply("task.reviewStarted", &json!({}));
                let review_instruction = completion_review_instruction(&self.prepared.state);
                append_control_notice(&mut self.prepared.state, "completionReviewNotice", Some(&review_instruction));
                self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                return Ok(PhaseOutcome::Continue);
            }
            if decision["action"] == "interrupt" {
                self.rounds.stopped = Some(GatewayError::new(
                    decision["code"]
                        .as_str()
                        .unwrap_or("TASK_CONTINUATION_NO_PROGRESS"),
                    self.rounds.task_status["reason"].as_str()
                        .filter(|reason| !reason.trim().is_empty())
                        .unwrap_or("继续执行没有取得新的进展，已有结果保留。"),
                    409,
                ));
                return Ok(PhaseOutcome::Stop);
            }
            let config = self.runtime.config.clone();
            let context = json!({"conversationId":self.run.conversation_id,"requestId":self.run.request_id,
                "bindingSnapshot":self.prepared.ctx.config["bindingSnapshot"],"workspaceMounts":self.prepared.ctx.config["workspaceMounts"]});
            let freshness = if crate::orchestration::task::response_only_scope(&self.prepared.state.assistant["CurrentWorkScope"])
                || crate::orchestration::task::genuine_blocker(&self.rounds.task_status) {
                json!({"current":true,"state":"not-required","reason":"settled-current-response"})
            } else { tokio::task::spawn_blocking(move || {
                crate::retrieval::gateway::final_validate(&config, &context)
            })
            .await
            .map_err(|_| {
                GatewayError::new("EVIDENCE_WORKER_FAILED", "证据版本核对退出。", 500)
            })?? };
            if freshness["current"] == false {
                self.prepared.state.completion.review_checkpoint = Value::Null;
                self.prepared.state.completion.acceptance_review_revision = None;
                self.prepared.state.completion.acceptance_reviewed = false;
                if self.rounds.freshness_recoveries < 2 {
                    self.rounds.freshness_recoveries += 1;
                    self.prepared.state.broker_note(&format!("Broker: cited source versions changed or access was revoked before this answer. Re-read only these affected authorized sources, revise the conclusion, and do not repeat effects: {}",freshness["invalidSources"]));
                    self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                    return Ok(PhaseOutcome::Continue);
                }
                self.rounds.stopped = Some(GatewayError::new(
                    "EVIDENCE_SOURCE_CHANGED",
                    "部分来源持续变化，先前结论尚未完成版本核验。",
                    409,
                ));
                return Ok(PhaseOutcome::Stop);
            }
            self.rounds.final_evidence_validation = Some(freshness);
            if buffered_delivery {
                self.prepared.state.publish_buffered_delivery(&self.run, round, &segment).await;
            }
            if let Some(last) = self.prepared.state.delivery.segments.last_mut() {
                last["phase"] = json!("final_answer");
                last["isAnswerContinuation"] = json!(false);
            }
            self.rounds.final_received = true;
            return Ok(PhaseOutcome::Stop);
    }

    /// Continue only while evidence can advance; reuse checkpointed correction state for text-only loops.
    /// 仅在证据仍可推进时续作；纯文字空转复用检查点中的纠偏状态，不另设轮次上限。
    fn continuation_interruption(&mut self, decision: &Value, output_truncated: bool) -> Option<GatewayError> {
        if output_truncated || !crate::orchestration::task::continuation_needs_progress(
                &self.prepared.state.assistant["CurrentWorkScope"], !self.prepared.state.context.result_sources.is_empty(),
                requires_task_verification(&self.prepared.state))
            || !matches!(decision["action"].as_str(), Some("resume" | "review")) {
            return None;
        }
        let plan = observe_work_progress(&mut self.prepared.state.work_progress, &mut self.prepared.state.progress,
            &RoundProgress {execution_without_action:true, ..Default::default()});
        if plan.correction_due {
            append_control_notice(&mut self.prepared.state, "continuationProgressNotice", Some(
                "Broker: these text-only turns added no new evidence, artifact version or read range. Resolve the specific unmet requirement with an authorized native action or a genuinely different method. Reuse successful receipts. If a necessary condition is missing, report the explicit blocker and its basis; do not repeat the same conclusion or claim completion."));
        }
        crate::orchestration::gateway_tool_execution::progress_interruption(&plan)
    }

    pub(super) async fn dispatch_tools(&mut self, round: u64, observed: ObservedRound, broker_planned_call: Value) -> Result<PhaseOutcome<()>, GatewayError> {
        let ObservedRound { model, inspected, echo_stripped, prefix_fingerprint, continuation_ref } = observed;
        let ModelRound { mut response, declared_tools, summarized_work_changed, .. } = model;
        let text = inspected["content"].as_str().unwrap_or(&response.content);
        let text = echo_stripped.as_deref().unwrap_or(text);
        // Normalize known tool IDs before effect classification; keep loading under admission.
        // 先归一化已知工具身份再分类副作用，加载仍须准入。
        {
            let cached = self.prepared.ctx.runtime.schemas.read().await;
            for call in &mut response.calls {
                if call["unavailable"] != true {
                    continue;
                }
                let requested = call["name"].as_str().unwrap_or("");
                if let Some(tool) = self.prepared.all.iter().chain(cached.iter()).find(|tool| {
                    tools::executable_descriptor(tool)
                        && tool["name"].as_str().is_some_and(|name| {
                            name == requested || crate::models::protocol::wire_name(name) == requested
                        })
                }) {
                    call["name"] = tool["name"].clone();
                }
            }
        }
        // Persist the completed model step before any effect is dispatched.
        // 派发任何副作用前，先持久化已完成的模型步骤。
        self.prepared.state.push_transcript_round(json!({"round":round,"text":text,"calls":response.calls,"nativeContinuationRef":continuation_ref,
            "brokerPlannedCall":broker_planned_call,"prefixFingerprint":prefix_fingerprint}));
        self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
        let tool_outcome = tool_round::ToolRound {
            runtime: &self.runtime, run: &self.run, input: &self.input, state: &mut self.prepared.state, ctx: &mut self.prepared.ctx,
            all: &mut self.prepared.all, system: &mut self.prepared.system, live_memory: &mut self.prepared.live_memory,
            cancellation: &mut self.prepared.cancellation, protocol_name: &self.prepared.protocol_name, limits: &self.prepared.limits,
            response: &response, declared_tools: &declared_tools, round, segment_initial_calls: self.rounds.segment_initial_calls,
            segment_initial_active_ms: self.rounds.segment_initial_active_ms,
            summarized_work_changed,
        }.execute().await?;
        if tool_outcome.review_invalidated { self.rounds.review_pending = false; }
        self.rounds.stopped = tool_outcome.stopped;
        if self.rounds.stopped.is_some() { return Ok(PhaseOutcome::Stop); }
        Ok(PhaseOutcome::Continue)
    }
}
