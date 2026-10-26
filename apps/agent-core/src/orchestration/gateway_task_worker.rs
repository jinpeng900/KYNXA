//! Independent delegated execution reuses the native model, tools and durable operation repository.
//! 独立委派执行复用原生模型、工具和持久操作仓储；不向父聊天伪造消息或复制权限。

use super::{
    budget, completion_evidence,
    gateway_model_request::{self, ModelRequestOutcome},
    gateway_live_memory::LiveMemoryProjection,
    gateway_runtime::{GatewayRuntime, RequestRun},
    gateway_task_journal::TaskJournal,
    gateway_tool_execution,
    progress::RunProgress,
    gateway_work_progress::{DeterministicFailures, RoundProgress, WorkProgress},
    task::{self, TaskState as SemanticTask},
};
use crate::{
    gateway_contract::{GatewayError, new_id},
    models::{gateway_config, gateway_protocol as protocol},
    storage::{
        gateway as data, gateway_memory, managed_files,
        task_repository::{
            OperationEffect, OperationIntent, OperationState, TaskCheckpoint, TaskRecord, TaskState,
        },
    },
    tools::{
        effects::EffectLedger,
        gateway::{self as tools, GatewayToolContext},
        local_files,
        observations::ObservationGuard,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

mod compaction;
mod model_round;
mod preparation;
mod completion;
mod tool_round;

use compaction::compact_child;
use completion::{ChildCompletionReview, restore_completion_proofs, review_instruction, verified_completion_receipt};
use tool_round::{child_tool_allowed, effect_receipt};
type Result<T> = std::result::Result<T, GatewayError>;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerContext {
    history: Vec<Value>,
    #[serde(default)]
    history_sources: Vec<Value>,
    pending: Option<protocol::ModelResponse>,
    pending_results: Vec<Value>,
    receipts: Vec<Value>,
    result_sources: Vec<Value>,
    output: String,
    #[serde(default)]
    final_summary: String,
    rounds: u64,
    #[serde(default)]
    segment_round_base: u64,
    #[serde(default)]
    segment_tool_base: u64,
    #[serde(default)]
    segment_generated_base: u64,
    #[serde(default)]
    segment_active_base_ms: u64,
    tool_calls: u64,
    generated_tokens: u64,
    compactions: u64,
    continuations: u64,
    code_version: Value,
    verification: Vec<Value>,
    changed_files: Vec<Value>,
    #[serde(default)]
    loaded_tools: Vec<Value>,
    /// In-memory request series; a resumed child simply starts a new one.
    /// 仅在内存中的请求序列；恢复后的子任务重新开始序列。
    #[serde(skip)]
    tool_series: crate::models::tool_updates::ToolSeries,
    #[serde(default)]
    acceptance_requirements: Vec<String>,
    #[serde(default)]
    review_pending: bool,
    #[serde(default)]
    pending_review: bool,
    #[serde(default)]
    review_attempts: u64,
    #[serde(default)]
    acceptance_reviewed: bool,
    #[serde(default)]
    acceptance_review: Value,
    #[serde(default)]
    review_checkpoint: Value,
    #[serde(default)]
    last_parent_revision: u64,
    #[serde(default)]
    memory_plan_invalidated: bool,
    #[serde(default)]
    deterministic_failures: DeterministicFailures,
    #[serde(default)]
    work_progress: WorkProgress,
}

pub(super) struct ChildAcceptance {
    pub query: String,
    pub requires_verification: bool,
    pub requires_readback: bool,
    pub requires_visual: bool,
}

pub(super) struct ChildPrompt {
    pub system: String,
    pub skill_block: String,
    pub cache_scope: String,
    pub cache_stable_system: String,
    pub live_memory: LiveMemoryProjection,
}

pub(super) struct ChildTools {
    pub all: Vec<Value>,
    pub catalog: Vec<Value>,
    pub effects: EffectLedger,
    pub observations: ObservationGuard,
}

fn child_mutation_revision(state: &WorkerContext) -> u64 {
    state.receipts.iter().filter(|saved| saved["mutating"] == true).count() as u64
}

fn invalidate_child_memory_plan(state:&mut WorkerContext,status:&Value) {
    state.memory_plan_invalidated=true;
    state.acceptance_reviewed=false;
    state.review_pending=false;
    state.history.push(json!({"role":"user","content":format!("Broker: confirmed scoped memory changed while this step was pending. Follow only the current confirmed-memory block and current user request. Old interpretations and approvals cannot restore withdrawn memory. Replan undispatched work without replaying successful effects; preserve actual output and receipts. Affected identities: {}",status["invalidSources"])}));
}

/// Keep only undispatched tool identities in the existing worker checkpoint, never executable parameters.
/// 现有子任务检查点只保留未派发工具身份，不把文本参数转成可执行调用。
fn retain_child_tool_intent(state: &mut WorkerContext, content: &str, catalog: &[Value]) -> bool {
    let requested = task::unexecuted_tool_names(content);
    if !task::retain_unexecuted_tool_intent(&mut state.review_checkpoint, &requested, catalog, state.rounds,
        task::ToolIntentPolicy::PreserveUnknown) { return false; }
    state.review_pending = false;
    state.acceptance_reviewed = false;
    true
}

fn child_tool_intent_notice(state: &WorkerContext) -> String {
    format!("Broker: earlier text proposed these tools without executing them: {}. Proposals do not add requirements. Assess the current parent scope against real receipts; use the native protocol only for an actual missing action, otherwise deliver the established result. Never claim the proposal ran or replay successful effects.",
        state.review_checkpoint["unexecutedToolIntent"]["names"])
}

fn failure(code: &str, message: &str) -> GatewayError {
    GatewayError::new(code, message, 409)
}

struct CancellationForwarder(tokio::task::JoinHandle<()>);
impl Drop for CancellationForwarder {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn checkpoint_decode(bytes: &[u8]) -> Result<WorkerContext> {
    serde_json::from_slice(bytes).map_err(|_| {
        failure(
            "INVALID_TASK_CHECKPOINT",
            "Child checkpoint is invalid. / 子任务检查点无效。",
        )
    })
}

async fn checkpoint(
    run: &Arc<RequestRun>,
    journal: &Arc<TaskJournal>,
    state: &WorkerContext,
    phase: &str,
) -> Result<Value> {
    run.checkpoint_active_duration().await?;
    let bytes = serde_json::to_vec(state).map_err(|_| {
        failure(
            "INVALID_TASK_CHECKPOINT",
            "Child context cannot be encoded. / 子任务上下文无法编码。",
        )
    })?;
    let reference = journal.save_blob("child-context", &bytes).await?;
    journal.flush().await?;
    let task = journal.load_current()?;
    journal.save_checkpoint(&TaskCheckpoint {
        checkpoint_id: new_id(), through_event_sequence: task.event_sequence,
        context_reference: reference.clone(), next_step: json!({"phase":phase,"round":state.rounds,"pendingCalls":state.pending.as_ref().map_or(0, |response| response.calls.len().saturating_sub(state.pending_results.len()))}),
        source_versions: if state.code_version.is_null() { vec![] } else { vec![state.code_version.clone()] },
    }).await?;
    Ok(reference)
}

/// Coordinate child phases without changing durable state or delegated authority.
/// 协调子任务各阶段，持久状态与委派权限保持不变。
async fn execute_loop(
    runtime: &Arc<GatewayRuntime>, parent: &Arc<RequestRun>, run: &Arc<RequestRun>,
    child: &TaskRecord, ctx: GatewayToolContext, input: &Value, state: &mut WorkerContext,
) -> Result<TaskState> {
    let journal = run.journal.as_ref().ok_or_else(|| failure(
        "TASK_JOURNAL_REQUIRED", "Child requires a durable task journal. / 子任务须有持久日志。"))?;
    let (mut ctx, input, mut work_scope) = preparation::prepare_context(runtime, run, child, ctx, input)?;
    let _request_tools = gateway_tool_execution::RequestToolGuard(ctx.clone());
    let _foreground = tools::foreground(&runtime.tools);
    let preparation::PreparedWorker {mut model, limits, acceptance, mut prompt, tools: mut child_tools} =
        preparation::prepare(runtime, run, child, &ctx, &input, &work_scope, state).await?;
    let mut semantic = SemanticTask::default();
    let started = Instant::now();
    let mut repairs = 0u64;
    loop {
        if !preparation::prepare_round(preparation::RoundPreparation {
            runtime, parent, run, journal, child, ctx: &mut ctx, input: &input,
            work_scope: &mut work_scope, limits: &limits,
            requires_verification: acceptance.requires_verification, state, prompt: &mut prompt,
        }).await? {continue;}
        let duration_ms = limits["maxDurationMs"].as_u64().unwrap_or(1_800_000);
        if state.pending.is_none() && !model.receive(model_round::ModelRound {
            runtime, parent, run, journal, child, ctx: &mut ctx, state, prompt: &mut prompt,
            tools: &mut child_tools, acceptance_query: &acceptance.query,
            requires_readback: acceptance.requires_readback, requires_visual: acceptance.requires_visual,
            repairs, limits: &limits,
        }).await? {continue;}
        let response = state.pending.as_ref().expect("durable pending response").clone();
        let reviewing = state.pending_review && response.calls.is_empty();
        if let Some(status) = prompt.live_memory.refresh(&runtime.config.data_root,
            &run.conversation_id, &run.request_id, &mut prompt.system).await? {
            invalidate_child_memory_plan(state, &status);
        }
        if correct_pending_scope(run, journal, state, &response, &model.connection, &work_scope).await? {
            continue;
        }
        let tool_result = tool_round::execute(tool_round::ToolRound {
            runtime, parent, run, journal, child, ctx: &ctx, input: &input, limits: &limits,
            duration_ms, started, response: &response, connection: &model.connection, state,
            effects: &mut child_tools.effects, observations: &mut child_tools.observations,
            catalog: &mut child_tools.catalog, live_memory: &mut prompt.live_memory, system: &mut prompt.system,
        }).await;
        if let Err(error) = tool_result {
            if error.code != "TASK_PARENT_SCOPE_CHANGED" {return Err(error);}
            state.memory_plan_invalidated = true;
            checkpoint(run, journal, state, "ancestor-scope-changed-before-dispatch").await?;
            continue;
        }
        match completion::finish_round(completion::ReviewPhase {
            runtime, run, journal, child, ctx: &ctx, state, response: &response, reviewing,
            semantic: &mut semantic, catalog: &child_tools.catalog, all: &child_tools.all,
            acceptance_query: &acceptance.query, limits: &limits,
            requires_verification: acceptance.requires_verification,
            requires_readback: acceptance.requires_readback, requires_visual: acceptance.requires_visual,
            live_memory: &mut prompt.live_memory, system: &mut prompt.system,
            skill_block: &mut prompt.skill_block, repairs: &mut repairs,
        }).await? {
            completion::ReviewOutcome::Continue => continue,
            completion::ReviewOutcome::Completed => return Ok(TaskState::Completed),
        }
    }
}

async fn correct_pending_scope(run: &Arc<RequestRun>, journal: &Arc<TaskJournal>, state: &mut WorkerContext,
    response: &protocol::ModelResponse, connection: &Value, work_scope: &Value) -> Result<bool> {
    if !response.calls.iter().any(|call| !task::scope_allows_call(work_scope,
        call["name"].as_str().unwrap_or(""), crate::tools::policy::is_read_only_call(
            call["name"].as_str().unwrap_or(""), &call["arguments"]))) {return Ok(false);}
    let skipped = response.calls.iter().map(|call| state.pending_results.iter()
        .find(|settled|settled["call"]["id"] == call["id"]).cloned().unwrap_or_else(||json!({
            "call":call,"result":{"status":"error","executed":false,"isError":true,
            "code":"TASK_SCOPE_NOT_AUTHORIZED","content":{"notice":"Not dispatched: the parent's current plan, explanation or read-only boundary also applies to this child."}}
        }))).collect::<Vec<_>>();
    state.history = protocol::append_results(protocol::protocol(connection), &state.history, response, &skipped)?;
    state.pending = None;
    state.pending_results.clear();
    state.review_pending = false;
    state.history.push(json!({"role":"user","content":task::work_scope_instruction(work_scope)}));
    checkpoint(run, journal, state, "current-scope-correction").await?;
    Ok(true)
}

/// The Hub schedules dependencies; workers lock each mutation, not child waits.
/// Hub 调度依赖；Worker 仅在修改时持工作区锁，等待子任务时不占锁。
pub async fn execute(
    runtime: Arc<GatewayRuntime>,
    parent: Arc<RequestRun>,
    child: TaskRecord,
    mut context: GatewayToolContext,
    input: Value,
) -> Result<Value> {
    if child.scope.conversation_id != context.conversation_id
        || child.scope.project_id != context.project_id
        || child.parent_task_id.as_deref()
            != parent.journal.as_ref().map(|journal| journal.task_id())
    {
        return Err(failure(
            "CHILD_SCOPE_MISMATCH",
            "Child scope must match the real parent binding. / 子范围须匹配真实父归属。",
        ));
    }
    let parent_scope = parent
        .journal
        .as_ref()
        .ok_or_else(|| {
            failure(
                "TASK_JOURNAL_REQUIRED",
                "Parent task journal is required. / 父任务日志缺失。",
            )
        })?
        .load_current()?
        .scope;
    if parent_scope.permission_snapshot != child.scope.permission_snapshot
        || child.scope.permission_snapshot["permissionMode"] != context.permission_mode
        || child.scope.permission_snapshot["bindingSnapshot"] != context.config["bindingSnapshot"]
    {
        return Err(failure(
            "CHILD_SCOPE_MISMATCH",
            "Child authorization must equal the parent's real permission and binding snapshot. / 子授权须与父任务真实权限及归属快照一致。",
        ));
    }
    let journal = TaskJournal::start_existing(
        &runtime.config.data_root,
        &child,
        "native-child-worker",
        60_000,
    )?;
    let run = runtime.child_run(&child, journal.clone())?;
    // Write child protocol checkpoints to the child attempt, not the parent's event channel.
    // 子任务协议检查点写入自身尝试，不借用父事件通道。
    let (tool_sender,mut tool_receiver)=tokio::sync::mpsc::channel(64);
    let tool_run=run.clone();
    let tool_forwarder=tokio::spawn(async move {
        while let Some(event)=tool_receiver.recv().await { tool_run.emit(event).await; }
    });
    context.events=Some(tool_sender);
    runtime.tasks.resume_children(run.clone(),context.clone()).await?;
    let mut queued_cancel = context.cancellation.clone();
    let mut parent_cancel = parent.cancellation.subscribe();
    let weak_run = Arc::downgrade(&run);
    let _cancellation_forwarder = CancellationForwarder(tokio::spawn(async move {
        tokio::select! { _ = tools::wait_for_cancellation(&mut queued_cancel) => {}, _ = tools::wait_for_cancellation(&mut parent_cancel) => {} }
        if let Some(run) = weak_run.upgrade() {
            run.cancel();
        }
    }));
    let mut state = match journal.latest_checkpoint()? {
        Some(saved) => checkpoint_decode(&journal.read_model_blob(&saved.context_reference).await?)?,
        None => WorkerContext::default(),
    };
    if matches!(child.stop_reason.as_deref(), Some("user-resumed" | "user-requested-revision")) {
        state.work_progress.resume_after_user_instruction();
    }
    // Sanitize legacy public summaries only; retain protocol history, call pairs and signatures.
    // 仅清理旧公开摘要，保留协议历史、调用配对和续传签名。
    state.output = super::task::strip_history_records(&state.output);
    if let Some(reason) = input["parentRevision"]["reason"].as_str().filter(|_|input["parentRevision"]["reviewedVersion"].as_u64().unwrap_or(0)>state.last_parent_revision) {
        state.work_progress.resume_after_user_instruction();
        state.last_parent_revision=input["parentRevision"]["reviewedVersion"].as_u64().unwrap_or(0);
        state.acceptance_reviewed = false;
        state.review_pending = false;
        if state.acceptance_requirements.is_empty() {
            state.acceptance_requirements=if child.acceptance.is_empty(){task::acceptance_requirements(&child.goal)}else{child.acceptance.clone()};
        }
        let requirement=format!("Parent revision requirement / 父验收退回要求：{reason}");
        if state.acceptance_requirements.len()>=128 {
            if let Some(last)=state.acceptance_requirements.last_mut() {last.push('\n');last.push_str(&requirement);}
        }else{state.acceptance_requirements.push(requirement);}
        state.history.push(json!({"role":"user","content":format!("Broker: the parent returned this same delivery for revision: {reason}. Preserve all original requirements, successful operations and saved output. Fix the identified gap and obtain acceptance using evidence for the affected targets and versions; do not replay successful writes.")}));
    }
    let outcome = execute_loop(&runtime, &parent, &run, &child, context, &input, &mut state).await;
    let _=tokio::time::timeout(Duration::from_secs(2),tool_forwarder).await;
    run.checkpoint_active_duration().await?;
    let (status, blocked) = match outcome {
        Ok(status) => (status, Value::Null),
        Err(error) => {
            let status = if error.code == "ABORT_ERR"
                && parent.suspending.load(std::sync::atomic::Ordering::Acquire)
            {
                TaskState::Paused
            } else if error.code == "ABORT_ERR" {
                journal
                    .load_current()
                    .map(|task| {
                        if task.state == TaskState::Paused {
                            TaskState::Paused
                        } else {
                            TaskState::Cancelled
                        }
                    })
                    .unwrap_or(TaskState::Cancelled)
            } else if super::gateway_runtime::automatic_network_recovery(&error.code)
            {
                TaskState::WaitingNetwork
            } else if error.code.contains("CONFIG")
                || error.code.contains("PROVIDER")
                || error.code.contains("AUTH")
                || matches!(error.code.as_str(), "TASK_INPUT_REQUIRED" | "TASK_EXTERNAL_WAIT" | "TASK_CAPABILITY_UNAVAILABLE")
                || error.code.starts_with("RESOURCE_")
                || error.code.starts_with("LOCAL_MODEL_RESOURCE_")
                || error.code.starts_with("LOCAL_MODEL_IDENTITY_")
                || error.code.starts_with("LOCAL_MODEL_TOOLS_")
            {
                TaskState::WaitingConfig
            } else {
                TaskState::Partial
            };
            (status, json!({"code":error.code,"message":error.message}))
        }
    };
    for verification in &mut state.verification {
        verification["current"] = json!(
            verification["codeVersion"] == state.code_version
                && verification["unchangedDuringExecution"] == true
                && verification["codeVersion"]["complete"] == true
                && state.code_version["complete"] == true
        );
    }
    // Separate final delivery from progress; retain the full transcript in its checkpoint.
    // 最终交付与进度分开，完整过程记录仍保存在原检查点。
    let delivery=if status==TaskState::Completed && !state.final_summary.trim().is_empty(){&state.final_summary}else{&state.output};
    let mut result = json!({"taskId":child.task_id,"status":status,"summary":delivery,"summarySource":if status==TaskState::Completed && !state.final_summary.trim().is_empty(){"latest-delivery"}else{"preserved-progress"},"evidence":state.receipts.iter().filter(|receipt| receipt["observation"]==true && verified_completion_receipt(receipt)).map(|receipt| receipt["result"]["resultRef"].clone()).collect::<Vec<_>>(),"changedFiles":state.changed_files,"verification":state.verification,"unresolved":if blocked.is_null() {json!([])} else {json!([blocked])},"successfulOpsRefs":state.receipts.iter().filter(|receipt| receipt["observation"]!=true && verified_completion_receipt(receipt)).map(|receipt| receipt["result"]["resultRef"].clone()).collect::<Vec<_>>(),"completionMeans":"executor-reported; verification is supported only by the listed real receipts","contextRef":journal.latest_checkpoint()?.map(|saved| saved.context_reference)});
    result["outputHistoryRef"]=result["contextRef"].clone();
    if status == TaskState::Completed && state.acceptance_reviewed {
        result["brokerDecision"] = json!("final");
    }
    result["modelTaskState"] = state.acceptance_review.clone();
    let requirements: Vec<Value> = state.acceptance_requirements.iter().enumerate().map(|(index,item)|
        json!({"requirementId":format!("requirement-{}",index + 1),"item":item})).collect();
    let review = ChildCompletionReview::new(&state, &child.goal);
    result["acceptanceReview"] = json!({"required":task::requires_execution_verification(&child.goal)
        || input["requiresVerification"]==true || !state.changed_files.is_empty(),
        "reviewed":state.acceptance_reviewed,"requirements":requirements,"met":state.acceptance_review["met"],
        "unmet":state.acceptance_review["unmet"],"readbackRefs":review.readback_refs,
        "visualReadbackRefs":review.visual_readback_refs,"correctnessCertified":false,"parentAcceptance":"pending"});
    result["completionMeans"] = json!("receipt-reviewed where required; parent acceptance pending; quality is not certified by IDs alone");
    let output_ref = journal
        .save_blob(
            "task-output",
            &serde_json::to_vec(&result).map_err(|_| {
                failure(
                    "INVALID_TASK_OUTPUT",
                    "Child output cannot be encoded. / 子输出无法编码。",
                )
            })?,
        )
        .await?;
    result["outputRef"] = output_ref;
    journal.emit_control(
        "task.worker.finished",
        &json!({"taskId":child.task_id,"state":status,"outputRef":result["outputRef"]}),
    )?;
    if status == TaskState::Cancelled
        && !matches!(
            journal.load_current()?.state,
            TaskState::Paused | TaskState::Cancelled
        )
    {
        let current = journal.load_current()?;
        journal.repository().control(
            journal.task_id(),
            current.version,
            crate::storage::task_repository::TaskControl::Cancel,
        )?;
    }
    journal
        .finish(status.clone(), blocked["code"].as_str())
        .await?;
    // Send bounded UI previews; keep full reports in formal artifacts.
    // 界面仅传有界预览，完整报告保存在正式产物。
    let mut preview_units = 0usize;
    let preview: String = delivery
        .chars()
        .take_while(|character| {
            preview_units += character.len_utf16();
            preview_units <= 4096
        })
        .collect();
    parent.emit(json!({"type":"task_finished","taskId":child.task_id,"state":status,"outputRef":result["outputRef"],"summaryPreview":preview,"summaryTruncated":preview_units>4096})).await;
    Ok(result)
}

#[cfg(test)]
#[path = "gateway_task_worker_tests.rs"]
mod tests;
