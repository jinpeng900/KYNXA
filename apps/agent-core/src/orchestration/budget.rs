use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

const LIMITS: [(&str, u64, u64, u64); 4] = [
    ("maxRounds", 64, 1, 128),
    ("maxToolCalls", 256, 1, 512),
    ("maxGeneratedTokens", 1_048_576, 1024, 4_194_304),
    ("maxDurationMs", 1_800_000, 1000, 3_600_000),
];

// Optional task-tree ceilings preserve existing defaults when the caller omits them.
// 可选任务树总额度在省略时保留旧默认，不作为每日搜索或整段聊天的固定次数限制。
const OPTIONAL_LIMITS: [(&str, u64, u64); 2] = [
    ("maxModelRequests", 1, 4096),
    ("maxKnownTokens", 1, 9_007_199_254_740_991),
];

/// Budget operations are pure; caller-owned timing and cancellation remain at actual I/O boundaries.
/// 预算操作是纯计算；计时与取消仍由调用方在真实 I/O 边界负责。
pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "budget.allocation" => Ok(context_allocation(args)),
        "budget.allocation-audit" => Ok(context_allocation_audit(args)),
        "budget.limits" => limits(args.get("input").unwrap_or(args)),
        "budget.check" => {
            let requested = args.get("limits").cloned().unwrap_or_else(|| json!({}));
            let limits = limits(&requested)?;
            for (counter, limit, code) in [
                ("rounds", "maxRounds", "TOOL_RUN_ROUND_LIMIT"),
                ("toolCalls", "maxToolCalls", "TOOL_RUN_CALL_LIMIT"),
                (
                    "generatedTokens",
                    "maxGeneratedTokens",
                    "TOOL_RUN_OUTPUT_LIMIT",
                ),
            ] {
                let count = args.get(counter).and_then(Value::as_u64).unwrap_or(0);
                if count > limits[limit].as_u64().expect("validated budget limit") {
                    return Err(code.into());
                }
            }
            Ok(Value::Null)
        }
        "context.capacity" => {
            let budget = number_field(args, "inputBudgetTokens", 0.0);
            let message_tokens = number_field(args, "messageTokens", 0.0);
            let declaration_tokens = number_field(args, "declarationTokens", 0.0);
            let reserve_tokens = number_field(args, "reserveTokens", 512.0);
            let occupied = message_tokens + declaration_tokens;
            Ok(json!({
                "compactionNeeded": occupied > budget * 0.9,
                "evidenceBudgetTokens": (budget - occupied - reserve_tokens).max(0.0).floor(),
            }))
        }
        "output.metrics" => Ok(output_metrics(
            args.get("text").and_then(Value::as_str).unwrap_or(""),
        )),
        "output.overlap" => Ok(json!(remove_overlap(
            args.get("previous").and_then(Value::as_str).unwrap_or(""),
            args.get("text").and_then(Value::as_str).unwrap_or(""),
        ))),
        _ => Err("UNKNOWN_AGENT_CORE_OPERATION".into()),
    }
}

fn projected_number(args: &Value, key: &str) -> f64 {
    let number = &args["numbers"][key];
    match number["kind"].as_str() {
        Some("nan") => f64::NAN,
        Some("positive-infinity") => f64::INFINITY,
        Some("negative-infinity") => f64::NEG_INFINITY,
        _ => number["value"].as_f64().unwrap_or(f64::NAN),
    }
}

fn allocation_minimum(values: &[f64]) -> f64 {
    if values.iter().any(|value| value.is_nan()) {
        f64::NAN
    } else {
        values.iter().copied().fold(f64::INFINITY, f64::min)
    }
}

fn allocation_maximum(values: &[f64]) -> f64 {
    if values.iter().any(|value| value.is_nan()) {
        f64::NAN
    } else {
        values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }
}

/// Allocation ceilings are policies, not measured consumption; mandatory input and safety remain reserved.
/// 分配上限属于策略，不能作为实际消耗；必须继续预留必要输入与安全额度。
fn context_allocation(args: &Value) -> Value {
    let input = allocation_maximum(&[0.0, projected_number(args, "inputBudgetTokens").floor()]);
    let research = args["taskType"] == "research" || args["taskType"] == "complex";
    let file = args["taskType"] == "file";
    let evidence_fraction = if research {
        0.35
    } else if file {
        0.30
    } else {
        0.20
    };
    let schema_fraction = if research {
        0.20
    } else if file {
        0.30
    } else {
        0.40
    };
    let mandatory =
        allocation_maximum(&[0.0, projected_number(args, "mandatoryInputTokens").ceil()]);
    let available = allocation_maximum(&[0.0, input - mandatory - 512.0]);
    let requested = allocation_minimum(&[
        if research {
            32768.0
        } else if file {
            16384.0
        } else {
            8192.0
        },
        allocation_maximum(&[1024.0, (input * evidence_fraction).floor()]),
    ]);
    json!({"version":2,"taskType":args["taskType"],"inputBudgetTokens":input,"evidenceFraction":evidence_fraction,
        "schemaFraction":schema_fraction,"requestedEvidenceTokens":requested,"availableEvidenceTokens":available,
        "approvedEvidenceTokens":allocation_minimum(&[requested,available]),
        "schemaCeilingTokens":allocation_minimum(&[24000.0,(input * schema_fraction).floor()]),"safetyTokens":512})
}

/// Preserve every existing audit field, including the caller's effective schema allowance and clipping details.
/// 保留全部既有审计字段，包括调用方实际生效的工具额度与截断细节。
fn context_allocation_audit(args: &Value) -> Value {
    let allocation = &args["allocation"];
    let options = &args["options"];
    let requested = projected_number(args, "requestedEvidenceTokens");
    let configured = projected_number(args, "configuredEvidenceTokens");
    let final_budget = projected_number(args, "finalEvidenceBudgetTokens");
    let initial_approved = if allocation["approvedEvidenceTokens"].is_null() {
        f64::INFINITY
    } else {
        projected_number(args, "approvedEvidenceTokens")
    };
    let evidence_requested = allocation["evidenceRequested"] != false;
    let approved = if evidence_requested {
        allocation_maximum(&[
            0.0,
            allocation_minimum(&[configured, requested, initial_approved, final_budget]),
        ])
    } else {
        0.0
    };
    let mut reasons = Vec::new();
    if !evidence_requested {
        reasons.push("retrieval-not-requested");
    }
    if evidence_requested && requested < configured {
        reasons.push("task-context-share");
    }
    if evidence_requested
        && allocation_minimum(&[initial_approved, final_budget])
            < allocation_minimum(&[configured, requested])
    {
        reasons.push("remaining-context");
    }
    if evidence_requested && projected_number(args, "actualEvidenceTokens") < approved {
        reasons.push("available-selected-evidence");
    }
    let mut result = allocation.clone();
    result["configuredEvidenceTokens"] = options["configuredEvidenceTokens"].clone();
    result["approvedEvidenceTokens"] = json!(approved);
    result["actualEvidenceTokens"] = options["actualEvidenceTokens"].clone();
    result["actualSchemaTokens"] = options["actualSchemaTokens"].clone();
    result["unusedSchemaTokens"] = json!(allocation_maximum(&[
        0.0,
        projected_number(args, "schemaCeilingTokens")
            - projected_number(args, "actualSchemaTokens")
    ]));
    result["earlyCutReasons"] = json!(reasons);
    let retrieval = &options["retrievalBudgetAudit"];
    let retrieval_present = match retrieval {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    };
    if retrieval_present {
        result["retrieval"] = retrieval.clone();
    }
    result
}

fn limits(input: &Value) -> Result<Value, String> {
    let Some(object) = input.as_object() else {
        return Err("INVALID_TOOL_RUN_LIMITS".into());
    };
    if object
        .keys()
        .any(|key| !LIMITS.iter().any(|(name, _, _, _)| name == key)
            && !OPTIONAL_LIMITS.iter().any(|(name, _, _)| name == key))
    {
        return Err("INVALID_TOOL_RUN_LIMITS".into());
    }
    let mut result = json!({});
    for (name, default, minimum, maximum) in LIMITS {
        let value = match object.get(name) {
            None => default,
            Some(value) => value
                .as_u64()
                .filter(|value| *value >= minimum && *value <= maximum)
                .ok_or_else(|| "INVALID_TOOL_RUN_LIMITS".to_owned())?,
        };
        result[name] = json!(value);
    }
    for (name, minimum, maximum) in OPTIONAL_LIMITS {
        if let Some(value) = object.get(name) {
            let value = value.as_u64()
                .filter(|value| *value >= minimum && *value <= maximum)
                .ok_or_else(|| "INVALID_TOOL_RUN_LIMITS".to_owned())?;
            result[name] = json!(value);
        }
    }
    Ok(result)
}

fn number_field(args: &Value, key: &str, default: f64) -> f64 {
    args.get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

fn output_metrics(text: &str) -> Value {
    static HAN: OnceLock<Regex> = OnceLock::new();
    let han =
        HAN.get_or_init(|| Regex::new(r"\p{Script=Han}").expect("static Han script expression"));
    let mut unicode_characters = 0;
    let mut utf16_characters = 0;
    let mut han_characters = 0;
    let mut line_breaks = 0;
    let mut buffer = [0; 4];
    for character in text.chars() {
        unicode_characters += 1;
        utf16_characters += character.len_utf16();
        han_characters += usize::from(han.is_match(character.encode_utf8(&mut buffer)));
        line_breaks += usize::from(character == '\n');
    }
    json!({ "unicodeCharacters":unicode_characters, "hanCharacters":han_characters,
        "lineBreaks":line_breaks, "utf16Characters":utf16_characters })
}

fn remove_overlap<'a>(previous: &str, text: &'a str) -> &'a str {
    if previous.is_empty() || text.is_empty() {
        return text;
    }
    // Compare exact suffixes at Unicode boundaries, while the maximum remains measured in UTF-16 units.
    // 按完整 Unicode 边界比较精确后缀；最大重叠长度仍遵循现有 UTF-16 单位合同。
    let mut prefix_boundaries = Vec::new();
    let mut utf16_length = 0;
    for (offset, character) in text.char_indices() {
        utf16_length += character.len_utf16();
        if utf16_length > 4096 {
            break;
        }
        if utf16_length >= 64 {
            prefix_boundaries.push(offset + character.len_utf8());
        }
    }
    for end in prefix_boundaries.into_iter().rev() {
        if previous.ends_with(&text[..end]) {
            return &text[end..];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_limits_reject_unknown_fields_and_keep_exact_boundary() {
        assert_eq!(
            apply("budget.limits", &json!({})).unwrap()["maxDurationMs"],
            1_800_000
        );
        assert_eq!(
            apply("budget.limits", &json!({"maxRounds":1})).unwrap()["maxRounds"],
            1
        );
        for input in [
            json!({"maxRounds":0}),
            json!({"maxRounds":1.5}),
            json!({"unused":true}),
            Value::Null,
        ] {
            assert_eq!(
                apply("budget.limits", &input).unwrap_err(),
                "INVALID_TOOL_RUN_LIMITS"
            );
        }
        assert_eq!(
            apply(
                "budget.check",
                &json!({"rounds":64,"toolCalls":256,"generatedTokens":1_048_576})
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            apply("budget.check", &json!({"rounds":65})).unwrap_err(),
            "TOOL_RUN_ROUND_LIMIT"
        );
        assert_eq!(
            apply("budget.check", &json!({"toolCalls":257})).unwrap_err(),
            "TOOL_RUN_CALL_LIMIT"
        );
    }

    #[test]
    fn optional_task_budgets_reject_invalid_numbers_and_preserve_legacy_defaults() {
        let defaults = apply("budget.limits", &json!({})).unwrap();
        assert_eq!(defaults["maxToolCalls"], 256);
        assert!(defaults.get("maxModelRequests").is_none());
        assert!(defaults.get("maxKnownTokens").is_none());
        let limits = apply("budget.limits", &json!({"maxModelRequests":1,"maxKnownTokens":120})).unwrap();
        assert_eq!(limits["maxModelRequests"], 1);
        assert_eq!(limits["maxKnownTokens"], 120);
        for invalid in [
            json!({"maxModelRequests":0}), json!({"maxModelRequests":4097}),
            json!({"maxModelRequests":1.5}), json!({"maxKnownTokens":0}),
            json!({"maxKnownTokens":null}), json!({"maxKnownTokens":"100"}),
            json!({"maxKnownTokens":9_007_199_254_740_992_u64}),
        ] {
            assert_eq!(apply("budget.limits", &invalid).unwrap_err(), "INVALID_TOOL_RUN_LIMITS");
        }
    }

    #[test]
    fn capacity_subtracts_schemas_and_reserve_and_uses_pressure_threshold() {
        let result = apply(
            "context.capacity",
            &json!({"inputBudgetTokens":8192,"messageTokens":7000,"declarationTokens":700}),
        )
        .unwrap();
        assert_eq!(result["compactionNeeded"], true);
        assert_eq!(result["evidenceBudgetTokens"], 0.0);
        let result = apply("context.capacity", &json!({"inputBudgetTokens":1000,"messageTokens":700,"declarationTokens":200,"reserveTokens":50})).unwrap();
        assert_eq!(result["compactionNeeded"], false);
        assert_eq!(result["evidenceBudgetTokens"], 50.0);
    }

    #[test]
    fn output_counts_han_scalars_and_utf16_without_splitting_overlap() {
        let metrics = output_metrics("甲\n😀𠀀");
        assert_eq!(metrics["unicodeCharacters"], 4);
        assert_eq!(metrics["hanCharacters"], 2);
        assert_eq!(metrics["utf16Characters"], 6);
        let repeated = "𠀀".repeat(32);
        assert_eq!(
            remove_overlap(&format!("earlier{repeated}"), &format!("{repeated}next")),
            "next"
        );
        assert_eq!(remove_overlap("short", "short next"), "short next");
    }
}
