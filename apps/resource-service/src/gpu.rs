use serde::Serialize;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuProcessMemory {
    pub process_id: u32,
    pub memory_bytes: Option<u64>,
}

pub struct GpuProcessSample {
    pub processes: Vec<GpuProcessMemory>,
    pub is_complete: bool,
    pub reason: Option<&'static str>,
}

impl GpuProcessSample {
    pub fn unknown(reason: &'static str) -> Self {
        Self { processes: Vec::new(), is_complete: false, reason: Some(reason) }
    }

    pub fn memory_for(&self, process_id: u32) -> Option<u64> {
        self.processes.iter().find(|process| process.process_id == process_id)
            .map(|process| process.memory_bytes).unwrap_or(if self.is_complete { Some(0) } else { None })
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuSnapshot {
    pub state: &'static str,
    pub device_id: Option<u32>,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub usage_percent: Option<u32>,
    pub reason: Option<&'static str>,
    pub execution_provider: Option<&'static str>,
    pub execution_device_id: Option<u32>,
    pub mapping_status: &'static str,
}

impl GpuSnapshot {
    pub fn unknown(reason: &'static str) -> Self {
        Self {
            state: "unknown",
            device_id: None,
            total_memory_bytes: None,
            available_memory_bytes: None,
            usage_percent: None,
            reason: Some(reason),
            execution_provider: None,
            execution_device_id: None,
            mapping_status: "unverified",
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::{GpuProcessMemory, GpuProcessSample, GpuSnapshot};
    use std::collections::HashMap;
    use std::ffi::{c_char, c_void};
    use std::ptr;

    type NvmlDevice = *mut c_void;
    type NvmlInit = unsafe extern "C" fn() -> u32;
    type NvmlShutdown = unsafe extern "C" fn() -> u32;
    type NvmlDeviceCount = unsafe extern "C" fn(*mut u32) -> u32;
    type NvmlDeviceHandle = unsafe extern "C" fn(u32, *mut NvmlDevice) -> u32;
    type NvmlDeviceMemory = unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> u32;
    type NvmlDeviceUtilization = unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> u32;
    type NvmlDeviceUuid = unsafe extern "C" fn(NvmlDevice, *mut c_char, u32) -> u32;
    type NvmlDeviceProcesses = unsafe extern "C" fn(NvmlDevice, *mut u32, *mut NvmlProcessInfo) -> u32;

    // NVML v3 uses the v2 process layout; WDDM reports u64::MAX when attribution is unavailable.
    // NVML v3 使用 v2 进程布局；WDDM 无法归属时返回 u64::MAX，绝不能当作占用或零。
    #[repr(C)]
    #[derive(Clone, Default)]
    struct NvmlProcessInfo {
        pid: u32,
        used_gpu_memory: u64,
        gpu_instance_id: u32,
        compute_instance_id: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct NvmlMemory {
        total: u64,
        free: u64,
        used: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct NvmlUtilization {
        gpu: u32,
        memory: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }

    pub struct GpuSensor {
        library: *mut c_void,
        device: NvmlDevice,
        shutdown: NvmlShutdown,
        memory: NvmlDeviceMemory,
        utilization: Option<NvmlDeviceUtilization>,
        compute_processes: Option<NvmlDeviceProcesses>,
        graphics_processes: Option<NvmlDeviceProcesses>,
        adapter_identity: Option<crate::gpu_mapping::AdapterIdentity>,
    }

    impl GpuSensor {
        pub fn open() -> Option<Self> {
            // Load only the system driver's DLL; never search the working directory or invoke a shell.
            // 仅加载系统驱动 DLL，不搜索工作目录，也不启动 shell 或轮询 nvidia-smi。
            let name: Vec<u16> = "nvml.dll\0".encode_utf16().collect();
            let library = unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), 0x0000_0800) };
            if library.is_null() {
                return None;
            }
            unsafe {
                let init = GetProcAddress(library, c"nvmlInit_v2".as_ptr());
                let shutdown = GetProcAddress(library, c"nvmlShutdown".as_ptr());
                let count = GetProcAddress(library, c"nvmlDeviceGetCount_v2".as_ptr());
                let handle = GetProcAddress(library, c"nvmlDeviceGetHandleByIndex_v2".as_ptr());
                let memory = GetProcAddress(library, c"nvmlDeviceGetMemoryInfo".as_ptr());
                let utilization =
                    GetProcAddress(library, c"nvmlDeviceGetUtilizationRates".as_ptr());
                let uuid = GetProcAddress(library, c"nvmlDeviceGetUUID".as_ptr());
                let compute_processes = GetProcAddress(library, c"nvmlDeviceGetComputeRunningProcesses_v3".as_ptr());
                let graphics_processes = GetProcAddress(library, c"nvmlDeviceGetGraphicsRunningProcesses_v3".as_ptr());
                if [init, shutdown, count, handle, memory]
                    .iter()
                    .any(|symbol| symbol.is_null())
                {
                    FreeLibrary(library);
                    return None;
                }
                let init: NvmlInit = std::mem::transmute(init);
                let shutdown: NvmlShutdown = std::mem::transmute(shutdown);
                let count: NvmlDeviceCount = std::mem::transmute(count);
                let handle: NvmlDeviceHandle = std::mem::transmute(handle);
                let memory: NvmlDeviceMemory = std::mem::transmute(memory);
                if init() != 0 {
                    FreeLibrary(library);
                    return None;
                }
                let mut devices = 0;
                let mut device = ptr::null_mut();
                if count(&mut devices) != 0
                    || devices == 0
                    || handle(0, &mut device) != 0
                    || device.is_null()
                {
                    shutdown();
                    FreeLibrary(library);
                    return None;
                }
                Some(Self {
                    library,
                    device,
                    shutdown,
                    memory,
                    utilization: if utilization.is_null() {
                        None
                    } else {
                        Some(std::mem::transmute::<*mut c_void, NvmlDeviceUtilization>(
                            utilization,
                        ))
                    },
                    compute_processes: if compute_processes.is_null() { None } else {
                        Some(std::mem::transmute::<*mut c_void, NvmlDeviceProcesses>(compute_processes)) },
                    graphics_processes: if graphics_processes.is_null() { None } else {
                        Some(std::mem::transmute::<*mut c_void, NvmlDeviceProcesses>(graphics_processes)) },
                    adapter_identity: if uuid.is_null() {
                        None
                    } else {
                        let uuid: NvmlDeviceUuid = std::mem::transmute(uuid);
                        let mut text = [0_u8; 96];
                        if uuid(device, text.as_mut_ptr().cast(), text.len() as u32) != 0 {
                            None
                        } else {
                            crate::gpu_mapping::parse_nvml_uuid(&text)
                                .and_then(|uuid| crate::gpu_mapping::adapter_identity(&uuid))
                        }
                    },
                })
            }
        }

        pub fn sample(&self) -> GpuSnapshot {
            let mut memory = NvmlMemory::default();
            let status = unsafe { (self.memory)(self.device, &mut memory) };
            if status != 0 || memory.total == 0 || memory.free > memory.total {
                return GpuSnapshot::unknown("GPU_NVML_SAMPLE_FAILED");
            }
            let mut utilization = NvmlUtilization::default();
            let usage_percent = self.utilization.and_then(|sample| {
                if unsafe { sample(self.device, &mut utilization) } == 0 && utilization.gpu <= 100 {
                    Some(utilization.gpu)
                } else {
                    None
                }
            });
            // Display docking can reorder DXGI ordinals; validate the LUID again before publishing a mapping.
            // 显示器连接可改变 DXGI 排序，每次发布设备映射前按 LUID 重新核对序号。
            let dml_device_id = self
                .adapter_identity
                .as_ref()
                .and_then(crate::gpu_mapping::dml_device_id);
            GpuSnapshot {
                state: "available",
                device_id: Some(0),
                total_memory_bytes: Some(memory.total),
                available_memory_bytes: Some(memory.free),
                usage_percent,
                reason: None,
                execution_provider: dml_device_id.map(|_| "dml"),
                execution_device_id: dml_device_id,
                mapping_status: if dml_device_id.is_some() {
                    "verified"
                } else {
                    "unverified"
                },
            }
        }

        pub fn sample_processes(&self) -> GpuProcessSample {
            query_processes(self.device, self.compute_processes, self.graphics_processes)
        }
    }

    fn query_processes(device: NvmlDevice, compute: Option<NvmlDeviceProcesses>, graphics: Option<NvmlDeviceProcesses>) -> GpuProcessSample {
        let mut processes: HashMap<u32, Option<u64>> = HashMap::new();
        let mut is_complete = true;
        for query in [compute, graphics] {
            let Some(query) = query else { is_complete = false; continue; };
            // Fixed bounds prevent driver churn from allocating unbounded IPC or process lists.
            // 固定上限防止驱动进程变化导致无界分配或 IPC，列表收紧失败明确未知。
            let mut count = 128_u32;
            let mut records = vec![NvmlProcessInfo::default(); count as usize];
            let status = unsafe { query(device, &mut count, records.as_mut_ptr()) };
            if status != 0 || count > records.len() as u32 { is_complete = false; continue; }
            for record in records.into_iter().take(count as usize) {
                if record.pid == 0 { continue; }
                let bytes = if record.used_gpu_memory == u64::MAX { None } else { Some(record.used_gpu_memory) };
                // Compute and graphics return total application memory, so duplicate PIDs are never summed.
                // 计算与图形接口都返回应用总占用，同一 PID 的两份记录不能相加；任一未知保持未知。
                processes.entry(record.pid).and_modify(|previous| *previous = match (*previous, bytes) {
                    (Some(first), Some(second)) => Some(first.min(second)), _ => None,
                }).or_insert(bytes);
            }
        }
        let has_unknown = processes.values().any(Option::is_none);
        GpuProcessSample { processes: processes.into_iter().map(|(process_id, memory_bytes)|
            GpuProcessMemory { process_id, memory_bytes }).collect(), is_complete,
            reason: if has_unknown { Some("GPU_PROCESS_MEMORY_DRIVER_UNAVAILABLE") }
                else if !is_complete { Some("GPU_PROCESS_MEMORY_QUERY_UNAVAILABLE") } else { None } }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        unsafe extern "C" fn reported(_device: NvmlDevice, count: *mut u32, records: *mut NvmlProcessInfo) -> u32 {
            unsafe { *count = 2;
                records.write(NvmlProcessInfo { pid: 8, used_gpu_memory: 100, ..Default::default() });
                records.add(1).write(NvmlProcessInfo { pid: 12, used_gpu_memory: u64::MAX, ..Default::default() }); }
            0
        }
        unsafe extern "C" fn too_many(_device: NvmlDevice, count: *mut u32, _records: *mut NvmlProcessInfo) -> u32 {
            unsafe { *count = 129; } 7
        }
        #[test]
        fn driver_lists_deduplicate_contexts_and_keep_wddm_sentinel_unknown() {
            let sample = query_processes(ptr::null_mut(), Some(reported), Some(reported));
            assert!(sample.is_complete);
            assert_eq!(sample.processes.len(), 2);
            assert_eq!(sample.memory_for(8), Some(100));
            assert_eq!(sample.memory_for(12), None);
            assert_eq!(sample.reason, Some("GPU_PROCESS_MEMORY_DRIVER_UNAVAILABLE"));
        }
        #[test]
        fn oversized_or_unsupported_lists_cannot_fabricate_absent_process_zeroes() {
            for query in [None, Some(too_many as NvmlDeviceProcesses)] {
                let sample = query_processes(ptr::null_mut(), query, Some(reported));
                assert!(!sample.is_complete);
                assert_eq!(sample.memory_for(999), None);
                assert_eq!(sample.memory_for(8), Some(100));
            }
        }
    }

    impl Drop for GpuSensor {
        fn drop(&mut self) {
            unsafe {
                (self.shutdown)();
                FreeLibrary(self.library);
            }
        }
    }
}

#[cfg(windows)]
pub use windows::GpuSensor;

#[cfg(not(windows))]
pub struct GpuSensor;
#[cfg(not(windows))]
impl GpuSensor {
    pub fn open() -> Option<Self> {
        None
    }
    pub fn sample(&self) -> GpuSnapshot {
        GpuSnapshot::unknown("GPU_SENSOR_PLATFORM_UNSUPPORTED")
    }
    pub fn sample_processes(&self) -> GpuProcessSample {
        GpuProcessSample::unknown("GPU_SENSOR_PLATFORM_UNSUPPORTED")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_absence_is_zero_only_for_a_complete_driver_list() {
        let mut sample = GpuProcessSample::unknown("fixture");
        assert_eq!(sample.memory_for(1), None);
        sample.is_complete = true;
        assert_eq!(sample.memory_for(1), Some(0));
        sample.processes.push(GpuProcessMemory { process_id: 1, memory_bytes: None });
        assert_eq!(sample.memory_for(1), None);
        sample.processes[0].memory_bytes = Some(100);
        assert_eq!(sample.memory_for(1), Some(100));
    }
}
