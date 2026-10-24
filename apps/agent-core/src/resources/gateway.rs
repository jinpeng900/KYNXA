//! Native resource-service transport; one process remains the hardware allocation authority.
//! 原生资源服务传输；硬件授额仍由唯一资源服务进程负责。

use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    process::Stdio,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, oneshot, watch},
};

type Reply = oneshot::Sender<Result<Value, GatewayError>>;
struct Pending {
    reply: Reply,
    admission: bool,
}
static GLOBAL: OnceLock<Arc<ResourceService>> = OnceLock::new();

pub struct ResourceService {
    child: Mutex<Child>,
    input: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    next_id: Arc<AtomicU64>,
}

/// A dropped or timed-out caller still owns admission retirement until its original receipt settles.
/// 调用方被丢弃或超时后仍须结算原准入回执，避免晚到的授予租约失去释放者。
struct PendingAdmission {
    service: Arc<ResourceService>,
    request_id: u64,
    attempt: Option<tokio::task::JoinHandle<Result<Value, GatewayError>>>,
}

impl Drop for PendingAdmission {
    fn drop(&mut self) {
        let Some(attempt) = self.attempt.take() else { return; };
        let service = self.service.clone();
        let request_id = self.request_id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = service.request("cancelAcquire", json!({"requestId":request_id}), Duration::from_secs(5)).await;
                if let Ok(Ok(receipt)) = attempt.await {
                    if let Some(lease_id) = receipt["leaseId"].as_str().filter(|_| receipt["status"] == "granted") {
                        let _ = service.request("release", json!({"leaseId":lease_id}), Duration::from_secs(5)).await;
                    }
                }
            });
        }
    }
}

impl ResourceService {
    pub async fn start(executable: &Path) -> Result<Arc<Self>, GatewayError> {
        let mut command = Command::new(executable);
        command
            .arg("--owner-pid")
            .arg(std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command.spawn().map_err(|_| {
            GatewayError::new(
                "RESOURCE_RUNTIME_UNAVAILABLE",
                "缺少原生资源服务或无法启动，请检查安装包。",
                503,
            )
        })?;
        let input = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let pending: Arc<Mutex<HashMap<u64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        let readers = pending.clone();
        let retirement_input = input.clone();
        let next_id = Arc::new(AtomicU64::new(1));
        let retirement_id = next_id.clone();
        tokio::spawn(async move {
            loop {
                let mut frame = Vec::new();
                // A bounded frame reader cannot allocate forever on a corrupt helper response.
                // 辅助进程响应异常时，有界帧读取不能无限分配内存。
                let mut complete = false;
                while frame.len() <= 65536 {
                    let available = match output.fill_buf().await {
                        Ok(bytes) => bytes,
                        Err(_) => break,
                    };
                    if available.is_empty() {
                        break;
                    }
                    let newline = available.iter().position(|byte| *byte == b'\n');
                    let consumed = newline.map_or(available.len(), |index| index + 1);
                    if frame.len() + consumed > 65536 {
                        break;
                    }
                    frame.extend_from_slice(&available[..consumed]);
                    output.consume(consumed);
                    if newline.is_some() {
                        complete = true;
                        break;
                    }
                }
                if !complete {
                    break;
                }
                let Ok(value) = serde_json::from_slice::<Value>(&frame) else {
                    break;
                };
                if let Some(id) = value["id"].as_u64() {
                    let pending = readers.lock().await.remove(&id);
                    if let Some(pending) = pending {
                        let result = value.get("result").cloned().unwrap_or(value);
                        if let Err(Ok(result)) = pending.reply.send(Ok(result)) {
                            if pending.admission
                                && result["status"] == "granted"
                                && result["leaseId"].is_string()
                            {
                                let id = retirement_id.fetch_add(1, Ordering::Relaxed);
                                let mut frame=serde_json::to_vec(&json!({"id":id,"method":"release","params":{"leaseId":result["leaseId"]}})).unwrap();
                                frame.push(b'\n');
                                let _ = retirement_input.lock().await.write_all(&frame).await;
                            }
                        }
                    }
                }
            }
            for (_, pending) in readers.lock().await.drain() {
                let _ = pending.reply.send(Err(GatewayError::new(
                    "RESOURCE_RUNTIME_EXITED",
                    "资源服务退出，现有未知预约保留隔离。",
                    503,
                )));
            }
        });
        let service = Arc::new(Self {
            child: Mutex::new(child),
            input,
            pending,
            next_id,
        });
        service
            .request("snapshot", json!({}), Duration::from_secs(10))
            .await?;
        Ok(service)
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.request_id(id, method, params, timeout).await
    }

    async fn request_id(
        &self,
        id: u64,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.len() >= 128 {
                return Err(GatewayError::new(
                    "RESOURCE_QUEUE_FULL",
                    "资源请求队列已满。",
                    503,
                ));
            }
            pending.insert(
                id,
                Pending {
                    reply: sender,
                    admission: method == "acquire",
                },
            );
        }
        let mut frame =
            serde_json::to_vec(&json!({"id":id,"method":method,"params":params})).unwrap();
        frame.push(b'\n');
        if self.input.lock().await.write_all(&frame).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err(GatewayError::new(
                "RESOURCE_RUNTIME_WRITE_FAILED",
                "资源请求未确认发送。",
                503,
            ));
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            _ => {
                if method != "acquire" {
                    self.pending.lock().await.remove(&id);
                }
                // Cancel only an admission attempt. Released or unknown effects are never fabricated.
                // 仅取消等待准入；不虚构预约释放或未知执行结果。
                if method == "acquire" {
                    let cancel_id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    let mut cancel = serde_json::to_vec(
                        &json!({"id":cancel_id,"method":"cancelAcquire","params":{"requestId":id}}),
                    )
                    .unwrap();
                    cancel.push(b'\n');
                    let _ = self.input.lock().await.write_all(&cancel).await;
                }
                Err(GatewayError::new(
                    "RESOURCE_WAIT_TIMEOUT",
                    "等待执行资源超时，已有结果保留。",
                    504,
                ))
            }
        }
    }

    pub async fn acquire(
        self: &Arc<Self>,
        params: Value,
        cancellation: watch::Receiver<bool>,
    ) -> Result<ResourceLease, GatewayError> {
        let ttl_ms = params["ttlMs"].as_u64().unwrap_or(30_000);
        let receipt = self.acquire_receipt(params, cancellation).await?;
        self.lease_from_receipt(receipt, ttl_ms)
    }

    /// Keep accepted foreground work waiting across admission windows until capacity returns or its owner stops it.
    /// 已接受的前台任务跨准入等待窗口继续等待，资源恢复后执行，直到所属任务明确停止。
    pub async fn acquire_waiting(
        self: &Arc<Self>,
        params: Value,
        cancellation: watch::Receiver<bool>,
    ) -> Result<ResourceLease, GatewayError> {
        let ttl_ms = params["ttlMs"].as_u64().unwrap_or(30_000);
        let receipt = wait_for_resource_response(
            || self.acquire_receipt(params.clone(), cancellation.clone()),
            cancellation.clone(),
        )
        .await?;
        self.lease_from_receipt(receipt, ttl_ms)
    }

    async fn acquire_receipt(
        self: &Arc<Self>,
        params: Value,
        mut cancellation: watch::Receiver<bool>,
    ) -> Result<Value, GatewayError> {
        if *cancellation.borrow() {
            return Err(GatewayError::new(
                "RESOURCE_CANCELLED",
                "资源等待已取消。",
                499,
            ));
        }
        let wait_ms = params["waitMs"].as_u64().unwrap_or(30_000);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let admission_service = self.clone();
        let mut admission = PendingAdmission {
            service: self.clone(),
            request_id: id,
            attempt: Some(tokio::spawn(async move {
                admission_service
                    .request_id(id, "acquire", params, Duration::from_millis(wait_ms + 5000))
                    .await
            })),
        };
        let result = tokio::select! {
            result = admission.attempt.as_mut().expect("pending admission attempt") => result,
            _ = cancellation.changed() => {
                return Err(GatewayError::new("RESOURCE_CANCELLED", "资源等待已取消。", 499));
            }
        };
        // The selected branch consumed the settled receipt; its caller now owns any granted allocation.
        // 已选分支取得结算回执后，授予资源交由调用方持有；其余退出统一经过待准入保护清理。
        admission.attempt.take();
        result.map_err(|_| GatewayError::new("RESOURCE_RUNTIME_EXITED", "资源等待任务退出。", 503))?
    }

    fn lease_from_receipt(
        self: &Arc<Self>,
        result: Value,
        ttl_ms: u64,
    ) -> Result<ResourceLease, GatewayError> {
        if result["status"] != "granted" {
            let code = result["reason"].as_str().unwrap_or("RESOURCE_INVALID_RECEIPT");
            let capacity = matches!(code, "RESOURCE_PRESSURE" | "RESOURCE_WAIT_TIMEOUT" | "RESOURCE_QUEUE_FULL" | "RESOURCE_LEASE_LIMIT");
            return Err(GatewayError::new(
                code,
                if capacity { "当前硬件资源不足，正在等待可用资源。" }
                else { "资源请求未通过协议校验，已有进度已保留。" },
                if code == "RESOURCE_INVALID_REQUEST" { 400 } else { 503 },
            ));
        }
        let lease_id = result["leaseId"]
            .as_str()
            .ok_or_else(|| {
                GatewayError::new("RESOURCE_INVALID_RECEIPT", "资源服务未返回有效租约。", 503)
            })?
            .to_owned();
        let (stop, mut stopped) = watch::channel(false);
        let renewal_service = self.clone();
        let renewal_id = lease_id.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis((ttl_ms / 3).max(1000)));
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    _ = timer.tick() => { let _ = renewal_service.request("renew", json!({"leaseId":renewal_id,"ttlMs":ttl_ms}), Duration::from_secs(5)).await; }
                }
            }
        });
        Ok(ResourceLease {
            service: self.clone(),
            lease_id,
            stop,
            released: false,
            release_on_drop: true,
            allocation: result,
        })
    }

    pub async fn close(&self) {
        let _ = self
            .request("close", json!({}), Duration::from_secs(2))
            .await;
        let mut child = self.child.lock().await;
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }
}

/// Retry only acknowledged admission denials: no model or tool action has started at this boundary.
/// 仅重排已确认被拒的准入；此边界尚未发起模型或工具操作，通信未知不得自动重派。
async fn wait_for_resource_response<Request, Response>(
    mut request: Request,
    mut cancellation: watch::Receiver<bool>,
) -> Result<Value, GatewayError>
where
    Request: FnMut() -> Response,
    Response: Future<Output = Result<Value, GatewayError>>,
{
    loop {
        if *cancellation.borrow() {
            return Err(GatewayError::new("RESOURCE_CANCELLED", "资源等待已取消。", 499));
        }
        let receipt = request().await?;
        if receipt["status"] != "denied"
            || !matches!(
                receipt["reason"].as_str(),
                Some("RESOURCE_PRESSURE" | "RESOURCE_WAIT_TIMEOUT" | "RESOURCE_QUEUE_FULL" | "RESOURCE_LEASE_LIMIT")
            )
        {
            return Ok(receipt);
        }
        // Yield between full queues; normal pressure still waits in the native queue for its bounded window.
        // 队列满时让出调度，普通压力仍在原生队列中按窗口等待，不紧密轮询或占用 UI 线程。
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {},
            _ = cancellation.changed() => {
                if *cancellation.borrow() || cancellation.has_changed().is_err() {
                    return Err(GatewayError::new("RESOURCE_CANCELLED", "资源等待已取消。", 499));
                }
            }
        }
    }
}

pub struct ResourceLease {
    service: Arc<ResourceService>,
    lease_id: String,
    stop: watch::Sender<bool>,
    released: bool,
    release_on_drop: bool,
    pub allocation: Value,
}
impl ResourceLease {
    /// Cancelling an external dispatch does not prove its work stopped; expiry retains its quarantined reservation.
    /// 取消外部派发不证明任务已停止；停止续租后，资源服务到期仍保留隔离预约，核验完成才显式释放。
    pub fn retain_unknown_on_drop(&mut self) {
        self.release_on_drop = false;
    }

    pub async fn release_confirmed(&mut self) -> Result<Value, GatewayError> {
        if self.released {
            return Ok(json!({"status":"released","leaseId":self.lease_id,"reused":true}));
        }
        let receipt = self
            .service
            .request(
                "release",
                json!({"leaseId":self.lease_id}),
                Duration::from_secs(5),
            )
            .await?;
        if receipt["status"] != "released" {
            return Err(GatewayError::new(
                "RESOURCE_RELEASE_UNCONFIRMED",
                "资源释放尚未确认，原预约继续保留。",
                503,
            ));
        }
        self.released = true;
        let _ = self.stop.send(true);
        Ok(receipt)
    }
    pub async fn release(mut self) {
        let _ = self.release_confirmed().await;
    }
}
impl Drop for ResourceLease {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if self.released || !self.release_on_drop {
            return;
        }
        let service = self.service.clone();
        let id = self.lease_id.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = service
                    .request("release", json!({"leaseId":id}), Duration::from_secs(5))
                    .await;
            });
        }
    }
}

pub fn install(service: Arc<ResourceService>) {
    let _ = GLOBAL.set(service);
}
pub fn global() -> Option<Arc<ResourceService>> {
    GLOBAL.get().cloned()
}

#[cfg(test)]
mod admission_wait_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn foreground_admission_continues_after_an_expired_wait_window() {
        let (_owner, cancellation) = watch::channel(false);
        let attempts = AtomicUsize::new(0);
        let result = wait_for_resource_response(
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(if attempt == 0 {
                        json!({"status":"denied","reason":"RESOURCE_WAIT_TIMEOUT"})
                    } else {
                        json!({"status":"granted","leaseId":"resumed-lease"})
                    })
                }
            },
            cancellation,
        )
        .await
        .unwrap();
        assert_eq!(result["leaseId"], "resumed-lease");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn user_pause_stops_resource_waiting_without_another_admission() {
        let (owner, cancellation) = watch::channel(false);
        let attempts = AtomicUsize::new(0);
        let stopped = wait_for_resource_response(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                owner.send_replace(true);
                async { Ok(json!({"status":"denied","reason":"RESOURCE_PRESSURE"})) }
            },
            cancellation,
        )
        .await
        .unwrap_err();
        assert_eq!(stopped.code, "RESOURCE_CANCELLED");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resource_transport_uncertainty_is_not_retried_as_a_capacity_denial() {
        let (_owner, cancellation) = watch::channel(false);
        let attempts = AtomicUsize::new(0);
        let failure = wait_for_resource_response(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async {
                    Err(GatewayError::new(
                        "RESOURCE_WAIT_TIMEOUT",
                        "The resource transport did not settle.",
                        504,
                    ))
                }
            },
            cancellation,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.code, "RESOURCE_WAIT_TIMEOUT");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalid_resource_parameters_do_not_enter_the_capacity_wait_loop() {
        let (_owner, cancellation) = watch::channel(false);
        let attempts = AtomicUsize::new(0);
        let receipt = wait_for_resource_response(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Ok(json!({"status":"denied","reason":"RESOURCE_INVALID_REQUEST"})) }
        }, cancellation).await.unwrap();
        assert_eq!(receipt["reason"], "RESOURCE_INVALID_REQUEST");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires KYNXA_TEST_RESOURCE_SERVICE pointing to the independently built resource helper"]
    async fn dropped_admission_future_cancels_queue_and_releases_a_racing_grant() {
        let executable = std::env::var_os("KYNXA_TEST_RESOURCE_SERVICE")
            .expect("set KYNXA_TEST_RESOURCE_SERVICE to the isolated resource-service executable");
        let service = ResourceService::start(Path::new(&executable)).await.unwrap();
        let (_owner, cancellation) = watch::channel(false);
        for grant_before_drop in [false, true] {
            let snapshot = service.request("snapshot", json!({}), Duration::from_secs(2)).await.unwrap();
            // Import exact owned CPU debt so this fixture is independent of adaptive grants under host pressure.
            // 恢复精确的自有 CPU 预约，使夹具不受宿主压力下的自适应授额影响。
            let restored = service.request("restore", json!({"leases":[{
                "leaseId":"drop-owner","taskId":"drop-owner",
                "expiresAt":chrono::Utc::now().timestamp_millis() + 30000,
                "cpuThreads":snapshot["budget"]["cpuThreads"],"memoryBytes":0,"gpuMemoryBytes":0,
            }]}), Duration::from_secs(2)).await.unwrap();
            assert_eq!(restored["status"], "restored");
            let owner = service.lease_from_receipt(
                json!({"status":"granted","leaseId":"drop-owner"}), 30000,
            ).unwrap();
            let mut owner = Some(owner);
            let mut admission = Box::pin(service.acquire(json!({
                "taskId":"drop-waiter","workspaceId":"isolated-drop-test","kind":"foreground",
                "workload":"compute","cpuThreads":1,"memoryBytes":0,"gpuMemoryBytes":0,
                "waitMs":30000,"ttlMs":30000,
            }), cancellation.clone()));
            tokio::select! {
                _ = &mut admission => panic!("fixture must wait behind its owner"),
                queued = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let snapshot = service.request("snapshot", json!({}), Duration::from_secs(2)).await.unwrap();
                        if snapshot["queuedRequests"] == 1 { break; }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }) => queued.expect("fixture admission was not queued"),
            }
            if grant_before_drop {
                owner.take().unwrap().release().await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let snapshot = service.request("snapshot", json!({}), Duration::from_secs(2)).await.unwrap();
                        if snapshot["queuedRequests"] == 0 && snapshot["activeLeases"] == 1 { break; }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.unwrap();
            }
            // Drop the original future without sending a cancellation signal, as preparation timeout does.
            // 如预准备超时一样直接丢弃原 future，不通过取消信号替代这条释放路径。
            drop(admission);
            let expected_leases = u64::from(owner.is_some());
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let snapshot = service.request("snapshot", json!({}), Duration::from_secs(2)).await.unwrap();
                    if snapshot["queuedRequests"] == 0 && snapshot["activeLeases"] == expected_leases { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            if let Some(owner) = owner { owner.release().await; }
        }
        service.close().await;
    }

    #[tokio::test]
    #[ignore = "requires KYNXA_TEST_RESOURCE_SERVICE pointing to the independently built resource helper"]
    async fn unknown_external_dispatch_keeps_actual_reservation_after_lease_expiry() {
        let executable = std::env::var_os("KYNXA_TEST_RESOURCE_SERVICE")
            .expect("set KYNXA_TEST_RESOURCE_SERVICE to the isolated resource-service executable");
        let service = ResourceService::start(Path::new(&executable))
            .await
            .unwrap();
        let (_owner, cancellation) = watch::channel(false);
        let mut lease = service
            .acquire(
                json!({"taskId":"unknown-dispatch-fixture","workspaceId":"isolated-preparation",
                "kind":"foreground","workload":"compute","cpuThreads":1,"memoryBytes":1048576,
                "gpuMemoryBytes":0,"waitMs":3000,"ttlMs":1000}),
                cancellation,
            )
            .await
            .unwrap();
        let lease_id = lease.lease_id.clone();
        let mut stopped = lease.stop.subscribe();
        lease.retain_unknown_on_drop();
        drop(lease);
        stopped.changed().await.unwrap();
        assert!(*stopped.borrow());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = service
                    .request("snapshot", json!({}), Duration::from_secs(2))
                    .await
                    .unwrap();
                let current = snapshot["accounting"]["leases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["leaseId"] == lease_id)
                    .expect("unknown work must remain in the actual resource ledger");
                assert_eq!(current["memoryBytes"], 1048576);
                if current["state"] == "quarantined" {
                    assert!(snapshot["quarantinedLeases"].as_u64().unwrap() >= 1);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        let released = service
            .request(
                "release",
                json!({"leaseId":lease_id}),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(released["status"], "released");
        let snapshot = service
            .request("snapshot", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(
            !snapshot["accounting"]["leases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["leaseId"] == lease_id)
        );
        service.close().await;
    }
}
