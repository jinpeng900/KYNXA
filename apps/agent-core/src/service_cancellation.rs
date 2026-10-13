//! Shared cancellation leases belong to request lifecycles, independent of storage and tool domains.
//! 共享取消租约属于请求生命周期，不依赖存储或工具领域。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
struct Entry {
    token: Arc<AtomicBool>,
    users: usize,
    owners: usize,
}
static TOKENS: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
thread_local! { static CURRENT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) }; }

fn valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
}

/// Tokens are shared across Node isolates; only atomic flags cross worker-thread boundaries.
/// 取消令牌跨 Node 隔离环境共享，线程间仅传递原子标志，不借用 JavaScript 缓冲区。
pub struct CancellationLease {
    id: String,
    pub token: Arc<AtomicBool>,
}
impl CancellationLease {
    pub fn acquire(id: &str) -> Result<Self, String> {
        if !valid(id) {
            return Err("AGENT_CORE_INVALID_ARGUMENT".into());
        }
        let mut tokens = TOKENS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| "AGENT_CORE_CANCEL_REGISTRY_FAILED")?;
        if !tokens.contains_key(id) && tokens.len() >= 512 {
            return Err("AGENT_CORE_SERVICE_CAPACITY".into());
        }
        let entry = tokens.entry(id.into()).or_default();
        entry.users += 1;
        Ok(Self {
            id: id.into(),
            token: entry.token.clone(),
        })
    }
    pub fn enter(&self) -> ContextGuard {
        let previous = CURRENT.with(|current| current.replace(Some(self.token.clone())));
        ContextGuard(previous)
    }
}
impl Drop for CancellationLease {
    fn drop(&mut self) {
        if let Some(registry) = TOKENS.get() {
            if let Ok(mut tokens) = registry.lock() {
                if let Some(entry) = tokens.get_mut(&self.id) {
                    entry.users = entry.users.saturating_sub(1);
                    if entry.users == 0 && entry.owners == 0 {
                        tokens.remove(&self.id);
                    }
                }
            }
        }
    }
}
pub struct ContextGuard(Option<Arc<AtomicBool>>);
impl Drop for ContextGuard {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            current.replace(self.0.take());
        });
    }
}
pub fn is_cancelled() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Acquire))
    })
}
/// Native inference observes the same request token as SQLite and file readers.
/// 原生推理复用 SQLite 与文件读取所在请求的同一取消标记。
pub fn current_token() -> Option<Arc<AtomicBool>> {
    CURRENT.with(|current| current.borrow().clone())
}
/// A worker request owns its token across several native transactions, including time between them.
/// worker 请求跨多次原生事务持有取消令牌，包括两次事务之间的空档。
pub fn retain(id: &str) -> Result<(), String> {
    if !valid(id) {
        return Err("AGENT_CORE_INVALID_ARGUMENT".into());
    }
    let mut tokens = TOKENS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "AGENT_CORE_CANCEL_REGISTRY_FAILED")?;
    if !tokens.contains_key(id) && tokens.len() >= 512 {
        return Err("AGENT_CORE_SERVICE_CAPACITY".into());
    }
    let entry = tokens.entry(id.into()).or_default();
    entry.owners += 1;
    Ok(())
}
pub fn cancel(id: &str) -> Result<(), String> {
    if !valid(id) {
        return Err("AGENT_CORE_INVALID_ARGUMENT".into());
    }
    let mut tokens = TOKENS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "AGENT_CORE_CANCEL_REGISTRY_FAILED")?;
    if !tokens.contains_key(id) && tokens.len() >= 512 {
        return Err("AGENT_CORE_SERVICE_CAPACITY".into());
    }
    tokens
        .entry(id.into())
        .or_default()
        .token
        .store(true, Ordering::Release);
    Ok(())
}
pub fn release(id: &str) -> Result<(), String> {
    if !valid(id) {
        return Err("AGENT_CORE_INVALID_ARGUMENT".into());
    }
    if let Some(registry) = TOKENS.get() {
        let mut tokens = registry
            .lock()
            .map_err(|_| "AGENT_CORE_CANCEL_REGISTRY_FAILED")?;
        if let Some(entry) = tokens.get_mut(id) {
            entry.owners = entry.owners.saturating_sub(1);
            if entry.users == 0 && entry.owners == 0 {
                tokens.remove(id);
            }
        }
    }
    Ok(())
}
