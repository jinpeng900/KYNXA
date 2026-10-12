//! Resource observations select MCP scheduling capacity; permits never revoke running protocol exchanges.
//! 资源观测决定 MCP 调度额度；收缩额度不会撤销已运行的协议交换，也不伪造吞吐反馈。
//!
//! Budgets are scheduling targets and quotas count active work; physical resource admission stays with the resource service.
//! 预算表示调度目标，租位记录活跃工作；真实硬件资源准入仍由资源服务负责。

use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[derive(Clone)]
pub(crate) struct McpBudget {
    pub starts: usize,
    pub calls: usize,
    pub warm_services: usize,
    pub idle_memory_bytes: u64,
    pub idle_timeout_ms: u64,
    pub observed: bool,
    pub reason: &'static str,
    pub sampled_at: Option<u64>,
}

impl McpBudget {
    pub fn from_snapshot(snapshot: Option<&Value>, fallback_cores: usize) -> Self {
        let Some(snapshot) = snapshot.filter(|value| {
            value["cpu"]["logicalCores"]
                .as_u64()
                .is_some_and(|cores| cores > 0)
                && value["memory"]["totalBytes"]
                    .as_u64()
                    .is_some_and(|bytes| bytes > 0)
                && value["accounting"]["availableMemoryBytes"].is_u64()
        }) else {
            let starts = (fallback_cores / 2).clamp(2, 8);
            return Self {
                starts,
                calls: fallback_cores.saturating_mul(2).clamp(8, 32),
                warm_services: starts,
                idle_memory_bytes: 256 * MIB,
                idle_timeout_ms: 180_000,
                observed: false,
                reason: "resource-observation-unavailable",
                sampled_at: None,
            };
        };
        let logical_cores = snapshot["cpu"]["logicalCores"].as_u64().unwrap() as usize;
        let total_memory_bytes = snapshot["memory"]["totalBytes"].as_u64().unwrap();
        let memory_headroom_bytes = snapshot["accounting"]["availableMemoryBytes"]
            .as_u64()
            .unwrap();
        let available_memory_bytes = snapshot["memory"]["availableBytes"].as_u64().unwrap_or(0);
        let available_cpu_threads = snapshot["budget"]["cpuThreads"]
            .as_u64()
            .unwrap_or(logical_cores as u64)
            .saturating_sub(
                snapshot["budget"]["reservedCpuThreads"]
                    .as_u64()
                    .unwrap_or(0),
            );
        // Utilisation alone is not resource exhaustion: idle background work may use the CPU fully.
        // CPU 使用率高本身不是资源不足，空闲后台工作可以充分使用 CPU；收缩依据真实余量。
        let is_under_pressure = available_memory_bytes < GIB
            || memory_headroom_bytes < 128 * MIB
            || available_cpu_threads == 0;
        let starts = ((2 + total_memory_bytes / (8 * GIB)) as usize)
            .clamp(2, 16)
            .min(logical_cores.saturating_sub(2).max(1))
            .min(available_cpu_threads.max(1) as usize)
            .min((memory_headroom_bytes / (32 * MIB)).max(1) as usize);
        let calls = ((total_memory_bytes / GIB) as usize)
            .clamp(8, 32)
            .min(logical_cores.saturating_mul(4).max(8))
            .min((memory_headroom_bytes / (64 * MIB)).max(1) as usize);
        let idle_memory_bytes = (total_memory_bytes / 32)
            .clamp(256 * MIB, 2 * GIB)
            .min(memory_headroom_bytes / 3);
        Self {
            starts: if is_under_pressure {
                starts.min(2)
            } else {
                starts
            },
            calls: if is_under_pressure {
                calls.min(8)
            } else {
                calls
            },
            warm_services: if is_under_pressure {
                1
            } else {
                (starts * 2).min(16)
            },
            idle_memory_bytes,
            idle_timeout_ms: if is_under_pressure { 30_000 } else { 300_000 },
            observed: true,
            reason: if is_under_pressure {
                "observed-resource-pressure"
            } else {
                "observed-resource-headroom"
            },
            sampled_at: snapshot["sampledAt"].as_u64(),
        }
    }

    pub fn value(&self) -> Value {
        json!({
            "maximumConcurrentStarts": self.starts,
            "maximumConcurrentCalls": self.calls,
            "maximumWarmServices": self.warm_services,
            "idleMemoryBudgetBytes": self.idle_memory_bytes,
            "idleTimeoutMs": self.idle_timeout_ms,
            "resourceObservationAvailable": self.observed,
            "adjustmentReason": self.reason,
            "sampledAt": self.sampled_at,
            "throughputAdaptive": false,
            "perConnectionConcurrentCalls": 1,
        })
    }
}

/// A lower limit blocks new work until old permits finish; it never invents available semaphore credits.
/// 降额后等待旧租位完成再准入，既不强取消运行中调用，也不凭归还旧租位放大新额度。
pub(crate) struct McpQuota {
    state: Mutex<QuotaState>,
    changed: Notify,
}

struct QuotaState {
    limit: usize,
    active: usize,
}

pub(crate) struct McpPermit(Arc<McpQuota>);
impl Drop for McpPermit {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active = state.active.saturating_sub(1);
        drop(state);
        self.0.changed.notify_waiters();
    }
}

impl McpQuota {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(QuotaState {
                limit: limit.max(1),
                active: 0,
            }),
            changed: Notify::new(),
        })
    }
    pub fn set_limit(&self, limit: usize) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .limit = limit.max(1);
        self.changed.notify_waiters();
    }
    pub fn limit(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .limit
    }
    pub fn active(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active
    }
    #[cfg(test)]
    pub fn available_permits(&self) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.limit.saturating_sub(state.active)
    }
    pub fn try_acquire_owned(self: &Arc<Self>) -> Option<McpPermit> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.active >= state.limit {
            return None;
        }
        state.active += 1;
        Some(McpPermit(self.clone()))
    }
    pub async fn acquire_owned(self: &Arc<Self>) -> McpPermit {
        loop {
            // Register the waiter before checking capacity so a released permit cannot be missed.
            // 先登记等待者再检查额度，避免租位释放通知落在检查与等待之间。
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(permit) = self.try_acquire_owned() {
                return permit;
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(total_gib: u64) -> Value {
        json!({
            "cpu": {"logicalCores": 8, "usagePercent": 20.0},
            "memory": {"totalBytes": total_gib * GIB, "availableBytes": 8 * GIB},
            "budget": {"cpuThreads": 6, "reservedCpuThreads": 0},
            "accounting": {"availableMemoryBytes": 4 * GIB},
            "sampledAt": 1000,
        })
    }
    #[test]
    fn observations_expand_and_pressure_reduces_real_scheduling_targets() {
        let small = McpBudget::from_snapshot(Some(&snapshot(16)), 1);
        assert_eq!(
            (small.starts, small.calls, small.idle_memory_bytes),
            (4, 16, 512 * MIB)
        );
        let larger = McpBudget::from_snapshot(Some(&snapshot(32)), 1);
        assert_eq!(
            (larger.starts, larger.calls, larger.idle_memory_bytes),
            (6, 32, GIB)
        );
        let mut fully_busy = snapshot(32);
        fully_busy["cpu"]["usagePercent"] = json!(100.0);
        assert_eq!(
            McpBudget::from_snapshot(Some(&fully_busy), 1).starts,
            larger.starts
        );
        let mut pressure = snapshot(32);
        pressure["memory"]["availableBytes"] = json!(512 * MIB);
        pressure["accounting"]["availableMemoryBytes"] = json!(64 * MIB);
        let reduced = McpBudget::from_snapshot(Some(&pressure), 1);
        assert!(reduced.starts < larger.starts && reduced.calls < larger.calls);
        assert!(reduced.idle_memory_bytes < larger.idle_memory_bytes);
        assert!(reduced.observed);
        assert_eq!(
            McpBudget::from_snapshot(None, 8).value()["resourceObservationAvailable"],
            false
        );
        assert_eq!(larger.value()["throughputAdaptive"], false);
    }
    #[tokio::test]
    async fn shrink_waits_for_running_work_then_growth_wakes_queued_work() {
        let quota = McpQuota::new(4);
        let permits = (0..4)
            .map(|_| quota.try_acquire_owned().unwrap())
            .collect::<Vec<_>>();
        quota.set_limit(1);
        assert!(quota.try_acquire_owned().is_none());
        let waiting = {
            let quota = quota.clone();
            tokio::spawn(async move { quota.acquire_owned().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(permits);
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(quota.active(), 1);
        quota.set_limit(3);
        let second = quota.try_acquire_owned().unwrap();
        let third = quota.try_acquire_owned().unwrap();
        assert!(quota.try_acquire_owned().is_none());
        drop((acquired, second, third));
        assert_eq!(quota.available_permits(), 3);
    }
}
