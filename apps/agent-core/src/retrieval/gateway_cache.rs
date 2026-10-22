//! Optional rebuildable caches share the existing hardware authority and independent observers.
//! 可重建缓存复用现有硬件授额；共享计算的各个观察者独立取消，缓存不足直接走原链路。

use super::{
    cache::ByteLru,
    cache_budget::{BudgetController, CacheKind, Feedback, HardwareHeadroom},
};
use crate::{
    resources::gateway::{self as hardware, ResourceLease},
    storage::{ContractError, gateway},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) struct MemoryGrant {
    lease: Option<ResourceLease>,
    runtime: tokio::runtime::Handle,
    bytes: usize,
}
impl Drop for MemoryGrant {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            self.runtime.spawn(async move {
                lease.release().await;
            });
        }
    }
}
struct MemorySlot {
    grant: Option<Arc<MemoryGrant>>,
    desired: usize,
    reclaim: fn(),
    generation: u64,
}
struct MemoryState {
    slots: HashMap<&'static str, MemorySlot>,
    controller: BudgetController,
    started: Instant,
    last_refresh: Instant,
    refreshing: bool,
    epoch: u64,
}
impl Default for MemoryState {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            controller: BudgetController::default(),
            started: Instant::now(),
            last_refresh: Instant::now() - Duration::from_secs(10),
            refreshing: false,
            epoch: 0,
        }
    }
}
static MEMORY: OnceLock<Mutex<MemoryState>> = OnceLock::new();
static MONITOR_STARTED: AtomicBool = AtomicBool::new(false);

fn resize_cache(kind: CacheKind, bytes: usize) {
    match kind {
        CacheKind::QueryVectors => super::gateway_embedding::resize_query_cache(bytes),
        CacheKind::Passages => super::cache_passages::set_capacity(bytes),
        CacheKind::Candidates => super::cache_candidates::set_capacity(bytes),
    }
}
fn cache_feedback(kind: CacheKind) -> Feedback {
    match kind {
        CacheKind::QueryVectors => super::gateway_embedding::query_cache_feedback(),
        CacheKind::Passages => super::cache_passages::budget_feedback(),
        CacheKind::Candidates => super::cache_candidates::budget_feedback(),
    }
}
fn number_bytes(value: &Value) -> usize {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
        .map_or(0, |n| n.min(usize::MAX as f64) as usize)
}
fn environment_limit(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()?
        .parse::<usize>()
        .ok()
        .map(|mib| mib.saturating_mul(super::cache_budget::MIB))
}
fn hardware_headroom(snapshot: &Value, approved: usize) -> HardwareHeadroom {
    HardwareHeadroom {
        available_bytes: number_bytes(&snapshot["accounting"]["availableMemoryBytes"]),
        capacity_bytes: number_bytes(&snapshot["accounting"]["capacityMemoryBytes"])
            .max(number_bytes(&snapshot["budget"]["memoryBytes"])),
        physical_total_bytes: snapshot["memory"]["totalBytes"]
            .as_f64()
            .map(|_| number_bytes(&snapshot["memory"]["totalBytes"])),
        physical_available_bytes: snapshot["memory"]["availableBytes"]
            .as_f64()
            .map(|_| number_bytes(&snapshot["memory"]["availableBytes"])),
        approved_cache_bytes: approved,
        queued: snapshot["queuedRequests"].as_f64().unwrap_or(0.0) > 0.0,
        // Only explicit local foreground measurements qualify; model/provider latency is excluded.
        // 仅采纳明确的本地前台反馈，不把供应商网络耗时或模型思考耗时当作界面延迟。
        foreground_latency_micros: snapshot["feedback"]["foregroundLatencyMs"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(|n| (n * 1000.0).min(u64::MAX as f64) as u64),
    }
}

/// Reuse the existing memory thresholds, excluding queue/CPU demand and untrusted missing telemetry.
/// 复用已有内存阈值；队列或CPU准入需求不等于缺内存，缺失或不可信观测不触发模型卸载。
pub(crate) fn confirmed_memory_pressure(snapshot:&Value)->bool {
    let valid_pair=|total:&Value,free:&Value| total.as_f64().zip(free.as_f64()).is_some_and(|(total,free)|
        total.is_finite() && total>0.0 && total<usize::MAX as f64 && free.is_finite() && free>=0.0 && free<=total);
    let accounting=valid_pair(&snapshot["accounting"]["capacityMemoryBytes"],&snapshot["accounting"]["availableMemoryBytes"]);
    let physical=valid_pair(&snapshot["memory"]["totalBytes"],&snapshot["memory"]["availableBytes"]);
    if !accounting && !physical {return false;}
    let mut headroom=hardware_headroom(snapshot,0);
    if accounting {headroom.capacity_bytes=number_bytes(&snapshot["accounting"]["capacityMemoryBytes"]);}
    else {headroom.available_bytes=headroom.capacity_bytes;}
    if !physical {headroom.physical_total_bytes=None;headroom.physical_available_bytes=None;}
    headroom.memory_pressured()
}

pub(crate) fn configure_cache_limit(name: &'static str, limit: Option<usize>) {
    let Some(kind) = CacheKind::from_name(name) else {
        return;
    };
    let mut retired = None;
    if let Ok(mut state) = MEMORY.get_or_init(Default::default).lock() {
        if state.controller.manual_limit(kind, limit) {
            state.epoch = state.epoch.saturating_add(1);
            state.refreshing = false;
            if let Some(slot) = state.slots.get_mut(name) {
                slot.generation = slot.generation.saturating_add(1);
                if let Some(limit) = limit {
                    slot.desired = slot.desired.min(limit);
                    if limit == 0 {
                        (slot.reclaim)();
                        retired = slot.grant.take();
                    } else if slot.grant.as_ref().is_some_and(|grant| grant.bytes > limit) {
                        resize_cache(kind, limit);
                    }
                }
            }
        }
    }
    drop(retired);
}
/// Record only local lookup/coordination work, never cloud TTFT or whole model execution.
/// 仅记录本地查找和协调开销，不录入云端首字时间或整个模型执行时长。
pub(crate) fn record_local_foreground_latency(elapsed: Duration) {
    if let Some(state) = MEMORY.get() {
        if let Ok(mut state) = state.try_lock() {
            state
                .controller
                .latency(elapsed.as_micros().min(u64::MAX as u128) as u64);
        }
    }
}
pub(crate) fn budget_status() -> Value {
    MEMORY
        .get()
        .and_then(|state| state.try_lock().ok())
        .map(|state| {
            let mut value = state.controller.stats();
            value["approvedBytes"] = json!(
                state
                    .slots
                    .values()
                    .filter_map(|slot| slot.grant.as_ref())
                    .fold(0usize, |total, grant| total.saturating_add(grant.bytes))
            );
            value["allocations"] = json!(
                state
                    .slots
                    .iter()
                    .map(
                        |(name, slot)| json!({"name":name,"desiredBytes":slot.desired,
            "approvedBytes":slot.grant.as_ref().map_or(0,|grant|grant.bytes)})
                    )
                    .collect::<Vec<_>>()
            );
            value
        })
        .unwrap_or_else(|| json!({"automatic":true,"state":"not-sampled"}))
}

fn monitor_pressure(
    runtime: &tokio::runtime::Handle,
    service: std::sync::Weak<hardware::ResourceService>,
) {
    if MONITOR_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    runtime.spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let registered = MEMORY
                .get()
                .and_then(|state| state.lock().ok())
                .is_some_and(|state| !state.slots.is_empty());
            if !registered {
                continue;
            }
            let Some(service) = service.upgrade() else {
                reclaim_memory("resource-service-unavailable");
                break;
            };
            let can_refresh = MEMORY
                .get()
                .and_then(|state| state.lock().ok())
                .is_some_and(|mut state| {
                    if state.refreshing {
                        false
                    } else {
                        state.refreshing = true;
                        true
                    }
                });
            if can_refresh {
                refresh_memory(service, runtime_handle()).await;
            }
        }
        MONITOR_STARTED.store(false, Ordering::Release);
    });
}
fn runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

async fn refresh_memory(service: Arc<hardware::ResourceService>, runtime: tokio::runtime::Handle) {
    let Some(epoch) = MEMORY
        .get()
        .and_then(|state| state.lock().ok())
        .map(|state| state.epoch)
    else {
        return;
    };
    let snapshot = service
        .request("snapshot", json!({}), Duration::from_millis(150))
        .await;
    let Ok(snapshot) = snapshot else {
        reclaim_memory("resource-service-unavailable");
        return;
    };
    let feedback = CacheKind::ALL.map(|kind| (kind, cache_feedback(kind)));
    let plans = {
        let Some(mut state) = MEMORY.get().and_then(|state| state.lock().ok()) else {
            return;
        };
        if state.epoch != epoch {
            return;
        }
        let approved = state
            .slots
            .values()
            .filter_map(|slot| slot.grant.as_ref())
            .fold(0usize, |total, grant| total.saturating_add(grant.bytes));
        let hardware = hardware_headroom(&snapshot, approved);
        for (kind, feedback) in feedback {
            state.controller.observe(kind, feedback);
        }
        state
            .controller
            .total_limit(environment_limit("KYNXA_CACHE_TOTAL_MIB"));
        let now = state.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        state.controller.update(now, hardware);
        let targets = CacheKind::ALL.map(|kind| (kind, state.controller.target(kind)));
        let mut plans = Vec::new();
        for (kind, target) in targets {
            if let Some(slot) = state.slots.get_mut(kind.name()) {
                slot.desired = target;
                plans.push((
                    kind,
                    target,
                    slot.generation,
                    slot.grant.as_ref().map_or(0, |grant| grant.bytes),
                ));
            }
        }
        plans
    };
    for (kind, target, generation, current) in plans {
        if target == current {
            continue;
        }
        if target == 0 {
            retire_slot(kind, generation);
            continue;
        }
        let (_sender, receiver) = tokio::sync::watch::channel(false);
        let request = json!({"taskId":gateway::new_id(),"workspaceId":"local","kind":"background","cpuThreads":0,
            "memoryBytes":target,"gpuMemoryBytes":0,"ttlMs":30000,"waitMs":0});
        let grant = match tokio::time::timeout(
            Duration::from_millis(150),
            service.acquire(request, receiver),
        )
        .await
        {
            Ok(Ok(lease)) => Arc::new(MemoryGrant {
                lease: Some(lease),
                runtime: runtime.clone(),
                bytes: target,
            }),
            _ => {
                if target < current {
                    retire_slot(kind, generation);
                }
                continue;
            }
        };
        let old = {
            let Some(mut state) = MEMORY.get().and_then(|state| state.lock().ok()) else {
                continue;
            };
            let Some(slot) = state
                .slots
                .get_mut(kind.name())
                .filter(|slot| slot.generation == generation && slot.desired == target)
            else {
                continue;
            };
            // Resizing is pure memory work under the publication lock; no admission wait holds this lock.
            // 容量与真实授额在发布锁内一起更新；授额等待已在锁外完成，旧请求不能覆盖新收缩。
            resize_cache(kind, target);
            slot.grant.replace(grant)
        };
        drop(old);
    }
    if let Some(mut state) = MEMORY.get().and_then(|state| state.lock().ok()) {
        if state.epoch == epoch {
            state.refreshing = false;
            state.last_refresh = Instant::now();
        }
    }
}
fn retire_slot(kind: CacheKind, generation: u64) {
    let retired = {
        let Some(mut state) = MEMORY.get().and_then(|state| state.lock().ok()) else {
            return;
        };
        let Some(slot) = state
            .slots
            .get_mut(kind.name())
            .filter(|slot| slot.generation == generation)
        else {
            return;
        };
        slot.generation = slot.generation.saturating_add(1);
        (slot.reclaim)();
        slot.grant.take()
    };
    drop(retired);
}

/// Targets adapt globally; only existing, already-approved grants are visible to a foreground lookup.
/// 三类目标统一自适应，前台仅使用现有已批准授额；缓存不足旁路原链路，不排队、不结束长任务。
pub(crate) fn cache_memory_limit(name: &'static str, seed_bytes: usize, reclaim: fn()) -> usize {
    let Some(kind) = CacheKind::from_name(name) else {
        return 0;
    };
    let Ok(runtime) = super::gateway_embedding::runtime() else {
        return 0;
    };
    let Some(service) = hardware::global() else {
        return 0;
    };
    if kind == CacheKind::Candidates {
        configure_cache_limit(name, environment_limit("KYNXA_CANDIDATE_CACHE_MIB"));
    } else if kind == CacheKind::Passages {
        configure_cache_limit(name, environment_limit("KYNXA_PASSAGE_CACHE_MIB"));
    }
    monitor_pressure(&runtime, Arc::downgrade(&service));
    let Ok(mut state) = MEMORY.get_or_init(Default::default).try_lock() else {
        return 0;
    };
    state.controller.register(kind, seed_bytes);
    state.slots.entry(name).or_insert(MemorySlot {
        grant: None,
        desired: 0,
        reclaim,
        generation: 0,
    });
    let approved = state
        .slots
        .get(name)
        .and_then(|slot| slot.grant.as_ref())
        .map_or(0, |grant| grant.bytes)
        .min(state.controller.manual_cap(kind).unwrap_or(usize::MAX));
    if state.refreshing || state.last_refresh.elapsed() < Duration::from_secs(2) {
        return approved;
    }
    state.refreshing = true;
    drop(state);
    let background_runtime = runtime.clone();
    runtime.spawn(async move {
        refresh_memory(service, background_runtime).await;
    });
    approved
}
pub(crate) fn memory_grant(name: &'static str) -> Option<Arc<MemoryGrant>> {
    MEMORY
        .get()?
        .try_lock()
        .ok()?
        .slots
        .get(name)?
        .grant
        .clone()
}

/// Reclamation never releases a worker's grant until its real computation has returned.
/// 回收只清理可重建值，活动工作者持有授额直到真实计算返回，不清会话、任务或正式检索数据。
pub(crate) fn reclaim_memory(_reason: &'static str) {
    let retired = {
        let Some(mut state) = MEMORY.get().and_then(|state| state.lock().ok()) else {
            return;
        };
        state.epoch = state.epoch.saturating_add(1);
        state.refreshing = false;
        state.last_refresh = Instant::now();
        state.controller.reclaimed();
        state
            .slots
            .values_mut()
            .map(|slot| {
                slot.generation = slot.generation.saturating_add(1);
                slot.desired = 0;
                (slot.reclaim)();
                slot.grant.take()
            })
            .collect::<Vec<_>>()
    };
    drop(retired);
}
struct Flight<V> {
    result: Mutex<Option<Result<Option<Arc<V>>, ContractError>>>,
    ready: Condvar,
    cancel: Arc<AtomicBool>,
    observers: AtomicUsize,
}
struct Observer<V>(Arc<Flight<V>>);
impl<V> Drop for Observer<V> {
    fn drop(&mut self) {
        if self.0.observers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.cancel.store(true, Ordering::Release);
        }
    }
}
struct FlightState<V> {
    values: ByteLru<V>,
    active: HashMap<String, Arc<Flight<V>>>,
    pending_bytes: usize,
    capacity: usize,
    coalesced: u64,
    computed_micros: u64,
    estimated_saved_micros: u64,
}
pub(crate) struct FlightCache<V> {
    state: Mutex<FlightState<V>>,
}
pub(crate) enum Lookup<V> {
    Bypass,
    Value { value: Option<Arc<V>>, reused: bool },
}

impl<V: Send + Sync + 'static> FlightCache<V> {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FlightState {
                values: ByteLru::new(0),
                active: HashMap::new(),
                pending_bytes: 0,
                capacity: 0,
                coalesced: 0,
                computed_micros: 0,
                estimated_saved_micros: 0,
            }),
        })
    }
    pub(crate) fn capacity(&self, bytes: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.capacity = bytes;
            let available = bytes.saturating_sub(state.pending_bytes);
            state.values.set_capacity(available);
        }
    }
    pub(crate) fn get(&self, key: &str) -> Option<Arc<V>> {
        self.state.lock().ok()?.values.get(key)
    }
    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .map_or(u64::MAX, |state| state.values.generation())
    }
    pub(crate) fn insert_generation(
        &self,
        key: String,
        value: Arc<V>,
        bytes: usize,
        generation: u64,
    ) {
        if let Ok(mut state) = self.state.lock() {
            if state.values.generation() == generation {
                state.values.insert(key, value, bytes);
            }
        }
    }
    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.capacity = 0;
            state.values.invalidate("cache-memory-reclaimed");
            state.values.set_capacity(0);
        }
    }
    pub(crate) fn stats(&self) -> Value {
        let Ok(state) = self.state.lock() else {
            return json!({"available":false});
        };
        let mut value = state.values.stats();
        value["pendingBytes"] = json!(state.pending_bytes);
        value["residentBytes"] = json!(
            state
                .pending_bytes
                .saturating_add(state.values.occupied_bytes())
        );
        value["budgetBytes"] = json!(state.capacity);
        value["inFlight"] = json!(state.active.len());
        value["coalesced"] = json!(state.coalesced);
        value["measuredComputeMicros"] = json!(state.computed_micros);
        value["estimatedSavedComputeMicros"] = json!(state.estimated_saved_micros);
        value
    }
    pub(crate) fn measured_compute(&self, micros: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.computed_micros = state.computed_micros.saturating_add(micros);
        }
    }
    pub(crate) fn saved_compute(&self, micros: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.estimated_saved_micros = state.estimated_saved_micros.saturating_add(micros);
        }
    }
    pub(crate) fn feedback(&self) -> Feedback {
        let Ok(state) = self.state.lock() else {
            return Feedback::default();
        };
        let stats = state.values.stats();
        Feedback {
            hits: stats["hits"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(state.coalesced),
            misses: stats["misses"].as_u64().unwrap_or(0),
            evictions: stats["evictions"].as_u64().unwrap_or(0),
            occupied_bytes: state.values.occupied_bytes(),
            computed_micros: state.computed_micros,
            estimated_saved_micros: state.estimated_saved_micros,
        }
    }
    pub(crate) fn compute<F>(
        self: &Arc<Self>,
        key: String,
        input_bytes: usize,
        caller_cancel: Arc<AtomicBool>,
        grant: Option<Arc<MemoryGrant>>,
        retain: bool,
        compute: F,
    ) -> Result<Lookup<V>, ContractError>
    where
        F: FnOnce(Arc<AtomicBool>) -> Result<Option<(V, usize)>, ContractError> + Send + 'static,
    {
        if caller_cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        // Thread stacks are admitted too; full optional capacity bypasses cache without rejecting inference.
        // 共享工作线程栈也计入缓存授额；可选额度不足只旁路缓存，不拒绝必要推理。
        const STACK_BYTES: usize = 2 * 1024 * 1024;
        let charge = input_bytes
            .saturating_add(key.capacity())
            .saturating_add(512)
            .saturating_add(STACK_BYTES);
        let (flight, leader, generation) = {
            let Ok(mut state) = self.state.lock() else {
                return Ok(Lookup::Bypass);
            };
            if retain {
                if let Some(value) = state.values.get(&key) {
                    return Ok(Lookup::Value {
                        value: Some(value),
                        reused: true,
                    });
                }
            }
            if let Some(flight) = state
                .active
                .get(&key)
                .filter(|flight| !flight.cancel.load(Ordering::Acquire))
                .cloned()
            {
                flight.observers.fetch_add(1, Ordering::AcqRel);
                state.coalesced = state.coalesced.saturating_add(1);
                (flight, false, state.values.generation())
            } else {
                if charge > state.capacity.saturating_sub(state.pending_bytes) {
                    return Ok(Lookup::Bypass);
                }
                state.pending_bytes = state.pending_bytes.saturating_add(charge);
                let available = state.capacity.saturating_sub(state.pending_bytes);
                state.values.set_capacity(available);
                let flight = Arc::new(Flight {
                    result: Mutex::new(None),
                    ready: Condvar::new(),
                    cancel: Arc::new(AtomicBool::new(false)),
                    observers: AtomicUsize::new(1),
                });
                state.active.insert(key.clone(), flight.clone());
                (flight, true, state.values.generation())
            }
        };
        let observer = Observer(flight.clone());
        if leader {
            let cache = self.clone();
            let worker_flight = flight.clone();
            let worker_key = key.clone();
            let spawned = std::thread::Builder::new()
                .name("retrieval-cache-inference".into())
                .stack_size(STACK_BYTES)
                .spawn(move || {
                    let _grant = grant;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        compute(worker_flight.cancel.clone())
                    }))
                    .unwrap_or_else(|_| {
                        Err(gateway::error(
                            "检索计算失败，可重新尝试。",
                            "RETRIEVAL_INFERENCE_FAILED",
                            503,
                        ))
                    });
                    let (result, bytes) = match result {
                        Ok(Some((value, bytes))) => (Ok(Some(Arc::new(value))), bytes),
                        Ok(None) => (Ok(None), 0),
                        Err(error) => (Err(error), 0),
                    };
                    if let Ok(mut state) = cache.state.lock() {
                        state.pending_bytes = state.pending_bytes.saturating_sub(charge);
                        let available = state.capacity.saturating_sub(state.pending_bytes);
                        state.values.set_capacity(available);
                        if retain
                            && state.values.generation() == generation
                            && !worker_flight.cancel.load(Ordering::Acquire)
                        {
                            if let Ok(Some(value)) = &result {
                                state
                                    .values
                                    .insert(worker_key.clone(), value.clone(), bytes);
                            }
                        }
                        if state
                            .active
                            .get(&worker_key)
                            .is_some_and(|active| Arc::ptr_eq(active, &worker_flight))
                        {
                            state.active.remove(&worker_key);
                        }
                    }
                    if let Ok(mut delivered) = worker_flight.result.lock() {
                        *delivered = Some(result);
                        worker_flight.ready.notify_all();
                    }
                });
            if spawned.is_err() {
                if let Ok(mut state) = self.state.lock() {
                    state.pending_bytes = state.pending_bytes.saturating_sub(charge);
                    if state
                        .active
                        .get(&key)
                        .is_some_and(|active| Arc::ptr_eq(active, &flight))
                    {
                        state.active.remove(&key);
                    }
                    let available = state.capacity.saturating_sub(state.pending_bytes);
                    state.values.set_capacity(available);
                }
                if let Ok(mut result) = flight.result.lock() {
                    *result = Some(Err(gateway::error(
                        "可选缓存工作线程不可用。",
                        "CACHE_WORKER_UNAVAILABLE",
                        503,
                    )));
                    flight.ready.notify_all();
                }
            }
        } else {
            drop(compute);
        }
        let mut result = flight
            .result
            .lock()
            .map_err(|_| gateway::error("检索共享计算不可用。", "CACHE_WORKER_UNAVAILABLE", 503))?;
        loop {
            if caller_cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            if let Some(result) = result.as_ref() {
                let value = result.clone()?;
                drop(observer);
                return Ok(Lookup::Value {
                    value,
                    reused: !leader,
                });
            }
            result = flight
                .ready
                .wait_timeout(result, Duration::from_millis(20))
                .map_err(|_| {
                    gateway::error("检索共享计算不可用。", "CACHE_WORKER_UNAVAILABLE", 503)
                })?
                .0;
        }
    }
}
fn cancelled() -> ContractError {
    gateway::error("检索推理已取消。", "INFERENCE_CANCELLED", 499)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn equal_requests_share_one_real_compute_and_ready_result() {
        let cache = FlightCache::new();
        cache.capacity(4 * 1024 * 1024);
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let calls = calls.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    cache
                        .compute(
                            "one".into(),
                            10,
                            Arc::new(AtomicBool::new(false)),
                            None,
                            true,
                            move |_| {
                                calls.fetch_add(1, Ordering::SeqCst);
                                std::thread::sleep(Duration::from_millis(30));
                                Ok(Some((37usize, 8)))
                            },
                        )
                        .map(|lookup| match lookup {
                            Lookup::Value {
                                value: Some(value), ..
                            } => *value,
                            _ => 0,
                        })
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), 37);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*cache.get("one").unwrap(), 37);
        assert!(cache.stats()["coalesced"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn cancelling_one_observer_does_not_poison_another_and_failures_are_not_retained() {
        let cache = FlightCache::new();
        cache.capacity(4 * 1024 * 1024);
        let cancel = Arc::new(AtomicBool::new(false));
        let (entered, started) = std::sync::mpsc::channel();
        let (finish, release) = std::sync::mpsc::channel();
        let first_cache = cache.clone();
        let first_cancel = cancel.clone();
        let first = std::thread::spawn(move || {
            first_cache.compute(
                "shared".into(),
                8,
                first_cancel,
                None,
                true,
                move |shared_cancel| {
                    entered.send(()).unwrap();
                    release.recv_timeout(Duration::from_secs(2)).unwrap();
                    assert!(!shared_cancel.load(Ordering::SeqCst));
                    Ok(Some((12usize, 8)))
                },
            )
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let second_cache = cache.clone();
        let second = std::thread::spawn(move || {
            second_cache.compute(
                "shared".into(),
                8,
                Arc::new(AtomicBool::new(false)),
                None,
                true,
                |_| panic!("duplicate compute"),
            )
        });
        let until = Instant::now() + Duration::from_secs(2);
        while cache.stats()["coalesced"].as_u64().unwrap() == 0 && Instant::now() < until {
            std::thread::yield_now();
        }
        assert_eq!(cache.stats()["coalesced"], 1);
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            first.join().unwrap().err().unwrap().code(),
            "INFERENCE_CANCELLED"
        );
        finish.send(()).unwrap();
        assert!(matches!(
            second.join().unwrap().unwrap(),
            Lookup::Value { value: Some(_), .. }
        ));
        let failed = cache.compute(
            "bad".into(),
            8,
            Arc::new(AtomicBool::new(false)),
            None,
            true,
            |_| Err(gateway::error("fake", "EMBEDDING_ASSET_INVALID", 503)),
        );
        assert_eq!(failed.err().unwrap().code(), "EMBEDDING_ASSET_INVALID");
        assert!(cache.get("bad").is_none());
        let retry = cache
            .compute(
                "bad".into(),
                8,
                Arc::new(AtomicBool::new(false)),
                None,
                true,
                |_| Ok(Some((9usize, 8))),
            )
            .unwrap();
        assert!(matches!(retry, Lookup::Value { value: Some(_), .. }));
    }

    #[test]
    fn pending_bytes_are_bounded_and_pressure_cannot_repopulate_a_cleared_cache() {
        let cache = FlightCache::new();
        cache.capacity(3 * 1024 * 1024);
        let (entered, started) = std::sync::mpsc::channel();
        let (finish, release) = std::sync::mpsc::channel();
        let worker_cache = cache.clone();
        let publisher = cache.clone();
        let generation = cache.generation();
        let first = std::thread::spawn(move || {
            worker_cache.compute(
                "running".into(),
                32,
                Arc::new(AtomicBool::new(false)),
                None,
                false,
                move |_| {
                    entered.send(()).unwrap();
                    release.recv_timeout(Duration::from_secs(2)).unwrap();
                    publisher.insert_generation("ready".into(), Arc::new(5usize), 8, generation);
                    Ok(Some((5usize, 8)))
                },
            )
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(cache.stats()["pendingBytes"].as_u64().unwrap() <= 3 * 1024 * 1024);
        let bypass = cache.compute(
            "another".into(),
            32,
            Arc::new(AtomicBool::new(false)),
            None,
            true,
            |_| panic!("full optional cache must bypass"),
        );
        assert!(matches!(bypass.unwrap(), Lookup::Bypass));
        cache.clear();
        cache.capacity(3 * 1024 * 1024);
        finish.send(()).unwrap();
        assert!(matches!(
            first.join().unwrap().unwrap(),
            Lookup::Value { value: Some(_), .. }
        ));
        assert!(cache.get("running").is_none());
        assert!(cache.get("ready").is_none());
        assert_eq!(cache.stats()["pendingBytes"], 0);
    }
}

#[cfg(test)]
mod model_retirement_pressure_tests {
    use super::*;
    #[test]
    fn model_retirement_requires_trusted_memory_pressure_not_just_admission_or_queueing() {
        let mut snapshot=json!({"accounting":{"capacityMemoryBytes":16000,"availableMemoryBytes":12000},
            "memory":{"totalBytes":32000,"availableBytes":18000},"queuedRequests":20});
        assert!(!confirmed_memory_pressure(&snapshot));
        snapshot["budget"]=json!({"memoryBytes":320000000});
        assert!(!confirmed_memory_pressure(&snapshot));
        snapshot["memory"]["availableBytes"]=json!(100);
        assert!(confirmed_memory_pressure(&snapshot));
        snapshot["memory"]["availableBytes"]=json!(18000);
        snapshot["accounting"]["availableMemoryBytes"]=json!(100);
        assert!(confirmed_memory_pressure(&snapshot));
        snapshot["accounting"]["availableMemoryBytes"]=Value::Null;
        snapshot["memory"]["availableBytes"]=json!(-1);
        assert!(!confirmed_memory_pressure(&snapshot));
        assert!(!confirmed_memory_pressure(&Value::Null));
        assert!(!confirmed_memory_pressure(&json!({"memory":{"totalBytes":1e100,"availableBytes":0}})));
    }
}
