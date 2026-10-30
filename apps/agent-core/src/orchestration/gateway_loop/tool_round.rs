//! Root tool dispatch and durable receipt settlement, separate from model planning.
//! 根任务工具派发与正式回执结算，与模型规划分开。

use super::*;

/// Borrow only this tool round; no state or ledger is copied.
/// 只借用本轮所需状态，不复制任务状态或账本。
pub(super) struct ToolRound<'a> {
    pub runtime: &'a Arc<GatewayRuntime>,
    pub run: &'a Arc<RequestRun>,
    pub input: &'a Value,
    pub state: &'a mut LoopState,
    pub ctx: &'a mut tools::GatewayToolContext,
    pub all: &'a mut Vec<Value>,
    pub system: &'a mut String,
    pub live_memory: &'a mut LiveMemoryProjection,
    pub cancellation: &'a mut tokio::sync::watch::Receiver<bool>,
    pub protocol_name: &'a str,
    pub limits: &'a Value,
    pub response: &'a protocol::ModelResponse,
    pub declared_tools: &'a [Value],
    pub round: u64,
    pub segment_initial_calls: u64,
    pub segment_initial_active_ms: u64,
    pub summarized_work_changed: bool,
}

pub(super) struct ToolRoundOutcome {
    pub stopped: Option<GatewayError>,
    pub review_invalidated: bool,
}

impl ToolRound<'_> {
    pub(super) async fn execute(self) -> Result<ToolRoundOutcome, GatewayError> {
        let Self { runtime, run, input, mut state, ctx, all, mut system, live_memory,
            mut cancellation, protocol_name, limits, response, declared_tools, round, segment_initial_calls,
            segment_initial_active_ms, summarized_work_changed } = self;
        let mut stopped = None;
        let mut review_invalidated = false;
        let permission = input["permissionMode"].as_str().unwrap_or("smart");
        let mut results = Vec::new();
        let mut round_progress = RoundProgress {
            evidence_changed: summarized_work_changed,
            ..Default::default()
        };
        // Batch independent reads; settle receipts, history and archives in original call order.
        // 独立读取批量执行，回执、历史及归档仍按原调用顺序结算。
        let batch_catalog = ctx.runtime.schemas.read().await.clone();
        let experience_names = response.calls.iter().filter_map(|call|call["name"].as_str().map(str::to_owned)).collect::<Vec<_>>();
        let experience_identities = tools::experience_identities(&ctx, &experience_names).unwrap_or_else(|_|json!({}));
        let tool_batches = gateway_tool_batch::plan_with_catalog(&response.calls, &batch_catalog);
        let mut prefetched = HashMap::new();
        let mut memory_plan_invalidated = false;
        for (call_index, call) in response.calls.iter().enumerate() {
            let mut prefetch = prefetched.remove(&call_index);
            if prefetch.is_none() && (run.active_elapsed(run.created).as_millis() as u64).saturating_sub(segment_initial_active_ms)
                >= limits["maxDurationMs"].as_u64().unwrap_or(1800000)
            {
                stopped = Some(GatewayError::new(
                    "TOOL_RUN_TIMEOUT",
                    "任务总时间已到，已完成内容保留。",
                    504,
                ));
                break;
            }
            if prefetch.is_none() && *cancellation.borrow() {
                stopped = Some(GatewayError::new("ABORT_ERR", "任务已取消。", 499));
                break;
            }
            // Recheck revocations before the next batch; settle completed prefetched reads normally.
            // 下一批派发前重核撤回，已完成的预取回执正常结算。
            if prefetch.is_none() && !memory_plan_invalidated
                && let Some(status) = live_memory.refresh(&runtime.config.data_root, &run.conversation_id, &run.request_id, &mut system).await? {
                memory_refresh_notice(&mut state, &status);
                review_invalidated = true;
                memory_plan_invalidated = true;
            }
            if prefetch.is_none() && (memory_plan_invalidated || steering_pending(&run, &ctx)) {
                // Pair undispatched calls with skipped results before applying steering.
                // 应用插话前，为未派发调用配对跳过结果。
                for skipped in &response.calls {
                    if !results.iter().any(|result:&Value|result["call"]["id"]==skipped["id"]) {
                        let skipped_receipt = if memory_plan_invalidated {
                            tool_execution_receipt(Ok(memory_replanning_result()))
                        } else {
                            json!({"status":"error","executed":false,"isError":true,"code":"TASK_PLAN_UPDATED","content":{"notice":"Not dispatched: the user amended the current task. Continue with the new requirements and retain completed receipts."}})
                        };
                        results.push(json!({"call":skipped,"observation":is_read_only_tool(skipped["name"].as_str().unwrap_or("")),
                            "result":skipped_receipt}));
                    }
                }
                break;
            }
            if let Some(batch) = tool_batches.iter().find(|batch| batch.start == call_index && prefetch.is_none()) {
                prefetched = prefetch_tool_batch(&mut state, &run, &runtime, &ctx, &response.calls, batch.clone(), round,
                    segment_initial_calls.saturating_add(limits["maxToolCalls"].as_u64().unwrap_or(256))).await?;
                prefetch = prefetched.remove(&call_index);
            }
            let name = call["name"].as_str().unwrap_or("");
            let (version_context, workspace_failure) = if tools::workspace_path_tool(name, &call["arguments"]) {
                match tools::select_workspace_context(&ctx, &call["arguments"]).await {
                    Ok(selected) => (selected, None),
                    Err(error) => (ctx.clone(), Some(error)),
                }
            } else { (ctx.clone(), None) };
            let operation_id = gateway_tool_batch::operation_id(&version_context, call, round);
            let mut tool_execution_ms = 0u64;
            let mut operation_dispatched = false;
            let control_correction = super::super::task::native_task_control_receipt(call, &state.completion.acceptance_requirements);
            let is_observation = control_correction.is_some() || crate::tools::policy::is_read_only_call(name, &call["arguments"])
                || matches!(name, "tool.load" | "task.document" | "context.compact");
            let _workspace_write = if workspace_failure.is_none() && !is_observation && !name.starts_with("task.") {
                Some(tokio::select! {
                    guard = runtime.tasks.workspace_mutation_guard(&version_context.workdir) => guard?,
                    _ = tools::wait_for_cancellation(&mut cancellation) => return Err(GatewayError::new("ABORT_ERR", "任务等待写入时已取消。", 499)),
                })
            } else {
                None
            };
            let track_version = name.starts_with("terminal.")
                || ["filesystem.write", "filesystem.edit", "filesystem.delete"].contains(&name);
            let before_version = if track_version && workspace_failure.is_none() {
                match tools::capture_code_version(&version_context).await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        stopped = Some(error);
                        break;
                    }
                }
            } else {
                Value::Null
            };
            let failure_definition = tool_failure_definition(call, &all, &batch_catalog);
            let failure_snapshot = if before_version.is_null() {
                state.progress.verification()["codeVersionCoverage"]["latestSnapshot"].clone()
            } else { before_version.clone() };
            let mut failure_environment = tool_failure_environment(&version_context,
                &failure_snapshot, state.completion.mutation_sequence);
            failure_environment["configuration"]["acceptanceRequirements"] = json!(state.completion.acceptance_requirements);
            failure_environment["operationIdentity"] = json!(operation_id);
            let failure_correction = state.work_progress.failed_actions()
                .before_dispatch(call, &failure_definition, &failure_environment);
            let count = count_tool_attempts(&state.assistant);
            if prefetch.is_none() && count.saturating_sub(segment_initial_calls) >= limits["maxToolCalls"].as_u64().unwrap_or(256) {
                stopped = Some(GatewayError::new(
                    "TOOL_RUN_CALL_LIMIT",
                    "工具调用额度已用完。",
                    409,
                ));
                break;
            }
            let tool_start = prefetch.as_ref().map_or_else(Instant::now, |prefetched| prefetched.started);
            let saved = native(state.tools.effects.apply(
                "effects.previous",
                &json!({"call":call,"observation":is_observation}),
            ))?;
            state.delivery.timeline_order += 1;
            let tool_order = state.delivery.timeline_order;
            // The batch already announced these call orders; do not register them again.
            // 批次已公布这些调用顺序，不重复登记。
            if prefetch.is_none() {
                let activity = json!({"toolCallId":call["id"],"name":name,"arguments":call["arguments"],"status":"running","summary":"执行中","round":round,"order":tool_order});
                run.emit(json!({"type":"tool_call","tool":activity})).await;
                state.assistant["ToolActivities"]
                    .as_array_mut()
                    .unwrap()
                    .push(activity.clone());
                state.save(&runtime, &run, "streaming", None).await?;
            }
            let mut definition_failure = control_correction.or_else(|| workspace_failure.map(|error|json!({"status":"error","executed":false,"isError":true,
                "code":error.code,"content":error.response()})));
            if definition_failure.is_none() && saved.is_null() && prefetch.is_none() {
                definition_failure = failure_correction;
            }
            if call["unavailable"] == true
                && definition_failure.is_none()
                && saved.is_null()
                && !steering_pending(&run, &ctx)
            {
                let candidate = {
                    let cached = ctx.runtime.schemas.read().await;
                    all.iter().chain(cached.iter()).find(|tool| {
                        tools::executable_descriptor(tool) && tool["name"] == name
                    }).map(|tool| tool["name"].clone())
                };
                if let Some(candidate) = candidate {
                    // Load the undispatched call once, then execute its original arguments; never replay unknown effects.
                    // 未派发调用加载一次后执行原参数，不重放未知副作用。
                    ctx.config["activeToolCallId"] = call["id"].clone();
                    match tools::execute("tool.load", json!({"names":[candidate]}), &ctx).await {
                        Ok(loaded) => {
                            select_loaded_tools(&mut state, &loaded, &json!([name]));
                            if !loaded["tools"].as_array().into_iter().flatten().any(|tool| {
                                tool["name"] == name && tools::executable_descriptor(tool)
                            }) {
                                definition_failure = Some(json!({"status":"error","executed":false,"isError":true,"code":"TOOL_NOT_LOADED",
                                    "content":{"notice":"The real loader did not return an executable definition; this call was not dispatched.","requestedName":name,"loadResult":loaded}}));
                            }
                        }
                        Err(error) => {
                            let mut content = error.response();
                            content["requestedName"] = json!(name);
                            definition_failure = Some(json!({"status":"error","executed":false,"isError":true,"code":error.code,
                                "content":content}));
                        }
                    }
                } else {
                    definition_failure = Some(json!({"status":"error","executed":false,"isError":true,"code":"TOOL_NOT_DISCOVERED",
                        "content":{"notice":"No exact executable definition is known. Discover the actual capability through tool.search; this call was not dispatched.","requestedName":name}}));
                }
            }
            let was_prefetched = prefetch.is_some();
            let (mut prefetched_result, prefetched_receipt, archived_reference) = match prefetch.take() {
                None => (None, None, None),
                Some(prefetched) => {
                    operation_dispatched = prefetched.dispatched;
                    tool_execution_ms = prefetched.execution_ms;
                    match prefetched.outcome {
                        gateway_tool_batch::PrefetchOutcome::NotDispatched(receipt) => (None, Some(receipt), None),
                        gateway_tool_batch::PrefetchOutcome::AwaitingApproval(result) => (Some(result), None, None),
                        gateway_tool_batch::PrefetchOutcome::Archived { receipt, reference } => (None, Some(receipt), Some(reference)),
                        gateway_tool_batch::PrefetchOutcome::Failed(error) => return Err(error),
                    }
                }
            };
            let mut receipt = if let Some(receipt) = prefetched_receipt {
                receipt
            } else if !was_prefetched && steering_pending(&run, &ctx) {
                json!({"status":"error","executed":false,"isError":true,"code":"TASK_PLAN_UPDATED","content":{"notice":"Not dispatched: the user amended the task before execution."}})
            } else if let Some(failure) = definition_failure {
                failure
            } else if !saved.is_null() {
                let mut receipt = saved["result"].clone();
                receipt["reused"] = json!(true);
                receipt
            } else {
                if !was_prefetched && let Some(journal) = &run.journal {
                    gateway_tool_batch::record_dispatch(journal, &run, &version_context, call, round, &operation_id, is_observation)?;
                    operation_dispatched = true;
                }
                ctx.config["activeToolCallId"] = call["id"].clone();
                ctx.config["activeOperationId"] = json!(operation_id);
                ctx.config["acceptanceRequirements"] = json!(state.completion.acceptance_requirements);
                if name.starts_with("task.") || name.starts_with("knowledge.") {
                    // Capture this dispatch identity, not the context from the start of the request.
                    // 捕获本次实际派发身份，不能沿用请求开始时的旧上下文。
                    ctx.external_execute = Some(specialist_executor(&runtime, &run, &ctx, permission,
                        input["message"].as_str().unwrap_or("")));
                }
                let dispatched_at = Instant::now();
                let result = if let Some(result) = prefetched_result.take() {
                    result
                } else if name == "task.document" {
                    document_tool(&mut state, &run, &call["arguments"]).await
                } else {
                    tools::execute(name, call["arguments"].clone(), &ctx).await
                };
                tool_execution_ms = tool_execution_ms.saturating_add(
                    dispatched_at.elapsed().as_millis().min(u64::MAX as u128) as u64,
                );
                let result = match result {
                    Ok(result) if result["requiresApproval"] == true => {
                        match runtime
                            .await_approval(
                                &run,
                                call,
                                &result,
                                round,
                                tool_order,
                                state.started
                                    + Duration::from_millis(
                                        limits["maxDurationMs"].as_u64().unwrap_or(1800000),
                                    ),
                            )
                            .await
                        {
                            Ok(true) => {
                                // Recheck memory after approval; withdrawn memory remains invalid.
                                // 审批后重核记忆，等待期间撤回的内容仍无效。
                                if let Some(status) = live_memory.refresh(&runtime.config.data_root, &run.conversation_id, &run.request_id, &mut system).await? {
                                    memory_refresh_notice(&mut state, &status);
                                    review_invalidated = true;
                                    memory_plan_invalidated = true;
                                    Ok(memory_replanning_result())
                                } else {
                                    ctx.approved = true;
                                    let dispatched_at = Instant::now();
                                    ctx.config["approvedInvocation"] = result["invocation"].clone();
                                    let result =
                                        tools::execute(name, call["arguments"].clone(), &ctx).await;
                                    tool_execution_ms = tool_execution_ms.saturating_add(
                                        dispatched_at.elapsed().as_millis().min(u64::MAX as u128)
                                            as u64,
                                    );
                                    ctx.approved = false;
                                    ctx.config
                                        .as_object_mut()
                                        .unwrap()
                                        .remove("approvedInvocation");
                                    result
                                }
                            }
                            Ok(false) => Err(GatewayError::new(
                                "TOOL_APPROVAL_DENIED",
                                "此操作未获批准。",
                                403,
                            )),
                            Err(error) => Err(error),
                        }
                    }
                    result => result,
                };
                tool_execution_receipt(result)
            };
            normalize_reused_receipt(name, &mut receipt);
            state.work_progress.failed_actions_mut().observe(call, &failure_definition, &failure_environment, &receipt);
            let canonical = receipt["content"].clone();
            let root = runtime.config.data_root.clone();
            let id = run.conversation_id.clone();
            let request = run.request_id.clone();
            let stored_call = call.clone();
            let environment = canonical.get("executionEnvironment").cloned();
            let archived_value = canonical.clone();
            let reference = if let Some(reference) = archived_reference.clone() { reference } else { tokio::task::spawn_blocking(move || {
                data::save_tool_result(
                    &root,
                    &id,
                    &request,
                    &stored_call,
                    &archived_value,
                    environment.as_ref(),
                )
            })
            .await
            .map_err(|_| GatewayError::new("DATA_WORKER_FAILED", "工具回执保存失败。", 500))?? };
            receipt["resultRef"] = reference.clone();
            let mut completion_proof = super::super::completion_evidence::from_receipt(&call, &receipt);
            // Batch calls settled on completion; settle only other dispatches here.
            // 批次调用完成时已结算，此处仅结算其他派发。
            if operation_dispatched && archived_reference.is_none() && let Some(journal) = &run.journal {
                gateway_tool_batch::finish_dispatch(journal, &operation_id, &receipt, &reference, name, tool_execution_ms)?;
            }
            if successful_completion_receipt(&receipt) {
                if let Some(journal) = &run.journal {
                    round_progress.evidence_changed |= state.documents.observe_output_version(journal, &canonical).await?;
                }
                if name == "tool.search" && canonical["tools"].is_array() {
                    if !state.completion.review_checkpoint.is_object() { state.completion.review_checkpoint = json!({}); }
                    state.completion.review_checkpoint["executionCapabilities"] = json!(canonical["tools"].as_array().into_iter().flatten()
                        .filter_map(|tool| tool["name"].as_str()).filter(|name| !matches!(*name,"tool.search" | "tool.load"))
                        .take(6).collect::<Vec<_>>());
                }
            }
            let source_metadata = crate::models::tool_context::source_metadata(&receipt["content"]);
            // Derive identity from full native text/structuredContent, not shortened projections.
            // 按完整原生 text/structuredContent 判定身份，不受展示缩短影响。
            if let Ok(observation) = crate::tools::observations::calculate(
                "observations.source",
                &json!({"call":call,"value":canonical}),
            ) {
                if observation.is_object() {
                    receipt["observationKey"] = observation["observationKey"].clone();
                    receipt["observationHash"] = observation["observationHash"].clone();
                }
            }
            round_progress.work_calls += 1;
            if matches!(name, "tool.search" | "tool.load") {
                round_progress.discovery_calls += 1;
            }
            if matches!(
                name,
                "filesystem.read" | "document.read" | "knowledge.read" | "web.fetch" | "tool.result.read"
            ) || name == "attachment.read" && successful_completion_receipt(&receipt)
                && super::super::task::attachment_read_has_content(&canonical) {
                round_progress.source_reads += 1;
                if receipt["isError"] != true
                    && receipt["status"] == "completed"
                    && let Some(journal) = &run.journal
                {
                    if let Some(document) = state
                        .documents
                        .observe_read(journal, name, &receipt["content"], &reference)
                        .await?
                    {
                        round_progress.new_read_range |= document["informationAdded"] == true;
                        if should_project_repeated_source_read(name, &document) {
                            receipt["content"] = repeated_read_projection(
                                &canonical,
                                &reference,
                                state.documents.saved_findings(
                                    document["sourceIdentity"].as_str().unwrap_or(""),
                                ),
                                None,
                            );
                        }
                    }
                }
            }
            if name == "tool.result.read"
                && receipt["isError"] != true
                && state.work_progress.needs_strategy_change()
                && source_metadata["nextOffset"]
                    .as_u64()
                    .zip(source_metadata["offset"].as_u64())
                    .is_some_and(|(end, start)| end.saturating_sub(start) > 4096)
                && state.context.result_sources.iter().any(|previous| {
                    previous["name"] == "tool.result.read"
                        && previous["status"] == "completed"
                        && previous["arguments"]["id"] == call["arguments"]["id"]
                        && previous["sourceMetadata"]["resultRef"] == source_metadata["resultRef"]
                        && previous["sourceMetadata"]["offset"]
                            .as_u64()
                            .zip(previous["sourceMetadata"]["nextOffset"].as_u64())
                            .zip(
                                source_metadata["offset"]
                                    .as_u64()
                                    .zip(source_metadata["nextOffset"].as_u64()),
                            )
                            .is_some_and(|((old_start, old_end), (start, end))| {
                                old_start <= start && old_end >= end
                            })
                })
            {
                let summary = state.context.history_sources.iter().rev().find_map(|source| {
                    let summary = &source["checkpointSemanticSummary"];
                    (summary["archive"]["verified"] == true
                        && summary["archive"]["resultRef"] == source_metadata["resultRef"])
                        .then_some(summary)
                });
                receipt["content"] =
                    repeated_read_projection(&canonical, &reference, None, summary);
            }
            if name == "task.document" && receipt["content"]["informationAdded"] == true {
                round_progress.artifact_updated = true;
            }
            // Sidecars retain the archived source identity even when repeated projections shrink.
            // 重复投影缩短时，侧车仍保留真实归档来源身份。
            state.context.result_sources.push(json!({"callId":call["id"],"name":call["name"],"arguments":call["arguments"],"sourceMetadata":source_metadata,"originalResultRef":if call["name"]=="tool.result.read" {source_metadata["resultRef"].clone()} else {Value::Null},"observation":is_observation,"executed":receipt["executed"]!=false,"originalExcerpt":crate::models::tool_context::source_excerpt(&canonical,512),"round":round,"status":receipt["status"],"resultRef":reference,"archiveOwner":{"requestId":run.request_id,"toolCallId":call["id"],"toolName":call["name"]}}));
            let _=state.tools.effects.apply("effects.observe",&json!({"call":call,"result":receipt,"observation":is_observation,"workspaceRoot":version_context.workdir}));
            if is_observation && receipt["isError"] != true {
                let records = native(state.tools.effects.apply("effects.state", &json!({})))?["unknown"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut proofs = tools::verify_unknown_effects(
                    &ctx,
                    &records,
                    &json!({"call":call,"result":receipt}),
                )
                .await?;
                for proof in &mut proofs {
                    if let Some(original) = records
                        .iter()
                        .find(|record| record["call"]["id"] == proof["toolCallId"])
                    {
                        let proof_root = runtime.config.data_root.clone();
                        let proof_chat = run.conversation_id.clone();
                        let proof_request = run.request_id.clone();
                        let original_call = original["call"].clone();
                        let value = proof["result"]["value"].clone();
                        let proof_ref = tokio::task::spawn_blocking(move || {
                            data::save_tool_result(
                                &proof_root,
                                &proof_chat,
                                &proof_request,
                                &original_call,
                                &value,
                                None,
                            )
                        })
                        .await
                        .map_err(|_| {
                            GatewayError::new("DATA_WORKER_FAILED", "核验回执保存失败。", 500)
                        })??;
                        proof["result"]["resultRef"] = proof_ref.clone();
                        proof["result"]["content"] = proof["result"]["value"].clone();
                        if let Some(activity) = state.assistant["ToolActivities"]
                            .as_array_mut()
                            .unwrap()
                            .iter_mut()
                            .find(|item| item["toolCallId"] == proof["toolCallId"])
                        {
                            activity["status"] = json!("completed");
                            activity["resultRef"] = proof_ref;
                            activity["result"] = json!(proof["result"]["content"].to_string());
                            activity["summary"] = json!("已核验派发状态");
                        }
                    }
                }
                if !proofs.is_empty() {
                    let _ = state.tools.effects
                        .apply("effects.resolve", &json!({"proofs":proofs}));
                }
            }
            let _=state.progress.apply("progress.recordTool",&json!({"id":call["id"],"round":round,"durationMs":tool_start.elapsed().as_millis() as u64,"approvalMs":0,"reused":receipt["reused"]==true,"dispatched":receipt["executed"]!=false}));
            let after_version = if track_version && !before_version.is_null() {
                // Keep execution receipts when verification fails or is cancelled; retain unknown verification state.
                // 核验失败或取消时保留执行回执，核验状态保持未知。
                match tools::capture_code_version(&version_context).await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        stopped = Some(error);
                        Value::Null
                    }
                }
            } else {
                Value::Null
            };
            if operation_dispatched && !is_observation {
                round_progress.artifact_updated |= super::super::gateway_tool_execution::artifact_version_changed(
                    call, &receipt, &before_version, &after_version);
            }
            // Compute completion coverage from settled originals, not shortened read projections.
            // 完成覆盖按已结算原文计算，不用缩短的读取投影。
            let acceptance_scope = if super::super::progress::validation_call(call) {
                let mut records = completion_review(state).records;
                records.push(completion_proof.clone());
                super::super::completion_evidence::validation_scope(
                    &state.completion.acceptance_requirements, &records,
                ).checkpoint()
            } else { Value::Null };
            let _=state.progress.apply("progress.observeOutcome",&json!({"call":call,"result":receipt,"canonicalResult":canonical,"beforeVersion":before_version,"afterVersion":after_version,
                "validationScope":{"cwd":version_context.workdir,"acceptanceScope":acceptance_scope}}));
            if completion_proof.is_object() && super::super::progress::validation_call(&call)
                && let Some(validation) = state.progress.verification()["receipts"].as_array().into_iter().flatten()
                    .find(|validation| validation["toolCallId"] == call["id"]) {
                completion_proof["validation"] = validation.clone();
            }
            let public = data::public_tool_result(
                &receipt["content"],
                Some(&reference),
                receipt["content"].get("executionEnvironment"),
                false,
            )?;
            let public_text = public.to_string();
            let public_text = if public_text.encode_utf16().count() > 65536 {
                json!({"resultRef":reference,"notice":"Result archived; use tool.result.read for bounded original pages.","preview":public_text.chars().take(8192).collect::<String>()}).to_string()
            } else {
                public_text
            };
            let activity = json!({"toolCallId":call["id"],"name":name,"arguments":call["arguments"],"status":receipt["status"],"summary":if receipt["isError"]==true{"执行受阻"}else{"已完成"},"result":public_text,"resultRef":reference,"round":round,"order":tool_order,"code":receipt["code"],"executed":receipt["executed"]!=false});
            if let Some(existing) = state.assistant["ToolActivities"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|item| item["toolCallId"] == call["id"])
            {
                *existing = activity.clone();
            } else {
                state.assistant["ToolActivities"]
                    .as_array_mut()
                    .unwrap()
                    .push(activity.clone());
            }
            run.emit(json!({"type":"tool_result","tool":activity}))
                .await;
            if name == "work.folder.bind" && receipt["status"] == "completed" {
                let mut rebound = runtime
                    .tool_context(
                        &run.conversation_id,
                        permission,
                        input["message"].as_str().unwrap(),
                        cancellation.clone(),
                    )
                    .await?;
                rebound.events = ctx.events.clone();
                rebound.external_execute = Some(specialist_executor(
                    &runtime,
                    &run,
                    &rebound,
                    permission,
                    input["message"].as_str().unwrap(),
                ));
                rebound.config["activeRequestId"] = json!(run.request_id);
                rebound.config["activeAssistantMessageId"] = state.assistant["Id"].clone();
                rebound.config["operationNamespace"] = state.assistant["OperationNamespace"].clone();
                rebound.config["renewOperationalExecutionBudget"] = ctx.config["renewOperationalExecutionBudget"].clone();
                rebound.config["nativeCancellationId"] = json!(run.cancellation_key);
                *ctx = rebound;
                state.broker_note(&format!("Broker: the formal work binding now resolves relative files within {}. This is the current authorized work scope; unrelated formal/private stores remain protected.",ctx.workdir.to_string_lossy()));
            }
            if name == "tool.load" {
                select_loaded_tools(&mut state, &receipt["content"], &call["arguments"]["names"]);
            }
            super::super::task::settle_unexecuted_tool_intent(&mut state.completion.review_checkpoint, name, &receipt);
            if name == "tool.search" && receipt["isError"] != true {
                // Prefer already loaded matches without loading new tools or granting permissions.
                // 优先保留已加载匹配项，不新增加载或授权。
                if let Some(index) = receipt["content"]["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|candidate| {
                        !declared_tools
                            .iter()
                            .any(|tool| tool["name"] == candidate["name"])
                    })
                    .find_map(|candidate| {
                        state.tools.selected
                            .iter()
                            .position(|tool| tool["name"] == candidate["name"])
                    })
                {
                    let loaded = state.tools.selected.remove(index);
                    state.tools.selected.insert(state.tools.selected.len().min(2), loaded);
                }
            }
            // Archive full originals; assign short references only to selected evidence without shrinking candidates.
            // 归档保留全文，仅选中证据分配短引用，不缩小候选池。
            if ["knowledge.search", "knowledge.plan"].contains(&name) && receipt["isError"] != true
            {
                if let Some(archive_id) = reference["id"].as_str().filter(|_| {
                    receipt["content"]["items"].is_array()
                        || receipt["content"]["structuredContent"]["items"].is_array()
                }) {
                    receipt["content"] = crate::retrieval::gateway::project_evidence_result(
                        &receipt["content"],
                        archive_id,
                    )?;
                }
            }
            if let Some(content) = receipt["content"].as_object_mut() {
                content.remove("fullText");
                content.remove("_meta");
            }
            let declared_read_only = ctx.runtime.schemas.read().await.iter().any(|descriptor|
                descriptor["name"] == name && descriptor["annotations"]["readOnlyHint"] == true);
            let scene_read_only = name.rsplit('.').next().is_some_and(|method|
                matches!(method, "get_scene_info" | "get_object_info" | "list_scene_items" | "get_world_state_snapshot"));
            let mutating = !is_observation && !declared_read_only && !scene_read_only && receipt["executed"] != false && receipt["reused"] != true && successful_completion_receipt(&receipt)
                && !name.starts_with("task.") && !matches!(name, "tool.load" | "context.compact");
            if mutating {
                state.completion.mutation_sequence = state.completion.mutation_sequence.saturating_add(1);
                state.completion.acceptance_reviewed = false;
                state.visual.visual_readback_delivered = false;
                state.visual.pending_visual_readback = false;
                state.visual.pending_visual_hashes.clear();
            }
            if successful_completion_receipt(&receipt) && super::super::task::receipt_has_visual_observation(&receipt["content"]) {
                state.visual.pending_visual_readback = true;
                state.visual.pending_visual_hashes = super::super::task::receipt_visual_hashes(&receipt["content"]);
            }
            if completion_proof.is_object() {
                completion_proof["mutationSequence"] = json!(state.completion.mutation_sequence);
                completion_proof["mutating"] = json!(mutating);
            }
            results.push(json!({"call":call,"result":receipt,"observation":is_observation,
                "mutationSequence":state.completion.mutation_sequence,"completionProof":completion_proof}));
            if results.last().unwrap()["result"]["status"] == "unknown" {
                let _ = state.tools.effects
                    .apply("effects.protect", &json!({"includeCurrent":true}));
            }
        }
        if let Some(transcript) = state.transcript.rounds.last_mut() {
            transcript["observations"]=json!(results.iter().map(|pair|json!({"id":pair["call"]["id"],"name":pair["call"]["name"],"status":pair["result"]["status"],"ref":pair["result"]["resultRef"],
                "verifiedSuccess":successful_completion_receipt(&pair["result"]),"mutationSequence":pair["mutationSequence"],
                "completionProof":pair["completionProof"],
                "toolIdentity":experience_identities[pair["call"]["name"].as_str().unwrap_or("")],
                "argumentsTemplate":crate::storage::experiences::procedure_arguments(&pair["call"]["arguments"]),
                "verifiedReadback":successful_completion_receipt(&pair["result"]) && super::super::task::receipt_is_readback(pair["call"]["name"].as_str().unwrap_or(""), &pair["result"]["content"]),
                "verifiedVisualReadback":successful_completion_receipt(&pair["result"]) && super::super::task::receipt_has_visual_observation(&pair["result"]["content"]),
                "completionEvidence":completed_output_paths(pair["call"]["name"].as_str().unwrap_or(""), &pair["result"])})).collect::<Vec<_>>());
        }
        state.context.history =
            protocol::append_results(&protocol_name, &state.context.history, &response, &results)?;
        round_progress.completed_source_count = analyzed_document_count(&state.documents);
        let previous_stagnation = state.work_progress.stagnant_rounds();
        let outcome = crate::orchestration::gateway_tool_execution::observe_progress_round(
            &mut state.work_progress, &mut state.tools.observations, round_progress, &results,
        )?;
        let work_plan = outcome.plan;
        let guard = outcome.observation;
        let added = work_plan.no_progress_rounds.saturating_sub(previous_stagnation);
        if added > 0 { let _ = state.progress.apply("progress.noProgress", &json!({"count":added})); }
        if stopped.is_none() {
            stopped = crate::orchestration::gateway_tool_execution::progress_interruption(&work_plan);
        }
        if work_plan.correction_due {
            state.broker_note(&format!("Work stalled: {}. Use saved analyses to produce the requested artifact; stop repeating directory or capability listings. Read only a specific missing saved fact. Report the actual output and remaining gap.",serde_json::to_value(&work_plan).unwrap_or(Value::Null)));
        }
        if !state.documents.directory()["documents"]
            .as_array()
            .is_none_or(|entries| entries.is_empty())
        {
            if let Some(tool) = all.iter().find(|tool| tool["name"] == "task.document") {
                if !state.tools.selected
                    .iter()
                    .any(|selected| selected["name"] == "task.document")
                {
                    state.tools.selected.insert(
                        state.tools.selected.len().min(2),
                        descriptor(tool, "task.document"),
                    );
                }
            }
            refresh_document_navigation(&mut state);
        }
        if guard["warning"] == true || guard["finalize"] == true {
            // Keep corrective guidance through compaction without appending permanent user turns.
            // 压缩后保留纠偏指导，不不断追加永久用户轮次。
            if !system.contains(REPEATED_READ_GUIDANCE) {
                system.push_str(REPEATED_READ_GUIDANCE);
            }
        }
        state.save(&runtime, &run, "streaming", None).await?;

        Ok(ToolRoundOutcome { stopped, review_invalidated })
    }
}
