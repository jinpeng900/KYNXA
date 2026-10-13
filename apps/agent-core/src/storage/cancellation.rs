// Preserve the storage call path while sharing one request-wide cancellation registry.
// 保留存储调用路径，并共享同一请求级取消注册表。
pub use crate::service_cancellation::{
    CancellationLease, ContextGuard, cancel, is_cancelled, release, retain,
};
