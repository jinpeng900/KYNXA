use crate::gpu::{GpuProcessSample, GpuSensor, GpuSnapshot};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareSnapshot {
    pub cpu: CpuSnapshot,
    pub memory: MemorySnapshot,
    pub gpu: GpuSnapshot,
}

#[cfg(all(test, windows))]
mod tests {
    use super::process_birth_time_ms;
    #[test]
    fn native_birth_identity_is_stable_and_nonzero_without_scanning_other_processes() {
        let first = process_birth_time_ms(std::process::id(), 0).unwrap();
        assert!(first > 0);
        assert_eq!(process_birth_time_ms(std::process::id(), 0), Some(first));
        assert_eq!(process_birth_time_ms(0, 0), None);
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuSnapshot {
    pub logical_cores: usize,
    pub usage_percent: Option<f32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub struct HardwareSampler {
    system: System,
    gpu_sensor: Option<GpuSensor>,
    last_refresh: Instant,
    last_gpu_refresh: Instant,
    snapshot: HardwareSnapshot,
    executors: HashMap<String, ExecutorSnapshot>,
    completed_executors: VecDeque<ExecutorSnapshot>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutorSnapshot {
    process_id: u32,
    start_time_ms: u64,
    state: &'static str,
    memory_bytes: Option<u64>,
    peak_memory_bytes: Option<u64>,
    cpu_usage_percent: Option<f32>,
    baseline_memory_bytes: Option<u64>,
    gpu_memory_bytes: Option<u64>,
    baseline_gpu_memory_bytes: Option<u64>,
    gpu_memory_reason: Option<&'static str>,
}

#[derive(Clone)]
pub struct LeaseMemoryObservation {
    pub lease_id: String,
    pub process_id: u32,
    pub start_time_ms: u64,
    pub baseline_memory_bytes: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub state: &'static str,
}

#[derive(Clone)]
pub struct LeaseGpuObservation {
    pub lease_id: String,
    pub process_id: u32,
    pub start_time_ms: u64,
    pub baseline_memory_bytes: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub state: &'static str,
}

// Use the native birth timestamp on Windows: sysinfo's second precision alone cannot fence rapid PID reuse.
// Windows 使用原生出生时间；仅用 sysinfo 的秒级精度不能防止短时间内 PID 被复用。
#[cfg(windows)]
fn process_birth_time_ms(process_id: u32, _fallback_seconds: u64) -> Option<u64> {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime { low: u32, high: u32 }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut c_void;
        fn GetProcessTimes(process: *mut c_void, creation: *mut FileTime, exit: *mut FileTime,
            kernel: *mut FileTime, user: *mut FileTime) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let handle = unsafe { OpenProcess(0x1000, 0, process_id) };
    if handle.is_null() { return None; }
    let (mut creation, mut exit, mut kernel, mut user) =
        (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
    let status = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    unsafe { CloseHandle(handle); }
    if status == 0 { return None; }
    (((creation.high as u64) << 32 | creation.low as u64) / 10_000).checked_sub(11_644_473_600_000)
}

#[cfg(not(windows))]
fn process_birth_time_ms(_process_id: u32, fallback_seconds: u64) -> Option<u64> {
    Some(fallback_seconds * 1000)
}

impl HardwareSampler {
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        let gpu_sensor = GpuSensor::open();
        let snapshot = HardwareSnapshot {
            cpu: CpuSnapshot {
                logical_cores: std::thread::available_parallelism().map(|cores| cores.get())
                    .unwrap_or_else(|_| system.cpus().len().max(1)),
                usage_percent: None,
            },
            memory: MemorySnapshot {
                total_bytes: system.total_memory(),
                available_bytes: system.available_memory(),
            },
            // Sensor absence is not zero free VRAM and cannot authorize GPU allocations.
            // 传感器缺失不等于显存为零，更不能作为批准 GPU 分配的依据。
            gpu: gpu_sensor
                .as_ref()
                .map(GpuSensor::sample)
                .unwrap_or_else(|| GpuSnapshot::unknown("GPU_SENSOR_UNAVAILABLE")),
        };
        Self {
            system,
            gpu_sensor,
            last_refresh: Instant::now(),
            last_gpu_refresh: Instant::now(),
            snapshot,
            executors: HashMap::new(),
            completed_executors: VecDeque::new(),
        }
    }

    pub fn snapshot(&mut self, has_gpu_allocation: bool) -> HardwareSnapshot {
        // CPU sampling requires a time delta; a first sample stays explicitly unknown.
        // CPU 使用率需要两次采样时间差，首次采样明确保持未知。
        if self.last_refresh.elapsed() >= Duration::from_secs(1) {
            self.system.refresh_cpu_usage();
            self.system
                .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
            self.snapshot.cpu.usage_percent = Some(self.system.global_cpu_usage());
            self.snapshot.memory.total_bytes = self.system.total_memory();
            self.snapshot.memory.available_bytes = self.system.available_memory();
            self.last_refresh = Instant::now();
            self.refresh_executors();
        }
        let gpu_interval = if has_gpu_allocation
            || self
                .snapshot
                .gpu
                .usage_percent
                .is_some_and(|usage| usage > 0)
        {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        };
        if self.last_gpu_refresh.elapsed() >= gpu_interval {
            self.refresh_gpu();
        }
        self.snapshot.clone()
    }

    pub fn reconcile(&mut self, has_gpu_allocation: bool) -> HardwareSnapshot {
        // Pair a fresh free-memory sample with owned executor RSS before crediting materialized reservations.
        // 可用内存与自有进程 RSS 同轮采样后才抵扣已兑现预约，避免使用过期的可用内存增大额度。
        self.system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        self.snapshot.memory.total_bytes = self.system.total_memory();
        self.snapshot.memory.available_bytes = self.system.available_memory();
        self.refresh_executors();
        if has_gpu_allocation { self.refresh_gpu(); }
        self.snapshot(has_gpu_allocation)
    }

    pub fn memory_observations(&self) -> Vec<LeaseMemoryObservation> {
        self.executors.iter().map(|(lease_id, executor)| LeaseMemoryObservation {
            lease_id: lease_id.clone(), process_id: executor.process_id, start_time_ms: executor.start_time_ms,
            baseline_memory_bytes: executor.baseline_memory_bytes, memory_bytes: executor.memory_bytes,
            state: executor.state,
        }).collect()
    }

    pub fn gpu_memory_observations(&self) -> Vec<LeaseGpuObservation> {
        self.executors.iter().map(|(lease_id, executor)| LeaseGpuObservation {
            lease_id: lease_id.clone(), process_id: executor.process_id, start_time_ms: executor.start_time_ms,
            baseline_memory_bytes: executor.baseline_gpu_memory_bytes, memory_bytes: executor.gpu_memory_bytes,
            state: executor.state,
        }).collect()
    }

    fn refresh_gpu(&mut self) {
        self.refresh_executors();
        self.snapshot.gpu = self.gpu_sensor.as_ref().map(GpuSensor::sample)
            .unwrap_or_else(|| GpuSnapshot::unknown("GPU_SENSOR_UNAVAILABLE"));
        let sample = self.gpu_sensor.as_ref().map(GpuSensor::sample_processes)
            .unwrap_or_else(|| GpuProcessSample::unknown("GPU_SENSOR_UNAVAILABLE"));
        for executor in self.executors.values_mut() {
            let birth = self.system.process(Pid::from_u32(executor.process_id))
                .and_then(|process| process_birth_time_ms(executor.process_id, process.start_time()));
            // Pair free VRAM and process usage in one round, with birth identity checked after the driver query.
            // 同轮配对空闲显存与进程显存，并在驱动查询后复核出生时间；身份未知不产生兑现信用。
            executor.gpu_memory_bytes = if self.snapshot.gpu.state == "available" && executor.state == "running"
                && birth == Some(executor.start_time_ms) { sample.memory_for(executor.process_id) } else { None };
            executor.gpu_memory_reason = if executor.gpu_memory_bytes.is_none() {
                Some(sample.reason.unwrap_or("GPU_PROCESS_IDENTITY_UNCONFIRMED")) } else { None };
        }
        self.last_gpu_refresh = Instant::now();
    }

    fn refresh_pid(&mut self, process_id: u32) {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[Pid::from_u32(process_id)]),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
    }

    pub fn register_executor(
        &mut self,
        lease_id: String,
        process_id: u32,
        expected_start_time_ms: Option<u64>,
        owner_pid: u32,
    ) -> Value {
        if process_id == 0 || owner_pid == 0 || self.executors.len() >= 128 {
            return json!({"status":"denied","reason":"RESOURCE_EXECUTOR_INVALID"});
        }
        let mut current = process_id;
        let mut is_owned = false;
        for _ in 0..16 {
            if current == owner_pid {
                is_owned = true;
                break;
            }
            self.refresh_pid(current);
            let Some(process) = self.system.process(Pid::from_u32(current)) else {
                break;
            };
            let Some(parent) = process.parent() else {
                break;
            };
            current = parent.as_u32();
        }
        if !is_owned {
            return json!({"status":"denied","reason":"RESOURCE_EXECUTOR_NOT_OWNED"});
        }
        self.refresh_pid(process_id);
        let Some(process) = self.system.process(Pid::from_u32(process_id)) else {
            return json!({"status":"denied","reason":"RESOURCE_EXECUTOR_EXITED"});
        };
        let Some(start_time_ms) = process_birth_time_ms(process_id, process.start_time()) else {
            return json!({"status":"denied","reason":"RESOURCE_EXECUTOR_IDENTITY_UNKNOWN"});
        };
        if expected_start_time_ms.is_some_and(|stamp| stamp != start_time_ms) {
            return json!({"status":"denied","reason":"RESOURCE_EXECUTOR_REPLACED"});
        }
        let gpu_sample = self.gpu_sensor.as_ref().map(GpuSensor::sample_processes)
            .unwrap_or_else(|| GpuProcessSample::unknown("GPU_SENSOR_UNAVAILABLE"));
        if process_birth_time_ms(process_id, process.start_time()) != Some(start_time_ms) {
            return json!({"status":"denied","reason":"RESOURCE_EXECUTOR_REPLACED"});
        }
        let executor = ExecutorSnapshot {
            process_id,
            start_time_ms,
            state: "running",
            memory_bytes: if process.memory() > 0 {
                Some(process.memory())
            } else {
                None
            },
            peak_memory_bytes: if process.memory() > 0 {
                Some(process.memory())
            } else {
                None
            },
            cpu_usage_percent: None,
            // Shared-PID worker leases start from one baseline; pre-existing unrelated RSS is never credited.
            // 同一 PID 的工作线程共用一个基线；启动前已经占用的无关 RSS 不能抵扣新预约。
            baseline_memory_bytes: self.executors.values().find(|executor|
                executor.process_id == process_id && executor.start_time_ms == start_time_ms)
                .and_then(|executor| executor.baseline_memory_bytes).or_else(||
                    if process.memory() > 0 { Some(process.memory()) } else { None }),
            // A repeat registration keeps the first GPU baseline; pre-existing external buffers receive no credit.
            // 重复登记保留首次显存基线；先前存在的外部缓冲不能抵扣新预约。
            baseline_gpu_memory_bytes: self.executors.values().find(|executor|
                executor.process_id == process_id && executor.start_time_ms == start_time_ms)
                .map(|executor| executor.baseline_gpu_memory_bytes).unwrap_or_else(|| gpu_sample.memory_for(process_id)),
            gpu_memory_bytes: None,
            gpu_memory_reason: Some("GPU_PROCESS_MEMORY_AWAITING_PAIRED_SAMPLE"),
        };
        let result = json!({"status":"registered","leaseId":lease_id,"executor":executor});
        self.executors.insert(lease_id, executor);
        result
    }

    pub fn recover_executor(&mut self, lease_id: String, process_id: u32, start_time_ms: u64, owner_pid: u32) -> Value {
        if process_id == 0 || start_time_ms == 0 || owner_pid == 0 || self.executors.len() >= 128 {
            return json!({"status":"denied","reason":"RESOURCE_EXECUTOR_INVALID"});
        }
        self.refresh_pid(process_id);
        let state = match self.system.process(Pid::from_u32(process_id)) {
            None => Some("exited"),
            Some(process) if process_birth_time_ms(process_id, process.start_time()).is_some_and(|birth|birth != start_time_ms) => Some("replaced"),
            _ => None,
        };
        if let Some(state) = state {
            // Restored birth identities identify old work only; a new process with a recycled PID is never charged or killed.
            // 恢复的出生时间仅用于识别旧工作；PID 被复用的新进程不登记、扣费或结束。
            let executor = ExecutorSnapshot {process_id,start_time_ms,state,memory_bytes:None,peak_memory_bytes:None,
                cpu_usage_percent:None,baseline_memory_bytes:None,gpu_memory_bytes:None,
                baseline_gpu_memory_bytes:None,gpu_memory_reason:Some("GPU_PROCESS_IDENTITY_UNCONFIRMED")};
            self.executors.insert(lease_id.clone(),executor.clone());
            return json!({"status":"registered","leaseId":lease_id,"executor":executor});
        }
        self.register_executor(lease_id,process_id,Some(start_time_ms),owner_pid)
    }

    fn refresh_executors(&mut self) {
        let mut ids: Vec<Pid> = self
            .executors
            .values()
            .map(|executor| Pid::from_u32(executor.process_id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        if !ids.is_empty() {
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&ids),
                true,
                ProcessRefreshKind::nothing().with_memory().with_cpu(),
            );
        }
        for executor in self.executors.values_mut() {
            if let Some(process) = self.system.process(Pid::from_u32(executor.process_id)) {
                let birth = process_birth_time_ms(executor.process_id, process.start_time());
                if birth.is_none() {
                    executor.state = "identity-unknown";
                    executor.memory_bytes = None;
                    executor.gpu_memory_bytes = None;
                    executor.cpu_usage_percent = None;
                } else if birth != Some(executor.start_time_ms) {
                    executor.state = "replaced";
                    executor.memory_bytes = None;
                    executor.gpu_memory_bytes = None;
                    executor.cpu_usage_percent = None;
                } else {
                    executor.state = "running";
                    executor.memory_bytes = if process.memory() > 0 {
                        Some(process.memory())
                    } else {
                        None
                    };
                    if let Some(bytes) = executor.memory_bytes {
                        executor.peak_memory_bytes =
                            Some(executor.peak_memory_bytes.unwrap_or(bytes).max(bytes));
                    }
                    executor.cpu_usage_percent = if process.cpu_usage().is_finite() {
                        Some(process.cpu_usage())
                    } else {
                        None
                    };
                }
            } else {
                executor.state = "exited";
                executor.memory_bytes = None;
                executor.gpu_memory_bytes = None;
                executor.cpu_usage_percent = None;
            }
        }
    }

    pub fn executor_snapshot(&self) -> Value {
        let mut processes: HashMap<u32, &ExecutorSnapshot> = HashMap::new();
        for executor in self.executors.values() {
            processes.entry(executor.process_id).or_insert(executor);
        }
        // Worker threads share their owner's RSS; report each PID once, never sum duplicated thread leases.
        // 工作线程共享拥有者 RSS，同一 PID 仅报告一次，不能把多个线程预约的 RSS 相加。
        json!({"processes":processes.values().collect::<Vec<_>>(),"completed":self.completed_executors,
            "gpuMemorySource":"nvml-process-memory-with-native-birth-identity","gpuMemoryExact":false,
            "registeredLeases":self.executors.len(),"sampleIntervalMs":1000})
    }

    pub fn retired_executors(&self) -> Vec<String> {
        self.executors
            .iter()
            .filter(|(_, executor)| ["exited", "replaced"].contains(&executor.state))
            .map(|(lease_id, _)| lease_id.clone())
            .collect()
    }
    pub fn remove_executor(&mut self, lease_id: &str) {
        if let Some(mut executor) = self.executors.remove(lease_id) {
            if executor.state == "running" {
                executor.state = "released";
            }
            self.completed_executors.push_back(executor);
            while self.completed_executors.len() > 32 {
                self.completed_executors.pop_front();
            }
        }
    }
}
