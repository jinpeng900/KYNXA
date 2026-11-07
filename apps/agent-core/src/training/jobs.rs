//! 训练作业独立于聊天流和预览，重复启动沿用原作业。
use super::{invalid, runtime, store};
use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    storage::managed_files,
    tools::gateway::GatewayToolContext,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::{Notify, watch};

struct Active {
    stop: watch::Sender<bool>,
    done: Arc<Notify>,
    home: PathBuf,
}
struct Registration(PathBuf, Arc<Notify>);
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut rows) = active().lock() {
            rows.remove(&self.0);
        }
        self.1.notify_waiters();
    }
}
static ACTIVE: OnceLock<Mutex<HashMap<PathBuf, Active>>> = OnceLock::new();
static CREATION: Mutex<()> = Mutex::new(());
fn active() -> &'static Mutex<HashMap<PathBuf, Active>> {
    ACTIVE.get_or_init(Default::default)
}
fn directory(ctx: &GatewayToolContext, input: &Value) -> Result<PathBuf, GatewayError> {
    Ok(store::scope_dir(ctx)
        .join("jobs")
        .join(store::id(input, "jobId")?))
}
fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "paused" | "stopped")
}
pub(super) fn list(ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let configs = store::list(ctx, "jobs", "config.json")?;
    let jobs = configs
        .into_iter()
        .map(|cfg| {
            read(&json!({"jobId":cfg["id"],"omitMetrics":true}), ctx).map(|v| v["job"].clone())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"jobs":jobs}))
}
pub(super) fn read(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let dir = directory(ctx, input)?;
    let config = store::read(ctx, &dir.join("config.json"), 1024 * 1024)?;
    let mut job = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    for key in [
        "id",
        "name",
        "createdAt",
        "backend",
        "algorithm",
        "datasetId",
        "modelPath",
        "parameters",
    ] {
        job[key] = config[key].clone();
    }
    super::records::overlay(ctx, &dir, &mut job)?;
    let running = active()
        .lock()
        .map_err(|_| invalid("训练状态正在恢复。"))?
        .contains_key(&dir);
    if !running
        && matches!(
            job["state"].as_str(),
            Some("queued" | "preparing" | "running" | "pausing" | "stopping")
        )
    {
        job["state"] = json!("paused");
        job["status"] = json!("paused");
        job["message"] =
            json!("上次运行已中断，原配置、指标和检查点已保留。可接回原作业，不会自动重做。");
        job["canResume"] = json!(true);
        store::write(ctx, &dir.join("state.json"), &job)?;
    }
    let (metrics, next, more) = if input["omitMetrics"] == true {
        (vec![], 0, false)
    } else {
        metrics(
            &dir,
            input["afterMetric"].as_u64().unwrap_or(0),
            input["metricLimit"].as_u64(),
            ctx,
        )?
    };
    let metric_length = std::fs::metadata(dir.join("metrics.jsonl"))
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(
        json!({"job":job,"metrics":metrics,"nextMetric":next,"metricLength":metric_length,"hasMoreMetrics":more,"artifacts":job["artifacts"]}),
    )
}
fn metrics(
    dir: &Path,
    offset: u64,
    limit: Option<u64>,
    ctx: &GatewayToolContext,
) -> Result<(Vec<Value>, u64, bool), GatewayError> {
    let path = dir.join("metrics.jsonl");
    managed_files::inspect(&ctx.data_home, &path, true, true, u64::MAX)?;
    if !path.exists() {
        return Ok((vec![], 0, false));
    }
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len().min(limit.unwrap_or(u64::MAX));
    let tail_start = size.saturating_sub(65536);
    file.seek(SeekFrom::Start(tail_start))?;
    let mut tail = vec![0u8; (size - tail_start) as usize];
    file.read_exact(&mut tail)?;
    let size = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|i| tail_start + i as u64 + 1)
        .unwrap_or(0);
    if offset > size {
        return Err(invalid("指标读取位置已变化，请重新读取。"));
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset))?;
    let mut rows = Vec::new();
    let mut cursor = offset;
    while rows.len() < 2000 && cursor - offset < 256 * 1024 {
        let mut line = String::new();
        let count = reader.read_line(&mut line)?;
        if count == 0 || !line.ends_with('\n') {
            break;
        }
        if cursor + count as u64 > size {
            break;
        }
        cursor += count as u64;
        if let Ok(row) = serde_json::from_str(&line) {
            rows.push(row);
        }
    }
    Ok((rows, cursor, cursor < size))
}
pub(super) fn create(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let _creation = CREATION.lock().map_err(|_| invalid("训练配置正在保存。"))?;
    let dataset_id = store::id(input, "datasetId")?;
    let dataset = store::read(
        ctx,
        &store::scope_dir(ctx)
            .join("datasets")
            .join(&dataset_id)
            .join("dataset.json"),
        store::RECORD_LIMIT,
    )?;
    if dataset["samples"].as_array().is_none_or(|v| v.is_empty()) {
        return Err(invalid("请先准备非空训练数据集。"));
    }
    let backend = input["backend"].as_str().unwrap_or("builtin");
    if !matches!(
        backend,
        "builtin" | "sklearn" | "torch" | "transformers" | "peft" | "trl"
    ) {
        return Err(invalid("训练后端无效。"));
    }
    let algorithm = input["algorithm"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() < 100)
        .ok_or_else(|| invalid("请选择训练算法。"))?;
    let parameters = input
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !parameters.is_object() || serde_json::to_vec(&parameters).unwrap_or_default().len() > 65536
    {
        return Err(invalid("训练参数无效。"));
    }
    let id = if let Some(key) = input["idempotencyKey"].as_str() {
        let identity = managed_files::digest(
            serde_json::to_vec(&json!({"key":key,"input":input}))
                .unwrap()
                .as_slice(),
        );
        uuid::Uuid::from_bytes(<[u8; 16]>::try_from(&identity.as_bytes()[..16]).unwrap())
            .to_string()
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    let dir = store::scope_dir(ctx).join("jobs").join(&id);
    if dir.join("config.json").exists() {
        return read(&json!({"jobId":id}), ctx);
    }
    let model_path = authorize_model(input, ctx)?;
    let config = json!({"schemaVersion":1,"id":id,"name":input["name"].as_str().unwrap_or("训练实验"),"datasetId":dataset_id,
        "backend":backend,"algorithm":algorithm,"modelPath":model_path,"task":input["task"].as_str().unwrap_or(dataset["task"].as_str().unwrap_or("classification")),
        "parameters":parameters,"createdAt":chrono::Utc::now().to_rfc3339(),"dataVersion":dataset["version"]});
    store::write(ctx, &dir.join("dataset.json"), &dataset)?;
    store::write(ctx, &dir.join("config.json"), &config)?;
    state(ctx, &dir, "ready", "配置与数据版本已保存，尚未开始训练。")?;
    read(&json!({"jobId":id}), ctx)
}
pub(super) fn authorize_model(
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let Some(requested) = input["modelPath"].as_str().filter(|v| !v.is_empty()) else {
        return Ok(Value::Null);
    };
    let canonical = std::fs::canonicalize(requested)?;
    let registered = store::list(ctx, "models", "model.json")?.iter().any(|m| {
        m["path"]
            .as_str()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .is_some_and(|p| p == canonical)
    });
    let (_, outside) = crate::tools::gateway::canonical_authorized(ctx, requested, false)?;
    if crate::tools::gateway::protected_path(ctx, &canonical)
        || outside && !registered && !ctx.approved
    {
        return Err(GatewayError::new(
            "TRAINING_MODEL_FORBIDDEN",
            "请通过训练台选择器授权这个模型目录。",
            403,
        ));
    }
    if !canonical.is_dir() || !canonical.join("config.json").is_file() {
        return Err(invalid(
            "训练需要含config.json的可训练权重目录，不能直接使用推理GGUF文件。",
        ));
    }
    Ok(json!(canonical))
}
fn state(
    ctx: &GatewayToolContext,
    dir: &Path,
    value: &str,
    message: &str,
) -> Result<(), GatewayError> {
    let mut record = store::read(ctx, &dir.join("state.json"), 1024 * 1024)
        .unwrap_or_else(|_| json!({"schemaVersion":1,"step":0,"epoch":0,"artifacts":[]}));
    record["state"] = json!(value);
    record["status"] = json!(value);
    record["message"] = json!(message);
    record["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
    store::write(ctx, &dir.join("state.json"), &record)
}
pub(super) fn control(
    action: &str,
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let dir = directory(ctx, input)?;
    let _ = store::read(ctx, &dir.join("config.json"), 1024 * 1024)?;
    if store::read(ctx, &dir.join("state.json"), 1024 * 1024)?["state"] == "completed" {
        return read(input, ctx);
    }
    let mode = if action.ends_with("stop") {
        "stop"
    } else {
        "pause"
    };
    store::write(ctx, &dir.join("control.json"), &json!({"action":mode}))?;
    if let Some(active) = active()
        .lock()
        .map_err(|_| invalid("训练状态忙。"))?
        .get(&dir)
    {
        active.stop.send_replace(true);
    } else {
        state(
            ctx,
            &dir,
            if mode == "stop" { "stopped" } else { "paused" },
            "运行已停止，已保存内容保留。",
        )?;
    }
    read(input, ctx)
}
pub(super) async fn start(
    input: &Value,
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    let dir = directory(ctx, input)?;
    let saved = store::read(ctx, &dir.join("config.json"), 1024 * 1024)?;
    let prior = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    if prior["state"] == "completed" {
        return read(input, ctx);
    }
    // 暂停结果可能早于旧进程退出，续训等待其交还所有权，不把用户指令吞掉。
    loop {
        let wait = {
            let rows = active().lock().map_err(|_| invalid("训练状态忙。"))?;
            match rows.get(&dir) {
                Some(owner)
                    if matches!(
                        store::read(ctx, &dir.join("state.json"), 1024 * 1024)?["state"].as_str(),
                        Some("paused" | "stopped" | "failed")
                    ) =>
                {
                    Some(owner.done.clone())
                }
                Some(_) => {
                    drop(rows);
                    return read(input, ctx);
                }
                None => None,
            }
        };
        let Some(done) = wait else {
            break;
        };
        let notified = done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let same_owner = active()
            .lock()
            .is_ok_and(|rows| rows.get(&dir).is_some_and(|a| Arc::ptr_eq(&a.done, &done)));
        if same_owner {
            notified.await;
        }
    }
    let python = runtime::job_python(ctx, config, &dir)?;
    let script = runtime::worker(ctx)?;
    let mut registry = active().lock().map_err(|_| invalid("训练状态忙。"))?;
    if registry.contains_key(&dir) {
        drop(registry);
        return read(input, ctx);
    }
    let (stop, cancel) = watch::channel(false);
    let done = Arc::new(Notify::new());
    store::write(ctx, &dir.join("control.json"), &json!({"action":"run"}))?;
    state(ctx, &dir, "queued", "正在等待训练资源。")?;
    registry.insert(
        dir.clone(),
        Active {
            stop,
            done: done.clone(),
            home: ctx.data_home.clone(),
        },
    );
    let scope = ctx.clone();
    let job_dir = dir.clone();
    tokio::spawn(async move {
        let _registration = Registration(job_dir.clone(), done);
        let result = run(&job_dir, &saved, &scope, &python, &script, cancel).await;
        if let Err(error) = result {
            let _ = state(&scope, &job_dir, "failed", &error.message);
        }
    });
    drop(registry);
    read(input, ctx)
}
async fn run(
    dir: &Path,
    config: &Value,
    ctx: &GatewayToolContext,
    python: &Path,
    script: &Path,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), GatewayError> {
    state(ctx, dir, "preparing", "正在检查训练依赖、模型和实际设备。")?;
    let prepared = runtime::prepare(python, script, dir).await?;
    let plan = super::resources::plan(config, dir, &prepared)?;
    if prepared["gpuTotalBytes"]
        .as_u64()
        .is_some_and(|total| total > 0 && plan["gpuMemoryBytes"].as_u64().unwrap_or(0) > total)
    {
        return Err(GatewayError::new(
            "TRAINING_CONFIGURATION_TOO_LARGE",
            "当前训练配置预计超出显卡总显存，请降低批量或训练范围，或改用量化LoRA。",
            400,
        ));
    }
    let threads = plan["cpuThreads"].as_u64().unwrap_or(2);
    let mut snapshot = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    snapshot["modelStructure"] = prepared["modelStructure"].clone();
    snapshot["resourcePlan"] = plan.clone();
    store::write(ctx, &dir.join("state.json"), &snapshot)?;
    state(
        ctx,
        dir,
        "queued",
        "正在等待训练资源，已有配置和检查点保留。",
    )?;
    let lease = if let Some(service) = crate::resources::gateway::global() {
        let hardware = service
            .request("snapshot", json!({}), Duration::from_secs(5))
            .await?;
        let maximum = hardware["accounting"]["maximumResidentReservationBytes"]
            .as_u64()
            .unwrap_or(u64::MAX);
        if plan["memoryBytes"].as_u64().unwrap_or(0) > maximum {
            return Err(GatewayError::new(
                "TRAINING_CONFIGURATION_TOO_LARGE",
                "当前训练配置超出本机可分配内存，请减少层宽、批量或解冻范围，或改用LoRA；不会一直排队等待不可能满足的配置。",
                400,
            ));
        }
        match service.acquire_waiting(json!({"taskId":format!("training:{}",config["id"].as_str().unwrap_or("")),"workspaceId":ctx.project_id.as_deref().unwrap_or(&ctx.conversation_id),
            "kind":"background","workload":"compute","cpuThreads":threads,"memoryBytes":plan["memoryBytes"],"gpuMemoryBytes":plan["gpuMemoryBytes"],"ttlMs":30000,"waitMs":30000}),cancel.clone()).await {
            Ok(lease)=>Some(lease),Err(_) if *cancel.borrow()=>{state(ctx,dir,"paused","已取消资源等待，配置和已有进度保留。")?;return Ok(());},Err(e)=>return Err(e)
        }
    } else {
        None
    };
    if *cancel.borrow() {
        state(ctx, dir, "paused", "已暂停。")?;
        return Ok(());
    }
    runtime::bind_job_python(ctx, dir, python)?;
    state(ctx, dir, "preparing", "正在校验训练配置和数据。")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("worker.log"))?;
    let mut command = runtime::command(python, script);
    command
        .arg("run")
        .arg("--job-dir")
        .arg(dir)
        .env("OMP_NUM_THREADS", threads.to_string())
        .env("MKL_NUM_THREADS", threads.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log));
    let mut child = command.spawn().map_err(|_| {
        GatewayError::new(
            "TRAINING_START_FAILED",
            "训练进程无法启动，请检查Python环境。",
            503,
        )
    })?;
    let _process_tree = crate::tools::gateway_process::OwnedProcessJob::attach(&child)?;
    if let (Some(service), Some(lease)) = (crate::resources::gateway::global(), lease.as_ref()) {
        let registered = service
            .request(
                "registerExecutor",
                json!({"leaseId":lease.allocation["leaseId"],"processId":child.id()}),
                Duration::from_secs(5),
            )
            .await?;
        if registered["status"] != "registered" {
            child.kill().await?;
            return Err(GatewayError::new(
                "TRAINING_RESOURCE_TRACKING_FAILED",
                "训练进程未完成资源登记，原作业保留。",
                503,
            ));
        }
    }
    let exit = loop {
        tokio::select! {
            exit=child.wait()=>{
                break exit?;
            },
            changed=cancel.changed()=>{
                if changed.is_err() || *cancel.borrow(){
                    // The worker consumes the persisted pause/stop at a safe checkpoint boundary.
                    break child.wait().await?;
                }
            }
        }
    };
    let result = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    if !terminal(result["state"].as_str().unwrap_or("")) {
        state(
            ctx,
            dir,
            if exit.success() { "paused" } else { "failed" },
            "训练进程退出，已保存数据保留。请查看运行日志。",
        )?;
    }
    if let Some(lease) = lease {
        lease.release().await;
    }
    Ok(())
}
pub(crate) async fn shutdown(home: &Path) {
    let entries = active()
        .lock()
        .map(|m| {
            m.iter()
                .filter(|(_, a)| a.home == home)
                .map(|(d, a)| (d.clone(), a.stop.clone(), a.done.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (dir, stop, _) in &entries {
        let bytes = serde_json::to_vec(&json!({"action":"pause"})).unwrap();
        let _ = managed_files::atomic_write(home, &dir.join("control.json"), &bytes, 65536);
        stop.send_replace(true);
    }
    for (dir, _, done) in entries {
        let notified = done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let pending = active().lock().is_ok_and(|m| m.contains_key(&dir));
        if pending {
            let _ = tokio::time::timeout(Duration::from_secs(30), notified).await;
        }
    }
}
// Keep start and maintenance mutually exclusive. / 清理与启动共用所有权锁。
pub(super) fn while_inactive<T>(
    dir: &Path,
    operation: impl FnOnce() -> Result<T, GatewayError>,
) -> Result<T, GatewayError> {
    let registry = active().lock().map_err(|_| invalid("训练状态忙。"))?;
    if registry.contains_key(dir) {
        return Err(invalid("训练运行时不能清理检查点。"));
    }
    operation()
}
/// 删除数据集与作业创建、注册共用锁，校验期间不会有新训练启动。
pub(super) fn while_dataset_idle<T>(
    ctx: &GatewayToolContext,
    dataset: &str,
    operation: impl FnOnce() -> Result<T, GatewayError>,
) -> Result<T, GatewayError> {
    let _creation = CREATION.lock().map_err(|_| invalid("训练配置正在保存。"))?;
    let registry = active().lock().map_err(|_| invalid("训练状态忙。"))?;
    let scope = store::scope_dir(ctx).join("jobs");
    for dir in registry.keys().filter(|dir| dir.starts_with(&scope)) {
        if store::read(ctx, &dir.join("config.json"), 1024 * 1024)?["datasetId"] == dataset {
            return Err(GatewayError::new(
                "TRAINING_DATASET_IN_USE",
                "这份数据集正在训练，暂停或结束后才能删除。",
                409,
            ));
        }
    }
    operation()
}
pub(crate) async fn shutdown_checked(home: &Path) -> Result<(), GatewayError> {
    shutdown(home).await;
    if active()
        .lock()
        .map_err(|_| invalid("训练状态忙。"))?
        .values()
        .any(|a| a.home == home)
    {
        return Err(GatewayError::new(
            "TRAINING_SAVE_PENDING",
            "训练仍在保存检查点，存储位置暂未移动。",
            409,
        ));
    }
    Ok(())
}
