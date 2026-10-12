use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{Value, json};

use super::protocol::{array, string};

const COMPUTE_OPERATORS: &[&str] = &[
    "MatMul",
    "Gemm",
    "FusedMatMul",
    "MatMulIntegerToFloat",
    "DynamicQuantizeMatMul",
    "Attention",
    "QAttention",
    "MultiHeadAttention",
    "Conv",
    "DmlFusedGraph",
];

fn memory_pressure(message: &str) -> bool {
    Regex::new("(?i)out of memory|bad_alloc|failed to allocate|not enough memory|insufficient memory|E_OUTOFMEMORY").unwrap().is_match(message)
}
fn compatibility(reference: &Value, actual: &Value) -> bool {
    let (Some(reference), Some(actual)) = (reference.as_array(), actual.as_array()) else {
        return false;
    };
    !reference.is_empty()
        && reference.len() == actual.len()
        && reference.iter().zip(actual).all(|(reference, actual)| {
            reference
                .as_f64()
                .zip(actual.as_f64())
                .is_some_and(|(left, right)| {
                    left.is_finite()
                        && right.is_finite()
                        && (left - right).abs() <= 1e-4 * left.abs().max(1.0)
                })
        })
}
fn drift(reference: &Value, actual: &Value, dimensions: &Value) -> Option<Value> {
    let (Some(reference), Some(actual), Some(dimensions)) = (
        reference.as_array(),
        actual.as_array(),
        dimensions.as_u64().filter(|dimensions| *dimensions > 0),
    ) else {
        return None;
    };
    let dimensions = dimensions as usize;
    if reference.len() != actual.len()
        || reference.len() < dimensions.checked_mul(2)?
        || reference.len() % dimensions != 0
    {
        return None;
    }
    let (mut minimum_cosine, mut maximum_difference) = (1.0_f64, 0.0_f64);
    for (reference, actual) in reference.chunks(dimensions).zip(actual.chunks(dimensions)) {
        let (mut dot, mut reference_norm, mut actual_norm) = (0.0, 0.0, 0.0);
        for (left, right) in reference.iter().zip(actual) {
            let (left, right) = (left.as_f64()?, right.as_f64()?);
            if !left.is_finite() || !right.is_finite() {
                return None;
            }
            dot += left * right;
            reference_norm += left * left;
            actual_norm += right * right;
            maximum_difference = maximum_difference.max((left - right).abs());
        }
        if (reference_norm - 1.0).abs() > 1e-4 || (actual_norm - 1.0).abs() > 1e-4 {
            return None;
        }
        minimum_cosine = minimum_cosine.min(dot / (reference_norm * actual_norm).sqrt());
    }
    Some(json!({"minCosine": minimum_cosine, "maxDifference": maximum_difference}))
}

/// Backend qualification is based on actual numerical probes and operator traces, never provider names.
/// 后端资格依据真实数值探针与算子追踪判断；提供程序名称或成功装载不能冒充 GPU 运算证据。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "models.inference.sessionOptions" => {
            let device = args["device"].as_str().unwrap_or("cpu");
            let mut options = json!({"executionProviders": [if device == "cpu" { json!("cpu") } else { json!({"name": device, "deviceId": args.get("deviceId").unwrap_or(&json!(0))}) }],
                "intraOpNumThreads": args["cpuThreads"], "interOpNumThreads": 1, "executionMode": "sequential", "logSeverityLevel": 3});
            if device != "cpu" {
                options["extra"] = json!({"session": {"disable_cpu_ep_fallback": "1"}});
                if device == "dml" {
                    options["enableMemPattern"] = json!(false);
                }
            }
            Ok(options)
        }
        "models.inference.memoryPressure" => Ok(json!(memory_pressure(string(&args["message"])))),
        "models.inference.gpuFailureReason" => {
            let message = string(&args["message"]);
            let reason = if memory_pressure(message) {
                "GPU_MEMORY_INSUFFICIENT"
            } else if Regex::new("(?i)cpu.*fallback|fallback.*cpu|disable_cpu_ep_fallback")
                .unwrap()
                .is_match(message)
            {
                "CPU_OPERATOR_FALLBACK_REQUIRED"
            } else if Regex::new(
                "(?i)not implemented|unsupported.*operator|operator.*not supported|unimplemented",
            )
            .unwrap()
            .is_match(message)
            {
                "MODEL_OPERATOR_UNSUPPORTED"
            } else if Regex::new("(?i)driver|LoadLibrary|device removed|device lost|DXGI_ERROR")
                .unwrap()
                .is_match(message)
            {
                "GPU_DRIVER_UNAVAILABLE"
            } else {
                "GPU_BACKEND_INITIALIZATION_FAILED"
            };
            Ok(json!(reason))
        }
        "models.inference.compatibleOutputs" => {
            Ok(json!(compatibility(&args["reference"], &args["actual"])))
        }
        "models.inference.normalizedDrift" => {
            Ok(
                drift(&args["reference"], &args["actual"], &args["dimensions"])
                    .unwrap_or(Value::Null),
            )
        }
        "models.inference.qualifiedOutputs" => {
            let Some(mut drift) = drift(&args["reference"], &args["actual"], &args["dimensions"])
            else {
                return Ok(Value::Null);
            };
            if drift["minCosine"].as_f64().unwrap() < 0.995
                || drift["maxDifference"].as_f64().unwrap() > 0.03
            {
                return Ok(Value::Null);
            }
            drift["embeddingSpaceQualified"] = json!(true);
            Ok(drift)
        }
        "models.inference.auditPartition" => {
            let Some(events) = args["events"]
                .as_array()
                .filter(|events| events.len() <= 100_000)
            else {
                return Err("INFERENCE_GPU_AUDIT_FAILED".into());
            };
            let provider = if args["device"].as_str().unwrap_or("dml") == "dml" {
                "DmlExecutionProvider"
            } else {
                "CUDAExecutionProvider"
            };
            let (mut gpu_count, mut cpu_count, mut gpu_compute, mut cpu_compute) =
                (0_u64, 0_u64, 0_u64, 0_u64);
            let (mut gpu_duration, mut cpu_duration) = (0.0, 0.0);
            let (mut gpu_operators, mut cpu_operators) = (
                BTreeMap::<String, u64>::new(),
                BTreeMap::<String, u64>::new(),
            );
            let op_name = Regex::new("^[A-Za-z0-9_.]+$").unwrap();
            for event in events {
                let Some(duration) = event["dur"]
                    .as_f64()
                    .filter(|duration| duration.is_finite() && *duration >= 0.0)
                else {
                    continue;
                };
                if event["cat"] != "Node" || !event["args"]["provider"].is_string() {
                    continue;
                }
                let op = event["args"]["op_name"].as_str().unwrap_or("unknown");
                if op.encode_utf16().count() > 64 || !op_name.is_match(op) {
                    continue;
                }
                if event["args"]["provider"] == provider {
                    gpu_count += 1;
                    gpu_duration += duration;
                    if COMPUTE_OPERATORS.contains(&op) {
                        gpu_compute += 1;
                    }
                    *gpu_operators.entry(op.into()).or_default() += 1;
                } else if event["args"]["provider"] == "CPUExecutionProvider" {
                    cpu_count += 1;
                    cpu_duration += duration;
                    if COMPUTE_OPERATORS.contains(&op) {
                        cpu_compute += 1;
                    }
                    *cpu_operators.entry(op.into()).or_default() += 1;
                }
            }
            Ok(
                json!({"version": "ort-node-partition-v1", "gpuProvider": provider, "gpuKernelCount": gpu_count, "cpuKernelCount": cpu_count,
                "gpuComputeKernelCount": gpu_compute, "cpuComputeKernelCount": cpu_compute, "gpuDurationUs": gpu_duration, "cpuDurationUs": cpu_duration,
                "gpuOperators": gpu_operators, "cpuOperators": cpu_operators, "usefulGpuCompute": gpu_compute > 0 && gpu_compute > cpu_compute && gpu_duration > 0.0}),
            )
        }
        "models.inference.batchProbePlan" => {
            let lengths = array(&args["tokenLengths"]);
            let count = args["count"].as_u64().unwrap_or(0) as usize;
            if count == 0 || lengths.len() != count {
                return Err("INFERENCE_INVALID_BATCH".into());
            }
            let requested = args["batchSize"]
                .as_u64()
                .unwrap_or(1)
                .min(128)
                .max(count as u64) as usize;
            let shortest = lengths
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| left.as_u64().cmp(&right.as_u64()))
                .map(|(index, _)| index)
                .unwrap_or(0);
            let indexes: Vec<usize> = (0..requested)
                .map(|index| index % count)
                .chain(std::iter::repeat_n(shortest, requested))
                .collect();
            Ok(json!(indexes))
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
