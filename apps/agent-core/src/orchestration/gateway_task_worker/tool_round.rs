//! Child tool rounds own scoped dispatch, durable receipts and protocol pairing.
//! 子任务工具阶段负责范围内派发、持久回执及协议配对。

use super::*;

mod dispatch;
mod receipts;

pub(super) struct ToolRound<'a> {
    pub runtime: &'a Arc<GatewayRuntime>,
    pub parent: &'a Arc<RequestRun>,
    pub run: &'a Arc<RequestRun>,
    pub journal: &'a Arc<TaskJournal>,
    pub child: &'a TaskRecord,
    pub ctx: &'a GatewayToolContext,
    pub input: &'a Value,
    pub limits: &'a Value,
    pub duration_ms: u64,
    pub started: Instant,
    pub response: &'a protocol::ModelResponse,
    pub connection: &'a Value,
    pub state: &'a mut WorkerContext,
    pub effects: &'a mut EffectLedger,
    pub observations: &'a mut ObservationGuard,
    pub catalog: &'a mut Vec<Value>,
    pub live_memory: &'a mut LiveMemoryProjection,
    pub system: &'a mut String,
}

pub(super) async fn execute(mut round: ToolRound<'_>) -> Result<()> {
    let response = round.response;
    let saved_results = round.state.pending_results.len();
    if !response.calls.is_empty() {
        round.state.review_pending = false;
        round.state.acceptance_reviewed = false;
    }
    let batches = crate::orchestration::gateway_tool_batch::plan_with_catalog(&response.calls, round.catalog);
    let mut early_approvals = std::collections::HashMap::new();
    for index in round.state.pending_results.len()..response.calls.len() {
        if round.run.effective_active_ms().saturating_sub(round.state.segment_active_base_ms) >= round.duration_ms {
            if round.input["runLimits"]["maxDurationMs"].as_u64().is_some() {
                return Err(failure("TOOL_RUN_TIMEOUT", "The explicit user execution budget is reached; progress is preserved. / 已到用户明确执行额度，进度仍保留。"));
            }
            round.state.segment_active_base_ms = round.run.effective_active_ms();
            checkpoint(round.run, round.journal, round.state, "duration-segment-renewed").await?;
        }
        if *round.parent.cancellation.borrow() || *round.run.cancellation.borrow() {
            return Err(GatewayError::new("ABORT_ERR", "Child cancelled before tool dispatch. / 子任务派发前已取消。", 499));
        }
        round.journal.check_dispatch_fence()?;
        if let Some(status) = round.live_memory.refresh(&round.runtime.config.data_root,
            &round.run.conversation_id, &round.run.request_id, round.system).await? {
            invalidate_child_memory_plan(round.state, &status);
        }
        if let Some(batch) = batches.iter().find(|batch|batch.start == index && !round.state.memory_plan_invalidated) {
            let count = batch.end - batch.start;
            if round.state.tool_calls.saturating_sub(round.state.segment_tool_base).saturating_add(count as u64)
                <= round.limits["maxToolCalls"].as_u64().unwrap_or(256) {
                let calls = batch.clone().map(|index|(index,response.calls[index].clone())).collect();
                early_approvals.extend(prefetch_child_reads(round.runtime, round.parent, round.run,
                    round.journal, round.ctx, round.input, &round.child.goal, calls,
                    round.state.rounds, round.state, round.catalog).await?);
            }
        }
        if round.state.tool_calls.saturating_sub(round.state.segment_tool_base)
            >= round.limits["maxToolCalls"].as_u64().unwrap_or(256) {
            round.state.segment_tool_base = round.state.tool_calls;
            checkpoint(round.run, round.journal, round.state, "tool-segment-renewed").await?;
        }
        let call = &response.calls[index];
        let early_approval = early_approvals.remove(&index);
        let mut prepared = dispatch::prepare(&mut round, call, early_approval.is_some()).await?;
        let receipt = dispatch::execute(&mut round, call, &mut prepared, early_approval).await?;
        receipts::settle(&mut round, call, &mut prepared, receipt).await?;
    }
    let results = round.state.pending_results[saved_results..].to_vec();
    let facts = RoundProgress {
        work_calls: results.len() as u64,
        discovery_calls: results.iter().filter(|pair|matches!(pair["call"]["name"].as_str(),
            Some("tool.search" | "tool.load"))).count() as u64,
        source_reads: results.iter().filter(|pair|pair["observation"] == true
            && !matches!(pair["call"]["name"].as_str(),Some("tool.search" | "tool.load"))).count() as u64,
        artifact_updated: round.state.receipts.iter().rev().take(results.len()).any(|saved|
            saved["mutating"] == true && gateway_tool_execution::artifact_version_changed(
                &saved["call"], &saved["result"], &saved["beforeVersion"], &saved["afterVersion"])),
        ..Default::default()
    };
    let progress = gateway_tool_execution::observe_progress_round(&mut round.state.work_progress,
        round.observations, facts, &results)?;
    if progress.plan.correction_due {
        round.state.history.push(json!({"role":"user","content":"Broker: unchanged operations added no new evidence. Reuse saved results; try a genuinely missing source/range or advance the next unmet requirement. Do not replay successful work or repeat the same result."}));
    }
    receipts::append_results(&mut round).await?;
    checkpoint(round.run, round.journal, round.state, "tool-round-paired").await?;
    if let Some(error) = gateway_tool_execution::progress_interruption(&progress.plan) {return Err(error);}
    Ok(())
}

pub(super) fn child_tool_allowed(name: &str) -> bool {
    (!name.starts_with("task.") || ["task.read", "task.result", "task.delegate", "task.status", "task.wait", "task.control", "task.describe"].contains(&name))
        && name != "work.folder.bind"
}

/// Reason and Skill/MCP text cannot expand write scope; opaque commands need the authorized workspace.
/// reason 和 Skill/MCP 描述不扩大写权限；不透明命令须声明授权工作区。
pub(super) fn check_write_target(ctx: &GatewayToolContext, input: &Value, call: &Value) -> Result<()> {
    let name = call["name"].as_str().unwrap_or("");
    if !child_tool_allowed(name) {
        return Err(failure(
            "CHILD_SCOPE_CHANGE_BLOCKED",
            "Child agents cannot delegate again or change the work binding. / 子代理不能再次委派或修改工作归属。",
        ));
    }
    if gateway_tool_execution::is_read_only_tool(name) || name.starts_with("task.") && child_tool_allowed(name) {
        return Ok(());
    }
    let declared = input["writeTargets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| {
            let path = PathBuf::from(value);
            let absolute = if path.is_absolute() {
                path
            } else {
                ctx.workdir.join(path)
            };
            local_files::absolute_path(&absolute.to_string_lossy())
        })
        .collect::<Vec<_>>();
    if declared.iter().any(|path| {
        !ctx.authorized_roots
            .iter()
            .any(|root| local_files::within(root, path))
    }) {
        return Err(failure(
            "CHILD_WRITE_SCOPE_BLOCKED",
            "Declared target exceeds the parent's scope. / 声明目标超出父任务范围。",
        ));
    }
    if ["filesystem.write", "filesystem.edit", "filesystem.delete", "document.create"].contains(&name) {
        let path = call["arguments"]["path"].as_str().ok_or_else(|| {
            failure(
                "INVALID_CHILD_WRITE_TARGET",
                "File mutation requires an explicit path. / 文件修改须给出明确路径。",
            )
        })?;
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            let mount = tools::operation_mount_binding(ctx, &call["arguments"])?;
            let root = mount["path"].as_str().map(PathBuf::from).unwrap_or_else(|| ctx.workdir.clone());
            root.join(path)
        };
        let path = local_files::absolute_path(&path.to_string_lossy());
        local_files::inspect(&path, true, false).map_err(|_| {
            failure(
                "CHILD_WRITE_SCOPE_BLOCKED",
                "File target is redirected or cannot be validated. / 文件目标被重定向或无法核验。",
            )
        })?;
        let permitted = declared.iter().any(|target| {
            local_files::path_key(target) == local_files::path_key(&path)
                || target.is_dir() && local_files::within(target, &path)
        });
        if permitted {
            return Ok(());
        }
    } else if declared
        .iter()
        .any(|target| local_files::path_key(target) == local_files::path_key(&ctx.workdir))
    {
        return Ok(());
    }
    Err(failure(
        "CHILD_WRITE_TARGET_REQUIRED",
        "Mutation was not delegated to this target; obtain a narrower task or explicit workspace write target. / 当前目标未获委派修改权限，须调整子任务或明确工作区写入目标。",
    ))
}

pub(super) fn operation_intent(call: &Value, observation: bool, round: u64) -> OperationIntent {
    let encoded = serde_json::to_vec(&json!({"name":call["name"],"arguments":call["arguments"]}))
        .unwrap_or_default();
    OperationIntent {
        operation_id: format!(
            "round-{round}-child-{}",
            call["id"].as_str().unwrap_or("invalid")
        ),
        tool_call_id: call["id"].as_str().unwrap_or("").to_owned(),
        tool_name: call["name"].as_str().unwrap_or("").to_owned(),
        input_sha256: managed_files::digest(&encoded),
        input_reference: None,
        effect: if observation {
            OperationEffect::Observation
        } else if ["filesystem.write", "filesystem.edit", "filesystem.delete", "document.create"]
            .contains(&call["name"].as_str().unwrap_or(""))
        {
            OperationEffect::FileMutation
        } else {
            OperationEffect::Irreversible
        },
    }
}

fn error_receipt(error: &GatewayError, executed: bool) -> Value {
    json!({"status":if error.code.contains("UNKNOWN") {"unknown"} else {"error"},"isError":true,"executed":executed,"code":error.code,"content":{"code":error.code,"error":error.message}})
}

pub(super) async fn archive_receipt(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    call: &Value,
    mut receipt: Value,
) -> Result<Value> {
    let root = runtime.config.data_root.clone();
    let chat = run.conversation_id.clone();
    let request = run.request_id.clone();
    let saved_call = call.clone();
    let canonical = receipt["content"].clone();
    let environment = canonical.get("executionEnvironment").cloned();
    let reference = tokio::task::spawn_blocking(move || {
        data::save_tool_result(
            &root,
            &chat,
            &request,
            &saved_call,
            &canonical,
            environment.as_ref(),
        )
    })
    .await
    .map_err(|_| {
        failure(
            "TASK_RESULT_SAVE_FAILED",
            "Child tool result writer stopped. / 子任务回执写入中止。",
        )
    })??;
    receipt["resultRef"] = reference;
    Ok(receipt)
}

pub(super) fn effect_receipt(receipt: &Value) -> Value {
    let mut projected = receipt.clone();
    // Recovered dispatch remains unknown; executed:false means this attempt did not replay it.
    // 恢复的原派发仍未知，executed:false 仅表示本次未重放。
    if projected["status"] == "unknown" && projected["historicalDispatch"] == true {
        if let Some(object) = projected.as_object_mut() {
            object.remove("executed");
        }
    }
    projected
}

/// Batch authorized independent reads; settle each receipt before ordered projection.
/// 批量执行已授权独立读取，逐个结算后按序投影。
async fn prefetch_child_reads(
    runtime:&Arc<GatewayRuntime>, parent:&Arc<RequestRun>, run:&Arc<RequestRun>, journal:&Arc<TaskJournal>,
    ctx:&GatewayToolContext,input:&Value,goal:&str,calls:Vec<(usize,Value)>,round:u64,state:&WorkerContext,catalog:&[Value],
) -> Result<std::collections::HashMap<usize,(Value,u64)>> {
    let mut prepared = Vec::new();
    let mut action_keys = std::collections::HashSet::new();
    for (index,call) in calls {
        let name = call["name"].as_str().unwrap_or("");
        if !(crate::tools::policy::is_read_only_call(name,&call["arguments"]) || name.starts_with("mcp."))
            || name.starts_with("mcp.") && ctx.permission_mode!="full"
            || check_write_target(ctx,input,&call).is_err() || previous_operation(journal,&call,round).await?.is_some() { continue; }
        let mut selected = if tools::workspace_path_tool(name,&call["arguments"]) {
            match tools::select_workspace_context(ctx,&call["arguments"]).await { Ok(value)=>value,Err(_)=>continue }
        } else { ctx.clone() };
        selected.authorized_roots = ctx.authorized_roots.clone();
        selected.config["acceptanceRequirements"] = json!(state.acceptance_requirements);
        let definition = gateway_tool_execution::tool_failure_definition(&call, catalog, &[]);
        let environment = gateway_tool_execution::tool_failure_environment(&selected, &state.code_version, child_mutation_revision(state));
        if state.deterministic_failures.before_dispatch(&call, &definition, &environment).is_some()
            || !action_keys.insert(state.deterministic_failures.action_key(&call, &definition, &environment)) { continue; }
        prepared.push((index,call,selected));
    }
    if prepared.len()<2 { return Ok(Default::default()); }
    let results = crate::orchestration::gateway_tool_batch::run_batch(&run.resource_wait_ms,prepared,
        |_|crate::orchestration::gateway_tool_batch::admit_parallel_read(ctx,journal.task_id(),run.cancellation.subscribe()),
        |(index,call,mut context)|async move {
            let result = async {
                if *run.cancellation.borrow() || *parent.cancellation.borrow() { return Err(failure("ABORT_ERR","Child read cancelled before dispatch. / 子读取派发前已停止。")); }
                journal.check_dispatch_fence()?;
                let changed = journal.refresh_ancestor_work_scope(&mut context, goal).await?.is_some();
                let name = call["name"].as_str().unwrap_or("");
                let read_only = crate::tools::policy::is_read_only_call(name, &call["arguments"]);
                if context.config["currentWorkScope"]["ancestorControlPending"] == true
                    || !task::scope_allows_call(&context.config["currentWorkScope"], name, read_only) || changed && !read_only {
                    return Err(failure("TASK_PARENT_SCOPE_CHANGED", "Parent instructions changed before child dispatch. / 子派发前父级要求已变化，旧调用未执行。"));
                }
                // Concurrency hints do not change effect classification or authorize replay.
                // 并发提示不改变副作用分类，也不授权重放。
                let mut intent = operation_intent(&call,crate::tools::policy::is_read_only_call(call["name"].as_str().unwrap_or(""),&call["arguments"]),round);
                let name = call["name"].as_str().unwrap_or("");
                if tools::workspace_path_tool(name,&call["arguments"]) {
                    intent.input_reference=Some(json!({"mountBinding":tools::operation_mount_binding(&context,&call["arguments"])?}));
                }
                if let Some(previous) = journal.repository().operation(journal.task_id(),&intent.operation_id)? {
                    if previous.state==OperationState::Pending && previous.intent.input_reference.as_ref().is_none_or(|value|value.get("mountBinding").is_none()) { intent.input_reference=previous.intent.input_reference; }
                }
                journal.record_operation(&intent)?;
                journal.dispatch_with_ancestor_scope(&intent.operation_id, &context)?;
                context.config["activeToolCallId"]=call["id"].clone();
                context.config["activeOperationId"]=json!(intent.operation_id);
                if name.starts_with("knowledge.") {
                    context.external_execute=Some(gateway_tool_execution::specialist_executor(runtime,run,&context,
                        &context.permission_mode,context.config["trustedUserText"].as_str().unwrap_or("")));
                }
                parent.emit(json!({"type":"task_tool_call","taskId":journal.task_id(),"tool":call})).await;
                let started=Instant::now();
                let executed=tools::execute(name,call["arguments"].clone(),&context).await;
                let milliseconds=started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                if let Ok(value)=&executed { if value["requiresApproval"]==true { return Ok(Some((value.clone(),milliseconds))); } }
                let receipt = gateway_tool_execution::tool_execution_receipt(executed);
                let receipt = archive_receipt(runtime,run,&call,receipt).await?;
                let reference=journal.save_blob("worker-log",&serde_json::to_vec(&receipt).map_err(|_|failure("INVALID_TASK_RECEIPT","Child read receipt cannot be encoded. / 子读取回执无法编码。"))?).await?;
                journal.repository().record_usage(journal.token(),&format!("tool-time-{}-{}",journal.token().attempt_id,intent.operation_id),
                    &crate::storage::task_repository::TaskUsage{tool_duration_ms:milliseconds,..Default::default()})?;
                journal.finish_operation(&intent.operation_id,gateway_tool_execution::operation_outcome(&receipt),Some(reference),None)?;
                Ok::<_,GatewayError>(None)
            }.await;
            (index,result)
        }, |(index,_,_),error|(index,Err(error))).await;
    let mut approvals=std::collections::HashMap::new();
    for (index,result) in results { if let Some(approval)=result? { approvals.insert(index,approval); } }
    Ok(approvals)
}

/// Recover outcomes from the repository, not the model's recollection.
/// 恢复结果以仓储为准，不采信模型回忆。
pub(super) async fn previous_operation(
    journal: &Arc<TaskJournal>,
    call: &Value,
    round: u64,
) -> Result<Option<Value>> {
    let id = operation_intent(call, false, round).operation_id;
    let operation = journal
        .repository()
        .operations(journal.task_id())?
        .into_iter()
        .find(|operation| operation.intent.operation_id == id);
    let Some(operation) = operation else {
        return Ok(None);
    };
    if operation.intent.input_sha256 != operation_intent(call, false, round).input_sha256
        || operation.intent.tool_name != call["name"].as_str().unwrap_or("")
    {
        return Err(failure(
            "TASK_OPERATION_ID_CONFLICT",
            "A tool call identity cannot refer to different inputs. / 工具调用身份不能指向不同参数。",
        ));
    }
    if operation.state == OperationState::Pending {
        return Ok(None);
    }
    if matches!(
        operation.state,
        OperationState::Succeeded | OperationState::Failed
    ) {
        let reference = operation.result_reference.ok_or_else(|| {
            failure(
                "TASK_RECEIPT_MISSING",
                "Settled operation lost its receipt. / 已结算操作缺少回执。",
            )
        })?;
        let bytes = journal.read_blob(&reference).await?;
        let mut receipt: Value = serde_json::from_slice(&bytes).map_err(|_| {
            failure(
                "INVALID_TASK_RECEIPT",
                "Stored tool receipt is invalid. / 已存工具回执无效。",
            )
        })?;
        receipt["reused"] = json!(true);
        return Ok(Some(receipt));
    }
    Ok(Some(
        json!({"status":"unknown","isError":true,"executed":false,"historicalDispatch":true,"code":"EXECUTION_UNCONFIRMED","content":{"notice":"A previous attempt dispatched this operation without a settled receipt. Verify its actual state; it will not be replayed.","operationId":id,"toolCallId":call["id"]}}),
    ))
}
