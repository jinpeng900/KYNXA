//! Task-local progress and delivery priorities derived from actual work receipts.
//! 从真实工作回执派生任务进展与交付优先级；不执行工具、不授予权限、不拥有正式账本。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const STAGNANT_ROUNDS_BEFORE_CORRECTION: u64 = 3;
const INITIAL_DISCOVERY_ROUNDS: u64 = 2;
const IDENTICAL_FAILURE_ATTEMPTS: u64 = 2;

/// Failure fingerprints travel in the existing task checkpoint, never in a second effect ledger.
/// 失败指纹随原任务检查点保存；不取代正式副作用与未知结果核验账本。
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeterministicFailures {
    actions: BTreeMap<String, BTreeMap<String, FailedAction>>,
    observed_conditions: BTreeMap<String, String>,
    observed_failures: BTreeSet<String>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct FailedAction {
    attempts: u64,
    code: String,
}

impl DeterministicFailures {
    /// Suppress only the unchanged failed action; callers still settle a paired tool result.
    /// 只抑制条件未变的失败动作；调用端仍配对工具回执，任务和其他操作继续。
    pub fn before_dispatch(&self, call: &Value, definition: &Value, environment: &Value) -> Option<Value> {
        let (key, class, failure) = ["invalid-arguments", "capability-unavailable", "target-unavailable"].into_iter().find_map(|class| {
            let key = self.failure_key(call, definition, environment, class);
            self.actions.get(&key)?.get(class).filter(|failure| failure.attempts >= IDENTICAL_FAILURE_ATTEMPTS)
                .map(|failure| (key, class, failure))
        })?;
        Some(json!({"status":"error","isError":true,"executed":false,
            "requestDispatched":false,"recoverable":true,"code":"TOOL_ACTION_REQUIRES_CORRECTION",
            "content":{"code":"TOOL_ACTION_REQUIRES_CORRECTION","originalCode":failure.code,
                "failureClass":class,"failureFingerprint":key,"executed":false,"requestDispatched":false,
                "notice":"This unchanged action already failed deterministically. Continue the same task: correct its parameters using the real schema, use an alternative capability, or observe the relevant target/configuration change before retrying. Preserve successful output and receipts; an unknown write must be verified, never replayed. Do not repeat this unchanged call or end the task because this action was suppressed. / 相同条件下该动作已确定失败；在原任务内修正参数、换可用能力或核实相关条件变化后再试，保留成果和成功回执，未知写入先核验。"}}))
    }

    pub fn observe(&mut self, call: &Value, definition: &Value, environment: &Value, receipt: &Value) {
        if receipt["code"] == "TOOL_ACTION_REQUIRES_CORRECTION"
            || receipt["reused"] == true && !receipt["resultRef"].is_object() {
            return;
        }
        if let Some((class, code)) = deterministic_failure(receipt) {
            let key = self.failure_key(call, definition, environment, class);
            let receipt_identity = fingerprint(&json!([key, class,
                environment.get("operationIdentity").unwrap_or(&call["id"])]));
            if !self.observed_failures.insert(receipt_identity) { return; }
            let failure = self.actions.entry(key).or_default().entry(class.into()).or_default();
            failure.attempts = failure.attempts.saturating_add(1);
            failure.code = code;
        } else if receipt["reused"] != true && settled_success(receipt) {
            for class in ["invalid-arguments", "capability-unavailable", "target-unavailable"] {
                let key = self.failure_key(call, definition, environment, class);
                self.actions.remove(&key);
            }
            // Only native receipt state changes conditions; call IDs, reasons and archive IDs do not.
            // 条件只取真实回执状态；调用 ID、说明和归档 ID 变化不能重置保护。
            if let Some(condition) = observed_condition(receipt) {
                for target in action_targets(call) {
                    self.observed_conditions.insert(condition_key(environment, &target), condition.clone());
                }
            }
        }
    }

    pub fn action_key(&self, call: &Value, definition: &Value, environment: &Value) -> String {
        self.scoped_action_key(call, definition, environment, true)
    }

    fn failure_key(&self, call: &Value, definition: &Value, environment: &Value, class: &str) -> String {
        self.scoped_action_key(call, definition, environment, class != "invalid-arguments")
    }

    fn scoped_action_key(&self, call: &Value, definition: &Value, environment: &Value, condition_sensitive: bool) -> String {
        let arguments = call.get("arguments").or_else(|| call.get("input")).unwrap_or(&Value::Null);
        let mut arguments = arguments.clone();
        if let Some(fields) = arguments.as_object_mut() {
            for explanation in ["reason", "explanation", "rationale"] { fields.remove(explanation); }
        }
        let mut conditions = action_targets(call).into_iter().filter(|_| condition_sensitive).map(|target| {
            let observed = self.observed_conditions.get(&condition_key(environment, &target));
            json!([target, observed])
        }).collect::<Vec<_>>();
        if call["name"] == "knowledge.search" && arguments["requirementId"].is_string() {
            // Bind only the selected native requirement; unrelated additions cannot renew failed actions.
            // 只绑定当前引用的原生要求；新增无关要求不能重置其他失败动作的次数。
            let identity = arguments["requirementId"].as_str().unwrap_or("");
            let requirement = identity.strip_prefix("requirement-").and_then(|index| index.parse::<usize>().ok())
                .filter(|index| *index > 0 && identity == format!("requirement-{index}"))
                .and_then(|index| environment["configuration"]["acceptanceRequirements"].as_array()?.get(index - 1))
                .and_then(Value::as_str).filter(|text| !text.trim().is_empty());
            conditions.push(json!({"nativeRequirement":requirement}));
        }
        let mut environment = stable_environment(environment);
        if !condition_sensitive && let Some(fields) = environment.as_object_mut() {
            fields.remove("version"); fields.remove("mutationRevision");
        }
        fingerprint(&json!([call["name"], definition, arguments, environment, conditions]))
    }
}

fn stable_environment(environment: &Value) -> Value {
    let mut stable = environment.clone();
    if let Some(fields) = stable.as_object_mut() { fields.remove("operationIdentity"); }
    if let Some(configuration) = stable.get_mut("configuration").and_then(Value::as_object_mut) {
        for field in ["activeToolCallId", "activeOperationId", "activeRequestId", "activeAssistantMessageId",
            "nativeCancellationId", "approvedInvocation", "operationNamespace", "attachmentContextChars",
            "activeSteeringSequence", "renewOperationalExecutionBudget", "acceptanceRequirements"] {
            configuration.remove(field);
        }
    }
    stable
}

fn fingerprint(value: &Value) -> String {
    fn ordered(value: &Value) -> Value {
        match value {
            Value::Object(fields) => {
                let fields = fields.iter().map(|(key, value)| (key.clone(), ordered(value))).collect::<BTreeMap<_, _>>();
                serde_json::to_value(fields).unwrap_or(Value::Null)
            }
            Value::Array(items) => Value::Array(items.iter().map(ordered).collect()),
            value => value.clone(),
        }
    }
    format!("{:x}", Sha256::digest(ordered(value).to_string().as_bytes()))
}

fn action_targets(call: &Value) -> Vec<Value> {
    let arguments = call.get("arguments").or_else(|| call.get("input")).unwrap_or(&Value::Null);
    let mut targets = Vec::new();
    for arguments in [arguments, &arguments["arguments"]] {
        for field in ["path", "filePath", "url", "uri", "sourceId", "sourceIdentity", "windowId", "processId", "tabId", "sessionId"] {
            if let Some(value) = arguments.get(field).filter(|value| !value.is_null()) {
                targets.push(json!([field, value]));
            }
        }
    }
    targets.sort_by_key(Value::to_string);
    targets.dedup();
    targets
}

fn condition_key(environment: &Value, target: &Value) -> String {
    fingerprint(&json!([environment["workspace"], environment["permission"], target]))
}

fn receipt_nodes(receipt: &Value) -> Vec<&Value> {
    let mut nodes = vec![receipt];
    let mut cursor = 0;
    while cursor < nodes.len() && nodes.len() < 32 {
        let node = nodes[cursor];
        for field in ["content", "structuredContent", "value", "result", "error", "details"] {
            if let Some(value) = node.get(field).filter(|value| value.is_object()) { nodes.push(value); }
        }
        cursor += 1;
    }
    nodes
}

fn settled_success(receipt: &Value) -> bool {
    receipt["executed"] != false && receipt["status"] == "completed"
        && receipt_nodes(receipt).iter().all(|node| node["isError"] != true
            && !matches!(node["status"].as_str(), Some("unknown" | "running" | "pending" | "queued" | "waiting" | "error" | "failed"))
            && node["code"].as_str().is_none_or(str::is_empty)
            && node["exitCode"].as_i64().is_none_or(|code| code == 0))
}

fn observed_condition(receipt: &Value) -> Option<String> {
    let mut facts = Vec::new();
    for node in receipt_nodes(receipt) {
        let mut state = serde_json::Map::new();
        for field in ["sha256", "contentHash", "sourceVersion", "sourceRevision", "bindingRevision", "version",
            "exists", "isRunning", "state", "processId", "windowId", "lastModified", "modifiedAt"] {
            if let Some(value) = node.get(field).filter(|value| !value.is_null()) { state.insert(field.into(), value.clone()); }
        }
        if !state.is_empty() { facts.push(Value::Object(state)); }
    }
    (!facts.is_empty()).then(|| fingerprint(&json!(facts)))
}

fn deterministic_failure(receipt: &Value) -> Option<(&'static str, String)> {
    let nodes = receipt_nodes(receipt);
    // Ambiguous execution and transient waits always win over an embedded validation code.
    // 未知执行和暂时等待优先于嵌套参数错误；不能因此放行重放未知写入。
    if nodes.iter().any(|node| node["outcomeUnknown"] == true || node["timedOut"] == true
        || node["cancelled"] == true || node["retryable"] == true || node["requiresApproval"] == true
        || node["retryAfterMs"].as_u64().is_some_and(|delay| delay > 0)
        || node["httpStatus"].as_u64().is_some_and(|status| matches!(status, 408 | 429 | 500..=599))
        || matches!(node["status"].as_str(), Some("unknown" | "running" | "pending" | "queued" | "waiting" | "waiting-network"))
        || matches!(node["outcome"].as_str(), Some("unknown" | "running" | "pending"))
        || node["code"].as_str().is_some_and(|code| ["UNKNOWN", "TIMEOUT", "NETWORK", "CONNECTION", "RATE_LIMIT", "QUEUE", "BUSY", "CANCEL", "UNCONFIRMED"].iter().any(|part| code.contains(part))
            || matches!(code, "RESOURCE_UNAVAILABLE" | "RESOURCE_WAIT" | "RESOURCE_LIMIT"))) {
        return None;
    }
    if !nodes.iter().any(|node| node["isError"] == true || matches!(node["status"].as_str(), Some("error" | "failed"))) { return None; }
    if nodes.iter().any(|node| node["rpcCode"] == -32602) { return Some(("invalid-arguments", "MCP_INVALID_PARAMS".into())); }
    if nodes.iter().any(|node| node["rpcCode"] == -32601) { return Some(("capability-unavailable", "MCP_METHOD_NOT_FOUND".into())); }
    for node in nodes {
        let Some(code) = node["code"].as_str() else { continue; };
        let class = if code == "INVALID_TOOL_ARGUMENTS" || code == "INVALID_DOCUMENT_PROGRESS" || code == "INVALID_RETRIEVAL_INPUT"
            || code.starts_with("INVALID_SKILL_") && code.ends_with("PAGINATION")
            || matches!(code, "INVALID_TOOL_SEARCH_CURSOR" | "TOOL_SCHEMA_INVALID" | "TOOL_SCHEMA_REJECTED" | "INVALID_PARAMETERS" | "INVALID_PARAMS" | "INVALID_ARGUMENTS" | "MISSING_REQUIRED_ARGUMENT") {
            "invalid-arguments"
        } else if matches!(code, "UNSUPPORTED_TOOL_OPERATION" | "UNSUPPORTED_RUNTIME" | "UNSUPPORTED_FILE_TYPE" | "SKILL_RUNTIME_UNSUPPORTED" | "TOOL_NOT_DISCOVERED" | "TOOL_NOT_LOADED" | "TOOL_NOT_FOUND" | "MCP_TOOL_NOT_FOUND") {
            "capability-unavailable"
        } else if matches!(code, "FILE_NOT_FOUND" | "PATH_NOT_FOUND" | "SOURCE_NOT_FOUND" | "ELEMENT_NOT_FOUND" | "WINDOW_NOT_FOUND" | "INVALID_WINDOW" | "SELECTOR_NOT_FOUND" | "BROWSER_SESSION_NOT_FOUND" | "ENOENT" | "ENOTDIR") {
            "target-unavailable"
        } else { continue; };
        return Some((class, code.to_owned()));
    }
    None
}

/// The caller supplies source/range changes after canonical receipt validation.
/// 调用端在核对正式回执后提供来源与范围变化；调用理由、归档新 ID 或缓存命中不算新证据。
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoundProgress {
    pub completed_source_count: u64,
    pub evidence_changed: bool,
    pub artifact_updated: bool,
    pub new_read_range: bool,
    pub repeated_reads: u64,
    pub source_reads: u64,
    pub discovery_calls: u64,
    pub work_calls: u64,
    pub waiting_only: bool,
    /// All such actions were refused by the existing same-condition failure guard.
    /// 这些动作均被已有同条件失败保护拒绝，不把纠偏回执算成新工作。
    pub suppressed_calls: u64,
    pub strategy_changed: bool,
    /// An execution turn that only narrates unchanged work is stagnant, unlike an ordinary text answer.
    /// 执行阶段只叙述未变工作的轮次计为停滞；普通文字回答不传入此标记。
    pub execution_without_action: bool,
}

/// These counters may travel in the task checkpoint; stored document facts remain elsewhere.
/// 计数可随业务检查点保存；逐文档事实仍由现有工作记录保存，不复制另一份证据账本。
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkProgress {
    completed_source_count: u64,
    no_progress_rounds: u64,
    repeated_reads: u64,
    initial_discovery_rounds: u64,
    work_started: bool,
    correction_pending: bool,
    deterministic_failures: DeterministicFailures,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkProgressPlan {
    pub made_progress: bool,
    pub completed_source_count: u64,
    pub no_progress_rounds: u64,
    pub repeated_reads: u64,
    pub correction_due: bool,
    pub change_strategy: bool,
    #[serde(default)]
    pub wait_for_user: bool,
}

impl WorkProgress {
    pub fn failed_actions(&self) -> &DeterministicFailures {
        &self.deterministic_failures
    }

    pub fn failed_actions_mut(&mut self) -> &mut DeterministicFailures {
        &mut self.deterministic_failures
    }

    pub fn stagnant_rounds(&self) -> u64 {
        self.no_progress_rounds
    }

    pub fn needs_strategy_change(&self) -> bool {
        self.no_progress_rounds >= STAGNANT_ROUNDS_BEFORE_CORRECTION
    }

    /// A real new user instruction permits a fresh strategy without deleting accumulated evidence.
    /// 真实新指令允许重新尝试策略，不删除已累计证据和重复读取诊断。
    pub fn resume_after_user_instruction(&mut self) {
        self.no_progress_rounds = 0;
        self.correction_pending = false;
    }

    /// Stalled work can enter synthesis before its tool allowance is nearly spent.
    /// 工作停滞时提前转入综合建议；不改变授额，不把已读资料认定为任务完成。
    pub fn delivery_plan(&self, max_tool_calls: u64, used_tool_calls: u64) -> DeliveryBudgetPlan {
        let mut plan = delivery_budget_plan(max_tool_calls, used_tool_calls);
        if self.needs_strategy_change() && plan.phase == ToolBudgetPhase::Analyze {
            plan.phase = ToolBudgetPhase::Consolidate;
            plan.deprioritize_discovery_and_search = true;
        }
        plan
    }

    /// Count useful changes, not calls; genuine wait-only rounds leave stagnation unchanged.
    /// 统计有效变化而非调用数量；正式等待或状态轮询不增长、也不清除已有无进展轮次。
    pub fn observe_round(&mut self, round: &RoundProgress) -> WorkProgressPlan {
        let made_progress = round.completed_source_count > self.completed_source_count
            || round.evidence_changed
            || round.artifact_updated
            || round.new_read_range;
        // Text-only rounds carry no document count; they cannot erase the last witnessed completion watermark.
        // 纯文字轮次不携带文档计数，不能清掉已有真实完成水位再制造假进展。
        self.completed_source_count = self.completed_source_count.max(round.completed_source_count);
        self.repeated_reads = self.repeated_reads.saturating_add(round.repeated_reads);

        let has_calls = round.work_calls > 0
            || round.source_reads > 0
            || round.discovery_calls > 0
            || round.repeated_reads > 0
            || round.execution_without_action;
        let all_actions_repeated = round.repeated_reads.saturating_add(round.suppressed_calls)
            >= round.work_calls.max(1)
            && round.repeated_reads.saturating_add(round.suppressed_calls) > 0;
        // Pause only a witnessed unchanged route after correction; new methods and active waits remain possible.
        // 仅纠偏后又回到已证实不变的路径才等待用户；新方法和真实等待仍可继续。
        let wait_for_user = self.correction_pending && !made_progress && !round.waiting_only && !round.strategy_changed
            && (round.execution_without_action || all_actions_repeated);
        if made_progress {
            self.work_started = true;
            self.no_progress_rounds = 0;
            self.correction_pending = false;
        } else if round.strategy_changed {
            self.no_progress_rounds = 0;
            self.correction_pending = false;
        } else if !round.waiting_only && has_calls {
            let initial_discovery = !self.work_started
                && round.source_reads == 0
                && round.repeated_reads == 0
                && round.discovery_calls > 0
                && round.work_calls <= round.discovery_calls;
            if initial_discovery && self.initial_discovery_rounds < INITIAL_DISCOVERY_ROUNDS {
                self.initial_discovery_rounds += 1;
            } else {
                self.work_started = true;
                self.no_progress_rounds = self.no_progress_rounds.saturating_add(1);
            }
        }

        let change_strategy = self.needs_strategy_change();
        // Remind once per three stalled work rounds rather than after every polling response.
        // 每三轮停滞工作提示一次，避免每次等待回执或每轮模型回复都追加纠偏文本。
        let correction_due = change_strategy
            && has_calls
            && !round.waiting_only
            && !wait_for_user
            && self.no_progress_rounds % STAGNANT_ROUNDS_BEFORE_CORRECTION == 0;
        self.correction_pending |= correction_due;
        WorkProgressPlan {
            made_progress,
            completed_source_count: self.completed_source_count,
            no_progress_rounds: self.no_progress_rounds,
            repeated_reads: self.repeated_reads,
            correction_due,
            change_strategy,
            wait_for_user,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolBudgetPhase {
    Analyze,
    Consolidate,
    Deliver,
}

/// Priority advice never changes the caller's granted limit or exempts cached calls.
/// 优先级建议不更改调用端真实授额，也不豁免缓存调用；耗尽工具额度仍可整理已有成果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryBudgetPlan {
    pub max_tool_calls: u64,
    pub used_tool_calls: u64,
    pub remaining_tool_calls: u64,
    pub delivery_reserve_calls: u64,
    pub analysis_allowance_calls: u64,
    pub phase: ToolBudgetPhase,
    pub deprioritize_discovery_and_search: bool,
    pub tools_exhausted: bool,
}

pub fn delivery_budget_plan(max_tool_calls: u64, used_tool_calls: u64) -> DeliveryBudgetPlan {
    let remaining_tool_calls = max_tool_calls.saturating_sub(used_tool_calls);
    // Reserve about one fifth for synthesis/verification, scaled down for small grants.
    // 常用额度约保留五分之一给综合和核验；小授额同比缩小，不锁死十二次调用。
    let proportional_reserve = max_tool_calls / 5 + u64::from(max_tool_calls % 5 != 0);
    let small_grant_cap = (max_tool_calls / 3).max(u64::from(max_tool_calls > 0));
    let delivery_reserve_calls = proportional_reserve.clamp(12, 48).min(small_grant_cap);
    let final_delivery_threshold = (delivery_reserve_calls / 4).clamp(1, 4);
    let phase = if remaining_tool_calls <= final_delivery_threshold {
        ToolBudgetPhase::Deliver
    } else if remaining_tool_calls <= delivery_reserve_calls {
        ToolBudgetPhase::Consolidate
    } else {
        ToolBudgetPhase::Analyze
    };

    DeliveryBudgetPlan {
        max_tool_calls,
        used_tool_calls,
        remaining_tool_calls,
        delivery_reserve_calls,
        analysis_allowance_calls: remaining_tool_calls.saturating_sub(delivery_reserve_calls),
        phase,
        deprioritize_discovery_and_search: phase != ToolBudgetPhase::Analyze,
        tools_exhausted: remaining_tool_calls == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bad_call(id: &str) -> Value {
        json!({"id":id,"name":"filesystem.read","arguments":{"path":"notes.txt","offset":-1}})
    }

    fn failed(code: &str) -> Value {
        json!({"status":"error","isError":true,"executed":false,"code":code})
    }

    #[test]
    fn deterministic_failures_allow_one_retry_then_preserve_correction_across_checkpoint() {
        let mut progress = WorkProgress::default();
        let definition = json!({"name":"filesystem.read","inputSchema":{"offset":{"minimum":0}}});
        let environment = json!({"workspace":"fixture","configuration":{"activeToolCallId":"one"}});
        for id in ["one", "two"] {
            let call = bad_call(id);
            assert!(progress.failed_actions().before_dispatch(&call, &definition, &environment).is_none());
            progress.failed_actions_mut().observe(&call, &definition, &environment, &failed("INVALID_TOOL_ARGUMENTS"));
        }
        let saved = serde_json::to_vec(&progress).unwrap();
        let restored: WorkProgress = serde_json::from_slice(&saved).unwrap();
        let mut call = bad_call("different-call-id");
        call["arguments"]["reason"] = json!("another explanation");
        let mut current = environment.clone();
        current["configuration"]["activeToolCallId"] = json!("different-call-id");
        let blocked = restored.failed_actions().before_dispatch(&call, &definition, &current).unwrap();
        assert_eq!(blocked["executed"], false);
        assert_eq!(blocked["recoverable"], true);
        assert_eq!(blocked["content"]["originalCode"], "INVALID_TOOL_ARGUMENTS");
        assert_eq!(restored.stagnant_rounds(), 0);
        assert_eq!(restored.delivery_plan(100, 2).remaining_tool_calls, 98);
        assert!(serde_json::from_value::<WorkProgress>(json!({"workStarted":true})).unwrap().work_started);
    }

    #[test]
    fn corrected_parameters_new_definition_and_real_conditions_reopen_only_the_action() {
        let mut failures = DeterministicFailures::default();
        let call = bad_call("one");
        let definition = json!({"schemaVersion":1});
        let environment = json!({"workspace":"fixture","permission":"smart","mutationRevision":0});
        failures.observe(&call, &definition, &environment, &failed("ENOENT"));
        failures.observe(&bad_call("two"), &definition, &environment, &failed("ENOENT"));
        assert!(failures.before_dispatch(&call, &definition, &environment).is_some());
        let mut corrected = call.clone();
        corrected["arguments"]["offset"] = json!(0);
        assert!(failures.before_dispatch(&corrected, &definition, &environment).is_none());
        assert!(failures.before_dispatch(&call, &json!({"schemaVersion":2}), &environment).is_none());
        assert!(failures.before_dispatch(&call, &definition, &json!({"workspace":"fixture","permission":"smart","mutationRevision":1})).is_none());
        let stat = json!({"id":"actual-stat","name":"filesystem.stat","arguments":{"path":"notes.txt"}});
        failures.observe(&stat, &json!({}), &environment, &json!({"status":"completed","content":{"exists":true,"sha256":"a".repeat(64)}}));
        assert!(failures.before_dispatch(&call, &definition, &environment).is_none());

        let mut failures = DeterministicFailures::default();
        let initial = json!({"configuration":{"acceptanceRequirements":["first requirement"]}});
        let search = |id:&str|json!({"id":id,"name":"knowledge.search","arguments":{"query":"facts","requirementId":"requirement-3"}});
        for id in ["one","two"] {
            failures.observe(&search(id), &definition, &initial, &failed("INVALID_RETRIEVAL_INPUT"));
            failures.observe(&bad_call(id), &definition, &initial, &failed("INVALID_TOOL_ARGUMENTS"));
        }
        assert!(failures.before_dispatch(&search("retry"), &definition, &initial).is_some());
        let unrelated = json!({"configuration":{"acceptanceRequirements":["first requirement","second requirement"]}});
        assert!(failures.before_dispatch(&search("retry"), &definition, &unrelated).is_some());
        let now_valid = json!({"configuration":{"acceptanceRequirements":["first requirement","second requirement","new native third requirement"]}});
        assert!(failures.before_dispatch(&search("retry"), &definition, &now_valid).is_none());
        assert!(failures.before_dispatch(&bad_call("retry"), &definition, &now_valid).is_some());
    }

    #[test]
    fn unrelated_state_observation_does_not_clear_a_failed_target() {
        let mut failures = DeterministicFailures::default();
        for id in ["one", "two"] { failures.observe(&bad_call(id), &json!({}), &json!({}), &failed("ENOENT")); }
        failures.observe(&json!({"id":"other","name":"filesystem.stat","arguments":{"path":"other.txt"}}),
            &json!({}), &json!({}), &json!({"status":"completed","content":{"exists":true,"sha256":"a".repeat(64)}}));
        assert!(failures.before_dispatch(&bad_call("retry"), &json!({}), &json!({})).is_some());
    }

    #[test]
    fn network_waits_polling_unknown_effects_and_generic_exits_never_enter_failure_suppression() {
        for receipt in [
            failed("MCP_CONNECTION_LOST"), failed("MCP_TIMEOUT"), failed("RESOURCE_BUSY"),
            json!({"status":"queued","isError":true,"code":"INVALID_TOOL_ARGUMENTS"}),
            json!({"status":"running","content":{"code":"INVALID_PARAMS"}}),
            json!({"status":"unknown","isError":true,"content":{"code":"INVALID_PARAMS"}}),
            json!({"status":"error","isError":true,"retryAfterMs":1000,"code":"INVALID_PARAMS"}),
            json!({"status":"completed","content":{"exitCode":1,"stderr":"invalid arguments"}}),
            json!({"status":"error","isError":true,"code":"MCP_RPC_ERROR","content":{"details":{"rpcCode":-32602,"httpStatus":503}}}),
        ] {
            let mut failures = DeterministicFailures::default();
            for id in ["one", "two", "three"] { failures.observe(&bad_call(id), &json!({}), &json!({}), &receipt); }
            assert!(failures.before_dispatch(&bad_call("retry"), &json!({}), &json!({})).is_none(), "{receipt}");
        }
    }

    #[test]
    fn rpc_invalid_params_and_resource_pagination_are_structured_deterministic_failures() {
        for receipt in [failed("INVALID_SKILL_RESOURCE_PAGINATION"), failed("INVALID_RETRIEVAL_INPUT"),
            json!({"status":"error","isError":true,"code":"MCP_RPC_ERROR","content":{"details":{"rpcCode":-32602}}})] {
            let mut failures = DeterministicFailures::default();
            for id in ["one", "two"] { failures.observe(&bad_call(id), &json!({}), &json!({}), &receipt); }
            assert_eq!(failures.before_dispatch(&bad_call("retry"), &json!({}), &json!({})).unwrap()["content"]["failureClass"], "invalid-arguments");
        }
    }

    #[test]
    fn journal_recovery_counts_each_actual_failed_dispatch_once_even_with_reused_call_ids() {
        let mut failures = DeterministicFailures::default();
        let call = bad_call("provider-reused-id");
        let receipt = json!({"status":"error","isError":true,"code":"INVALID_TOOL_ARGUMENTS","reused":true,
            "resultRef":{"id":"owned-archive","sha256":"a".repeat(64)}});
        let first = json!({"operationIdentity":"round-one"});
        failures.observe(&call, &json!({}), &first, &receipt);
        for _ in 0..5 { failures.observe(&call, &json!({}), &first, &receipt); }
        assert!(failures.before_dispatch(&call, &json!({}), &first).is_none());
        let mut restored: DeterministicFailures = serde_json::from_slice(&serde_json::to_vec(&failures).unwrap()).unwrap();
        restored.observe(&call, &json!({}), &json!({"operationIdentity":"round-two"}), &receipt);
        assert!(restored.before_dispatch(&call, &json!({}), &json!({"operationIdentity":"round-three"})).is_some());
    }

    #[test]
    fn argument_order_and_dynamic_operation_identity_do_not_change_failure_identity() {
        let failures = DeterministicFailures::default();
        let left = json!({"id":"left","name":"tool","arguments":{"path":"a","options":{"limit":2,"offset":0}}});
        let right: Value = serde_json::from_str(r#"{"id":"right","name":"tool","arguments":{"options":{"offset":0,"limit":2},"path":"a","reason":"new reason"}}"#).unwrap();
        assert_eq!(failures.action_key(&left,&json!({}),&json!({"operationIdentity":"one"})),
            failures.action_key(&right,&json!({}),&json!({"operationIdentity":"two"})));
    }

    #[test]
    fn completion_prose_without_action_changes_strategy_without_ending_or_spending_tools() {
        let mut progress = WorkProgress::default();
        for round in 1..=3 {
            let plan = progress.observe_round(&RoundProgress { execution_without_action:true,..Default::default() });
            assert_eq!(plan.no_progress_rounds,round);
            assert_eq!(plan.correction_due,round==3);
        }
        assert!(progress.needs_strategy_change());
        assert_eq!(progress.delivery_plan(256,14).remaining_tool_calls,242);
        let useful = progress.observe_round(&RoundProgress { new_read_range:true,source_reads:1,..Default::default() });
        assert!(useful.made_progress); assert_eq!(useful.no_progress_rounds,0);
    }

    #[test]
    fn repeated_reads_correct_strategy_without_blocking_new_evidence() {
        let mut progress = WorkProgress::default();
        let first = progress.observe_round(&RoundProgress {
            completed_source_count: 1,
            source_reads: 1,
            new_read_range: true,
            ..Default::default()
        });
        assert!(first.made_progress);
        for index in 1..=3 {
            let plan = progress.observe_round(&RoundProgress {
                completed_source_count: 1,
                source_reads: 1,
                repeated_reads: 1,
                ..Default::default()
            });
            assert_eq!(plan.no_progress_rounds, index);
            assert_eq!(plan.correction_due, index == 3);
            assert_eq!(progress.stagnant_rounds(), index);
            assert_eq!(progress.needs_strategy_change(), index == 3);
            let delivery = progress.delivery_plan(80, 29);
            assert_eq!(delivery.max_tool_calls, 80);
            assert_eq!(delivery.remaining_tool_calls, 51);
            assert_eq!(delivery.deprioritize_discovery_and_search, index == 3);
            assert_eq!(
                delivery.phase,
                if index == 3 {
                    ToolBudgetPhase::Consolidate
                } else {
                    ToolBudgetPhase::Analyze
                }
            );
        }
        let resumed = progress.observe_round(&RoundProgress {
            completed_source_count: 2,
            artifact_updated: true,
            ..Default::default()
        });
        assert!(resumed.made_progress);
        assert!(!resumed.change_strategy);
        assert_eq!(resumed.repeated_reads, 3);
        assert_eq!(progress.stagnant_rounds(), 0);
        assert!(!progress.needs_strategy_change());
        assert_eq!(
            progress.delivery_plan(80, 29).phase,
            ToolBudgetPhase::Analyze
        );
    }

    #[test]
    fn setup_and_real_polling_do_not_fake_progress_or_trigger_early_correction() {
        let mut progress = WorkProgress::default();
        for _ in 0..2 {
            let plan = progress.observe_round(&RoundProgress {
                discovery_calls: 1,
                work_calls: 1,
                ..Default::default()
            });
            assert_eq!(plan.no_progress_rounds, 0);
            assert!(!plan.made_progress);
        }
        let plan = progress.observe_round(&RoundProgress {
            discovery_calls: 1,
            work_calls: 1,
            ..Default::default()
        });
        assert_eq!(plan.no_progress_rounds, 1);
        let before_wait = serde_json::to_value(&progress).unwrap();
        for _ in 0..4 {
            let waiting = progress.observe_round(&RoundProgress {
                work_calls: 1,
                waiting_only: true,
                ..Default::default()
            });
            assert!(!waiting.correction_due);
            assert_eq!(waiting.no_progress_rounds, 1);
        }
        assert_eq!(serde_json::to_value(&progress).unwrap(), before_wait);
    }

    #[test]
    fn only_a_repeated_unchanged_route_after_correction_waits_for_user() {
        let mut work = WorkProgress::default();
        let repeated = RoundProgress {work_calls:1,source_reads:1,repeated_reads:1,..Default::default()};
        for _ in 0..3 {assert!(!work.observe_round(&repeated).wait_for_user);}
        let saved = serde_json::to_value(&work).unwrap();
        let mut work: WorkProgress = serde_json::from_value(saved).unwrap();
        let waiting = work.observe_round(&RoundProgress {work_calls:1,waiting_only:true,..Default::default()});
        assert!(!waiting.wait_for_user);
        let alternative = work.observe_round(&RoundProgress {work_calls:1,..Default::default()});
        assert!(!alternative.wait_for_user);
        assert!(work.observe_round(&repeated).wait_for_user);
        let new_range = work.observe_round(&RoundProgress {work_calls:1,source_reads:1,new_read_range:true,..Default::default()});
        assert!(new_range.made_progress);
        assert!(!new_range.wait_for_user);
        assert!(!work.observe_round(&repeated).wait_for_user);
        for _ in 0..3 {work.observe_round(&repeated);}
        work.resume_after_user_instruction();
        assert!(!work.observe_round(&repeated).wait_for_user);
        let legacy: WorkProgress = serde_json::from_value(json!({"workStarted":true,"noProgressRounds":9})).unwrap();
        assert!(!legacy.correction_pending);
    }

    #[test]
    fn text_rounds_do_not_erase_source_completion_and_fake_later_progress() {
        let mut work = WorkProgress::default();
        assert!(work.observe_round(&RoundProgress {completed_source_count:2,..Default::default()}).made_progress);
        let text = work.observe_round(&RoundProgress {execution_without_action:true,..Default::default()});
        assert_eq!(text.completed_source_count,2);
        let repeated = work.observe_round(&RoundProgress {completed_source_count:2,source_reads:1,
            repeated_reads:1,work_calls:1,..Default::default()});
        assert!(!repeated.made_progress);
        assert_eq!(repeated.no_progress_rounds,2);
    }

    #[test]
    fn granted_budget_keeps_delivery_room_and_never_increases_the_limit() {
        let ordinary = delivery_budget_plan(80, 64);
        assert_eq!(ordinary.delivery_reserve_calls, 16);
        assert_eq!(ordinary.remaining_tool_calls, 16);
        assert_eq!(ordinary.phase, ToolBudgetPhase::Consolidate);
        assert_eq!(delivery_budget_plan(256, 0).delivery_reserve_calls, 48);
        assert_eq!(delivery_budget_plan(8, 0).delivery_reserve_calls, 2);
        assert_eq!(delivery_budget_plan(80, 76).phase, ToolBudgetPhase::Deliver);
        let exhausted = delivery_budget_plan(80, 81);
        assert!(exhausted.tools_exhausted);
        assert_eq!(exhausted.remaining_tool_calls, 0);
        assert_eq!(exhausted.max_tool_calls, 80);
        assert_eq!(exhausted.used_tool_calls, 81);
        assert_eq!(delivery_budget_plan(u64::MAX, 0).delivery_reserve_calls, 48);
    }
}
