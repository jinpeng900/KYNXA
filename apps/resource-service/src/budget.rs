use crate::policy::{AdaptivePolicy, WorkFeedback};
use crate::sampling::{HardwareSnapshot, LeaseGpuObservation, LeaseMemoryObservation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

const MAX_LEASES: usize = 128;
const MAX_TTL_MS: u64 = 300_000;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AllocationRequest {
    pub task_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default = "background")]
    pub kind: String,
    #[serde(default)]
    pub cpu_threads: usize,
    #[serde(default)]
    pub memory_bytes: u64,
    #[serde(default)]
    pub gpu_memory_bytes: u64,
    #[serde(default = "default_ttl")]
    pub ttl_ms: u64,
    #[serde(default)]
    pub wait_ms: u64,
}

fn background() -> String {
    "background".into()
}
fn default_ttl() -> u64 {
    30_000
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Lease {
    lease_id: String,
    expires_at: u64,
    cpu_threads: usize,
    memory_bytes: u64,
    gpu_memory_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_provider: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_device_id: Option<u32>,
    task_id: String,
}

pub struct ResourceBudget {
    leases: HashMap<String, Lease>,
    sequence: u64,
    policy: AdaptivePolicy,
    memory_observations: Vec<LeaseMemoryObservation>,
    baseline_floors: HashMap<(u32, u64), u64>,
    gpu_observations: Vec<LeaseGpuObservation>,
    gpu_baseline_floors: HashMap<(u32, u64), u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoredLease {
    lease_id: String,
    expires_at: u64,
    cpu_threads: usize,
    memory_bytes: u64,
    gpu_memory_bytes: u64,
    task_id: String,
}

impl ResourceBudget {
    pub fn new() -> Self {
        Self {
            leases: HashMap::new(),
            sequence: 0,
            policy: AdaptivePolicy::new(),
            memory_observations: Vec::new(),
            baseline_floors: HashMap::new(),
            gpu_observations: Vec::new(),
            gpu_baseline_floors: HashMap::new(),
        }
    }

    pub fn has_gpu_allocation(&self) -> bool {
        self.leases.values().any(|lease| lease.gpu_memory_bytes > 0)
    }

    pub fn snapshot(&self, hardware: &HardwareSnapshot, now_ms: u64) -> Value {
        let (cpu, memory, gpu_memory) = self.reserved();
        let materialized_memory = self.materialized_memory();
        let materialized_bytes = materialized_memory.values().sum::<u64>();
        let materialized_gpu = self.materialized_gpu_memory();
        let materialized_gpu_bytes = materialized_gpu.values().sum::<u64>();
        let active = self
            .leases
            .values()
            .filter(|lease| lease.expires_at > now_ms)
            .count();
        json!({ "mode": "rust", "cpu": hardware.cpu, "memory": hardware.memory, "gpu": hardware.gpu,
            "budget": { "cpuThreads": cpu_capacity(hardware), "memoryBytes": memory_capacity(hardware),
                "gpuMemoryBytes": gpu_capacity(hardware),
                "reservedCpuThreads": cpu, "reservedMemoryBytes": memory, "reservedGpuMemoryBytes": gpu_memory },
            "accounting": { "observedMaterializedMemoryBytes": materialized_bytes,
                "unmaterializedMemoryBytes": memory.saturating_sub(materialized_bytes),
                "capacityMemoryBytes":memory_capacity(hardware),"maximumResidentReservationBytes":hardware.memory.total_bytes/2,
                "availableMemoryBytes": self.available_memory(hardware),
                "gpuMemoryState": if materialized_gpu_bytes > 0 { "observed-process-increments" } else { "unverified-reservation" },
                "observedMaterializedGpuMemoryBytes":materialized_gpu_bytes,
                "availableGpuMemoryBytes":self.available_gpu_memory(hardware),
                "gpuUnverifiedReservationBytes": gpu_memory.saturating_sub(materialized_gpu_bytes),
                "gpuAttributionExact":false, "leases": self.leases.values().map(|lease| {
                    let observed = materialized_memory.get(&lease.lease_id).copied().unwrap_or(0);
                    let observed_gpu = materialized_gpu.get(&lease.lease_id).copied().unwrap_or(0);
                    json!({ "leaseId":lease.lease_id,"memoryBytes":lease.memory_bytes,"observedMaterializedMemoryBytes":observed,
                        "gpuMemoryBytes":lease.gpu_memory_bytes,"observedMaterializedGpuMemoryBytes":observed_gpu,
                        "gpuUnverifiedReservationBytes":lease.gpu_memory_bytes.saturating_sub(observed_gpu),
                        "unmaterializedMemoryBytes":lease.memory_bytes.saturating_sub(observed),
                        "state":if lease.expires_at <= now_ms {"quarantined"} else if observed > 0 {"observed-materialized"}
                            else if self.memory_observations.iter().any(|observation|observation.lease_id == lease.lease_id && observation.memory_bytes.is_none())
                            {"unknown-executor-reservation"} else {"unmaterialized"} })
                }).collect::<Vec<_>>() },
            "activeLeases": active, "quarantinedLeases": self.leases.len() - active,
            "feedback": self.policy,
            "maxLeases": MAX_LEASES, "sampledAt": now_ms })
    }

    fn reserved(&self) -> (usize, u64, u64) {
        self.leases
            .values()
            .fold((0, 0, 0), |(cpu, memory, gpu_memory), lease| {
                (
                    cpu + lease.cpu_threads,
                    memory.saturating_add(lease.memory_bytes),
                    gpu_memory.saturating_add(lease.gpu_memory_bytes),
                )
            })
    }

    pub fn reconcile_memory(&mut self, observations: Vec<LeaseMemoryObservation>) {
        self.memory_observations = observations;
        self.baseline_floors.retain(|identity,_|self.memory_observations.iter().any(|observation|
            (observation.process_id,observation.start_time_ms) == *identity && self.leases.contains_key(&observation.lease_id)));
    }

    pub fn reconcile_gpu_memory(&mut self, observations: Vec<LeaseGpuObservation>) {
        self.gpu_observations = observations;
        self.gpu_baseline_floors.retain(|identity, _| self.gpu_observations.iter().any(|observation|
            (observation.process_id, observation.start_time_ms) == *identity && self.leases.contains_key(&observation.lease_id)));
    }

    fn materialized_gpu_memory(&self) -> HashMap<String, u64> {
        let mut groups: HashMap<(u32, u64), Vec<&LeaseGpuObservation>> = HashMap::new();
        for observation in &self.gpu_observations {
            if observation.state == "running" && observation.process_id > 0 && observation.start_time_ms > 0
                && self.leases.get(&observation.lease_id).is_some_and(|lease|lease.gpu_memory_bytes > 0) {
                groups.entry((observation.process_id, observation.start_time_ms)).or_default().push(observation);
            }
        }
        let mut credits = HashMap::new();
        for (identity, mut observations) in groups {
            let Some(current) = observations.iter().filter_map(|observation| observation.memory_bytes).min() else { continue; };
            let Some(baseline) = observations.iter().filter_map(|observation| observation.baseline_memory_bytes).max() else { continue; };
            let mut available_credit = current.saturating_sub(baseline.max(self.gpu_baseline_floors.get(&identity).copied().unwrap_or(0)));
            observations.sort_by_key(|observation| &observation.lease_id);
            // Free VRAM already includes verified process increments; pay down each increment only once across leases.
            // 空闲显存已反映经过身份复核的进程增量；同一增量跨租约只能抵扣一次，未知数值保留完整预约。
            for observation in observations {
                let credit = available_credit.min(self.leases[&observation.lease_id].gpu_memory_bytes);
                available_credit -= credit;
                credits.insert(observation.lease_id.clone(), credit);
            }
        }
        credits
    }

    fn available_gpu_memory(&self, hardware: &HardwareSnapshot) -> Option<u64> {
        let (_, _, reserved) = self.reserved();
        let materialized = self.materialized_gpu_memory().values().sum::<u64>();
        gpu_capacity(hardware).map(|capacity| capacity.saturating_sub(reserved.saturating_sub(materialized)))
    }

    fn materialized_memory(&self) -> HashMap<String, u64> {
        let mut groups: HashMap<(u32, u64), Vec<&LeaseMemoryObservation>> = HashMap::new();
        for observation in &self.memory_observations {
            if observation.state == "running" && self.leases.contains_key(&observation.lease_id) {
                groups.entry((observation.process_id, observation.start_time_ms)).or_default().push(observation);
            }
        }
        let mut credits = HashMap::new();
        for (identity, mut observations) in groups {
            let Some(current) = observations.iter().filter_map(|observation| observation.memory_bytes).min() else { continue; };
            let Some(mut baseline) = observations.iter().filter_map(|observation| observation.baseline_memory_bytes).max() else { continue; };
            baseline = baseline.max(self.baseline_floors.get(&identity).copied().unwrap_or(0));
            let mut available_credit = current.saturating_sub(baseline);
            observations.sort_by_key(|observation| &observation.lease_id);
            // One process increment can pay down several leases only once, and never beyond their reservation.
            // 单个进程的增量可分摊多个预约，但只计算一次且不超过每份预约；基线和未知显存不作信用。
            for observation in observations {
                let credit = available_credit.min(self.leases[&observation.lease_id].memory_bytes);
                available_credit -= credit;
                credits.insert(observation.lease_id.clone(), credit);
            }
        }
        credits
    }

    fn available_memory(&self, hardware: &HardwareSnapshot) -> u64 {
        let (_, reserved, _) = self.reserved();
        let materialized = self.materialized_memory().values().sum::<u64>();
        memory_capacity(hardware).saturating_sub(reserved.saturating_sub(materialized))
            .min((hardware.memory.total_bytes / 2).saturating_sub(reserved))
    }

    pub fn restore(&mut self, leases: Vec<RestoredLease>) -> Value {
        if !self.leases.is_empty() || leases.len() > MAX_LEASES || leases.iter().any(|lease|
            lease.lease_id.is_empty() || lease.lease_id.len() > 160 || lease.task_id.is_empty() || lease.task_id.len() > 128 ||
            lease.cpu_threads > 4096 || (lease.cpu_threads == 0 && lease.memory_bytes == 0 && lease.gpu_memory_bytes == 0)) {
            return json!({"status":"denied","reason":"RESOURCE_RESTORE_INVALID"});
        }
        let mut restored = HashMap::new();
        for lease in leases {
            if restored.contains_key(&lease.lease_id) { return json!({"status":"denied","reason":"RESOURCE_RESTORE_INVALID"}); }
            restored.insert(lease.lease_id.clone(), Lease { lease_id:lease.lease_id,expires_at:lease.expires_at,
                cpu_threads:lease.cpu_threads,memory_bytes:lease.memory_bytes,gpu_memory_bytes:lease.gpu_memory_bytes,
                device_id:None,execution_provider:None,execution_device_id:None,task_id:lease.task_id });
        }
        // Restoration imports held debt atomically, without treating a restarted monitor as newly free capacity.
        // 重启时原子恢复未结清账目；监控重启不等于旧工作已经释放，也不重新判为可分配容量。
        self.leases = restored;
        json!({"status":"restored","restoredLeases":self.leases.len(),"accountingState":"unverified-reservations"})
    }

    pub fn acquire(
        &mut self,
        request: AllocationRequest,
        hardware: &HardwareSnapshot,
        now_ms: u64,
    ) -> Value {
        if request.task_id.is_empty()
            || request.task_id.len() > 128
            || request.workspace_id.len() > 128
            || !["foreground", "background"].contains(&request.kind.as_str())
            || request.cpu_threads > 4096
            || request.ttl_ms < 1000
            || request.ttl_ms > MAX_TTL_MS
            || request.wait_ms > 60_000
            || (request.cpu_threads == 0
                && request.memory_bytes == 0
                && request.gpu_memory_bytes == 0)
        {
            return json!({ "status": "denied", "reason": "RESOURCE_INVALID_REQUEST", "mode": "rust" });
        }
        if request.gpu_memory_bytes > 0 && gpu_capacity(hardware).is_none() {
            return json!({ "status": "denied", "reason": "RESOURCE_GPU_UNKNOWN", "mode": "rust" });
        }
        if self.leases.len() >= MAX_LEASES {
            return json!({ "status": "denied", "reason": "RESOURCE_LEASE_LIMIT", "mode": "rust" });
        }
        let (reserved_cpu, _, _) = self.reserved();
        let available_cpu = cpu_capacity(hardware).saturating_sub(reserved_cpu);
        let available_memory = self.available_memory(hardware);
        let available_gpu = self.available_gpu_memory(hardware).unwrap_or(0);
        if request.memory_bytes > available_memory
            || request.gpu_memory_bytes > available_gpu
            || (request.cpu_threads > 0 && available_cpu == 0)
            || (request.kind == "background"
                && request.cpu_threads > 0
                && hardware.cpu.usage_percent.is_some_and(|usage| usage > 90.0))
            || (request.kind == "background"
                && request.gpu_memory_bytes > 0
                && hardware.gpu.usage_percent.is_some_and(|usage| usage >= 95))
        {
            return json!({ "status": "denied", "reason": "RESOURCE_PRESSURE", "mode": "rust" });
        }
        self.sequence += 1;
        let policy_fraction = self.policy.fraction(&request.task_id, request.gpu_memory_bytes > 0);
        let execution_fraction = if hardware.cpu.usage_percent.is_some_and(|usage| usage > 85.0)
            || hardware.memory.available_bytes < 1024 * 1024 * 1024 {
            policy_fraction.min(0.5)
        } else { policy_fraction };
        let lease = Lease {
            lease_id: format!("rust-{}-{}", std::process::id(), self.sequence),
            expires_at: now_ms + request.ttl_ms,
            cpu_threads: request.cpu_threads.min(available_cpu).min(
                if request.kind == "background" {
                    (cpu_capacity(hardware) as f64 * execution_fraction)
                        .floor()
                        .max(1.0) as usize
                } else {
                    available_cpu
                },
            ),
            memory_bytes: request.memory_bytes,
            gpu_memory_bytes: request.gpu_memory_bytes,
            device_id: if request.gpu_memory_bytes > 0 {
                hardware.gpu.device_id
            } else {
                None
            },
            execution_provider: if request.gpu_memory_bytes > 0 {
                hardware.gpu.execution_provider
            } else {
                None
            },
            execution_device_id: if request.gpu_memory_bytes > 0 {
                hardware.gpu.execution_device_id
            } else {
                None
            },
            task_id: request.task_id,
        };
        self.leases.insert(lease.lease_id.clone(), lease.clone());
        let mut output = serde_json::to_value(&lease).expect("lease serialization");
        output["status"] = json!("granted");
        output["mode"] = json!("rust");
        output["device"] = json!(if lease.gpu_memory_bytes > 0 {
            "gpu"
        } else {
            "cpu"
        });
        output["suggestions"] =
            self.policy
                .suggestions(hardware, lease.cpu_threads, lease.memory_bytes,&lease.task_id,lease.gpu_memory_bytes > 0,
                    available_cpu.saturating_sub(lease.cpu_threads), available_memory.saturating_sub(lease.memory_bytes),
                    available_gpu.saturating_sub(lease.gpu_memory_bytes));
        output
    }

    pub fn renew(&mut self, lease_id: &str, ttl_ms: u64, now_ms: u64) -> Value {
        if !(1000..=MAX_TTL_MS).contains(&ttl_ms) {
            return json!({ "status": "denied", "reason": "RESOURCE_INVALID_TTL" });
        }
        match self.leases.get_mut(lease_id) {
            Some(lease) => {
                lease.expires_at = now_ms + ttl_ms;
                json!({ "status": "renewed", "leaseId": lease_id, "expiresAt": lease.expires_at })
            }
            None => json!({ "status": "denied", "reason": "RESOURCE_LEASE_UNKNOWN" }),
        }
    }

    pub fn release(&mut self, lease_id: &str) -> Value {
        let gpu_credit = self.materialized_gpu_memory().get(lease_id).copied().unwrap_or(0);
        if gpu_credit > 0 {
            if let Some(observation) = self.gpu_observations.iter().find(|observation|observation.lease_id == lease_id) {
                let identity = (observation.process_id, observation.start_time_ms);
                let baseline = self.gpu_baseline_floors.get(&identity).copied().unwrap_or(observation.baseline_memory_bytes.unwrap_or(0));
                // Released buffers may stay cached by the runtime; do not reassign their old credit to another lease.
                // 已释放缓冲可能留在运行时缓存；其旧信用不能转移给另一份预约。
                self.gpu_baseline_floors.insert(identity, baseline.saturating_add(gpu_credit));
            }
        }
        let credit = self.materialized_memory().get(lease_id).copied().unwrap_or(0);
        if credit > 0 {
            if let Some(observation) = self.memory_observations.iter().find(|observation|observation.lease_id == lease_id) {
                let identity = (observation.process_id,observation.start_time_ms);
                let baseline = self.baseline_floors.get(&identity).copied().unwrap_or(observation.baseline_memory_bytes.unwrap_or(0));
                // Released RSS may remain in the allocator; it cannot be reassigned as another lease's new credit.
                // 已释放工作对应的 RSS 可能仍在分配器中，不能把它重新算成另一预约的新兑现信用。
                self.baseline_floors.insert(identity,baseline.saturating_add(credit));
            }
        }
        json!({ "status": "released", "leaseId": lease_id, "existed": self.leases.remove(lease_id).is_some() })
    }

    pub fn contains_lease(&self, lease_id: &str) -> bool {
        self.leases.contains_key(lease_id)
    }

    pub fn report(&mut self, lease_id: &str, feedback: WorkFeedback, now_ms: u64) -> Value {
        if !self.leases.contains_key(lease_id) {
            return json!({"status":"denied","reason":"RESOURCE_LEASE_UNKNOWN"});
        }
        if !self
            .policy
            .report(&self.leases[lease_id].task_id, feedback, now_ms)
        {
            return json!({"status":"denied","reason":"RESOURCE_INVALID_FEEDBACK"});
        }
        let mut feedback = serde_json::to_value(&self.policy).expect("policy serialization");
        feedback["backgroundFraction"] = json!(self.policy.task_backend_fraction);
        json!({"status":"reported","feedback":feedback})
    }
}

fn cpu_capacity(hardware: &HardwareSnapshot) -> usize {
    hardware.cpu.logical_cores.saturating_sub(2).max(1).min(64)
}

fn memory_capacity(hardware: &HardwareSnapshot) -> u64 {
    // Keep foreground/system headroom and count every unresolved reservation, including expiry.
    // 预留前台和系统余量；包括已过期但未确认退出的预约，避免失联后重复分配。
    (hardware.memory.available_bytes.saturating_mul(7) / 10)
        .saturating_sub(512 * 1024 * 1024)
        .min(hardware.memory.total_bytes / 2)
}

fn gpu_capacity(hardware: &HardwareSnapshot) -> Option<u64> {
    if hardware.gpu.state != "available" || hardware.gpu.device_id != Some(0) {
        return None;
    }
    let free = hardware.gpu.available_memory_bytes?;
    let total = hardware.gpu.total_memory_bytes?;
    Some(free.saturating_sub((total / 10).min(512 * 1024 * 1024)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuSnapshot;
    use crate::sampling::{CpuSnapshot, MemorySnapshot};
    fn hardware() -> HardwareSnapshot {
        HardwareSnapshot {
            cpu: CpuSnapshot {
                logical_cores: 8,
                usage_percent: Some(10.0),
            },
            memory: MemorySnapshot {
                total_bytes: 16 << 30,
                available_bytes: 8 << 30,
            },
            gpu: GpuSnapshot::unknown("GPU_SENSOR_UNAVAILABLE"),
        }
    }
    fn request(cpu: usize, memory: u64) -> AllocationRequest {
        AllocationRequest {
            task_id: "test".into(),
            workspace_id: "temporary".into(),
            kind: "background".into(),
            cpu_threads: cpu,
            memory_bytes: memory,
            gpu_memory_bytes: 0,
            ttl_ms: 1000,
            wait_ms: 0,
        }
    }

    #[test]
    fn search_planning_uses_remaining_headroom_without_approving_reranker_execution() {
        let mut budget = ResourceBudget::new();
        let search = budget.acquire(request(1, 8 << 20), &hardware(), 0);
        assert_eq!(search["suggestions"]["rerankCandidates"], 60);
        assert_eq!(search["suggestions"]["rerankSuggestionRequiresApproval"], true);
        assert_eq!(budget.snapshot(&hardware(), 0)["budget"]["reservedCpuThreads"], 1);
        let other = budget.acquire(request(4, 8 << 20), &hardware(), 0);
        assert_eq!(other["status"], "granted");
        let pressured = budget.acquire(request(1, 8 << 20), &hardware(), 0);
        assert_eq!(pressured["suggestions"]["rerankCandidateLimit"], 20);
    }
    #[test]
    fn allocation_is_atomic_and_expiry_does_not_free_unconfirmed_work() {
        let mut budget = ResourceBudget::new();
        let first = budget.acquire(request(6, 1024), &hardware(), 0);
        assert_eq!(first["status"], "granted");
        assert_eq!(
            budget.acquire(request(1, 0), &hardware(), 2000)["status"],
            "denied"
        );
        assert_eq!(budget.snapshot(&hardware(), 2000)["quarantinedLeases"], 1);
        budget.release(first["leaseId"].as_str().unwrap());
        assert_eq!(
            budget.acquire(request(1, 0), &hardware(), 2000)["status"],
            "granted"
        );
    }
    #[test]
    fn memory_and_cpu_can_be_reserved_separately_but_unknown_gpu_is_rejected() {
        let mut budget = ResourceBudget::new();
        assert_eq!(
            budget.acquire(request(0, 1024), &hardware(), 0)["cpuThreads"],
            0
        );
        let mut gpu = request(1, 0);
        gpu.gpu_memory_bytes = 1024;
        assert_eq!(
            budget.acquire(gpu, &hardware(), 0)["reason"],
            "RESOURCE_GPU_UNKNOWN"
        );
        assert_eq!(
            budget.acquire(request(1, 20 << 30), &hardware(), 0)["status"],
            "denied"
        );
    }

    #[test]
    fn gpu_reservations_share_one_atomic_budget_and_expiry_keeps_memory_fenced() {
        let mut hardware = hardware();
        hardware.gpu = GpuSnapshot {
            state: "available",
            device_id: Some(0),
            total_memory_bytes: Some(8 << 30),
            available_memory_bytes: Some(4 << 30),
            usage_percent: Some(5),
            reason: None,
            execution_provider: Some("dml"),
            execution_device_id: Some(1),
            mapping_status: "verified",
        };
        let mut budget = ResourceBudget::new();
        let mut first = request(0, 0);
        first.gpu_memory_bytes = 3 << 30;
        let first = budget.acquire(first, &hardware, 0);
        assert_eq!(first["status"], "granted");
        assert_eq!(first["deviceId"], 0);
        let mut second = request(0, 0);
        second.gpu_memory_bytes = 2 << 30;
        assert_eq!(budget.acquire(second, &hardware, 2000)["status"], "denied");
        assert_eq!(
            budget.snapshot(&hardware, 2000)["budget"]["reservedGpuMemoryBytes"],
            3_u64 << 30
        );
        budget.release(first["leaseId"].as_str().unwrap());
        let mut second = request(0, 0);
        second.gpu_memory_bytes = 2 << 30;
        assert_eq!(budget.acquire(second, &hardware, 2000)["status"], "granted");
        hardware.gpu.usage_percent = Some(96);
        let mut pressure = request(0, 0);
        pressure.gpu_memory_bytes = 1;
        assert_eq!(
            budget.acquire(pressure, &hardware, 2000)["reason"],
            "RESOURCE_PRESSURE"
        );
        hardware.gpu = GpuSnapshot::unknown("GPU_NVML_SAMPLE_FAILED");
        let mut third = request(0, 0);
        third.gpu_memory_bytes = 1;
        assert_eq!(
            budget.acquire(third, &hardware, 2000)["reason"],
            "RESOURCE_GPU_UNKNOWN"
        );
    }

    #[test]
    fn measured_owned_rss_is_credited_once_and_released_rss_is_not_reassigned() {
        let mut budget = ResourceBudget::new();
        let first = budget.acquire(request(0, 1 << 30), &hardware(), 0);
        let second = budget.acquire(request(0, 1 << 30), &hardware(), 0);
        let ids = [first["leaseId"].as_str().unwrap().to_string(),second["leaseId"].as_str().unwrap().to_string()];
        budget.reconcile_memory(ids.iter().map(|id|LeaseMemoryObservation {lease_id:id.clone(),process_id:1,start_time_ms:10,
            baseline_memory_bytes:Some(100),memory_bytes:Some((1 << 30)+100),state:"running"}).collect());
        let mut current = hardware(); current.memory.available_bytes -= 1 << 30;
        let snapshot = budget.snapshot(&current, 1);
        assert_eq!(snapshot["accounting"]["observedMaterializedMemoryBytes"],1_u64 << 30);
        assert_eq!(snapshot["accounting"]["unmaterializedMemoryBytes"],1_u64 << 30);
        assert_eq!(snapshot["accounting"]["availableMemoryBytes"],memory_capacity(&current)-(1_u64 << 30));
        budget.release(&ids[0]);
        assert_eq!(budget.snapshot(&current, 1)["accounting"]["observedMaterializedMemoryBytes"],0);
        budget.reconcile_memory(vec![LeaseMemoryObservation {lease_id:ids[1].clone(),process_id:1,start_time_ms:11,
            baseline_memory_bytes:Some(100),memory_bytes:None,state:"replaced"}]);
        assert_eq!(budget.snapshot(&current, 2000)["quarantinedLeases"],1);
        assert_eq!(budget.snapshot(&current, 2000)["accounting"]["observedMaterializedMemoryBytes"],0);
    }

    #[test]
    fn recovery_restores_debt_atomically_and_keeps_unknown_gpu_reserved() {
        let mut budget = ResourceBudget::new();
        let restored:Vec<RestoredLease> = serde_json::from_value(json!([
            {"leaseId":"rust-old-1","expiresAt":1,"cpuThreads":6,"memoryBytes":1024,"gpuMemoryBytes":4096,"taskId":"model"}
        ])).unwrap();
        assert_eq!(budget.restore(restored)["status"],"restored");
        assert_eq!(budget.acquire(request(1,0),&hardware(),2000)["status"],"denied");
        let snapshot = budget.snapshot(&hardware(),2000);
        assert_eq!(snapshot["accounting"]["gpuUnverifiedReservationBytes"],4096);
        assert_eq!(snapshot["quarantinedLeases"],1);
        assert_eq!(budget.restore(Vec::new())["reason"],"RESOURCE_RESTORE_INVALID");
    }

    fn gpu_hardware() -> HardwareSnapshot {
        let mut machine = hardware();
        machine.gpu = GpuSnapshot { state: "available", device_id: Some(0), total_memory_bytes: Some(8 << 30),
            available_memory_bytes: Some(7 << 30), usage_percent: Some(1), reason: None,
            execution_provider: Some("dml"), execution_device_id: Some(0), mapping_status: "verified" };
        machine
    }

    #[test]
    fn verified_gpu_increment_is_not_subtracted_twice_from_free_vram() {
        let mut budget = ResourceBudget::new();
        let mut machine = gpu_hardware();
        let mut allocation = request(0, 0); allocation.gpu_memory_bytes = 3 << 30;
        let lease = budget.acquire(allocation, &machine, 0);
        let id = lease["leaseId"].as_str().unwrap().to_string();
        machine.gpu.available_memory_bytes = Some(5 << 30);
        let mut next = request(0, 0); next.gpu_memory_bytes = 2 << 30;
        assert_eq!(budget.acquire(next.clone(), &machine, 1)["status"], "denied");
        budget.reconcile_gpu_memory(vec![LeaseGpuObservation { lease_id: id, process_id: 12, start_time_ms: 100,
            baseline_memory_bytes: Some(100), memory_bytes: Some((2 << 30) + 100), state: "running" }]);
        let snapshot = budget.snapshot(&machine, 1);
        assert_eq!(snapshot["accounting"]["observedMaterializedGpuMemoryBytes"], 2_u64 << 30);
        assert_eq!(snapshot["accounting"]["gpuUnverifiedReservationBytes"], 1_u64 << 30);
        assert_eq!(budget.acquire(next, &machine, 1)["status"], "granted");
    }

    #[test]
    fn shared_pid_gpu_credit_is_bounded_once_and_release_does_not_transfer_cached_buffers() {
        let mut budget = ResourceBudget::new();
        let mut allocation = request(0, 0); allocation.gpu_memory_bytes = 2 << 30;
        let first = budget.acquire(allocation.clone(), &gpu_hardware(), 0);
        let second = budget.acquire(allocation, &gpu_hardware(), 0);
        let ids = [first["leaseId"].as_str().unwrap().to_string(), second["leaseId"].as_str().unwrap().to_string()];
        budget.reconcile_gpu_memory(ids.iter().map(|id| LeaseGpuObservation { lease_id: id.clone(), process_id: 12,
            start_time_ms: 100, baseline_memory_bytes: Some(100), memory_bytes: Some((2 << 30) + 100), state: "running" }).collect());
        assert_eq!(budget.snapshot(&gpu_hardware(), 1)["accounting"]["observedMaterializedGpuMemoryBytes"], 2_u64 << 30);
        budget.release(&ids[0]);
        assert_eq!(budget.snapshot(&gpu_hardware(), 1)["accounting"]["observedMaterializedGpuMemoryBytes"], 0);
        budget.reconcile_gpu_memory(vec![LeaseGpuObservation { lease_id: ids[1].clone(), process_id: 12,
            start_time_ms: 101, baseline_memory_bytes: Some(100), memory_bytes: Some(3 << 30), state: "replaced" }]);
        assert_eq!(budget.snapshot(&gpu_hardware(), 1)["accounting"]["observedMaterializedGpuMemoryBytes"], 0);
        assert_eq!(budget.snapshot(&gpu_hardware(), 2000)["accounting"]["gpuUnverifiedReservationBytes"], 2_u64 << 30);
    }

    #[test]
    fn unknown_wddm_usage_and_unknown_baseline_never_authorize_gpu_credit() {
        for (baseline, current) in [(Some(0), None), (None, Some(2 << 30)), (None, None)] {
            let mut budget = ResourceBudget::new();
            let mut allocation = request(0, 0); allocation.gpu_memory_bytes = 2 << 30;
            let lease = budget.acquire(allocation, &gpu_hardware(), 0);
            budget.reconcile_gpu_memory(vec![LeaseGpuObservation { lease_id: lease["leaseId"].as_str().unwrap().into(),
                process_id: 12, start_time_ms: 100, baseline_memory_bytes: baseline, memory_bytes: current, state: "running" }]);
            let snapshot = budget.snapshot(&gpu_hardware(), 2000);
            assert_eq!(snapshot["accounting"]["observedMaterializedGpuMemoryBytes"], 0);
            assert_eq!(snapshot["accounting"]["gpuUnverifiedReservationBytes"], 2_u64 << 30);
        }
    }
}
