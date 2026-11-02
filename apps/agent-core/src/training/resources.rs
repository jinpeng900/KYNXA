//! 训练估计交给现有资源准入，执行进程采样负责校正占用。
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::path::Path;
const MIB: u64 = 1024 * 1024;
fn mul(values: &[u64]) -> Result<u64, GatewayError> {
    values
        .iter()
        .try_fold(1u64, |a, b| a.checked_mul(*b))
        .ok_or_else(|| super::invalid("训练规模超出可表示范围，请检查批量、层宽与文本长度。"))
}
pub(super) fn plan(config: &Value, dir: &Path, prepared: &Value) -> Result<Value, GatewayError> {
    let data = std::fs::metadata(dir.join("dataset.json"))?.len();
    estimate(config, data, prepared)
}
fn estimate(config: &Value, data: u64, prepared: &Value) -> Result<Value, GatewayError> {
    let p = &config["parameters"];
    let mut weights = 0u64;
    if let Some(path) = config["modelPath"].as_str() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file()
                && matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("safetensors" | "bin" | "pt")
                )
            {
                weights = weights.saturating_add(entry.metadata()?.len());
            }
        }
    }
    let backend = prepared["backend"].as_str().unwrap_or("builtin");
    let batch = p["batchSize"].as_u64().unwrap_or(16).max(1);
    let cuda = prepared["device"] == "cuda";
    let mut gpu = 0;
    let base = if backend == "builtin" {
        64 * MIB
    } else {
        512 * MIB
    };
    let mut memory = base + mul(&[data, 12])?;
    if backend == "builtin" {
        let count = &prepared["modelStructure"]["parameterCount"];
        let params = if count.is_null() {
            0
        } else {
            count
                .as_u64()
                .ok_or_else(|| super::invalid("网络参数量超出本机可处理范围。"))?
        };
        memory = memory.saturating_add(mul(&[params, 160])?);
    } else if config["algorithm"] == "vision.classifier" {
        let channels = p["hiddenLayers"]
            .as_array()
            .map(|v| v.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
            .unwrap_or_else(|| vec![16, 32]);
        let mut side = p["imageSize"].as_u64().unwrap_or(64);
        let mut previous = 3;
        let mut parameters = 0u64;
        let mut activations = 0u64;
        for channel in channels {
            parameters = parameters.saturating_add(mul(&[previous, channel, 9])?);
            activations = activations.saturating_add(mul(&[side, side, channel, batch, 16])?);
            side = side.div_ceil(2);
            previous = channel;
        }
        let working = mul(&[parameters, 20])?.saturating_add(activations);
        if cuda {
            gpu = 256 * MIB + working;
        } else {
            memory = memory.saturating_add(working);
        }
    } else if weights > 0 {
        let structure = &prepared["modelStructure"];
        let activations = mul(&[
            batch,
            p["maxLength"].as_u64().unwrap_or(1024),
            structure["hiddenSize"].as_u64().unwrap_or(2048),
            structure["hiddenLayers"].as_u64().unwrap_or(24),
            if p["gradientCheckpointing"] == true {
                3
            } else {
                12
            },
        ])?;
        let lora = config["algorithm"] == "text.lora"
            || p["quantization"] == "4bit"
            || p["loraRank"].is_number();
        let model_memory = if lora {
            (if p["quantization"] == "4bit" {
                weights / 3
            } else {
                weights
            })
            .saturating_add(128 * MIB)
        } else {
            mul(&[weights, 8])?
        };
        if cuda {
            gpu = model_memory.saturating_add(activations);
            memory = memory.saturating_add(weights.min(2048 * MIB));
        } else {
            memory = memory
                .saturating_add(model_memory)
                .saturating_add(activations);
        }
    }
    Ok(
        json!({"cpuThreads":p["cpuThreads"].as_u64().unwrap_or(2).max(1),"memoryBytes":p["memoryMiB"].as_u64().map(|v|mul(&[v,MIB])).transpose()?.unwrap_or(memory),
        "gpuMemoryBytes":p["gpuMemoryMiB"].as_u64().map(|v|mul(&[v,MIB])).transpose()?.unwrap_or(gpu),"device":prepared["device"],"estimate":true,"weightBytes":weights}),
    )
}
pub(super) fn preview(
    input: &Value,
    ctx: &crate::tools::gateway::GatewayToolContext,
) -> Result<Value, GatewayError> {
    let id = super::store::id(input, "datasetId")?;
    let dir = super::store::scope_dir(ctx).join("datasets").join(id);
    let summary = super::store::read(ctx, &dir.join("summary.json"), super::store::RECORD_LIMIT)?;
    let mut config = input.clone();
    config["modelPath"] = super::jobs::authorize_model(input, ctx)?;
    let p = &config["parameters"];
    let backend = input["backend"].as_str().unwrap_or("builtin");
    let device = if p["device"] == "cuda" { "cuda" } else { "cpu" };
    let mut structure = json!({});
    if backend == "builtin" {
        let width = summary["preview"][0]["features"]
            .as_array()
            .map_or(0, |v| v.len()) as u64;
        let hidden = if input["algorithm"]
            .as_str()
            .is_some_and(|a| a.starts_with("mlp."))
        {
            p["hiddenLayers"]
                .as_array()
                .map(|v| v.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
                .unwrap_or_else(|| vec![32, 16])
        } else {
            vec![]
        };
        let mut shape = vec![width];
        shape.extend(hidden);
        // 预览未包含全部类别，用样本数上界估算，正式启动仍由实际模型检查。
        shape.push(if input["task"] == "regression" {
            1
        } else {
            summary["sampleCount"].as_u64().unwrap_or(1).max(1)
        });
        let count = shape.windows(2).try_fold(0u64, |sum, w| {
            mul(&[w[0].saturating_add(1), w[1]]).and_then(|n| {
                sum.checked_add(n)
                    .ok_or_else(|| super::invalid("层规模过大。"))
            })
        })?;
        structure["parameterCount"] = json!(count);
    } else if let Some(path) = config["modelPath"].as_str() {
        let path = std::path::Path::new(path).join("config.json");
        if std::fs::metadata(&path)?.len() > 1024 * 1024 {
            return Err(super::invalid("模型配置过大。"));
        }
        let model: Value = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| super::invalid("模型配置无法读取。"))?;
        structure = json!({"hiddenSize":model["hidden_size"].as_u64().or(model["n_embd"].as_u64()).unwrap_or(2048),"hiddenLayers":model["num_hidden_layers"].as_u64().or(model["n_layer"].as_u64()).unwrap_or(24)});
    }
    let plan = estimate(
        &config,
        std::fs::metadata(dir.join("dataset.json"))?.len(),
        &json!({"backend":backend,"device":device,"modelStructure":structure}),
    )?;
    Ok(
        json!({"plan":plan,"devicePending":p["device"].as_str().is_none_or(|v|v=="auto"),"basis":"configuration-estimate","sampleCount":summary["sampleCount"]}),
    )
}
