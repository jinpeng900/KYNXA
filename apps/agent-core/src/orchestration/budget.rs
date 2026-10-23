use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

const LIMITS: [(&str, u64, u64, u64); 4] = [
    ("maxRounds", 64, 1, 128),
    ("maxToolCalls", 256, 1, 512),
    ("maxGeneratedTokens", 1_048_576, 1024, 4_194_304),
    ("maxDurationMs", 1_800_000, 1000, MAX_DURATION_MS),
];

/// Transport overrides bound one ordinary segment; an explicit useful work budget may extend it.
/// 传输覆盖值约束普通执行段；用户明确要求的有意义工作预算可以超过该段上限。
const MAX_DURATION_MS: u64 = 21_600_000;
/// Keep duration and the four-worker tree budget exactly representable in shared JSON contracts.
/// 保证时长与四执行器树预算都能在共享 JSON 合同中精确表示。
const MAX_EXPLICIT_DURATION_MS: u64 = 9_007_199_254_740_991 / 4;
/// Finishing margin added after a user-required minimum, so verification and delivery still fit.
/// 用户要求的最低时长之后预留的收尾余量，保证核验与交付仍在额度内。
const MINIMUM_ACTIVE_MARGIN_MS: u64 = 1_800_000;

/// Leave room for evidence IDs and reasoning; repair attempts may grow within negotiated output.
/// 为证据 ID 和思考留足输出；验收重试可增额，始终遵守实际协商上限。
pub(super) fn review_output_limit(requirements: usize, attempts: u64, available: u64) -> u64 {
    let requirements = u64::try_from(requirements).unwrap_or(u64::MAX);
    2048_u64.saturating_add(requirements.saturating_mul(128))
        .saturating_add(attempts.saturating_mul(1024)).min(available)
}

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

/// Ceilings are policy limits, not usage; reserve mandatory input and safety margins.
/// 分配上限不是实际用量；仍须预留必要输入和安全余量。
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

/// Preserve audit fields, including effective schema limits and truncation details.
/// 保留审计字段，包括生效的工具定义额度和截断信息。
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
        .any(|key| !LIMITS.iter().any(|(name, _, _, _)| name == key))
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
    Ok(result)
}

/// Normalize limits before acceptance; invalid values cannot change the goal or stop accepted work.
/// 接受前归一化额度；异常值不能改变目标或中断已接受任务。
pub(crate) fn resolve_request_limits(input: &Value) -> Value {
    let object = input.as_object();
    let mut resolved = json!({});
    let mut corrections = Vec::new();
    if !input.is_null() && object.is_none() {
        corrections.push(json!({"field":"runLimits","reason":"invalid-object-using-defaults"}));
    }
    for (name, default, minimum, maximum) in LIMITS {
        let requested = object.and_then(|object| object.get(name));
        let number = requested.and_then(Value::as_u64);
        // Clamp oversized requests to the supported maximum, not the default.
        // 超限请求取支持的最大值，不退回默认值。
        let valid = number.map(|value| value.min(maximum)).filter(|value| *value >= minimum);
        if requested.is_some_and(|value| !value.is_null()) && valid.is_none() {
            corrections.push(json!({"field":name,"reason":"invalid-limit-using-default","effective":default}));
        } else if number.is_some_and(|value| value > maximum) {
            corrections.push(json!({"field":name,"reason":"limit-clamped-to-maximum","effective":maximum}));
        }
        resolved[name] = json!(valid.unwrap_or(default));
    }
    for name in object.into_iter().flat_map(|object| object.keys()) {
        if !LIMITS.iter().any(|(known, _, _, _)| known == name) {
            corrections.push(json!({"field":name,"reason":"unsupported-limit-ignored"}));
        }
    }
    json!({"limits":resolved,"corrections":corrections})
}

/// Combine transport limits with the user's explicit minimum work duration.
/// 合并传输额度与用户明确要求的最低工作时长。
pub(crate) fn resolve_input_limits(input: &Value) -> Value {
    let mut resolved = resolve_request_limits(input.get("runLimits").unwrap_or(&Value::Null));
    let message = input["message"].as_str().unwrap_or("");
    if let Some(minimum) = requested_minimum_ms(message) {
        if is_finite_response_request(message) {
            // Do not idle or repeat a finite answer to satisfy an artificial duration.
            // 有限回复交付后即完成，不为空凑时长等待或重复。
            resolved["corrections"].as_array_mut().expect("correction array").push(json!({
                "field":"minActiveMs","reason":"finite-response-does-not-require-time-filling",
                "requested":minimum
            }));
        } else {
            apply_minimum_active(&mut resolved, minimum);
        }
    }
    resolved
}

/// Reserve the minimum effective duration plus a finishing margin.
/// 为最低有效工作时长和收尾预留足够额度。
pub(crate) fn apply_minimum_active(resolved: &mut Value, minimum: u64) {
    let minimum = minimum.min(MAX_EXPLICIT_DURATION_MS - MINIMUM_ACTIVE_MARGIN_MS);
    let current = resolved["limits"]["maxDurationMs"].as_u64().unwrap_or(1_800_000);
    let needed = minimum.saturating_add(MINIMUM_ACTIVE_MARGIN_MS);
    resolved["limits"]["minActiveMs"] = json!(minimum);
    if needed > current {
        resolved["limits"]["maxDurationMs"] = json!(needed);
    }
}

/// User steering may replace or revoke a minimum; omitted changes retain the accepted value.
/// 用户引导可替换或撤销最低时长；未提及时保留原值，不重置累计工作时间。
pub(super) fn revised_minimum_active(current: u64, requested: Option<&Value>, stopped: bool) -> Result<u64, String> {
    let requested = requested.map(|value| value.as_u64()
        .filter(|minimum| *minimum <= MAX_EXPLICIT_DURATION_MS - MINIMUM_ACTIVE_MARGIN_MS)
        .ok_or_else(|| "INVALID_STEERING_MINIMUM_ACTIVE".to_owned())).transpose()?;
    if stopped {
        if requested.is_some_and(|minimum| minimum > 0) { return Err("INVALID_STEERING_MINIMUM_ACTIVE".into()); }
        Ok(0)
    } else {
        Ok(requested.unwrap_or(current))
    }
}

/// Scale tree-wide model requests at one per 10 seconds, with a minimum of 256.
/// 任务树按每 10 秒一次估算模型额度，最少 256 次。
pub(crate) fn model_request_allowance(max_duration_ms: u64) -> u64 {
    (max_duration_ms / 10_000).max(256)
}

/// An explicit "at least N minutes/hours" requirement; plain mentions of time are ignored.
/// 只识别“至少／不少于 N 分钟／小时”等明确的最低时长要求，普通时间描述不计入。
fn minimum_duration_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    // Allow short verbs before Arabic or Chinese duration values, e.g. 至少工作一个半小时.
    // 限定词与时长之间允许短动词，支持阿拉伯及中文数字，如“至少工作一个半小时”。
    PATTERN.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)(?:至少|最少|不少于|不低于|起码|at\s+least|no\s+less\s+than|a\s+minimum\s+of)",
            r"[^0-9零〇一二两三四五六七八九十百半\n。；;]{0,10}?",
            r"([0-9]+(?:\.[0-9]+)?|[零〇一二两三四五六七八九十百]+|半)\s*(个半|个)?\s*",
            r"(分钟|小时|钟头|minutes?|mins?|hours?|hrs?)",
            r"(?:\s*(?:又)?\s*([0-9]+|[零〇一二两三四五六七八九十]+)\s*分钟?)?"
        ))
        .expect("static minimum duration expression")
    })
}

fn requested_minimum_ms(message: &str) -> Option<u64> {
    let captures = minimum_duration_pattern().captures(message)?;
    let amount = match &captures[1] {
        "半" => 0.5,
        text => text.parse::<f64>().ok().or_else(|| chinese_number(text))?,
    } + if captures.get(2).is_some_and(|half| half.as_str() == "个半") { 0.5 } else { 0.0 };
    let unit = captures[3].to_lowercase();
    let hours = unit.starts_with('小') || unit.starts_with('钟') || unit.starts_with('h');
    let extra_minutes = captures
        .get(4)
        .filter(|_| hours)
        .and_then(|minutes| minutes.as_str().parse::<f64>().ok().or_else(|| chinese_number(minutes.as_str())))
        .unwrap_or(0.0);
    let minutes = if hours { amount * 60.0 + extra_minutes } else { amount };
    (minutes.is_finite() && minutes >= 1.0).then(|| (minutes * 60_000.0) as u64)
}

/// Exclude only finite greetings or literal repetition, not substantial or unknown tasks.
/// 仅排除有限问候或字面重复，不限制实质性或尚未明确的任务。
pub(crate) fn is_finite_response_request(message: &str) -> bool {
    static FINITE: OnceLock<Regex> = OnceLock::new();
    static QUALIFIER: OnceLock<Regex> = OnceLock::new();
    let qualifier = QUALIFIER.get_or_init(|| Regex::new(
        r"(?i)^(?:至少|最少|不少于|不低于|起码|at\s+least|no\s+less\s+than|a\s+minimum\s+of)"
    ).expect("static duration qualifier expression"));
    let remainder = minimum_duration_pattern().replace_all(message, |captures: &regex::Captures<'_>| {
        let whole = captures.get(0).expect("duration match");
        let amount = captures.get(1).expect("duration amount");
        qualifier.replace(&message[whole.start()..amount.start()], "").into_owned()
    });
    let remainder = remainder.trim_matches(|character: char| character.is_whitespace()
        || "，,。.!！?？；;：:".contains(character));
    FINITE.get_or_init(|| Regex::new(concat!(
        r#"(?ix)^(?:请|麻烦|帮我|你|让模型|让你|继续|接着|连续|一直|不断|反复|持续|工作|只|please|just|keep|for|and|\s|[，,。.!！?？；;：:])*"#,
        r#"(?:(?:说|回复|回答|输出|打印|重复|say|repeat|respond(?:\s+with)?|print)\s*)?"#,
        r#"(?:你好|您好|hello|hi|hey|[\"“‘'][^\"”’'\r\n]{1,80}[\"”’'])"#,
        r#"(?:\s|吧|就行|即可|一直|不断|反复|持续|次|遍|个|小时|分钟|for|hours?|minutes?|times|[0-9零〇一二两三四五六七八九十百千万半.，,。!！?？；;：:])*\z"#
    )).expect("static finite response expression")).is_match(remainder)
}

/// Parse a small Chinese numeral such as 一, 两, 十五, 九十 or 一百二十.
/// 解析常见的中文小数字，如“一”“两”“十五”“九十”“一百二十”；无法识别时返回 None。
fn chinese_number(text: &str) -> Option<f64> {
    let digit = |character: char| match character {
        '零' | '〇' => Some(0u64),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    };
    let mut total = 0u64;
    let mut current = 0u64;
    let mut seen = false;
    for character in text.chars() {
        seen = true;
        match character {
            '百' => {
                total += current.max(1) * 100;
                current = 0;
            }
            '十' => {
                total += current.max(1) * 10;
                current = 0;
            }
            other => current = digit(other)?,
        }
    }
    seen.then_some((total + current) as f64)
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
    fn single_requirement_review_has_room_beyond_the_old_640_token_ceiling() {
        assert!(review_output_limit(1, 0, 8192) >= 2048);
        assert!(review_output_limit(1, 0, 8192) > 640);
        assert!(review_output_limit(8, 0, 8192) > review_output_limit(1, 0, 8192));
    }

    #[test]
    fn review_repairs_expand_output_without_exceeding_the_negotiated_budget() {
        let first = review_output_limit(1, 0, 8192);
        let retry = review_output_limit(1, 1, 8192);
        assert!(retry > first);
        assert!(review_output_limit(1, 2, 8192) > retry);
        assert_eq!(review_output_limit(1, 100, 8192), 8192);
    }

    #[test]
    fn review_output_respects_small_windows_and_extreme_counters_without_overflow() {
        for available in [0, 1, 640, 2048] {
            assert_eq!(review_output_limit(1, 0, available), available);
            assert_eq!(review_output_limit(usize::MAX, u64::MAX, available), available);
        }
        assert_eq!(review_output_limit(usize::MAX, u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn request_limits_repair_invalid_overrides_without_adding_task_tree_ceilings() {
        let defaults = limits(&json!({})).unwrap();
        for omitted in [Value::Null, json!({}), json!({"maxDurationMs":null})] {
            assert_eq!(resolve_request_limits(&omitted)["limits"], defaults);
        }
        let resolved = resolve_request_limits(&json!({"maxRounds":"wrong","maxToolCalls":12,
            "maxDurationMs":0,"maxGeneratedTokens":1.5,"maxModelRequests":1,"maxKnownTokens":10}));
        let mut expected = defaults.clone();
        expected["maxToolCalls"] = json!(12);
        assert_eq!(resolved["limits"], expected);
        assert_eq!(resolved["corrections"].as_array().unwrap().len(), 5);
        assert!(resolved["limits"].get("maxModelRequests").is_none());
        assert!(resolved["limits"].get("maxKnownTokens").is_none());
        assert_eq!(resolve_request_limits(&json!([1,2]))["limits"], defaults);
        assert!(limits(&json!({"maxRounds":"wrong"})).is_err());
    }

    #[test]
    fn large_durations_clamp_and_explicit_minimums_extend_the_task_budget() {
        let clamped = resolve_request_limits(&json!({"maxDurationMs":28_800_000}));
        assert_eq!(clamped["limits"]["maxDurationMs"], MAX_DURATION_MS);
        assert_eq!(clamped["corrections"][0]["reason"], "limit-clamped-to-maximum");
        let hour = resolve_input_limits(&json!({"message":"继续精修，通过 Blender MCP 至少进行 60 分钟有效建模"}));
        assert_eq!(hour["limits"]["minActiveMs"], 3_600_000);
        assert_eq!(hour["limits"]["maxDurationMs"], 5_400_000);
        let english = resolve_input_limits(&json!({"message":"work for at least 2 hours"}));
        assert_eq!(english["limits"]["minActiveMs"], 7_200_000);
        let explicit = resolve_input_limits(&json!({"message":"至少有效工作 1.5 小时","runLimits":{"maxDurationMs":21_600_000}}));
        assert_eq!(explicit["limits"]["maxDurationMs"], 21_600_000);
        for (message, minutes) in [("至少工作一小时", 60), ("至少半小时", 30), ("至少要再认真做一个半小时", 90),
            ("至少两个小时", 120), ("起码九十分钟", 90), ("不少于 1 小时 30 分钟", 90), ("至少一个钟头", 60),
            ("至少十五分钟", 15), ("至少 45 min", 45)] {
            assert_eq!(resolve_input_limits(&json!({"message":message}))["limits"]["minActiveMs"], minutes * 60_000, "{message}");
        }
        for plain in ["5 分钟前我打开了 Blender", "你好", "渲染 60 帧", "至少三个文件，耗时 60 分钟"] {
            assert!(resolve_input_limits(&json!({"message":plain}))["limits"].get("minActiveMs").is_none(), "{plain}");
        }
        assert_eq!(model_request_allowance(1_800_000), 256);
        assert_eq!(model_request_allowance(5_400_000), 540);
    }

    #[test]
    fn finite_greetings_never_fill_time_but_substantive_long_work_keeps_the_requested_budget() {
        for message in ["你好", "说你好100个小时", "请一直说你好，至少 100 个小时", "至少说你好100小时",
            "say hello for at least 100 hours", "至少工作100小时，只说你好", "重复‘你好’至少一小时", "请至少连续说你好100个小时"] {
            let resolved = resolve_input_limits(&json!({"message":message}));
            assert!(resolved["limits"].get("minActiveMs").is_none(), "{message}: {resolved}");
        }
        for message in ["至少工作100小时，持续监控并记录网站可用性", "至少精修8小时，制作完整教堂并逐项检查",
            "至少工作10小时研究新的算法，保存代码与对照实验", "至少用八小时处理这项没有关键词的新工作"] {
            let resolved = resolve_input_limits(&json!({"message":message}));
            assert!(resolved["limits"]["minActiveMs"].as_u64().unwrap() > MAX_DURATION_MS, "{message}");
            assert!(resolved["limits"]["maxDurationMs"].as_u64().unwrap()
                > resolved["limits"]["minActiveMs"].as_u64().unwrap(), "{message}");
        }
    }

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
