//! Request-local diagnostics, mutation tracking, and version-bound validation receipts.
//! 请求内诊断、修改跟踪和绑定代码版本的验证回执；不负责执行工具或正式持久化。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod validation_scope;
pub(super) use validation_scope::{ValidationRequirement, ValidationScope, ValidationTarget};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const DIAGNOSTIC_ERROR: &str = "INVALID_TOOL_RUN_DIAGNOSTICS";
const MAX_DOCUMENT_TARGETS: usize = 4096;
const HOST_EFFECT_CLASSIFICATION_VERSION: u64 = 3;

/// Document artifacts use complete, hash-matched reads; code still needs execution checks.
/// 文档产物依据完整且哈希一致的回读核验；代码修改仍须真实执行检查，回读不证明语义正确。
#[derive(Default)]
struct DocumentValidation {
    sha256: String,
    total_characters: Option<u64>,
    ranges: Vec<(u64, u64)>,
    read_back: bool,
    result_ref: Value,
    page_count: Option<u64>,
    page_windows: BTreeMap<(u64, u64), DocumentReadWindow>,
    version_check_required: bool,
}

#[derive(Default)]
struct DocumentReadWindow {
    total_characters: Option<u64>,
    ranges: Vec<(u64, u64)>,
}

/// Request-local metrics and execution evidence never certify semantic task correctness.
/// 请求内计量和执行证据不能认证任务语义正确；计时及正式持久化仍属于调用端。
pub struct RunProgress {
    mutation_revision: u64,
    checked_mutation_revision: u64,
    validation_receipts: Vec<Value>,
    required_checks: BTreeMap<String, Value>,
    uncertain_mutations: BTreeSet<String>,
    unclassified_terminal_effects: u64,
    latest_code_version: Value,
    validated_code_version: Value,
    task_completion: Value,
    diagnostics: Value,
    artifact_revisions: BTreeSet<u64>,
    // Latest code-change obligation; settled host effects keep their counters without inventing code targets.
    // 最近一次代码变更义务；已结算宿主效果保留原计数，不凭空创造代码目标。
    code_mutation_revision: u64,
    host_effect_classification_version: u64,
    document_validations: BTreeMap<String, DocumentValidation>,
}

impl Default for RunProgress {
    fn default() -> Self {
        Self {
            mutation_revision: 0,
            checked_mutation_revision: 0,
            validation_receipts: Vec::new(),
            required_checks: BTreeMap::new(),
            uncertain_mutations: BTreeSet::new(),
            unclassified_terminal_effects: 0,
            latest_code_version: Value::Null,
            validated_code_version: Value::Null,
            task_completion: Value::Null,
            artifact_revisions: BTreeSet::new(),
            code_mutation_revision: 0,
            host_effect_classification_version: HOST_EFFECT_CLASSIFICATION_VERSION,
            document_validations: BTreeMap::new(),
            diagnostics: json!({
                "version": 1,
                "totalModelMs": 0,
                "totalToolMs": 0,
                "totalApprovalWaitMs": 0,
                "maxModelMs": 0,
                "maxToolMs": 0,
                "maxApprovalWaitMs": 0,
                "modelCalls": 0,
                "executedToolCalls": 0,
                "reusedToolCalls": 0,
                "blockedToolCalls": 0,
                "noProgressRounds": 0,
                "modelRounds": [],
                "toolCallsTiming": []
            }),
        }
    }
}

impl RunProgress {
    /// Ordinary session snapshots omit receipt histories; callers request them only for reporting.
    /// 普通会话快照不复制回执历史；仅在核对和汇报时按需取得完整视图。
    pub fn summary(&self) -> Value {
        json!({
            "mutationRevision": self.mutation_revision,
            "checkedMutationRevision": self.checked_mutation_revision,
            "receiptCount": self.validation_receipts.len()
        })
    }

    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "progress.recordModel" => self.record_model(args),
            "progress.recordTool" => self.record_tool(args),
            "progress.noProgress" => {
                let count = safe_integer(&args["count"])?;
                let next = checked_sum(&self.diagnostics["noProgressRounds"], count)?;
                self.diagnostics["noProgressRounds"] = json!(next);
                Ok(Value::Null)
            }
            "progress.observeOutcome" => {
                self.observe_outcome(args);
                Ok(Value::Null)
            }
            "progress.restoreValidation" => self.restore_validation(args),
            "progress.finalVersion" => {
                let previous = &self.latest_code_version;
                let latest = version_identity(&args["snapshot"]);
                let observed_change = previous["complete"] == true && latest["complete"] == true
                    && previous["signature"] != latest["signature"];
                self.latest_code_version = latest;
                if observed_change || !self.validated_code_version.is_null()
                    && (self.latest_code_version["complete"] != true
                        || self.latest_code_version["signature"]
                            != self.validated_code_version["signature"])
                {
                    self.mutation_revision += 1;
                    self.code_mutation_revision = self.mutation_revision;
                    self.validated_code_version = Value::Null;
                }
                Ok(Value::Null)
            }
            "progress.needsValidation" => Ok(json!(
                self.needs_validation(args["task"].as_str().unwrap_or(""))
            )),
            "progress.verification" => Ok(self.verification()),
            "progress.documentReadbackTargets" => Ok(json!(self.document_readback_targets())),
            "progress.restoreDocumentReadbackTargets" => Ok(self.restore_document_readback_targets(
                &args["targets"], args["sameTaskCheckpoint"] == true)),
            "progress.completion" => Ok(self.complete(args)),
            "progress.diagnostics" => Ok(self.diagnostics.clone()),
            "progress.field" => self.field(args["name"].as_str().unwrap_or("")),
            "progress.state" => Ok(json!({
                "mutationRevision": self.mutation_revision,
                "checkedMutationRevision": self.checked_mutation_revision,
                "validationReceipts": self.validation_receipts,
                "requiredChecks": self.required_checks.values().collect::<Vec<_>>(),
                "artifactRevisions": self.artifact_revisions,
                "codeMutationRevision": self.code_mutation_revision,
                "hostEffectClassificationVersion": self.host_effect_classification_version,
                "uncertainMutations": self.uncertain_mutations,
                "unclassifiedTerminalEffects": self.unclassified_terminal_effects,
                "latestCodeVersion": self.latest_code_version,
                "validatedCodeVersion": self.validated_code_version,
                "diagnostics": self.diagnostics,
                "taskCompletion": self.task_completion
            })),
            "progress.patch" => self.patch(args),
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
        }
    }

    fn field(&self, name: &str) -> Result<Value, String> {
        Ok(match name {
            "mutationRevision" => json!(self.mutation_revision),
            "codeMutationRevision" => json!(self.code_mutation_revision),
            "hostEffectClassificationVersion" => json!(self.host_effect_classification_version),
            "checkedMutationRevision" => json!(self.checked_mutation_revision),
            "validationReceipts" => json!(self.validation_receipts),
            "requiredChecks" => json!(self.required_checks.values().collect::<Vec<_>>()),
            "uncertainMutations" => json!(self.uncertain_mutations),
            "unclassifiedTerminalEffects" => json!(self.unclassified_terminal_effects),
            "latestCodeVersion" => self.latest_code_version.clone(),
            "validatedCodeVersion" => self.validated_code_version.clone(),
            "diagnostics" => self.diagnostics.clone(),
            _ => return Err(DIAGNOSTIC_ERROR.to_owned()),
        })
    }

    fn patch(&mut self, args: &Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or(DIAGNOSTIC_ERROR.to_owned())?;
        let value = &args["value"];
        match name {
            "mutationRevision" => {
                self.mutation_revision = safe_integer(value)?;
                self.code_mutation_revision = self.mutation_revision;
            }
            "checkedMutationRevision" => self.checked_mutation_revision = safe_integer(value)?,
            "unclassifiedTerminalEffects" => {
                self.unclassified_terminal_effects = safe_integer(value)?
            }
            "validationReceipts" => {
                let receipts = value
                    .as_array()
                    .filter(|items| items.len() <= 64)
                    .ok_or(DIAGNOSTIC_ERROR.to_owned())?;
                self.validation_receipts = receipts.clone();
                for receipt in receipts {
                    self.remember_check(receipt.clone());
                }
            }
            "uncertainMutations" => {
                let entries = value.as_array().ok_or(DIAGNOSTIC_ERROR.to_owned())?;
                self.uncertain_mutations = entries
                    .iter()
                    .map(|entry| {
                        entry
                            .as_str()
                            .map(str::to_owned)
                            .ok_or(DIAGNOSTIC_ERROR.to_owned())
                    })
                    .collect::<Result<_, _>>()?;
            }
            "latestCodeVersion" => self.latest_code_version = value.clone(),
            "validatedCodeVersion" => self.validated_code_version = value.clone(),
            "diagnostics" if value.is_object() => {
                for key in [
                    "totalModelMs",
                    "totalToolMs",
                    "totalApprovalWaitMs",
                    "maxModelMs",
                    "maxToolMs",
                    "maxApprovalWaitMs",
                    "modelCalls",
                    "executedToolCalls",
                    "reusedToolCalls",
                    "noProgressRounds",
                ] {
                    safe_integer(&value[key])?;
                }
                if !value["modelRounds"]
                    .as_array()
                    .is_some_and(|items| items.len() <= 64)
                    || !value["toolCallsTiming"]
                        .as_array()
                        .is_some_and(|items| items.len() <= 256)
                {
                    return Err(DIAGNOSTIC_ERROR.to_owned());
                }
                self.diagnostics = value.clone();
                if self.diagnostics.get("blockedToolCalls").is_none() {
                    self.diagnostics["blockedToolCalls"] = json!(0);
                }
            }
            _ => return Err(DIAGNOSTIC_ERROR.to_owned()),
        }
        Ok(Value::Null)
    }

    fn remember_check(&mut self, mut receipt: Value) {
        if !receipt.is_object() { return; }
        let identity = receipt["checkId"].as_str().filter(|id| !id.is_empty()).map(str::to_owned)
            .unwrap_or_else(|| {
                // Old checkpoints lack command identity; unrelated calls must not cancel each other.
                // 旧断点没有命令身份，按调用隔离，不能让无关成功抵消失败。
                let legacy = receipt["toolCallId"].as_str().map(str::to_owned)
                    .unwrap_or_else(|| format!("unidentified-{}", self.required_checks.len()));
                stable_hash(&json!(["legacy-validation", legacy]))
            });
        receipt["checkId"] = json!(identity);
        self.required_checks.insert(identity, receipt);
    }

    fn restore_validation(&mut self, args: &Value) -> Result<Value, String> {
        let state = args.get("state").filter(|state| state.is_object()).unwrap_or(args);
        for name in ["mutationRevision", "checkedMutationRevision", "unclassifiedTerminalEffects"] {
            if let Some(value) = state.get(name) {
                self.patch(&json!({"name":name,"value":value}))?;
            }
        }
        for name in ["latestCodeVersion", "validatedCodeVersion"] {
            if let Some(value) = state.get(name) {
                self.patch(&json!({"name":name,"value":value}))?;
            }
        }
        if let Some(entries) = state.get("uncertainMutations") {
            self.patch(&json!({"name":"uncertainMutations","value":entries}))?;
        }
        if let Some(entries) = state["artifactRevisions"].as_array() {
            self.artifact_revisions = entries.iter().map(safe_integer).collect::<Result<_, _>>()?;
        }
        if let Some(receipts) = state["validationReceipts"].as_array() {
            let receipts: Vec<Value> = receipts.iter().map(|receipt| {
                let mut receipt = receipt.clone();
                if receipt["checkId"].as_str().is_none() {
                    if let Some(legacy) = args["legacyCalls"].as_array().into_iter().flatten()
                        .find(|legacy| legacy["call"]["id"].is_string() && legacy["call"]["id"] == receipt["toolCallId"] && validation_call(&legacy["call"])) {
                        let canonical = &legacy["output"];
                        let output = canonical.get("structuredContent").filter(|value| value.is_object())
                            .or_else(|| canonical.get("value").filter(|value| value.is_object())).unwrap_or(canonical);
                        attach_check_identity(&mut receipt, &legacy["call"], output, &legacy["validationScope"]);
                    }
                }
                self.remember_check(receipt.clone());
                receipt
            }).collect();
            self.validation_receipts = receipts.iter().rev().take(64).cloned().collect();
            self.validation_receipts.reverse();
        }
        if let Some(checks) = state["requiredChecks"].as_array() {
            for check in checks { self.remember_check(check.clone()); }
        }
        if let Some(revision) = state.get("codeMutationRevision") {
            let revision = safe_integer(revision)?;
            if revision > self.mutation_revision { return Err(DIAGNOSTIC_ERROR.to_owned()); }
            self.code_mutation_revision = revision;
        } else {
            let mut revision = self.mutation_revision;
            while revision > 0 && self.artifact_revisions.contains(&revision) { revision -= 1; }
            self.code_mutation_revision = revision;
        }
        self.host_effect_classification_version = state.get("hostEffectClassificationVersion")
            .map(safe_integer).transpose()?.unwrap_or(0);
        if needs_host_effect_reclassification(state) {
            if self.legacy_host_effects_have_no_code_targets(&args["legacyCalls"]) {
                self.code_mutation_revision = 0;
                self.host_effect_classification_version = HOST_EFFECT_CLASSIFICATION_VERSION;
            }
        } else {
            self.host_effect_classification_version = self.host_effect_classification_version.max(HOST_EFFECT_CLASSIFICATION_VERSION);
        }
        Ok(json!({"hostEffectClassificationVersion":self.host_effect_classification_version}))
    }

    fn code_mutation_since(&self, revision: u64) -> bool {
        self.code_mutation_revision > revision
    }

    fn legacy_host_effects_have_no_code_targets(&self, legacy_calls: &Value) -> bool {
        // Owned receipts classify targets, not success; retain every original status and counter.
        // 自有回执用于分类目标，不证明成功；原状态与计数全部保留。
        if self.unclassified_terminal_effects == 0
            || !self.required_checks.is_empty()
            || observed_code_targets(&self.latest_code_version) || observed_code_targets(&self.validated_code_version)
            || self.mutation_revision.saturating_sub(self.artifact_revisions.len() as u64) != self.unclassified_terminal_effects {
            return false;
        }
        let calls = legacy_calls.as_array().into_iter().flatten().filter(|saved|
            host_effect_tool(saved["call"]["name"].as_str().unwrap_or("")) && !validation_call(&saved["call"]))
            .collect::<Vec<_>>();
        let ids = calls.iter().filter_map(|saved| saved["call"]["id"].as_str().filter(|id| !id.is_empty()))
            .collect::<BTreeSet<_>>();
        calls.len() as u64 == self.unclassified_terminal_effects && ids.len() == calls.len()
            && self.uncertain_mutations.iter().all(|id| ids.contains(id.as_str()))
            && calls.iter().all(|saved| {
                let output = execution_output(&saved["output"]);
                saved["result"]["executed"] != false && output.is_object()
                    && matches!(saved["result"]["status"].as_str(), Some("completed" | "error" | "unknown" | "cancelled"))
                    && !observed_code_targets(&saved["beforeVersion"]) && !observed_code_targets(&saved["afterVersion"])
                    && !(output["sandbox"] == "appcontainer" && output["tokenVerified"] == true && output["workspaceCopy"] == true)
            })
    }

    fn required_check_views(&self) -> Vec<Value> {
        self.required_check_views_for_scope(None)
    }

    fn required_check_views_for_scope(&self, scope: Option<&ValidationScope>) -> Vec<Value> {
        let latest_check = self.validation_receipts.last().unwrap_or(&Value::Null);
        self.required_checks.values().map(|receipt| {
            let observed = &receipt["codeVersionCoverage"]["after"];
            // A fresh process may observe a different subset; only a broker-known workspace change can retire it.
            // 重启后的观察子集可能不同，只有程序明确记录工作区切换时才允许旧目标退出当前核验。
            let workspace_changed = receipt["workspaceKnown"] == true && latest_check["workspaceKnown"] == true
                && receipt["workspaceId"] != latest_check["workspaceId"];
            let outside_scope = scope.is_some_and(|scope| scope.excludes(receipt));
            let active = !(outside_scope || workspace_changed && disjoint_targets(observed, &self.latest_code_version));
            let changed = observed["complete"] == true && self.latest_code_version["complete"] == true
                && observed["signature"] != self.latest_code_version["signature"]
                || self.code_mutation_since(receipt["mutationRevision"].as_u64().unwrap_or(0));
            let covered = receipt["passed"] == true && receipt["coversObservedRevision"] == true
                && observed["complete"] == true && self.latest_code_version["complete"] == true
                && observed["signature"] == self.latest_code_version["signature"] && !changed;
            let resolution = if !active { "inactive" }
                else if changed { "needs-revalidation" }
                else if receipt.get("exitCode").is_some_and(Value::is_null)
                    || receipt["outcomeUnknown"] == true || receipt["cancelled"] == true || receipt["timedOut"] == true { "execution-unconfirmed" }
                else if receipt["passed"] != true { "failed" }
                else if !covered { "version-unverified" }
                else { "passed" };
            let mut view = receipt.clone();
            view["active"] = json!(active);
            view["current"] = json!(active && !changed && (receipt["passed"] != true || covered));
            view["resolution"] = json!(resolution);
            if outside_scope { view["scopeResolution"] = json!("outside-current-requirements"); }
            view.as_object_mut().unwrap().remove("acceptanceScope");
            view
        }).collect()
    }

    fn record_model(&mut self, args: &Value) -> Result<Value, String> {
        let duration = safe_integer(&args["durationMs"])?;
        let round = metric_round(&args["round"])?;
        let total = checked_sum(&self.diagnostics["totalModelMs"], duration)?;
        let calls = checked_sum(&self.diagnostics["modelCalls"], 1)?;
        let maximum = safe_integer(&self.diagnostics["maxModelMs"])?.max(duration);
        // Validate every counter before publishing any part of the observation.
        // 先验证全部计数，避免溢出后发布半份计量状态。
        self.diagnostics["totalModelMs"] = json!(total);
        self.diagnostics["maxModelMs"] = json!(maximum);
        self.diagnostics["modelCalls"] = json!(calls);
        append_bounded(
            &mut self.diagnostics["modelRounds"],
            json!({"round": round, "durationMs": duration}),
            64,
        );
        Ok(Value::Null)
    }

    fn record_tool(&mut self, args: &Value) -> Result<Value, String> {
        // Approval waiting is measured separately; reused or blocked calls never count as dispatches.
        // 审批等待独立计量；复用回执或被阻止的调用不能计为真实派发。
        let duration = safe_integer(&args["durationMs"])?;
        let approval = safe_integer(&args["approvalMs"])?;
        let round = metric_round(&args["round"])?;
        let id = args["id"].as_str().ok_or(DIAGNOSTIC_ERROR.to_owned())?;
        let reused = args["reused"]
            .as_bool()
            .ok_or(DIAGNOSTIC_ERROR.to_owned())?;
        let dispatched = args
            .get("dispatched")
            .map_or(Some(!reused), Value::as_bool)
            .ok_or(DIAGNOSTIC_ERROR.to_owned())?
            && !reused;
        if id.is_empty()
            || id.encode_utf16().count() > 200
            || id.chars().any(|c| matches!(c, '\0' | '\r' | '\n'))
            || approval > duration
        {
            return Err(DIAGNOSTIC_ERROR.to_owned());
        }
        let execution = duration - approval;
        let total_tool = checked_sum(&self.diagnostics["totalToolMs"], execution)?;
        let total_approval = checked_sum(&self.diagnostics["totalApprovalWaitMs"], approval)?;
        let executed = checked_sum(
            &self.diagnostics["executedToolCalls"],
            u64::from(dispatched),
        )?;
        let cached = checked_sum(&self.diagnostics["reusedToolCalls"], u64::from(reused))?;
        let blocked = checked_sum(
            &self.diagnostics["blockedToolCalls"],
            u64::from(!reused && !dispatched),
        )?;
        let maximum_tool = safe_integer(&self.diagnostics["maxToolMs"])?.max(execution);
        let maximum_approval = safe_integer(&self.diagnostics["maxApprovalWaitMs"])?.max(approval);
        for (key, value) in [
            ("totalToolMs", total_tool),
            ("totalApprovalWaitMs", total_approval),
            ("maxToolMs", maximum_tool),
            ("maxApprovalWaitMs", maximum_approval),
            ("executedToolCalls", executed),
            ("reusedToolCalls", cached),
            ("blockedToolCalls", blocked),
        ] {
            self.diagnostics[key] = json!(value);
        }
        append_bounded(
            &mut self.diagnostics["toolCallsTiming"],
            json!({
                "toolCallId": id,
                "round": round,
                "durationMs": duration,
                "approvalMs": approval,
                "reused": reused,
                "dispatched": dispatched
            }),
            256,
        );
        Ok(Value::Null)
    }

    fn observe_outcome(&mut self, args: &Value) {
        let call = &args["call"];
        let result = &args["result"];
        let name = call["name"].as_str().unwrap_or("");
        let status = result["status"].as_str().unwrap_or("");
        let completed =
            !truthy(&result["isError"]) && !["unknown", "cancelled", "error"].contains(&status);
        // Transparent loading may retain the model's original unavailable flag; a real dispatched receipt wins.
        // 透明加载后原始模型调用仍可保留未加载标记；实际已派发回执优先，未执行调用继续不计完成覆盖。
        if truthy(&result["reused"]) || result["executed"] == false
            || truthy(&call["unavailable"]) && result["executed"] != true
        {
            return;
        }
        let uncertain =
            ["unknown", "cancelled"].contains(&status) || result["code"] == "TOOL_CANCELLED";
        let decoded_payload = args["canonicalResult"]
            .as_str()
            .or_else(|| result["content"].as_str())
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap_or(Value::Null);
        let canonical = if args["canonicalResult"].is_object() {
            &args["canonicalResult"]
        } else if decoded_payload.is_object() {
            &decoded_payload
        } else if result["content"].is_object() {
            &result["content"]
        } else {
            &Value::Null
        };
        let output = canonical
            .get("structuredContent")
            .filter(|value| value.is_object())
            .or_else(|| canonical.get("value").filter(|value| value.is_object()))
            .unwrap_or(canonical);
        let uncertain = uncertain || result["outcomeUnknown"] == true || output["outcomeUnknown"] == true
            || output["cancelled"] == true || output["timedOut"] == true
            || result["code"] == "TOOL_TIMED_OUT";
        if completed && matches!(name, "filesystem.stat" | "artifact.open") {
            self.observe_document_version(output, name == "artifact.open");
            return;
        }
        if matches!(name, "filesystem.read" | "document.read") && completed {
            self.observe_document_version(output, false);
            self.observe_document_read(output, result);
            return;
        }
        if [
            "filesystem.write",
            "filesystem.edit",
            "filesystem.mkdir",
            "filesystem.move",
            "filesystem.delete",
            "document.create",
        ]
        .contains(&name)
        {
            if completed || uncertain {
                self.mutation_revision += 1;
            }
            if completed && matches!(name, "filesystem.write" | "filesystem.edit" | "document.create") {
                self.observe_document_write(output);
            }
            // A successful directory receipt checks the structural effect, not executable code.
            // 成功的目录回执核验结构操作，不代表修改了可执行代码；未知结果仍保留待核验状态。
            if name == "filesystem.mkdir"
                && completed
                && output["path"].is_string()
                && output["created"].is_boolean()
                && self.artifact_revisions.len() < MAX_DOCUMENT_TARGETS
            {
                self.artifact_revisions.insert(self.mutation_revision);
            }
            let document_path = output["path"].as_str().or_else(|| call["arguments"]["path"].as_str())
                .filter(|path| matches!(name, "filesystem.write" | "filesystem.edit" | "document.create") && is_document_target(path));
            if uncertain {
                if let Some(document) = document_path.and_then(|path| self.document_validations.get_mut(path)) {
                    document.version_check_required = true;
                }
            }
            if (completed || uncertain) && !self.artifact_revisions.contains(&self.mutation_revision)
                && !(uncertain && document_path.is_some()) {
                self.code_mutation_revision = self.mutation_revision;
            }
            if uncertain {
                self.uncertain_mutations
                    .insert(call["id"].as_str().unwrap_or("").to_owned());
            }
            return;
        }
        if !host_effect_tool(name) {
            return;
        }
        // Read the actual host receipt without cloning output; structured MCP content retains precedence.
        // 直接借用真实宿主回执，避免复制大输出；结构化 MCP 内容继续保持最高优先级。
        let isolated = output["sandbox"] == "appcontainer"
            && output["tokenVerified"] == true
            && output["workspaceCopy"] == true;
        if !validation_call(call) {
            if !isolated {
                self.mutation_revision += 1;
                self.unclassified_terminal_effects += 1;
                let before = version_identity(&args["beforeVersion"]);
                let after = version_identity(&args["afterVersion"]);
                let targets_known = observed_code_targets(&before) || observed_code_targets(&after)
                    || observed_code_targets(&self.latest_code_version) || !self.required_checks.is_empty();
                let same_observed_version = before["complete"] == true && after["complete"] == true
                    && before["signature"] == after["signature"];
                // Code checks follow observed code targets; unrelated host uncertainty stays in its receipt.
                // 代码检查跟随实际代码目标，无关宿主操作未确认只记在原回执中。
                if targets_known && !same_observed_version {
                    self.code_mutation_revision = self.mutation_revision;
                }
                if observed_code_targets(&after) { self.latest_code_version = after; }
                // Unknown host effects require a fresh version check, not a blind loss of unchanged read ranges.
                // 未分类宿主副作用需要重新核对版本；保留旧范围供同哈希复用，不能继续把旧版本当已验收。
                for document in self.document_validations.values_mut() {
                    document.version_check_required = true;
                }
                if uncertain {
                    self.uncertain_mutations
                        .insert(call["id"].as_str().unwrap_or("").to_owned());
                }
            }
            return;
        }
        let before = version_identity(&args["beforeVersion"]);
        let after = version_identity(&args["afterVersion"]);
        if !before["signature"].is_null()
            && !after["signature"].is_null()
            && before["signature"] != after["signature"]
        {
            self.mutation_revision += 1;
            self.code_mutation_revision = self.mutation_revision;
        }
        let unknown = status == "unknown";
        let cancelled = status == "cancelled"
            || result["code"] == "TOOL_CANCELLED"
            || output["cancelled"] == true;
        let timed_out = output["timedOut"] == true || result["code"] == "TOOL_TIMED_OUT";
        if (unknown || cancelled || timed_out) && !isolated {
            self.mutation_revision += 1;
            self.code_mutation_revision = self.mutation_revision;
            self.uncertain_mutations
                .insert(call["id"].as_str().unwrap_or("").to_owned());
        }
        let exit_code = output["exitCode"].as_i64();
        let passed = completed && exit_code == Some(0) && !cancelled && !timed_out;
        let same_version = before["complete"] == true
            && after["complete"] == true
            && !before["signature"].is_null()
            && before["signature"] == after["signature"];
        // A real passing check covers its observed version, not unrelated effects or a sandbox copy.
        // 真实通过的检查覆盖其观察版本，不认证无关副作用或沙箱副本。
        let covered = passed && same_version && !isolated;
        self.latest_code_version = after.clone();
        let state = if unknown {
            "unknown"
        } else if cancelled {
            "cancelled"
        } else if timed_out {
            "timed-out"
        } else if exit_code.is_none() {
            "outcome-unavailable"
        } else if passed {
            "passed"
        } else {
            "failed"
        };
        let mut receipt = json!({
            "toolCallId": call["id"],
            "mutationRevision": self.mutation_revision,
            "exitCode": exit_code,
            "passed": passed,
            "cancelled": cancelled,
            "timedOut": timed_out,
            "outcomeUnknown": unknown,
            "coversObservedRevision": covered,
            "status": state,
            "codeVersionCoverage": {
                "before": before,
                "after": after,
                "sameObservedVersion": same_version,
                "atomicSnapshot": false,
                "executionTarget": if isolated {"sandbox-workspace-copy"} else {"terminal-working-directory"},
                "sandboxSnapshotVersion": if isolated {"unverified"} else {"not-applicable"},
                "exhaustive": false
            }
        });
        attach_check_identity(&mut receipt, call, output, &args["validationScope"]);
        if let Some(scope) = ValidationScope::read_checkpoint(&args["validationScope"]["acceptanceScope"]) {
            receipt["acceptanceScope"] = scope.checkpoint();
        }
        if truthy(&result["resultRef"]) {
            receipt["resultRef"] = result["resultRef"].clone();
        }
        if !passed {
            let mut summary = json!({
                "stdoutTail": output_tail(&output["stdout"]),
                "stderrTail": output_tail(&output["stderr"]),
                "fullOutput": if truthy(&result["resultRef"]) {"tool-result-archive"} else {"original-tool-receipt"}
            });
            if truthy(&result["code"]) {
                summary["code"] = result["code"].clone();
            }
            receipt["outputSummary"] = summary;
        }
        self.remember_check(receipt.clone());
        self.validation_receipts.push(receipt);
        if self.validation_receipts.len() > 64 {
            self.validation_receipts.remove(0);
        }
        if covered && self.required_check_views().iter().all(|check| check["active"] == false || check["resolution"] == "passed") {
            self.checked_mutation_revision = self.mutation_revision;
            self.validated_code_version = after;
        }
    }

    fn observe_document_write(&mut self, output: &Value) {
        let Some(path) = output["path"].as_str().filter(|path| is_document_target(path)) else {
            return;
        };
        let Some(hash) = content_hash(&output["sha256"]) else {
            return;
        };
        if self.artifact_revisions.len() >= MAX_DOCUMENT_TARGETS
            || self.document_validations.len() >= MAX_DOCUMENT_TARGETS
                && !self.document_validations.contains_key(path)
        {
            return;
        }
        self.artifact_revisions.insert(self.mutation_revision);
        self.document_validations.insert(
            path.to_owned(),
            DocumentValidation {
                sha256: hash.to_owned(),
                ..Default::default()
            },
        );
    }

    fn observe_document_version(&mut self, output: &Value, preview: bool) {
        let Some(path) = output["path"].as_str() else { return; };
        let Some(document) = self.document_validations.get_mut(path) else { return; };
        let Some(hash) = content_hash(&output["sha256"])
            .or_else(|| preview.then(|| content_hash(&output["version"])).flatten()) else {
            // Large-file stat receipts may omit SHA-256; a subsequent original read must discover the version.
            // 大文件属性回执可能不含哈希；明确转为未知版本，下一次原文读取取得真实版本，不能反复空查属性。
            if !preview && output["type"] == "file" && document.version_check_required {
                *document = DocumentValidation::default();
            }
            return;
        };
        if !document.sha256.eq_ignore_ascii_case(hash) {
            // A real read/stat/preview version replaces the tracked output version; old receipts remain archived.
            // 实际读取、属性或预览回执更新已跟踪产物版本；旧回执仍保留归档，只清当前版本的覆盖结论。
            *document = DocumentValidation { sha256: hash.to_owned(), ..Default::default() };
        }
        document.version_check_required = false;
    }

    fn document_readback_targets(&self) -> Vec<Value> {
        self.document_validations.iter().map(|(path, document)| json!({
            "path":path,"sha256":document.sha256,"totalCharacters":document.total_characters,
            "versionKnown":!document.sha256.is_empty(),
            "readRanges":document.ranges,"versionCheckRequired":document.version_check_required,
            "pageCount":document.page_count,"pageWindows":document.page_windows.iter().map(|(&(first,last),window)|
                json!({"startPage":first,"endPage":last,"totalCharacters":window.total_characters,"readRanges":window.ranges})).collect::<Vec<_>>(),
            "readBackComplete":document.read_back && !document.version_check_required,
            "resultRef":document.result_ref
        })).collect()
    }

    fn restore_document_readback_targets(&mut self, targets: &Value, same_task_checkpoint: bool) -> Value {
        let mut restored = 0;
        for target in targets.as_array().into_iter().flatten() {
            let Some(path) = target["path"].as_str().filter(|path| is_document_target(path)) else { continue; };
            let Some(hash) = content_hash(&target["sha256"]) else { continue; };
            if self.document_validations.contains_key(path) || self.document_validations.len() >= MAX_DOCUMENT_TARGETS {
                continue;
            }
            // Only authenticated same-task checkpoints retain coverage; all restores recheck the current file version.
            // 仅已认证的同任务断点保留读取覆盖；跨任务仅恢复目标导航，两者都须重新核对当前文件版本。
            let mut document = DocumentValidation {
                sha256: hash.to_owned(),
                total_characters: target["totalCharacters"].as_u64().filter(|total| *total <= MAX_SAFE_INTEGER),
                version_check_required: true,
                ..Default::default()
            };
            if same_task_checkpoint {
                if let Some(total) = document.total_characters {
                    restore_read_ranges(&mut document.ranges, &target["readRanges"], total);
                }
                if let Some(count) = target["pageCount"].as_u64().filter(|count| *count > 0 && *count <= MAX_SAFE_INTEGER) {
                    document.page_count = Some(count);
                    for window in target["pageWindows"].as_array().into_iter().flatten().take(MAX_DOCUMENT_TARGETS) {
                        let Some(((first,last),total)) = window["startPage"].as_u64().zip(window["endPage"].as_u64())
                            .zip(window["totalCharacters"].as_u64()).filter(|((first,last),total)|
                                *first > 0 && first <= last && *last <= count && *total <= MAX_SAFE_INTEGER) else { continue; };
                        let mut restored_window = DocumentReadWindow { total_characters:Some(total), ..Default::default() };
                        restore_read_ranges(&mut restored_window.ranges, &window["readRanges"], total);
                        document.page_windows.insert((first,last), restored_window);
                    }
                }
                document.read_back = document_coverage_complete(&document);
                if target["resultRef"].is_object() && target["resultRef"]["id"].as_str().is_some_and(|id| !id.is_empty()) {
                    document.result_ref = target["resultRef"].clone();
                }
            }
            self.document_validations.insert(path.to_owned(), document);
            restored += 1;
        }
        json!({"restoredTargets":restored,"requiresCurrentVersionCheck":true})
    }

    fn observe_document_read(&mut self, output: &Value, result: &Value) {
        let Some(path) = output["path"].as_str() else {
            return;
        };
        let Some(document) = self.document_validations.get_mut(path) else {
            return;
        };
        if content_hash(&output["sha256"]) != Some(document.sha256.as_str()) {
            document.version_check_required = true;
            document.read_back = false;
            document.result_ref = Value::Null;
            document.ranges.clear();
            document.total_characters = None;
            document.page_count = None;
            document.page_windows.clear();
            return;
        }
        let Some((start, end, total)) = output["offset"]
            .as_u64()
            .zip(output["nextOffset"].as_u64())
            .zip(output["totalCharacters"].as_u64())
            .map(|((start, end), total)| (start, end, total))
        else {
            return;
        };
        if start > end
            || end > total
            || total > MAX_SAFE_INTEGER
            || output.get("content").filter(|value| value.is_string()).or_else(|| output.get("text"))
                .unwrap_or(&Value::Null)
                .as_str()
                .is_none_or(|content| content.encode_utf16().count() as u64 != end - start)
        {
            return;
        }
        if output["extraction"]["format"] == "pdf" {
            let page_window = &output["extraction"]["pageWindow"];
            let Some(((first, last), count)) = page_window["startPage"].as_u64()
                .zip(page_window["endPage"].as_u64()).zip(output["extraction"]["pageCount"].as_u64())
                .filter(|((first, last), count)| *first > 0 && first <= last && last <= count && *count <= MAX_SAFE_INTEGER)
            else { return; };
            if document.page_count.is_some_and(|previous| previous != count) {
                document.page_windows.clear();
            }
            if document.page_windows.len() >= MAX_DOCUMENT_TARGETS && !document.page_windows.contains_key(&(first, last)) {
                return;
            }
            document.page_count = Some(count);
            // PDF text offsets restart per page window; keep their ranges separate before merging completed pages.
            // PDF 文本偏移在各页窗口内重新计数，先分别保存字符范围，再合并完整窗口的页覆盖。
            let window = document.page_windows.entry((first, last)).or_default();
            record_read_range(&mut window.total_characters, &mut window.ranges, start, end, total);
            let mut pages = Vec::new();
            for (&(first, last), window) in &document.page_windows {
                if window.total_characters.is_some_and(|total| window.ranges.as_slice() == [(0, total)]) {
                    merge_read_range(&mut pages, first, last + 1);
                }
            }
            document.read_back = pages.as_slice() == [(1, count + 1)];
        } else {
            if output["extractionComplete"] == false || output["extraction"]["complete"] == false {
                return;
            }
            record_read_range(&mut document.total_characters, &mut document.ranges, start, end, total);
            document.read_back = document.ranges.as_slice() == [(0, total)];
        }
        if document.read_back {
            document.result_ref = result["resultRef"].clone();
        }
        if !self.pending_code_mutation()
            && self.required_check_views().iter().all(|check| check["active"] == false || check["resolution"] == "passed")
        {
            self.checked_mutation_revision = self.mutation_revision;
        }
    }

    fn pending_code_mutation(&self) -> bool {
        self.code_mutation_since(self.checked_mutation_revision)
    }

    fn needs_validation(&self, task: &str) -> bool {
        self.needs_validation_from(task, &self.verification())
    }

    pub(super) fn needs_validation_for_scope(&self, task: &str, scope: &ValidationScope) -> bool {
        self.needs_validation_from(task, &self.verification_for_scope(scope))
    }

    fn needs_validation_from(&self, task: &str, verification: &Value) -> bool {
        let unresolved = verification["unresolvedChecks"].as_array().into_iter().flatten().collect::<Vec<_>>();
        let retryable_check = unresolved.iter().any(|check| matches!(check["resolution"].as_str(), Some("failed" | "needs-revalidation")));
        if unresolved.iter().any(|check| check["resolution"] == "execution-unconfirmed")
            || !retryable_check && unresolved.iter().any(|check| check["resolution"] == "version-unverified")
        {
            return false;
        }
        static TASK_PATTERN: OnceLock<Regex> = OnceLock::new();
        (verification["pendingValidation"] == true || retryable_check)
            && TASK_PATTERN
                .get_or_init(|| {
                    Regex::new(r"(?i)代码|修复|实现|重构|\b(?:code|fix|implement|refactor)\b|\.(?:rs|py|js|jsx|ts|tsx|cs|java|go|c|cpp|h|hpp|sql|html|css)\b")
                        .unwrap()
                })
                .is_match(task)
    }

    /// Transport completion and task outcome are separate; failed checks cannot become successful facts.
    /// 传输完成与任务结果分别记录；核验失败不能被提升为已完成事实。
    fn complete(&mut self, arguments: &Value) -> Value {
        self.complete_with_verification(arguments, self.verification())
    }

    pub(super) fn complete_for_scope(&mut self, arguments: &Value, scope: &ValidationScope) -> Value {
        self.complete_with_verification(arguments, self.verification_for_scope(scope))
    }

    fn complete_with_verification(&mut self, arguments: &Value, mut result: Value) -> Value {
        let evidence_invalid = arguments["evidenceValidation"]["current"] == false;
        let checks_failed = result["state"] == "checks-failed";
        let model_state = arguments["modelTaskState"]["state"].as_str();
        let unmet = arguments["modelTaskState"]["unmet"]
            .as_array()
            .is_some_and(|items| !items.is_empty());
        // Without a reviewed state, real external effects leave the task unassessed rather than complete.
        // 有真实外部操作却没有核对状态时，任务属于尚未评估，不能记为完成。
        let acceptance_unreviewed = arguments["requiresVerification"] == true && arguments["acceptanceReviewed"] != true;
        // A settled broker decision is distinct from the model's preserved next-step proposal.
        // 程序已核验的完成结论，与保留的模型下一步提议分别记录。
        let broker_final = arguments["brokerDecision"] == "final" && arguments["acceptanceReviewed"] == true;
        let readback_missing = arguments["requiresReadback"] == true && arguments["hasReadback"] != true
            || arguments["requiresVisualReadback"] == true && arguments["hasVisualReadback"] != true;
        let unassessed = arguments["effectfulWork"] == true && model_state == Some("unknown")
            || acceptance_unreviewed || readback_missing;
        let incomplete = arguments["interrupted"] == true
            || arguments["code"].as_str().is_some()
            || result["pendingValidation"] == true
            || matches!(model_state, Some("blocked" | "review"))
            || model_state == Some("continue") && !broker_final
            || unmet
            || unassessed;
        result["completionState"] = json!(if evidence_invalid || checks_failed {
            "verification-failed"
        } else if incomplete {
            "partial"
        } else {
            "completed"
        });
        result["correctnessCertified"] = json!(false);
        result["acceptanceReviewRequired"] = arguments["requiresVerification"].clone();
        result["acceptanceReviewed"] = json!(arguments["acceptanceReviewed"] == true);
        if broker_final { result["brokerDecision"] = json!("final"); }
        if arguments["interrupted"] == true {
            result["state"] = json!("incomplete");
        }
        if let Some(validation) = arguments["evidenceValidation"].as_object() {
            let mut source_validation =
                json!({"freshnessOnly": true, "correctnessCertified": false});
            for field in [
                "current",
                "checked",
                "memoryChecked",
                "invalidSources",
                "code",
            ] {
                if let Some(value) = validation.get(field) {
                    source_validation[field] = value.clone();
                }
            }
            result["sourceValidation"] = source_validation;
        }
        if arguments["modelTaskState"].is_object() {
            result["modelTaskState"] = arguments["modelTaskState"].clone();
        }
        if let Some(code) = arguments["code"].as_str() {
            result["code"] = json!(code);
        }
        self.task_completion = result.clone();
        result
    }

    pub(super) fn verification(&self) -> Value {
        self.verification_with_scope(None)
    }

    pub(super) fn verification_for_scope(&self, scope: &ValidationScope) -> Value {
        self.verification_with_scope(Some(scope))
    }

    fn verification_with_scope(&self, scope: Option<&ValidationScope>) -> Value {
        let checks = self.required_check_views_for_scope(scope);
        let unresolved: Vec<&Value> = checks.iter().filter(|check| check["active"] == true && check["resolution"] != "passed").collect();
        let failed = unresolved.iter().any(|check| check["resolution"] == "failed");
        let version_unverified = unresolved.iter().any(|check| check["resolution"] == "version-unverified");
        let pending_document = self
            .document_validations
            .values()
            .any(|document| !document.read_back || document.version_check_required);
        // A current passing check or an entirely withdrawn target closes only the scoped debt.
        // 当前通过的检查或已完全撤销的目标只消除本次范围内的检查欠账，不改原计数。
        let scope_covered = scope.is_some() && checks.iter().any(|check| check["active"] == true && check["resolution"] == "passed");
        let withdrawn_latest = scope.is_some_and(|scope| self.validation_receipts.last().is_some_and(|receipt|
            scope.excludes(receipt) && receipt["codeVersionCoverage"]["after"]["signature"] == self.latest_code_version["signature"]));
        let observed_version_changed = scope.is_some_and(|scope| self.latest_code_version["files"].as_array()
            .into_iter().flatten().any(|file| scope.known_targets.iter().any(|target|
                file["pathId"] == target.path_id && file["sha256"].as_str().is_some_and(|hash|
                    !hash.eq_ignore_ascii_case(&target.sha256)))));
        // Mutation bookkeeping does not add a test requirement to an ordinary deliverable.
        // 修改记录不能替普通制作任务增加测试要求；真实失败、未知结果及版本冲突仍须处理。
        let code_check_required = scope.is_none_or(|scope| scope.requires_code_validation)
            || !self.uncertain_mutations.is_empty() || observed_version_changed;
        let needs_check = self.pending_code_mutation() && code_check_required && !scope_covered && !withdrawn_latest;
        let pending = needs_check || !unresolved.is_empty();
        let state = if failed {
            "checks-failed"
        } else if unresolved.iter().any(|check| check["resolution"] == "execution-unconfirmed") {
            "execution-unconfirmed"
        } else if version_unverified {
            "checks-version-unverified"
        } else if needs_check || unresolved.iter().any(|check| check["resolution"] == "needs-revalidation") {
            "needs-validation"
        } else if checks.iter().any(|check| check["active"] == true && check["resolution"] == "passed")
        {
            "checks-passed"
        } else {
            "no-execution-checks"
        };
        json!({
            "version": 1,
            "mutationRevision": self.mutation_revision,
            "checkedMutationRevision": self.checked_mutation_revision,
            "pendingValidation": pending,
            "receipts": self.validation_receipts.iter().map(|receipt| {
                let identity = receipt["checkId"].as_str().map(str::to_owned).or_else(||
                    receipt["toolCallId"].as_str().map(|id| stable_hash(&json!(["legacy-validation",id]))));
                let latest = checks.iter().find(|check| identity.as_deref() == check["checkId"].as_str());
                let mut view = receipt.clone();
                view["active"] = json!(latest.is_none_or(|check| check["active"] == true));
                view["current"] = json!(latest.is_some_and(|check| check["toolCallId"] == receipt["toolCallId"] && check["current"] == true));
                view.as_object_mut().unwrap().remove("acceptanceScope");
                view
            }).collect::<Vec<_>>(),
            "requiredChecks": checks,
            "unresolvedChecks": unresolved,
            "documentValidation": {
                "coverage": "complete-hash-matched-readback",
                "semanticCorrectnessCertified": false,
                "pending": pending_document,
                "targetCount": self.document_validations.len(),
                "readBackCount": self.document_validations.values().filter(|document| document.read_back && !document.version_check_required).count(),
                "omittedTargets": self.document_validations.len().saturating_sub(64),
                "targets": self.document_readback_targets().into_iter().take(64).collect::<Vec<_>>()
            },
            "codeVersionCoverage": {
                "coverage": "observed-filesystem-targets",
                "exhaustive": false,
                "actualFileHashesVerified": !self.validated_code_version.is_null() && !pending,
                "observedMutationRevision": self.mutation_revision,
                "codeMutationRevision": self.code_mutation_revision,
                "hostEffectClassificationVersion": self.host_effect_classification_version,
                "checkedMutationRevision": self.checked_mutation_revision,
                "uncertainMutations": self.uncertain_mutations.len(),
                "unclassifiedTerminalEffects": self.unclassified_terminal_effects,
                "latestSnapshot": self.latest_code_version,
                "validatedSnapshot": self.validated_code_version,
                "atomicSnapshot": false,
                "wholeRepositoryCertified": false
            },
            "state": state,
            "conclusion": "task-correctness-not-certified"
        })
    }
}

fn record_read_range(total_characters: &mut Option<u64>, ranges: &mut Vec<(u64, u64)>, start: u64, end: u64, total: u64) {
    if total_characters.is_some_and(|previous| previous != total) { ranges.clear(); }
    *total_characters = Some(total);
    merge_read_range(ranges, start, end);
}

fn merge_read_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    if ranges.len() >= MAX_DOCUMENT_TARGETS { return; }
    ranges.push((start, end));
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for &(start, end) in ranges.iter() {
        if let Some(previous) = merged.last_mut().filter(|previous| start <= previous.1) {
            previous.1 = previous.1.max(end);
        } else { merged.push((start, end)); }
    }
    *ranges = merged;
}

fn restore_read_ranges(ranges: &mut Vec<(u64,u64)>, saved: &Value, total: u64) {
    for range in saved.as_array().into_iter().flatten().take(MAX_DOCUMENT_TARGETS) {
        if let Some((start,end)) = range.as_array().and_then(|range|
            range.first()?.as_u64().zip(range.get(1)?.as_u64()))
            .filter(|(start,end)| start <= end && *end <= total) {
            merge_read_range(ranges, start, end);
        }
    }
}

fn document_coverage_complete(document: &DocumentValidation) -> bool {
    if let Some(count) = document.page_count {
        let mut pages = Vec::new();
        for (&(first,last),window) in &document.page_windows {
            if window.total_characters.is_some_and(|total| window.ranges.as_slice() == [(0,total)]) {
                merge_read_range(&mut pages, first, last + 1);
            }
        }
        pages.as_slice() == [(1,count + 1)]
    } else {
        document.total_characters.is_some_and(|total| document.ranges.as_slice() == [(0,total)])
    }
}

fn content_hash(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn is_document_target(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if ["AGENTS.md", "SKILL.md"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return false;
    }
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        ["md", "markdown", "txt", "rst", "adoc", "docx", "pptx", "xlsx", "pdf"]
            .iter()
            .any(|prose| extension.eq_ignore_ascii_case(prose))
    })
}

pub fn validation_call(call: &Value) -> bool {
    validation_command(call).is_some()
}

fn validation_command(call: &Value) -> Option<String> {
    let name = call["name"].as_str().unwrap_or("");
    if !["terminal.run", "terminal.host.run"].contains(&name) {
        return None;
    }
    let args = &call["arguments"];
    let arguments = args["args"].as_array();
    let command = if name == "terminal.host.run" {
        args.get("script")
            .filter(|value| !value.is_null())
            .unwrap_or(&args["command"])
            .as_str()
            .unwrap_or("")
            .to_owned()
    } else if args["command"].as_str().is_some_and(|value| {
        ["cmd", "cmd.exe"]
            .iter()
            .any(|name| value.eq_ignore_ascii_case(name))
    }) && arguments.is_some_and(|values| {
        values.len() == 3
            && values[0]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case("/d"))
            && values[1]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case("/c"))
    }) {
        arguments.unwrap()[2].as_str().unwrap_or("").to_owned()
    } else {
        std::iter::once(&args["command"])
            .chain(arguments.into_iter().flatten())
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    };
    if command
        .chars()
        .any(|value| matches!(value, ';' | '&' | '|' | '`' | '\r' | '\n'))
    {
        return None;
    }
    static COMMAND_PATTERN: OnceLock<Regex> = OnceLock::new();
    COMMAND_PATTERN.get_or_init(|| Regex::new(r"(?i)^\s*(?:dotnet(?:\.exe)?\s+(?:test|build)|cargo(?:\.exe)?\s+(?:test|check|build)|npm(?:\.cmd)?\s+(?:test|run\s+(?:test|build))|node(?:\.exe)?\s+--test|pytest(?:\.exe)?|(?:py(?:\.exe)?(?:\s+-[23](?:\.\d+)?)?|python(?:[23](?:\.\d+)?)?(?:\.exe)?)\s+-m\s+(?:pytest|unittest))(?:\s|$)").unwrap()).is_match(&command).then_some(command)
}

fn stable_hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

fn attach_check_identity(receipt: &mut Value, call: &Value, output: &Value, scope: &Value) {
    let Some(command) = validation_command(call) else { return; };
    let cwd = output["cwd"].as_str().filter(|cwd| !cwd.trim().is_empty())
        .or_else(|| scope["cwd"].as_str().filter(|cwd| !cwd.trim().is_empty()))
        .or_else(|| scope.as_str()).unwrap_or("");
    let environment = if output["sandbox"].as_str().is_some_and(|value| !value.is_empty()) || call["name"] == "terminal.run" {
        "sandbox"
    } else { "host" };
    let command = normalize_command(&command);
    let cwd = normalize_workdir(cwd);
    // Identity excludes the code version and keeps command arguments and paths out of the public view.
    // 身份不含代码版本，公开视图只保存指纹，不新增命令参数和路径明文。
    receipt["commandId"] = json!(stable_hash(&json!(command)));
    receipt["scopeId"] = json!(stable_hash(&json!([cwd, environment])));
    let workspace = scope["cwd"].as_str().or_else(|| scope.as_str()).unwrap_or("");
    receipt["workspaceKnown"] = json!(!workspace.trim().is_empty());
    receipt["workspaceId"] = json!(stable_hash(&json!(normalize_workdir(workspace))));
    receipt["checkId"] = json!(stable_hash(&json!([command, cwd, environment])));
    receipt["category"] = json!(validation_category(&command));
    receipt["executionEnvironment"] = json!(environment);
}

fn normalize_workdir(value: &str) -> String {
    let path = value.trim().replace('\\', "/");
    let root = path == "/" || path.len() == 3 && path.as_bytes().get(1) == Some(&b':') && path.ends_with('/');
    let path = if root { path.as_str() } else { path.trim_end_matches('/') };
    if path.as_bytes().get(1) == Some(&b':') || path.starts_with("//") { path.to_lowercase() } else { path.to_owned() }
}

fn normalize_command(value: &str) -> String {
    let mut result = String::new();
    let mut quote = None;
    let mut space = false;
    for character in value.trim().chars() {
        if quote.is_none() && character.is_whitespace() { space = true; continue; }
        if space && !result.is_empty() { result.push(' '); }
        space = false;
        if matches!(character, '\'' | '"') {
            if quote == Some(character) { quote = None; }
            else if quote.is_none() { quote = Some(character); }
        }
        result.push(character);
    }
    result
}

fn validation_category(command: &str) -> &'static str {
    static BUILD: OnceLock<Regex> = OnceLock::new();
    static CHECK: OnceLock<Regex> = OnceLock::new();
    if BUILD.get_or_init(|| Regex::new(r"(?i)^\s*(?:(?:dotnet|cargo)(?:\.exe)?\s+build|npm(?:\.cmd)?\s+run\s+build)(?:\s|$)").unwrap()).is_match(command) { "build" }
    else if CHECK.get_or_init(|| Regex::new(r"(?i)^\s*cargo(?:\.exe)?\s+check(?:\s|$)").unwrap()).is_match(command) { "check" }
    else { "test" }
}

fn disjoint_targets(left: &Value, right: &Value) -> bool {
    fn targets(snapshot: &Value) -> Option<(String, BTreeSet<String>)> {
        let files = snapshot["files"].as_array()?;
        if snapshot["complete"] != true || files.is_empty() || snapshot["fileCount"].as_u64() != Some(files.len() as u64) { return None; }
        let source = files.first()?["identitySource"].as_str().unwrap_or("pathId");
        let ids = files.iter().map(|file| {
            (file["identitySource"].as_str().unwrap_or("pathId") == source)
                .then(|| file["pathId"].as_str().map(str::to_owned)).flatten()
        }).collect::<Option<BTreeSet<_>>>()?;
        Some((source.to_owned(), ids))
    }
    match (targets(left), targets(right)) {
        (Some((left_source, left)), Some((right_source, right))) => left_source == right_source && left.is_disjoint(&right),
        _ => false,
    }
}

fn safe_integer(value: &Value) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or(DIAGNOSTIC_ERROR.to_owned())
}

fn metric_round(value: &Value) -> Result<u64, String> {
    safe_integer(value).and_then(|value| {
        if (1..=128).contains(&value) {
            Ok(value)
        } else {
            Err(DIAGNOSTIC_ERROR.to_owned())
        }
    })
}

fn checked_sum(previous: &Value, added: u64) -> Result<u64, String> {
    safe_integer(previous)?
        .checked_add(added)
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or(DIAGNOSTIC_ERROR.to_owned())
}

fn append_bounded(value: &mut Value, entry: Value, maximum: usize) {
    let entries = value.as_array_mut().expect("metrics lists are initialized");
    entries.push(entry);
    if entries.len() > maximum {
        entries.remove(0);
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}

fn host_effect_tool(name: &str) -> bool {
    matches!(name, "terminal.run" | "terminal.host.run" | "terminal.host.start" | "skill.run")
}

fn execution_output(canonical: &Value) -> &Value {
    canonical.get("structuredContent").filter(|value| value.is_object())
        .or_else(|| canonical.get("value").filter(|value| value.is_object())).unwrap_or(canonical)
}

/// Old, targetless host debt needs owned receipts once; current classifications require no archive scan.
/// 旧的无目标宿主债务只需凭自有回执迁移一次；当前分类不再扫描归档。
pub(super) fn needs_host_effect_reclassification(state: &Value) -> bool {
    let mutation = state["mutationRevision"].as_u64().unwrap_or(0);
    let host_effects = state["unclassifiedTerminalEffects"].as_u64().unwrap_or(0);
    let artifact_count = state["artifactRevisions"].as_array().into_iter().flatten()
        .filter_map(Value::as_u64).collect::<BTreeSet<_>>().len() as u64;
    state["hostEffectClassificationVersion"].as_u64().unwrap_or(0) < HOST_EFFECT_CLASSIFICATION_VERSION
        && state["codeMutationRevision"].as_u64().unwrap_or(mutation)
            > state["checkedMutationRevision"].as_u64().unwrap_or(0)
        && host_effects > 0 && mutation.saturating_sub(artifact_count) == host_effects
        && !observed_code_targets(&state["latestCodeVersion"]) && !observed_code_targets(&state["validatedCodeVersion"])
        && ["requiredChecks", "validationReceipts"].iter()
            .all(|field| state[*field].is_null() || state[*field].as_array().is_some_and(Vec::is_empty))
}

fn observed_code_targets(snapshot: &Value) -> bool {
    snapshot["files"].as_array().is_some_and(|files| !files.is_empty())
        || snapshot["fileCount"].as_u64().is_some_and(|count| count > 0)
}

fn version_identity(snapshot: &Value) -> Value {
    let signature = snapshot["signature"]
        .as_str()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let file_count = snapshot["files"].as_array().map_or(0, Vec::len);
    let files: Vec<Value> = snapshot["files"].as_array().into_iter().flatten().take(64).map(|file| {
        let mut identity = if let Some(id) = file["pathId"].as_str().filter(|value| !value.is_empty()) {
            json!({"pathId":id,"identitySource":file["identitySource"].as_str().unwrap_or("pathId")})
        } else if let Some(path) = file["path"].as_str().filter(|value| !value.is_empty()) {
            json!({"pathId":stable_hash(&json!(normalize_workdir(path))),"identitySource":"legacy-path"})
        } else { Value::Null };
        if identity.is_object() && let Some(hash) = content_hash(&file["sha256"]) {
            identity["sha256"] = json!(hash.to_ascii_lowercase());
        }
        identity
    }).collect();
    let mut result = json!({
        "signature": signature,
        "complete": snapshot["complete"] == true && signature.is_some() && (1..=64).contains(&file_count),
        "fileCount": file_count.min(64),
        "files": files,
        "coverage": "observed-filesystem-targets",
        "exhaustive": false
    });
    if truthy(&snapshot["code"]) {
        result["code"] = snapshot["code"].clone();
    }
    result
}

fn output_tail(value: &Value) -> String {
    let text = match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    };
    let units = text.encode_utf16().collect::<Vec<_>>();
    String::from_utf16_lossy(&units[units.len().saturating_sub(768)..])
}

#[cfg(test)]
mod document_validation_tests {
    use super::*;

    fn write(progress: &mut RunProgress, path: &str, hash: char) {
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"document-write","name":"filesystem.write"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"sha256":hash.to_string().repeat(64)}})).unwrap();
    }

    fn read(
        progress: &mut RunProgress,
        path: &str,
        hash: char,
        start: u64,
        total: u64,
        text: &str,
    ) {
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"document-read","name":"filesystem.read"},
            "result":{"status":"completed","isError":false,"resultRef":{"id":"read-reference"}},
            "canonicalResult":{"path":path,"sha256":hash.to_string().repeat(64),"content":text,"offset":start,"nextOffset":start+text.encode_utf16().count() as u64,"totalCharacters":total}})).unwrap();
    }

    #[test]
    fn prose_delivery_only_requires_readback_when_requested() {
        let mut progress = RunProgress::default();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"book-directory","name":"filesystem.mkdir"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":"/work/book","created":true}})).unwrap();
        write(&mut progress, "/work/book.md", 'a');
        write(&mut progress, "/work/notes.md", 'b');
        assert_eq!(
            progress.complete(&json!({"modelTaskState":{"state":"complete"}}))["completionState"],
            "completed"
        );
        assert_eq!(
            progress.complete(&json!({"modelTaskState":{"state":"complete"},"requiresReadback":true}))["completionState"],
            "partial"
        );
        read(&mut progress, "/work/book.md", 'a', 0, 6, "正文完成😀");
        let complete = progress.complete(&json!({"modelTaskState":{"state":"complete"},"requiresReadback":true,"hasReadback":true}));
        assert_eq!(complete["completionState"], "completed");
        assert_eq!(complete["documentValidation"]["pending"], true);
        assert_eq!(complete["checkedMutationRevision"], complete["mutationRevision"]);
        assert_eq!(complete["documentValidation"]["readBackCount"], 1);
        assert_eq!(
            complete["documentValidation"]["semanticCorrectnessCertified"],
            false
        );
        assert_eq!(complete["correctnessCertified"], false);
        assert_eq!(
            complete["codeVersionCoverage"]["actualFileHashesVerified"],
            false
        );
        assert_eq!(complete["receipts"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn restored_document_progress_keeps_readback_separate_from_code_checks() {
        let mut original = RunProgress::default();
        write(&mut original, "/work/report.md", 'a');
        let state = original.apply("progress.state", &json!({})).unwrap();
        let targets = original.apply("progress.documentReadbackTargets", &json!({})).unwrap();
        let mut restored = RunProgress::default();
        restored.apply("progress.restoreValidation", &json!({"state":state})).unwrap();
        restored.apply("progress.restoreDocumentReadbackTargets", &json!({"targets":targets,"sameTaskCheckpoint":true})).unwrap();
        assert!(!restored.needs_validation("修复说明文档"));
        assert_eq!(restored.verification()["pendingValidation"], false);
        assert_eq!(restored.verification()["documentValidation"]["pending"], true);
        read(&mut restored, "/work/report.md", 'a', 0, 6, "abcdef");
        assert_eq!(restored.verification()["documentValidation"]["pending"], false);
        assert!(restored.verification()["receipts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn document_readback_current_hash_stat_preserves_ranges_and_preview_invalidates_old_version() {
        let mut progress = RunProgress::default();
        let path = "/work/deck.pptx";
        write(&mut progress, path, 'a');
        read(&mut progress, path, 'a', 0, 8, "abcdefgh");
        assert_eq!(progress.verification()["documentValidation"]["readBackCount"], 1);
        let terminal = json!({"call":{"id":"host","name":"terminal.host.run","arguments":{"script":"inspect-helper"}},
            "result":{"status":"completed","isError":false},"canonicalResult":{"exitCode":0}});
        progress.apply("progress.observeOutcome", &terminal).unwrap();
        assert_eq!(progress.verification()["documentValidation"]["readBackCount"], 0);
        assert_eq!(progress.document_readback_targets()[0]["versionCheckRequired"], true);
        progress.apply("progress.observeOutcome", &json!({"call":{"name":"filesystem.stat"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"sha256":"a".repeat(64)}})).unwrap();
        assert_eq!(progress.verification()["documentValidation"]["readBackCount"], 1);
        progress.apply("progress.observeOutcome", &terminal).unwrap();
        progress.apply("progress.observeOutcome", &json!({"call":{"name":"artifact.open"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"version":"b".repeat(64)}})).unwrap();
        let target = progress.document_readback_targets().remove(0);
        assert_eq!(target["sha256"], "b".repeat(64));
        assert_eq!(target["readBackComplete"], false);
        assert_eq!(target["readRanges"], json!([]));
        assert_eq!(target["resultRef"], Value::Null);
        read(&mut progress, path, 'b', 0, 8, "new body");
        assert_eq!(progress.verification()["documentValidation"]["readBackCount"], 1);
        assert_eq!(progress.document_readback_targets()[0]["sha256"], "b".repeat(64));
        progress.apply("progress.observeOutcome", &terminal).unwrap();
        progress.apply("progress.observeOutcome", &json!({"call":{"name":"filesystem.stat"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"type":"file","bytes":20_000_000}})).unwrap();
        assert_eq!(progress.document_readback_targets()[0]["versionKnown"], false);
        assert_eq!(progress.document_readback_targets()[0]["versionCheckRequired"], false);
        read(&mut progress, path, 'c', 0, 8, "new body");
        assert_eq!(progress.verification()["documentValidation"]["readBackCount"], 1);
        assert_eq!(progress.document_readback_targets()[0]["sha256"], "c".repeat(64));
        let mut continued = RunProgress::default();
        continued.apply("progress.restoreDocumentReadbackTargets", &json!({"targets":progress.document_readback_targets()})).unwrap();
        assert_eq!(continued.document_readback_targets()[0]["versionCheckRequired"], true);
        assert_eq!(continued.document_readback_targets()[0]["readBackComplete"], false);
        assert_eq!(continued.document_readback_targets()[0]["readRanges"], json!([]));
        assert_eq!(continued.summary()["mutationRevision"], 0);
    }

    #[test]
    fn document_readback_same_task_restore_checks_hash_then_reads_only_the_gap() {
        let path = "/work/restored.docx";
        let mut before = RunProgress::default();
        write(&mut before, path, 'a');
        read(&mut before, path, 'a', 4, 8, "tail");
        let saved = before.document_readback_targets();
        let mut restored = RunProgress::default();
        restored.apply("progress.restoreDocumentReadbackTargets", &json!({
            "targets":saved,"sameTaskCheckpoint":true})).unwrap();
        assert_eq!(restored.document_readback_targets()[0]["readRanges"], json!([[4,8]]));
        assert_eq!(restored.document_readback_targets()[0]["versionCheckRequired"], true);
        restored.apply("progress.observeOutcome", &json!({"call":{"name":"filesystem.stat"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"sha256":"a".repeat(64)}})).unwrap();
        assert_eq!(restored.document_readback_targets()[0]["readRanges"], json!([[4,8]]));
        read(&mut restored, path, 'a', 0, 8, "head");
        assert_eq!(restored.complete(&json!({"modelTaskState":{"state":"complete"}}))["completionState"], "completed");
        let mut changed = RunProgress::default();
        changed.apply("progress.restoreDocumentReadbackTargets", &json!({
            "targets":saved,"sameTaskCheckpoint":true})).unwrap();
        changed.apply("progress.observeOutcome", &json!({"call":{"name":"filesystem.stat"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"sha256":"b".repeat(64)}})).unwrap();
        assert_eq!(changed.document_readback_targets()[0]["readRanges"], json!([]));
        assert_eq!(changed.document_readback_targets()[0]["sha256"], "b".repeat(64));
    }

    #[test]
    fn document_pages_must_cover_the_current_observed_hash_and_complete_range() {
        let mut progress = RunProgress::default();
        write(&mut progress, "/work/report.txt", 'a');
        read(&mut progress, "/work/report.txt", 'a', 4, 8, "efgh");
        read(&mut progress, "/work/report.txt", 'b', 0, 8, "abcdefgh");
        assert_eq!(progress.verification()["documentValidation"]["pending"], false);
        read(&mut progress, "/work/report.txt", 'a', 0, 8, "abcd");
        assert_eq!(progress.verification()["documentValidation"]["pending"], true);
        read(&mut progress, "/work/report.txt", 'a', 4, 8, "efgh");
        assert_eq!(progress.verification()["documentValidation"]["pending"], false);
        write(&mut progress, "/work/report.txt", 'b');
        assert_eq!(progress.verification()["documentValidation"]["pending"], true);
        read(&mut progress, "/work/report.txt", 'a', 0, 8, "abcdefgh");
        assert_eq!(progress.verification()["documentValidation"]["pending"], false);
        read(&mut progress, "/work/report.txt", 'b', 0, 8, "new body");
        assert_eq!(progress.verification()["documentValidation"]["pending"], false);
    }

    #[test]
    fn generated_office_documents_accept_document_read_text_with_the_written_hash() {
        for format in ["docx", "pptx", "xlsx"] {
            let mut progress = RunProgress::default();
            let path = format!("/work/report.{format}");
            progress.apply("progress.observeOutcome", &json!({"call":{"id":"create","name":"document.create"},
                "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"sha256":"a".repeat(64),
                    "bytes":1024,"format":format,"actualFormat":format,"validated":true}})).unwrap();
            assert_eq!(progress.verification()["documentValidation"]["targetCount"], 1);
            assert_eq!(progress.verification()["pendingValidation"], false);
            assert_eq!(progress.verification()["documentValidation"]["pending"], true);
            for (hash, complete) in [('b', true), ('a', false), ('a', true)] {
                progress.apply("progress.observeOutcome", &json!({"call":{"id":"read","name":"document.read"},
                    "result":{"status":"completed","isError":false},"canonicalResult":{"path":path,"sha256":hash.to_string().repeat(64),
                        "text":"正文42","offset":0,"nextOffset":4,"totalCharacters":4,"extractionComplete":complete,
                        "extraction":{"format":format,"complete":complete}}})).unwrap();
                assert_eq!(progress.verification()["documentValidation"]["pending"], !complete);
            }
            assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete"}}))["completionState"], "completed");
        }
    }

    #[test]
    fn unassessed_effectful_work_and_listed_unmet_requirements_are_partial() {
        let mut progress = RunProgress::default();
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"unknown"},"effectfulWork":true}))["completionState"], "partial");
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete","unmet":["彩窗"]}}))["completionState"], "partial");
        // A plain answer without external effects keeps the ordinary completed outcome.
        // 无外部操作的纯文字答复沿用普通完成状态。
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"unknown"}}))["completionState"], "completed");
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete","unmet":[]},"effectfulWork":true}))["completionState"], "completed");
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete"},"requiresVerification":true}))["completionState"],"partial");
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete"},"requiresVerification":true,
            "acceptanceReviewed":true,"requiresVisualReadback":true,"hasVisualReadback":false}))["completionState"],"partial");
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete"},"requiresVerification":true,
            "acceptanceReviewed":true,"requiresVisualReadback":true,"hasVisualReadback":true}))["completionState"],"completed");
    }

    #[test]
    fn generated_pdf_readback_merges_complete_page_windows_without_offset_collisions() {
        let mut progress = RunProgress::default();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"pdf-create","name":"document.create"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":"/work/book.pdf","sha256":"a".repeat(64),
                "format":"pdf","validated":true,"bytes":2048}})).unwrap();
        for (first, last, offset, text, total) in [(1, 2, 0, "Page12", 6), (3, 3, 0, "A", 2), (3, 3, 1, "B", 2)] {
            progress.apply("progress.observeOutcome", &json!({"call":{"id":"pdf-read","name":"document.read"},
                "result":{"status":"completed","isError":false},"canonicalResult":{"path":"/work/book.pdf","sha256":"a".repeat(64),
                    "text":text,"offset":offset,"nextOffset":offset+text.len(),"totalCharacters":total,"extractionComplete":false,
                    "extraction":{"format":"pdf","complete":false,"pageCount":3,
                        "pageWindow":{"version":"pdf-page-window-v1","startPage":first,"endPage":last}}}})).unwrap();
            assert_eq!(progress.verification()["documentValidation"]["pending"], !(first == 3 && offset == 1));
        }
        assert_eq!(progress.verification()["documentValidation"]["readBackCount"], 1);
        assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete"}}))["completionState"], "completed");
    }

    #[test]
    fn prose_readback_cannot_clear_code_changes_or_failed_checks() {
        let mut progress = RunProgress::default();
        write(&mut progress, "/work/source.rs", 'a');
        read(&mut progress, "/work/source.rs", 'a', 0, 4, "code");
        write(&mut progress, "/work/notes.md", 'b');
        read(&mut progress, "/work/notes.md", 'b', 0, 4, "note");
        assert_eq!(progress.complete(&json!({}))["completionState"], "partial");
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"real-test","name":"terminal.host.run","arguments":{"script":"cargo test"}},
            "result":{"status":"completed","isError":true},"canonicalResult":{"exitCode":1}})).unwrap();
        assert_eq!(
            progress.complete(&json!({}))["completionState"],
            "verification-failed"
        );
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"uncertain-write","name":"filesystem.write"},
            "result":{"status":"unknown","isError":true},"canonicalResult":{"path":"/work/notes.md","sha256":"b".repeat(64)}})).unwrap();
        read(&mut progress, "/work/notes.md", 'b', 0, 4, "note");
        assert_eq!(
            progress.complete(&json!({}))["completionState"],
            "verification-failed"
        );
    }

    #[test]
    fn quoted_prose_and_instruction_files_do_not_bypass_execution_validation() {
        for path in [
            "/work/AGENTS.md",
            "/work/SKILL.md",
            "/work/config.json",
            "/work/build.ps1",
        ] {
            let mut progress = RunProgress::default();
            write(&mut progress, path, 'a');
            read(&mut progress, path, 'a', 0, 4, "text");
            assert_eq!(progress.verification()["pendingValidation"], true, "{path}");
        }
        let mut progress = RunProgress::default();
        write(&mut progress, "/work/book.md", 'a');
        // A schema or stat response has a hash but is not the requested original body.
        // 定义或属性响应即使带哈希，也不是用户要求核对的原文。
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"stat","name":"filesystem.stat"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":"/work/book.md","sha256":"a".repeat(64)}})).unwrap();
        assert_eq!(progress.verification()["pendingValidation"], false);
        assert_eq!(progress.verification()["documentValidation"]["pending"], true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(progress: &mut RunProgress, id: &str, command: &str, exit: i64, snapshot: &Value) {
        progress.apply("progress.observeOutcome", &json!({
            "call":{"id":id,"name":"terminal.host.run","arguments":{"script":command}},
            "result":{"status":"completed","isError":exit != 0},"canonicalResult":{"exitCode":exit,"cwd":"D:/work"},
            "validationScope":{"cwd":"D:/fallback"},"beforeVersion":snapshot,"afterVersion":snapshot
        })).unwrap();
    }

    #[test]
    fn settled_host_queries_and_application_actions_do_not_create_targetless_code_debt() {
        let mut progress = RunProgress::default();
        for (id, script) in [("directory", "Get-ChildItem -LiteralPath ."),
            ("registry", "Get-ItemProperty -LiteralPath HKCU:\\Software\\Fixture"),
            ("application", "Start-Process -FilePath fixture-app.exe")] {
            progress.apply("progress.observeOutcome", &json!({
                "call":{"id":id,"name":"terminal.host.run","arguments":{"script":script}},
                "result":{"status":"completed","isError":false,"executed":true},
                "canonicalResult":{"exitCode":0},
                "beforeVersion":{"complete":false,"files":[]},"afterVersion":{"complete":false,"files":[]}
            })).unwrap();
        }
        let verification = progress.verification();
        assert_eq!(progress.mutation_revision, 3);
        assert_eq!(progress.unclassified_terminal_effects, 3);
        assert_eq!(progress.checked_mutation_revision, 0);
        assert_eq!(verification["pendingValidation"], false);
        assert_eq!(verification["state"], "no-execution-checks");
        assert_eq!(verification["codeVersionCoverage"]["actualFileHashesVerified"], false);
        assert_eq!(verification["codeVersionCoverage"]["wholeRepositoryCertified"], false);
        let state = progress.apply("progress.state", &json!({})).unwrap();
        let mut restored = RunProgress::default();
        restored.apply("progress.restoreValidation", &json!({"state":state})).unwrap();
        assert_eq!(restored.verification()["pendingValidation"], false);
        assert_eq!(restored.mutation_revision, 3);
    }

    #[test]
    fn known_failed_host_exits_do_not_create_targetless_code_debt_or_success() {
        for status in ["completed", "error"] {
            let failed = json!({"status":status,"isError":true,"executed":true});
            let mut progress = RunProgress::default();
            progress.apply("progress.observeOutcome", &json!({"call":{"id":"failed-download","name":"terminal.host.run",
                "arguments":{"script":"Invoke-FixtureDownload"}},"result":failed,
                "canonicalResult":{"exitCode":1,"success":false,"error":"fixture download rejected"}})).unwrap();
            progress.apply("progress.observeOutcome", &json!({"call":{"id":"alternate-download","name":"terminal.host.run"},
                "result":{"status":"completed","isError":false,"executed":true},"canonicalResult":{"exitCode":0}})).unwrap();
            assert_eq!(progress.verification()["pendingValidation"], false);
            assert_eq!(progress.code_mutation_revision, 0);
            assert_eq!(progress.unclassified_terminal_effects, 2);
            assert_eq!(progress.mutation_revision, 2);
            assert_eq!(failed["isError"], true);
            assert!(progress.validation_receipts.is_empty());
            assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"complete","unmet":["missing user output"]}}))
                ["completionState"], "partial");
        }
    }

    #[test]
    fn host_uncertainty_stays_auditable_without_blocking_unrelated_work() {
        for (status, output) in [("unknown", json!({"exitCode":null})),
            ("cancelled", json!({"exitCode":null,"cancelled":true})),
            ("completed", json!({"exitCode":0,"timedOut":true})),
            ("completed", json!({"exitCode":1,"outcomeUnknown":true}))] {
            let mut progress = RunProgress::default();
            progress.apply("progress.observeOutcome", &json!({"call":{"id":"uncertain","name":"terminal.host.run",
                "arguments":{"script":"Invoke-FixtureOperation"}},
                "result":{"status":status,"isError":status!="completed"},"canonicalResult":output})).unwrap();
            assert_eq!(progress.verification()["pendingValidation"], false);
            assert_eq!(progress.code_mutation_revision, 0);
            assert_eq!(progress.uncertain_mutations.len(), 1);
            assert!(progress.validation_receipts.is_empty());
            assert_eq!(progress.complete(&json!({"modelTaskState":{"state":"continue","unmet":["deliver output"]}}))["completionState"], "partial");
            let state = progress.apply("progress.state", &json!({})).unwrap();
            let mut restored = RunProgress::default();
            restored.apply("progress.restoreValidation", &json!({"state":state})).unwrap();
            assert_eq!(restored.verification()["pendingValidation"], false);
            assert_eq!(restored.uncertain_mutations.len(), 1);
        }
        let mut progress = RunProgress::default();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"changed-code","name":"terminal.host.run",
            "arguments":{"script":"Invoke-FixtureOperation"}},"result":{"status":"error","isError":true},
            "canonicalResult":{"exitCode":1},"beforeVersion":snapshot('a'),"afterVersion":snapshot('b')})).unwrap();
        assert_eq!(progress.verification()["pendingValidation"], true);
        assert_eq!(progress.code_mutation_revision, progress.mutation_revision);
        let mut observed = RunProgress::default();
        observed.apply("progress.finalVersion", &json!({"snapshot":snapshot('a')})).unwrap();
        observed.apply("progress.finalVersion", &json!({"snapshot":snapshot('b')})).unwrap();
        assert_eq!(observed.verification()["pendingValidation"], true);
        check(&mut progress,"test-failed","cargo test",1,&snapshot('b'));
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"query","name":"terminal.host.run",
            "arguments":{"script":"Get-ChildItem"}},"result":{"status":"completed","isError":false},
            "canonicalResult":{"exitCode":0},"beforeVersion":snapshot('b'),"afterVersion":snapshot('b')})).unwrap();
        assert_eq!(progress.verification()["pendingValidation"], true);
        assert!(progress.verification()["unresolvedChecks"].as_array().unwrap().iter()
            .any(|check| check["toolCallId"] == "test-failed" && check["resolution"] == "failed"));
        let mut uncertain_check = RunProgress::default();
        uncertain_check.apply("progress.observeOutcome", &json!({"call":{"id":"test-unknown","name":"terminal.host.run",
            "arguments":{"script":"cargo test"}},"result":{"status":"unknown","isError":true},
            "canonicalResult":{"exitCode":null},"beforeVersion":snapshot('a'),"afterVersion":snapshot('a')})).unwrap();
        assert_eq!(uncertain_check.verification()["pendingValidation"], true);
        assert_eq!(uncertain_check.complete(&json!({}))["completionState"], "partial");
        assert_eq!(uncertain_check.verification()["state"], "execution-unconfirmed");
    }

    #[test]
    fn legacy_targetless_host_reclassification_requires_complete_owned_receipts() {
        let state = json!({"mutationRevision":2,"checkedMutationRevision":0,"unclassifiedTerminalEffects":2,
            "artifactRevisions":[],"requiredChecks":[],"uncertainMutations":[],
            "codeMutationRevision":2,"hostEffectClassificationVersion":1,
            "latestCodeVersion":{"complete":false,"files":[]}});
        let calls = ["failed-download", "alternate-download"].into_iter().enumerate().map(|(index,id)| json!({
            "call":{"id":id,"name":"terminal.host.run","arguments":{"script":"Invoke-FixtureOperation"}},
            "result":{"status":if index == 0 {"error"} else {"completed"},"isError":index == 0,"executed":true},
            "output":{"exitCode":if index == 0 {1} else {0}}
        })).collect::<Vec<_>>();
        assert!(needs_host_effect_reclassification(&state));
        let mut restored = RunProgress::default();
        restored.apply("progress.restoreValidation", &json!({"state":state,"legacyCalls":calls})).unwrap();
        assert_eq!(restored.verification()["pendingValidation"], false);
        assert_eq!(restored.mutation_revision, 2);
        assert_eq!(restored.checked_mutation_revision, 0);
        assert_eq!(restored.unclassified_terminal_effects, 2);
        let migrated = restored.apply("progress.state", &json!({})).unwrap();
        assert_eq!(migrated["hostEffectClassificationVersion"], HOST_EFFECT_CLASSIFICATION_VERSION);
        assert!(!needs_host_effect_reclassification(&migrated));
        restored.apply("progress.restoreValidation", &json!({"state":migrated})).unwrap();
        assert_eq!(restored.verification()["pendingValidation"], false);
        assert_eq!(calls[0]["result"]["isError"], true);
        let mut oldest = state.clone();
        oldest.as_object_mut().unwrap().remove("codeMutationRevision");
        oldest.as_object_mut().unwrap().remove("hostEffectClassificationVersion");
        restored.apply("progress.restoreValidation", &json!({"state":oldest,"legacyCalls":calls})).unwrap();
        assert_eq!(restored.verification()["pendingValidation"], false);
        for legacy in [json!([]), json!([calls[0]]), json!([calls[0],calls[0]]),
            json!([calls[0],{"call":{"id":"missing-receipt","name":"terminal.host.run"},"output":{}}])] {
            let mut conservative = RunProgress::default();
            conservative.apply("progress.restoreValidation", &json!({"state":state,"legacyCalls":legacy})).unwrap();
            assert_eq!(conservative.verification()["pendingValidation"], true);
            assert_eq!(conservative.host_effect_classification_version, 1);
        }
        let mut uncertain_state = state.clone();
        uncertain_state["hostEffectClassificationVersion"] = json!(2);
        uncertain_state["uncertainMutations"] = json!(["unconfirmed"]);
        let unknown_receipt = json!({"call":{"id":"unconfirmed","name":"terminal.host.run"},
            "result":{"status":"unknown","isError":true,"executed":true},"output":{"exitCode":null}});
        assert!(needs_host_effect_reclassification(&uncertain_state));
        let mut unconfirmed = RunProgress::default();
        unconfirmed.apply("progress.restoreValidation", &json!({"state":uncertain_state,"legacyCalls":[calls[0],unknown_receipt]})).unwrap();
        assert_eq!(unconfirmed.verification()["pendingValidation"], false);
        assert_eq!(unconfirmed.uncertain_mutations.len(), 1);
        assert!(unconfirmed.validation_receipts.is_empty());
        assert_eq!(unknown_receipt["result"]["status"], "unknown");
    }

    #[test]
    fn failed_test_survives_unrelated_build_and_only_its_current_retry_resolves_it() {
        let mut progress = RunProgress::default();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"edit","name":"filesystem.edit"},
            "result":{"status":"completed","isError":false}})).unwrap();
        check(&mut progress,"test-failed","cargo test",1,&snapshot('a'));
        check(&mut progress,"build-passed","cargo build",0,&snapshot('a'));
        assert_eq!(progress.verification()["state"],"checks-failed");
        assert_eq!(progress.checked_mutation_revision,0);
        assert!(progress.needs_validation("修复代码"));
        check(&mut progress,"test-passed","cargo   test",0,&snapshot('a'));
        assert_eq!(progress.verification()["pendingValidation"],false);
        assert_eq!(progress.checked_mutation_revision,1);
        assert_eq!(progress.verification()["receipts"][0]["current"],false);
        assert_eq!(progress.verification()["unresolvedChecks"],json!([]));
    }

    #[test]
    fn changed_overlapping_versions_require_revalidation_instead_of_reusing_failure_or_success() {
        let mut progress = RunProgress::default();
        check(&mut progress,"test-failed","cargo test",1,&snapshot('a'));
        check(&mut progress,"build-passed","cargo build",0,&snapshot('a'));
        progress.apply("progress.finalVersion",&json!({"snapshot":snapshot('b')})).unwrap();
        assert_eq!(progress.verification()["state"],"needs-validation");
        assert!(progress.verification()["unresolvedChecks"].as_array().unwrap().iter()
            .all(|check| check["active"] == true && check["resolution"] == "needs-revalidation"));
        check(&mut progress,"test-current","cargo test",0,&snapshot('b'));
        assert_eq!(progress.verification()["pendingValidation"],true);
        check(&mut progress,"build-current","cargo build",0,&snapshot('b'));
        assert_eq!(progress.verification()["pendingValidation"],false);
    }

    #[test]
    fn only_explicit_disjoint_targets_make_old_checks_inactive() {
        let mut progress = RunProgress::default();
        let first=json!({"signature":"a".repeat(64),"complete":true,"files":[{"pathId":"target-a"}]});
        let second=json!({"signature":"b".repeat(64),"complete":true,"files":[{"pathId":"target-b"}]});
        check(&mut progress,"old-test","cargo test",1,&first);
        check(&mut progress,"other-build","cargo build",0,&second);
        assert_eq!(progress.verification()["pendingValidation"],true,
            "a restarted or changed observation subset in the same workspace cannot retire the original check");
        progress.apply("progress.observeOutcome", &json!({
            "call":{"id":"new-workspace-build","name":"terminal.host.run","arguments":{"script":"cargo build"}},
            "result":{"status":"completed","isError":false},"canonicalResult":{"exitCode":0,"cwd":"D:/other-workspace"},
            "validationScope":{"cwd":"D:/other-workspace"},"beforeVersion":second,"afterVersion":second
        })).unwrap();
        assert_eq!(progress.verification()["pendingValidation"],false);
        assert!(progress.verification()["requiredChecks"].as_array().unwrap().iter()
            .any(|check| check["toolCallId"] == "old-test" && check["active"] == false));
        let extended=json!({"signature":"c".repeat(64),"complete":true,"files":[{"pathId":"target-a"},{"pathId":"target-b"}]});
        progress.apply("progress.finalVersion",&json!({"snapshot":extended})).unwrap();
        assert!(progress.verification()["requiredChecks"].as_array().unwrap().iter()
            .all(|check| check["active"] == true));
        let opaque=json!({"signature":"d".repeat(64),"complete":true,"files":[{}]});
        progress.apply("progress.finalVersion",&json!({"snapshot":opaque})).unwrap();
        assert!(progress.verification()["requiredChecks"].as_array().unwrap().iter().all(|check| check["active"] == true));
    }

    #[test]
    fn check_requirements_survive_display_history_trimming_and_checkpoint_restore() {
        let mut progress = RunProgress::default();
        check(&mut progress,"failed","cargo test -p old",1,&snapshot('a'));
        for index in 0..65 {
            check(&mut progress,&format!("check-{index}"),&format!("cargo check -p fixture-{index}"),0,&snapshot('a'));
        }
        assert_eq!(progress.validation_receipts.len(),64);
        assert_eq!(progress.required_checks.len(),66);
        assert_eq!(progress.verification()["state"],"checks-failed");
        let state=progress.apply("progress.state",&json!({})).unwrap();
        let mut restored=RunProgress::default();
        restored.apply("progress.restoreValidation",&json!({"state":state})).unwrap();
        assert_eq!(restored.verification()["state"],"checks-failed");
        check(&mut restored,"retried","cargo test -p old",0,&snapshot('a'));
        assert_eq!(restored.verification()["pendingValidation"],false);
    }

    #[test]
    fn legacy_calls_only_recover_identity_from_matching_real_validation_calls() {
        let mut original=RunProgress::default();
        check(&mut original,"failed","cargo test",1,&snapshot('a'));
        let mut legacy=original.validation_receipts[0].clone();
        for key in ["checkId","commandId","scopeId","category","executionEnvironment"] { legacy.as_object_mut().unwrap().remove(key); }
        let mut restored=RunProgress::default();
        restored.apply("progress.restoreValidation",&json!({"state":{"validationReceipts":[legacy.clone()]},
            "legacyCalls":[{"call":{"id":"failed","name":"terminal.host.run","arguments":{"script":"cargo test"}},
                "output":{"cwd":"D:/work","exitCode":1},"validationScope":{"cwd":"D:/fallback"}}]})).unwrap();
        check(&mut restored,"retried","cargo test",0,&snapshot('a'));
        assert_eq!(restored.verification()["pendingValidation"],false);
        let mut conservative=RunProgress::default();
        conservative.apply("progress.restoreValidation",&json!({"state":{"validationReceipts":[legacy]}})).unwrap();
        check(&mut conservative,"unmatched","cargo build",0,&snapshot('a'));
        assert_eq!(conservative.verification()["state"],"checks-failed");
    }

    #[test]
    fn command_identity_uses_actual_cwd_and_separates_execution_environments() {
        let mut host=RunProgress::default();
        check(&mut host,"one","cargo test --test specific",1,&snapshot('a'));
        let receipt=host.validation_receipts[0].clone();
        let mut other=RunProgress::default();
        other.apply("progress.observeOutcome",&json!({"call":{"id":"two","name":"terminal.host.run","arguments":{"script":"cargo test --test specific"}},
            "result":{"status":"completed","isError":false},"canonicalResult":{"exitCode":0,"cwd":"d:\\WORK\\"},
            "validationScope":{"cwd":"D:/different-fallback"},"beforeVersion":snapshot('a'),"afterVersion":snapshot('a')})).unwrap();
        assert_eq!(receipt["checkId"],other.validation_receipts[0]["checkId"]);
        other.apply("progress.observeOutcome",&json!({"call":{"id":"three","name":"terminal.run","arguments":{"command":"cargo","args":["test","--test","specific"]}},
            "result":{"status":"completed","isError":false},"canonicalResult":{"exitCode":0,"cwd":"D:/work","sandbox":"appcontainer","tokenVerified":true,"workspaceCopy":true},
            "beforeVersion":snapshot('a'),"afterVersion":snapshot('a')})).unwrap();
        assert_ne!(receipt["checkId"],other.validation_receipts[1]["checkId"]);
        let public=host.verification();
        assert!(public["requiredChecks"][0].get("command").is_none());
        assert!(public["requiredChecks"][0].get("cwd").is_none());
    }

    #[test]
    fn unloaded_calls_are_measured_as_blocked_without_inventing_dispatch() {
        let mut progress = RunProgress::default();
        progress.apply("progress.recordTool", &json!({"id":"unloaded","round":1,"durationMs":7,"approvalMs":0,"reused":false,"dispatched":false})).unwrap();
        let metrics = progress.apply("progress.diagnostics", &json!({})).unwrap();
        assert_eq!(metrics["executedToolCalls"], 0);
        assert_eq!(metrics["blockedToolCalls"], 1);
        assert_eq!(metrics["toolCallsTiming"][0]["dispatched"], false);
        let call = json!({"id":"transparent-write","name":"filesystem.write","unavailable":true});
        let output = json!({"path":"/work/transparent.md","sha256":"a".repeat(64)});
        progress.apply("progress.observeOutcome", &json!({"call":call,
            "result":{"status":"completed","executed":false},"canonicalResult":output})).unwrap();
        assert_eq!(progress.verification()["documentValidation"]["targetCount"], 0);
        progress.apply("progress.observeOutcome", &json!({"call":call,
            "result":{"status":"completed","executed":true},"canonicalResult":output})).unwrap();
        assert_eq!(progress.verification()["documentValidation"]["targetCount"], 1,
            "transparent schema loading cannot erase a real dispatched write receipt");
    }

    fn snapshot(hash: char) -> Value {
        json!({"signature": hash.to_string().repeat(64), "complete": true, "files": [{"path": "source.rs"}]})
    }

    #[test]
    fn diagnostics_are_atomic_bounded_and_separate_approval_wait() {
        let mut progress = RunProgress::default();
        progress.apply("progress.recordTool", &json!({"id": "call", "round": 1, "durationMs": 120, "approvalMs": 90, "reused": false})).unwrap();
        assert_eq!(progress.diagnostics["totalToolMs"], 30);
        assert_eq!(progress.diagnostics["totalApprovalWaitMs"], 90);
        progress
            .apply(
                "progress.recordModel",
                &json!({"round": 1, "durationMs": MAX_SAFE_INTEGER}),
            )
            .unwrap();
        let before = progress.diagnostics.clone();
        assert_eq!(
            progress
                .apply(
                    "progress.recordModel",
                    &json!({"round": 1, "durationMs": 1})
                )
                .unwrap_err(),
            DIAGNOSTIC_ERROR
        );
        assert_eq!(progress.diagnostics, before);
        assert_eq!(progress.apply("progress.recordTool", &json!({"id": "call", "round": 1, "durationMs": 1, "approvalMs": 2, "reused": false})).unwrap_err(), DIAGNOSTIC_ERROR);
        assert_eq!(progress.diagnostics, before);
    }

    #[test]
    fn actual_host_version_changes_invalidate_checks_and_unknown_effects_do_not_replay() {
        let mut progress = RunProgress::default();
        let call = json!({"id": "test", "name": "terminal.host.run", "arguments": {"script": "node --test check.mjs"}});
        let result =
            json!({"status": "completed", "isError": false, "content": "{\"exitCode\":0}"});
        progress.apply("progress.observeOutcome", &json!({"call": call, "result": result, "beforeVersion": snapshot('a'), "afterVersion": snapshot('a')})).unwrap();
        assert_eq!(progress.verification()["pendingValidation"], false);
        progress
            .apply("progress.finalVersion", &json!({"snapshot": snapshot('b')}))
            .unwrap();
        assert_eq!(progress.verification()["pendingValidation"], true);
        progress
            .apply(
                "progress.observeOutcome",
                &json!({"call": {"id": "write", "name": "filesystem.edit"},
            "result": {"status": "unknown", "isError": true}}),
            )
            .unwrap();
        assert_eq!(progress.verification()["state"], "needs-validation");
        assert!(progress.needs_validation("修复代码"));
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"after-edit","name":"terminal.host.run",
            "arguments":{"script":"node --test check.mjs"}},"result":{"status":"completed","isError":false},
            "canonicalResult":{"exitCode":0},"beforeVersion":snapshot('b'),"afterVersion":snapshot('b')})).unwrap();
        assert_eq!(progress.verification()["pendingValidation"], false);
        assert_eq!(progress.uncertain_mutations.len(), 1);
    }

    #[test]
    fn host_receipt_wrappers_preserve_execution_and_version_verification() {
        for (status, exit_code, expected) in [
            ("completed", Some(0), "passed"),
            ("error", Some(1), "failed"),
            ("unknown", None, "unknown"),
        ] {
            let mut progress = RunProgress::default();
            progress
                .apply(
                    "progress.observeOutcome",
                    &json!({
                        "call": {"id": "wrapped-check", "name": "terminal.host.run",
                            "arguments": {"script": "node --test fixture.mjs"}},
                        "result": {"status": status, "isError": status != "completed"},
                        "canonicalResult": {"value": {"exitCode": exit_code,
                            "stdout": "actual stdout", "stderr": "actual stderr"}},
                        "beforeVersion": snapshot('a'), "afterVersion": snapshot('a')
                    }),
                )
                .unwrap();
            let receipt = &progress.validation_receipts[0];
            assert_eq!(receipt["exitCode"], json!(exit_code));
            assert_eq!(receipt["status"], expected);
            assert_eq!(receipt["passed"], status == "completed");
            assert_eq!(receipt["coversObservedRevision"], status == "completed");
            if status != "completed" {
                assert_eq!(receipt["outputSummary"]["stdoutTail"], "actual stdout");
                assert_eq!(receipt["outputSummary"]["stderrTail"], "actual stderr");
            }
            if status == "completed" {
                progress
                    .apply("progress.finalVersion", &json!({"snapshot": snapshot('b')}))
                    .unwrap();
                assert_eq!(progress.verification()["pendingValidation"], true);
            }
        }
    }

    #[test]
    fn structured_receipt_content_precedes_the_legacy_value_wrapper() {
        let mut progress = RunProgress::default();
        progress
            .apply(
                "progress.observeOutcome",
                &json!({
                    "call": {"id": "structured-check", "name": "terminal.host.run",
                        "arguments": {"script": "node --test fixture.mjs"}},
                    "result": {"status": "completed", "isError": false},
                    "canonicalResult": {"structuredContent": {"exitCode": 0},
                        "value": {"exitCode": 1}},
                    "beforeVersion": snapshot('a'), "afterVersion": snapshot('a')
                }),
            )
            .unwrap();
        assert_eq!(progress.validation_receipts[0]["exitCode"], 0);
        assert_eq!(
            progress.validation_receipts[0]["coversObservedRevision"],
            true
        );
    }

    #[test]
    fn sandbox_copy_checks_and_unbound_passes_keep_their_coverage_limits() {
        let mut progress = RunProgress::default();
        let call = json!({"id": "test", "name": "terminal.run", "arguments": {"command": "cmd", "args": ["/d", "/c", "node --test check.mjs"]}});
        assert!(validation_call(&call));
        progress.apply("progress.observeOutcome", &json!({"call": call, "result": {"status": "completed", "isError": false},
            "canonicalResult": {"exitCode": 0, "sandbox": "appcontainer", "tokenVerified": true, "workspaceCopy": true},
            "beforeVersion": snapshot('a'), "afterVersion": snapshot('a')})).unwrap();
        assert_eq!(
            progress.verification()["state"],
            "checks-version-unverified"
        );
        assert!(!progress.needs_validation("修复代码"));
        assert_eq!(progress.summary()["receiptCount"], 1);
        assert!(progress.summary().get("validationReceipts").is_none());
        assert!(!validation_call(
            &json!({"name": "terminal.host.run", "arguments": {"script": "echo npm test"}})
        ));
        assert!(!validation_call(
            &json!({"name": "terminal.host.run", "arguments": {"script": "npm test && echo unsafe"}})
        ));
    }

    #[test]
    fn final_task_outcomes_keep_transport_success_separate_from_checks_and_sources() {
        let mut progress = RunProgress::default();
        assert_eq!(
            progress.complete(&json!({}))["completionState"],
            "completed"
        );
        let failed = progress.complete(&json!({"evidenceValidation": {"current": false,
            "invalidSources": [{"sourceId": "fixture", "code": "STALE_RETRIEVAL_SOURCE"}]}}));
        assert_eq!(failed["completionState"], "verification-failed");
        assert_eq!(failed["sourceValidation"]["current"], false);
        assert_eq!(failed["correctnessCertified"], false);
        assert_eq!(progress.complete(&json!({"unknownEffects":true}))["completionState"], "completed");
        let unknown = progress.complete(&json!({"unknownEffects": true, "interrupted": true}));
        assert_eq!(unknown["completionState"], "partial");
        assert_eq!(unknown["state"], "incomplete");
        let partial = progress.complete(&json!({"interrupted": true, "code": "TASK_BLOCKED"}));
        assert_eq!(partial["completionState"], "partial");
        assert_eq!(partial["state"], "incomplete");
        assert_eq!(progress.task_completion, partial);
        let proposed = json!({"state":"continue","next":"Optional additional work","unmet":[]});
        let mut accepted = json!({"modelTaskState":proposed,"requiresVerification":true,"acceptanceReviewed":true});
        assert_eq!(progress.complete(&accepted)["completionState"], "partial");
        accepted["brokerDecision"] = json!("final");
        let complete = progress.complete(&accepted);
        assert_eq!(complete["completionState"], "completed");
        assert_eq!(complete["modelTaskState"], proposed, "Keep the model proposal rather than rewrite its history");
        accepted["modelTaskState"]["unmet"] = json!(["A required current item is missing"]);
        assert_eq!(progress.complete(&accepted)["completionState"], "partial");
        progress.apply("progress.observeOutcome", &json!({"call": {"id": "check", "name": "terminal.host.run",
            "arguments": {"script": "node --test fixture.mjs"}}, "result": {"status": "completed", "isError": true},
            "canonicalResult": {"exitCode": 1}})).unwrap();
        assert_eq!(
            progress.complete(&json!({}))["completionState"],
            "verification-failed"
        );
        accepted["modelTaskState"]["unmet"] = json!([]);
        assert_eq!(progress.complete(&accepted)["completionState"], "verification-failed",
            "A broker delivery cannot override an actual failed check");
    }
}

#[cfg(test)]
mod python_validation_tests {
    use super::*;

    #[test]
    fn python_test_modules_are_validation_but_arbitrary_scripts_are_not() {
        for command in [
            "py -3.12 -m unittest discover -p test_children.py",
            "python -m pytest -q",
            "python3.12 -m unittest test_task",
            "py.exe -3 -m pytest",
        ] {
            assert!(
                validation_call(
                    &json!({"name":"terminal.host.run","arguments":{"command":command}})
                ),
                "{command}"
            );
        }
        for command in [
            "py -3.12 script.py",
            "python -c 'print(1)'",
            "python -m unittest_other",
            "py -m unittest; echo okay",
        ] {
            assert!(
                !validation_call(
                    &json!({"name":"terminal.host.run","arguments":{"command":command}})
                ),
                "{command}"
            );
        }
    }
}
