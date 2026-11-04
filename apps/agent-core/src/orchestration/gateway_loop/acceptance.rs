//! 根任务验收映射、版本和恢复；正式证明仍由原领域维护。

use super::super::{
    completion_evidence::{
        self, directory as completion_evidence_directory, unresolved_check_summary,
    },
    gateway_runtime::RequestRun,
    task,
};
use super::{LoopState, bounded_advisory_notice, native};
use crate::gateway_contract::GatewayError;
#[cfg(test)]
use crate::storage::gateway as data;
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct CompletionState {
    pub(super) retained_completion_observations: Vec<Value>,
    pub(super) acceptance_reviewed: bool,
    pub(super) acceptance_review_revision: Option<(u64, String)>,
    pub(super) last_acceptance_reminder_round: u64,
    pub(super) review_checkpoint: Value,
    pub(super) mutation_sequence: u64,
    pub(super) acceptance_requirements: Vec<String>,
}

impl CompletionState {
    /// 重置评审不影响成功观察和当前工作版本。
    pub(super) fn reset_review(&mut self) {
        self.review_checkpoint = Value::Null;
        self.acceptance_reviewed = false;
        self.acceptance_review_revision = None;
    }

    /// 上下文变化只使评审失效，不复活或丢弃尚未派发的意图。
    pub(super) fn reset_review_preserving_intent(&mut self, checkpoint: Value) {
        let intent = self.review_checkpoint["unexecutedToolIntent"].clone();
        self.reset_review();
        self.review_checkpoint = checkpoint;
        if !intent.is_null() {
            if !self.review_checkpoint.is_object() {
                self.review_checkpoint = json!({});
            }
            self.review_checkpoint["unexecutedToolIntent"] = intent;
        }
    }

    fn checkpoint_review(&mut self, pending: bool, evidence: bool, attempts: u64) {
        if !self.review_checkpoint.is_object() {
            self.review_checkpoint = json!({});
        }
        self.review_checkpoint["pending"] = json!(pending);
        self.review_checkpoint["evidenceRequired"] = json!(evidence);
        self.review_checkpoint["attempts"] = json!(attempts);
    }

    fn retain_tool_intent(&mut self, requested: &[String], catalog: &[Value], round: u64) -> bool {
        if !task::retain_unexecuted_tool_intent(
            &mut self.review_checkpoint,
            requested,
            catalog,
            round,
            task::ToolIntentPolicy::CatalogOnly,
        ) {
            return false;
        }
        self.review_checkpoint["pending"] = json!(false);
        self.review_checkpoint
            .as_object_mut()
            .unwrap()
            .remove("cachedReview");
        self.acceptance_reviewed = false;
        self.acceptance_review_revision = None;
        true
    }
}

impl LoopState {
    /// 未发布的公开草稿沿用验收事实版本，不另建结果或执行记录。
    pub(in crate::orchestration::gateway_loop) fn bind_pending_delivery(
        &mut self,
        assessment: &Value,
    ) {
        if self.delivery.pending_delivery.is_empty() {
            if let Some(checkpoint) = self.completion.review_checkpoint.as_object_mut() {
                checkpoint.remove("pendingDelivery");
            }
            return;
        }
        let binding = json!({
            "contentHash":crate::retrieval::gateway_index::hash(&self.delivery.pending_delivery),
            "revision":acceptance_revision(self),
            "assessment":{
                "source":assessment["source"],"state":assessment["state"],
                "blocker":assessment["blocker"],"reason":assessment["reason"],
                "feasibility":assessment["feasibility"],
            },
        });
        if !self.completion.review_checkpoint.is_object() {
            self.completion.review_checkpoint = json!({});
        }
        self.completion.review_checkpoint["pendingDelivery"] = binding;
    }

    fn pending_delivery_is_current(&self) -> bool {
        let binding = &self.completion.review_checkpoint["pendingDelivery"];
        !self.delivery.pending_delivery.is_empty()
            && binding["contentHash"]
                == crate::retrieval::gateway_index::hash(&self.delivery.pending_delivery)
            && binding["revision"] == json!(acceptance_revision(self))
    }

    /// 停止时只直接发布本轮对应的受阻结论。
    pub(in crate::orchestration::gateway_loop) fn pending_delivery_matches_blocker(
        &self,
        assessment: &Value,
    ) -> bool {
        let saved = &self.completion.review_checkpoint["pendingDelivery"]["assessment"];
        self.pending_delivery_is_current()
            && assessment["state"] == "blocked"
            && task::genuine_blocker(assessment)
            && ["source", "state", "blocker", "reason", "feasibility"]
                .iter()
                .all(|field| saved[*field] == assessment[*field])
    }
}

/// 按原执行顺序借用公开观察，供同次评审使用。
fn completion_observations(state: &LoopState) -> impl Iterator<Item = &Value> {
    state
        .completion
        .retained_completion_observations
        .iter()
        .chain(
            state
                .transcript
                .rounds
                .iter()
                .flat_map(|round| round["observations"].as_array().into_iter().flatten()),
        )
}

pub(super) fn completion_review(state: &LoopState) -> completion_evidence::CompletionEvidenceSet {
    completion_evidence::CompletionEvidenceSet::from_observations(
        completion_observations(state),
        state.progress.verification(),
    )
    .with_current_requirements(&state.progress, &state.completion.acceptance_requirements)
}

pub(super) fn completion_transcript(state: &LoopState) -> Vec<Value> {
    let mut rounds = Vec::with_capacity(state.transcript.rounds.len() + 1);
    rounds.push(json!({"observations":state.completion.retained_completion_observations}));
    rounds.extend(state.transcript.rounds.iter().cloned());
    rounds
}

pub(super) fn mark_visual_proofs(observations: &mut [Value], delivered_hashes: &[String]) {
    for observation in observations {
        let proof = &mut observation["completionProof"];
        if proof.is_object()
            && proof["visualHashes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|hash| delivered_hashes.iter().any(|delivered| delivered == hash))
        {
            proof["visualObserved"] = json!(true);
        }
    }
}

pub(super) fn requires_task_verification(state: &LoopState) -> bool {
    !task::response_only_scope(&state.assistant["CurrentWorkScope"])
        && state
            .completion
            .acceptance_requirements
            .iter()
            .any(|requirement| task::requires_execution_verification(requirement))
}

/// 恢复交付时比较验收事实，不改历史输出。
pub(super) fn restored_completion_projection(state: &LoopState) -> Vec<Value> {
    state.transcript.rounds.iter().flat_map(|round| round["observations"].as_array().into_iter().flatten())
        .chain(state.completion.retained_completion_observations.iter())
        .map(|observation| json!({"id":observation["id"],"tool":observation["name"],"ref":observation["ref"],
            "status":observation["status"],"verifiedSuccess":observation["verifiedSuccess"],
            "completionProof":observation["completionProof"],"completionEvidence":observation["completionEvidence"],
            "completionProofVerification":observation["completionProofVerification"]})).collect()
}

pub(super) fn reconcile_restored_delivery(state: &mut LoopState, before: &[Value]) {
    let after = restored_completion_projection(state);
    let evidence_changed = before != after.as_slice();
    let revising_delivery = unresolved_user_steering(state);
    let unbound_delivery =
        !state.delivery.pending_delivery.is_empty() && !state.pending_delivery_is_current();
    // 只保留绑定当前证据、要求与权限的草稿。
    if !evidence_changed && !revising_delivery && !unbound_delivery {
        return;
    }
    let changed = after
        .iter()
        .filter(|item| !before.contains(item))
        .map(|item| {
            json!({"id":item["id"],
        "tool":item["tool"],"verifiedSuccess":item["verifiedSuccess"],
        "verificationPending":item["completionProofVerification"] == "pending"})
        })
        .collect::<Vec<_>>();
    let historical_failures = after
        .iter()
        .filter(|item| item["verifiedSuccess"] == false)
        .map(|item| {
            json!({"id":item["id"],"tool":item["tool"],"status":item["status"],
            "verificationPending":item["completionProofVerification"] == "pending"})
        })
        .collect::<Vec<_>>();
    let successes = after
        .iter()
        .filter(|item| item["verifiedSuccess"] == true)
        .map(|item| json!({"id":item["id"],"tool":item["tool"],"resultRef":item["ref"]}))
        .collect::<Vec<_>>();
    // 被替代的草稿保留在不可变检查点中，不继续留在当前交付缓冲。
    let correction = json!({"evidenceChanged":evidence_changed,"changedReceipts":changed,
        "requestedDeliveryRevision":revising_delivery,
        "unboundDelivery":unbound_delivery,
        "supersededDelivery":!state.delivery.pending_delivery.is_empty(),"sourceCheckpoint":state.context.work_context_reference,
        "supersededDeliveryHash":crate::retrieval::gateway_index::hash(&state.delivery.pending_delivery)});
    state.delivery.pending_delivery.clear();
    state.delivery.delivery_continuing = false;
    state.completion.reset_review_preserving_intent(json!({
        "pending":false,"evidenceRequired":true,"attempts":0,"restorationCorrection":correction,
    }));
    let _ = state.task.apply("task.resumed", &json!({}));
    state.broker_note(&format!("Broker: the previous draft and assessment are superseded; use current requirements and verified receipts. Historical failed/unverified receipts: {}. Preserved successful receipts: {}. Corrected receipt facts: {}. These describe earlier operations, not current service availability. Continue only remaining authorized work, reuse successful results and verify missing facts, then report the actual result. Keep prior output as history; do not repeat operations, reread complete archives or add optional audits.",
        json!(historical_failures), json!(successes), state.completion.review_checkpoint["restorationCorrection"]["changedReceipts"]));
}

pub(super) fn completion_review_instruction(state: &LoopState) -> String {
    if unresolved_user_steering(state) {
        return format!(
            "Broker: resolve the latest real user steering BEFORE enforcing older acceptance checks or duration. Encode continue, deliver-existing or stop in the normal footer. Deliver-existing returns saved work without further checking or certifying the original requirements; stop acknowledges withdrawal. Neither requires completion evidence. Missing control is not consent to keep the old scope. Native tools remain available after the current instruction is resolved.{}{}",
            steering_revision_instruction(state),
            if state.completion.review_checkpoint["reviewFormatRepair"].is_object() {
                " Tools are closed for this one turn: reply now with one standalone single-line <kynxa_task> footer that resolves this instruction. Tools return on the next turn."
            } else {
                ""
            }
        );
    }
    let requirements = state.completion.acceptance_requirements.iter().enumerate().map(|(index, requirement)|
        json!({"requirementId":format!("requirement-{}", index + 1),"item":requirement})).collect::<Vec<_>>();
    let review = completion_review(state);
    let verification = &review.verification;
    let instruction = format!(
        "Broker: {}\nExpected acceptance requirements: {}\nVerified completion evidence: {}\nBroker validation: {}",
        task::COMPLETION_REVIEW_CONTRACT,
        json!(requirements),
        json!(review.valid_references),
        json!({"state":verification["state"],"pendingValidation":verification["pendingValidation"],
            "unresolvedChecks":unresolved_check_summary(&verification),
            "latestCheck":verification["receipts"].as_array().and_then(|receipts|receipts.last())})
    );
    let instruction = instruction + &steering_revision_instruction(state);
    // 仅在验收未通过时补充详细身份，普通验收不挤占小窗口预算。
    if state.completion.review_checkpoint["cachedReview"]["decision"]["action"] != "resume" {
        return instruction;
    }
    let evidence = completion_evidence_directory(&review.records);
    instruction
        + &format!(
            "\nNative receipt directory (IDs and operations are authoritative; a model-written label cannot change them): {evidence}. Put exact IDs alone in evidence; put explanations in item/reason. Historical receipts prove their original operations, not a fresh connection check. Fix a reference-format problem from this directory without rereading unrelated archives."
        )
}

pub(super) fn unresolved_user_steering(state: &LoopState) -> bool {
    state.assistant["LatestUserSteering"]["userMessageId"]
        .as_str()
        .is_some_and(|id| {
            !id.is_empty() && state.assistant["UserTaskDirective"]["userMessageId"] != id
        })
}

pub(super) fn steering_revision_instruction(state: &LoopState) -> String {
    let latest = &state.assistant["LatestUserSteering"];
    if !latest["userMessageId"].is_string() {
        return String::new();
    }
    let requirements = state
        .completion
        .acceptance_requirements
        .iter()
        .enumerate()
        .map(|(index, text)| json!({"requirementId":format!("requirement-{}",index+1),"text":text}))
        .collect::<Vec<_>>();
    format!(
        "\nLatest real user steering: {latest}. It overrides conflicting earlier instructions. Interpret additions, replacements, withdrawals and stopping semantically, not by matching words or quoted examples. Resolve it in the SAME <kynxa_task> footer using steeringRevision: {{\"userMessageId\":\"the exact latest ID\",\"quote\":\"exact words from that user message\",\"disposition\":\"continue\",\"removeRequirementIds\":[],\"replaceRequirements\":[{{\"requirementId\":\"requirement-1\",\"text\":\"replacement\"}}],\"addRequirements\":[],\"minimumActiveMs\":0}}. Omit unchanged scope fields; an unchanged interpretation must explicitly state disposition continue with the ID and quote. Zero withdraws a minimum duration without erasing time already worked. Use disposition deliver-existing when the user asks to finish with existing work, or stop when further work is withdrawn; a top-level disposition is also bound to this turn's verified user message. Both return a brief answer with no further tools or old acceptance checks, preserving unfinished scope and successful receipts without claiming the original target complete. Unmentioned requirements remain active for continue. Current IDs: {}",
        json!(requirements)
    )
}

/// 仅应用绑定最新正式引导的要求变更，执行结果保持不变。
pub(super) fn apply_steering_revision(
    state: &mut LoopState,
    inspected: &Value,
    minimum_active_ms: &mut u64,
) {
    // 同一已接受消息只修改一次范围。
    if !unresolved_user_steering(state) {
        return;
    }
    let latest = &state.assistant["LatestUserSteering"];
    let id = latest["userMessageId"].as_str().unwrap_or("").to_owned();
    // 只绑定明确控制；没有变更字段时不视为要求未变。
    let mut revision = inspected["steeringRevision"].clone();
    if let Some(disposition) = inspected["disposition"]
        .as_str()
        .or_else(|| (inspected["stop"] == true).then_some("stop"))
    {
        if revision.is_null() {
            revision = json!({"userMessageId":id,"quote":latest["text"]});
        }
        if revision["disposition"]
            .as_str()
            .is_some_and(|nested| nested != disposition)
        {
            state.broker_note("Conflicting steering dispositions were not applied. Resolve the latest user instruction consistently; preserve existing work.");
            return;
        }
        revision["disposition"] = json!(disposition);
    }
    if revision.is_null() {
        return;
    }
    let revision_hash = crate::retrieval::gateway_index::hash(&revision.to_string());
    match task::steering_revision(&revision, &id, latest["text"].as_str().unwrap_or(""),
        &state.completion.acceptance_requirements, *minimum_active_ms) {
        Ok(updated) => {
            if revision["disposition"].is_null() && revision["stop"].is_null()
                && updated.requirements == state.completion.acceptance_requirements
                && updated.minimum_active_ms == *minimum_active_ms {
                // 这里只复写消息身份，按原范围继续需要明确选择。
                return;
            }
            let suspended = updated.stop.then(|| state.completion.acceptance_requirements.clone());
            state.completion.acceptance_requirements = updated.requirements;
            *minimum_active_ms = updated.minimum_active_ms;
            state.assistant["UserTaskDirective"] = json!({"userMessageId":id,
                "minimumActiveMs":updated.minimum_active_ms,"stop":updated.stop,
                "deliverExisting":updated.deliver_existing,
                "requirements":state.completion.acceptance_requirements,"suspendedRequirements":suspended,
                "revisionHash":revision_hash});
            state.completion.reset_review();
        }
        Err(_) => state.broker_note("The steeringRevision metadata did not match the latest user message or current requirement IDs. Correct it in the next normal reply; retain progress and successful receipts."),
    }
}

/// 用户是否在要求停下；这类话不能当成新增要求继续执行。
fn steering_stop_words() -> &'static regex::Regex {
    static STOP: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    STOP.get_or_init(|| {
        regex::Regex::new(
            r"(?i)停止|停下|别做|不用做|不用了|不要了|算了|取消|终止|\b(?:stop|cancel|abort|halt)\b",
        )
        .expect("static stop expression")
    })
}

/// 去掉停止用语、语气词和标点后什么都不剩，才是单纯要求停下；不依赖用户怎么加标点。
pub(super) fn steering_means_stop(text: &str) -> bool {
    let rest = steering_stop_words().replace_all(text, "").to_lowercase();
    steering_stop_words().is_match(text)
        && rest
            .replace("please", "")
            .replace("now", "")
            .chars()
            .all(|c| !c.is_alphanumeric() || "来吧了呀啊哦嗯啦先请那就".contains(c))
}

/// 既提到停止又有别的内容时，原来的工作还做不做由模型按这句话的意思判断，这里不替它拆句。
fn steering_changes_work(text: &str) -> bool {
    steering_stop_words().is_match(text) && !steering_means_stop(text)
}

/// 把用户原话加入验收项继续工作，格式问题不该让任务中断。
pub(super) fn adopt_unacknowledged_steering(state: &mut LoopState, minimum_active_ms: u64) {
    let latest = state.assistant["LatestUserSteering"].clone();
    let text = latest["text"].as_str().unwrap_or("").trim().to_owned();
    let changes_work = steering_changes_work(&text);
    if changes_work {
        state.completion.acceptance_requirements.clear();
    }
    if !text.is_empty() && !state.completion.acceptance_requirements.contains(&text) {
        state.completion.acceptance_requirements.push(text);
    }
    state.assistant["UserTaskDirective"] = json!({"userMessageId":latest["userMessageId"],
        "minimumActiveMs":minimum_active_ms,"stop":false,"deliverExisting":false,
        "requirements":state.completion.acceptance_requirements,"suspendedRequirements":null,
        "revisionHash":"adopted-without-revision"});
    state.completion.reset_review();
    if let Some(fields) = state.completion.review_checkpoint.as_object_mut() {
        fields.remove("reviewFormatRepair");
    }
    state.broker_note(if changes_work {
        "The latest user message changes the earlier work. Follow it by its meaning: earlier work continues only where the message keeps it. If it says to stop that work, do not continue it and do only what it asks. Tools are available."
    } else {
        "The latest user message was added to the requirements as written. Tools are available; continue the work and cover it."
    });
}

pub(super) fn acceptance_revision(state: &LoopState) -> (u64, String) {
    let verification = completion_review(state).verification;
    let receipts = completion_observations(state)
        .filter(|observation| {
            observation["verifiedSuccess"] == true
                && task::is_completion_evidence_tool(observation["name"].as_str().unwrap_or(""))
        })
        .map(|observation| {
            json!([
                observation["id"],
                observation["ref"],
                observation["mutationSequence"],
                observation["completionProof"]
            ])
        })
        .collect::<Vec<_>>();
    (
        state.completion.mutation_sequence,
        crate::retrieval::gateway_index::hash(
            &json!([
                state.completion.acceptance_requirements,
                state
                    .context
                    .runtime_snapshot
                    .as_ref()
                    .map(|snapshot| &snapshot["source"]["authorityHash"]),
                receipts,
                state.visual.visual_readback_delivered,
                state.documents.compact_projection(),
                state.context.formal_sources,
                state.tools.selected,
                state.progress.summary(),
                verification["state"],
                verification["pendingValidation"]
            ])
            .to_string(),
        ),
    )
}

/// 仅复用证据和权限版本均匹配的验收结果，执行方案重新选择。
fn cached_review_for_revision(checkpoint: &Value, revision: &(u64, String)) -> Option<Value> {
    let cached = &checkpoint["cachedReview"];
    (cached["revision"] == json!(revision) && cached["decision"]["reviewed"] == true)
        .then(|| cached["decision"].clone())
}

pub(super) fn cached_completion_review(state: &LoopState) -> Option<Value> {
    cached_review_for_revision(
        &state.completion.review_checkpoint,
        &acceptance_revision(state),
    )
}

pub(super) fn cache_completion_review(
    checkpoint: &mut Value,
    revision: &(u64, String),
    decision: &Value,
    assessment: &Value,
) {
    if !checkpoint.is_object() {
        *checkpoint = json!({});
    }
    checkpoint["cachedReview"] =
        json!({"revision":revision,"decision":decision,"assessment":assessment});
}

pub(super) fn checkpoint_completion_review(
    state: &mut LoopState,
    pending: bool,
    evidence: bool,
    attempts: u64,
) {
    state
        .completion
        .checkpoint_review(pending, evidence, attempts);
}

/// 调用语法文本仅作导航，须经正式派发核对结果，仍可复用相同操作的成功结果。
pub(super) fn retain_unexecuted_tool_intent(
    state: &mut LoopState,
    requested: &[String],
    catalog: &[Value],
    round: u64,
) -> bool {
    if !state
        .completion
        .retain_tool_intent(requested, catalog, round)
    {
        return false;
    }
    state.delivery.pending_delivery.clear();
    state.delivery.delivery_continuing = false;
    true
}

/// 仅继承正式产物目标，凭当前结果重核版本和读取覆盖。
pub(super) async fn restore_continued_readback_targets(
    run: &RequestRun,
    limits: &Value,
    state: &mut LoopState,
) -> Result<(), GatewayError> {
    let Some(previous_id) = limits["continuedFromTaskId"]
        .as_str()
        .filter(|id| !id.is_empty())
    else {
        return Ok(());
    };
    let Some(journal) = &run.journal else {
        return Ok(());
    };
    let current = journal.load_current()?;
    let repository = journal.repository().clone();
    let previous_id = previous_id.to_owned();
    let targets = tokio::task::spawn_blocking(move || -> Result<Value, GatewayError> {
        let previous = repository.load(&previous_id)?;
        if previous.parent_task_id.is_some()
            || previous.scope.conversation_id != current.scope.conversation_id
            || previous.scope.project_id != current.scope.project_id
        {
            return Ok(Value::Null);
        }
        Ok(repository
            .latest_event(&previous_id, "task.final")?
            .map(|event| event.payload["completion"]["documentValidation"]["targets"].clone())
            .unwrap_or(Value::Null))
    })
    .await
    .map_err(|_| GatewayError::new("TASK_CONTEXT_WORKER_FAILED", "续作目标读取退出。", 500))??;
    if targets.is_array() {
        native(state.progress.apply(
            "progress.restoreDocumentReadbackTargets",
            &json!({"targets":targets}),
        ))?;
    }
    Ok(())
}

pub(super) fn current_effectful_work(state: &LoopState) -> bool {
    task::scope_allows_mutation(&state.assistant["CurrentWorkScope"])
        && state.completion.mutation_sequence > 0
}

/// 每个工作版本只执行一次替代动作，新结果 ID 不重置次数。
fn replanned_action(
    checkpoint: &mut Value,
    selected: &[Value],
    proposed: &Value,
    revision: &Value,
) -> Option<Value> {
    if !proposed["arguments"].is_object()
        || !selected.iter().any(|tool| tool["name"] == proposed["name"])
    {
        return None;
    }
    let identity = json!([proposed["name"], proposed["arguments"], revision]);
    let mut actions = checkpoint["executionActions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if actions.contains(&identity) {
        return None;
    }
    actions.push(identity);
    if !checkpoint.is_object() {
        *checkpoint = json!({});
    }
    checkpoint["executionActions"] = json!(actions);
    Some(
        json!({"name":proposed["name"],"arguments":proposed["arguments"],"purpose":"execute-replanned-action"}),
    )
}

/// 执行验收提出的具体修正，不遍读所有产物。
pub(super) fn completion_progress_plan(state: &mut LoopState, proposed: &Value) -> Option<Value> {
    let decision = cached_completion_review(state)?;
    if decision["action"] != "resume" || decision["minimumActive"] == true {
        return None;
    }
    let progress = state.progress.summary();
    let revision = json!([
        state.completion.mutation_sequence,
        progress["mutationRevision"],
        progress["checkedMutationRevision"]
    ]);
    completion_execution_plan(
        &mut state.completion.review_checkpoint,
        &state.tools.selected,
        proposed,
        &revision,
    )
}

fn completion_execution_plan(
    checkpoint: &mut Value,
    selected: &[Value],
    proposed: &Value,
    revision: &Value,
) -> Option<Value> {
    // 引用错误只修正元数据；具体动作仍走原授权、去重和派发链。
    if let Some(plan) = replanned_action(checkpoint, selected, proposed, revision) {
        return Some(plan);
    }
    let attempted = checkpoint["executionChecks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let names = checkpoint["executionCapabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|name| !selected.iter().any(|tool| tool["name"] == **name))
        .cloned()
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }
    let plan = json!({"name":"tool.load","arguments":{"names":names},"purpose":"load-alternative-execution-capability"});
    if attempted.contains(&json!([plan["name"], plan["arguments"]])) {
        return None;
    }
    if !checkpoint.is_object() {
        *checkpoint = json!({});
    }
    let mut attempted = attempted;
    attempted.push(json!([plan["name"], plan["arguments"]]));
    checkpoint["executionChecks"] = json!(attempted);
    Some(plan)
}

/// 尾部提醒仅说明证据缺口，不改变目标或结束条件。
pub(super) fn refresh_acceptance_reminder(state: &mut LoopState, round: u64) {
    if round.saturating_sub(state.completion.last_acceptance_reminder_round) < 8
        || !requires_task_verification(state)
    {
        return;
    }
    state.completion.last_acceptance_reminder_round = round;
    let current =
        state.completion.acceptance_review_revision.as_ref() == Some(&acceptance_revision(state));
    let review = completion_review(state);
    let gaps = state.task.acceptance_gaps(
        &state.completion.acceptance_requirements,
        &review.valid_references,
        current,
        Some(&review.records),
    );
    if gaps.is_empty() {
        return;
    }
    let preview = gaps
        .iter()
        .take(6)
        .map(|gap| {
            json!({"id":gap["requirementId"],
        "item":gap["item"].as_str().unwrap_or("").chars().take(160).collect::<String>()})
        })
        .collect::<Vec<_>>();
    let notice = format!(
        "Broker internal acceptance reminder: {} requirements still lack a current successful receipt mapping; examples: {}. This means unverified, not necessarily undone. Reuse saved receipts and completed outputs; do not repeat successful operations. Continue useful work toward the original target. This reminder is neither a final answer nor a reason to stop.",
        gaps.len(),
        json!(preview)
    );
    let concise = format!(
        "Broker: {} acceptance items remain unverified. Reuse saved receipts; verify remaining original requirements, preserve progress and do not redo successful operations or stop for this reminder.",
        gaps.len()
    );
    let notice = bounded_advisory_notice(Some(notice), &concise, 320).unwrap_or(concise);
    state.context_note(
        &notice,
        json!({"projectionOrigin":"broker-control","acceptanceGapNotice":true}),
    );
}

#[cfg(test)]
mod tests {
    use super::super::{
        CacheState, ContextState, DeliveryState, ToolsState, TranscriptState, VisualState,
        control_notice_update,
    };
    use super::*;
    use crate::{
        gateway_contract::new_id,
        orchestration::{gateway_tool_execution::restore_completion_evidence, task::TaskState},
    };
    use std::{collections::HashSet, time::Instant};

    #[test]
    fn completion_review_cache_requires_current_evidence_and_valid_assessment() {
        let revision = (7, "receipts-authority-requirements-v1".to_owned());
        let mut checkpoint = json!({"cachedReview":{"revision":revision,"decision":{
            "action":"resume","reviewed":true,"remaining":["inspect the saved result"]}}});
        let expected = cached_review_for_revision(&checkpoint, &revision).unwrap();
        for _ in 0..55 {
            assert_eq!(
                cached_review_for_revision(&checkpoint, &revision),
                Some(expected.clone())
            );
        }
        assert!(cached_review_for_revision(&checkpoint, &(8, revision.1.clone())).is_none());
        assert!(
            cached_review_for_revision(
                &checkpoint,
                &(7, "changed-source-permission-or-user-requirement".into())
            )
            .is_none()
        );
        checkpoint["cachedReview"]["decision"]["reviewed"] = json!(false);
        assert!(cached_review_for_revision(&checkpoint, &revision).is_none());
        let (_, source) = control_notice_update(
            &[],
            "completionExecutionNotice",
            Some("Read the missing range."),
        )
        .unwrap();
        assert!(
            control_notice_update(
                &[source],
                "completionExecutionNotice",
                Some("Read the missing range.")
            )
            .is_none()
        );
    }

    #[test]
    fn inline_review_replaces_stale_gaps_and_keeps_protocol_repair_out_of_execution() {
        let revision = (7, "unchanged-real-receipts".to_owned());
        let mut checkpoint = json!({"unexecutedToolIntent":{"names":[]}});
        let records = ["a", "b"].into_iter().map(|name| {
            let call = json!({"id":format!("save-{name}"),"name":"filesystem.write"});
            let result = json!({"status":"completed","content":{"path":format!("C:/fixture/{name}.txt"),
                "sha256":name.repeat(64),"bytes":1}});
            completion_evidence::from_receipt(&call, &result)
        }).collect::<Vec<_>>();
        let original_records = records.clone();
        let mut task = TaskState::default();
        let mut assessment = json!({"state":"complete","source":"model-task-state","met":[
            {"requirementId":"requirement-1","item":"save a.txt","evidence":["wrong-a"]},
            {"requirementId":"requirement-2","item":"save b.txt","evidence":["wrong-b"]}]});
        let mut args = json!({"reply":assessment,"isReview":true,"evidenceRequired":true,
            "expectedRequirements":["save a.txt","save b.txt"],"validEvidence":["save-a","save-b"],"evidenceRecords":records});
        let initial = task.apply("task.decide", &args).unwrap();
        cache_completion_review(&mut checkpoint, &revision, &initial, &assessment);
        assert!(initial["remaining"].to_string().contains("a.txt"));
        assessment["met"][0]["evidence"] = json!(["save-a"]);
        args["reply"] = assessment.clone();
        let corrected = task.apply("task.decide", &args).unwrap();
        cache_completion_review(&mut checkpoint, &revision, &corrected, &assessment);
        let current = cached_review_for_revision(&checkpoint, &revision).unwrap();
        assert_eq!(current["action"], "resume");
        assert!(!current["remaining"].to_string().contains("a.txt"));
        assert!(current["remaining"].to_string().contains("b.txt"));
        assert_eq!(checkpoint["cachedReview"]["assessment"], assessment);
        let malformed = json!({"state":"unknown","source":"legacy-response"});
        args["reply"] = malformed.clone();
        let repair = task.apply("task.decide", &args).unwrap();
        cache_completion_review(&mut checkpoint, &revision, &repair, &malformed);
        assert_eq!(repair["action"], "retry-review");
        assert!(cached_review_for_revision(&checkpoint, &revision).is_none());
        assessment["met"][1]["evidence"] = json!(["save-b"]);
        args["reply"] = assessment;
        assert_eq!(task.apply("task.decide", &args).unwrap()["action"], "final");
        assert_eq!(
            records, original_records,
            "Reference repair must not mutate or replay successful receipts."
        );
    }

    #[test]
    fn completion_replanning_dispatches_loaded_actions_once_per_changed_work_revision() {
        let mut checkpoint = json!({});
        let selected = vec![json!({"name":"filesystem.read"})];
        let action = json!({"name":"filesystem.read","arguments":{"path":"saved.txt"}});
        assert!(replanned_action(&mut checkpoint, &selected, &action, &json!([1, 1, 0])).is_some());
        assert!(replanned_action(&mut checkpoint, &selected, &action, &json!([1, 1, 0])).is_none());
        assert!(replanned_action(&mut checkpoint, &selected, &action, &json!([2, 2, 0])).is_some());
        assert!(replanned_action(&mut checkpoint, &[], &action, &json!([3, 3, 0])).is_none());
        assert!(
            replanned_action(
                &mut checkpoint,
                &selected,
                &json!({"name":"filesystem.read","arguments":"not an object"}),
                &json!([3, 3, 0])
            )
            .is_none()
        );
    }

    #[test]
    fn completion_planning_repairs_specific_gaps_without_sweeping_historical_results() {
        let selected = vec![
            json!({"name":"tool.result.read"}),
            json!({"name":"filesystem.read"}),
        ];
        let mut checkpoint =
            json!({"executionChecks":[],"historicalResults":[{"id":"old-failure"}]});
        assert!(
            completion_execution_plan(&mut checkpoint, &selected, &Value::Null, &json!([1, 1, 0]))
                .is_none()
        );
        let action = json!({"name":"filesystem.read","arguments":{"path":"saved.txt","offset":40}});
        let plan =
            completion_execution_plan(&mut checkpoint, &selected, &action, &json!([1, 1, 0]))
                .unwrap();
        assert_eq!(plan["arguments"], action["arguments"]);
        assert!(
            completion_execution_plan(&mut checkpoint, &selected, &action, &json!([1, 1, 0]))
                .is_none()
        );
        checkpoint["executionCapabilities"] = json!(["filesystem.read", "document.read"]);
        let load =
            completion_execution_plan(&mut checkpoint, &selected, &Value::Null, &json!([1, 1, 0]))
                .unwrap();
        assert_eq!(load["name"], "tool.load");
        assert_eq!(load["arguments"]["names"], json!(["document.read"]));
        assert!(
            completion_execution_plan(&mut checkpoint, &selected, &Value::Null, &json!([1, 1, 0]))
                .is_none()
        );
        assert!(
            completion_execution_plan(&mut checkpoint, &selected, &action, &json!([2, 2, 0]))
                .is_some()
        );
    }

    #[tokio::test]
    async fn restored_blender_proofs_reject_false_success_and_preserve_real_progress_and_archives()
    {
        let root = std::env::temp_dir().join(format!("kynxa-restored-blender-proof-{}", new_id()));
        let chat = new_id();
        let request = new_id();
        data::ensure_conversation(&root, &chat, "Isolated Blender proof restoration").unwrap();
        let mut observations = Vec::new();
        let mut references = Vec::new();
        for (id, text) in [
            (
                "early-failure",
                "Error executing code: Could not connect to Blender.",
            ),
            (
                "saved-scene",
                "Code executed successfully: saved scene.blend",
            ),
        ] {
            let call = json!({"id":id,"name":"mcp.blender.execute_blender_code"});
            let canonical = json!({"isError":false,"content":[{"type":"text","text":text}]});
            let reference =
                data::save_tool_result(&root, &chat, &request, &call, &canonical, None).unwrap();
            let receipt = json!({"status":"completed","isError":false,"content":canonical,"resultRef":reference});
            let mut proof = completion_evidence::from_receipt(&call, &receipt);
            proof["mutationSequence"] = json!(7);
            proof["visualObserved"] = json!(true);
            observations.push(json!({"id":id,"name":call["name"],"status":"completed","ref":reference,
                "verifiedSuccess":true,"completionEvidence":[],"completionProof":proof,"mutationSequence":7}));
            references.push((reference, canonical));
        }
        let valid = observations[1].clone();
        let mut transcript = vec![json!({"observations":observations})];
        let original_transcript = transcript.clone();
        restore_completion_evidence(&root, &chat, &request, &mut transcript).await;
        assert_eq!(transcript[0]["observations"][0]["verifiedSuccess"], false);
        assert!(transcript[0]["observations"][0]["completionProof"].is_null());
        assert_eq!(transcript[0]["observations"][1], valid);
        assert_eq!(
            crate::orchestration::completion_evidence::CompletionEvidenceSet::from_observations(
                transcript
                    .iter()
                    .flat_map(|round| round["observations"].as_array().into_iter().flatten()),
                Value::Null
            )
            .valid_references,
            vec![json!("saved-scene")]
        );
        let original_checkpoint =
            json!({"id":"immutable-original-checkpoint","sha256":"c".repeat(64)});
        let mut state = LoopState {
            assistant: json!({"Content":"Already published historical output","RequestId":request}),
            progress: Default::default(),
            task: Default::default(),
            origin: json!({}),
            generated: 0,
            recoveries: 0,
            started: Instant::now(),
            applied_steering_ids: HashSet::new(),
            documents: Default::default(),
            work_progress: Default::default(),
            context: ContextState {
                history: vec![json!({"role":"assistant","content":"Original historical response"})],
                history_sources: vec![json!({"projectionOrigin":"formal-assistant"})],
                summary_sources: vec![],
                result_sources: vec![],
                runtime_snapshot: None,
                work_context_ready: true,
                work_context_reference: original_checkpoint.clone(),
                formal_sources: vec![],
            },
            tools: ToolsState {
                selected: vec![],
                effects: Default::default(),
                observations: Default::default(),
            },
            completion: CompletionState {
                retained_completion_observations: vec![],
                acceptance_reviewed: true,
                acceptance_review_revision: Some((7, "old".into())),
                last_acceptance_reminder_round: 0,
                review_checkpoint: json!({"pending":true,"attempts":3,"cachedReview":{"decision":{"reviewed":true,"action":"complete"}}}),
                mutation_sequence: 7,
                acceptance_requirements: vec!["Save the Blender scene".into()],
            },
            delivery: DeliveryState {
                segments: vec![
                    json!({"content":"Already published historical output","status":"completed"}),
                ],
                pending_delivery:
                    "Old draft claims Blender is disconnected and the scene is missing".into(),
                delivery_continuing: true,
                last_progress_ms: 0,
                model_started: None,
                generation_duration_base_ms: None,
                timeline_order: 0,
            },
            visual: VisualState {
                visual_readback_delivered: true,
                pending_visual_readback: false,
                pending_visual_hashes: vec![],
            },
            cache: CacheState {
                prefix_diagnostics: Default::default(),
                tool_series: Default::default(),
                previous_summary: Value::Null,
            },
            transcript: TranscriptState {
                rounds: original_transcript.clone(),
                pending_context_before: vec![],
            },
        };
        let call = json!({"id":"saved-effect","name":"filesystem.write","arguments":{"path":"scene.blend","content":"saved"}});
        let receipt = json!({"status":"completed","executed":true,"isError":false,"content":{"path":"C:/fixture/scene.blend"}});
        state.tools.effects.apply("effects.observe", &json!({"call":call,"result":receipt,"observation":false,"workspaceRoot":"C:/fixture"})).unwrap();
        state
            .tools
            .effects
            .apply("effects.protect", &json!({"includeCurrent":true}))
            .unwrap();
        state.bind_pending_delivery(&json!({"source":"model-task-state","state":"complete"}));
        let original_assistant = state.assistant.clone();
        let original_history = state.context.history.clone();
        let original_segments = state.delivery.segments.clone();
        let before = restored_completion_projection(&state);
        state.transcript.rounds = transcript.clone();
        reconcile_restored_delivery(&mut state, &before);
        assert!(state.delivery.pending_delivery.is_empty());
        assert!(!state.delivery.delivery_continuing && !state.completion.acceptance_reviewed);
        assert!(state.completion.acceptance_review_revision.is_none());
        assert_eq!(state.completion.review_checkpoint["pending"], false);
        assert!(state.completion.review_checkpoint["cachedReview"].is_null());
        assert_eq!(
            state.completion.review_checkpoint["restorationCorrection"]["evidenceChanged"],
            true
        );
        assert_eq!(
            state.completion.review_checkpoint["restorationCorrection"]["sourceCheckpoint"],
            original_checkpoint
        );
        assert_eq!(state.context.work_context_reference, original_checkpoint);
        assert_eq!(state.assistant, original_assistant);
        assert_eq!(state.delivery.segments, original_segments);
        assert_eq!(
            &state.context.history[..original_history.len()],
            original_history.as_slice()
        );
        assert_eq!(
            state.context.history_sources.last().unwrap()["projectionOrigin"],
            "broker-control"
        );
        let notice = state.context.history.last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(notice.contains("early-failure") && notice.contains("saved-scene"));
        assert!(notice.contains("not current service availability"));
        assert_eq!(state.transcript.rounds, transcript);
        assert_eq!(
            original_transcript[0]["observations"][0]["verifiedSuccess"],
            true
        );
        assert_eq!(
            state
                .tools
                .effects
                .apply(
                    "effects.previous",
                    &json!({"call":call,"observation":false})
                )
                .unwrap()["result"],
            receipt
        );
        let corrected = restored_completion_projection(&state);
        let history_length = state.context.history.len();
        reconcile_restored_delivery(&mut state, &corrected);
        assert_eq!(
            state.context.history.len(),
            history_length,
            "an unchanged restore without a draft needs no new notice"
        );
        state.delivery.pending_delivery =
            "Old unbound draft claims everything was completed.".into();
        state.completion.review_checkpoint["pending"] = json!(true);
        state.completion.acceptance_reviewed = true;
        reconcile_restored_delivery(&mut state, &corrected);
        assert!(
            state.delivery.pending_delivery.is_empty() && !state.completion.acceptance_reviewed
        );
        assert_eq!(
            state.completion.review_checkpoint["restorationCorrection"]["evidenceChanged"],
            false
        );
        assert_eq!(
            state.completion.review_checkpoint["restorationCorrection"]["unboundDelivery"],
            true
        );
        let complete = json!({"source":"model-task-state","state":"complete"});
        state.delivery.pending_delivery =
            "Current version is ready for its normal acceptance review.".into();
        state.bind_pending_delivery(&complete);
        let current_draft = state.delivery.pending_delivery.clone();
        reconcile_restored_delivery(&mut state, &corrected);
        assert_eq!(state.delivery.pending_delivery, current_draft);
        assert!(!state.pending_delivery_matches_blocker(&complete));
        let original_requirements = state.completion.acceptance_requirements.clone();
        for changed in ["requirements", "authority", "body"] {
            state.delivery.pending_delivery = current_draft.clone();
            state.bind_pending_delivery(&complete);
            match changed {
                "requirements" => state
                    .completion
                    .acceptance_requirements
                    .push("A new user requirement".into()),
                "authority" => {
                    state.context.runtime_snapshot =
                        Some(json!({"source":{"authorityHash":"changed-authority"}}))
                }
                _ => state
                    .delivery
                    .pending_delivery
                    .push_str(" Changed after binding."),
            }
            reconcile_restored_delivery(&mut state, &corrected);
            assert!(
                state.delivery.pending_delivery.is_empty(),
                "A changed {changed} cannot reuse the old draft."
            );
            state.completion.acceptance_requirements = original_requirements.clone();
            state.context.runtime_snapshot = None;
        }
        let history_length = state.context.history.len();
        let blocked = json!({"source":"model-task-state","state":"blocked","blocker":"authorization",
            "reason":"The requested operation was denied by the execution environment."});
        state
            .task
            .apply("task.reviewResult", &json!({"value":blocked}))
            .unwrap();
        state.delivery.pending_delivery =
            "Operation was not executed because authorization was denied.".into();
        state.completion.review_checkpoint["pending"] = json!(false);
        state.completion.review_checkpoint["attempts"] = json!(3);
        let revision = acceptance_revision(&state);
        cache_completion_review(
            &mut state.completion.review_checkpoint,
            &revision,
            &json!({"action":"interrupt","reviewed":true,"code":"TASK_AUTHORIZATION_REQUIRED"}),
            &blocked,
        );
        state.bind_pending_delivery(&blocked);
        let binding = state.completion.review_checkpoint["pendingDelivery"].clone();
        checkpoint_completion_review(&mut state, false, true, 3);
        cache_completion_review(
            &mut state.completion.review_checkpoint,
            &revision,
            &json!({"action":"interrupt","reviewed":true,"code":"TASK_AUTHORIZATION_REQUIRED"}),
            &blocked,
        );
        assert_eq!(
            state.completion.review_checkpoint["pendingDelivery"],
            binding
        );
        assert!(state.pending_delivery_matches_blocker(&blocked));
        let mut different_blocker = blocked.clone();
        different_blocker["reason"] =
            json!("Another operation now requires a different authorization.");
        assert!(!state.pending_delivery_matches_blocker(&different_blocker));
        let saved_draft = state.delivery.pending_delivery.clone();
        let saved_assessment = state.completion.review_checkpoint.clone();
        let saved_task = state.task.apply("task.state", &json!({})).unwrap();
        reconcile_restored_delivery(&mut state, &corrected);
        assert_eq!(state.delivery.pending_delivery, saved_draft);
        assert_eq!(state.completion.review_checkpoint, saved_assessment);
        assert_eq!(
            state.task.apply("task.state", &json!({})).unwrap(),
            saved_task
        );
        assert_eq!(state.context.history.len(), history_length);
        assert_eq!(state.assistant, original_assistant);
        state.assistant["Status"] = json!("completed");
        reconcile_restored_delivery(&mut state, &corrected);
        assert_eq!(
            state.delivery.pending_delivery, saved_draft,
            "a completed message alone cannot revoke its draft"
        );
        assert_eq!(state.completion.review_checkpoint, saved_assessment);
        assert_eq!(
            state.task.apply("task.state", &json!({})).unwrap(),
            saved_task
        );
        state.assistant = original_assistant.clone();
        state.assistant["LatestUserSteering"] =
            json!({"userMessageId":"actual-user-revision","text":"Revise the requested output."});
        reconcile_restored_delivery(&mut state, &corrected);
        assert!(state.delivery.pending_delivery.is_empty());
        assert_eq!(
            state.completion.review_checkpoint["restorationCorrection"]["requestedDeliveryRevision"],
            true
        );
        state.assistant = original_assistant.clone();
        let saved_transcript = state.transcript.rounds.clone();
        let saved_review = state.completion.review_checkpoint.clone();
        state.assistant = original_assistant.clone();
        state.transcript.rounds = saved_transcript;
        state.completion.review_checkpoint = saved_review;
        let pending_wire = crate::models::protocol::wire_name("mcp.blender.execute_blender_code");
        let catalog = vec![json!({"name":"mcp.blender.execute_blender_code"})];
        assert!(retain_unexecuted_tool_intent(
            &mut state,
            &[pending_wire],
            &catalog,
            127
        ));
        assert!(crate::orchestration::task::has_unexecuted_tool_intent(
            &state.completion.review_checkpoint
        ));
        assert_eq!(
            state.completion.review_checkpoint["unexecutedToolIntent"]["names"],
            json!(["mcp.blender.execute_blender_code"])
        );
        state.delivery.pending_delivery =
            "Superseded answer calls the unexecuted action optional".into();
        reconcile_restored_delivery(&mut state, &corrected);
        assert!(
            crate::orchestration::task::has_unexecuted_tool_intent(
                &state.completion.review_checkpoint
            ),
            "restoring a draft cannot drop the pending action"
        );
        assert_eq!(state.assistant, original_assistant);
        for (reference, canonical) in references {
            let archive =
                data::load_private_tool_result(&root, &chat, reference["id"].as_str().unwrap())
                    .unwrap();
            assert_eq!(archive["canonical"], canonical);
            assert_eq!(archive["sha256"], reference["sha256"]);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn visual_completion_proofs_require_the_matching_dispatched_image_and_keep_metadata() {
        let call = json!({"id":"actual-image","name":"browser.screenshot"});
        let receipt = json!({"status":"completed","content":{"path":"C:/fixture/view.png",
            "sha256":"a".repeat(64),"content":[{"type":"image","mimeType":"image/png","data":"bmV3LWltYWdl"}]}});
        let proof = completion_evidence::from_receipt(&call, &receipt);
        let mut observations = vec![json!({"id":call["id"],"completionProof":proof})];
        let wrong = task::receipt_visual_hashes(&json!({"content":[
            {"type":"image","mimeType":"image/png","data":"b2xkLWltYWdl"}]}));
        mark_visual_proofs(&mut observations, &wrong);
        assert_eq!(observations[0]["completionProof"]["visualObserved"], false);
        mark_visual_proofs(
            &mut observations,
            &task::receipt_visual_hashes(&receipt["content"]),
        );
        assert_eq!(observations[0]["completionProof"]["visualObserved"], true);
        assert_eq!(
            observations[0]["completionProof"]["targets"],
            proof["targets"]
        );
        assert_eq!(
            observations[0]["completionProof"]["visualHashes"],
            proof["visualHashes"]
        );
    }
}

#[cfg(test)]
mod steering_stop_tests {
    use super::steering_means_stop;

    #[test]
    fn stop_words_are_not_adopted_as_new_requirements() {
        for text in ["先停下", "算了不用做", "取消吧", "please stop now"] {
            assert!(steering_means_stop(text), "{text}");
        }
        for text in ["另外把结果写到 squares.txt", "顺便加一个非停止条件的测试用例说明"] {
            assert!(!steering_means_stop(text), "{text}");
        }
        assert!(!steering_means_stop("add a nonstop mode"));
    }
}
