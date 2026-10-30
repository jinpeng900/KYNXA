//! Throughput probes share a resource budget; foreground model requests take priority.
//! 按吞吐收益试探并发，共享资源预算，前台模型请求优先。
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use super::super::super::gateway_download_network as network;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static FILES: AtomicUsize = AtomicUsize::new(0);
static AVAILABLE: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub(super) struct Connection;
impl Drop for Connection {
    fn drop(&mut self) { ACTIVE.fetch_sub(1, Ordering::AcqRel); AVAILABLE.notify_waiters(); }
}
pub(super) struct FileTransfer;
impl FileTransfer { pub fn new() -> Self { FILES.fetch_add(1, Ordering::AcqRel); Self } }
impl Drop for FileTransfer { fn drop(&mut self) { FILES.fetch_sub(1, Ordering::AcqRel); } }

// Each connection reserves 1 MiB of headroom; use only 1/64 of available RAM.
// 每连接预留 1 MiB 余量，下载仅使用当前可用内存的 1/64。
pub(super) fn memory_capacity() -> usize {
    #[cfg(windows)] {
        #[repr(C)] struct Memory { length: u32, load: u32, total: u64, available: u64,
            total_page: u64, available_page: u64, total_virtual: u64, available_virtual: u64, extended: u64 }
        #[link(name="kernel32")] unsafe extern "system" { fn GlobalMemoryStatusEx(value: *mut Memory) -> i32; }
        let mut value: Memory = unsafe { std::mem::zeroed() };
        value.length = std::mem::size_of::<Memory>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut value) } != 0 { return (value.available / 64 / (1024*1024)).max(1) as usize; }
    }
    // Unknown resource capacity keeps the safe starting level, rather than assuming unlimited RAM.
    // 无法读取资源时沿用起始档，不假设内存无限。
    2
}

pub(super) fn resource_capacity(directory: &Path) -> usize {
    let memory = memory_capacity();
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        #[link(name="kernel32")] unsafe extern "system" {
            fn GetDiskFreeSpaceExW(path: *const u16, free: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
        }
        let path: Vec<u16> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0;
        if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) } != 0 {
            return memory.min((free / 64 / ranges::RANGE_BYTES).max(1) as usize);
        }
    }
    let _ = directory;
    memory
}

pub(super) async fn connection(receiver: &mut watch::Receiver<DownloadAction>) -> Option<Connection> {
    loop {
        // Register before checking the counter, so a release cannot be lost between check and wait.
        // 先注册通知再检查计数，避免错过连接释放。
        let changed = AVAILABLE.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let limit = if network::foreground_active() { 1 } else { memory_capacity() };
        let active = ACTIVE.load(Ordering::Acquire);
        if active < limit && ACTIVE.compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire).is_ok() { return Some(Connection); }
        tokio::select! {
            _ = interrupted(receiver) => return None,
            _ = &mut changed => {},
            _ = tokio::time::sleep(Duration::from_millis(250)) => {},
        }
    }
}

pub(super) fn file_capacity(resources: usize) -> usize {
    resources.min(memory_capacity()).checked_div(FILES.load(Ordering::Acquire).max(1)).unwrap_or(1).max(1)
}

pub(super) struct Controller {
    pub target: usize,
    pub reason: &'static str,
    stable: usize,
    baseline: u64,
    start: Instant,
    bytes: u64,
    next_probe: Instant,
    settling: bool,
    foreground: bool,
}
impl Controller {
    pub fn new(now: Instant) -> Self {
        Self { target: 2, reason: "", stable: 2, baseline: 0, start: now, bytes: 0,
            next_probe: now, settling: false, foreground: false }
    }
    fn reset(&mut self, now: Instant, bytes: u64) { self.start = now; self.bytes = bytes; }
    pub fn congestion(&mut self, now: Instant, bytes: u64, delay: Duration) {
        self.target = (self.target / 2).max(1); self.stable = self.target; self.baseline = 0;
        self.next_probe = now + delay.max(Duration::from_secs(30));
        self.reason = "backoff"; self.settling = false; self.reset(now, bytes);
    }
    pub fn observe(&mut self, now: Instant, bytes: u64, capacity: usize, foreground: bool) {
        let capacity = capacity.max(1);
        if foreground {
            self.foreground = true; self.target = 1; self.stable = 1; self.baseline = 0;
            self.reason = "chat-priority"; self.reset(now, bytes); return;
        }
        if self.foreground {
            *self = Self::new(now); self.bytes = bytes; self.target = 2.min(capacity); return;
        }
        if self.target > capacity {
            self.target = capacity; self.stable = capacity; self.baseline = 0;
            self.reason = "resource-limited"; self.settling = false; self.reset(now, bytes); return;
        }
        if now.duration_since(self.start) < Duration::from_secs(6) { return; }
        if self.settling { self.settling = false; self.reset(now, bytes); return; }
        let rate = ((bytes.saturating_sub(self.bytes)) as f64 / now.duration_since(self.start).as_secs_f64()) as u64;
        self.reset(now, bytes);
        if self.baseline > 0 && self.target > self.stable {
            if rate < self.baseline.saturating_add(self.baseline / 10) {
                self.target = self.stable; self.baseline = 0; self.reason = "no-gain";
                self.next_probe = now + Duration::from_secs(30); return;
            }
            self.stable = self.target;
        }
        if rate == 0 || now < self.next_probe || self.target >= capacity { return; }
        self.stable = self.target; self.baseline = rate;
        self.target = self.target.saturating_mul(2).min(capacity);
        self.reason = "probing"; self.settling = true;
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn adaptive_download_probes_beyond_sixteen_and_rolls_back() {
        let mut now = Instant::now(); let mut c = Controller::new(now); let mut bytes = 0;
        for expected in [4, 8, 16, 32, 64] {
            now += Duration::from_secs(6); bytes += expected as u64 * 1_000_000;
            c.observe(now, bytes, 128, false); assert_eq!(c.target, expected);
            now += Duration::from_secs(6); c.observe(now, bytes, 128, false);
        }
        now += Duration::from_secs(6); c.observe(now, bytes + 1, 128, false);
        assert_eq!(c.target, 32); assert_eq!(c.reason, "no-gain");
    }
    #[test] fn adaptive_download_congestion_resources_and_chat_take_priority() {
        let now = Instant::now(); let mut c = Controller::new(now);
        c.observe(now + Duration::from_secs(6), 6_000_000, 64, false); assert_eq!(c.target, 4);
        c.congestion(now + Duration::from_secs(7), 6_000_000, Duration::from_secs(40)); assert_eq!(c.target, 2);
        c.observe(now + Duration::from_secs(20), 20_000_000, 64, false); assert_eq!(c.target, 2);
        c.observe(now + Duration::from_secs(21), 20_000_000, 1, false); assert_eq!(c.target, 1);
        c.observe(now + Duration::from_secs(22), 20_000_000, 64, true); assert_eq!(c.reason, "chat-priority");
        c.observe(now + Duration::from_secs(23), 20_000_000, 64, false); assert_eq!(c.target, 2);
    }
}
