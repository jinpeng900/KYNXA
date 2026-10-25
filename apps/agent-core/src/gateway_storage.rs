//! Storage maintenance closes admission, drains writers and replaces runtimes.
//! 存储维护先关闭准入、等待写入结束，再替换运行时。

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
use tokio::sync::{Mutex, watch};

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
    shutdown_requested: watch::Sender<bool>,
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
            shutdown_requested: watch::channel(false).0,
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
            shutdown_requested: watch::channel(false).0,
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
        value["shuttingDown"] = json!(self.is_shutting_down());
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
        if self.is_shutting_down() { return Err(shutdown_error()); }
        self.refresh().await;
        let state = self.state.lock().await;
        if self.is_shutting_down() { return Err(shutdown_error()); }
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
        if self.is_shutting_down() { return; }
        let Some(path) = &self.migration_lock else {
            return;
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        let mut attempted_cleanup = false;
        loop {
            if self.is_shutting_down() { return; }
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
            if self.is_shutting_down() { return; }
            match (active, state.phase) {
                (true, Phase::Running) => state.phase = Phase::Waiting,
                (false, Phase::Waiting) => state.phase = Phase::Running,
                _ => {}
            }
            // Retry unfinished cleanup before rebuilding runtimes, even if its marker was removed.
            // 重建运行时前重试未完成清理，维护标记已删除也一样。
            if !attempted_cleanup
                && (active && state.phase == Phase::Waiting || state.phase == Phase::CleanupFailed)
                && self.active_http.load(Ordering::Acquire) == 0
                && state.runtime.active_requests().await == 0
            {
                attempted_cleanup = true;
                state.phase = Phase::Draining;
                let lifecycle = self.clone();
                let runtime = state.runtime.clone();
                // Run cleanup independently so health-request disconnects cannot cancel it.
                // 清理由独立任务执行，健康检查断开不会中途取消。
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
        let cancellation = self.shutdown_signal();
        let prepared = tokio::task::spawn_blocking(move || {
            if *cancellation.borrow() { return Err(shutdown_error()); }
            let config = resolver()?;
            let owner = if config.data_root == old_config.data_root {
                None
            } else {
                Some(crate::gateway::own_data_root(&config.data_root)?)
            };
            crate::gateway::initialize_storage(config.data_root.to_str().ok_or_else(|| {
                GatewayError::new("INVALID_STORAGE_CONFIGURATION", "存储目录编码无效。", 409)
            })?)?;
            if *cancellation.borrow() { return Err(shutdown_error()); }
            crate::retrieval::gateway::start_background(&config)?;
            Ok::<_, GatewayError>((config, owner))
        })
        .await;
        let prepared = prepared
            .map_err(|_| GatewayError::new("STORAGE_RELOAD_FAILED", "新目录加载线程未完成。", 503))
            .and_then(|value| value);
        let mut state = self.state.lock().await;
        if self.is_shutting_down() {
            drop(state);
            if let Ok((config, _owner)) = prepared {
                let _ = tokio::task::spawn_blocking(move || crate::retrieval::gateway::drain_background(&config)).await;
            }
            self.state.lock().await.phase = Phase::Ready;
            return;
        }
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
    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown_requested.borrow()
    }

    pub(crate) fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown_requested.subscribe()
    }

    pub(crate) async fn wait_for_shutdown(&self) {
        let mut requested = self.shutdown_signal();
        while !*requested.borrow_and_update() {
            if requested.changed().await.is_err() { break; }
        }
    }

    /// Bind desktop exit to this installation's current paths before closing admission.
    /// 桌面退出先核对当前安装路径，再关闭准入；不接管其他数据目录的网关。
    pub(crate) async fn request_shutdown(&self, identity: &Value) -> Result<(), GatewayError> {
        let state = self.state.lock().await;
        for (field, expected) in [
            ("modelDataHome", &state.runtime.config.data_home),
            ("extensionRoot", &state.runtime.config.extension_root),
        ] {
            let supplied = identity[field].as_str().filter(|path| std::path::Path::new(path).is_absolute())
                .ok_or_else(|| GatewayError::new("INVALID_SHUTDOWN_IDENTITY", "退出请求缺少有效的软件目录。", 400))?;
            let normalize = |path: &str| -> Result<String, GatewayError> {
                let resolved = crate::storage::administration::configured_path(&json!(path), &state.runtime.config.application_root)?;
                let path = resolved.to_string_lossy();
                Ok(if cfg!(windows) {
                    crate::storage::retrieval::normalize_path(&path).trim_end_matches('/').to_owned()
                } else { path.trim_end_matches('/').to_owned() })
            };
            if normalize(supplied)? != normalize(&expected.to_string_lossy())? {
                return Err(GatewayError::new("SHUTDOWN_IDENTITY_MISMATCH", "退出请求不属于当前软件。", 409));
            }
        }
        state.runtime.close_admission();
        self.shutdown_requested.send_replace(true);
        Ok(())
    }

    pub(crate) async fn begin_shutdown(&self) {
        let state = self.state.lock().await;
        state.runtime.close_admission();
        self.shutdown_requested.send_replace(true);
    }

    pub async fn shutdown(&self) {
        self.begin_shutdown().await;
        loop {
            let phase = self.state.lock().await.phase;
            if !matches!(phase, Phase::Draining | Phase::Reloading) { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let runtime = self.current().await;
        // Accepted HTTP writes settle independently; SSE observation ends on the shutdown signal.
        // 已准入写入自行结算；SSE 观察随退出信号结束，不能相互等待。
        loop {
            runtime.interrupt_for_shutdown().await;
            if self.active_http.load(Ordering::Acquire) == 0 { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        runtime.shutdown().await;
    }
}

fn shutdown_error() -> GatewayError {
    GatewayError::new("GATEWAY_SHUTTING_DOWN", "软件正在退出，已有任务与回执正在保存。", 503)
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_identity_closes_admission_without_waiting_for_existing_request() {
        let root = std::env::temp_dir().join(format!("kynxa-exit-{}", uuid::Uuid::new_v4()));
        let config = GatewayConfig {
            data_root: root.join("Data"), data_home: root.join("Data/Models"),
            extension_root: root.join("Extensions"), application_root: root.join("App"), port: 0,
            toolhost_exe: root.join("unused-toolhost.exe"), vector_extension: root.join("unused-vec.dll"),
            inference_library: root.join("unused-ort.dll"),
        };
        let lifecycle = StorageLifecycle::fixed(GatewayRuntime::new(config.clone()));
        let (_, admitted) = lifecycle.admit(false).await.unwrap();
        let mut identity = json!({"modelDataHome":config.data_home,"extensionRoot":root.join("Other")});
        assert_eq!(lifecycle.request_shutdown(&identity).await.unwrap_err().code, "SHUTDOWN_IDENTITY_MISMATCH");
        assert!(!lifecycle.is_shutting_down());
        identity["extensionRoot"] = json!(config.extension_root.join("."));
        #[cfg(windows)] {
            identity["modelDataHome"] = json!(format!("{}\\", config.data_home.to_string_lossy().to_uppercase()));
        }
        tokio::time::timeout(Duration::from_secs(1), lifecycle.request_shutdown(&identity)).await.unwrap().unwrap();
        assert_eq!(lifecycle.active_http.load(Ordering::Acquire), 1);
        assert_eq!(lifecycle.admit(false).await.err().unwrap().code, "GATEWAY_SHUTTING_DOWN");
        assert_eq!(lifecycle.admit(true).await.err().unwrap().code, "GATEWAY_SHUTTING_DOWN");
        lifecycle.request_shutdown(&identity).await.unwrap();
        lifecycle.wait_for_shutdown().await;
        drop(admitted);
        assert_eq!(lifecycle.active_http.load(Ordering::Acquire), 0);
    }
}

/// Require the desktop's deny-write handle; stale marker files cannot pause the gateway.
/// 以桌面的拒写句柄为准，旧标记文件不能暂停网关。
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
