use serde_json::{Value, json};

const DEFAULT_OUTPUT_TOKENS: f64 = 262_144.0;

/// One model contract owns output caps and input reservation; orchestration only delegates.
/// 同一模型合同拥有输出硬限及输入预留；编排只调用该服务，不复制预算规则。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "context.outputDefaults" => Ok(json!({"defaultOutputTokens": DEFAULT_OUTPUT_TOKENS as u64,
            "minimumOutputTokens": 1024, "maximumOutputTokens": 262144})),
        "context.outputValidate" => Ok(json!(output_tokens(args.get("value"))? as u64)),
        "context.outputBudget" => output_budget(args),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}

fn integer(value: Option<&Value>, minimum: f64, maximum: f64, code: &str) -> Result<f64, String> {
    value
        .and_then(Value::as_f64)
        .filter(|value| {
            value.is_finite() && value.fract() == 0.0 && *value >= minimum && *value <= maximum
        })
        .ok_or(code.to_owned())
}

fn output_tokens(value: Option<&Value>) -> Result<f64, String> {
    value.map_or(Ok(DEFAULT_OUTPUT_TOKENS), |value| {
        integer(
            Some(value),
            1024.0,
            DEFAULT_OUTPUT_TOKENS,
            "INVALID_OUTPUT_BUDGET",
        )
    })
}

fn output_budget(args: &Value) -> Result<Value, String> {
    let window = integer(
        args.get("contextWindowTokens"),
        2048.0,
        2_000_000.0,
        "INVALID_CONTEXT_WINDOW",
    )?;
    let required = match args.get("requiredInputTokens") {
        None => 0.0,
        value => integer(value, 0.0, f64::MAX, "INVALID_OUTPUT_BUDGET")?,
    };
    let provider_output = match args.get("providerMaxOutputTokens") {
        None => f64::INFINITY,
        value => integer(value, 256.0, f64::MAX, "INVALID_OUTPUT_BUDGET")?,
    };
    let provider_input = match args.get("providerMaxInputTokens") {
        None => f64::INFINITY,
        value => integer(value, 256.0, f64::MAX, "INVALID_OUTPUT_BUDGET")?,
    };
    let requested = output_tokens(args.get("requestedOutputTokens"))?;
    let safety = (window * 0.1).ceil().clamp(256.0, 8192.0);
    let usable = window - safety;
    let provider_input_budget = provider_input - safety;
    if required > provider_input_budget {
        return Err("CONTEXT_INPUT_TOO_LARGE".to_owned());
    }
    // Output reservation and provider input/output caps are separate contracts.
    // 输出预留与供应商输入、输出硬上限是不同合同，均必须落实到最终额度。
    // Tool turns reserve room for the next schema and receipts; prose turns retain their normal output share.
    // 工具阶段为下一步定义及回执留出空间；正文阶段仍使用原有输出比例，不增加固定字数上限。
    let share = if args["phase"] == "tool" {
        0.2
    } else if window <= 32768.0 {
        0.5
    } else {
        0.3
    };
    let context_output = (usable * share).floor().min(usable - required);
    let output = requested.min(provider_output).min(context_output);
    if output < 256.0 {
        return Err("CONTEXT_INPUT_TOO_LARGE".to_owned());
    }
    let reduced = output < requested;
    let reason = if !reduced {
        Value::Null
    } else {
        json!(
            if provider_output < requested && provider_output <= context_output {
                "provider_limit"
            } else {
                "context_window"
            }
        )
    };
    let mut result = json!({"requestedOutputTokens": requested as u64, "maxOutputTokens": output as u64,
        "safetyMarginTokens": safety as u64, "inputBudgetTokens": usable.min(usable - output).min(provider_input_budget) as i64,
        "outputBudgetReduced": reduced, "outputBudgetReductionReason": reason});
    if let Some(value) = args.get("providerMaxOutputTokens") {
        result["providerMaxOutputTokens"] = value.clone();
    }
    if let Some(value) = args.get("providerMaxInputTokens") {
        result["providerMaxInputTokens"] = value.clone();
    }
    Ok(result)
}
