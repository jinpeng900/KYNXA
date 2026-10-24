//! Child acceptance uses shared evidence rules and preserves delegated delivery boundaries.
//! 子任务验收复用共享证据规则，并保留委派交付边界。

use super::*;

pub(super) enum ReviewOutcome { Continue, Completed }

pub(super) struct ReviewPhase<'a> {
    pub runtime: &'a Arc<GatewayRuntime>,
    pub run: &'a Arc<RequestRun>,
    pub journal: &'a Arc<TaskJournal>,
    pub child: &'a TaskRecord,
    pub ctx: &'a GatewayToolContext,
    pub state: &'a mut WorkerContext,
    pub response: &'a protocol::ModelResponse,
    pub reviewing: bool,
    pub semantic: &'a mut SemanticTask,
    pub catalog: &'a [Value],
    pub all: &'a [Value],
    pub acceptance_query: &'a str,
    pub limits: &'a Value,
    pub requires_verification: bool,
    pub requires_readback: bool,
    pub requires_visual: bool,
    pub live_memory: &'a mut LiveMemoryProjection,
    pub system: &'a mut String,
    pub skill_block: &'a mut String,
    pub repairs: &'a mut u64,
}

pub(super) async fn finish_round(phase: ReviewPhase<'_>) -> Result<ReviewOutcome> {
    let ReviewPhase { runtime, run, journal, child, ctx, state, response, reviewing,
        semantic, catalog, all, acceptance_query, limits, requires_verification,
        requires_readback, requires_visual, live_memory, system, skill_block, repairs } = phase;
    let inspected = semantic
        .apply("task.inspect", &json!({"text":response.content}))
        .unwrap_or(json!({"content":response.content,"state":"unknown"}));
    let response_only = task::response_only_scope(&ctx.config["currentWorkScope"]);
    let acceptance_query = if !task::scope_allows_mutation(&ctx.config["currentWorkScope"]) {
        ctx.config["currentWorkScope"]["ancestorRequest"].as_str()
            .or_else(||ctx.config["currentWorkScope"]["request"].as_str()).unwrap_or(acceptance_query)
    } else { acceptance_query };
    let requires_verification = !response_only && requires_verification;
    let requires_readback = !response_only && requires_readback;
    let requires_visual = !response_only && requires_visual;
    if !response_only && response.calls.is_empty() && inspected["source"] == "unexecuted-tool-intent" {
        let mut available = catalog.to_vec();
        available.extend(all.iter().cloned());
        if retain_child_tool_intent(state, &response.content, &available) {
            state.history.push(json!({"role":"user","content":child_tool_intent_notice(state)}));
            checkpoint(run,journal,state,"unexecuted-tool-intent").await?;
            observe_unexecuted_response(run,journal,state).await?;
            return Ok(ReviewOutcome::Continue);
        }
    }
    let public = task::strip_history_records(inspected["content"].as_str().unwrap_or(""));
    if !reviewing {
        let overlap = budget::apply(
            "output.overlap",
            &json!({"previous":state.output,"text":public}),
        ).unwrap_or(json!(public));
        state.output.push_str(overlap.as_str().unwrap_or(&public));
        if response.calls.is_empty() && !state.memory_plan_invalidated && !public.trim().is_empty() {
            state.final_summary=public.clone();
        }
        let _ = semantic.apply("task.observe", &json!({"content":public}));
    } else {
        state.acceptance_review = inspected.clone();
        let _ = semantic.apply("task.reviewResult",&json!({"value":inspected}));
    }
    checkpoint(run,journal, state, "next-step").await?;
    if state.memory_plan_invalidated {
        state.review_pending=false;
        return Ok(ReviewOutcome::Continue);
    }
    if !response.calls.is_empty() {
        if let Some(fields) = state.review_checkpoint.as_object_mut() { fields.remove("reviewFormatRepair"); }
        return Ok(ReviewOutcome::Continue);
    }
    let needs_verification = !response_only && (requires_verification
        || task::scope_allows_mutation(&ctx.config["currentWorkScope"]) && !state.changed_files.is_empty());
    let execution_intent = task::continuation_needs_progress(&ctx.config["currentWorkScope"],
        state.tool_calls > 0 || !state.result_sources.is_empty(), needs_verification);
    let review = ChildCompletionReview::new(state, &acceptance_query);
    let decision = semantic.decide_typed(&task::TaskDecisionInput {
        reply: inspected.clone(), is_review: reviewing, verification: review.verification.clone(),
        has_calls: false, output_truncated: response.output_truncated,
        execution_intent, response_only,
        current_work_scope: ctx.config["currentWorkScope"].clone(),
        output_continuations: state.continuations, requires_verification: needs_verification,
        evidence_required: needs_verification, expected_requirements: state.acceptance_requirements.clone(),
        valid_evidence: review.valid_evidence.clone(), evidence_records: Some(review.records.clone()),
        requires_readback, has_readback: !review.readback_refs.is_empty(),
        requires_visual_readback: requires_visual, has_visual_readback: !review.visual_readback_refs.is_empty(),
        review_attempts: state.review_attempts, minimum_active_ms: if response_only {0} else {limits["minActiveMs"].as_u64().unwrap_or(0)},
        active_elapsed_ms: run.effective_active_ms(), remaining_rounds: Some(limits["maxRounds"].as_u64().unwrap_or(64)
            .saturating_sub(state.rounds.saturating_sub(state.segment_round_base))),
        ..Default::default()
    }).into_value();
    let format_notice = if decision["action"] == "retry-review" {
        let repair = task::review_format_repair(&mut state.review_checkpoint, &response.content,
            response.output_truncated, response.usage["completion_tokens"].as_u64().unwrap_or(0));
        if repair["action"] == "wait" {
            state.review_pending = false;
            state.acceptance_reviewed = false;
            checkpoint(run,journal,state,"review-format-await-user").await?;
            return Err(failure("TASK_COMPLETION_REVIEW_INVALID",repair["reason"].as_str().unwrap_or("Review format is unchanged. / 核验格式未改善。")));
        }
        repair["reason"].as_str().map(str::to_owned)
    } else {
        if let Some(fields) = state.review_checkpoint.as_object_mut() { fields.remove("reviewFormatRepair"); }
        None
    };
    // Format repair has its own measured progress; it is not failed execution work.
    // 格式修正独立衡量进展，不计作执行失败；只读调查仍检查真实执行进展。
    if execution_intent && !response.output_truncated
        && matches!(decision["action"].as_str(), Some("review" | "resume")) {
        observe_unexecuted_response(run,journal,state).await?;
    }
    match decision["action"].as_str() {
        Some("review" | "retry-review") => {
            state.review_pending = true;
            state.review_attempts += u64::from(reviewing);
            let mut instruction = review_instruction(state,&review,requires_readback,requires_visual);
            if let Some(notice) = format_notice { instruction.push('\n'); instruction.push_str(&notice); }
            state.history.push(json!({"role":"user","content":instruction}));
            checkpoint(run,journal,state,"acceptance-review").await?;
            if reviewing {
                let mut repair_cancel = run.cancellation.subscribe();
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis((state.review_attempts * 250).min(2000))) => {}, _ = tools::wait_for_cancellation(&mut repair_cancel) => {} }
            }
            return Ok(ReviewOutcome::Continue);
        }
        Some("resume") => {
            state.review_pending = false;
            state.acceptance_reviewed = false;
            state.continuations += 1;
            let _ = semantic.apply("task.resumed",&json!({}));
            let remaining = decision["remaining"].as_array().filter(|items|!items.is_empty())
                .map(|items|json!(items).to_string()).unwrap_or_else(||json!(task::remaining_work(&inspected)).to_string());
            if !state.review_checkpoint.is_object() { state.review_checkpoint = json!({}); }
            state.review_checkpoint["remaining"] = decision["remaining"].clone();
            state.history.push(json!({"role":"user","content":format!("Broker: continue feasible work within the parent's current authorized scope. Remaining={remaining}. Preserve successful effects. Use a reasonable alternative for ordinary difficulty; do not repeat unchanged calls or idle. If a necessary capability is demonstrably absent or requirements are contradictory, report feasibility basis and alternatives instead of forcing further execution.")}));
            checkpoint(run,journal,state,"continue-unmet-acceptance").await?;
            return Ok(ReviewOutcome::Continue);
        }
        Some("interrupt") => {
            state.review_pending = false;
            state.acceptance_reviewed = false;
            state.acceptance_review = inspected.clone();
            checkpoint(run,journal,state,"execution-await-user").await?;
            let code = decision["code"].as_str().unwrap_or("TASK_CONTINUATION_NO_PROGRESS");
            return Err(failure(code,inspected["reason"].as_str()
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or("The task needs a changed condition or a new instruction; saved work is retained. / 任务需条件变化或新指令，已有工作已保留。")));
        }
        Some("final") => { state.acceptance_reviewed = reviewing; }
        _ => {
            state.review_pending = true;
            state.history.push(json!({"role":"user","content":review_instruction(state,&review,requires_readback,requires_visual)}));
            checkpoint(run,journal,state,"repair-task-assessment").await?;
            return Ok(ReviewOutcome::Continue);
        }
    }
    if *run.cancellation.borrow() {
        return Err(GatewayError::new("ABORT_ERR", "Child stopped before final verification; saved progress is retained. / 子任务已停止，保留原进度，不再启动收尾核验。", 499));
    }
    if let Some(status) = live_memory.refresh(&runtime.config.data_root,&run.conversation_id,&run.request_id,system).await? {
        state.acceptance_reviewed = false;
        state.review_pending = needs_verification;
        state.history.push(json!({"role":"user","content":format!("Broker: scope memory changed before delivery. Reassess the original requirements under current confirmed memory; do not repeat successful effects. {}",status["invalidSources"])}));
        checkpoint(run,journal,state,"memory-refresh-before-delivery").await?;
        return Ok(ReviewOutcome::Continue);
    }
    if response_only { return Ok(ReviewOutcome::Completed); }
    let descendants = runtime.tasks.children_acceptance(journal.task_id()).await?;
    if descendants["allAccepted"] != true {
        state.acceptance_reviewed = false;
        state.review_pending = false;
        state.history.push(json!({"role":"user","content":format!("Broker: delegated results still need your actual acceptance. Use task.wait for running children, read their real output with task.result, inspect the evidence, then task.result review accept or return with concrete missing requirements. Child self-review is not your review. Continue the original task: {descendants}")}));
        checkpoint(run,journal,state,"parent-child-acceptance").await?;
        return Ok(ReviewOutcome::Continue);
    }
    if requires_verification {
        let headers = tools::relevant_skill_headers(&ctx,&child.goal).await.unwrap_or_default();
        let current = format!("\n<kynxa_available_skills>\nCurrent relevant skill headers (read bodies with skill.read; stale historical skill instructions are not current configuration): {}\n</kynxa_available_skills>\n",json!(headers));
        if current != *skill_block {
            *system = system.replacen(skill_block.as_str(),&current,1);
            *skill_block = current;
            state.acceptance_reviewed = false;
            state.review_pending = needs_verification;
            state.history.push(json!({"role":"user","content":"Broker: available skill configuration changed before delivery. Follow the current headers and recheck the work; preserve successful outputs and receipts."}));
            checkpoint(run,journal,state,"skill-refresh-before-delivery").await?;
            return Ok(ReviewOutcome::Continue);
        }
    }
    let config = runtime.config.clone();
    let identity = json!({"conversationId":run.conversation_id,"requestId":run.request_id});
    let freshness = tokio::task::spawn_blocking(move || {
        crate::retrieval::gateway::final_validate(&config, &identity)
    })
    .await
    .map_err(|_| {
        failure(
            "EVIDENCE_WORKER_FAILED",
            "Child evidence validator stopped. / 子证据核验者退出。",
        )
    })??;
    if freshness["current"] == false {
        *repairs += 1;
        state.acceptance_reviewed = false;
        state.review_pending = false;
        state.history.push(json!({"role":"user","content":format!("Broker: source versions changed before completion. Re-read only affected authorized sources and correct conclusions without repeating successful effects: {}",freshness["invalidSources"])}));
        checkpoint(run,journal,state,"source-version-refresh").await?;
        let mut refresh_cancel = run.cancellation.subscribe();
        tokio::select! { _ = tokio::time::sleep(Duration::from_millis((*repairs * 250).min(2000))) => {}, _ = tools::wait_for_cancellation(&mut refresh_cancel) => {} }
        return Ok(ReviewOutcome::Continue);
    }
    state.code_version = tools::capture_code_version(&ctx).await?;
    Ok(ReviewOutcome::Completed)
}

async fn observe_unexecuted_response(run: &Arc<RequestRun>, journal: &Arc<TaskJournal>, state: &mut WorkerContext)
    -> Result<()> {
    let plan = state.work_progress.observe_round(&RoundProgress {execution_without_action:true,..Default::default()});
    if plan.correction_due {
        state.history.push(json!({"role":"user","content":"Broker: repeating the same plan added no new evidence. Use the native protocol for an actual unmet action, or deliver the result already supported by receipts. Do not replay successful effects."}));
    }
    if let Some(error) = gateway_tool_execution::progress_interruption(&plan) {
        state.review_pending = false;
        state.acceptance_reviewed = false;
        checkpoint(run,journal,state,"unchanged-response-await-user").await?;
        return Err(error);
    }
    Ok(())
}

/// Children use the root's successful-receipt contract, never tool arguments as proof.
/// 子任务沿用根任务的成功回执合同，工具参数不作证明。
pub(super) fn verified_completion_receipt(saved: &Value) -> bool {
    saved["verifiedSuccess"] != false && saved["completionProofVerification"] != "pending"
        && task::successful_completion_receipt(&saved["result"])
}

/// One review borrows the same receipt facts; the snapshot is never persisted or reused after new work.
/// 同一次评审复用相同回执事实；临时快照不持久化，新操作后重新生成。
pub(super) struct ChildCompletionReview {
    pub(super) verification: Value,
    pub(super) valid_evidence: Vec<Value>,
    pub(super) records: Vec<Value>,
    pub(super) readback_refs: Vec<Value>,
    pub(super) visual_readback_refs: Vec<Value>,
}

impl ChildCompletionReview {
    pub(super) fn new(state: &WorkerContext, goal: &str) -> Self {
        let mut progress = RunProgress::default();
        let mut valid_evidence = Vec::new();
        let mut records = Vec::new();
        let mut result_refs = HashMap::new();
        let mut mutation_sequence = 0_u64;
        for saved in &state.receipts {
            let _ = progress.apply("progress.observeOutcome", &json!({"call":saved["call"],"result":saved["result"],
                "canonicalResult":saved["result"]["content"],"beforeVersion":saved["beforeVersion"],"afterVersion":saved["afterVersion"],
                "validationScope":saved["validationScope"]}));
            if let Some(id) = saved["call"]["id"].as_str() {
                result_refs.entry(id).or_insert(&saved["result"]["resultRef"]);
            }
            if !verified_completion_receipt(saved) { continue; }
            let name = saved["call"]["name"].as_str().unwrap_or("");
            if task::is_completion_evidence_tool(name) {
                if let Some(id) = saved["call"]["id"].as_str().filter(|id| !id.is_empty()) {
                    valid_evidence.push(json!(id));
                }
                for path in task::completed_output_paths(name, &saved["result"]) {
                    if !valid_evidence.contains(&path) { valid_evidence.push(path); }
                }
            }
            let mutating = saved["mutating"].as_bool().unwrap_or_else(||
                saved["observation"] != true && saved["result"]["executed"] != false
                    && saved["result"]["reused"] != true && !name.starts_with("task.")
                    && !matches!(name, "tool.load" | "context.compact"));
            if mutating { mutation_sequence = mutation_sequence.saturating_add(1); }
            let mut proof = saved["completionProof"].clone();
            if !proof.is_object() || proof["id"] != saved["call"]["id"] { continue; }
            proof["mutationSequence"] = json!(mutation_sequence);
            proof["mutating"] = json!(mutating);
            if saved["visualDeliveredToModel"] == true { proof["visualObserved"] = json!(true); }
            records.push(proof);
        }
        let _ = progress.apply("progress.finalVersion", &json!({"snapshot":state.code_version}));
        let evidence = completion_evidence::CompletionEvidenceSet::from_records(
            progress.verification(), valid_evidence, records,
        ).with_current_requirements(&progress, &state.acceptance_requirements);
        let scope = evidence.validation_scope(&state.acceptance_requirements);
        let completion_evidence::CompletionEvidenceSet { mut verification, valid_references: valid_evidence, records } = evidence;
        verification["needsValidation"] = json!(progress.needs_validation_for_scope(goal, &scope));
        let readbacks = |visual| -> Vec<Value> {
            completion_evidence::readbacks(&records, visual).filter_map(|proof|
                result_refs.get(proof["id"].as_str()?).map(|reference|
                    json!({"toolCallId":proof["id"],"resultRef":reference,"visual":proof["visualObserved"]==true}))).collect()
        };
        let readback_refs = readbacks(false);
        let visual_readback_refs = readbacks(true);
        Self { verification, valid_evidence, records, readback_refs, visual_readback_refs }
    }
}

pub(super) async fn restore_completion_proofs(root: &std::path::Path, conversation: &str, request: &str, state: &mut WorkerContext) {
    let observations = state.receipts.iter().map(|saved| {
        let mut observation = json!({"id":saved["call"]["id"],"name":saved["call"]["name"],
            "status":saved["result"]["status"],"ref":saved["result"]["resultRef"]});
        if let Some(proof) = saved.get("completionProof") {
            observation["completionProof"] = proof.clone();
            observation["verifiedSuccess"] = saved.get("verifiedSuccess").cloned()
                .unwrap_or_else(|| json!(task::successful_completion_receipt(&saved["result"])));
        }
        if let Some(pending) = saved.get("completionProofVerification") {
            observation["completionProofVerification"] = pending.clone();
        }
        observation
    }).collect::<Vec<_>>();
    if observations.is_empty() { return; }
    let mut rounds = vec![json!({"observations":observations})];
    // Reuse archive ownership checks; missing metadata never authorizes replay.
    // 复用归档归属核验，元数据缺失不能作为重做操作的依据。
    gateway_tool_execution::restore_completion_evidence(root, conversation, request, &mut rounds).await;
    for observation in rounds[0]["observations"].as_array().into_iter().flatten() {
        if let Some(saved) = state.receipts.iter_mut().find(|saved| saved["call"]["id"] == observation["id"]
            && saved["result"]["resultRef"] == observation["ref"]) {
            // Copy invalidation as well as proof; historical receipt bytes and effects stay untouched.
            // 证明及失效状态一起恢复，历史回执正文与副作用事实保持原样。
            for field in ["completionProof", "verifiedSuccess"] {
                if let Some(value) = observation.get(field) { saved[field] = value.clone(); }
            }
            if let Some(pending) = observation.get("completionProofVerification") {
                saved["completionProofVerification"] = pending.clone();
            } else { saved.as_object_mut().unwrap().remove("completionProofVerification"); }
        }
    }
}

pub(super) fn review_instruction(state: &WorkerContext, review: &ChildCompletionReview, requires_readback: bool, requires_visual: bool) -> String {
    let requirements: Vec<Value> = state.acceptance_requirements.iter().enumerate().map(|(index, item)|
        json!({"requirementId":format!("requirement-{}",index + 1),"item":item})).collect();
    let instruction = format!("Broker: {} Readback required={requires_readback}, image observation required={requires_visual}.\nExpected acceptance requirements: {}\nVerified completion evidence: {}\nCurrent readback evidence: {}\nCurrent visual observations: {}\nUnresolved execution checks: {}",
        task::COMPLETION_REVIEW_CONTRACT,json!(requirements),json!(review.valid_evidence),json!(review.readback_refs),json!(review.visual_readback_refs),
        completion_evidence::unresolved_check_summary(&review.verification));
    if state.review_attempts == 0 && state.review_checkpoint["remaining"].as_array().is_none_or(Vec::is_empty) {
        return instruction;
    }
    let directory = completion_evidence::directory(&review.records);
    instruction + &format!("\nNative receipt directory (actual IDs, tools and targets; labels cannot change them): {directory}. Use exact IDs alone in evidence, explanations in item/reason. Repair references from this directory without sweeping unrelated archives.")
}
