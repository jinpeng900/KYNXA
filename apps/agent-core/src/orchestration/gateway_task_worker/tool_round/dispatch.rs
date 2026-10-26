//! Child dispatch chooses existing outcomes before executing authorized work.
//! 子派发优先复用已有结果，再执行当前授权操作。

use super::*;

pub(super) struct PreparedCall {
    pub context: GatewayToolContext,
    pub observation: bool,
    pub before: Value,
    pub can_use_workspace: bool,
    pub failure_definition: Value,
    pub failure_environment: Value,
    selection_error: Option<GatewayError>,
    control_correction: Option<Value>,
    failure_correction: Option<Value>,
    protected: Value,
    previous: Option<Value>,
    // Hold mutation exclusion through the receipt and post-execution version checkpoint.
    // 修改互斥持续到回执保存及执行后版本检查点结束。
    _workspace_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

pub(super) async fn prepare(round: &mut ToolRound<'_>, call: &Value, prefetched: bool) -> Result<PreparedCall> {
    let runtime = round.runtime;
    let journal = round.journal;
    let child = round.child;
    let ctx = round.ctx;
    let state = &mut *round.state;
    let effects = &mut *round.effects;
    let catalog = &*round.catalog;
    let name = call["name"].as_str().unwrap_or("");
    let control_correction = task::native_task_control_receipt(call, &state.acceptance_requirements);
    let observation = control_correction.is_some() || crate::tools::policy::is_read_only_call(name,&call["arguments"]);
    let selection = if tools::workspace_path_tool(name,&call["arguments"]) {
        tools::select_workspace_context(&ctx,&call["arguments"]).await
    } else { Ok(ctx.clone()) };
    let selection_error = selection.as_ref().err().cloned();
    let can_use_workspace = selection_error.is_none();
    let mut call_context = selection.unwrap_or_else(|_|ctx.clone());
    // Preserve the original delegation root; mount selection cannot reinterpret relative write grants.
    // 保留原委派根目录，切换挂载不能重解释相对写权限。
    call_context.authorized_roots = ctx.authorized_roots.clone();
    let ctx = &mut call_context;
    if journal.refresh_ancestor_work_scope(ctx, &child.goal).await?.is_some() {
        state.memory_plan_invalidated = true;
    }
    let workspace_guard = if !state.memory_plan_invalidated && !observation && !name.starts_with("task.") && selection_error.is_none() {
        Some(runtime.tasks.workspace_mutation_guard(&ctx.workdir).await?)
    } else { None };
    let before = if selection_error.is_none() && (name.starts_with("terminal.")
        || name.starts_with("filesystem.") && !observation)
    {
        tools::capture_code_version(&ctx).await?
    } else {
        Value::Null
    };
    let failure_definition = gateway_tool_execution::tool_failure_definition(call, &catalog, &[]);
    let mut failure_environment = gateway_tool_execution::tool_failure_environment(ctx,
        if before.is_null() { &state.code_version } else { &before }, child_mutation_revision(state));
    failure_environment["configuration"]["acceptanceRequirements"] = json!(state.acceptance_requirements);
    failure_environment["operationIdentity"] = json!(operation_intent(call, observation, state.rounds).operation_id);
    let failure_correction = state.deterministic_failures.before_dispatch(call, &failure_definition, &failure_environment);
    let protected = effects
        .apply(
            "effects.previous",
            &json!({"call":call,"observation":observation}),
        )
        .map_err(|code| {
            failure(
                &code,
                "Protected execution receipt is unavailable. / 受保护执行回执不可用。",
            )
        })?;
    let previous=if !prefetched{previous_operation(journal,call,state.rounds).await?}else{None};
    Ok(PreparedCall {context: call_context, observation, before, can_use_workspace,
        failure_definition, failure_environment, selection_error, control_correction,
        failure_correction, protected, previous, _workspace_guard: workspace_guard})
}

pub(super) async fn execute(round: &mut ToolRound<'_>, call: &Value, prepared: &mut PreparedCall,
    early_approval: Option<(Value, u64)>) -> Result<Value> {
    let runtime = round.runtime;
    let parent = round.parent;
    let run = round.run;
    let journal = round.journal;
    let child = round.child;
    let input = round.input;
    let state = &mut *round.state;
    let name = call["name"].as_str().unwrap_or("");
    let ctx = &mut prepared.context;
    let observation = prepared.observation;
    let control_correction = prepared.control_correction.take();
    let selection_error = prepared.selection_error.take();
    let failure_correction = prepared.failure_correction.take();
    let previous = prepared.previous.take();
    let protected = &prepared.protected;
    let receipt = if let Some(control) = control_correction {
        control
    } else if state.memory_plan_invalidated {
        if let Some(previous)=previous {previous} else {
            let skipped=gateway_tool_execution::tool_execution_receipt(Ok(gateway_tool_execution::memory_replanning_result()));
            if early_approval.is_some() {
                let skipped=archive_receipt(runtime,run,call,skipped).await?;
                let reference=journal.save_blob("worker-log",&serde_json::to_vec(&skipped).map_err(|_|failure("INVALID_TASK_RECEIPT","Memory change receipt cannot be encoded. / 记忆变更回执无法编码。"))?).await?;
                journal.finish_operation(&operation_intent(call,observation,state.rounds).operation_id,OperationState::Failed,Some(reference),None)?;
                skipped
            }else{skipped}
        }
    } else if let Some(error) = selection_error {
        error_receipt(&error, false)
    } else if let Err(error) = check_write_target(&ctx, input, call) {
        error_receipt(&error, false)
    } else if let Some(previous) = previous {
        previous
    } else if !protected.is_null() {
        let mut receipt = protected["result"].clone();
        receipt["reused"] = json!(true);
        receipt
    } else if early_approval.is_none() && failure_correction.is_some() {
        failure_correction.unwrap()
    } else if call["unavailable"] == true {
        error_receipt(
            &failure(
                "TOOL_NOT_LOADED",
                "Discover and load the real definition before execution. / 执行前发现并加载真实定义。",
            ),
            false,
        )
    } else {
        let mut intent = operation_intent(call, observation, state.rounds);
        if tools::workspace_path_tool(name,&call["arguments"]) {
            intent.input_reference = Some(json!({"mountBinding":tools::operation_mount_binding(&ctx,&call["arguments"])?}));
        }
        if let Some(previous) = journal.repository().operation(journal.task_id(),&intent.operation_id)? {
            if previous.state == OperationState::Pending && previous.intent.input_reference.as_ref().is_none_or(|reference|reference.get("mountBinding").is_none()) {
                intent.input_reference = previous.intent.input_reference;
            }
        }
        let already_dispatched = early_approval.is_some();
        let recorded = if already_dispatched {Ok(())}else{journal.record_operation(&intent).map(|_|())};
        if let Err(error) = recorded {
            error_receipt(&error, false)
        } else {
            if !already_dispatched { journal.dispatch_with_ancestor_scope(&intent.operation_id, ctx)?; }
            ctx.config["activeToolCallId"] = call["id"].clone();
            ctx.config["activeOperationId"] = json!(intent.operation_id);
            ctx.config["acceptanceRequirements"] = json!(state.acceptance_requirements);
            if name.starts_with("task.") || name.starts_with("knowledge.") {
                ctx.external_execute = Some(gateway_tool_execution::specialist_executor(runtime, run, &ctx,
                    &ctx.permission_mode, ctx.config["trustedUserText"].as_str().unwrap_or("")));
            }
            if !already_dispatched { parent
                .emit(
                    json!({"type":"task_tool_call","taskId":child.task_id,"tool":call}),
                )
                .await; }
            let actual_tool_started = Instant::now();
            let (result,initial_read_ms) = match early_approval {
                Some((value,milliseconds))=>(Ok(value),milliseconds),
                None=>(tools::execute(name,call["arguments"].clone(),&ctx).await,0),
            };
            let mut tool_duration_ms = actual_tool_started
                .elapsed()
                .as_millis()
                .min(u64::MAX as u128)
                as u64;
            tool_duration_ms=tool_duration_ms.saturating_add(initial_read_ms);
            let result = resolve_approval(round, ctx, call, &intent, result, &mut tool_duration_ms).await?;
            let receipt = match result {
                Ok(value) => {
                    let unknown = value["status"] == "unknown"
                        || value["outcome"] == "unknown"
                        || value["code"]
                            .as_str()
                            .is_some_and(|code| code.contains("UNKNOWN"));
                    json!({"status":if unknown {"unknown"} else if value["isError"]==true {"error"} else {"completed"},"isError":unknown||value["isError"]==true,"executed":value["executed"],"code":value["code"],"recoveryHandle":value["recoveryHandle"],"content":value})
                }
                Err(error) => error_receipt(&error, false),
            };
            journal.repository().record_usage(
                journal.token(),
                &format!(
                    "tool-time-{}-{}",
                    journal.token().attempt_id,
                    intent.operation_id
                ),
                &crate::storage::task_repository::TaskUsage {
                    tool_duration_ms,
                    ..Default::default()
                },
            )?;
            let receipt = archive_receipt(runtime, run, call, receipt).await?;
            let owned = journal
                .save_blob(
                    "worker-log",
                    &serde_json::to_vec(&receipt).map_err(|_| {
                        failure(
                            "INVALID_TASK_RECEIPT",
                            "Receipt encoding failed. / 回执编码失败。",
                        )
                    })?,
                )
                .await?;
            journal.finish_operation(
                &intent.operation_id,
                gateway_tool_execution::operation_outcome(&receipt),
                Some(owned),
                None,
            )?;
            receipt
        }
    };
    Ok(receipt)
}

async fn resolve_approval(round: &mut ToolRound<'_>, ctx: &mut GatewayToolContext,
    call: &Value, intent: &OperationIntent, result: Result<Value>, tool_duration_ms: &mut u64)
    -> Result<Result<Value>> {
    let runtime = round.runtime;
    let parent = round.parent;
    let run = round.run;
    let journal = round.journal;
    let child = round.child;
    let limits = round.limits;
    let started = round.started;
    let state = &mut *round.state;
    let live_memory = &mut *round.live_memory;
    let system = &mut *round.system;
    let name = call["name"].as_str().unwrap_or("");
    let resolved = match result {
        Ok(value) if value["requiresApproval"] == true => {
            if runtime
                .await_child_approval(
                    parent,
                    run,
                    call,
                    &value,
                    state.rounds,
                    state.tool_calls,
                    started
                        + Duration::from_millis(run.resource_wait_ms.load(std::sync::atomic::Ordering::Relaxed))
                        + Duration::from_millis(
                            limits["maxDurationMs"]
                                .as_u64()
                                .unwrap_or(1_800_000),
                        ),
                )
                .await?
            {
                if let Some(status)=live_memory.refresh(&runtime.config.data_root,&run.conversation_id,&run.request_id,system).await? {
                    invalidate_child_memory_plan(state,&status);
                    Ok(gateway_tool_execution::memory_replanning_result())
                }else{
                journal.refresh_ancestor_work_scope(ctx, &child.goal).await?;
                if ctx.config["currentWorkScope"]["ancestorControlPending"] == true
                    || !task::scope_allows_call(&ctx.config["currentWorkScope"], name,
                    crate::tools::policy::is_read_only_call(name, &call["arguments"])) {
                    let error = failure("TASK_PARENT_SCOPE_CHANGED", "Parent scope changed while approval waited; no new action was dispatched. / 等待审批时父级范围变化，未派发新操作。");
                    let receipt = archive_receipt(runtime,run,call,error_receipt(&error,false)).await?;
                    let reference = journal.save_blob("worker-log",&serde_json::to_vec(&receipt)
                        .map_err(|_|failure("INVALID_TASK_RECEIPT","Blocked child receipt cannot be encoded. / 子任务未派发回执无法编码。"))?).await?;
                    journal.finish_operation(&intent.operation_id,OperationState::Failed,Some(reference),None)?;
                    return Err(error);
                }
                ctx.approved = true;
                ctx.config["approvedInvocation"] = value["invocation"].clone();
                let approved_tool_started = Instant::now();
                let executed =
                    tools::execute(name, call["arguments"].clone(), &ctx).await;
                *tool_duration_ms = tool_duration_ms.saturating_add(
                    approved_tool_started
                        .elapsed()
                        .as_millis()
                        .min(u64::MAX as u128)
                        as u64,
                );
                ctx.approved = false;
                ctx.config
                    .as_object_mut()
                    .unwrap()
                    .remove("approvedInvocation");
                executed
                }
            } else {
                Err(GatewayError::new(
                    "TOOL_APPROVAL_DENIED",
                    "Child operation was not approved. / 子操作未获批准。",
                    403,
                ))
            }
        }
        value => value,
    };
    Ok(resolved)
}
