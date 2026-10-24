//! Optional indexing starts after mandatory resource admission, without delaying HTTP readiness.
//! 必要资源准入就绪后启动可选索引，HTTP 就绪不等待索引恢复。

use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    gateway_storage::StorageLifecycle,
};
use serde_json::json;
use std::sync::Arc;
use tokio::task::JoinHandle;

pub(super) fn schedule_indexing(lifecycle: Arc<StorageLifecycle>) -> JoinHandle<()> {
    schedule_indexing_with(lifecycle, |config| {
        crate::retrieval::gateway::start_background(&config)
            .map(|_| ())
            .map_err(Into::into)
    })
}

fn schedule_indexing_with(
    lifecycle: Arc<StorageLifecycle>,
    initialize: impl FnOnce(GatewayConfig) -> Result<(), GatewayError> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = async {
            // Reuse migration admission: old-root startup must settle before its workers are drained.
            // 复用迁移准入：旧根目录的启动须先结算，维护才能清退该目录的作业。
            let (runtime, _admission) = lifecycle.admit(false).await?;
            let config = runtime.config.clone();
            tokio::task::spawn_blocking(move || initialize(config))
                .await
                .map_err(|_| {
                    GatewayError::new("INDEX_START_FAILED", "后台资料服务未启动。", 503)
                })?
        }
        .await;
        if let Err(error) = result {
            eprintln!(
                "{}",
                json!({"code":error.code,"message":error.message,"lexicalReadAvailable":true})
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gateway, orchestration::gateway_runtime::GatewayRuntime};
    use serde_json::Value;
    use std::time::Duration;

    #[tokio::test]
    async fn indexing_startup_allows_health_and_settles_before_storage_migration() {
        let root = std::env::temp_dir()
            .join(format!("kynxa-startup-{}", crate::gateway_contract::new_id()));
        let config = GatewayConfig {
            data_root: root.join("Data"),
            data_home: root.join("Data/Models"),
            extension_root: root.join("Extensions"),
            application_root: root.join("App"),
            port: 0,
            toolhost_exe: root.join("unused-toolhost.exe"),
            vector_extension: root.join("unused-vec.dll"),
            inference_library: root.join("unused-ort.dll"),
        };
        std::fs::create_dir_all(&config.data_home).unwrap();
        let migration_path = root.join("migration.lock");
        let selected = config.clone();
        let lifecycle = StorageLifecycle::managed(
            GatewayRuntime::new(config.clone()),
            migration_path.clone(),
            Arc::new(move || Ok(selected.clone())),
        )
        .unwrap();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let expected_root = config.data_root.clone();
        let startup = schedule_indexing_with(lifecycle.clone(), move |observed| {
            assert_eq!(observed.data_root, expected_root);
            entered.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            Err(GatewayError::new("INDEX_FIXTURE_FAILURE", "fixture", 503))
        });
        tokio::time::timeout(Duration::from_secs(2), entering)
            .await
            .unwrap()
            .unwrap();
        let ready = lifecycle.health().await;
        assert_eq!(ready["status"], "ok");
        assert_eq!(ready["migrating"], false);
        assert_eq!(ready["activeRequests"], 1);

        // The desktop's deny-write marker must not drain a still-starting old-root worker.
        // 桌面的拒写维护标记不能越过仍在启动的旧根目录作业，提前清退资源。
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let marker = options.open(&migration_path).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/health", listener.local_addr().unwrap());
        let router = gateway::router_with_lifecycle(lifecycle.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let health: Value = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(endpoint)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(health["status"], "ok");
        assert_eq!(health["activeRequests"], 1);
        #[cfg(windows)]
        {
            assert_eq!(health["migrating"], true);
            assert_eq!(health["migrationReady"], false);
            assert_eq!(lifecycle.admit(false).await.err().unwrap().code, "STORAGE_MIGRATING");
        }
        assert!(!startup.is_finished());

        release.send(()).unwrap();
        startup.await.unwrap();
        drop(marker);
        std::fs::remove_file(migration_path).unwrap();
        let settled = lifecycle.health().await;
        assert_eq!(settled["activeRequests"], 0);
        assert_eq!(settled["status"], "ok");
        assert_eq!(settled["migrating"], false);
        server.abort();
        let _ = server.await;
        drop(lifecycle);
        std::fs::remove_dir_all(root).unwrap();
    }
}
