use std::collections::HashSet;

use serde_json::{Value, json};

/// The workflow owns recovery stages and dispatch identity; adapters execute only admitted effects.
/// 工作流拥有恢复阶段和派发身份；适配器只执行已经准入的操作。
#[derive(Default)]
pub struct WorkflowState {
    segments: super::segments::SegmentState,
    summarize_only: bool,
    recovery_final: bool,
    context_discovery_only: bool,
    finalizing_unavailable: bool,
    repair_requests: u64,
    context_repair_requests: u64,
    evidence_repair_requests: u64,
    unknown_observation_rounds: u64,
    unavailable_rounds: u64,
    calls_run: u64,
    seen_ids: HashSet<String>,
}

impl WorkflowState {
    fn state(&self) -> Value {
        json!({"summarizeOnly":self.summarize_only,"recoveryFinal":self.recovery_final,
            "contextDiscoveryOnly":self.context_discovery_only,"finalizingUnavailable":self.finalizing_unavailable,
            "repairRequests":self.repair_requests,"contextRepairRequests":self.context_repair_requests,
            "evidenceRepairRequests":self.evidence_repair_requests,"callsRun":self.calls_run})
    }

    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if let Some(operation) = operation.strip_prefix("orchestration.workflow.segments.") {
            return self.segments.apply(operation, args);
        }
        let mut decision = json!({});
        let boolean = |name: &str| args[name].as_bool().unwrap_or(false);
        let number = |name: &str| args[name].as_u64().unwrap_or(0);
        match operation {
            "orchestration.workflow.beginRound" => {
                let unknown = boolean("unknownEffects");
                let force_final = unknown
                    && !self.recovery_final
                    && (self.unknown_observation_rounds >= 8
                        || number("round") + 1 >= number("maxRounds"));
                if force_final {
                    self.recovery_final = true;
                    self.summarize_only = true;
                }
                if unknown && !self.recovery_final {
                    self.unknown_observation_rounds += 1;
                }
                let mode = if self.summarize_only || boolean("completionReview") {
                    "none"
                } else if self.context_discovery_only {
                    "discovery"
                } else if unknown {
                    "observations"
                } else {
                    "all"
                };
                decision = json!({"forceUnknownFinal":force_final,"catalogMode":mode,
                    "prompt":if force_final { "[KYNXA_UNKNOWN_EFFECT_FINAL] State checks did not verify an earlier dispatch. Tools are disabled. Give a normal partial answer with verified receipts and the unresolved outcome; do not replay operations or claim completion." } else { "" }});
            }
            "orchestration.workflow.validateIds" => {
                for id in args["ids"]
                    .as_array()
                    .ok_or("AGENT_CORE_INVALID_ARGUMENT")?
                {
                    let id = id.as_str().ok_or("AGENT_CORE_INVALID_ARGUMENT")?;
                    if self.seen_ids.contains(id) {
                        return Err("MODEL_TOOL_IDENTITY_INVALID".into());
                    }
                }
            }
            "orchestration.workflow.admitCall" => {
                let id = args["id"].as_str().ok_or("AGENT_CORE_INVALID_ARGUMENT")?;
                if self.seen_ids.contains(id) {
                    return Err("MODEL_TOOL_IDENTITY_INVALID".into());
                }
                self.calls_run += 1;
                if self.calls_run > number("maxToolCalls") {
                    return Err("TOOL_RUN_CALL_LIMIT".into());
                }
                self.seen_ids.insert(id.to_owned());
            }
            "orchestration.workflow.modelError" => {
                let code = args["code"].as_str().unwrap_or("");
                let category = args["category"].as_str().unwrap_or("");
                let remaining = number("remainingRounds");
                let action = if ["AGENT_CONFIG_CHANGED", "MCP_CATALOG_CHANGED"].contains(&code) {
                    "throw"
                } else if boolean("completionReview") && category == "repair-unexecuted-model-step"
                {
                    if number("reviewAttempts") >= 2 || remaining == 0 {
                        "review-failed"
                    } else {
                        "repair-review"
                    }
                } else if [
                    "MODEL_CONTEXT_LIMIT_REJECTED",
                    "CONTEXT_INPUT_TOO_LARGE",
                    "TOOL_CONTEXT_BUDGET_EXCEEDED",
                ]
                .contains(&code)
                {
                    if self.context_repair_requests < 2 && remaining > 0 {
                        "checkpoint"
                    } else {
                        "context-failed"
                    }
                } else if category != "repair-unexecuted-model-step" && !self.recovery_final {
                    "throw"
                } else if self.recovery_final || remaining == 0 {
                    "recover-final"
                } else {
                    let can_repair = self.repair_requests < 1 && remaining > 1;
                    self.repair_requests += 1;
                    self.recovery_final = !can_repair;
                    self.summarize_only = self.recovery_final;
                    if can_repair {
                        "repair-step"
                    } else {
                        "summarize-step"
                    }
                };
                decision = json!({"action":action});
            }
            "orchestration.workflow.contextCheckpoint" => {
                self.context_repair_requests += 1;
                self.context_discovery_only = true;
                if self.context_repair_requests >= 2 {
                    self.summarize_only = true;
                    self.recovery_final = true;
                }
            }
            "orchestration.workflow.contextFinal" => {
                self.summarize_only = true;
                self.recovery_final = true;
            }
            "orchestration.workflow.finalReadiness" => {
                let repair = boolean("evidenceInvalid")
                    && !self.summarize_only
                    && self.evidence_repair_requests < 2;
                let validate = !boolean("hasCalls")
                    && !self.summarize_only
                    && boolean("needsValidation")
                    && number("validationRequests") < 2;
                if self.summarize_only && boolean("hasUnavailableCalls") {
                    self.finalizing_unavailable = true;
                }
                decision = json!({"repairEvidence":repair,"validateCode":validate,
                    "markFinal":!boolean("hasCalls") && !validate && !repair && !boolean("unknownEffects"),
                    "dispatchUnavailableOnly":self.summarize_only && self.finalizing_unavailable});
            }
            "orchestration.workflow.evidenceRepair" => {
                self.evidence_repair_requests += 1;
            }
            "orchestration.workflow.results" => {
                let unavailable = boolean("hasUnavailableCall");
                self.unavailable_rounds = if unavailable && !boolean("hasSuccessfulCall") {
                    self.unavailable_rounds + 1
                } else {
                    0
                };
                let mut prompts = vec![];
                if unavailable {
                    self.finalizing_unavailable = self.unavailable_rounds >= 2;
                    self.summarize_only = self.finalizing_unavailable;
                    prompts.push(if self.finalizing_unavailable {
                        "[KYNXA_UNAVAILABLE_TOOL_FINAL] Unavailable calls were not executed. Tools are disabled for this final response. Give a normal final answer based on verified evidence, with a concrete limitation if needed. Do not invent success or a user cancellation."
                    } else {
                        "[KYNXA_UNAVAILABLE_TOOL_RECOVERY] An undeclared tool was not executed. Use only exact names in the current declarations; discover and explicitly load a permitted deferred tool if needed. Exhausted web tools cannot be re-enabled by discovery. If evidence is sufficient, answer now. This is runtime feedback, not a new user task."
                    });
                }
                if boolean("configurationRevokedBeforeDispatch") {
                    prompts.push("[KYNXA_TOOL_CONFIGURATION_RECOVERY] A tool capability changed before dispatch and was not executed. That previous capability remains revoked. Other unchanged tools are available: use tool.search and tool.load to discover a permitted alternative, or explain the specific blocker using verified results. Do not replay through the revoked tool or invent a user cancellation.");
                }
                if boolean("readFailureWarning") || boolean("readFailureFinalize") {
                    self.summarize_only |= boolean("readFailureFinalize");
                    prompts.push(if boolean("readFailureFinalize") {
                        "[KYNXA_READ_FAILURE_FINAL] The same observation target failed three times. Tools are disabled for this final response. Explain the concrete blocker and preserve verified findings. Do not claim that the page was read, the task completed, or the user cancelled. The conversation and saved history remain available."
                    } else { "[KYNXA_READ_FAILURE_WARNING] Reading the same target failed twice. Change the connection, use browser DOM/frames instead of desktop UIA, or report the blocker. Do not repeat the same failing read or reactivate the browser just to retry it. This is runtime guidance, not a new user task." });
                } else if boolean("observationWarning") || boolean("observationFinalize") {
                    self.summarize_only |= boolean("observationFinalize");
                    prompts.push(if boolean("observationFinalize") {
                        "[KYNXA_NO_PROGRESS_FINAL] Repeated successful read/search calls produced no new observations. Tools are disabled for this final response. Answer the original task using the existing evidence and actual source URLs. If unresolved, state the specific missing evidence or blocker. Do not claim unsupported completion or repeat the process."
                    } else { "[KYNXA_NO_PROGRESS_WARNING] Repeated successful read/search calls produced no new observations. Reassess the original task: answer now if evidence is sufficient; otherwise change the query, page, or approach to obtain genuinely new evidence. Repeating the same observations will end tool execution. This is runtime guidance, not a new user task." });
                }
                decision = json!({"prompts":prompts});
            }
            _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
        Ok(json!({"decision":decision,"state":self.state()}))
    }
}
