//! Bound actual model requests, not task count; cooling or child waits hold no generation slot.
//! 约束真实模型请求，不限制任务总数；冷却、子任务等待不占用生成槽位。

use crate::models::gateway::ModelError;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Notify, watch};

struct State {
    api_active: usize,
    api_child_active: usize,
    api_limit: usize,
    successful: usize,
    baseline_ms: f64,
    local_active: HashMap<String, usize>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            api_active: 0,
            api_child_active: 0,
            api_limit: 4,
            successful: 0,
            baseline_ms: 0.0,
            local_active: HashMap::new(),
        }
    }
}
#[derive(Default)]
pub struct ModelSlots {
    state: Mutex<State>,
    changed: Notify,
}
pub struct ModelSlot {
    pool: Arc<ModelSlots>,
    local: Option<String>,
    primary: bool,
    started: Instant,
}
impl ModelSlots {
    pub async fn acquire(
        self: &Arc<Self>,
        connection: &Value,
        _model: &str,
        primary: bool,
        mut cancellation: watch::Receiver<bool>,
    ) -> Result<ModelSlot, ModelError> {
        let endpoint = connection["baseUrl"].as_str().unwrap_or("");
        let local = connection["protocol"] == "ollama"
            || url::Url::parse(endpoint).ok().is_some_and(|url| {
                url.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                })
            });
        let key = local.then(|| endpoint.to_owned());
        loop {
            // Subscribe before checking the count, avoiding a lost slot-release wakeup.
            // 先注册唤醒再检查计数，避免释放槽位后的漏唤醒。
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if *cancellation.borrow() {
                return Err(ModelError::cancelled());
            }
            {
                let mut state = self.state.lock().map_err(|_| {
                    ModelError::new("MODEL_POOL_UNAVAILABLE", "模型调度不可用。", 503)
                })?;
                let available = key.as_ref().map_or(
                    state.api_active < state.api_limit
                        && (primary || state.api_child_active < state.api_limit.saturating_sub(1)),
                    |key| state.local_active.get(key).copied().unwrap_or(0) == 0,
                );
                if available {
                    if let Some(key) = &key {
                        *state.local_active.entry(key.clone()).or_default() += 1;
                    } else {
                        state.api_active += 1;
                        if !primary {
                            state.api_child_active += 1;
                        }
                    }
                    return Ok(ModelSlot {
                        pool: self.clone(),
                        local: key,
                        primary,
                        started: Instant::now(),
                    });
                }
            }
            tokio::select! { _ = changed => {}, _ = async { if cancellation.changed().await.is_err() { std::future::pending::<()>().await; } } => {}, }
        }
    }
}
impl ModelSlot {
    pub fn observe(&self, success: bool, throttled: bool) {
        if self.local.is_some() {
            return;
        }
        if let Ok(mut state) = self.pool.state.lock() {
            let elapsed = self.started.elapsed().as_millis() as f64;
            if throttled {
                state.api_limit = (state.api_limit / 2).max(2);
                state.successful = 0;
                return;
            }
            if !success {
                return;
            }
            if state.baseline_ms == 0.0 {
                state.baseline_ms = elapsed;
            }
            let healthy = elapsed <= state.baseline_ms * 1.5
                || self.started.elapsed() <= Duration::from_secs(5);
            state.baseline_ms = state.baseline_ms * 0.9 + elapsed * 0.1;
            if healthy {
                state.successful += 1;
            } else {
                state.successful = 0;
            }
            // Increase only after observed progress; hardware admission and provider limits remain independent.
            // 仅观察到有效进展后扩容；硬件准入及供应商限流仍独立生效。
            if state.successful >= 8 {
                state.api_limit = (state.api_limit + 1).min(8);
                state.successful = 0;
                self.pool.changed.notify_waiters();
            }
        }
    }
}
impl Drop for ModelSlot {
    fn drop(&mut self) {
        if let Ok(mut state) = self.pool.state.lock() {
            if let Some(key) = &self.local {
                if let Some(count) = state.local_active.get_mut(key) {
                    *count = count.saturating_sub(1);
                }
                state.local_active.retain(|_, count| *count > 0);
            } else {
                state.api_active = state.api_active.saturating_sub(1);
                if !self.primary {
                    state.api_child_active = state.api_child_active.saturating_sub(1);
                }
            }
        }
        self.pool.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn child_requests_leave_a_slot_for_primary_and_cancel_without_admission() {
        let pool = Arc::new(ModelSlots::default());
        let connection =
            json!({"baseUrl":"https://fixture.invalid/v1","protocol":"openai-completions"});
        let (owner, cancellation) = watch::channel(false);
        let mut children = Vec::new();
        for _ in 0..3 {
            children.push(
                pool.acquire(&connection, "fixture", false, cancellation.clone())
                    .await
                    .unwrap(),
            );
        }
        let waiting_pool = pool.clone();
        let waiting_connection = connection.clone();
        let (cancel, stopped) = watch::channel(false);
        let blocked = tokio::spawn(async move {
            waiting_pool
                .acquire(&waiting_connection, "fixture", false, stopped)
                .await
        });
        let primary = tokio::time::timeout(
            Duration::from_millis(100),
            pool.acquire(&connection, "fixture", true, cancellation),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!blocked.is_finished());
        cancel.send_replace(true);
        assert_eq!(blocked.await.unwrap().err().unwrap().code, "ABORT_ERR");
        drop(primary);
        drop(children);
        drop(owner);
        assert_eq!(pool.state.lock().unwrap().api_active, 0);
    }

    #[tokio::test]
    async fn local_endpoint_serializes_different_models_and_releases_after_cancel() {
        let pool = Arc::new(ModelSlots::default());
        let connection =
            json!({"baseUrl":"http://127.0.0.1:19382/v1","protocol":"openai-completions"});
        let (_owner, cancellation) = watch::channel(false);
        let first = pool
            .acquire(&connection, "model-a", false, cancellation.clone())
            .await
            .unwrap();
        let other_pool = pool.clone();
        let other_connection = connection.clone();
        let (cancel, stopped) = watch::channel(false);
        let blocked = tokio::spawn(async move {
            other_pool
                .acquire(&other_connection, "model-b", true, stopped)
                .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!blocked.is_finished());
        cancel.send_replace(true);
        assert_eq!(blocked.await.unwrap().err().unwrap().code, "ABORT_ERR");
        drop(first);
        let next = tokio::time::timeout(
            Duration::from_millis(100),
            pool.acquire(&connection, "model-b", true, cancellation),
        )
        .await
        .unwrap()
        .unwrap();
        drop(next);
        assert!(pool.state.lock().unwrap().local_active.is_empty());
    }
}
