use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::local_files::{absolute_path, path_key};
use super::service_filesystem::canonical;

const MAX_EFFECT_RECEIPTS: usize = 4096;

#[derive(Clone)]
struct EffectReceipt {
    call: Value,
    result: Value,
    mutable_target: Option<String>,
    reusable: bool,
}

/// Request-owned effect protection. Model metadata cannot confirm a dispatch.
/// 请求级副作用保护；模型状态和文字不能确认真实派发结果。
#[derive(Default)]
pub struct EffectLedger {
    completed: Vec<(String, EffectReceipt)>,
    protected_effects: Option<Vec<(String, EffectReceipt)>>,
    unknown: Vec<(String, EffectReceipt)>,
    receipts: Vec<Value>,
    observed_receipt_count: u64,
    events: Vec<Value>,
    verified_effects: Vec<Value>,
}

impl EffectLedger {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> Result<Value, String> {
        match operation {
            "effects.observe" => {
                let call = required_object(arguments, "call")?;
                let result = required_object(arguments, "result")?;
                if arguments["observation"].as_bool().unwrap_or(false) {
                    return Ok(Value::Null);
                }
                let key = effect_key(call)?;
                let status = result
                    .get("status")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or_else(|| {
                        json!(if is_truthy(&result["isError"]) {
                            "error"
                        } else {
                            "completed"
                        })
                    });
                // This list is a bounded diagnostic projection; recovery protection lives in the keyed sets.
                // 此列表只是有界诊断投影；恢复保护保存在按操作键组织的集合中，不能因诊断容量丢弃。
                self.observed_receipt_count = self.observed_receipt_count.saturating_add(1);
                if self.receipts.len() == MAX_EFFECT_RECEIPTS { self.receipts.remove(0); }
                self.receipts
                    .push(json!({"id": call["id"], "name": call["name"], "status": status}));
                // Rebinding an old receipt is not a new dispatch or a fresh state observation.
                // 复用回执和未派发结果不会产生新副作用，也不能刷新旧状态的有效性。
                if result["reused"].as_bool().unwrap_or(false) || result["executed"] == false {
                    return Ok(Value::Null);
                }
                let mutable_target = mutable_target(call, arguments["workspaceRoot"].as_str());
                self.invalidate_file_state(mutable_target.as_deref());
                let receipt = EffectReceipt {
                    call: call.clone(),
                    result: result.clone(),
                    mutable_target,
                    reusable: true,
                };
                if result["status"] == "unknown" {
                    set_receipt(&mut self.unknown, key, receipt);
                } else if !is_truthy(&result["isError"])
                    && (!is_truthy(&result["status"]) || result["status"] == "completed")
                {
                    // A restored protected state now belongs to this dispatch, not the superseded one.
                    // 已恢复的受保护文件状态归属于本次派发，后续修复不能再绑定过期回执。
                    if receipt.mutable_target.is_some() {
                        if let Some(protected) = &mut self.protected_effects {
                            if protected.iter().any(|(previous_key, _)| previous_key == &key) {
                                set_receipt(protected, key.clone(), receipt.clone());
                            }
                        }
                    }
                    set_receipt(&mut self.completed, key, receipt);
                }
                Ok(Value::Null)
            }
            "effects.protect" => {
                // Freeze the pre-recovery set; later work joins only at an explicit checkpoint.
                // 冻结恢复前的回执集合；后续成功操作仅在明确检查点加入保护。
                let protected = self
                    .protected_effects
                    .get_or_insert_with(|| self.completed.clone());
                if arguments["includeCurrent"].as_bool().unwrap_or(false) {
                    for (key, receipt) in &self.completed {
                        set_receipt(protected, key.clone(), receipt.clone());
                    }
                }
                Ok(Value::Null)
            }
            "effects.previous" => {
                let call = required_object(arguments, "call")?;
                if arguments["observation"].as_bool().unwrap_or(false) {
                    return Ok(Value::Null);
                }
                let key = effect_key(call)?;
                // Reuse the unconfirmed dispatch as-is; do not replay it or certify success.
                // 原样复用未确认的派发，既不重复执行，也不冒充成功。
                if let Some((_, receipt)) = self.unknown.iter().find(|(previous_key, _)| previous_key == &key) {
                    let mut previous = receipt_value(receipt);
                    previous["result"]["reused"] = json!(true);
                    return Ok(previous);
                }
                Ok(self
                    .protected_effects
                    .as_ref()
                    .and_then(|items| {
                        items.iter().find(|(previous_key, receipt)| {
                            previous_key == &key && receipt.reusable
                        })
                    })
                    .map(|(_, receipt)| receipt_value(receipt))
                    .unwrap_or(Value::Null))
            }
            "effects.resolve" => self.resolve(arguments),
            "effects.record" => {
                self.events.push(
                    json!({"code": arguments["code"], "action": arguments["action"],
                    "round": arguments["round"], "dispatched": false}),
                );
                if self.events.len() > 8 {
                    self.events.remove(0);
                }
                Ok(Value::Null)
            }
            "effects.audit" => Ok(self.audit()),
            "effects.fallback" => Ok(json!(
                self.fallback(arguments["message"].as_str().unwrap_or_default())
            )),
            "effects.state" => Ok(json!({
                "unknown": receipt_entries(&self.unknown),
                "effectCount": self.observed_receipt_count, "audit": self.audit()
            })),
            _ => Err("AGENT_CORE_OPERATION_UNKNOWN".into()),
        }
    }

    fn invalidate_file_state(&mut self, target: Option<&str>) {
        // A later mutation invalidates state receipts, not historical one-time dispatch facts.
        // 后续变更使旧文件状态回执失效；发送、提交或启动等一次性派发记录仍保留。
        // Opaque effects may touch files, so they form a barrier for file-state reuse only.
        // 无法解析目标的副作用可能修改文件，只撤销文件状态回执的复用资格。
        let invalidate = |items: &mut Vec<(String, EffectReceipt)>| {
            for (_, receipt) in items {
                if let Some(previous_target) = receipt.mutable_target.as_deref() {
                    if target.is_none_or(|target| target == previous_target) {
                        receipt.reusable = false;
                    }
                }
            }
        };
        invalidate(&mut self.completed);
        if let Some(protected) = &mut self.protected_effects {
            invalidate(protected);
        }
    }

    fn resolve(&mut self, arguments: &Value) -> Result<Value, String> {
        let proofs = arguments
            .get("proofs")
            .and_then(Value::as_array)
            .ok_or("INVALID_AGENT_CORE_ARGUMENTS")?;
        let mut resolved = 0;
        for proof in proofs {
            let Some(index) = self
                .unknown
                .iter()
                .position(|(_, receipt)| receipt.call["id"] == proof["toolCallId"])
            else {
                continue;
            };
            let Some(result) = proof.get("result").filter(|value| value.is_object()) else {
                continue;
            };
            if proof["outcome"] != "dispatch-confirmed"
                || is_truthy(&result["isError"])
                || result["status"] != "completed"
            {
                continue;
            }
            let (key, original) = self.unknown.remove(index);
            let verified = EffectReceipt {
                call: original.call,
                result: result.clone(),
                mutable_target: original.mutable_target,
                reusable: original.reusable,
            };
            set_receipt(&mut self.completed, key.clone(), verified.clone());
            let protected = self
                .protected_effects
                .get_or_insert_with(|| self.completed.clone());
            set_receipt(protected, key, verified);
            let mut record = json!({"toolCallId": proof["toolCallId"],
                "observationToolCallId": proof["observationToolCallId"], "outcome": proof["outcome"]});
            if let Some(reference) = result.get("resultRef") {
                record["resultRef"] = reference.clone();
            }
            self.verified_effects.push(record);
            resolved += 1;
        }
        Ok(json!(resolved))
    }

    fn audit(&self) -> Value {
        json!({"version": 1, "events": self.events, "unknownEffects": self.unknown.len(),
            "verifiedEffects": self.verified_effects})
    }

    fn fallback(&self, message: &str) -> String {
        let mut completed = 0;
        let mut unknown = 0;
        let mut error = 0;
        for receipt in &self.receipts {
            let verified = self
                .verified_effects
                .iter()
                .any(|proof| proof["toolCallId"] == receipt["id"]);
            if receipt["status"] == "completed" || verified {
                completed += 1;
            } else if receipt["status"] == "unknown" {
                unknown += 1;
            } else {
                error += 1;
            }
        }
        let chinese = regex::Regex::new(r"\p{Script=Han}")
            .expect("constant Han pattern")
            .is_match(message);
        if chinese {
            format!(
                "已有执行记录已保留：已完成 {completed} 项，结果尚未确认 {unknown} 项，未成功 {error} 项。后续工具调用未能可靠恢复，任务尚未全部完成；没有盲目重放既有派发或结果未知的操作。你可以继续对话。"
            )
        } else {
            format!(
                "Saved execution records: {completed} completed, {unknown} unconfirmed, {error} unsuccessful. Further tool calls could not be recovered reliably, so the task remains incomplete. Previous dispatches and unconfirmed operations were not blindly replayed. You can continue the conversation."
            )
        }
    }
}

fn required_object<'a>(arguments: &'a Value, name: &str) -> Result<&'a Value, String> {
    arguments
        .get(name)
        .filter(|value| value.is_object())
        .ok_or_else(|| "INVALID_AGENT_CORE_ARGUMENTS".into())
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64().unwrap_or(0.0) != 0.0,
        _ => true,
    }
}

fn effect_key(call: &Value) -> Result<String, String> {
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .ok_or("INVALID_AGENT_CORE_ARGUMENTS")?;
    let mut business = call
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or("INVALID_AGENT_CORE_ARGUMENTS")?
        .clone();
    // Only app-owned root justification fields are excluded; nested MCP arguments are business data.
    // 仅排除应用自有的顶层理由字段；MCP 嵌套 arguments 内同名字段仍属于业务输入。
    business.remove("reason");
    business.remove("policy");
    let bytes =
        serde_json::to_vec(&json!([name, business])).map_err(|_| "INVALID_AGENT_CORE_ARGUMENTS")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn mutable_target(call: &Value, workspace_root: Option<&str>) -> Option<String> {
    if !matches!(
        call["name"].as_str(),
        Some("filesystem.write" | "filesystem.edit" | "filesystem.delete" | "filesystem.mkdir" | "document.create")
    ) {
        return None;
    }
    let path = call["arguments"]["path"].as_str()?;
    let target = Path::new(path);
    // The trusted request root resolves aliases; this identity never grants file access.
    // 可信请求目录用于归一化相对路径；目标标识不授予或替代文件访问权限。
    let resolved = if target.is_relative() {
        workspace_root.map(|root| Path::new(root).join(target))
    } else {
        None
    };
    let normalized = absolute_path(&resolved.map_or_else(|| path.to_owned(), |path| {
        path.to_string_lossy().into_owned()
    }));
    // Approved targets can have Windows short-name aliases; a failed resolution proves no identity.
    // 已批准目标可能使用 Windows 短名别名；解析失败不能被当作已确认的同一目标。
    canonical(&normalized, true)
        .ok()
        .map(|target| path_key(&target))
}

fn set_receipt(items: &mut Vec<(String, EffectReceipt)>, key: String, receipt: EffectReceipt) {
    if let Some((_, previous)) = items
        .iter_mut()
        .find(|(previous_key, _)| previous_key == &key)
    {
        *previous = receipt;
    } else {
        items.push((key, receipt));
    }
}

fn receipt_value(receipt: &EffectReceipt) -> Value {
    json!({"call": receipt.call, "result": receipt.result})
}

fn receipt_entries(items: &[(String, EffectReceipt)]) -> Vec<Value> {
    items
        .iter()
        .map(|(key, receipt)| json!({"key": key, "call": receipt.call, "result": receipt.result}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, command: &str) -> Value {
        json!({"id": id, "name": "terminal.host.run", "arguments": {"command": command}})
    }

    #[test]
    fn diagnostic_capacity_does_not_drop_recovery_protection_or_unknown_outcomes() {
        let mut ledger = EffectLedger::default();
        for index in 0..MAX_EFFECT_RECEIPTS + 5 {
            ledger.apply("effects.observe", &json!({"call":call(&index.to_string(), &format!("command-{index}")),
                "result":{"status":"completed"}})).unwrap();
        }
        ledger.apply("effects.protect", &json!({"includeCurrent":true})).unwrap();
        let first = ledger.apply("effects.previous", &json!({"call":call("retry", "command-0")})).unwrap();
        assert_eq!(first["result"]["status"], "completed");
        assert_eq!(ledger.apply("effects.state", &json!({})).unwrap()["effectCount"], MAX_EFFECT_RECEIPTS + 5);
        ledger.apply("effects.observe", &json!({"call":call("unknown", "uncertain"),"result":{"status":"unknown"}})).unwrap();
        assert_eq!(ledger.apply("effects.state", &json!({})).unwrap()["unknown"].as_array().unwrap().len(), 1);
        assert_eq!(ledger.apply("effects.previous", &json!({"call":call("retry", "command-0")})).unwrap()["result"]["status"], "completed");
    }

    #[test]
    fn protection_freezes_at_the_recovery_boundary() {
        let mut ledger = EffectLedger::default();
        let first = call("first", "first");
        let later = call("later", "later");
        ledger
            .apply(
                "effects.observe",
                &json!({"call": first, "result": {"status": "completed"}}),
            )
            .unwrap();
        assert!(
            ledger
                .apply("effects.previous", &json!({"call": first}))
                .unwrap()
                .is_null()
        );
        ledger.apply("effects.protect", &json!({})).unwrap();
        ledger
            .apply(
                "effects.observe",
                &json!({"call": later, "result": {"status": "completed"}}),
            )
            .unwrap();
        ledger.apply("effects.protect", &json!({})).unwrap();
        assert_eq!(
            ledger
                .apply("effects.previous", &json!({"call": first}))
                .unwrap()["call"]["id"],
            "first"
        );
        assert!(
            ledger
                .apply("effects.previous", &json!({"call": later}))
                .unwrap()
                .is_null()
        );
        ledger
            .apply("effects.protect", &json!({"includeCurrent": true}))
            .unwrap();
        assert_eq!(
            ledger
                .apply("effects.previous", &json!({"call": later}))
                .unwrap()["call"]["id"],
            "later"
        );
    }

    #[test]
    fn unknown_receipts_prevent_replay_without_blocking_other_work_or_claiming_success() {
        let mut ledger = EffectLedger::default();
        let original = call("original", "fixture");
        ledger
            .apply("effects.observe", &json!({"call": original, "result": {"status": "unknown"}}))
            .unwrap();
        ledger.apply("effects.protect", &json!({"includeCurrent": true})).unwrap();
        let state = ledger.apply("effects.state", &json!({})).unwrap();
        assert_eq!(state["unknown"].as_array().unwrap().len(), 1);
        assert!(state.get("hasUnknownEffects").is_none());
        let mut repeated = call("retry", "fixture");
        repeated["arguments"]["reason"] = json!("Retry the same operation");
        let previous = ledger.apply("effects.previous", &json!({"call": repeated})).unwrap();
        assert_eq!(previous["call"]["id"], "original");
        assert_eq!(previous["result"]["status"], "unknown");
        assert_eq!(previous["result"]["reused"], true);
        assert!(ledger.completed.is_empty());
        assert!(ledger.apply("effects.previous", &json!({"call":call("other", "unrelated")})).unwrap().is_null());
        assert!(ledger.apply("effects.previous", &json!({"call":original,"observation":true})).unwrap().is_null());
        let fallback = ledger.apply("effects.fallback", &json!({"message": "继续处理"})).unwrap();
        assert!(fallback.as_str().unwrap().contains("结果尚未确认 1 项"));
    }

    #[test]
    fn broker_proof_alone_resolves_unknown_dispatch() {
        let mut ledger = EffectLedger::default();
        let original = call("original", "fixture");
        ledger
            .apply(
                "effects.observe",
                &json!({"call": original, "result": {"status": "unknown"}}),
            )
            .unwrap();
        assert_eq!(
            ledger
                .apply(
                    "effects.resolve",
                    &json!({"proofs": [{"toolCallId": "original", "outcome": "model-confirmed",
            "result": {"status": "completed"}}]})
                )
                .unwrap(),
            0
        );
        assert_eq!(
            ledger.apply("effects.audit", &json!({})).unwrap()["unknownEffects"],
            1
        );
        assert_eq!(ledger.apply("effects.resolve", &json!({"proofs": [{"toolCallId": "original", "outcome": "dispatch-confirmed",
            "observationToolCallId": "read", "result": {"status": "completed", "resultRef": {"id": "proof"}}}]})).unwrap(), 1);
        assert_eq!(
            ledger.apply("effects.audit", &json!({})).unwrap()["unknownEffects"],
            0
        );
        assert_eq!(
            ledger
                .apply("effects.previous", &json!({"call": original}))
                .unwrap()["result"]["resultRef"]["id"],
            "proof"
        );
        let fallback = ledger
            .apply("effects.fallback", &json!({"message": "继续处理"}))
            .unwrap();
        assert!(
            fallback
                .as_str()
                .unwrap()
                .contains("已完成 1 项，结果尚未确认 0 项")
        );
    }

    #[test]
    fn explanations_do_not_change_effect_identity_but_nested_business_fields_do() {
        let mut ledger = EffectLedger::default();
        let original = json!({"id": "one", "name": "mcp.fixture.write", "arguments": {
            "reason": "first", "policy": {"reason": "approval"}, "arguments": {"reason": "business", "value": 1}}});
        ledger
            .apply(
                "effects.observe",
                &json!({"call": original, "result": {"status": "completed"}}),
            )
            .unwrap();
        ledger.apply("effects.protect", &json!({})).unwrap();
        let mut repaired = original.clone();
        repaired["arguments"]["reason"] = json!("repair");
        repaired["arguments"]["policy"] = json!({"reason": "changed"});
        assert!(
            !ledger
                .apply("effects.previous", &json!({"call": repaired}))
                .unwrap()
                .is_null()
        );
        repaired["arguments"]["arguments"]["reason"] = json!("different business");
        assert!(
            ledger
                .apply("effects.previous", &json!({"call": repaired}))
                .unwrap()
                .is_null()
        );
    }
}
