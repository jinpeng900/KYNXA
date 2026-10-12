use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};
use url::Url;

const MIB: f64 = 1_048_576.0;
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= 0.0 && *n <= 9_007_199_254_740_991.0)
}
fn bounded(value: &Value) -> Option<f64> {
    number(value).filter(|n| *n <= 8.0 * 1024_f64.powi(4))
}
fn positive_tokens(value: &Value) -> Option<f64> {
    number(value).filter(|n| *n > 0.0 && *n <= 16_777_216.0)
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn null_number(value: Option<f64>) -> Value {
    value.map_or(Value::Null, |n| json!(n))
}
fn unknown(snapshot: &Value, reason: &str, breakdown: Option<Value>) -> Value {
    let mut result = json!({"state":"unknown","requiresAdmission":false,"memoryBytes":null,"gpuMemoryBytes":null,"observationOnly":true,"memoryOwnership":"external","observedAt":snapshot.get("observedAt").unwrap_or(&Value::Null),"uncertainty":{"state":"unbounded","reasons":[reason],"marginBytes":null}});
    if let Some(v) = breakdown {
        result["breakdown"] = v;
    }
    result
}

/// Forecast only pending growth; existing external residency is never reserved a second time.
/// 只预测待发生的增量；外部已驻留资源不能被再次整份预约。
pub fn demand(args: &Value) -> Value {
    let s = &args["snapshot"];
    if s["backend"] != "ollama" {
        let mut result = unknown(s, "EXTERNAL_MODEL_RESOURCE_API_UNAVAILABLE", None);
        result["state"] = json!("not-applicable");
        return result;
    }
    let requested = positive_tokens(&args["contextTokens"]);
    let maximum = positive_tokens(&s["modelMaximumContextTokens"]);
    let target = requested.map(|n| maximum.map_or(n, |m| n.min(m)));
    let baseline = positive_tokens(&s["runtimeContextTokens"]);
    let weight = bounded(&s["estimatedWeightBytes"]);
    let mut breakdown = json!({"baselineContextTokens":null_number(baseline),"targetContextTokens":null_number(target),"estimatedWeightBytes":null_number(weight),"weightIncrementBytes":null,"kvIncrementBytes":null,"observedResidencyReservedAgain":false});
    let Some(loaded) = s["loaded"].as_bool() else {
        return unknown(s, "EXTERNAL_MODEL_LOAD_STATE_UNKNOWN", Some(breakdown));
    };
    let partial = !loaded && target.is_none() && weight.is_some_and(|n| n > 0.0);
    if target.is_none() && !partial {
        return unknown(s, "EXTERNAL_MODEL_CONTEXT_TARGET_UNKNOWN", Some(breakdown));
    }
    if loaded && baseline.is_some() && target.is_some_and(|n| n <= baseline.unwrap()) {
        breakdown["weightIncrementBytes"] = json!(0);
        breakdown["kvIncrementBytes"] = json!(0);
        return json!({"state":"ready","requiresAdmission":false,"memoryBytes":0,"gpuMemoryBytes":0,"observationOnly":true,"memoryOwnership":"external","observedAt":s.get("observedAt").unwrap_or(&Value::Null),"uncertainty":{"state":"no-increment-observed","reasons":[],"marginBytes":0},"breakdown":breakdown});
    }
    let kv = bounded(&s["kvBytesPerToken"]);
    if kv.is_none_or(|n| n == 0.0) && !partial {
        return unknown(s, "EXTERNAL_MODEL_KV_SHAPE_UNKNOWN", Some(breakdown));
    }
    if loaded && baseline.is_none() {
        return unknown(
            s,
            "EXTERNAL_MODEL_ALLOCATED_CONTEXT_UNKNOWN",
            Some(breakdown),
        );
    }
    if !loaded && weight.is_none_or(|n| n == 0.0) {
        return unknown(s, "EXTERNAL_MODEL_WEIGHT_ESTIMATE_UNKNOWN", Some(breakdown));
    }
    let growth = if loaded {
        target.unwrap_or(0.0) - baseline.unwrap_or(0.0)
    } else {
        target.unwrap_or(0.0)
    };
    let increment = if partial {
        None
    } else {
        bounded(&json!(growth * kv.unwrap_or(0.0)))
    };
    let weight_increment = if loaded { 0.0 } else { weight.unwrap_or(0.0) };
    if increment.is_none() && !partial {
        return unknown(s, "EXTERNAL_MODEL_DEMAND_EXCEEDS_BOUND", Some(breakdown));
    }
    let kv_estimate = &s["kvCacheEstimate"];
    let mut reasons = vec![
        if partial {
            "CONTEXT_TARGET_UNKNOWN"
        } else if kv_estimate["dtype"] == "unknown" {
            "KV_DTYPE_NOT_REPORTED_F16_UPPER_BOUND"
        } else {
            "KV_DTYPE_ASSUMED_F16"
        },
        "RUNTIME_SCRATCH_ESTIMATED",
    ];
    if kv_estimate["parallelSlots"] == "unknown" {
        reasons.push("KV_PARALLEL_SLOTS_NOT_REPORTED");
    }
    let placement = if loaded {
        let Some(gpu) = bounded(&s["observedGpuMemoryBytes"]) else {
            return unknown(
                s,
                "EXTERNAL_MODEL_DEVICE_PLACEMENT_UNKNOWN",
                Some(breakdown),
            );
        };
        if gpu > 0.0 {
            reasons.push("KV_OFFLOAD_DISTRIBUTION_UNKNOWN");
            "gpu-upper-bound"
        } else {
            "cpu"
        }
    } else {
        let cpu = s["backendSelection"]["configuredGpuLayers"] == 0;
        let gpu = &args["hardware"]["gpu"]["state"];
        if !cpu && gpu != "available" && gpu != "unavailable" {
            return unknown(s, "EXTERNAL_MODEL_GPU_CAPACITY_UNKNOWN", Some(breakdown));
        }
        reasons.push("WEIGHTS_ESTIMATED_FROM_QUANTIZATION");
        if !cpu && gpu == "available" {
            reasons.push("INITIAL_OFFLOAD_DISTRIBUTION_UNKNOWN");
            "gpu-upper-bound"
        } else {
            "cpu"
        }
    };
    let Some(pending) = bounded(&json!(weight_increment + increment.unwrap_or(0.0))) else {
        return unknown(s, "EXTERNAL_MODEL_DEMAND_EXCEEDS_BOUND", Some(breakdown));
    };
    let margin = (64.0 * MIB)
        .max(pending * if loaded { 0.15 } else { 0.2 })
        .ceil();
    let Some(memory) = bounded(&json!(pending + margin)) else {
        return unknown(s, "EXTERNAL_MODEL_DEMAND_EXCEEDS_BOUND", Some(breakdown));
    };
    let gpu = if placement == "gpu-upper-bound" {
        memory
    } else {
        0.0
    };
    let slots = !partial && kv_estimate["parallelSlots"] == "unknown";
    breakdown["weightIncrementBytes"] = json!(weight_increment);
    breakdown["kvIncrementBytes"] = null_number(increment);
    breakdown["placement"] = json!(placement);
    breakdown["kvEstimateDtype"] = if partial {
        Value::Null
    } else {
        kv_estimate
            .get("reservationDtype")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!("assumed-f16"))
    };
    breakdown["reportedKvDtype"] = kv_estimate
        .get("dtype")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!("unknown"));
    breakdown["backendSelection"] = s.get("backendSelection").cloned().unwrap_or(Value::Null);
    breakdown["kvParallelSlots"] = kv_estimate
        .get("parallelSlots")
        .cloned()
        .unwrap_or(json!("unknown"));
    breakdown["memoryIncludesTransientStaging"] = json!(!loaded);
    breakdown["gpuReservationIncludesOnlyPendingAllocations"] = json!(true);
    json!({"state":if partial||slots{"partial"}else{"ready"},"requiresAdmission":memory>0.0||gpu>0.0,"memoryBytes":memory,"gpuMemoryBytes":gpu,"observationOnly":true,"memoryOwnership":"external","partialCoverage":partial||slots,"unknownComponents":if partial{vec!["kv-cache"]}else if slots{vec!["kv-parallel-slots"]}else{vec![]},"observedAt":s.get("observedAt").unwrap_or(&Value::Null),"uncertainty":{"state":if slots{"estimated-per-slot-upper-bound"}else{"estimated-upper-bound"},"reasons":reasons,"marginBytes":margin},"breakdown":breakdown})
}

fn base() -> Value {
    json!({"backend":"external-openai","loaded":null,"contextTokens":null,"observedMemoryBytes":null,"observedGpuMemoryBytes":null,"estimatedKvCacheBytes":null,"uncertaintyMarginBytes":256.0*MIB,"observationOnly":true,"memoryOwnership":"external","generationState":"unknown","applicationGenerationState":"idle","globalGenerationState":"unknown"})
}
fn model_match<'a>(models: &'a [Value], model: &str) -> Option<&'a Value> {
    models.iter().find(|v| {
        text(&v["model"]) == model
            || text(&v["name"]) == model
            || !model.contains(':')
                && (text(&v["model"]) == format!("{model}:latest")
                    || text(&v["name"]) == format!("{model}:latest"))
    })
}
fn configured(show: &Value, name: &str) -> Option<f64> {
    let parameters = text(&show["parameters"]);
    if parameters.encode_utf16().count() > 16384 {
        return None;
    }
    let pattern = format!(r"(?m)^\s*{name}\s+(\d+)\s*$");
    regex::Regex::new(&pattern)
        .ok()?
        .captures(parameters)?
        .get(1)?
        .as_str()
        .parse::<f64>()
        .ok()
        .and_then(|n| number(&json!(n)))
}

fn ollama(args: &Value) -> Value {
    let model = text(&args["model"]);
    let running_result = &args["running"];
    let models = array(&running_result["models"]);
    let has_list = running_result["models"].is_array();
    let complete = has_list && models.len() <= 128;
    let running = model_match(&models[..models.len().min(128)], model)
        .cloned()
        .unwrap_or(Value::Null);
    let show = &args["show"];
    let info = &show["model_info"];
    let arch = text(&info["general.architecture"]);
    let shape = |suffix: &str| {
        if regex::Regex::new(r"(?i)^[a-z0-9_]{1,48}$")
            .unwrap()
            .is_match(arch)
        {
            number(&info[format!("{arch}.{suffix}")])
        } else {
            None
        }
    };
    let layers = shape("block_count");
    let heads = shape("attention.head_count");
    let hidden = shape("embedding_length");
    let fallback = hidden
        .zip(heads)
        .filter(|(h, n)| *h > 0.0 && *n > 0.0 && h % n == 0.0)
        .map(|(h, n)| h / n);
    let kv_heads = shape("attention.head_count_kv").or(heads);
    let key = shape("attention.key_length").or(fallback);
    let value = shape("attention.value_length").or(fallback);
    let configured_context = configured(show, "num_ctx");
    let configured_gpu = configured(show, "num_gpu");
    let runtime = number(&running["context_length"]);
    let context = runtime
        .filter(|n| *n > 0.0)
        .or(configured_context.filter(|n| *n > 0.0));
    let estimation = number(&args["contextTokens"])
        .filter(|n| *n > 0.0)
        .or(context);
    let memory = number(&running["size"]);
    let gpu = number(&running["size_vram"]);
    let placement = if !running.is_null()
        && gpu.is_some()
        && memory.is_some_and(|n| n > 0.0)
        && gpu.unwrap() <= memory.unwrap()
    {
        if gpu == Some(0.0) {
            "cpu"
        } else if gpu.unwrap() < memory.unwrap() {
            "mixed"
        } else {
            "gpu"
        }
    } else {
        "unknown"
    };
    let backend = json!({"state":placement,"source":if placement=="unknown"{"unreported"}else{"ollama-api-ps-residency"},"configuredGpuLayers":null_number(configured_gpu),"executionBackend":"unknown","gpuDeviceIdentity":"unknown","exact":false});
    let kv_shape = layers
        .zip(kv_heads)
        .zip(key.zip(value))
        .filter(|((l, h), (k, v))| {
            *l > 0.0
                && *l <= 512.0
                && *h > 0.0
                && *h <= 256.0
                && *k > 0.0
                && *k <= 4096.0
                && *v > 0.0
                && *v <= 4096.0
        });
    let kv=kv_shape.zip(estimation).and_then(|(((l,h),(k,v)),c)|{
        let per=l*h*(k+v)*2.0;let max=c*per;if number(&json!(max)).is_none()||max>8.0*1024_f64.powi(4){return None;}
        let quantized=|bytes:f64|l*h*((k/32.0).ceil()+(v/32.0).ceil())*bytes;
        Some(json!({"dtype":"unknown","dtypeSource":"ollama-api-not-reported","reservationDtype":"f16-upper-bound","supportedDtypeEstimates":{"f16BytesPerToken":per,"q8_0BytesPerToken":quantized(34.0),"q4_0BytesPerToken":quantized(18.0)},"minimumBytes":c*per.min(quantized(18.0)),"maximumBytes":max,"contextTokens":c,"backend":placement,"parallelSlots":"unknown","accountsForSlidingWindow":false,"includesParallelSlotMultiplication":false,"exact":false}))
    });
    let estimated = kv.as_ref().and_then(|v| number(&v["maximumBytes"]));
    let parameter_count = number(&info["general.parameter_count"]).filter(|n| *n > 0.0);
    let quant = text(
        show["details"]
            .get("quantization_level")
            .filter(|value| !value.is_null())
            .unwrap_or(&running["details"]["quantization_level"]),
    );
    let quant = if regex::Regex::new(r"(?i)^[a-z0-9_]{1,32}$")
        .unwrap()
        .is_match(quant)
    {
        Some(quant.to_uppercase())
    } else {
        None
    };
    let bits = match quant.as_deref() {
        Some("Q2_K") => Some(3.0),
        Some("Q3_K_S") => Some(4.0),
        Some("Q3_K_M") => Some(4.5),
        Some("Q3_K_L" | "Q4_0" | "Q4_1" | "Q4_K_S") => Some(5.0),
        Some("Q4_K_M") => Some(5.5),
        Some("Q5_0" | "Q5_1" | "Q5_K_S") => Some(6.0),
        Some("Q5_K_M") => Some(6.5),
        Some("Q6_K") => Some(7.0),
        Some("Q8_0" | "Q8_1") => Some(9.0),
        Some("F16" | "BF16") => Some(16.0),
        Some("F32") => Some(32.0),
        _ => None,
    };
    let file_size = if running.is_null() && complete && array(&args["tags"]["models"]).len() <= 128
    {
        model_match(array(&args["tags"]["models"]), model)
            .and_then(|v| number(&v["size"]))
            .filter(|n| *n > 0.0)
    } else {
        None
    };
    let weight = file_size
        .or(parameter_count.zip(bits).map(|(n, b)| (n * b / 8.0).ceil()))
        .filter(|n| number(&json!(n)).is_some() && *n > 0.0);
    let weight_estimate=weight.map(|_|{let mut e=json!({"exact":false,"effectiveBitsPerParameter":if file_size.is_some(){Value::Null}else{null_number(bits)},"source":if file_size.is_some(){"serialized-size"}else{"parameter-count-and-quantization"},"includesRuntimeScratch":false});if file_size.is_some(){e["includesQuantizationMetadata"]=json!(true);}e});
    let mut result = base();
    let values = json!({"backend":"ollama","loaded":if !running.is_null(){json!(true)}else if complete{json!(false)}else{Value::Null},"contextTokens":null_number(context),"runtimeContextTokens":null_number(runtime),"configuredContextTokens":null_number(configured_context),"modelMaximumContextTokens":null_number(shape("context_length")),"observedMemoryBytes":null_number(memory),"observedGpuMemoryBytes":null_number(gpu),"estimatedKvCacheBytes":null_number(estimated),"parameterCount":null_number(parameter_count),"quantizationLevel":quant,"estimatedWeightBytes":null_number(weight),"modelFileBytes":null_number(file_size),"weightEstimate":weight_estimate,"kvBytesPerToken":null_number(kv.as_ref().and_then(|v|number(&v["supportedDtypeEstimates"]["f16BytesPerToken"]))),"backendSelection":backend,"observedMemoryScope":"server-reported-model-size-not-host-rss","kvCacheEstimate":kv,"uncertaintyComponents":if placement=="unknown"{vec!["kv-dtype-not-reported","parallel-slots-not-reported","runtime-scratch-not-reported","execution-device-not-reported"]}else{vec!["kv-dtype-not-reported","parallel-slots-not-reported","runtime-scratch-not-reported"]},"uncertaintyMarginBytes":(256.0*MIB).max(memory.or(estimated).unwrap_or(0.0)*0.2).ceil(),"observedAt":args["now"],"source":if has_list{"ollama-api-ps"}else{"unknown"},"activityScope":"this-application"});
    result
        .as_object_mut()
        .unwrap()
        .extend(values.as_object().unwrap().clone());
    if !complete || args["detailsRejected"] == true {
        let code = if args["runningRejected"] == true {
            args["runningErrorCode"].clone()
        } else if !has_list {
            json!("LOCAL_MODEL_OBSERVATION_INVALID_PAYLOAD")
        } else if !complete {
            json!("LOCAL_MODEL_OBSERVATION_LIST_LIMIT")
        } else {
            args["detailsErrorCode"].clone()
        };
        let mut diagnostic = json!({"metadataPartial":true});
        if !code.is_null() {
            diagnostic["code"] = code;
        }
        result["diagnostic"] = diagnostic;
    }
    result
}

#[derive(Default)]
pub struct ExternalObserverState {
    active: HashMap<String, u64>,
    cache: HashMap<String, Value>,
    order: VecDeque<String>,
    ttl: f64,
}
impl ExternalObserverState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let key = text(&args["key"]);
        match operation {
            "resources.external.init" => {
                self.ttl = args["cacheTtlMs"]
                    .as_f64()
                    .unwrap_or(2000.0)
                    .clamp(0.0, 10000.0);
                Ok(json!(null))
            }
            "resources.external.begin" => {
                if !self.active.contains_key(key) && self.active.len() >= 128 {
                    return Ok(json!(false));
                }
                *self.active.entry(key.into()).or_default() += 1;
                Ok(json!(true))
            }
            "resources.external.end" => {
                if let Some(n) = self.active.get_mut(key) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        self.active.remove(key);
                    }
                }
                Ok(Value::Null)
            }
            "resources.external.activity" => Ok(
                json!({"generationState":if self.active.contains_key(key){"generating"}else{"unknown"},"applicationGenerationState":if self.active.contains_key(key){"generating"}else{"idle"},"globalGenerationState":"unknown"}),
            ),
            "resources.external.get" => Ok(if args["refresh"] != true {
                self.cache
                    .get(text(&args["cacheKey"]))
                    .filter(|snapshot| {
                        args["now"].as_f64().unwrap_or(0.0)
                            - snapshot["observedAt"].as_f64().unwrap_or(0.0)
                            <= self.ttl
                    })
                    .cloned()
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            }),
            "resources.external.store" => {
                let cache_key = text(&args["cacheKey"]).to_owned();
                self.order.retain(|k| *k != cache_key);
                self.order.push_back(cache_key.clone());
                self.cache.insert(cache_key, args["snapshot"].clone());
                while self.order.len() > 32 {
                    if let Some(k) = self.order.pop_front() {
                        self.cache.remove(&k);
                    }
                }
                Ok(Value::Null)
            }
            "resources.external.clear" => {
                self.cache.clear();
                self.order.clear();
                Ok(Value::Null)
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "resources.externalDemand" => Ok(demand(args)),
        "resources.externalUnknown" => Ok(base()),
        "resources.externalEndpoint" => {
            let url = Url::parse(text(&args["baseUrl"])).ok();
            let local = url.as_ref().is_some_and(|u| {
                ["http", "https"].contains(&u.scheme())
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.query().is_none()
                    && u.fragment().is_none()
                    && match u.host() {
                        Some(url::Host::Domain(h)) => h.eq_ignore_ascii_case("localhost"),
                        Some(url::Host::Ipv4(ip)) => ip.octets()[0] == 127,
                        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                        _ => false,
                    }
            });
            let model = text(&args["model"]);
            Ok(
                json!({"local":local,"model":if !model.is_empty()&&model.encode_utf16().count()<=160&&!model.chars().any(|c|c<='\u{1f}'){json!(model)}else{Value::Null},"ollama":args["protocol"]=="ollama"||text(&args["providerId"]).to_lowercase().contains("ollama")||url.as_ref().is_some_and(|u|u.port()==Some(11434))}),
            )
        }
        "resources.externalOllama" => Ok(ollama(args)),
        "resources.externalNeedsTags" => {
            let models = array(&args["running"]["models"]);
            Ok(json!(
                args["running"]["models"].is_array()
                    && models.len() <= 128
                    && model_match(models, text(&args["model"])).is_none()
            ))
        }
        "resources.externalLlama" => {
            let props = &args["props"];
            let settings = &props["default_generation_settings"];
            let context = number(&settings["n_ctx"]).filter(|n| *n > 0.0 && *n <= 2_000_000.0);
            let slots = number(&props["total_slots"]).filter(|n| *n > 0.0 && *n <= 256.0);
            if context.is_none()
                || slots.is_none()
                || !settings["params"].is_object()
                || !props["chat_template"].is_string()
            {
                return Ok(Value::Null);
            }
            let loaded = props["is_sleeping"].as_bool().map(|b| !b);
            let mut result = base();
            let v = json!({"backend":"llama.cpp","loaded":loaded,"contextTokens":context,"runtimeContextTokens":if loaded==Some(false){None}else{context},"configuredContextTokens":null,"modelMaximumContextTokens":null,"parallelSlots":slots,"source":"llama-cpp-props","endpointOrigin":args["origin"],"observedAt":args["now"],"activityScope":"this-application","uncertaintyComponents":["memory-residency-not-reported","execution-device-not-reported","kv-shape-not-reported"]});
            result
                .as_object_mut()
                .unwrap()
                .extend(v.as_object().unwrap().clone());
            Ok(result)
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
