//! Final reply delivery and durable task settlement.
//! 最终答复交付与正式任务结算。

use super::*;

/// Report settled facts when delivery fails; a saved file does not certify every requirement.
/// 收尾异常时说明已落地事实，文件保存不等于全部要求均已满足。
fn stopped_reply_notice(error: &GatewayError, review: &super::super::completion_evidence::CompletionEvidenceSet) -> String {
    let current = review.records.iter().filter(|record| record["current"] == true
        && record["observationCurrent"] == true).collect::<Vec<_>>();
    let has_output = current.iter().any(|record| record["kinds"].as_array().into_iter().flatten()
        .any(|kind| kind == "committed-output"));
    let has_preview = current.iter().any(|record| record["tool"] == "artifact.open"
        && record["kinds"].as_array().into_iter().flatten()
            .any(|kind| kind == "observed-state"));
    let result = match (has_output, has_preview) {
        (true, true) => "文件已保存，文件预览也已打开。",
        (true, false) => "生成的文件已保存。",
        (false, true) => "文件预览已打开。",
        (false, false) => "",
    };
    let reason = if error.code == "TASK_CONTINUATION_NO_PROGRESS" {
        if review.verification["state"] == "checks-failed" {
            "仍有检查未通过，尚未完成全部要求；已有结果和进度已保留。"
        } else if review.verification["pendingValidation"] == true {
            "必要检查尚未完成，已有结果和进度已保留。"
        } else {
            error.message.as_str()
        }
    } else { error.message.as_str() };
    format!("{result}{reason}")
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
        let Self { runtime, run, mut state, mut ctx,
            cancellation, model_started,
            preparation_stages, mut stopped, final_received, final_evidence_validation, task_status,
            review_pending, review_evidence_required, review_attempts } = self;
        if !final_received && stopped.is_none() {
            stopped = Some(GatewayError::new(
                "TOOL_RUN_ROUND_LIMIT",
                "本次任务轮次已用完，已完成内容和回执保留。",
                409,
            ));
        }
        checkpoint_completion_review(&mut state, review_pending, review_evidence_required, review_attempts);
        if stopped.is_some() && !*cancellation.borrow() {
            apply_user_steering(&runtime, &run, &mut state, &mut ctx).await?;
        }
        let continuing = stopped.as_ref().is_some_and(|error| internal_recovery_required(&error.code))
            && !*cancellation.borrow() && run.journal.is_some();
        if let Some(error) = stopped.clone().filter(|_| !continuing) {
            state.interrupt_open_segments(run).await;
            let round = state.delivery.segments.last()
                .and_then(|segment| segment["round"].as_u64())
                .unwrap_or(0).saturating_add(1).min(MAX_MESSAGE_ROUNDS);
            if state.pending_delivery_matches_blocker(&task_status) {
                state.publish_buffered_delivery(run, round, &new_id()).await;
            } else {
                // An unreviewed completion draft is not a valid result of a stopped task.
                // 未验收的完成草稿不能充当停止结果；原草稿仍保留在此前检查点中。
                state.delivery.pending_delivery.clear();
                state.delivery.delivery_continuing = false;
                if let Some(checkpoint) = state.completion.review_checkpoint.as_object_mut() {
                    checkpoint.remove("pendingDelivery");
                }
                let notice = format!("{}{}", if state.content().is_empty() { "" } else { "\n\n" },
                    stopped_reply_notice(&error, &completion_review(&state)));
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
        let response_only = super::super::task::response_only_scope(&state.assistant["CurrentWorkScope"]);
        let settled_limitation = super::super::task::genuine_blocker(&task_status);
        let final_version = if user_stopped || work_suspended || response_only || settled_limitation || final_received && state.completion.acceptance_reviewed {
            state.progress.verification()["codeVersionCoverage"]["latestSnapshot"].clone()
        } else { match tools::capture_code_version(&ctx).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                stopped.get_or_insert(error);
                Value::Null
            }
        } };
        let _ = state
            .progress
            .apply("progress.finalVersion", &json!({"snapshot":final_version}));
        let validation_config = runtime.config.clone();
        let validation_context =
            json!({"conversationId":run.conversation_id,"requestId":run.request_id,
                "bindingSnapshot":ctx.config["bindingSnapshot"],"workspaceMounts":ctx.config["workspaceMounts"]});
        // Stopping preserves previous evidence; it does not certify completion or run new checks.
        // 停止保留已有证据，不认证完成，也不启动新的验收检查。
        let evidence_validation = if work_suspended {
            json!({"state":"not-checked","reason":"execution-suspended-with-saved-progress"}) }
        else if user_stopped || response_only || settled_limitation {
            json!({"state":"not-checked","reason":"current-response-does-not-certify-background-work"}) }
        else if let Some(validation) = final_evidence_validation { validation }
        else { tokio::task::spawn_blocking(move || {
            crate::retrieval::gateway::final_validate(&validation_config, &validation_context)
        })
        .await
        .map_err(|_| GatewayError::new("EVIDENCE_WORKER_FAILED", "证据核对线程退出。", 500))?? };
        let review = completion_review(&state);
        let evidence_records = &review.records;
        let current_status = if response_only && final_received && stopped.is_none() {
            json!({"state":"complete","source":"current-turn-response","unmet":[]})
        } else { task_status };
        let broker_final = final_received && stopped.is_none() && state.completion.acceptance_reviewed
            && !user_stopped && !settled_limitation;
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
        if response_only { completion["backgroundGoalCertified"] = json!(false); }
        if state.assistant["UserTaskDirective"]["stop"] == true {
            completion["completionState"] = json!("partial");
            completion["userStopped"] = json!(true);
            completion["userDirectedDelivery"] = json!(state.assistant["UserTaskDirective"]["deliverExisting"] == true);
        }
        let mut diagnostics = native(state.progress.apply("progress.diagnostics", &json!({})))?;
        // Keep model timing and report accepted-request preparation separately, excluding desktop saves.
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
        state.assistant["DurationMs"] = json!(super::super::gateway_runtime::generation_elapsed_ms(
            state.delivery.generation_duration_base_ms, state.delivery.model_started,
        ));
        // Separate reply delivery from task success; retain failures in TaskCompletion.
        // 回复交付与任务成功分开，失败原因保留在 TaskCompletion。
        let reply_interrupted = super::super::gateway_runtime::reply_interrupted(
            stopped.as_ref().map_or("", |error| error.code.as_str()), *cancellation.borrow());
        if !continuing && !reply_interrupted {
            state.publish_final_segment(run).await;
        }
        if !continuing {
            // Delivery is being committed, not still generated; completion waits for durable settlement.
            // 答复进入正式保存，已非继续生成；成功终态仍须等待持久化结算。
            run.emit(json!({"type":"task_progress","phase":"saving","round":1})).await;
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
                stopped.as_ref().filter(|_| reply_interrupted && !continuing),
            )
            .await?;
        if !continuing {
            if !reply_interrupted && stopped.is_none() && completion["completionState"] == "completed" {
                if completion["completionState"] == "completed" && state.completion.acceptance_reviewed {
                    if let Some(journal) = &run.journal {
                        let observations = completion_transcript(&state).iter()
                            .flat_map(|round| round["observations"].as_array().into_iter().flatten().cloned())
                            .collect::<Vec<_>>();
                        if let Some(candidate) = crate::storage::experiences::verified_procedure_candidate(
                            &state.completion.acceptance_requirements.join("\n"), &observations) {
                            // Derived candidates are private hints, not artifacts, authority or mature reusable experience.
                            // 衍生候选仅作私有提示，不是产物、权限或成熟经验。
                            let _ = journal.emit_control("experience.candidate", &candidate);
                        }
                    }
                }
                crate::retrieval::gateway::enqueue_conversation_semantic(&runtime.config, &run.conversation_id);
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
    fn stopped_reply_reports_current_outputs_without_certifying_completion() {
        let error = GatewayError::new("TASK_CONTINUATION_NO_PROGRESS", "后续处理未能继续推进，已有结果和进度已保留。", 409);
        let mut review = CompletionEvidenceSet { verification:json!({}), valid_references:vec![], records:vec![
            json!({"current":true,"observationCurrent":true,"kinds":["execution","committed-output"]}),
            json!({"current":true,"observationCurrent":true,"tool":"artifact.open","kinds":["execution","observed-state"]}),
        ] };
        let notice = stopped_reply_notice(&error, &review);
        assert!(notice.starts_with("文件已保存，文件预览也已打开。"));
        assert!(!notice.contains("No new result") && !notice.contains("任务已完成") && !notice.contains("中间内容"));
        review.verification = json!({"state":"checks-failed","pendingValidation":true});
        assert!(stopped_reply_notice(&error, &review).contains("检查未通过"));
        review.records.iter_mut().for_each(|record| record["current"] = json!(false));
        assert!(!stopped_reply_notice(&error, &review).contains("文件已保存"));
        assert!(!stopped_reply_notice(&error, &review).contains("预览已打开"));
        review.records = vec![json!({"current":true,"tool":"artifact.open","kinds":["execution"]})];
        assert!(!stopped_reply_notice(&error, &review).contains("预览已打开"));
    }
}
