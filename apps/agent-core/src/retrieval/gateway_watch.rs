//! Owned native directory notifications only signal dirty roots; the scanner still owns parsing and authorization.
//! 自有原生目录通知只合并变更根目录；解析、忽略规则与授权仍由现有扫描器处理，通知溢出时回到清单核对。

use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};

#[derive(Default)]
pub(crate) struct WatchSet {
    owners: HashMap<PathBuf, DirectoryWatch>,
    dirty: Arc<Mutex<HashSet<PathBuf>>>,
}
impl WatchSet {
    pub(crate) fn reconcile(&mut self, roots: &[PathBuf]) {
        self.owners.retain(|path, owner| roots.contains(path) && !owner.finished());
        for root in roots {
            // Unwatched roots remain covered by periodic inventory; bound native thread/handle residency.
            // 超出原生驻留容量的根仍由周期清单覆盖，不会丢失索引；限制的是句柄/线程而非用户挂载数量。
            if self.owners.len() >= 64 { break; }
            if !self.owners.contains_key(root) {
                if let Some(owner) = DirectoryWatch::start(root, self.dirty.clone()) { self.owners.insert(root.clone(), owner); }
            }
        }
    }
    pub(crate) fn watched(&self, root: &Path) -> bool { self.owners.get(root).is_some_and(|owner| !owner.finished()) }
    pub(crate) fn pending(&self) -> bool { !self.dirty.lock().unwrap().is_empty() }
    pub(crate) fn take(&self) -> HashSet<PathBuf> { std::mem::take(&mut *self.dirty.lock().unwrap()) }
}

struct DirectoryWatch {
    stop: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<usize>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl DirectoryWatch {
    fn finished(&self) -> bool { self.worker.as_ref().is_none_or(std::thread::JoinHandle::is_finished) }
    #[cfg(not(windows))]
    fn start(root: &Path, dirty: Arc<Mutex<HashSet<PathBuf>>>) -> Option<Self> { let _ = (root, dirty); None }
    #[cfg(windows)]
    fn start(root: &Path, dirty: Arc<Mutex<HashSet<PathBuf>>>) -> Option<Self> {
        use std::os::windows::ffi::OsStrExt;
        let path = root.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let directory = unsafe { CreateFileW(path.as_ptr(), 1, 7, std::ptr::null(), 3, 0x42000000, std::ptr::null_mut()) };
        if directory as isize == -1 { return None; }
        let directory = directory as usize;
        let handle = Arc::new(Mutex::new(Some(directory)));
        let stop = Arc::new(AtomicBool::new(false));
        let owned_handle = handle.clone(); let stopping = stop.clone(); let root = root.to_owned();
        let worker = std::thread::Builder::new().name("kynxa-directory-events".into()).spawn(move || {
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if !event.is_null() {
                let mut buffer = vec![0u8; 32768];
                while !stopping.load(Ordering::Acquire) {
                    let mut overlapped = Overlapped { internal:0, internal_high:0, offset:0, offset_high:0, event:event as usize };
                    unsafe { ResetEvent(event); }
                    let accepted = unsafe { ReadDirectoryChangesW(directory as *mut _, buffer.as_mut_ptr().cast(), buffer.len() as u32,
                        1, 0x1F, std::ptr::null_mut(), &mut overlapped, std::ptr::null()) };
                    if accepted == 0 { dirty.lock().unwrap().insert(root.clone()); break; }
                    loop {
                        let waited = unsafe { WaitForSingleObject(event, 500) };
                        if waited == 0 { break; }
                        if stopping.load(Ordering::Acquire) || waited != 258 {
                            unsafe { CancelIoEx(directory as *mut _, &overlapped); }
                            break;
                        }
                    }
                    let mut bytes = 0u32;
                    let completed = unsafe { GetOverlappedResult(directory as *mut _, &overlapped, &mut bytes, 1) };
                    if stopping.load(Ordering::Acquire) { break; }
                    // Zero returned bytes also means an overflow; invalidate the root inventory, not just one filename.
                    // 返回零字节也可能是通知溢出；合并整根变更提示并重新核对，不猜测删除或回放文件操作。
                    if bytes == 0 || relevant_events(&buffer[..bytes as usize]) { dirty.lock().unwrap().insert(root.clone()); }
                    if completed == 0 { break; }
                }
                unsafe { CloseHandle(event); }
            }
            if let Some(handle) = owned_handle.lock().unwrap().take() { unsafe { CloseHandle(handle as *mut _); } }
        }).ok();
        if worker.is_none() {
            if let Some(handle) = handle.lock().unwrap().take() { unsafe { CloseHandle(handle as *mut _); } }
            return None;
        }
        Some(Self { stop, handle, worker })
    }
}

#[cfg(windows)]
fn relevant_events(bytes: &[u8]) -> bool {
    let mut offset = 0;
    while offset + 12 <= bytes.len() {
        let next = u32::from_le_bytes(bytes[offset..offset+4].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(bytes[offset+8..offset+12].try_into().unwrap()) as usize;
        if offset + 12 + length > bytes.len() { return true; }
        let name = bytes[offset+12..offset+12+length].chunks_exact(2).map(|unit| u16::from_le_bytes([unit[0],unit[1]])).collect::<Vec<_>>();
        let path = PathBuf::from(String::from_utf16_lossy(&name));
        if crate::tools::service_source_reading::watch_path_relevant(&path) { return true; }
        if next == 0 { break; } offset += next;
    }
    false
}
impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        #[cfg(windows)]
        if let Some(handle) = *self.handle.lock().unwrap() { unsafe { CancelIoEx(handle as *mut _, std::ptr::null()); } }
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[cfg(windows)]
#[repr(C)]
struct Overlapped { internal: usize, internal_high: usize, offset: u32, offset_high: u32, event: usize }
#[cfg(windows)]
#[link(name="kernel32")]
unsafe extern "system" {
    fn CreateFileW(name:*const u16, access:u32, share:u32, security:*const std::ffi::c_void, creation:u32, flags:u32, template:*mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CreateEventW(security:*const std::ffi::c_void, manual:i32, initial:i32, name:*const u16) -> *mut std::ffi::c_void;
    fn ResetEvent(event:*mut std::ffi::c_void) -> i32;
    fn ReadDirectoryChangesW(directory:*mut std::ffi::c_void, buffer:*mut std::ffi::c_void, length:u32, subtree:i32, filter:u32, bytes:*mut u32, overlapped:*mut Overlapped, completion:*const std::ffi::c_void) -> i32;
    fn GetOverlappedResult(file:*mut std::ffi::c_void, overlapped:*const Overlapped, bytes:*mut u32, wait:i32) -> i32;
    fn WaitForSingleObject(handle:*mut std::ffi::c_void, milliseconds:u32) -> u32;
    fn CancelIoEx(file:*mut std::ffi::c_void, overlapped:*const Overlapped) -> i32;
    fn CloseHandle(handle:*mut std::ffi::c_void) -> i32;
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn native_directory_events_coalesce_and_release_owned_handles() {
        let root = std::env::temp_dir().join(format!("kynxa-watch-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let mut watches = WatchSet::default();
        watches.reconcile(&[root.clone()]);
        assert!(watches.watched(&root));
        let started = std::time::Instant::now();
        while !watches.pending() && started.elapsed() < std::time::Duration::from_secs(3) {
            std::thread::sleep(std::time::Duration::from_millis(30));
            std::fs::write(root.join("source.txt"), started.elapsed().as_nanos().to_string()).unwrap();
        }
        assert!(watches.take().contains(&root));
        drop(watches);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
