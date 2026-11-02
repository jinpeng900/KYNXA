//! 训练记录按工作归属隔离，沿用正式存储的原子写入。
use crate::{
    gateway_contract::GatewayError, storage::managed_files, tools::gateway::GatewayToolContext,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) const RECORD_LIMIT: u64 = 128 * 1024 * 1024;
pub(crate) fn scope_dir(ctx: &GatewayToolContext) -> PathBuf {
    let owner = format!(
        "{}\n{}",
        ctx.project_id.as_deref().unwrap_or(&ctx.conversation_id),
        ctx.workdir.to_string_lossy()
    );
    ctx.data_home
        .join("Training")
        .join(managed_files::digest(owner.as_bytes()))
}
pub(crate) fn id(value: &Value, key: &str) -> Result<String, GatewayError> {
    value[key]
        .as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .map(|id| id.to_string())
        .ok_or_else(|| super::invalid("训练记录标识无效。"))
}
pub(crate) fn read(
    ctx: &GatewayToolContext,
    path: &Path,
    maximum: u64,
) -> Result<Value, GatewayError> {
    let data = managed_files::read(&ctx.data_home, path, maximum)?
        .ok_or_else(|| GatewayError::new("TRAINING_NOT_FOUND", "训练记录不存在。", 404))?;
    serde_json::from_slice(&data).map_err(|_| {
        GatewayError::new(
            "TRAINING_RECORD_INVALID",
            "训练记录无法读取，原文件已保留。",
            409,
        )
    })
}
pub(crate) fn write(
    ctx: &GatewayToolContext,
    path: &Path,
    value: &Value,
) -> Result<(), GatewayError> {
    let bytes = serde_json::to_vec(value).map_err(|_| super::invalid("训练记录无法编码。"))?;
    managed_files::atomic_write(&ctx.data_home, path, &bytes, RECORD_LIMIT)?;
    Ok(())
}
pub(super) fn list(
    ctx: &GatewayToolContext,
    family: &str,
    record: &str,
) -> Result<Vec<Value>, GatewayError> {
    let path = scope_dir(ctx).join(family);
    managed_files::inspect(&ctx.data_home, &path, true, false, RECORD_LIMIT)?;
    let mut rows = Vec::new();
    if !path.exists() {
        return Ok(rows);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_ok()
        {
            let p = entry.path().join(record);
            if p.exists() {
                rows.push(read(ctx, &p, RECORD_LIMIT)?);
            }
        }
    }
    rows.sort_by(|a, b| b["createdAt"].as_str().cmp(&a["createdAt"].as_str()));
    Ok(rows)
}
pub(super) fn summary(mut value: Value) -> Value {
    if let Some(samples) = value.get("samples").and_then(Value::as_array) {
        let count = samples.len();
        let preview = samples.iter().take(8).cloned().collect::<Vec<_>>();
        value["sampleCount"] = json!(count);
        value["preview"] = json!(preview);
        value.as_object_mut().unwrap().remove("samples");
    }
    value
}
