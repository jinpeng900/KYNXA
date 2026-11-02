//! 实验备注与显式断点清理，不改冻结配置和训练数据。
use super::{invalid, jobs, store};
use crate::{
    gateway_contract::GatewayError, storage::managed_files, tools::gateway::GatewayToolContext,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
static METADATA: Mutex<()> = Mutex::new(());

pub(super) fn overlay(
    ctx: &GatewayToolContext,
    dir: &Path,
    job: &mut Value,
) -> Result<(), GatewayError> {
    let file = dir.join("metadata.json");
    if file.exists() {
        let meta = store::read(ctx, &file, 16384)?;
        for key in ["name", "notes"] {
            if meta[key].is_string() {
                job[key] = meta[key].clone();
            }
        }
    }
    Ok(())
}
pub(super) fn update(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let _lock = METADATA.lock().map_err(|_| invalid("实验备注正在保存。"))?;
    let dir = directory(input, ctx)?;
    let mut meta = if dir.join("metadata.json").exists() {
        store::read(ctx, &dir.join("metadata.json"), 16384)?
    } else {
        json!({})
    };
    for (key, max) in [("name", 160), ("notes", 2000)] {
        if let Some(value) = input.get(key) {
            let value = value
                .as_str()
                .ok_or_else(|| invalid("名称和备注必须是文字。"))?
                .trim();
            if value.chars().count() > max || key == "name" && value.is_empty() {
                return Err(invalid("名称不能为空，名称最多160字、备注最多2000字。"));
            }
            meta[key] = json!(value);
        }
    }
    meta["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
    store::write(ctx, &dir.join("metadata.json"), &meta)?;
    Ok(json!({"metadata":meta}))
}
fn directory(input: &Value, ctx: &GatewayToolContext) -> Result<PathBuf, GatewayError> {
    let dir = store::scope_dir(ctx)
        .join("jobs")
        .join(store::id(input, "jobId")?);
    store::read(ctx, &dir.join("config.json"), 1024 * 1024)?;
    Ok(dir)
}
fn references(value: &Value, paths: &mut Vec<PathBuf>) {
    match value {
        Value::String(s) => {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                paths.push(p);
            }
        }
        Value::Array(v) => {
            for v in v {
                references(v, paths);
            }
        }
        Value::Object(v) => {
            for v in v.values() {
                references(v, paths);
            }
        }
        _ => {}
    }
}
fn footprint(path: &Path, ctx: &GatewayToolContext) -> Result<(u64, Vec<Value>), GatewayError> {
    let meta = std::fs::symlink_metadata(path)?;
    managed_files::inspect(&ctx.data_home, path, false, meta.is_file(), u64::MAX)?;
    if meta.file_type().is_symlink() {
        return Err(invalid("检查点包含链接，已跳过清理。"));
    }
    if meta.is_file() {
        return Ok((
            meta.len(),
            vec![
                json!({"path":path,"bytes":meta.len(),"modified":meta.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|t|t.as_nanos().to_string())}),
            ],
        ));
    }
    let mut bytes = 0u64;
    let mut entries = vec![];
    for entry in std::fs::read_dir(path)? {
        let (size, mut files) = footprint(&entry?.path(), ctx)?;
        bytes = bytes.saturating_add(size);
        entries.append(&mut files);
    }
    entries.sort_by_key(|v| v["path"].as_str().unwrap_or("").to_owned());
    Ok((bytes, entries))
}
fn plan(dir: &Path, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let state = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    if !matches!(
        state["state"].as_str(),
        Some("ready" | "completed" | "paused" | "stopped" | "failed")
    ) {
        return Err(invalid("请等训练保存完成后再整理。"));
    }
    let checkpoint = dir.join("checkpoint");
    let mut protected = Vec::new();
    references(&state, &mut protected);
    let pointer = checkpoint.join("training.json");
    if pointer.exists() {
        references(
            &store::read(ctx, &pointer, store::RECORD_LIMIT)?,
            &mut protected,
        );
    }
    let protected = protected
        .into_iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for (root, prefix, suffix) in [
        (checkpoint.clone(), "estimator-", ".joblib"),
        (checkpoint.clone(), "vision-", ".pt"),
        (checkpoint.join("hf"), "checkpoint-", ""),
    ] {
        if !root.exists() {
            continue;
        }
        managed_files::inspect(&ctx.data_home, &root, false, false, u64::MAX)?;
        let mut family = Vec::new();
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(step) = name
                .strip_prefix(prefix)
                .and_then(|s| s.strip_suffix(suffix))
                .and_then(|s| s.parse::<u64>().ok())
            {
                family.push((step, entry.path()));
            }
        }
        family.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, path) in family.into_iter().skip(2) {
            // Keep every live reference plus the two latest checkpoints. / 保留所有断点引用及最新两份。
            let resolved = std::fs::canonicalize(&path)?;
            if protected
                .iter()
                .any(|p| p == &resolved || p.starts_with(&resolved) || resolved.starts_with(p))
            {
                continue;
            }
            let (bytes, files) = footprint(&path, ctx)?;
            candidates.push(json!({"path":path,"bytes":bytes,"files":files}));
        }
    }
    candidates.sort_by_key(|v| v["path"].as_str().unwrap_or("").to_owned());
    let token = managed_files::digest(&serde_json::to_vec(&candidates).unwrap());
    let bytes: u64 = candidates.iter().filter_map(|v| v["bytes"].as_u64()).sum();
    Ok(json!({"token":token,"bytes":bytes,"count":candidates.len(),"candidates":candidates}))
}
/// 作业保留自己的数据集记录，但图片样本仍读取原数据集冻结的图片。
fn dependent_jobs(
    ctx: &GatewayToolContext,
    dataset: &str,
    summary: &Value,
) -> Result<usize, GatewayError> {
    if summary["kind"] != "image" {
        return Ok(0);
    }
    let jobs = store::scope_dir(ctx).join("jobs");
    let mut count = 0;
    for config in store::list(ctx, "jobs", "config.json")? {
        if config["datasetId"] != dataset {
            continue;
        }
        let state = store::read(
            ctx,
            &jobs
                .join(config["id"].as_str().unwrap_or_default())
                .join("state.json"),
            1024 * 1024,
        )?;
        if state["state"] != "completed" {
            count += 1;
        }
    }
    Ok(count)
}
/// 仅桌面端可删除；先改名立即下架，清理失败也不会列出残缺版本，下次删除时再回收。
pub(super) async fn delete_dataset(
    action: &str,
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let ctx = ctx.clone();
    let input = input.clone();
    let apply = action.ends_with("apply");
    tokio::task::spawn_blocking(move || {
        let id = store::id(&input, "datasetId")?;
        let root = store::scope_dir(&ctx).join("datasets");
        let dir = root.join(&id);
        let result = jobs::while_dataset_idle(&ctx, &id, || {
            let summary = store::read(&ctx, &dir.join("summary.json"), store::RECORD_LIMIT)?;
            if !apply {
                return Ok(json!({"dependentJobs":dependent_jobs(&ctx,&id,&summary)?}));
            }
            managed_files::inspect(&ctx.data_home, &dir, false, false, u64::MAX)?;
            std::fs::rename(&dir, root.join(format!(".deleted-{id}")))?;
            Ok(json!({"deleted":true}))
        })?;
        if apply {
            for entry in std::fs::read_dir(&root)?.flatten() {
                if entry.file_name().to_string_lossy().starts_with(".deleted-") {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        Ok(result)
    })
    .await
    .map_err(|_| invalid("数据集删除未完成。"))?
}
pub(super) async fn cleanup(
    action: &str,
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let ctx = ctx.clone();
    let input = input.clone();
    let apply = action.ends_with("apply");
    tokio::task::spawn_blocking(move || {
        let dir = directory(&input, &ctx)?;
        jobs::while_inactive(&dir, || {
            let current = plan(&dir, &ctx)?;
            if !apply {
                return Ok(current);
            }
            if input["token"] != current["token"] {
                return Err(invalid("检查点已变化，请重新查看可清理内容。"));
            }
            if current["count"] == 0 {
                return Ok(json!({"removedCount":0,"freedBytes":0}));
            }
            let canonical_root = std::fs::canonicalize(dir.join("checkpoint"))?;
            for entry in current["candidates"].as_array().unwrap() {
                let path = PathBuf::from(entry["path"].as_str().unwrap());
                let resolved = std::fs::canonicalize(&path)?;
                if resolved == canonical_root || !resolved.starts_with(&canonical_root) {
                    return Err(invalid("检查点路径越界，未清理。"));
                }
                let (_, files) = footprint(&path, &ctx)?;
                if json!(files) != entry["files"] {
                    return Err(invalid("检查点内容已变化，请重新查看可清理内容。"));
                }
                if path.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
            Ok(json!({"removedCount":current["count"],"freedBytes":current["bytes"]}))
        })
    })
    .await
    .map_err(|_| invalid("检查点整理未完成。"))?
}
