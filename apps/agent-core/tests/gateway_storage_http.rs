//! Real maintenance and history HTTP checks use owned temporary roots and a local fake provider.
//! 真实维护和历史 HTTP 检查仅使用自有临时根目录与本机模拟供应商，不接触正式数据或凭据。

use axum::{Json, Router, routing::post};
use kynxa_agent_core_native::{
    gateway,
    gateway_contract::{GatewayConfig, new_id},
    gateway_storage::StorageLifecycle,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

struct Fixture {
    root: PathBuf,
    config: GatewayConfig,
    lifecycle: Arc<StorageLifecycle>,
    client: reqwest::Client,
    url: String,
    conversation: String,
    message: String,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-storage-http-{}", new_id()));
        let data = root.join("Data");
        let config = GatewayConfig {
            data_root: data.clone(),
            data_home: data.join("Models"),
            extension_root: root.join("Extensions"),
            application_root: root.join("App"),
            port: 0,
            toolhost_exe: root.join("unused-toolhost.exe"),
            vector_extension: root.join("unused-vec.dll"),
            inference_library: root.join("unused-ort.dll"),
        };
        std::fs::create_dir_all(&config.data_home).unwrap();
        std::fs::create_dir_all(&config.extension_root).unwrap();
        let resolver_root = root.clone();
        let resolver_config = config.clone();
        let lifecycle = StorageLifecycle::managed(
            kynxa_agent_core_native::orchestration::gateway_runtime::GatewayRuntime::new(
                config.clone(),
            ),
            root.join("migration.lock"),
            Arc::new(move || {
                let mut config = resolver_config.clone();
                let selected: Value =
                    serde_json::from_slice(&std::fs::read(resolver_root.join("selected.json"))?)
                        .map_err(|_| {
                            kynxa_agent_core_native::gateway_contract::GatewayError::new(
                                "INVALID_STORAGE_CONFIGURATION",
                                "fixture pointer invalid",
                                409,
                            )
                        })?;
                config.data_root = PathBuf::from(selected["dataRoot"].as_str().unwrap());
                config.data_home = config.data_root.join("Models");
                config.extension_root = PathBuf::from(selected["extensionRoot"].as_str().unwrap());
                Ok(config)
            }),
        )
        .unwrap();
        std::fs::write(
            root.join("selected.json"),
            json!({"dataRoot":config.data_root,"extensionRoot":config.extension_root}).to_string(),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let service = gateway::router_with_lifecycle(lifecycle.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        let conversation = new_id();
        let message = new_id();
        let catalog: Value = client
            .get(format!(
                "{url}/api/conversations/catalog?includeMessages=false"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let result = client.put(format!("{url}/api/conversations/catalog")).json(&json!({"Revision":catalog["Revision"],"Projects":[],"Chats":[{
            "Id":conversation,"Title":"Storage fixture","Draft":"","Messages":[{"Id":message,"Role":"user","Content":"完整原文 中文","CreatedAt":"2026-10-14T00:00:00Z"}]}]})).send().await.unwrap();
        assert!(
            result.status().is_success(),
            "{}",
            result.text().await.unwrap()
        );
        Self {
            root,
            config,
            lifecycle,
            client,
            url,
            conversation,
            message,
            server,
        }
    }
    fn log(&self) -> PathBuf {
        self.config
            .data_root
            .join("Chats")
            .join(&self.conversation)
            .join("events.jsonl")
    }
    async fn page(&self) -> reqwest::Response {
        self.client
            .get(format!(
                "{}/api/conversations/{}/messages",
                self.url, self.conversation
            ))
            .send()
            .await
            .unwrap()
    }
    async fn health(&self) -> Value {
        self.client
            .get(format!("{}/health", self.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn wait_health(&self, ready: bool, model_home: &Path) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let health = self.health().await;
            assert!(health["runtimeCleanupError"].is_null(), "{health}");
            if health["migrationReady"] == ready
                && health["modelDataHome"] == json!(model_home)
                && (ready || health["migrating"] == false)
            {
                return health;
            }
            assert!(tokio::time::Instant::now() < deadline, "{health}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    async fn close(self) {
        self.lifecycle
            .current()
            .await
            .quiesce_storage()
            .await
            .unwrap();
        self.lifecycle.shutdown().await;
        self.server.abort();
        let _ = self.server.await;
        drop(self.lifecycle);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[tokio::test]
async fn history_http_recovers_ascii_and_partial_chinese_tail_without_losing_original_bytes() {
    for tail in [
        b"{\"version\":1,\"message\":".to_vec(),
        [b"{\"version\":1,\"message\":\"".as_slice(), &[0xe4, 0xb8]].concat(),
    ] {
        let fixture = Fixture::new().await;
        let original = std::fs::read(fixture.log()).unwrap();
        let mut broken = original.clone();
        broken.extend_from_slice(&tail);
        std::fs::write(fixture.log(), broken).unwrap();
        let page = fixture.page().await;
        assert!(page.status().is_success(), "{}", page.text().await.unwrap());
        let page: Value = page.json().await.unwrap();
        assert_eq!(page["messages"][0]["Id"], fixture.message);
        assert_eq!(page["messages"][0]["Content"], "完整原文 中文");
        assert_eq!(std::fs::read(fixture.log()).unwrap(), original);
        let backup = std::fs::read_dir(fixture.log().parent().unwrap())
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("events.jsonl.recovered-tail-")
            })
            .unwrap();
        assert_eq!(std::fs::read(backup.path()).unwrap(), tail);
        fixture.close().await;
    }
}

#[tokio::test]
async fn history_http_refuses_closed_corruption_and_complete_invalid_utf8_without_rewriting() {
    for tail in [b"{broken}\n".to_vec(), b"{\"content\":\"\xff\"}".to_vec()] {
        let fixture = Fixture::new().await;
        let mut broken = std::fs::read(fixture.log()).unwrap();
        broken.extend_from_slice(&tail);
        std::fs::write(fixture.log(), &broken).unwrap();
        assert!(!fixture.page().await.status().is_success());
        assert_eq!(std::fs::read(fixture.log()).unwrap(), broken);
        fixture.close().await;
    }
}

#[cfg(windows)]
fn maintenance_lock(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(1)
        .open(path)
        .unwrap()
}
fn copy_fixture(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap().map(Result::unwrap) {
        if entry.file_name() == ".gateway-owner.lock" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_http_drains_active_durable_run_copies_ledger_then_reloads_data_and_extension_roots()
 {
    let fixture = Fixture::new().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let provider_entered = entered.clone();
    let provider = Router::new().route("/chat/completions", post(move || {
        let entered = provider_entered.clone();
        async move { entered.notify_one(); tokio::time::sleep(Duration::from_secs(30)).await;
            Json(json!({"choices":[{"message":{"role":"assistant","content":"delayed fixture"},"finish_reason":"stop"}]})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}", listener.local_addr().unwrap());
    let provider_server = tokio::spawn(async move {
        axum::serve(listener, provider).await.unwrap();
    });
    let saved = fixture.client.post(format!("{}/api/models",fixture.url)).json(&json!({"providerId":"storage-fixture","displayName":"Storage HTTP fixture","baseUrl":provider_url,"apiKey":"","protocol":"openai-completions","models":["fixture"],"contextWindowTokens":32768,"maxOutputTokens":4096})).send().await.unwrap();
    assert!(
        saved.status().is_success(),
        "{}",
        saved.text().await.unwrap()
    );
    let request_id = new_id();
    let request = fixture.client.post(format!("{}/api/chat",fixture.url)).json(&json!({"conversationId":fixture.conversation,"requestId":request_id,"userMessageId":fixture.message,"provider":"storage-fixture","model":"fixture","message":"完整原文 中文","permissionMode":"ask"}));
    let pending = tokio::spawn(async move { request.send().await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let lock = maintenance_lock(&fixture.root.join("migration.lock"));
    let busy = fixture.health().await;
    assert_eq!(busy["migrating"], true);
    assert_eq!(busy["migrationReady"], false);
    assert!(busy["activeRequests"].as_u64().unwrap() > 0);
    assert_eq!(fixture.page().await.status().as_u16(), 503);
    let cancelled = fixture
        .client
        .post(format!("{}/api/chat/cancel", fixture.url))
        .json(&json!({"conversationId":fixture.conversation,"requestId":request_id}))
        .send()
        .await
        .unwrap();
    assert!(
        cancelled.status().is_success(),
        "{}",
        cancelled.text().await.unwrap()
    );
    let _ = pending.await.unwrap();
    let ready = fixture.wait_health(true, &fixture.config.data_home).await;
    assert_eq!(ready["activeRequests"], 0);
    let ledger = fixture.config.data_root.join("Agent/tasks.sqlite");
    assert!(ledger.is_file());
    assert!(
        !ledger.with_file_name("tasks.sqlite-wal").exists()
            || std::fs::metadata(ledger.with_file_name("tasks.sqlite-wal"))
                .unwrap()
                .len()
                == 0
    );
    let destination = fixture.root.join("MovedData");
    copy_fixture(&fixture.config.data_root, &destination);
    let before = std::fs::read(destination.join("Agent/tasks.sqlite")).unwrap();
    std::fs::write(
        fixture.root.join("selected.json"),
        json!({"dataRoot":destination,"extensionRoot":fixture.config.extension_root}).to_string(),
    )
    .unwrap();
    drop(lock);
    fixture
        .wait_health(false, &destination.join("Models"))
        .await;
    let page: Value = fixture.page().await.json().await.unwrap();
    assert_eq!(page["messages"][0]["Id"], fixture.message);
    let tasks: Value = fixture
        .client
        .get(format!(
            "{}/api/tasks?conversationId={}",
            fixture.url, fixture.conversation
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(tasks.to_string().contains(&request_id), "{tasks}");
    assert!(!before.is_empty());
    assert!(
        fixture
            .config
            .data_root
            .join("Chats")
            .join(&fixture.conversation)
            .join("events.jsonl")
            .is_file()
    );
    let extension = fixture.root.join("MovedExtensions");
    std::fs::create_dir_all(&extension).unwrap();
    let lock = maintenance_lock(&fixture.root.join("migration.lock"));
    fixture.wait_health(true, &destination.join("Models")).await;
    std::fs::write(
        fixture.root.join("selected.json"),
        json!({"dataRoot":destination,"extensionRoot":extension}).to_string(),
    )
    .unwrap();
    drop(lock);
    let health = fixture
        .wait_health(false, &destination.join("Models"))
        .await;
    assert_eq!(health["extensionRoot"], json!(extension));
    provider_server.abort();
    let _ = provider_server.await;
    fixture.close().await;
}
