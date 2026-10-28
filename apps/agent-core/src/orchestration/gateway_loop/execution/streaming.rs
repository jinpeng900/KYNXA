//! Stream one provider request, then recover only the corresponding failed request.
//! 流式接收一次供应商请求，仅按该次失败恢复，不重放成功工具操作。

use super::*;

impl LoopState {
    /// Close published phases before reporting a pre-dispatch failure; keep their text and identities intact.
    /// 派发前失败也须结束已发布分段，保留原文与身份，不能留下“正在生成”挡住后续结果。
    pub(in crate::orchestration::gateway_loop) async fn interrupt_open_segments(&mut self, run: &RequestRun) {
        for segment in &mut self.delivery.segments {
            if segment["status"] == "streaming" {
                segment["status"] = json!("interrupted");
                run.emit(json!({"type":"assistant_segment","segment":segment,"toolStreamProtocol":3})).await;
            }
        }
    }

    /// Publish the terminal phase without rewriting a closed segment's body or identity.
    /// 发布最终阶段，不改已关闭分段的正文或身份，保证终态与实际事件一致。
    pub(in crate::orchestration::gateway_loop) async fn publish_final_segment(&mut self, run: &RequestRun) {
        if self.delivery.segments.last().is_none_or(|segment| segment["status"] == "interrupted") {
            let round = self.delivery.segments.last().and_then(|segment| segment["round"].as_u64())
                .unwrap_or(0).saturating_add(1);
            let id = new_id();
            self.begin_segment(round, &id);
            self.append("", "", round, &id);
        }
        if let Some(last) = self.delivery.segments.last_mut() {
            last["phase"] = json!("final_answer");
            last["status"] = json!("completed");
            last["isAnswerContinuation"] = json!(false);
            let segment = last.clone();
            run.emit(json!({"type":"assistant_segment","segment":segment,"toolStreamProtocol":3})).await;
        }
    }

    /// Publish only a settled public draft; review text never enters this buffer.
    /// 只发布已进入收尾的公开草稿；验收内部文字不进入这个缓冲。
    pub(in crate::orchestration::gateway_loop) async fn publish_buffered_delivery(
        &mut self,
        run: &RequestRun,
        round: u64,
        segment: &str,
    ) {
        if self.delivery.pending_delivery.is_empty() { return; }
        let delivery = std::mem::take(&mut self.delivery.pending_delivery);
        if let Some(checkpoint) = self.completion.review_checkpoint.as_object_mut() {
            checkpoint.remove("pendingDelivery");
        }
        self.delivery.delivery_continuing = false;
        let phase = self.begin_segment(round, segment);
        run.emit(json!({"type":"assistant_segment","segment":phase,"toolStreamProtocol":3})).await;
        run.emit(json!({"type":"text_delta","delta":delivery,"segmentId":segment})).await;
        self.append(&delivery, "", round, segment);
        if let Some(last) = self.delivery.segments.last_mut() {
            last["phase"] = json!("final_answer");
            last["isAnswerContinuation"] = json!(false);
        }
        run.emit(json!({"type":"assistant_segment","segment":self.segment(segment),"toolStreamProtocol":3})).await;
    }
}

impl Execution {
    pub(super) async fn request_round(&mut self, round: u64, prepared: PreparedRound) -> Result<PhaseOutcome<ModelRound>, GatewayError> {
        let PreparedRound { output, input_budget, schemas, declared_tools, tools_closed, summarized_work_changed } = prepared;
        let segment = new_id();
        let current = Arc::new(Mutex::new((String::new(), String::new())));
        // Create visible segments only for settled buffered progress or final answers.
        // 缓冲进度或最终答复可发布时才建可见分段，避免空生成段。
        let buffered_delivery = requires_task_verification(&self.prepared.state);
        if !self.rounds.review_pending && !buffered_delivery {
            let phase = self.prepared.state.begin_segment(round, &segment);
            self.run.emit(json!({"type":"assistant_segment","segment":phase,"toolStreamProtocol":3}))
                .await;
        }
        let reviewing = self.rounds.review_pending;
        let (sender, relay) = start_relay(&self.run, current.clone(), segment.clone(), reviewing, buffered_delivery);
        let mut body = match protocol::build_request(
            &self.prepared.connection,
            self.prepared.model_name.as_str(),
            &self.prepared.state.context.history,
            &self.prepared.system,
            if reviewing {
                // Scale review output with checklist size within the negotiated budget; avoid a fixed 512-token cut.
                // 核验输出随清单增大且不超协商额度，不用固定 512 token 截断。
                Some(budget::review_output_limit(self.prepared.state.completion.acceptance_requirements.len(), self.rounds.review_attempts,
                    output["maxOutputTokens"].as_u64().unwrap_or(self.prepared.context_window / 2)))
            } else {
                output["maxOutputTokens"].as_u64()
            },
            &schemas,
            true,
        ) {
            Ok(body) => body,
            Err(error) => {
                self.rounds.stopped = Some(error.into());
                return Ok(PhaseOutcome::Stop);
            }
        };
        let tool_update = crate::models::tool_updates::project(&self.prepared.connection, &mut body, &mut self.prepared.state.cache.tool_series);
        let mut cache_policy = protocol::apply_prompt_cache(&self.prepared.connection, self.prepared.model_name.as_str(), &mut body, &self.prepared.cache_scope, &self.prepared.cache_stable_system);
        if !tool_update.is_null() { cache_policy["toolUpdates"] = tool_update; }
        let request_fingerprint = crate::retrieval::gateway_index::hash(&body.to_string());
        if self.rounds.rejected_request.as_ref() == Some(&request_fingerprint) {
            self.rounds.stopped = Some(GatewayError::new(
                "MODEL_CONTEXT_LIMIT_REJECTED",
                "重建请求仍未减少必要输入，已保留原文与回执。",
                409,
            ));
            return Ok(PhaseOutcome::Stop);
        }
        let prefix_observation = self.prepared.state.cache.prefix_diagnostics.prepare(&self.prepared.connection, self.prepared.model_name.as_str(), &self.prepared.protocol_name, &body, &self.prepared.cache_scope);
        cache_policy["prefix"] = prefix_observation.report();
        let visual_hashes = crate::orchestration::task::model_visual_hashes(&body);
        let visual_observation_in_request = visual_hashes.iter().any(|hash| self.prepared.state.visual.pending_visual_hashes.contains(hash));
        let ModelRequestOutcome { result, dispatched, duration_ms } = request_model(
            &self.runtime,
            &self.run,
            &self.prepared.ctx,
            &self.prepared.connection,
            self.prepared.model_name.as_str(),
            self.prepared.context_window,
            body,
            &self.prepared.state.tools.selected,
            self.prepared.cancellation.clone(),
            Some(sender.clone()),
            Some(cache_policy),
            std::mem::take(&mut self.rounds.retry_next_request),
        )
        .await;
        if dispatched { self.prepared.state.cache.prefix_diagnostics.commit(prefix_observation); }
        if dispatched && result.is_ok() && !visual_hashes.is_empty() {
            // Only bytes present in a successful model request count as visual observation.
            // 仅成功模型请求中实际包含的图像字节计入视觉观察。
            mark_visual_proofs(&mut self.prepared.state.completion.retained_completion_observations, &visual_hashes);
            for round in &mut self.prepared.state.transcript.rounds {
                if let Some(observations) = round["observations"].as_array_mut() {
                    mark_visual_proofs(observations, &visual_hashes);
                }
            }
        }
        if dispatched && result.is_ok() && visual_observation_in_request && self.prepared.state.visual.pending_visual_readback {
            self.prepared.state.visual.visual_readback_delivered = true;
            self.prepared.state.visual.pending_visual_readback = false;
            self.prepared.state.visual.pending_visual_hashes.clear();
        }
        // Count failed provider requests too, matching actual transmissions.
        // 失败请求同样计量，与真实发送次数一致。
        if dispatched {
            let _ = self.prepared.state.progress.apply(
                "progress.recordModel",
                &json!({"round":round,"durationMs":duration_ms}),
            );
        }
        drop(sender);
        let _ = relay.await;
        let partial = current.lock().await.clone();
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                if !reviewing && (dispatched || !partial.0.is_empty() || !partial.1.is_empty()
                    || self.prepared.state.delivery.segments.iter().any(|item|
                        item["id"] == segment && item["status"] == "streaming")) {
                    self.prepared.state.append(&partial.0, &partial.1, round, &segment);
                    if let Some(last) = self.prepared.state.delivery.segments.iter_mut().find(|item| item["id"] == segment)
                    {
                        last["status"] = json!("interrupted");
                    }
                    self.run.emit(json!({"type":"assistant_segment","segment":self.prepared.state.segment(&segment),"toolStreamProtocol":3})).await;
                }
                if error.code == "TASK_USER_STEERED" && !*self.prepared.cancellation.borrow() {
                    if !reviewing && !partial.0.is_empty() {
                        self.prepared.state.context.history.push(json!({"role":"assistant","content":partial.0}));
                    }
                    apply_user_steering(&self.runtime, &self.run, &mut self.prepared.state, &mut self.prepared.ctx).await?;
                    self.rounds.review_pending = false;
                    self.rounds.retry_next_request = false;
                    let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
                    return Ok(PhaseOutcome::Continue);
                }
                if !self.rounds.image_fallback && crate::models::message_attachments::image_rejection(error.status_code, &error.message, &error.details)
                    && crate::models::message_attachments::remove_images(&mut self.prepared.state.context.history)
                {
                    self.rounds.image_fallback = true;
                    self.prepared.state.broker_note("The configured model rejected image input. Continue using extracted document text; use attachment.read for image OCR. Do not claim to have inspected image pixels.");
                    return Ok(PhaseOutcome::Continue);
                }
                if error.details["contextRejection"].is_object() {
                    let rejection = &error.details["contextRejection"];
                    let plan = native(crate::models::recovery::calculate(
                        "models.recovery.contextPlan",
                        &json!({"errorCode":error.code,"isRequestFactory":true,"canRebuildContext":true,
                            "recoveryAttempts":self.rounds.context_recoveries,"rejection":rejection,
                            "current":{"contextWindowTokens":self.prepared.context_window,"maxOutputTokens":output["maxOutputTokens"],"providerMaxInputTokens":self.rounds.provider_input_limit,"inputBudgetTokens":input_budget},
                            "cacheKey":request_fingerprint,"cacheKeys":[],"cacheEntry":{},"now":chrono::Utc::now().timestamp_millis()}),
                    ))?;
                    if !plan.is_null() {
                        self.run.retain_model_limits(self.prepared.state.origin["connectionFingerprint"].as_str().unwrap_or(""), &plan["limits"]);
                        self.rounds.context_recoveries = plan["recoveryAttempts"]
                            .as_u64()
                            .unwrap_or(self.rounds.context_recoveries + 1);
                        self.prepared.context_window = plan["limits"]["contextWindowTokens"]
                            .as_u64()
                            .unwrap_or(self.prepared.context_window);
                        if let Some(actual) = plan["limits"]["providerMaxOutputTokens"].as_u64() {
                            self.prepared.max_output = Some(self.prepared.max_output.unwrap_or(actual).min(actual));
                        }
                        if let Some(actual) = plan["limits"]["providerMaxInputTokens"].as_u64() {
                            self.rounds.provider_input_limit = Some(actual);
                        }
                        if let Some(estimated) = plan["limits"]["estimatedInputBudgetTokens"].as_u64() {
                            self.rounds.projected_input_limit = Some(self.rounds.projected_input_limit.unwrap_or(estimated).min(estimated));
                        }
                        self.rounds.rejected_request = Some(request_fingerprint);
                        self.rounds.retry_next_request = true;
                        let _ = self.prepared.state.tools.effects
                            .apply("effects.protect", &json!({"includeCurrent":true}));
                        return Ok(PhaseOutcome::Continue);
                    }
                }
                if self.rounds.grammar_recoveries < 2 && error.details["toolGrammarRejection"].is_object() {
                    self.rounds.retry_next_request = true;
                    self.rounds.grammar_recoveries += 1;
                    if self.rounds.grammar_recoveries == 2 {
                        while trim_optional_tool(&mut self.prepared.state.tools.selected) {}
                    }
                    // Fix rejected declarations before retry; execution still validates the original schema.
                    // 重试前修正被拒声明，执行时仍校验原 schema。
                    let _ = self.prepared.state.tools.effects
                        .apply("effects.protect", &json!({"includeCurrent":true}));
                    self.prepared.state.broker_note("Broker: this provider rejected its tool grammar before dispatch. Declarations were simplified; use discovery to load only the next necessary capability. Prior successful effects remain valid.");
                    return Ok(PhaseOutcome::Continue);
                }
                if budget::resume_execution_after_truncated_review(
                    &mut self.rounds.review_pending, &error.code, &error.details,
                ) {
                    self.rounds.retry_next_request = true;
                    self.prepared.state.completion.acceptance_reviewed = false;
                    self.prepared.state.completion.acceptance_review_revision = None;
                    checkpoint_completion_review(&mut self.prepared.state, false,
                        self.rounds.review_evidence_required, self.rounds.review_attempts);
                    append_control_notice(&mut self.prepared.state, "completionReviewNotice", None);
                    let _ = self.prepared.state.task.apply("task.resumed", &json!({}));
                    let _ = self.prepared.state.tools.effects
                        .apply("effects.protect", &json!({"includeCurrent":true}));
                    self.prepared.state.broker_note(budget::TRUNCATED_REVIEW_EXECUTION_NOTICE);
                    self.prepared.state.save(&self.runtime, &self.run, "streaming", None).await?;
                    return Ok(PhaseOutcome::Continue);
                }
                if self.prepared.state.recoveries < 2
                    && [
                        "MODEL_TOOL_CALL_INVALID",
                        "MODEL_TOOL_ARGUMENT_INVALID",
                        "MODEL_TOOL_IDENTITY_INVALID",
                        "MODEL_TOOL_OUTPUT_TRUNCATED",
                        "MODEL_RESPONSE_INCOMPLETE",
                        "INVALID_TOOL_CALL_INDEX",
                        "MODEL_STREAM_INCOMPLETE",
                    ]
                    .contains(&error.code.as_str())
                {
                    self.prepared.state.recoveries += 1;
                    self.rounds.retry_next_request = true;
                    let _ = self.prepared.state.tools.effects
                        .apply("effects.protect", &json!({"includeCurrent":true}));
                    self.prepared.state.broker_note(&format!("Broker: previous step was not complete ({}). No incomplete call was dispatched. Continue the original task with complete valid declared calls. Completed effects remain valid and must not be repeated.",error.code));
                    return Ok(PhaseOutcome::Continue);
                }
                self.rounds.stopped = Some(error.into());
                return Ok(PhaseOutcome::Stop);
            }
        };
        Ok(PhaseOutcome::Ready(ModelRound { response, segment, reviewing, buffered_delivery,
            schemas, declared_tools, tools_closed, summarized_work_changed }))
    }
}

fn start_relay(
    run: &Arc<RequestRun>,
    current: Arc<Mutex<(String, String)>>,
    segment: String,
    reviewing: bool,
    buffered_delivery: bool,
) -> (mpsc::Sender<Value>, tokio::task::JoinHandle<()>) {
        let (sender, mut receiver) = mpsc::channel::<Value>(64);
        let relay_run = run.clone();
        let relay_current = current.clone();
        let relay_segment = segment.clone();
        let relay = tokio::spawn(async move {
            let mut projection = TaskTextProjection::default();
            // Filter echoed internal history before sending the first desktop delta.
            // 首个增量发送前过滤复述的内部历史。
            let mut history_filter = crate::orchestration::task::HistoryEchoFilter::default();
            let mut ready_event = None;
            while let Some(mut event) = match ready_event.take() {
                Some(event) => Some(event),
                None => receiver.recv().await,
            } {
                // Merge ready adjacent deltas without timer delays or reordering other events.
                // 仅合并已就绪的相邻增量，不加定时等待，不重排其他事件。
                if matches!(event["type"].as_str(), Some("text_delta" | "reasoning_delta")) {
                    let mut identity = event.clone();
                    identity.as_object_mut().unwrap().remove("delta");
                    let mut delta = event["delta"].as_str().unwrap_or("").to_owned();
                    while let Ok(next) = receiver.try_recv() {
                        let mut next_identity = next.clone();
                        if let Some(fields) = next_identity.as_object_mut() { fields.remove("delta"); }
                        let next_delta = next["delta"].as_str();
                        if identity != next_identity || next_delta.is_none()
                            || delta.len().saturating_add(next_delta.unwrap_or("").len()) > 16 * 1024
                        {
                            ready_event = Some(next);
                            break;
                        }
                        delta.push_str(next_delta.unwrap());
                    }
                    event["delta"] = json!(delta);
                }
                if event["type"] == "content_snapshot" {
                    projection = TaskTextProjection::default();
                    history_filter = crate::orchestration::task::HistoryEchoFilter::default();
                    let mut content = history_filter.push(&projection.push(event["content"].as_str().unwrap_or("")));
                    content.push_str(&history_filter.push(&projection.finish()));
                    content.push_str(&history_filter.finish());
                    let reasoning = event["reasoning"].as_str().unwrap_or("").to_owned();
                    *relay_current.lock().await = (content.clone(), reasoning.clone());
                    if !reviewing && !buffered_delivery {
                        if !content.is_empty() {
                            relay_run.emit(json!({"type":"text_delta","delta":content,"segmentId":relay_segment})).await;
                        }
                        if !reasoning.is_empty() {
                            relay_run.emit(json!({"type":"reasoning_delta","delta":reasoning,"segmentId":relay_segment})).await;
                        }
                    }
                    continue;
                }
                if event["type"] == "text_delta" {
                    let public = history_filter.push(&projection.push(event["delta"].as_str().unwrap_or("")));
                    if public.is_empty() {
                        continue;
                    }
                    event["delta"] = json!(public);
                }
                {
                    let mut text = relay_current.lock().await;
                    match event["type"].as_str() {
                        Some("text_delta") => {
                            text.0.push_str(event["delta"].as_str().unwrap_or(""))
                        }
                        Some("reasoning_delta") => {
                            text.1.push_str(event["delta"].as_str().unwrap_or(""))
                        }
                        _ => {}
                    }
                }
                if !reviewing && !buffered_delivery {
                    event["segmentId"] = json!(relay_segment);
                    relay_run.emit(event).await;
                }
            }
            let mut tail = history_filter.push(&projection.finish());
            tail.push_str(&history_filter.finish());
            if !tail.is_empty() {
                relay_current.lock().await.0.push_str(&tail);
                if !reviewing && !buffered_delivery {
                    relay_run
                        .emit(json!({"type":"text_delta","delta":tail,"segmentId":relay_segment}))
                        .await;
                }
            }
        });
    (sender, relay)
}
