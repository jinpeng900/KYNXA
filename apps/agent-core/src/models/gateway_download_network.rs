//! Download-only routing honors environment proxies, Windows proxy/PAC and transparent VPNs.
//! 下载专用路由遵守环境代理、Windows 代理/PAC 和透明 VPN，不修改聊天接口客户端。
use super::gateway::ModelError;

static FOREGROUND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(super) struct ForegroundRequest;
impl ForegroundRequest {
    pub fn new() -> Self { FOREGROUND.fetch_add(1, std::sync::atomic::Ordering::AcqRel); Self }
}
impl Drop for ForegroundRequest {
    fn drop(&mut self) { FOREGROUND.fetch_sub(1, std::sync::atomic::Ordering::AcqRel); }
}
pub(super) fn foreground_active() -> bool { FOREGROUND.load(std::sync::atomic::Ordering::Acquire) > 0 }

pub(super) async fn builder(url: &str) -> Result<reqwest::ClientBuilder, ModelError> {
    let builder = reqwest::Client::builder();
    // Explicit environment configuration wins, including NO_PROXY handled by reqwest.
    // 显式环境代理优先，NO_PROXY 继续由 reqwest 处理。
    if ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"]
        .iter().any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty())) { return Ok(builder); }
    #[cfg(all(windows, not(test)))]
    {
        let url = url.to_owned();
        let route = tokio::task::spawn_blocking(move || windows_proxy(&url)).await
            .map_err(|_|ModelError::new("DOWNLOAD_PROXY_UNAVAILABLE", "系统下载代理暂不可用。", 503))??;
        if let Some((address, bypass)) = route {
            let bypass = [bypass, std::env::var("NO_PROXY").or_else(|_|std::env::var("no_proxy")).unwrap_or_default()].join(",");
            let proxy = reqwest::Proxy::all(&address)
                .map_err(|_|ModelError::new("DOWNLOAD_PROXY_UNAVAILABLE", "系统下载代理配置无效。", 400))?
                .no_proxy(reqwest::NoProxy::from_string(&bypass));
            return Ok(builder.proxy(proxy));
        }
    }
    let _ = url;
    Ok(builder)
}

#[cfg(all(windows, not(test)))]
fn windows_proxy(url: &str) -> Result<Option<(String, String)>, ModelError> {
    use std::{ffi::c_void, ptr::null};
    #[repr(C)] struct Config { detect: i32, pac: *mut u16, proxy: *mut u16, bypass: *mut u16 }
    #[repr(C)] struct Options { flags: u32, detect_flags: u32, pac: *const u16, reserved: *const c_void, reserved_flags: u32, auto_logon: i32 }
    #[repr(C)] struct ProxyInfo { access: u32, proxy: *mut u16, bypass: *mut u16 }
    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpGetIEProxyConfigForCurrentUser(config: *mut Config) -> i32;
        fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> *mut c_void;
        fn WinHttpSetTimeouts(handle: *mut c_void, resolve: i32, connect: i32, send: i32, receive: i32) -> i32;
        fn WinHttpGetProxyForUrl(handle: *mut c_void, url: *const u16, options: *const Options, info: *mut ProxyInfo) -> i32;
        fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" { fn GlobalFree(memory: *mut c_void) -> *mut c_void; }
    unsafe fn text(value: *const u16) -> String {
        if value.is_null() { return String::new(); }
        let mut length = 0;
        while length < 32768 && unsafe { *value.add(length) } != 0 { length += 1; }
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, length) })
    }
    // All strings returned by WinHTTP are owned here, including the PAC result.
    // WinHTTP 返回的字符串和 PAC 结果均在本次调用内释放。
    unsafe {
        let mut config: Config = std::mem::zeroed();
        if WinHttpGetIEProxyConfigForCurrentUser(&mut config) == 0 { return Ok(None); }
        let mut proxy = text(config.proxy);
        let mut bypass = text(config.bypass);
        let mut failure = false;
        let explicit_pac = !config.pac.is_null();
        if !config.pac.is_null() || config.detect != 0 {
            let agent: Vec<u16> = "KYNXA-downloads\0".encode_utf16().collect();
            let handle = WinHttpOpen(agent.as_ptr(), 1, null(), null(), 0);
            if !handle.is_null() {
                WinHttpSetTimeouts(handle, 2000, 3000, 3000, 3000);
                let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
                let options = Options { flags: if !config.pac.is_null() { 2 } else { 1 }, detect_flags: 3,
                    pac: config.pac, reserved: null(), reserved_flags: 0, auto_logon: 0 };
                let mut info: ProxyInfo = std::mem::zeroed();
                if WinHttpGetProxyForUrl(handle, url.as_ptr(), &options, &mut info) != 0 {
                    proxy = text(info.proxy); bypass = text(info.bypass);
                } else { failure = explicit_pac && proxy.is_empty(); }
                GlobalFree(info.proxy.cast()); GlobalFree(info.bypass.cast()); WinHttpCloseHandle(handle);
            } else { failure = explicit_pac && proxy.is_empty(); }
        }
        GlobalFree(config.pac.cast()); GlobalFree(config.proxy.cast()); GlobalFree(config.bypass.cast());
        if failure { return Err(ModelError::new("DOWNLOAD_PROXY_UNAVAILABLE", "自动下载代理解析失败，请检查系统代理或 VPN；已有断点保留。", 503)); }
        if proxy.is_empty() { return Ok(None); }
        let selected = proxy.split(';').filter_map(|part| part.split_once('='))
            .find(|(scheme, _)| scheme.eq_ignore_ascii_case("https")).map(|(_,value)|value.to_owned())
            .unwrap_or_else(||proxy.split(';').next().unwrap_or("").split('=').next_back().unwrap_or("").to_owned());
        let address = if selected.contains("://") { selected } else { format!("http://{selected}") };
        let bypass = bypass.replace("<local>", "localhost,127.0.0.1,::1").replace(';', ",").replace("*.", ".");
        Ok(Some((address, bypass)))
    }
}
