use serde_json::{Value, json};

const MIB: f64 = 1024.0 * 1024.0;
fn number(value: &Value, default: f64) -> f64 {
    value.as_f64().unwrap_or(default)
}
fn shape(profile: &Value) -> Value {
    json!({"hiddenSize": number(&profile["hiddenSize"], number(&profile["dimensions"], 384.0)), "heads": number(&profile["attentionHeads"], 12.0)})
}
fn activations(profile: &Value, sequence: f64, batch: f64) -> f64 {
    let shape = shape(profile);
    (batch
        * (sequence * number(&shape["hiddenSize"], 384.0) * 4.0 * 8.0
            + sequence * sequence * number(&shape["heads"], 12.0) * 4.0))
        .ceil()
}
fn request(args: &Value) -> Value {
    let profile = &args["profile"];
    let backend = args["backend"].as_str().unwrap_or("cpu");
    let sequence = number(&args["sequenceTokens"], 128.0);
    let batch = number(&args["batchSize"], 32.0);
    let files = profile["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let weight_bytes: f64 = files
        .iter()
        .filter(|file| file["path"].as_str().unwrap_or("").ends_with(".onnx"))
        .map(|file| number(&file["bytes"], 0.0))
        .sum();
    let tokenizer_bytes: f64 = files
        .iter()
        .filter(|file| file["path"].as_str().unwrap_or("").contains("tokenizer"))
        .map(|file| number(&file["bytes"], 0.0))
        .sum();
    let threads = args["requestedCpuThreads"]
        .as_f64()
        .filter(|threads| threads.fract() == 0.0)
        .map(|threads| threads.clamp(1.0, 32.0))
        .unwrap_or(
            (number(&args["logicalCores"], 1.0) / 2.0)
                .floor()
                .clamp(1.0, 8.0),
        );
    let tokenizer_memory = (tokenizer_bytes * 4.0 + 64.0 * MIB).ceil();
    let dtype = profile["dtype"].as_str().unwrap_or("fp32");
    let quantized = ["q4", "q8", "int4", "int8"]
        .iter()
        .any(|kind| dtype.contains(kind));
    let runtime_weights = weight_bytes * if quantized { 2.0 } else { 1.4 };
    let scratch = activations(
        profile,
        sequence
            .min(number(&profile["maxInputTokens"], 512.0))
            .max(1.0),
        batch.clamp(1.0, 128.0),
    );
    let resident = runtime_weights + tokenizer_memory + 96.0 * MIB;
    let records = args["records"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let observed_peak = records
        .iter()
        .map(|record| number(&record["bytes"], 0.0))
        .fold(0.0, f64::max);
    json!({"cpuThreads": threads, "memoryBytes": (resident + scratch).max(observed_peak * 1.125 + 32.0 * MIB).ceil(),
        "tokenizerMemoryBytes": tokenizer_memory, "gpuMemoryBytes": (runtime_weights + 128.0 * MIB + activations(profile, sequence.min(number(&profile["maxInputTokens"], 512.0)), batch)).ceil(),
        "memoryEstimate": {"backend": backend, "dtype": dtype, "sequenceTokens": sequence, "batchSize": batch,
            "runtimeWeightBytes": runtime_weights.ceil(), "residentBaselineBytes": resident.ceil(), "activationEstimateBytes": scratch,
            "observedPeakBytes": if observed_peak > 0.0 { json!(observed_peak) } else { Value::Null }, "sampleCount": records.len(),
            "source": if records.is_empty() { "model-shape-startup-estimate" } else { "owned-workload-peak-with-margin" }}})
}
fn batch(args: &Value) -> Value {
    let input = &args["budget"];
    let maximum = number(&args["maxInputTokens"], 512.0);
    let fraction = number(&input["batchMultiplier"], 1.0).clamp(0.125, 4.0);
    let cpu = input.get("device").and_then(Value::as_str).unwrap_or("cpu") == "cpu";
    let memory = number(&input["memoryBytes"], 0.0);
    let gpu_memory = number(&input["gpuMemoryBytes"], 0.0);
    let activations_memory = number(&input["activationMemoryBytes"], 0.0);
    let capacity = if !cpu && gpu_memory > 0.0 {
        (gpu_memory / (16.0 * MIB)).min(memory / (8.0 * MIB))
    } else if activations_memory > 0.0
        && number(&input["hiddenSize"], 0.0) > 0.0
        && number(&input["attentionHeads"], 0.0) > 0.0
    {
        activations_memory
            / activations(
                input,
                number(&input["sequenceTokens"], 128.0)
                    .min(maximum)
                    .max(1.0),
                1.0,
            )
    } else {
        memory / (8.0 * MIB)
    };
    let rows = capacity
        .min(if cpu {
            32.0 * fraction
        } else {
            capacity * fraction
        })
        .floor()
        .clamp(1.0, 128.0);
    let rows = if rows >= 32.0 {
        2.0_f64.powf(rows.log2().floor())
    } else {
        rows
    };
    json!({"batchSize": rows, "batchTokenBudget": maximum.max((rows * if cpu { 384.0 } else { 128.0 }).min(65536.0))})
}

/// Batch and residency estimates are requests, never claims that a lease or GPU device was admitted.
/// 批次与驻留估计只是需求申请，不能声称租约或 GPU 设备已经获得准入。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    Ok(match operation {
        "resources.inferenceRequest" => request(args),
        "resources.inferenceBatch" => batch(args),
        "resources.inferenceShape" => shape(&args["profile"]),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    })
}
