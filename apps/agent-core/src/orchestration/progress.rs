//! Request-local diagnostics, mutation tracking, and version-bound validation receipts.
//! 请求内诊断、修改跟踪和绑定代码版本的验证回执；不负责执行工具或正式持久化。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const DIAGNOSTIC_ERROR: &str = "INVALID_TOOL_RUN_DIAGNOSTICS";
const MAX_DOCUMENT_TARGETS: usize = 4096;

/// A prose artifact is checked by complete, hash-matched reads; code still needs execution checks.
/// 文本产物依据完整且哈希一致的回读核验；代码修改仍须真实执行检查，回读不证明语义正确。
#[derive(Default)]
struct DocumentValidation {
    sha256: String,
    total_characters: Option<u64>,
    ranges: Vec<(u64, u64)>,
    read_back: bool,
    result_ref: Value,
}

/// Request-local metrics and execution evidence never certify semantic task correctness.
/// 请求内计量和执行证据不能认证任务语义正确；计时及正式持久化仍属于调用端。
pub struct RunProgress {
    mutation_revision: u64,
    checked_mutation_revision: u64,
    validation_receipts: Vec<Value>,
    uncertain_mutations: BTreeSet<String>,
    unclassified_terminal_effects: u64,
    latest_code_version: Value,
    validated_code_version: Value,
    task_completion: Value,
    diagnostics: Value,
    artifact_revisions: BTreeSet<u64>,
    document_validations: BTreeMap<String, DocumentValidation>,
}

impl Default for RunProgress {
    fn default() -> Self {
        Self {
            mutation_revision: 0,
            checked_mutation_revision: 0,
            validation_receipts: Vec::new(),
            uncertain_mutations: BTreeSet::new(),
            unclassified_terminal_effects: 0,
            latest_code_version: Value::Null,
            validated_code_version: Value::Null,
            task_completion: Value::Null,
            artifact_revisions: BTreeSet::new(),
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
            "progress.finalVersion" => {
                self.latest_code_version = version_identity(&args["snapshot"]);
                if !self.validated_code_version.is_null()
                    && (self.latest_code_version["complete"] != true
                        || self.latest_code_version["signature"]
                            != self.validated_code_version["signature"])
                {
                    self.mutation_revision += 1;
                    self.validated_code_version = Value::Null;
                }
                Ok(Value::Null)
            }
            "progress.needsValidation" => Ok(json!(
                self.needs_validation(args["task"].as_str().unwrap_or(""))
            )),
            "progress.verification" => Ok(self.verification()),
            "progress.completion" => Ok(self.complete(args)),
            "progress.diagnostics" => Ok(self.diagnostics.clone()),
            "progress.field" => self.field(args["name"].as_str().unwrap_or("")),
            "progress.state" => Ok(json!({
                "mutationRevision": self.mutation_revision,
                "checkedMutationRevision": self.checked_mutation_revision,
                "validationReceipts": self.validation_receipts,
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
            "checkedMutationRevision" => json!(self.checked_mutation_revision),
            "validationReceipts" => json!(self.validation_receipts),
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
            "mutationRevision" => self.mutation_revision = safe_integer(value)?,
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
        if truthy(&result["reused"]) || result["executed"] == false || truthy(&call["unavailable"])
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
        if name == "filesystem.read" && completed {
            self.observe_document_read(output, result);
            return;
        }
        if [
            "filesystem.write",
            "filesystem.edit",
            "filesystem.mkdir",
            "filesystem.move",
            "filesystem.delete",
        ]
        .contains(&name)
        {
            if completed || uncertain {
                self.mutation_revision += 1;
            }
            if completed && matches!(name, "filesystem.write" | "filesystem.edit") {
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
            if uncertain {
                self.uncertain_mutations
                    .insert(call["id"].as_str().unwrap_or("").to_owned());
            }
            return;
        }
        if ![
            "terminal.run",
            "terminal.host.run",
            "terminal.host.start",
            "skill.run",
        ]
        .contains(&name)
        {
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
        }
        let unknown = status == "unknown";
        let cancelled = status == "cancelled"
            || result["code"] == "TOOL_CANCELLED"
            || output["cancelled"] == true;
        let timed_out = output["timedOut"] == true || result["code"] == "TOOL_TIMED_OUT";
        if (unknown || cancelled || timed_out) && !isolated {
            self.mutation_revision += 1;
            self.uncertain_mutations
                .insert(call["id"].as_str().unwrap_or("").to_owned());
        }
        let exit_code = output["exitCode"].as_i64();
        let passed = completed && exit_code == Some(0) && !cancelled && !timed_out;
        let same_version = before["complete"] == true
            && after["complete"] == true
            && !before["signature"].is_null()
            && before["signature"] == after["signature"];
        // Host observations do not certify the copied sandbox version or unknown effects.
        // 宿主观察不能认证沙箱副本版本或未知副作用。
        let covered = passed && same_version && self.uncertain_mutations.is_empty() && !isolated;
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
        self.validation_receipts.push(receipt);
        if self.validation_receipts.len() > 64 {
            self.validation_receipts.remove(0);
        }
        if covered {
            self.checked_mutation_revision = self.mutation_revision;
            self.validated_code_version = after;
        }
    }

    fn observe_document_write(&mut self, output: &Value) {
        let Some(path) = output["path"].as_str().filter(|path| is_prose_target(path)) else {
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

    fn observe_document_read(&mut self, output: &Value, result: &Value) {
        let Some(path) = output["path"].as_str() else {
            return;
        };
        let Some(document) = self.document_validations.get_mut(path) else {
            return;
        };
        if content_hash(&output["sha256"]) != Some(document.sha256.as_str()) {
            document.read_back = false;
            document.ranges.clear();
            document.total_characters = None;
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
            || output["content"]
                .as_str()
                .is_none_or(|content| content.encode_utf16().count() as u64 != end - start)
        {
            return;
        }
        if document
            .total_characters
            .is_some_and(|previous| previous != total)
        {
            document.ranges.clear();
            document.read_back = false;
        }
        document.total_characters = Some(total);
        if document.ranges.len() >= MAX_DOCUMENT_TARGETS {
            return;
        }
        document.ranges.push((start, end));
        document.ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(document.ranges.len());
        for &(start, end) in &document.ranges {
            if let Some(previous) = merged.last_mut().filter(|previous| start <= previous.1) {
                previous.1 = previous.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        document.ranges = merged;
        document.read_back = document.ranges.as_slice() == [(0, total)];
        if document.read_back {
            document.result_ref = result["resultRef"].clone();
        }
        if !self.pending_code_mutation()
            && self
                .document_validations
                .values()
                .all(|document| document.read_back)
            && self.uncertain_mutations.is_empty()
        {
            self.checked_mutation_revision = self.mutation_revision;
        }
    }

    fn pending_code_mutation(&self) -> bool {
        let unchecked = self
            .mutation_revision
            .saturating_sub(self.checked_mutation_revision);
        let document_changes = self
            .artifact_revisions
            .range((
                std::ops::Bound::Excluded(self.checked_mutation_revision),
                std::ops::Bound::Unbounded,
            ))
            .count() as u64;
        unchecked > document_changes
    }

    fn needs_validation(&self, task: &str) -> bool {
        let latest = self.validation_receipts.last().unwrap_or(&Value::Null);
        let changed_during_check = latest["codeVersionCoverage"]["before"]["complete"] == true
            && latest["codeVersionCoverage"]["after"]["complete"] == true
            && latest["codeVersionCoverage"]["sameObservedVersion"] != true;
        if !self.uncertain_mutations.is_empty()
            || latest.get("exitCode").is_some_and(Value::is_null)
            || latest["passed"] == true
                && latest["mutationRevision"] == self.mutation_revision
                && latest["coversObservedRevision"] != true
                && !changed_during_check
        {
            return false;
        }
        static TASK_PATTERN: OnceLock<Regex> = OnceLock::new();
        (self.mutation_revision > self.checked_mutation_revision || latest["passed"] == false)
            && TASK_PATTERN
                .get_or_init(|| {
                    Regex::new(r"(?i)代码|修复|实现|重构|\b(?:code|fix|implement|refactor)\b")
                        .unwrap()
                })
                .is_match(task)
    }

    /// Transport completion and task outcome are separate; failed checks cannot become successful facts.
    /// 传输完成与任务结果分别记录；核验失败不能被提升为已完成事实。
    fn complete(&mut self, arguments: &Value) -> Value {
        let mut result = self.verification();
        let unknown =
            arguments["unknownEffects"] == true || result["state"] == "execution-unconfirmed";
        let evidence_invalid = arguments["evidenceValidation"]["current"] == false;
        let checks_failed = result["state"] == "checks-failed";
        let incomplete = arguments["interrupted"] == true
            || arguments["code"].as_str().is_some()
            || result["pendingValidation"] == true
            || matches!(
                arguments["modelTaskState"]["state"].as_str(),
                Some("continue" | "blocked" | "review")
            );
        result["completionState"] = json!(if unknown {
            "execution-unknown"
        } else if evidence_invalid || checks_failed {
            "verification-failed"
        } else if incomplete {
            "partial"
        } else {
            "completed"
        });
        result["correctnessCertified"] = json!(false);
        if unknown {
            result["state"] = json!("execution-unconfirmed");
        } else if arguments["interrupted"] == true {
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

    fn verification(&self) -> Value {
        let latest = self.validation_receipts.last().unwrap_or(&Value::Null);
        let failed = latest["passed"] == false;
        let version_unverified =
            latest["passed"] == true && latest["coversObservedRevision"] != true;
        let pending_document = self
            .document_validations
            .values()
            .any(|document| !document.read_back);
        let needs_check = self.pending_code_mutation() || pending_document;
        let pending =
            needs_check || failed || version_unverified || !self.uncertain_mutations.is_empty();
        let state = if !self.uncertain_mutations.is_empty()
            || latest.get("exitCode").is_some_and(Value::is_null)
        {
            "execution-unconfirmed"
        } else if failed {
            "checks-failed"
        } else if version_unverified {
            "checks-version-unverified"
        } else if needs_check {
            "needs-validation"
        } else if self
            .validation_receipts
            .iter()
            .any(|receipt| receipt["passed"] == true)
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
            "receipts": self.validation_receipts,
            "documentValidation": {
                "coverage": "complete-hash-matched-readback",
                "semanticCorrectnessCertified": false,
                "pending": pending_document,
                "targetCount": self.document_validations.len(),
                "readBackCount": self.document_validations.values().filter(|document| document.read_back).count(),
                "omittedTargets": self.document_validations.len().saturating_sub(64),
                "targets": self.document_validations.iter().take(64).map(|(path, document)| json!({
                    "path": path, "sha256": document.sha256, "totalCharacters": document.total_characters,
                    "readBackComplete": document.read_back, "resultRef": document.result_ref
                })).collect::<Vec<_>>()
            },
            "codeVersionCoverage": {
                "coverage": "observed-filesystem-targets",
                "exhaustive": false,
                "actualFileHashesVerified": latest["coversObservedRevision"] == true && !self.validated_code_version.is_null() && !pending,
                "observedMutationRevision": self.mutation_revision,
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

fn content_hash(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn is_prose_target(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if ["AGENTS.md", "SKILL.md"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return false;
    }
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        ["md", "markdown", "txt", "rst", "adoc"]
            .iter()
            .any(|prose| extension.eq_ignore_ascii_case(prose))
    })
}

pub fn validation_call(call: &Value) -> bool {
    let name = call["name"].as_str().unwrap_or("");
    if !["terminal.run", "terminal.host.run"].contains(&name) {
        return false;
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
        return false;
    }
    static COMMAND_PATTERN: OnceLock<Regex> = OnceLock::new();
    COMMAND_PATTERN.get_or_init(|| Regex::new(r"(?i)^\s*(?:dotnet(?:\.exe)?\s+(?:test|build)|cargo(?:\.exe)?\s+(?:test|check|build)|npm(?:\.cmd)?\s+(?:test|run\s+(?:test|build))|node(?:\.exe)?\s+--test|pytest(?:\.exe)?|(?:py(?:\.exe)?(?:\s+-[23](?:\.\d+)?)?|python(?:[23](?:\.\d+)?)?(?:\.exe)?)\s+-m\s+(?:pytest|unittest))(?:\s|$)").unwrap()).is_match(&command)
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

fn version_identity(snapshot: &Value) -> Value {
    let signature = snapshot["signature"]
        .as_str()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let file_count = snapshot["files"].as_array().map_or(0, Vec::len);
    let mut result = json!({
        "signature": signature,
        "complete": snapshot["complete"] == true && signature.is_some() && (1..=64).contains(&file_count),
        "fileCount": file_count.min(64),
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
    fn prose_delivery_requires_readback_but_not_a_program_test() {
        let mut progress = RunProgress::default();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"book-directory","name":"filesystem.mkdir"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":"/work/book","created":true}})).unwrap();
        write(&mut progress, "/work/book.md", 'a');
        assert_eq!(
            progress.complete(&json!({"modelTaskState":{"state":"complete"}}))["completionState"],
            "partial"
        );
        read(&mut progress, "/work/book.md", 'a', 0, 6, "正文完成😀");
        let complete = progress.complete(&json!({"modelTaskState":{"state":"complete"}}));
        assert_eq!(complete["completionState"], "completed");
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
    fn document_pages_must_cover_the_current_written_hash_and_complete_range() {
        let mut progress = RunProgress::default();
        write(&mut progress, "/work/report.txt", 'a');
        read(&mut progress, "/work/report.txt", 'a', 4, 8, "efgh");
        read(&mut progress, "/work/report.txt", 'b', 0, 8, "abcdefgh");
        assert_eq!(progress.verification()["pendingValidation"], true);
        read(&mut progress, "/work/report.txt", 'a', 0, 8, "abcd");
        assert_eq!(progress.verification()["pendingValidation"], true);
        read(&mut progress, "/work/report.txt", 'a', 4, 8, "efgh");
        assert_eq!(progress.verification()["pendingValidation"], false);
        write(&mut progress, "/work/report.txt", 'b');
        assert_eq!(progress.verification()["pendingValidation"], true);
        read(&mut progress, "/work/report.txt", 'a', 0, 8, "abcdefgh");
        assert_eq!(progress.verification()["pendingValidation"], true);
        read(&mut progress, "/work/report.txt", 'b', 0, 8, "new body");
        assert_eq!(progress.verification()["pendingValidation"], false);
    }

    #[test]
    fn prose_readback_cannot_clear_code_changes_failed_checks_or_unknown_effects() {
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
            "execution-unknown"
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
        assert_eq!(progress.verification()["pendingValidation"], true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unloaded_calls_are_measured_as_blocked_without_inventing_dispatch() {
        let mut progress = RunProgress::default();
        progress.apply("progress.recordTool", &json!({"id":"unloaded","round":1,"durationMs":7,"approvalMs":0,"reused":false,"dispatched":false})).unwrap();
        let metrics = progress.apply("progress.diagnostics", &json!({})).unwrap();
        assert_eq!(metrics["executedToolCalls"], 0);
        assert_eq!(metrics["blockedToolCalls"], 1);
        assert_eq!(metrics["toolCallsTiming"][0]["dispatched"], false);
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
        assert_eq!(progress.verification()["state"], "execution-unconfirmed");
        assert!(!progress.needs_validation("修复代码"));
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
        let unknown = progress.complete(&json!({"unknownEffects": true, "interrupted": true}));
        assert_eq!(unknown["completionState"], "execution-unknown");
        assert_eq!(unknown["state"], "execution-unconfirmed");
        let partial = progress.complete(&json!({"interrupted": true, "code": "TASK_BLOCKED"}));
        assert_eq!(partial["completionState"], "partial");
        assert_eq!(partial["state"], "incomplete");
        assert_eq!(progress.task_completion, partial);
        progress.apply("progress.observeOutcome", &json!({"call": {"id": "check", "name": "terminal.host.run",
            "arguments": {"script": "node --test fixture.mjs"}}, "result": {"status": "completed", "isError": true},
            "canonicalResult": {"exitCode": 1}})).unwrap();
        assert_eq!(
            progress.complete(&json!({}))["completionState"],
            "verification-failed"
        );
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
