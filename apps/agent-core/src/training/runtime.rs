//! 版本化训练执行器与独立Python环境，不复用聊天请求进程。
use super::{invalid, store};
use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    storage::managed_files,
    tools::gateway::GatewayToolContext,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

const FILES: &[(&str, &str)] = &[
    (
        "training_worker.py",
        include_str!("../../training-runtime/training_worker.py"),
    ),
    (
        "builtin_backend.py",
        include_str!("../../training-runtime/builtin_backend.py"),
    ),
    (
        "sklearn_backend.py",
        include_str!("../../training-runtime/sklearn_backend.py"),
    ),
    (
        "vision_backend.py",
        include_str!("../../training-runtime/vision_backend.py"),
    ),
    (
        "transformer_backend.py",
        include_str!("../../training-runtime/transformer_backend.py"),
    ),
    (
        "prediction.py",
        include_str!("../../training-runtime/prediction.py"),
    ),
    (
        "environment_setup.py",
        include_str!("../../training-runtime/environment_setup.py"),
    ),
    (
        "trial_worker.py",
        include_str!("../../training-runtime/trial_worker.py"),
    ),
];
pub(super) fn worker(ctx: &GatewayToolContext) -> Result<PathBuf, GatewayError> {
    let fingerprint = managed_files::digest(
        FILES
            .iter()
            .flat_map(|(_, text)| text.as_bytes().iter().copied())
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let dir = ctx.data_home.join("Runtime/training").join(fingerprint);
    for (name, text) in FILES {
        let file = dir.join(name);
        if managed_files::read(&ctx.data_home, &file, 4 * 1024 * 1024)?.as_deref()
            != Some(text.as_bytes())
        {
            managed_files::atomic_write(&ctx.data_home, &file, text.as_bytes(), 4 * 1024 * 1024)?;
        }
    }
    Ok(dir.join("training_worker.py"))
}
pub(super) fn python(ctx: &GatewayToolContext, config: &GatewayConfig) -> Option<PathBuf> {
    let choices = [
        store::read(ctx, &store::scope_dir(ctx).join("environment.json"), 65536)
            .ok()
            .and_then(|v| v["path"].as_str().map(PathBuf::from)),
        std::env::var_os("KYNXA_TRAINING_PYTHON").map(PathBuf::from),
        Some(
            ctx.data_home
                .join("Runtime/training-env/Scripts/python.exe"),
        ),
        Some(config.application_root.join("runtime/training/python.exe")),
    ];
    choices
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| {
                    p.join(if cfg!(windows) {
                        "python.exe"
                    } else {
                        "python3"
                    })
                })
                .find(|p| p.is_file() && !p.to_string_lossy().contains("WindowsApps"))
        })
}
pub(super) fn command(python: &Path, script: &Path) -> Command {
    let mut command = Command::new(python);
    command
        .arg("-I")
        .arg("-B")
        .arg("-u")
        .arg(script)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // Import only these versioned sibling modules; -I otherwise omits the script directory.
    command.env("KYNXA_TRAINING_OWNER_PID", std::process::id().to_string());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000 | 0x0000_4000);
    command
}
pub(super) async fn capabilities(
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    let mut result = if let Some(python) = python(ctx, config) {
        let script = worker(ctx)?;
        let mut command = command(&python, &script);
        command.arg("capabilities");
        match tokio::time::timeout(Duration::from_secs(45), command.output()).await {
            Ok(Ok(out)) if out.status.success() => serde_json::from_slice::<Value>(&out.stdout)
                .map_err(|_| {
                    GatewayError::new("TRAINING_PROTOCOL_INVALID", "训练环境返回格式无效。", 502)
                })?,
            _ => {
                json!({"available":false,"backends":[],"reason":"训练环境未能完成能力检查，请检查已选择的Python环境。"})
            }
        }
    } else {
        json!({"available":false,"backends":[],"reason":"尚未找到训练Python。可在训练台指定Python，训练依赖独立管理。"})
    };
    result["environment"] = json!({"available":python(ctx,config).is_some(),"pythonPath":python(ctx,config),"variable":"KYNXA_TRAINING_PYTHON","localOnly":true});
    if let Ok(selected) = store::read(ctx, &store::scope_dir(ctx).join("environment.json"), 65536) {
        if selected["path"] == result["environment"]["pythonPath"] && selected["probe"].is_object()
        {
            result["environment"]["verifiedCapabilities"] = selected["probe"].clone();
        }
    }
    let mut models = store::list(ctx, "models", "model.json")?;
    if let Some(Ok((_, snapshot))) = crate::models::gateway_local_models::handle(
        "GET",
        "/api/local-models",
        json!({}),
        &config.data_home,
    )
    .await
    {
        for item in snapshot["models"].as_array().into_iter().flatten() {
            if item["path"].is_string() {
                let mut item = item.clone();
                item["trainable"] = json!(false);
                item["trainingReason"] =
                    json!("GGUF为现有推理格式，请选择匹配的可训练模型目录。原模型不被覆盖。");
                models.push(item);
            }
        }
    }
    result["models"] = json!(models);
    Ok(result)
}
pub(super) fn require_python(
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<PathBuf, GatewayError> {
    python(ctx, config).ok_or_else(|| invalid("请先在训练台配置可用的Python环境。"))
}
/// 作业固定使用首次解释器；切换训练台环境不改冻结输入或已有检查点。
pub(super) fn job_python(
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
    dir: &Path,
) -> Result<PathBuf, GatewayError> {
    let record = binding_record(ctx, dir)?;
    let _lock = super::CONFIGURATION
        .lock()
        .map_err(|_| invalid("训练配置正在保存。"))?;
    if let Some(path) = bound_python(ctx, &record)? {
        return Ok(path);
    }
    let path = std::path::absolute(require_python(ctx, config)?)?;
    Ok(path)
}
fn binding_record(ctx: &GatewayToolContext, dir: &Path) -> Result<PathBuf, GatewayError> {
    let jobs = store::scope_dir(ctx).join("jobs");
    if dir.parent() != Some(jobs.as_path())
        || dir
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| uuid::Uuid::parse_str(name).ok())
            .is_none()
    {
        return Err(invalid("作业运行环境的归属无效。"));
    }
    Ok(dir.join("runtime.json"))
}
fn bound_python(ctx: &GatewayToolContext, record: &Path) -> Result<Option<PathBuf>, GatewayError> {
    if let Some(bytes) = managed_files::read(&ctx.data_home, record, 65536)? {
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            GatewayError::new(
                "TRAINING_JOB_ENVIRONMENT_INVALID",
                "作业环境记录无法读取，原记录已保留。",
                409,
            )
        })?;
        let path = value["pythonPath"]
            .as_str()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                GatewayError::new(
                    "TRAINING_JOB_ENVIRONMENT_INVALID",
                    "作业环境记录缺少有效解释器路径。",
                    409,
                )
            })?;
        if !path.is_file() {
            return Err(GatewayError::new(
                "TRAINING_JOB_ENVIRONMENT_MISSING",
                "此作业使用的训练环境已移除，请先恢复原环境。作业、指标和检查点均已保留。",
                409,
            ));
        }
        return Ok(Some(path));
    }
    Ok(None)
}
fn save_binding(
    ctx: &GatewayToolContext,
    dir: &Path,
    record: &Path,
    python: &Path,
) -> Result<(), GatewayError> {
    if !python.is_absolute() || !python.is_file() {
        return Err(invalid("已检查的训练解释器不再可用，原作业保留。"));
    }
    store::read(ctx, &dir.join("config.json"), 1024 * 1024)?;
    store::write(
        ctx,
        record,
        &json!({"schemaVersion":1,"pythonPath":python,"pinnedAt":chrono::Utc::now().to_rfc3339()}),
    )
}
/// 依赖和资源准入通过后再绑定已捕获解释器，首次准备失败不锁死作业。
pub(super) fn bind_job_python(
    ctx: &GatewayToolContext,
    dir: &Path,
    python: &Path,
) -> Result<(), GatewayError> {
    let record = binding_record(ctx, dir)?;
    let _lock = super::CONFIGURATION
        .lock()
        .map_err(|_| invalid("训练配置正在保存。"))?;
    if let Some(existing) = bound_python(ctx, &record)? {
        if existing != python {
            return Err(GatewayError::new(
                "TRAINING_JOB_ENVIRONMENT_CHANGED",
                "作业已有固定环境，不能隐式更换解释器。",
                409,
            ));
        }
        return Ok(());
    }
    save_binding(ctx, dir, &record, python)
}
pub(super) async fn prepare(
    python: &Path,
    script: &Path,
    dir: &Path,
) -> Result<Value, GatewayError> {
    let mut cmd = command(python, script);
    cmd.arg("prepare").arg("--job-dir").arg(dir);
    let out = tokio::time::timeout(Duration::from_secs(90), cmd.output())
        .await
        .map_err(|_| {
            GatewayError::new(
                "TRAINING_PREPARATION_TIMEOUT",
                "训练配置检查超时，原作业已保存。",
                504,
            )
        })??;
    let result = serde_json::from_slice::<Value>(&out.stdout).map_err(|_| {
        GatewayError::new(
            "TRAINING_PROTOCOL_INVALID",
            "训练配置检查未返回有效结果。",
            502,
        )
    })?;
    if !out.status.success() {
        return Err(GatewayError::new(
            result["error"]["code"]
                .as_str()
                .unwrap_or("TRAINING_PREPARATION_FAILED"),
            result["error"]["message"]
                .as_str()
                .or_else(|| result["error"].as_str())
                .unwrap_or("训练配置或依赖未就绪，请查看训练环境与参数。"),
            400,
        ));
    }
    Ok(result)
}
