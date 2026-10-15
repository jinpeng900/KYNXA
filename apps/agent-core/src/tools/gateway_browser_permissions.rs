//! Keep portable Chromium's sandbox enabled when its files inherit a developer or extraction-folder ACL.
//! 便携 Chromium 在开发目录或解压目录中也保留沙箱；仅补齐其程序资源的只读执行权限。

use super::gateway::{GatewayToolContext, failure};
use crate::gateway_contract::GatewayError;
use std::path::Path;

/// This applies only to the exact bundled browser, never a user-selected executable or application data.
/// 仅处理清单指定的随包浏览器，不改用户指定程序、数据目录或扩展凭据。
pub(crate) async fn prepare(
    ctx: &GatewayToolContext,
    executable: &Path,
) -> Result<(), GatewayError> {
    #[cfg(windows)]
    {
        use std::collections::HashSet;
        use std::path::PathBuf;
        use std::process::Stdio;
        use std::sync::OnceLock;

        static PREPARED: OnceLock<tokio::sync::Mutex<HashSet<PathBuf>>> = OnceLock::new();
        let Some((application, manifest)) = super::gateway::bundled_mcp_manifest(ctx) else {
            return Ok(());
        };
        if super::gateway::bundled_mcp_path(&application, &manifest["browser"]["path"])
            .as_deref()
            != Some(executable)
        {
            return Ok(());
        }
        let directory = executable.parent().ok_or_else(permission_error)?;
        let mut prepared = PREPARED.get_or_init(Default::default).lock().await;
        if prepared.contains(directory) {
            return Ok(());
        }
        if !sandbox_can_read(directory) {
            let mut system = vec![0u16; 32768];
            let length = unsafe { GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32) };
            if length == 0 || length as usize >= system.len() {
                return Err(permission_error());
            }
            let tool = PathBuf::from(String::from_utf16_lossy(&system[..length as usize]))
                .join("icacls.exe");
            // Fixed arguments grant RX to AppContainer groups; no shell, write permission, or sandbox switch.
            // 固定实参只向 AppContainer 组授予 RX，不经 shell、不授写权限，也不关闭沙箱。
            let status = tokio::process::Command::new(tool)
                .arg(directory)
                .args([
                    "/grant",
                    "*S-1-15-2-1:(OI)(CI)(RX)",
                    "*S-1-15-2-2:(OI)(CI)(RX)",
                    "/Q",
                ])
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map_err(|_| permission_error())?;
            if !status.success() {
                return Err(permission_error());
            }
        }
        prepared.insert(directory.to_path_buf());
    }
    #[cfg(not(windows))]
    let _ = (ctx, executable);
    Ok(())
}

#[cfg(windows)]
fn permission_error() -> GatewayError {
    failure(
        "BROWSER_SANDBOX_ACCESS_DENIED",
        "随包浏览器缺少沙箱读取权限，且当前安装目录不允许补齐；请检查该目录权限。",
        503,
    )
}

#[cfg(windows)]
fn sandbox_can_read(directory: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    let mut acl = std::ptr::null_mut();
    if unsafe {
        GetNamedSecurityInfoW(path.as_ptr(), 1, 4, std::ptr::null_mut(), std::ptr::null_mut(),
            &mut acl, std::ptr::null_mut(), &mut descriptor)
    } != 0
    {
        return false;
    }
    // Inheritable directory entries also cover Chromium's DLLs and resource subdirectories.
    // 目录中的可继承规则同时覆盖 Chromium 的 DLL 和资源子目录，已有正确安装权限时不启动修复进程。
    let result = if acl.is_null() {
        true
    } else {
        let mut masks = [0u32; 2];
        let count = unsafe { (*acl).ace_count };
        for index in 0..count {
            let mut entry = std::ptr::null_mut();
            if unsafe { GetAce(acl, index as u32, &mut entry) } == 0 { continue; }
            let bytes = entry.cast::<u8>();
            let header = unsafe { std::slice::from_raw_parts(bytes, 4) };
            if header[0] != 0 || header[1] & 0x0b != 0x03
                || u16::from_le_bytes([header[2], header[3]]) < 24 { continue; }
            let mask = unsafe { *bytes.add(4).cast::<u32>() };
            let sid = unsafe { std::slice::from_raw_parts(bytes.add(8), 16) };
            for (slot, package) in [1u8, 2].into_iter().enumerate() {
                if sid == [1, 2, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, package, 0, 0, 0] {
                    masks[slot] |= mask;
                }
            }
        }
        masks.into_iter().all(|mask| mask & 0x1200a9 == 0x1200a9 || mask & 0xa0000000 == 0xa0000000)
    };
    unsafe { LocalFree(descriptor); }
    result
}

#[cfg(windows)]
#[repr(C)]
struct Acl { revision: u8, reserved: u8, size: u16, ace_count: u16, reserved2: u16 }

#[cfg(windows)]
#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetNamedSecurityInfoW(name: *const u16, kind: u32, information: u32,
        owner: *mut *mut std::ffi::c_void, group: *mut *mut std::ffi::c_void,
        dacl: *mut *mut Acl, sacl: *mut *mut Acl, descriptor: *mut *mut std::ffi::c_void) -> u32;
    fn GetAce(acl: *const Acl, index: u32, entry: *mut *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemDirectoryW(buffer: *mut u16, length: u32) -> u32;
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}
