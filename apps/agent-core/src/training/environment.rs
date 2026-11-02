//! 训练台独立版本环境；正在执行的作业保留原解释器，不升级原目录。
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
    time::{Duration, Instant},
};
use tokio::sync::{Notify, Semaphore, watch};
#[path = "environment_download.rs"]
mod download;
#[path = "environment_process.rs"]
mod process;

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
static INSTALLS: Semaphore = Semaphore::const_new(1);
static HARDWARE: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();
fn active() -> &'static Mutex<HashMap<PathBuf, Active>> {
    ACTIVE.get_or_init(Default::default)
}
#[derive(Clone)]
struct Setup {
    context: GatewayToolContext,
    state: PathBuf,
    directory: PathBuf,
    id: String,
    lease: Option<String>,
    previous: Option<Vec<u8>>,
}
fn state_path(ctx: &GatewayToolContext) -> PathBuf {
    store::scope_dir(ctx).join("environment-setup.json")
}
fn running(value: &Value) -> bool {
    matches!(
        value["state"].as_str(),
        Some("queued" | "preparing" | "cancelling")
    )
}
fn cancel_error() -> GatewayError {
    GatewayError::new(
        "TRAINING_ENV_CANCELLED",
        "环境准备已取消，原环境保留。",
        499,
    )
}
fn cancelled(stop: &watch::Receiver<bool>) -> Result<(), GatewayError> {
    if *stop.borrow() {
        Err(cancel_error())
    } else {
        Ok(())
    }
}

fn patch(setup: &Setup, values: Value) -> Result<Value, GatewayError> {
    let mut current = store::read(&setup.context, &setup.state, 128 * 1024)?;
    if current["id"] != setup.id {
        return Err(invalid("环境准备记录已变化。"));
    }
    for (key, value) in values.as_object().into_iter().flatten() {
        current[key] = value.clone();
    }
    current["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
    store::write(&setup.context, &setup.state, &current)?;
    Ok(current)
}
fn update(setup: &Setup, stage: Option<&str>, line: Option<&str>) -> Result<(), GatewayError> {
    let mut values = json!({});
    if let Some(stage) = stage {
        values["stage"] = json!(stage);
        values["downloadedBytes"] = Value::Null;
        values["totalBytes"] = Value::Null;
    }
    if let Some(line) = line.filter(|s| !s.is_empty()) {
        let mut rows = store::read(&setup.context, &setup.state, 128 * 1024)?["logs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for line in line.lines().filter(|s| !s.trim().is_empty()).take(8) {
            let text = line
                .chars()
                .filter(|c| !c.is_control() || *c == '\t')
                .take(512)
                .collect::<String>();
            rows.push(json!(text));
        }
        if rows.len() > 48 {
            rows.drain(..rows.len() - 48);
        }
        values["logs"] = json!(rows);
    }
    patch(setup, values)?;
    Ok(())
}

pub(super) async fn handle(
    action: &str,
    input: &Value,
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    match action {
        "environment.read" => read(ctx, config).await,
        "environment.prepare" => prepare(input, ctx, config).await,
        "environment.cancel" => cancel(ctx, config).await,
        _ => Err(invalid("未知环境操作。")),
    }
}

async fn hardware() -> Value {
    let cache = HARDWARE.get_or_init(Default::default);
    if let Some(value) = cache.lock().ok().and_then(|v| {
        v.as_ref()
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(60))
            .map(|(_, v)| v.clone())
    }) {
        return value;
    }
    let mut result = json!({"logicalCores":std::thread::available_parallelism().map(|v|v.get()).unwrap_or(1),"gpus":[],"nvidiaDetected":false});
    if let Some(service) = crate::resources::gateway::global() {
        if let Ok(snapshot) = service
            .request("snapshot", json!({}), Duration::from_secs(3))
            .await
        {
            result["resources"] = snapshot;
        }
    }
    let candidate = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| {
            p.join(if cfg!(windows) {
                "nvidia-smi.exe"
            } else {
                "nvidia-smi"
            })
        })
        .find(|p| p.is_file())
        .or_else(|| {
            std::env::var_os("SystemRoot")
                .map(|p| PathBuf::from(p).join("System32/nvidia-smi.exe"))
                .filter(|p| p.is_file())
        });
    if let Some(program) = candidate {
        let mut command = tokio::process::Command::new(program);
        command
            .args([
                "--query-gpu=name,memory.total,driver_version",
                "--format=csv,noheader,nounits",
            ])
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(3), command.output()).await
        {
            if output.status.success() {
                let rows=String::from_utf8_lossy(&output.stdout).lines().take(16).filter_map(|line| {
                    let cells=line.split(',').map(str::trim).collect::<Vec<_>>();
                    (cells.len()==3).then(||json!({"name":cells[0],"memoryBytes":cells[1].parse::<u64>().unwrap_or(0)*1024*1024,"driver":cells[2]}))
                }).collect::<Vec<_>>();
                result["nvidiaDetected"] = json!(!rows.is_empty());
                result["gpus"] = json!(rows);
            }
        }
    }
    if let Ok(mut cached) = cache.lock() {
        *cached = Some((Instant::now(), result.clone()));
    }
    result
}

async fn read(ctx: &GatewayToolContext, config: &GatewayConfig) -> Result<Value, GatewayError> {
    let state = state_path(ctx);
    let setup = {
        let rows = active().lock().map_err(|_| invalid("环境状态正在恢复。"))?;
        let mut setup = store::read(ctx, &state, 128 * 1024)
            .unwrap_or_else(|_| json!({"state":"idle","stage":"idle","logs":[]}));
        if running(&setup) && !rows.contains_key(&state) {
            setup["state"] = json!("interrupted");
            setup["stage"] = json!("interrupted");
            setup["message"] = json!("上次环境准备已中断，原环境仍可使用。可重新准备。");
            store::write(ctx, &state, &setup)?;
        }
        setup
    };
    Ok(
        json!({"setup":setup,"pythonPath":runtime::python(ctx,config),"device":hardware().await,
        "backends":["builtin","sklearn","torch","transformers","peft","trl"],
        "sources":[{"name":"Python / uv","url":"https://github.com/astral-sh"},{"name":"依赖包","url":"https://pypi.org"},{"name":"PyTorch","url":"https://download.pytorch.org"}],
        "managedRoot":ctx.data_home.join("Runtime/training-envs")}),
    )
}

async fn prepare(
    input: &Value,
    ctx: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    let backend = input["backend"].as_str().unwrap_or("builtin");
    if !matches!(
        backend,
        "builtin" | "sklearn" | "torch" | "transformers" | "peft" | "trl"
    ) {
        return Err(invalid("请选择支持的训练后端。"));
    }
    let device = input["device"].as_str().unwrap_or("auto");
    if !matches!(device, "auto" | "cpu" | "cuda") {
        return Err(invalid("训练设备应为 auto、cpu 或 cuda。"));
    }
    let state = state_path(ctx);
    let previous = {
        let _guard = super::CONFIGURATION
            .lock()
            .map_err(|_| invalid("训练配置正在保存。"))?;
        managed_files::read(
            &ctx.data_home,
            &store::scope_dir(ctx).join("environment.json"),
            65536,
        )?
    };
    let (stop, receiver) = watch::channel(false);
    let done = Arc::new(Notify::new());
    let id = uuid::Uuid::new_v4().to_string();
    let directory = ctx.data_home.join("Runtime/training-envs").join(&id);
    let initial = json!({"id":id,"state":"queued","stage":"queued","backend":backend,"device":device,
        "environmentPath":directory,"logs":[],"createdAt":chrono::Utc::now().to_rfc3339(),"message":"等待准备独立训练环境。"});
    {
        let mut rows = active().lock().map_err(|_| invalid("环境状态正在恢复。"))?;
        if rows.contains_key(&state) {
            return Ok(json!({"setup":store::read(ctx,&state,128*1024)?,"reused":true}));
        }
        store::write(ctx, &state, &initial)?;
        rows.insert(
            state.clone(),
            Active {
                stop,
                done: done.clone(),
                home: ctx.data_home.clone(),
            },
        );
    }
    let context = ctx.clone();
    let config = config.clone();
    let backend = backend.to_owned();
    let device = device.to_owned();
    tokio::spawn(async move {
        let _registration = Registration(state.clone(), done);
        let mut setup = Setup {
            context,
            state,
            directory,
            id,
            lease: None,
            previous,
        };
        let result = run(&mut setup, &config, &backend, &device, receiver.clone()).await;
        if let Err(error) = result {
            let cancelled = *receiver.borrow() || error.code == "TRAINING_ENV_CANCELLED";
            let _ = patch(
                &setup,
                json!({"state":if cancelled{"cancelled"}else{"failed"},"stage":if cancelled{"cancelled"}else{"failed"},
                "message":if cancelled{"环境准备已取消，原环境保留。"}else{error.message.as_str()},"errorCode":error.code}),
            );
        }
    });
    Ok(json!({"setup":initial,"reused":false}))
}

async fn cancel(ctx: &GatewayToolContext, config: &GatewayConfig) -> Result<Value, GatewayError> {
    {
        let rows = active().lock().map_err(|_| invalid("环境状态正在恢复。"))?;
        if let Some(active) = rows.get(&state_path(ctx)) {
            let mut state = store::read(ctx, &state_path(ctx), 128 * 1024)?;
            if running(&state) {
                active.stop.send_replace(true);
                state["state"] = json!("cancelling");
                state["message"] = json!("正在取消环境准备。");
                store::write(ctx, &state_path(ctx), &state)?;
            }
        }
    }
    read(ctx, config).await
}

async fn run(
    setup: &mut Setup,
    config: &GatewayConfig,
    backend: &str,
    device: &str,
    mut stop: watch::Receiver<bool>,
) -> Result<(), GatewayError> {
    let _slot = tokio::select! {permit=INSTALLS.acquire()=>permit.map_err(|_|invalid("安装队列已关闭。"))?,_ = stop.changed()=>return Err(cancel_error())};
    cancelled(&stop)?;
    let mut lease = None;
    if let Some(service) = crate::resources::gateway::global() {
        update(
            setup,
            Some("waiting-resources"),
            Some("等待后台资源，聊天继续正常运行。"),
        )?;
        let acquired=service.acquire_waiting(json!({"taskId":format!("training-environment:{}",setup.id),
            "workspaceId":setup.context.project_id.as_deref().unwrap_or(&setup.context.conversation_id),
            "kind":"background","workload":"compute","cpuThreads":2,"memoryBytes":256*1024*1024,"gpuMemoryBytes":0,"ttlMs":30000,"waitMs":30000}),stop.clone()).await?;
        setup.lease = acquired.allocation["leaseId"].as_str().map(str::to_owned);
        lease = Some(acquired);
    }
    patch(
        setup,
        json!({"state":"preparing","message":"正在准备独立训练环境。"}),
    )?;
    let result = install(setup, config, backend, device, stop).await;
    if let Some(lease) = lease {
        lease.release().await;
    }
    result
}

async fn install(
    setup: &Setup,
    config: &GatewayConfig,
    backend: &str,
    device: &str,
    stop: watch::Receiver<bool>,
) -> Result<(), GatewayError> {
    managed_files::inspect(
        &setup.context.data_home,
        &setup.directory,
        true,
        false,
        u64::MAX,
    )?;
    let base = if let Some(path) = runtime::python(&setup.context, config) {
        let mut probe = process::command(&path, &setup.directory);
        probe.args([
            "-I",
            "-B",
            "-c",
            "import sys; print(str(sys.version_info.major)+'.'+str(sys.version_info.minor))",
        ]);
        let compatible = process::run(setup, probe, stop.clone(), Duration::from_secs(15), false)
            .await
            .map(|v| matches!(v.trim(), "3.11" | "3.12" | "3.13"))
            .unwrap_or(false);
        if compatible {
            path
        } else {
            download::runtime(setup, true, stop.clone()).await?
        }
    } else {
        download::runtime(setup, true, stop.clone()).await?
    };
    update(
        setup,
        Some("creating-environment"),
        Some("创建隔离环境，原 Python 和已训练作业不变。"),
    )?;
    let mut command = process::command(&base, &setup.directory);
    command
        .args(["-I", "-B", "-m", "venv", "--without-pip"])
        .arg(&setup.directory);
    process::run(setup, command, stop.clone(), Duration::from_secs(120), true).await?;
    let python = setup.directory.join(if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    });
    if backend != "builtin" {
        let uv = download::runtime(setup, false, stop.clone()).await?;
        let hardware = hardware().await;
        let nvidia = hardware["nvidiaDetected"] == true;
        let cuda = device == "cuda" || device == "auto" && nvidia;
        if device == "cuda" && !nvidia {
            return Err(invalid(
                "未检测到 NVIDIA 训练设备，可改用 CPU 或检查显卡驱动。",
            ));
        }
        let mut packages = vec!["numpy==2.3.4", "pillow==12.0.0"];
        if backend == "sklearn" {
            packages.push("scikit-learn==1.7.2");
        }
        if matches!(backend, "transformers" | "peft" | "trl") {
            packages.extend([
                "transformers==4.57.1",
                "datasets==4.3.0",
                "accelerate==1.11.0",
            ]);
        }
        if matches!(backend, "peft" | "trl") {
            packages.push("peft==0.17.1");
        }
        if backend == "trl" {
            packages.push("trl==0.24.0");
        }
        if backend != "sklearn" {
            install_packages(
                setup,
                &uv,
                &python,
                &["torch==2.9.0", "torchvision==0.24.0"],
                if cuda {
                    "https://download.pytorch.org/whl/cu128"
                } else {
                    "https://download.pytorch.org/whl/cpu"
                },
                stop.clone(),
            )
            .await?;
        }
        install_packages(
            setup,
            &uv,
            &python,
            &packages,
            "https://pypi.org/simple",
            stop.clone(),
        )
        .await?;
    }
    update(setup, Some("checking"), Some("检查依赖和实际可用设备。"))?;
    let script = runtime::worker(&setup.context)?.with_file_name("environment_setup.py");
    let mut command = process::command(&python, &setup.directory);
    command.args(["-I", "-B", "-u"]).arg(script).arg(backend);
    let output = process::run(
        setup,
        command,
        stop.clone(),
        Duration::from_secs(120),
        false,
    )
    .await?;
    let probe = output
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .ok_or_else(|| invalid("训练环境检查没有返回有效结果。"))?;
    patch(setup, json!({"probe":probe}))?;
    if probe["available"] != true {
        for failure in probe["missing"].as_array().into_iter().flatten() {
            update(
                setup,
                None,
                Some(&format!(
                    "{}: {}",
                    failure["name"].as_str().unwrap_or("依赖"),
                    failure["reason"].as_str().unwrap_or("无法加载")
                )),
            )?;
        }
        return Err(invalid(
            "训练依赖检查未通过，原因已写入安装记录，原环境继续保留。",
        ));
    }
    if device == "cuda" && probe["cudaAvailable"] != true {
        return Err(invalid(
            "CUDA 运行检查未通过，未切换原环境；请检查 NVIDIA 驱动。",
        ));
    }
    // 仅在未取消且用户没有改选环境时发布，运行中的作业始终使用旧路径。
    let _guard = active().lock().map_err(|_| invalid("环境状态正在恢复。"))?;
    cancelled(&stop)?;
    let selected = store::scope_dir(&setup.context).join("environment.json");
    let _configuration = super::CONFIGURATION
        .lock()
        .map_err(|_| invalid("训练配置正在保存。"))?;
    if managed_files::read(&setup.context.data_home, &selected, 65536)? != setup.previous {
        return Err(invalid(
            "你已改选训练环境；新环境已准备好，当前选择保持不变。",
        ));
    }
    store::write(
        &setup.context,
        &selected,
        &json!({"path":python,"managed":true,"setupId":setup.id,"backend":backend,"probe":probe}),
    )?;
    patch(
        setup,
        json!({"state":"ready","stage":"ready","message":"训练环境已就绪。","pythonPath":python,"probe":probe}),
    )?;
    Ok(())
}

async fn install_packages(
    setup: &Setup,
    uv: &Path,
    python: &Path,
    packages: &[&str],
    source: &str,
    stop: watch::Receiver<bool>,
) -> Result<(), GatewayError> {
    update(
        setup,
        Some("installing-packages"),
        Some(&format!("安装 {}", packages.join(" · "))),
    )?;
    patch(setup, json!({"source":source}))?;
    let mut command = process::command(uv, &setup.directory);
    command
        .args([
            "--no-config",
            "--no-progress",
            "--native-tls",
            "pip",
            "install",
            "--python",
        ])
        .arg(python)
        .args(["--only-binary", ":all:", "--index-url", source])
        .args(packages)
        .env(
            "UV_CACHE_DIR",
            setup
                .context
                .data_home
                .join("Runtime/training-package-cache"),
        )
        .env("UV_CONCURRENT_DOWNLOADS", "2")
        .env("UV_CONCURRENT_INSTALLS", "2")
        .env("UV_CONCURRENT_BUILDS", "1");
    if let Some((proxy, bypass)) = crate::models::gateway::training_download_proxy(source)
        .await
        .map_err(|e| GatewayError::new("TRAINING_ENV_NETWORK", &e.message, 503))?
    {
        command
            .env("HTTPS_PROXY", &proxy)
            .env("HTTP_PROXY", proxy)
            .env("NO_PROXY", bypass);
    }
    process::run(setup, command, stop, Duration::from_secs(2 * 60 * 60), true).await?;
    Ok(())
}

pub(super) async fn shutdown(home: &Path) {
    let waiting = {
        let Ok(rows) = active().lock() else {
            return;
        };
        rows.values()
            .filter(|v| v.home == home)
            .map(|v| {
                v.stop.send_replace(true);
                v.done.clone()
            })
            .collect::<Vec<_>>()
    };
    for done in waiting {
        let _ = tokio::time::timeout(Duration::from_secs(5), done.notified()).await;
    }
}
