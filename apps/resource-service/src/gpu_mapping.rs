// Adapter mapping is isolated from sampling because CUDA and DXGI identities are initialized once.
// 适配器映射与采样分离，CUDA 与 DXGI 设备身份仅在初始化时核对。
#[cfg(windows)]
pub struct AdapterIdentity {
    low: u32,
    high: i32,
}

#[cfg(windows)]
pub fn adapter_identity(nvml_uuid: &[u8; 16]) -> Option<AdapterIdentity> {
    use std::ffi::{c_char, c_void};
    use std::ptr;

    type CudaInit = unsafe extern "system" fn(u32) -> i32;
    type CudaDeviceGet = unsafe extern "system" fn(*mut i32, i32) -> i32;
    type CudaDeviceUuid = unsafe extern "system" fn(*mut [u8; 16], i32) -> i32;
    type CudaDeviceLuid = unsafe extern "system" fn(*mut u8, *mut u32, i32) -> i32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }
    struct DriverLibrary(*mut c_void);
    impl Drop for DriverLibrary {
        fn drop(&mut self) {
            unsafe {
                FreeLibrary(self.0);
            }
        }
    }

    let name: Vec<u16> = "nvcuda.dll\0".encode_utf16().collect();
    let library = unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), 0x0000_0800) };
    if library.is_null() {
        return None;
    }
    let _owned_library = DriverLibrary(library);
    unsafe {
        let init = GetProcAddress(library, c"cuInit".as_ptr());
        let get = GetProcAddress(library, c"cuDeviceGet".as_ptr());
        let uuid = GetProcAddress(library, c"cuDeviceGetUuid".as_ptr());
        let luid = GetProcAddress(library, c"cuDeviceGetLuid".as_ptr());
        if [init, get, uuid, luid]
            .iter()
            .any(|symbol| symbol.is_null())
        {
            return None;
        }
        let init: CudaInit = std::mem::transmute(init);
        let get: CudaDeviceGet = std::mem::transmute(get);
        let uuid: CudaDeviceUuid = std::mem::transmute(uuid);
        let luid: CudaDeviceLuid = std::mem::transmute(luid);
        let mut device = 0;
        let mut actual_uuid = [0_u8; 16];
        if init(0) != 0
            || get(&mut device, 0) != 0
            || uuid(&mut actual_uuid, device) != 0
            || &actual_uuid != nvml_uuid
        {
            return None;
        }
        let mut actual_luid = [0_u8; 8];
        let mut node_mask = 0;
        if luid(actual_luid.as_mut_ptr(), &mut node_mask, device) != 0 || node_mask == 0 {
            return None;
        }
        let low = u32::from_le_bytes(actual_luid[..4].try_into().ok()?);
        let high = i32::from_le_bytes(actual_luid[4..].try_into().ok()?);
        Some(AdapterIdentity { low, high })
    }
}

#[cfg(windows)]
pub fn dml_device_id(identity: &AdapterIdentity) -> Option<u32> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        // Bound adapter enumeration; release every COM interface through its owned wrapper.
        // 适配器枚举有固定上限，所有 COM 接口由拥有者包装释放。
        for ordinal in 0..16 {
            let adapter = match factory.EnumAdapters1(ordinal) {
                Ok(adapter) => adapter,
                Err(_) => break,
            };
            let description = adapter.GetDesc1().ok()?;
            if description.VendorId == 0x10de
                && description.AdapterLuid.LowPart == identity.low
                && description.AdapterLuid.HighPart == identity.high
            {
                return Some(ordinal);
            }
        }
    }
    None
}

pub fn parse_nvml_uuid(text: &[u8]) -> Option<[u8; 16]> {
    let text = std::str::from_utf8(text.split(|byte| *byte == 0).next()?).ok()?;
    let text = text.strip_prefix("GPU-")?;
    if text.len() != 36
        || [8, 13, 18, 23]
            .iter()
            .any(|index| text.as_bytes()[*index] != b'-')
    {
        return None;
    }
    let hexadecimal: String = text.chars().filter(|character| *character != '-').collect();
    if hexadecimal.len() != 32 || !hexadecimal.is_ascii() {
        return None;
    }
    let mut uuid = [0_u8; 16];
    for (index, byte) in uuid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hexadecimal[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uuid_parsing_accepts_only_complete_gpu_device_identity() {
        assert_eq!(
            parse_nvml_uuid(b"GPU-00010203-0405-0607-0809-0a0b0c0d0e0f\0"),
            Some([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
        assert_eq!(
            parse_nvml_uuid(b"GPU-00010203-0405-0607-0809-0a0b0c0d0e0z"),
            None
        );
        assert_eq!(parse_nvml_uuid(b"GPU-00010203"), None);
        assert_eq!(
            parse_nvml_uuid(b"MIG-00010203-0405-0607-0809-0a0b0c0d0e0f"),
            None
        );
    }
}
