//! 最终答复交付与正式任务结算。

use super::*;

const UNVERIFIED_REPLY_NOTICE: &str = "这次回答没有通过完成核验，可以让它继续。";

/// 收尾异常时说明已落地事实，文件保存不等于全部要求均已满足。
fn stopped_reply_notice(
    error: &GatewayError,
    review: &super::super::completion_evidence::CompletionEvidenceSet,
) -> String {
    let current = review
        .records
        .iter()
        .filter(|record| record["current"] == true && record["observationCurrent"] == true)
        .collect::<Vec<_>>();
    let has_output = current.iter().any(|record| {
        record["kinds"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|kind| kind == "committed-output")
    });
    let has_preview = current.iter().any(|record| {
        record["tool"] == "artifact.open"
            && record["kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|kind| kind == "observed-state")
    });
    let result = match (has_output, has_preview) {
        (true, true) => "文件已保存，文件预览也已打开。",
        (true, false) => "生成的文件已保存。",
        (false, true) => "文件预览已打开。",
        (false, false) => "",
    };
    let reason = if error.code == "MODEL_TOOL_OUTPUT_TRUNCATED" {
        // 截断的工具请求尚未执行；解码器错误不能作为正常答复交付。
        "这一步的操作内容未能完整生成，尚未执行；已有结果保留。"
    } else if error.code == "TASK_CONTINUATION_NO_PROGRESS" {
        if review.verification["state"] == "checks-failed" {
            "仍有检查未通过，尚未完成全部要求；已有结果和进度已保留。"
        } else if review.verification["pendingValidation"] == true {
            "必要检查尚未完成，已有结果和进度已保留。"
        } else {
            error.message.as_str()
        }
    } else {
        error.message.as_str()
    };
    format!("{result}{reason}")
}

/// 本轮已公开的普通说明可以正常交付，不因此认证未完成的任务。
fn current_public_reply(
    error: &GatewayError,
    assessment: &Value,
    delivery: &DeliveryState,
    rounds: &[Value],
) -> Option<usize> {
    if !delivery.pending_delivery.is_empty() || delivery.delivery_continuing {
        return None;
    }
    let is_public_reply = |segment: &Value, round: &Value| {
        segment["phase"] == "commentary"
            && segment["status"] == "completed"
            && segment["isAnswerContinuation"] != true
            && segment["round"].is_u64()
            && segment["round"] == round["round"]
            && segment["content"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty() && round["text"].as_str() == Some(text))
            && round["calls"].as_array().is_some_and(Vec::is_empty)
            && round
                .get("unexecutedCallCount")
                .is_none_or(|count| count.as_u64() == Some(0))
    };
    if error.code == "TASK_COMPLETION_REVIEW_INVALID" {
        // 内部核验可在公开回答之后继续；不能用核验文字或空段替换已公开的正常回答。
        delivery
            .segments
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, segment)| {
                let round = rounds
                    .iter()
                    .rev()
                    .find(|round| round["round"] == segment["round"])?;
                (is_public_reply(segment, round)
                    && round["reviewing"] == false
                    && round["outputTruncated"] == false
                    && round["taskState"] == "unknown"
                    && round["taskStateSource"] == "legacy-response")
                    .then_some(index)
            })
    } else if error.code == "TASK_CONTINUATION_NO_PROGRESS"
        && assessment["state"] == "unknown"
        && assessment["source"] == "legacy-response"
    {
        let index = delivery.segments.len().checked_sub(1)?;
        let (segment, round) = (&delivery.segments[index], rounds.last()?);
        (is_public_reply(segment, round)
            && segment["content"] == assessment["content"]
            && round["reviewing"] != true
            && round["outputTruncated"] != true)
            .then_some(index)
    } else {
        None
    }
}

/// 取最后一轮写好的公开回答；内部核验、工具轮次和已经显示过的内容不算。
pub(super) fn unpublished_answer(rounds: &[Value], shown: &str) -> Option<String> {
    let round = rounds.iter().rev().find(|round| {
        round["reviewing"] != true
            && round["calls"].as_array().is_none_or(Vec::is_empty)
            && round
                .get("unexecutedCallCount")
                .is_none_or(|count| count.as_u64() == Some(0))
            && round["text"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
    })?;
    let text = round["text"].as_str()?.trim();
    (!shown.contains(text)).then(|| text.to_owned())
}

pub(super) struct FinalDelivery<'a> {
    pub runtime: &'a Arc<GatewayRuntime>,
    pub run: &'a Arc<RequestRun>,
    pub state: LoopState,
    pub ctx: tools::GatewayToolContext,
    pub cancellation: tokio::sync::watch::Receiver<bool>,
    pub model_started: Instant,
    pub preparation_stages: Value,
    pub stopped: Option<GatewayError>,
    pub final_received: bool,
    pub final_evidence_validation: Option<Value>,
    pub task_status: Value,
    pub review_pending: bool,
    pub review_evidence_required: bool,
    pub review_attempts: u64,
}

impl FinalDelivery<'_> {
    pub(super) async fn finish(self) -> Result<Value, GatewayError> {
        let Self {
            runtime,
            run,
            mut state,
            mut ctx,
            cancellation,
            model_started,
            preparation_stages,
            mut stopped,
            final_received,
            final_evidence_validation,
            task_status,
            review_pending,
            review_evidence_required,
            review_attempts,
        } = self;
        if !final_received && stopped.is_none() {
            stopped = Some(GatewayError::new(
                "TOOL_RUN_ROUND_LIMIT",
                "本次任务轮次已用完，已完成内容和回执保留。",
                409,
            ));
        }
        checkpoint_completion_review(
            &mut state,
            review_pending,
            review_evidence_required,
            review_attempts,
        );
        if stopped.is_some() && !*cancellation.borrow() {
            apply_user_steering(&runtime, &run, &mut state, &mut ctx).await?;
        }
        let continuing = stopped
            .as_ref()
            .is_some_and(|error| internal_recovery_required(&error.code))
            && !*cancellation.borrow()
            && run.journal.is_some();
        if let Some(error) = stopped.clone().filter(|_| !continuing) {
            state.interrupt_open_segments(run).await;
            let public_reply = if !*cancellation.borrow()
                && (error.code == "TASK_COMPLETION_REVIEW_INVALID"
                    || !unresolved_user_steering(&state))
            {
                current_public_reply(
                    &error,
                    &task_status,
                    &state.delivery,
                    &state.transcript.rounds,
                )
            } else {
                None
            };
            let round = state
                .delivery
                .segments
                .last()
                .and_then(|segment| segment["round"].as_u64())
                .unwrap_or(0)
                .saturating_add(1)
                .min(MAX_MESSAGE_ROUNDS);
            // 缓冲已被清空时，最后写好但没有发布的回答仍要交给用户。
            if state.delivery.pending_delivery.trim().is_empty()
                && !*cancellation.borrow()
                && !matches!(
                    error.code.as_str(),
                    "ABORT_ERR" | "TASK_PAUSED" | "TASK_CANCELLED"
                )
                && public_reply.is_none()
                && let Some(answer) = unpublished_answer(&state.transcript.rounds, &state.content())
            {
                state.delivery.pending_delivery = answer;
            }
            if state.pending_delivery_matches_blocker(&task_status) {
                state.publish_buffered_delivery(run, round, &new_id()).await;
            } else if let Some(index) = public_reply {
                if index + 1 != state.delivery.segments.len() {
                    // 只重发已选中的公开原文；已关闭核验段的身份和正文保持原样。
                    let text = state.delivery.segments[index]["content"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    let id = new_id();
                    state.begin_segment(round, &id);
                    state.append(&text, "", round, &id);
                    run.emit(json!({"type":"assistant_segment","segment":state.segment(&id),"toolStreamProtocol":3})).await;
                }
            } else if !*cancellation.borrow()
                && !matches!(
                    error.code.as_str(),
                    "ABORT_ERR" | "TASK_PAUSED" | "TASK_CANCELLED"
                )
                && !state.delivery.pending_delivery.trim().is_empty()
                && super::super::task::unexecuted_tool_names(&state.delivery.pending_delivery)
                    .is_empty()
            {
                // 任务仍不算完成，但模型已写好的回答要交给用户。
                let id = new_id();
                state.publish_buffered_delivery(run, round, &id).await;
                let note = format!("\n\n{UNVERIFIED_REPLY_NOTICE}");
                run.emit(json!({"type":"text_delta","delta":note,"segmentId":id}))
                    .await;
                state.append(&note, "", round, &id);
                run.emit(json!({"type":"assistant_segment","segment":state.segment(&id),"toolStreamProtocol":3})).await;
            } else {
                // 已取消的任务不再发布内容；原草稿仍保留在此前检查点中。
                state.delivery.pending_delivery.clear();
                state.delivery.delivery_continuing = false;
                if let Some(checkpoint) = state.completion.review_checkpoint.as_object_mut() {
                    checkpoint.remove("pendingDelivery");
                }
                let notice = format!(
                    "{}{}",
                    if state.content().is_empty() {
                        ""
                    } else {
                        "\n\n"
                    },
                    stopped_reply_notice(&error, &completion_review(&state))
                );
                let id = new_id();
                state.begin_segment(round, &id);
                state.append(&notice, "", round, &id);
                if let Some(last) = state.delivery.segments.last_mut() {
                    last["phase"] = json!("final_answer");
                    last["isAnswerContinuation"] = json!(false);
                }
                run.emit(json!({"type":"assistant_segment","segment":state.segment(&id),"toolStreamProtocol":3})).await;
            }
        }
        let effects = native(state.tools.effects.apply("effects.state", &json!({})))?;
        let user_stopped = state.assistant["UserTaskDirective"]["stop"] == true;
        let work_suspended = *cancellation.borrow() || stopped.is_some() && !continuing;
        let response_only =
            super::super::task::response_only_scope(&state.assistant["CurrentWorkScope"]);
        let settled_limitation = super::super::task::genuine_blocker(&task_status);
        let final_version = if user_stopped
            || work_suspended
            || response_only
            || settled_limitation
            || final_received && state.completion.acceptance_reviewed
        {
            state.progress.verification()["codeVersionCoverage"]["latestSnapshot"].clone()
        } else {
            match tools::capture_code_version(&ctx).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    stopped.get_or_insert(error);
                    Value::Null
                }
            }
        };
        let _ = state
            .progress
            .apply("progress.finalVersion", &json!({"snapshot":final_version}));
        let validation_config = runtime.config.clone();
        let validation_context = json!({"conversationId":run.conversation_id,"requestId":run.request_id,
                "bindingSnapshot":ctx.config["bindingSnapshot"],"workspaceMounts":ctx.config["workspaceMounts"]});
        // 停止保留已有证据，不认证完成，也不启动新的验收检查。
        let evidence_validation = if work_suspended {
            json!({"state":"not-checked","reason":"execution-suspended-with-saved-progress"})
        } else if user_stopped || response_only || settled_limitation {
            json!({"state":"not-checked","reason":"current-response-does-not-certify-background-work"})
        } else if let Some(validation) = final_evidence_validation {
            validation
        } else {
            tokio::task::spawn_blocking(move || {
                crate::retrieval::gateway::final_validate(&validation_config, &validation_context)
            })
            .await
            .map_err(|_| GatewayError::new("EVIDENCE_WORKER_FAILED", "证据核对线程退出。", 500))??
        };
        let review = completion_review(&state);
        let evidence_records = &review.records;
        let current_status = if response_only && final_received && stopped.is_none() {
            json!({"state":"complete","source":"current-turn-response","unmet":[]})
        } else {
            task_status
        };
        let broker_final = final_received
            && stopped.is_none()
            && state.completion.acceptance_reviewed
            && !user_stopped
            && !settled_limitation;
        let validation_scope = review.validation_scope(&state.completion.acceptance_requirements);
        let mut completion=state.progress.complete_for_scope(&json!({"interrupted":stopped.is_some(),"code":stopped.as_ref().map(|error|&error.code),"modelTaskState":current_status,"evidenceValidation":evidence_validation,
            "brokerDecision":if broker_final {json!("final")} else {Value::Null},
            "effectfulWork":current_effectful_work(&state),"requiresVerification":requires_task_verification(&state),
            "acceptanceReviewed":state.completion.acceptance_reviewed,
            "requiresReadback":!response_only && state.completion.acceptance_requirements.iter().any(|goal|super::super::task::requires_result_readback(goal)),
            "hasReadback":super::super::completion_evidence::has_readback(&evidence_records, false),
            "requiresVisualReadback":!response_only && state.completion.acceptance_requirements.iter().any(|goal|super::super::task::requires_visual_observation(goal)),
            "hasVisualReadback":super::super::completion_evidence::has_readback(&evidence_records, true)}), &validation_scope);
        completion["workStage"] = state.assistant["CurrentWorkScope"]["stage"].clone();
        if response_only {
            completion["backgroundGoalCertified"] = json!(false);
        }
        if state.assistant["UserTaskDirective"]["stop"] == true {
            completion["completionState"] = json!("partial");
            completion["userStopped"] = json!(true);
            completion["userDirectedDelivery"] =
                json!(state.assistant["UserTaskDirective"]["deliverExisting"] == true);
        }
        let mut diagnostics = native(state.progress.apply("progress.diagnostics", &json!({})))?;
        // 保留模型计时，另记请求接受后的准备耗时，不含桌面保存等待。
        diagnostics["requestTiming"] = json!({
            "acceptedToModelMs": model_started.saturating_duration_since(run.created).as_millis() as u64,
            "modelLoopMs": model_started.elapsed().as_millis() as u64,
            "acceptedToFinalizationMs": run.created.elapsed().as_millis() as u64,
            "includesDesktopCatalogSave": false,
            "includesFinalPersistence": false,
            "preparationStages": preparation_stages,
        });
        state.assistant["ToolRun"] = json!({"version":1,"requestId":run.request_id,"diagnostics":diagnostics,"taskCompletion":completion,"effects":effects});
        state.assistant["DurationMs"] =
            json!(super::super::gateway_runtime::generation_elapsed_ms(
                state.delivery.generation_duration_base_ms,
                state.delivery.model_started,
            ));
        // 回复交付与任务成功分开，失败原因保留在 TaskCompletion。
        let reply_interrupted = super::super::gateway_runtime::reply_interrupted(
            stopped.as_ref().map_or("", |error| error.code.as_str()),
            *cancellation.borrow(),
        );
        if !continuing && !reply_interrupted {
            state.publish_final_segment(run).await;
        }
        if !continuing {
            // 答复进入正式保存，已非继续生成；成功终态仍须等待持久化结算。
            run.emit(json!({"type":"task_progress","phase":"saving","round":1}))
                .await;
        }
        state
            .save(
                &runtime,
                &run,
                if continuing {
                    "streaming"
                } else if reply_interrupted {
                    "interrupted"
                } else {
                    "completed"
                },
                stopped
                    .as_ref()
                    .filter(|_| reply_interrupted && !continuing),
            )
            .await?;
        if !continuing {
            if !reply_interrupted
                && stopped.is_none()
                && completion["completionState"] == "completed"
            {
                if state.completion.acceptance_reviewed {
                    if let Some(journal) = &run.journal {
                        let observations = completion_transcript(&state)
                            .iter()
                            .flat_map(|round| {
                                round["observations"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .cloned()
                            })
                            .collect::<Vec<_>>();
                        if let Some(candidate) =
                            crate::storage::experiences::verified_procedure_candidate(
                                &state.completion.acceptance_requirements.join("\n"),
                                &observations,
                            )
                        {
                            // 衍生候选仅作私有提示，不是产物、权限或成熟经验。
                            let _ = journal.emit_control("experience.candidate", &candidate);
                        }
                    }
                }
                crate::retrieval::gateway::enqueue_conversation_semantic(
                    &runtime.config,
                    &run.conversation_id,
                );
            }
            crate::retrieval::gateway::release_task(&run.conversation_id, &run.request_id);
        }
        Ok(
            json!({"type":if continuing{"recovering"}else if reply_interrupted{"interrupted"}else{"completed"},"replyMessageId":state.assistant["Id"],"content":state.assistant["Content"],"reasoning":state.assistant["Reasoning"],"assistantSegments":state.delivery.segments,"toolStreamProtocol":3,"durationMs":state.assistant["DurationMs"],"taskCompletion":completion,"error":stopped.as_ref().filter(|_|reply_interrupted && !continuing).map(|error|&error.message),"code":stopped.as_ref().map(|error|&error.code)}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::completion_evidence::CompletionEvidenceSet;

    #[test]
    fn stopped_tasks_recover_the_last_written_answer_only() {
        let rounds = vec![
            json!({"round":1,"text":"先检查","calls":[{"name":"terminal.host.run"}]}),
            json!({"round":2,"text":"完整回答","calls":[],"reviewing":false}),
            json!({"round":3,"text":"","calls":[]}),
            json!({"round":4,"text":"内部核验","calls":[],"reviewing":true}),
        ];
        assert_eq!(
            unpublished_answer(&rounds, "先检查").as_deref(),
            Some("完整回答")
        );
        assert_eq!(
            unpublished_answer(&rounds, "先检查 完整回答"),
            None,
            "already shown"
        );
        assert_eq!(
            unpublished_answer(&rounds[..1], ""),
            None,
            "a tool round is not an answer"
        );
        let unexecuted = [json!({"text":"调用文字","calls":[],"unexecutedCallCount":1})];
        assert_eq!(unpublished_answer(&unexecuted, ""), None);
    }

    fn published_explanation() -> (Value, DeliveryState, Vec<Value>) {
        let text = "画板绘制尚未完成。请松开按键，再告诉我，我会继续。";
        (
            json!({"state":"unknown","source":"legacy-response","content":text}),
            DeliveryState {
                segments: vec![json!({"id":"current-reply","round":28,"phase":"commentary",
                "status":"completed","content":text})],
                ..Default::default()
            },
            vec![
                json!({"round":28,"text":text,"calls":[],"reviewing":false,"outputTruncated":false,
                "taskState":"unknown","taskStateSource":"legacy-response"}),
            ],
        )
    }

    #[test]
    fn current_public_explanation_survives_stopped_work_without_certifying_completion() {
        for code in [
            "TASK_CONTINUATION_NO_PROGRESS",
            "TASK_COMPLETION_REVIEW_INVALID",
        ] {
            let error =
                GatewayError::new(code, "后续处理未能继续推进，已有结果和进度已保留。", 409);
            let (assessment, delivery, mut rounds) = published_explanation();
            assert_eq!(
                current_public_reply(&error, &assessment, &delivery, &rounds),
                Some(0)
            );
            rounds[0]["unexecutedCallCount"] = json!(0);
            assert_eq!(
                current_public_reply(&error, &assessment, &delivery, &rounds),
                Some(0)
            );
            let completion = RunProgress::default()
                .apply(
                    "progress.completion",
                    &json!({
                "interrupted":true,"code":code,"modelTaskState":assessment,
                "requiresVerification":true,"acceptanceReviewed":false}),
                )
                .unwrap();
            assert_eq!(completion["completionState"], "partial");
            assert_eq!(completion["acceptanceReviewed"], false);
            assert_eq!(completion["correctnessCertified"], false);
        }
    }

    #[test]
    fn stopped_work_does_not_promote_buffered_stale_tool_or_truncated_drafts() {
        for boundary in [
            "buffered",
            "continuing",
            "truncated",
            "interrupted",
            "stale",
            "tools",
            "complete",
            "review",
            "internal",
            "output-limit",
            "unexecuted",
            "declared-complete",
            "declared-source",
        ] {
            let (mut assessment, mut delivery, mut rounds) = published_explanation();
            match boundary {
                "buffered" => delivery.pending_delivery = "未经验收的旧完成草稿".into(),
                "continuing" => delivery.delivery_continuing = true,
                "truncated" => delivery.segments[0]["isAnswerContinuation"] = json!(true),
                "interrupted" => delivery.segments[0]["status"] = json!("interrupted"),
                "stale" => rounds[0]["round"] = json!(29),
                "tools" => rounds[0]["calls"] = json!([{"name":"computer.click"}]),
                "complete" => assessment["state"] = json!("complete"),
                "review" => assessment["source"] = json!("model-task-state"),
                "internal" => rounds[0]["reviewing"] = json!(true),
                "output-limit" => rounds[0]["outputTruncated"] = json!(true),
                "unexecuted" => rounds[0]["unexecutedCallCount"] = json!(1),
                "declared-complete" => rounds[0]["taskState"] = json!("complete"),
                "declared-source" => rounds[0]["taskStateSource"] = json!("model-task-state"),
                _ => unreachable!(),
            }
            let code = if matches!(boundary, "complete" | "review") {
                "TASK_CONTINUATION_NO_PROGRESS"
            } else {
                "TASK_COMPLETION_REVIEW_INVALID"
            };
            let error = GatewayError::new(code, "控制格式未能修复。", 409);
            assert_eq!(
                current_public_reply(&error, &assessment, &delivery, &rounds),
                None,
                "{boundary}"
            );
        }
        let (assessment, delivery, rounds) = published_explanation();
        let decoder_error =
            GatewayError::new("MODEL_TOOL_OUTPUT_TRUNCATED", "回复达到模型输出上限。", 502);
        assert_eq!(
            current_public_reply(&decoder_error, &assessment, &delivery, &rounds),
            None
        );
    }

    #[test]
    fn invalid_control_review_reuses_public_answer_before_an_empty_review_segment() {
        let error = GatewayError::new("TASK_COMPLETION_REVIEW_INVALID", "控制格式未能修复。", 409);
        let (mut assessment, mut delivery, mut rounds) = published_explanation();
        assessment["content"] =
            json!("Internal review control text must not replace the public answer.");
        delivery
            .segments
            .push(json!({"id":"empty-review","round":29,"phase":"commentary",
            "status":"completed","content":""}));
        rounds.push(json!({"round":29,"text":assessment["content"],"calls":[],
            "reviewing":true,"outputTruncated":false}));
        assert_eq!(
            current_public_reply(&error, &assessment, &delivery, &rounds),
            Some(0)
        );
        assert_eq!(
            delivery.segments[0]["content"],
            "画板绘制尚未完成。请松开按键，再告诉我，我会继续。"
        );
        delivery.segments[1]["content"] = assessment["content"].clone();
        assert_eq!(
            current_public_reply(&error, &assessment, &delivery, &rounds),
            Some(0)
        );
        let no_progress =
            GatewayError::new("TASK_CONTINUATION_NO_PROGRESS", "尚未取得新进展。", 409);
        assert_eq!(
            current_public_reply(&no_progress, &assessment, &delivery, &rounds),
            None
        );
    }

    #[test]
    fn stopped_reply_reports_current_outputs_without_certifying_completion() {
        let error = GatewayError::new(
            "TASK_CONTINUATION_NO_PROGRESS",
            "后续处理未能继续推进，已有结果和进度已保留。",
            409,
        );
        let mut review = CompletionEvidenceSet {
            verification: json!({}),
            valid_references: vec![],
            records: vec![
                json!({"current":true,"observationCurrent":true,"kinds":["execution","committed-output"]}),
                json!({"current":true,"observationCurrent":true,"tool":"artifact.open","kinds":["execution","observed-state"]}),
            ],
        };
        let notice = stopped_reply_notice(&error, &review);
        assert!(notice.starts_with("文件已保存，文件预览也已打开。"));
        assert!(
            !notice.contains("No new result")
                && !notice.contains("任务已完成")
                && !notice.contains("中间内容")
        );
        review.verification = json!({"state":"checks-failed","pendingValidation":true});
        assert!(stopped_reply_notice(&error, &review).contains("检查未通过"));
        review
            .records
            .iter_mut()
            .for_each(|record| record["current"] = json!(false));
        assert!(!stopped_reply_notice(&error, &review).contains("文件已保存"));
        assert!(!stopped_reply_notice(&error, &review).contains("预览已打开"));
        review.records = vec![json!({"current":true,"tool":"artifact.open","kinds":["execution"]})];
        assert!(!stopped_reply_notice(&error, &review).contains("预览已打开"));
    }

    #[test]
    fn unrecoverable_truncated_operation_reports_nonexecution_without_exposing_decoder_text() {
        let error = GatewayError::new(
            "MODEL_TOOL_OUTPUT_TRUNCATED",
            "回复达到模型输出上限，已保留生成的内容。 decoder-internal-detail",
            502,
        );
        let mut review = CompletionEvidenceSet {
            verification: json!({"state":"checks-failed","pendingValidation":true}),
            valid_references: vec![],
            records: vec![],
        };
        let notice = stopped_reply_notice(&error, &review);
        assert_eq!(
            notice,
            "这一步的操作内容未能完整生成，尚未执行；已有结果保留。"
        );
        assert!(
            !notice.contains("回复达到模型输出上限") && !notice.contains("decoder-internal-detail")
        );
        review.records.push(json!({"current":true,"observationCurrent":true,"kinds":["execution","committed-output"]}));
        let notice = stopped_reply_notice(&error, &review);
        assert!(notice.starts_with("生成的文件已保存。"));
        assert!(notice.contains("尚未执行"));
        assert!(!notice.contains("任务已完成") && !notice.contains("全部要求均已满足"));
        assert!(!notice.contains(error.message.as_str()));
    }

    #[test]
    fn stopped_reply_preserves_other_error_messages() {
        let error = GatewayError::new("TOOL_ACCESS_DENIED", "所选文件不可访问。", 403);
        let review = CompletionEvidenceSet {
            verification: json!({}),
            valid_references: vec![],
            records: vec![],
        };
        assert_eq!(stopped_reply_notice(&error, &review), error.message);
    }
}
