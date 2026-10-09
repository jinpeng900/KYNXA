use serde::Serialize;

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
    use super::GpuSnapshot;
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
}
