//! Native resource-service transport; one process remains the hardware allocation authority.
//! 原生资源服务传输；硬件授额仍由唯一资源服务进程负责。

use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
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
        mut cancellation: watch::Receiver<bool>,
    ) -> Result<ResourceLease, GatewayError> {
        if *cancellation.borrow() {
            return Err(GatewayError::new(
                "RESOURCE_CANCELLED",
                "资源等待已取消。",
                499,
            ));
        }
        let wait_ms = params["waitMs"].as_u64().unwrap_or(30_000);
        let ttl_ms = params["ttlMs"].as_u64().unwrap_or(30_000);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let admission_service = self.clone();
        let mut attempt = tokio::spawn(async move {
            admission_service
                .request_id(id, "acquire", params, Duration::from_millis(wait_ms + 5000))
                .await
        });
        let result = tokio::select! {
            result = &mut attempt => result.map_err(|_| GatewayError::new("RESOURCE_RUNTIME_EXITED", "资源等待任务退出。", 503))??,
            _ = cancellation.changed() => {
                // Keep the original attempt alive to retire a grant that raced with cancellation.
                // 原准入请求继续接收回执，释放与取消竞争时已授予的租约，不能提前删除账目。
                let retirement_service = self.clone();
                tokio::spawn(async move {
                    let _ = retirement_service.request("cancelAcquire", json!({"requestId":id}), Duration::from_secs(5)).await;
                    if let Ok(Ok(receipt)) = attempt.await {
                        if let Some(lease) = receipt["leaseId"].as_str().filter(|_| receipt["status"] == "granted") {
                            let _ = retirement_service.request("release", json!({"leaseId":lease}), Duration::from_secs(5)).await;
                        }
                    }
                });
                return Err(GatewayError::new("RESOURCE_CANCELLED", "资源等待已取消。", 499));
            }
        };
        if result["status"] != "granted" {
            return Err(GatewayError::new(
                result["reason"].as_str().unwrap_or("RESOURCE_PRESSURE"),
                "当前硬件资源不足，等待或暂停后台任务后可继续。",
                503,
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

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
