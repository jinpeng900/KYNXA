use serde_json::{Value, json};

const MIB: f64 = 1024.0 * 1024.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const MAX_TRANSPORT_MEMORY_BYTES: f64 = 64.0 * MIB;
const TRANSPORT_SYSTEM_HEADROOM_BYTES: f64 = 64.0 * MIB;

fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}
fn finite(value: &Value) -> bool {
    value.as_f64().is_some_and(f64::is_finite)
}
fn integer(value: &Value, minimum: f64, maximum: f64) -> bool {
    value.as_f64().is_some_and(|number| {
        number.is_finite() && number.fract() == 0.0 && number >= minimum && number <= maximum
    })
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn cpu_capacity(hardware: &Value) -> f64 {
    (number(&hardware["cpu"]["logicalCores"]) / 2.0)
        .floor()
        .clamp(1.0, 8.0)
}
fn memory_capacity(hardware: &Value, workload: &str) -> f64 {
    if workload == "model-transport" {
        return (number(&hardware["memory"]["availableBytes"]) - TRANSPORT_SYSTEM_HEADROOM_BYTES)
            .min(MAX_TRANSPORT_MEMORY_BYTES).floor().max(0.0);
    }
    (number(&hardware["memory"]["availableBytes"]) * 0.6 - 512.0 * MIB)
        .min(number(&hardware["memory"]["totalBytes"]) * 0.4)
        .floor()
        .max(0.0)
}
fn valid_transport_request(request: &Value) -> bool {
    request["kind"] == "foreground"
        && integer(&request["cpuThreads"], 0.0, 1.0)
        && integer(&request["memoryBytes"], 1.0, 32.0 * MIB)
        && request["gpuMemoryBytes"] == 0
}
fn valid_request(request: &Value) -> bool {
    let utf16 = |value: &Value| value.as_str().map(|value| value.encode_utf16().count());
    utf16(&request["taskId"]).is_some_and(|length| (1..=128).contains(&length))
        && utf16(&request["workspaceId"]).is_some_and(|length| length <= 128)
        && ["foreground", "background"].contains(&string(&request["kind"]))
        && ["compute", "model-transport"].contains(&string(&request["workload"]))
        && (request["workload"] != "model-transport"
            || valid_transport_request(request))
        && integer(&request["cpuThreads"], 0.0, 4096.0)
        && integer(&request["memoryBytes"], 0.0, MAX_SAFE_INTEGER)
        && integer(&request["gpuMemoryBytes"], 0.0, MAX_SAFE_INTEGER)
        && number(&request["cpuThreads"])
            + number(&request["memoryBytes"])
            + number(&request["gpuMemoryBytes"])
            > 0.0
        && integer(&request["ttlMs"], 1000.0, 300000.0)
        && integer(&request["waitMs"], 0.0, 60000.0)
}
fn valid_feedback(feedback: &Value) -> bool {
    if feedback.as_array().is_some_and(Vec::is_empty) {
        return true;
    }
    let Some(fields) = feedback.as_object() else {
        return false;
    };
    for (key, value) in fields {
        let valid = match key.as_str() {
            "phase" => ["cold-load", "queue", "hot-inference", "other"].contains(&string(value)),
            "unit" => {
                ["tokens", "documents", "pairs", "vectors", "operations"].contains(&string(value))
            }
            "backend" => ["cpu", "gpu", "dml", "cuda", "host"].contains(&string(value)),
            "allocationFailure" => value.is_boolean(),
            "throughputPerSecond" | "latencyMs" | "foregroundLatencyMs" => {
                finite(value) && number(value) >= 0.0 && number(value) <= 1e9
            }
            "progress" => finite(value) && (0.0..=1.0).contains(&number(value)),
            "queueDepth" => integer(value, 0.0, 1e6),
            "inputTokens" => integer(value, 1.0, 65536.0),
            "sequenceTokens" => integer(value, 1.0, 512.0),
            "batchSize" => integer(value, 1.0, 128.0),
            "cpuThreads" => integer(value, 1.0, 32.0),
            _ => false,
        };
        if !valid {
            return false;
        }
    }
    true
}
fn feedback_identity(task_id: &str, feedback: &Value) -> Value {
    let backend = feedback.get("backend").map(string).unwrap_or("legacy");
    let phase = feedback.get("phase").map(string).unwrap_or("legacy");
    let sequence = if number(&feedback["sequenceTokens"]) > 0.0 {
        format!(
            "{}",
            2.0_f64.powf(number(&feedback["sequenceTokens"]).log2().ceil())
        )
    } else {
        "legacy".into()
    };
    let history_key = format!(
        "{task_id}|{backend}|{phase}|{}|{sequence}",
        feedback.get("unit").map(string).unwrap_or("legacy")
    );
    let control_key = if backend == "legacy" {
        "legacy".into()
    } else {
        format!(
            "{task_id}|{}",
            if ["gpu", "dml", "cuda"].contains(&backend) {
                "gpu"
            } else {
                backend
            }
        )
    };
    json!({"historyKey": history_key, "controlKey": control_key, "backend": backend, "phase": phase})
}
fn feedback_update(args: &Value) -> Value {
    let feedback = &args["feedback"];
    let mut previous = args["previous"].clone();
    let mut control = args
        .get("control")
        .cloned()
        .unwrap_or_else(|| json!({"fraction": 1, "lastAdjustmentMs": 0}));
    let mut history = args.get("history").cloned().unwrap_or_else(
        || json!({"throughputPerSecond": null, "latencyMsPerToken": null, "latencyMs": null}),
    );
    let now = number(&args["now"]);
    let phase = string(&args["identity"]["phase"]);
    let latency = feedback
        .get("latencyMs")
        .map(|value| number(value) / number(&feedback["inputTokens"]).max(1.0));
    let fraction = number(&control["fraction"]);
    let previous_throughput = history
        .get("throughputPerSecond")
        .filter(|value| !value.is_null())
        .map(number);
    let hot = ["legacy", "hot-inference"].contains(&phase);
    let mut adjustment = None;
    if feedback["allocationFailure"] == true || number(&feedback["foregroundLatencyMs"]) > 250.0 {
        control["fraction"] = json!((fraction / 2.0).min(0.5).max(0.125));
        adjustment = Some(if feedback["allocationFailure"] == true {
            "allocation-pressure"
        } else {
            "foreground-latency"
        });
    } else if feedback.get("throughputPerSecond").is_some()
        && previous_throughput
            .is_some_and(|throughput| number(&feedback["throughputPerSecond"]) > throughput * 1.05)
        && number(&feedback["queueDepth"]) > 0.0
        && latency.is_none_or(|latency| {
            history["latencyMsPerToken"].is_null()
                || latency <= number(&history["latencyMsPerToken"]) * 1.1
        })
        && hot
        && now - number(&control["lastAdjustmentMs"]) >= 5000.0
    {
        control["fraction"] = json!(
            (if fraction < 1.0 {
                fraction + 0.125
            } else {
                fraction * 2.0
            })
            .min(4.0)
        );
        adjustment = Some("measured-hot-throughput-gain");
    } else if phase == "hot-inference"
        && feedback.get("throughputPerSecond").is_some()
        && number(&feedback["queueDepth"]) > 0.0
        && previous_throughput
            .is_some_and(|throughput| number(&feedback["throughputPerSecond"]) < throughput * 0.9)
        && now - number(&control["lastAdjustmentMs"]) >= 5000.0
    {
        control["fraction"] = json!((fraction / 2.0).max(0.125));
        adjustment = Some("measured-hot-throughput-regression");
    }
    if let Some(reason) = adjustment {
        control["lastAdjustmentMs"] = json!(now);
        previous["lastAdjustmentMs"] = json!(now);
        previous["adjustmentReason"] = json!(reason);
    } else {
        previous["adjustmentReason"] = json!(if hot {
            "no-comparable-throughput-gain"
        } else {
            "non-hot-sample-held"
        });
    }
    previous["taskBackendFraction"] = control["fraction"].clone();
    previous["measurementContext"] = args["identity"]["historyKey"].clone();
    for (key, sample) in [
        (
            "throughputPerSecond",
            feedback.get("throughputPerSecond").map(number),
        ),
        ("latencyMsPerToken", latency),
        ("latencyMs", feedback.get("latencyMs").map(number)),
    ] {
        if let Some(sample) = sample {
            history[key] = json!(if history[key].is_null() {
                sample
            } else {
                number(&history[key]) * 0.75 + sample * 0.25
            });
        }
    }
    previous["throughputPerSecond"] = history["throughputPerSecond"].clone();
    previous["latencyMs"] = history["latencyMs"].clone();
    if let Some(depth) = feedback.get("queueDepth") {
        previous["queueDepth"] = depth.clone();
    }
    previous["reports"] = json!(number(&previous["reports"]) + 1.0);
    json!({"control": control, "history": history, "feedback": previous})
}
fn suggestions(args: &Value, threads: f64, memory_bytes: f64, fraction: f64) -> Value {
    let hardware = &args["hardware"];
    let reserved = &args["reserved"];
    let feedback = &args["feedback"];
    let usage = number(&hardware["cpu"]["usagePercent"]);
    let available = number(&hardware["memory"]["availableBytes"]);
    let candidates = ((if usage > 85.0 {
        40.0
    } else {
        40.0 + number(&hardware["cpu"]["logicalCores"]).min(32.0) * 3.0
    }) * fraction)
        .round()
        .clamp(16.0, 136.0);
    let memory = memory_bytes.min((available / 3.0).floor());
    let idle =
        finite(&hardware["cpu"]["usagePercent"]) && usage < 35.0 && available >= 2048.0 * MIB;
    let cpu_headroom =
        (cpu_capacity(hardware) - number(&reserved["cpuThreads"]) - threads).max(0.0);
    let memory_headroom =
        (memory_capacity(hardware, "compute") - number(&reserved["memoryBytes"]) - memory_bytes)
            .max(0.0);
    let rerank =
        if fraction < 1.0 || usage > 85.0 || memory_headroom < 512.0 * MIB || cpu_headroom < 1.0 {
            20
        } else if cpu_headroom >= 3.0 && memory_headroom >= 1024.0 * MIB {
            60
        } else {
            40
        };
    json!({"annShardBytes": (memory / 2.0).floor().min(1024.0 * MIB), "annCacheBytes": memory,
        "annBuildConcurrency": threads.clamp(1.0, 4.0), "candidateLimit": candidates,
        "fusedCandidateLimit": (candidates * 1.2).floor().min(160.0), "evidenceBudgetTokens": (candidates * 128.0).clamp(2048.0, 16384.0),
        "rerankCandidates": rerank, "rerankCandidateLimit": rerank, "batchMultiplier": fraction, "rerankSuggestionRequiresApproval": true,
        "batchProbeMultiplier": if fraction == 1.0 && idle { 2.0 } else { fraction },
        "adjustmentReason": if fraction < 1.0 { "held-task-backend-pressure" } else { "capacity-approved" },
        "lastFeedbackReason": feedback.get("adjustmentReason").unwrap_or(&json!("not-adjusted")),
        "measurementContext": feedback.get("measurementContext").unwrap_or(&Value::Null), "source": "resource-authority"})
}
fn grant(args: &Value) -> Value {
    let request = &args["request"];
    if request["workload"] == "model-transport" && !valid_transport_request(request) {
        return json!({"status":"denied","reason":"RESOURCE_INVALID_REQUEST","mode":"fallback"});
    }
    let hardware = &args["hardware"];
    let reserved = &args["reserved"];
    let threads = (cpu_capacity(hardware) - number(&reserved["cpuThreads"])).max(0.0);
    // Without native measurements every reservation remains unresolved, including resident models.
    // 缺少原生观测时所有预约都视为未兑现；小额通信池仍扣除这些债务。
    let memory = (memory_capacity(hardware, string(&request["workload"]))
        - number(&reserved["memoryBytes"]))
    .max(0.0);
    let usage = number(&hardware["cpu"]["usagePercent"]);
    let denial = if number(&request["gpuMemoryBytes"]) > 0.0 {
        Some("RESOURCE_GPU_UNKNOWN")
    } else if number(&args["leasesCount"]) >= 128.0 {
        Some("RESOURCE_LEASE_LIMIT")
    } else if number(&request["memoryBytes"]) > memory
        || number(&request["cpuThreads"]) > 0.0 && threads == 0.0
        || request["kind"] == "background" && number(&request["cpuThreads"]) > 0.0 && usage > 90.0
    {
        Some("RESOURCE_PRESSURE")
    } else {
        None
    };
    if let Some(reason) = denial {
        return json!({"status": "denied", "reason": reason, "mode": "fallback"});
    }
    let policy_fraction = number(&args["policyFraction"]);
    let fraction = if usage > 85.0 || number(&hardware["memory"]["availableBytes"]) < 1024.0 * MIB {
        policy_fraction.min(0.5)
    } else {
        policy_fraction
    };
    let granted =
        threads
            .min(number(&request["cpuThreads"]))
            .min(if request["kind"] == "background" {
                (cpu_capacity(hardware) * fraction).floor().max(1.0)
            } else {
                threads
            });
    json!({"status": "granted", "mode": "fallback", "device": "cpu", "cpuThreads": granted,
        "memoryBytes": request["memoryBytes"], "gpuMemoryBytes": 0, "suggestions": suggestions(args, granted, number(&request["memoryBytes"]), fraction)})
}

/// Unknown native ownership remains fenced; these rules never infer GPU release from free memory alone.
/// 原生归属未知时继续隔离预约；这些规则不能仅凭空闲显存推断 GPU 预约已释放。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    Ok(match operation {
        "resources.validRequest" => json!(valid_request(&args["request"])),
        "resources.validFeedback" => json!(valid_feedback(&args["feedback"])),
        "resources.capacity" => {
            json!({"cpuThreads": cpu_capacity(&args["hardware"]), "memoryBytes": memory_capacity(&args["hardware"], string(&args["workload"]))})
        }
        "resources.fallbackGrant" => grant(args),
        "resources.feedbackIdentity" => {
            feedback_identity(string(&args["taskId"]), &args["feedback"])
        }
        "resources.feedbackUpdate" => feedback_update(args),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hardware(available_mib: u64) -> Value {
        json!({"cpu":{"logicalCores":8,"usagePercent":10},
            "memory":{"totalBytes":16_u64 << 30,"availableBytes":available_mib << 20}})
    }

    fn transport_request() -> Value {
        json!({"taskId":"transport","workspaceId":"test","kind":"foreground","workload":"model-transport",
            "cpuThreads":1,"memoryBytes":16 << 20,"gpuMemoryBytes":0,"ttlMs":1000,"waitMs":0})
    }

    fn args() -> Value {
        json!({"request":transport_request(),"hardware":hardware(480),
            "reserved":{"cpuThreads":0,"memoryBytes":0},"leasesCount":0,"policyFraction":1,"feedback":{}})
    }

    #[test]
    fn low_memory_transport_uses_a_small_pool_without_relaxing_compute() {
        assert_eq!(grant(&args())["status"], "granted");
        assert_eq!(memory_capacity(&hardware(480), "compute"), 0.0);
        assert_eq!(memory_capacity(&hardware(480), "model-transport"), MAX_TRANSPORT_MEMORY_BYTES);
        assert_eq!(memory_capacity(&hardware(70), "model-transport"), 6.0 * MIB);
        let mut request = args(); request["request"]["workload"] = json!("compute");
        assert_eq!(grant(&request)["reason"], "RESOURCE_PRESSURE");
        request["request"]["workload"] = json!("model-transport");
        request["hardware"] = hardware(79);
        assert_eq!(grant(&request)["reason"], "RESOURCE_PRESSURE");
    }

    #[test]
    fn fallback_keeps_unknown_debt_and_bounds_aggregate_transport_reservations() {
        let mut request = args();
        request["reserved"]["memoryBytes"] = json!(48 << 20);
        assert_eq!(grant(&request)["status"], "granted");
        request["reserved"]["memoryBytes"] = json!(64 << 20);
        assert_eq!(grant(&request)["reason"], "RESOURCE_PRESSURE");
        request["reserved"]["memoryBytes"] = json!(8_u64 << 30);
        assert_eq!(grant(&request)["reason"], "RESOURCE_PRESSURE");
    }

    #[test]
    fn direct_fallback_transport_cannot_bypass_the_small_foreground_contract() {
        assert!(valid_request(&transport_request()));
        for (field, value) in [("kind",json!("background")),("cpuThreads",json!(2)),
            ("memoryBytes",json!(33 << 20)),("memoryBytes",json!(0)),("gpuMemoryBytes",json!(1))] {
            let mut request = args(); request["request"][field] = value;
            assert!(!valid_request(&request["request"]));
            assert_eq!(grant(&request)["reason"], "RESOURCE_INVALID_REQUEST");
        }
    }
}
