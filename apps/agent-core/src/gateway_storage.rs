//! Storage maintenance closes admission, drains real writers and replaces immutable runtimes.
//! 存储维护关闭准入、结算真实写入者并替换不可变运行时，健康字段不能代替实际结算。

use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    orchestration::gateway_runtime::GatewayRuntime,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;

type ConfigResolver = Arc<dyn Fn() -> Result<GatewayConfig, GatewayError> + Send + Sync>;
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Running,
    Waiting,
    Draining,
    Ready,
    Reloading,
    CleanupFailed,
    ReloadFailed,
}
struct State {
    runtime: Arc<GatewayRuntime>,
    phase: Phase,
    cleanup_error: Option<String>,
    owner: Option<File>,
}
pub struct StorageLifecycle {
    state: Mutex<State>,
    active_http: Arc<AtomicUsize>,
    migration_lock: Option<PathBuf>,
    resolve_config: Option<ConfigResolver>,
}
pub struct RequestAdmission(Arc<AtomicUsize>);
impl Drop for RequestAdmission {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl StorageLifecycle {
    pub fn fixed(runtime: Arc<GatewayRuntime>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                runtime,
                phase: Phase::Running,
                cleanup_error: None,
                owner: None,
            }),
            active_http: Default::default(),
            migration_lock: None,
            resolve_config: None,
        })
    }
    pub fn managed(
        runtime: Arc<GatewayRuntime>,
        migration_lock: PathBuf,
        resolver: ConfigResolver,
    ) -> Result<Arc<Self>, GatewayError> {
        let owner = crate::gateway::own_data_root(&runtime.config.data_root)?;
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                runtime,
                phase: Phase::Running,
                cleanup_error: None,
                owner: Some(owner),
            }),
            active_http: Default::default(),
            migration_lock: Some(migration_lock),
            resolve_config: Some(resolver),
        }))
    }
    pub async fn current(&self) -> Arc<GatewayRuntime> {
        self.state.lock().await.runtime.clone()
    }
    pub async fn health(self: &Arc<Self>) -> Value {
        self.refresh().await;
        let state = self.state.lock().await;
        let mut value = crate::gateway::health();
        value["storageProtocol"] = json!(1);
        value["modelDataHome"] = json!(state.runtime.config.data_home);
        value["extensionRoot"] = json!(state.runtime.config.extension_root);
        value["migrating"] = json!(state.phase != Phase::Running);
        value["activeRequests"] = json!(
            self.active_http
                .load(Ordering::Acquire)
                .saturating_add(state.runtime.active_requests().await)
        );
        value["migrationReady"] = json!(state.phase == Phase::Ready);
        value["runtimeCleanupError"] = json!(state.cleanup_error);
        if state.cleanup_error.is_some() {
            value["status"] = json!("degraded");
        }
        value
    }
    pub async fn admit(
        self: &Arc<Self>,
        control: bool,
    ) -> Result<(Arc<GatewayRuntime>, RequestAdmission), GatewayError> {
        self.refresh().await;
        let state = self.state.lock().await;
        if state.phase != Phase::Running
            && !(control && matches!(state.phase, Phase::Waiting | Phase::CleanupFailed))
        {
            return Err(GatewayError::new(
                "STORAGE_MIGRATING",
                "存储正在维护，请稍后重试；原记录已保留。",
                503,
            ));
        }
        self.active_http.fetch_add(1, Ordering::AcqRel);
        Ok((
            state.runtime.clone(),
            RequestAdmission(self.active_http.clone()),
        ))
    }
    async fn refresh(self: &Arc<Self>) {
        let Some(path) = &self.migration_lock else {
            return;
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        let mut attempted_cleanup = false;
        loop {
            let active = match migration_active(path) {
                Ok(active) => active,
                Err(error) => {
                    let mut state = self.state.lock().await;
                    if !matches!(state.phase, Phase::Draining | Phase::Reloading) {
                        state.phase = Phase::CleanupFailed;
                    }
                    state.cleanup_error = Some(error.code);
                    return;
                }
            };
            let mut state = self.state.lock().await;
            match (active, state.phase) {
                (true, Phase::Running) => state.phase = Phase::Waiting,
                (false, Phase::Waiting) => state.phase = Phase::Running,
                _ => {}
            }
            // Retry failed cleanup on a later request, including an abandoned migration whose marker was removed.
            // 后续请求可重试失败的结算；即使用户已撤销维护标记，也必须先结算成功再重建运行时。
            if !attempted_cleanup
                && (active && state.phase == Phase::Waiting || state.phase == Phase::CleanupFailed)
                && self.active_http.load(Ordering::Acquire) == 0
                && state.runtime.active_requests().await == 0
            {
                attempted_cleanup = true;
                state.phase = Phase::Draining;
                let lifecycle = self.clone();
                let runtime = state.runtime.clone();
                // Own cleanup in an independent task: a disconnected health caller cannot cancel halfway.
                // 独立任务拥有清理流程；健康请求断开不能在结算中途撤销清理。
                tokio::spawn(async move {
                    let result = runtime.quiesce_storage().await;
                    let mut state = lifecycle.state.lock().await;
                    match result {
                        Ok(()) => {
                            state.phase = Phase::Ready;
                            state.cleanup_error = None;
                        }
                        Err(error) => {
                            state.phase = Phase::CleanupFailed;
                            state.cleanup_error = Some(error.code);
                        }
                    }
                });
            }
            if !active && matches!(state.phase, Phase::Ready | Phase::ReloadFailed) {
                state.phase = Phase::Reloading;
                let lifecycle = self.clone();
                tokio::spawn(async move {
                    lifecycle.reload().await;
                });
            }
            let waiting = matches!(state.phase, Phase::Draining | Phase::Reloading);
            drop(state);
            if !waiting || tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn reload(self: Arc<Self>) {
        let resolver = self.resolve_config.as_ref().unwrap().clone();
        let old_config = self.current().await.config.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let config = resolver()?;
            let owner = if config.data_root == old_config.data_root {
                None
            } else {
                Some(crate::gateway::own_data_root(&config.data_root)?)
            };
            crate::gateway::initialize_storage(config.data_root.to_str().ok_or_else(|| {
                GatewayError::new("INVALID_STORAGE_CONFIGURATION", "存储目录编码无效。", 409)
            })?)?;
            crate::retrieval::gateway::start_background(&config)?;
            Ok::<_, GatewayError>((config, owner))
        })
        .await;
        let prepared = prepared
            .map_err(|_| GatewayError::new("STORAGE_RELOAD_FAILED", "新目录加载线程未完成。", 503))
            .and_then(|value| value);
        let mut state = self.state.lock().await;
        match prepared {
            Ok((config, owner)) => {
                let runtime = GatewayRuntime::new(config);
                if owner.is_some() {
                    state.owner = owner;
                }
                state.runtime = runtime.clone();
                state.cleanup_error = None;
                state.phase = Phase::Running;
                drop(state);
                let recovery = runtime.tasks.clone();
                tokio::spawn(async move {
                    if let Err(error) = recovery.recover().await {
                        eprintln!("{}", json!({"code":error.code,"taskRecovery":"blocked"}));
                    }
                });
                runtime.schedule_extension_prewarm();
            }
            Err(error) => {
                state.phase = Phase::ReloadFailed;
                state.cleanup_error = Some(error.code);
            }
        }
    }
    pub async fn shutdown(&self) {
        self.current().await.shutdown().await;
    }
}

/// The desktop holds a deny-write OS handle; stale files alone never pause the gateway.
/// 桌面持有拒绝写共享的操作系统句柄；仅残留锁文件不能使网关永久进入维护。
fn migration_active(path: &std::path::Path) -> Result<bool, GatewayError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 1024 => {}
        _ => {
            return Err(GatewayError::new(
                "INVALID_MIGRATION_LOCK",
                "维护锁路径无效，原目录已保留。",
                409,
            ));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(7).custom_flags(0x0020_0000);
        match options.open(path) {
            Ok(_) => Ok(false),
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => Ok(true),
            Err(_) => Err(GatewayError::new(
                "MIGRATION_LOCK_UNAVAILABLE",
                "维护锁无法核验，原目录已保留。",
                409,
            )),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(path.exists())
    }
}
