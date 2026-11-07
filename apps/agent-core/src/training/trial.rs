//! 产物试用独占临时进程与记录，不替换聊天模型、不修改训练作业。
use super::{invalid, runtime, store};
use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    storage::managed_files,
    tools::gateway::GatewayToolContext,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
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
static ACTIVE: OnceLock<Mutex<HashMap<PathBuf, Active>>> = OnceLock::new();
fn active() -> &'static Mutex<HashMap<PathBuf, Active>> {
    ACTIVE.get_or_init(Default::default)
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut rows) = active().lock() {
            rows.remove(&self.0);
        }
        self.1.notify_waiters();
    }
}
fn terminal(value: &Value) -> bool {
    matches!(
        value["state"].as_str(),
        Some("completed" | "failed" | "cancelled" | "interrupted")
    )
}
fn directories(
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<(PathBuf, PathBuf), GatewayError> {
    let job = store::scope_dir(ctx)
        .join("jobs")
        .join(store::id(input, "jobId")?);
    let trial = job.join("trials").join(store::id(input, "trialId")?);
    Ok((job, trial))
}
fn update(
    ctx: &GatewayToolContext,
    dir: &Path,
    state: &str,
    message: &str,
) -> Result<(), GatewayError> {
    let mut value = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    value["state"] = json!(state);
    value["message"] = json!(message);
    value["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
    store::write(ctx, &dir.join("state.json"), &value)
}
pub(super) fn authorize_model(
    ctx: &GatewayToolContext,
    saved: &Value,
    dir: &Path,
) -> Result<(), GatewayError> {
    if !matches!(saved["algorithm"].as_str(), Some("text.sft" | "text.lora")) {
        return Err(invalid("这个产物不支持文本试聊。"));
    }
    let requested = saved["modelPath"]
        .as_str()
        .ok_or_else(|| invalid("训练记录没有底座模型路径。"))?;
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
            "请在训练台重新选择并授权底座模型目录。",
            403,
        ));
    }
    let artifact = dir
        .join("artifacts")
        .join(if saved["algorithm"] == "text.lora" {
            "adapter"
        } else {
            "model"
        });
    managed_files::inspect(&ctx.data_home, &artifact, false, false, u64::MAX)?;
    if !artifact.is_dir() || !canonical.is_dir() {
        return Err(invalid("本地模型或训练产物已移动，请确认文件位置。"));
    }
    Ok(())
}
fn request(input: &Value) -> Result<Value, GatewayError> {
    let mode = input["mode"].as_str().unwrap_or("trained");
    if !matches!(mode, "trained" | "base" | "compare") {
        return Err(invalid("试用方式无效。"));
    }
    let messages = input["messages"]
        .as_array()
        .filter(|m| !m.is_empty() && m.len() <= 64)
        .ok_or_else(|| invalid("请输入试聊内容，最多保留64条消息。"))?;
    if messages.iter().any(|m| {
        !matches!(m["role"].as_str(), Some("user" | "assistant"))
            || m["content"].as_str().is_none_or(|s| s.trim().is_empty())
    }) || messages.last().unwrap()["role"] != "user"
    {
        return Err(invalid("试聊消息格式无效，最后一条应为用户输入。"));
    }
    let tokens = input["maxNewTokens"].as_u64().unwrap_or(256);
    if !(1..=8192).contains(&tokens) {
        return Err(invalid("试用回复长度应为1至8192 token。"));
    }
    let value = json!({"mode":mode,"messages":messages,"maxNewTokens":tokens});
    if serde_json::to_vec(&value).unwrap().len() > 128 * 1024 {
        return Err(invalid("试聊内容超过128 KiB，请缩短内容。"));
    }
    Ok(value)
}
pub(super) fn read(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let (_, dir) = directories(input, ctx)?;
    let mut value = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    let registry = active().lock().map_err(|_| invalid("试用状态正在恢复。"))?;
    if !terminal(&value) {
        if let Some(owner) = registry.get(&dir) {
            if *owner.stop.borrow() {
                value["message"] = json!("正在取消本次试用。");
            }
        } else {
            value["state"] = json!("interrupted");
            value["message"] =
                json!("上次试用已中断，已生成结果保留。可以重新试用，训练作业不受影响。");
            store::write(ctx, &dir.join("state.json"), &value)?;
        }
    }
    Ok(json!({"trial":value}))
}
pub(super) fn cancel(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let (_, dir) = directories(input, ctx)?;
    let _ = store::read(ctx, &dir.join("state.json"), 1024 * 1024)?;
    if let Some(owner) = active()
        .lock()
        .map_err(|_| invalid("试用状态忙。"))?
        .get(&dir)
    {
        owner.stop.send_replace(true);
    }
    read(input, ctx)
}
pub(super) async fn start(
    input: &Value,
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    let job_id = store::id(input, "jobId")?;
    let trial_id = if input["trialId"].is_null() {
        uuid::Uuid::new_v4().to_string()
    } else {
        store::id(input, "trialId")?
    };
    let identity = json!({"jobId":job_id,"trialId":trial_id});
    let (job, dir) = directories(&identity, ctx)?;
    let state = store::read(ctx, &job.join("state.json"), 1024 * 1024)?;
    if state["state"] != "completed" {
        return Err(invalid("请先选择已完成训练的模型。"));
    }
    let saved = store::read(ctx, &job.join("config.json"), 1024 * 1024)?;
    authorize_model(ctx, &saved, &job)?;
    let request = request(input)?;
    let python = runtime::job_python(ctx, config, &job)?;
    let script = runtime::worker(ctx)?.with_file_name("trial_worker.py");
    loop {
        let previous = {
            let registry = active().lock().map_err(|_| invalid("试用状态忙。"))?;
            registry
                .iter()
                .find(|(key, _)| key.parent() == dir.parent())
                .map(|(key, owner)| (key.clone(), owner.done.clone()))
        };
        let Some((previous, done)) = previous else {
            break;
        };
        if !terminal(&store::read(
            ctx,
            &previous.join("state.json"),
            1024 * 1024,
        )?) {
            break;
        }
        let notified = done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if active().lock().is_ok_and(|rows| {
            rows.get(&previous)
                .is_some_and(|owner| Arc::ptr_eq(&owner.done, &done))
        }) {
            notified.await;
        }
    }
    let mut registry = active().lock().map_err(|_| invalid("试用状态忙。"))?;
    if dir.join("state.json").exists() {
        let old = store::read(ctx, &dir.join("request.json"), 256 * 1024)?;
        if old != request {
            return Err(invalid("这个试用标识已用于其他输入，请重新试用。"));
        }
        drop(registry);
        return read(&identity, ctx);
    }
    if registry.keys().any(|key| key.parent() == dir.parent()) {
        return Err(GatewayError::new(
            "TRAINING_TRIAL_BUSY",
            "这个模型仍在试用，可先取消当前试用。",
            409,
        ));
    }
    store::write(ctx, &dir.join("request.json"), &request)?;
    store::write(
        ctx,
        &dir.join("state.json"),
        &json!({"schemaVersion":1,"id":trial_id,"jobId":job_id,"state":"queued","mode":request["mode"],
        "message":"正在准备试用。","results":[],"createdAt":chrono::Utc::now().to_rfc3339()}),
    )?;
    let (stop, cancel) = watch::channel(false);
    let done = Arc::new(Notify::new());
    registry.insert(
        dir.clone(),
        Active {
            stop,
            done: done.clone(),
            home: ctx.data_home.clone(),
        },
    );
    let context = ctx.clone();
    tokio::spawn(async move {
        let _registration = Registration(dir.clone(), done);
        if let Err(error) = run(&context, &job, &dir, &python, &script, cancel).await {
            let _ = update(&context, &dir, "failed", &error.message);
        }
    });
    drop(registry);
    read(&identity, ctx)
}
pub(super) async fn probe(mut command: tokio::process::Command) -> Result<Value, GatewayError> {
    let output = tokio::time::timeout(Duration::from_secs(90), command.output())
        .await
        .map_err(|_| {
            GatewayError::new(
                "TRAINING_TRIAL_PREPARE_TIMEOUT",
                "试用环境检查超时，训练产物保留。",
                504,
            )
        })??;
    let value: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| invalid("试用环境返回格式无效。"))?;
    if !output.status.success() {
        return Err(GatewayError::new(
            value["error"]["code"]
                .as_str()
                .unwrap_or("TRAINING_TRIAL_FAILED"),
            value["error"]["message"]
                .as_str()
                .unwrap_or("试用环境检查失败。"),
            400,
        ));
    }
    Ok(value)
}
async fn run(
    ctx: &GatewayToolContext,
    job: &Path,
    dir: &Path,
    python: &Path,
    script: &Path,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), GatewayError> {
    if *cancel.borrow() {
        return update(ctx, dir, "cancelled", "已取消本次试用。");
    }
    update(ctx, dir, "preparing", "正在检查模型与试用环境。")?;
    let mut command = runtime::command(python, script);
    command
        .arg("prepare")
        .arg("--job-dir")
        .arg(job)
        .arg("--trial-dir")
        .arg(dir);
    let plan = tokio::select! {result=probe(command)=>result?,_=cancel.changed()=>return update(ctx,dir,"cancelled","已取消本次试用。")};
    let gpu = plan["gpuMemoryBytes"].as_u64().unwrap_or(0);
    if gpu > 0
        && plan["gpuTotalBytes"]
            .as_u64()
            .is_some_and(|total| gpu > total)
    {
        return Err(invalid(
            "这个模型的试用配置超过显卡容量，请使用CPU或较小的模型。",
        ));
    }
    store::write(ctx, &dir.join("plan.json"), &plan)?;
    update(ctx, dir, "queued", "正在等待空闲资源，聊天与训练继续运行。")?;
    let threads = plan["cpuThreads"].as_u64().unwrap_or(2);
    let lease = if let Some(service) = crate::resources::gateway::global() {
        let snapshot = service
            .request("snapshot", json!({}), Duration::from_secs(5))
            .await?;
        if plan["memoryBytes"].as_u64().unwrap_or(0)
            > snapshot["accounting"]["maximumResidentReservationBytes"]
                .as_u64()
                .unwrap_or(u64::MAX)
        {
            return Err(invalid("当前模型试用需要的内存超过可分配容量。"));
        }
        let id = dir.file_name().unwrap().to_string_lossy();
        match service.acquire_waiting(json!({"taskId":format!("training-trial:{id}"),"workspaceId":ctx.project_id.as_deref().unwrap_or(&ctx.conversation_id),
            "kind":"background","workload":"compute","cpuThreads":threads,"memoryBytes":plan["memoryBytes"],"gpuMemoryBytes":gpu,"ttlMs":30000,"waitMs":30000}),cancel.clone()).await {
            Ok(lease)=>Some(lease),Err(_) if *cancel.borrow()=>return update(ctx,dir,"cancelled","已取消资源等待。"),Err(error)=>return Err(error)
        }
    } else {
        None
    };
    if *cancel.borrow() {
        return update(ctx, dir, "cancelled", "已取消本次试用。");
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("worker.log"))?;
    let mut command = runtime::command(python, script);
    command
        .arg("run")
        .arg("--job-dir")
        .arg(job)
        .arg("--trial-dir")
        .arg(dir)
        .env("OMP_NUM_THREADS", threads.to_string())
        .env("MKL_NUM_THREADS", threads.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log));
    let mut child = command.spawn()?;
    let _tree = crate::tools::gateway_process::OwnedProcessJob::attach(&child)?;
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
            return Err(invalid("试用进程资源登记失败，请稍后重试。"));
        }
    }
    tokio::select! {
        exit=child.wait()=>{
            let _=exit?;
            let result=store::read(ctx,&dir.join("state.json"),1024*1024)?;
            if !terminal(&result){update(ctx,dir,"failed","试用进程已退出，已有结果保留。请查看运行日志。")?;}
        },
        _=cancel.changed()=>{let _=child.kill().await;let _=child.wait().await;update(ctx,dir,"cancelled","已取消本次试用，已有结果保留。")?;}
    }
    if let Some(lease) = lease {
        lease.release().await;
    }
    Ok(())
}
pub(crate) async fn shutdown(home: &Path) {
    let entries = active()
        .lock()
        .map(|rows| {
            rows.iter()
                .filter(|(_, a)| a.home == home)
                .map(|(dir, a)| (dir.clone(), a.stop.clone(), a.done.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (_, stop, _) in &entries {
        stop.send_replace(true);
    }
    for (dir, _, done) in entries {
        let notified = done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if active().lock().is_ok_and(|rows| rows.contains_key(&dir)) {
            let _ = tokio::time::timeout(Duration::from_secs(10), notified).await;
        }
    }
}
pub(crate) async fn shutdown_checked(home: &Path) -> Result<(), GatewayError> {
    shutdown(home).await;
    if active()
        .lock()
        .map_err(|_| invalid("试用状态忙。"))?
        .values()
        .any(|a| a.home == home)
    {
        return Err(GatewayError::new(
            "TRAINING_TRIAL_STOP_PENDING",
            "模型试用仍在停止，请稍后重试。",
            409,
        ));
    }
    Ok(())
}
