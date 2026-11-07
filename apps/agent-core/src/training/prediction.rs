//! 预测复用已完成模型及预处理，不重新训练。
use super::{invalid, runtime, store};
use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    tools::gateway::GatewayToolContext,
};
use serde_json::{Value, json};
use std::time::Duration;
pub(super) async fn predict(
    input: &Value,
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    let dir = store::scope_dir(ctx)
        .join("jobs")
        .join(store::id(input, "jobId")?);
    let state = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    if state["state"] != "completed" {
        return Err(invalid("请先选择已完成训练的模型。"));
    }
    let samples = input["samples"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 1000)
        .ok_or_else(|| invalid("请提供1至1000条预测样本。"))?;
    let python = runtime::job_python(ctx, config, &dir)?;
    let script = runtime::worker(ctx)?;
    let path = dir.join(format!("prediction-{}.json", uuid::Uuid::new_v4()));
    store::write(ctx, &path, &json!({"samples":samples}))?;
    let mut command = runtime::command(&python, &script);
    command
        .arg("predict")
        .arg("--job-dir")
        .arg(&dir)
        .arg("--input-file")
        .arg(&path);
    let result = tokio::time::timeout(Duration::from_secs(90), command.output()).await;
    let _ = std::fs::remove_file(path);
    let out = result.map_err(|_| {
        GatewayError::new(
            "TRAINING_PREDICTION_TIMEOUT",
            "本次预测超时，训练模型及进度未改变。",
            504,
        )
    })??;
    let value = serde_json::from_slice::<Value>(&out.stdout)
        .map_err(|_| invalid("模型预测返回格式无效。"))?;
    if !out.status.success() {
        return Err(GatewayError::new(
            value["error"]["code"]
                .as_str()
                .unwrap_or("TRAINING_PREDICTION_FAILED"),
            value["error"]["message"]
                .as_str()
                .unwrap_or("本次预测未完成，训练模型未改变。"),
            400,
        ));
    }
    Ok(value)
}
pub(super) async fn schema(
    input: &Value,
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    let dir = store::scope_dir(ctx)
        .join("jobs")
        .join(store::id(input, "jobId")?);
    let state = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    if state["state"] != "completed" {
        return Err(invalid("请先选择已完成训练的模型。"));
    }
    let saved = store::read(ctx, &dir.join("config.json"), 1024 * 1024)?;
    if matches!(saved["algorithm"].as_str(), Some("text.sft" | "text.lora")) {
        super::trial::authorize_model(ctx, &saved, &dir)?;
    }
    let python = runtime::job_python(ctx, config, &dir)?;
    let script = runtime::worker(ctx)?.with_file_name("trial_worker.py");
    let mut command = runtime::command(&python, &script);
    command.arg("schema").arg("--job-dir").arg(&dir);
    super::trial::probe(command).await
}
