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

const BACKGROUND_AGING: Duration = Duration::from_secs(5);
const FOREGROUND_BURST: usize = 4;

// Address locality is not generation capacity: a loopback endpoint can be a cloud relay.
// 本机地址不代表生成容量；回环地址也可能只是云端 API 转发。
fn endpoint_slots(connection: &Value, observed: Option<&Value>) -> (Option<String>, usize) {
    let endpoint = connection["baseUrl"].as_str().unwrap_or("");
    let parsed = url::Url::parse(endpoint).ok();
    let key = || parsed.as_ref().map_or_else(|| endpoint.trim_end_matches('/').to_owned(),
        |url| url.as_str().trim_end_matches('/').to_owned());
    if crate::models::connections::is_managed_local(connection) {
        return (Some(key()), 1);
    }
    if let (Some(url), Some(snapshot)) = (parsed, observed) {
        if snapshot["backend"] == "llama.cpp"
            && snapshot["endpointOrigin"].as_str() == Some(url.origin().ascii_serialization().as_str())
        {
            if let Some(slots) = snapshot["parallelSlots"].as_f64()
                .filter(|n| n.is_finite() && n.fract()==0.0 && (1.0..=256.0).contains(n)) {
                return (Some(url.as_str().trim_end_matches('/').to_owned()), slots as usize);
            }
        }
    }
    // Unknown external capacity remains under API admission and the server's own scheduler.
    // 外部容量未知时沿用 API 准入及服务自身调度，不猜测端口或强制串行。
    (None, 0)
}

struct BackgroundWait {
    local: Option<String>,
    primary: bool,
    since: Instant,
}

struct State {
    api_active: usize,
    api_child_active: usize,
    api_limit: usize,
    successful: usize,
    baseline_ms: f64,
    local_active: HashMap<String, usize>,
    local_limits: HashMap<String, usize>,
    api_foreground_waiters: usize,
    api_primary_foreground_waiters: usize,
    local_foreground_waiters: HashMap<String, usize>,
    background_waiters: HashMap<u64, BackgroundWait>,
    next_waiter_id: u64,
    api_foreground_streak: usize,
    local_foreground_streak: HashMap<String, usize>,
    api_last_background: bool,
    local_last_background: HashMap<String, bool>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            api_active: 0,
            api_child_active: 0,
            api_limit: 6,
            successful: 0,
            baseline_ms: 0.0,
            local_active: HashMap::new(),
            local_limits: HashMap::new(),
            api_foreground_waiters: 0,
            api_primary_foreground_waiters: 0,
            local_foreground_waiters: HashMap::new(),
            background_waiters: HashMap::new(),
            next_waiter_id: 0,
            api_foreground_streak: 0,
            local_foreground_streak: HashMap::new(),
            api_last_background: false,
            local_last_background: HashMap::new(),
        }
    }
}
impl State {
    fn clear_idle_local(&mut self, key: &str) {
        if !self.local_active.contains_key(key)
            && !self.local_foreground_waiters.contains_key(key)
            && !self.background_waiters.values().any(|waiter|waiter.local.as_deref()==Some(key)) {
            self.local_limits.remove(key);
            self.local_foreground_streak.remove(key);
            self.local_last_background.remove(key);
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
struct ForegroundWaiter {
    pool: Arc<ModelSlots>,
    local: Option<String>,
    background_id: Option<u64>,
    primary: bool,
}
impl Drop for ForegroundWaiter {
    fn drop(&mut self) {
        if let Ok(mut state) = self.pool.state.lock() {
            if let Some(id)=self.background_id {
                state.background_waiters.remove(&id);
                if !state.background_waiters.values().any(|waiter|waiter.local==self.local) {
                    if let Some(key)=&self.local {state.local_foreground_streak.remove(key);} else {state.api_foreground_streak=0;}
                }
            } else if let Some(key) = &self.local {
                if let Some(count) = state.local_foreground_waiters.get_mut(key) { *count = count.saturating_sub(1); }
                state.local_foreground_waiters.retain(|_, count| *count > 0);
            } else {
                state.api_foreground_waiters = state.api_foreground_waiters.saturating_sub(1);
                if self.primary {state.api_primary_foreground_waiters=state.api_primary_foreground_waiters.saturating_sub(1);}
            }
            if let Some(key) = &self.local { state.clear_idle_local(key); }
        }
        self.pool.changed.notify_waiters();
    }
}
impl ModelSlots {
    pub async fn acquire(
        self: &Arc<Self>,
        connection: &Value,
        _model: &str,
        primary: bool,
        cancellation: watch::Receiver<bool>,
    ) -> Result<ModelSlot, ModelError> {
        self.acquire_with_priority(connection, _model, primary, true, cancellation).await
    }

    /// Foreground requests go ahead of queued recovery/preparation work; in-flight requests keep their receipts.
    /// 前台请求优先于排队中的恢复和准备；已经派发的请求保留执行与回执，不通过抢占中断它们。
    pub async fn acquire_with_priority(
        self: &Arc<Self>, connection: &Value, _model: &str, primary: bool,
        foreground: bool, cancellation: watch::Receiver<bool>,
    ) -> Result<ModelSlot, ModelError> {
        self.acquire_with_observation(connection, None, primary, foreground, cancellation).await
    }

    pub async fn acquire_with_observation(
        self: &Arc<Self>, connection: &Value, observed: Option<&Value>, primary: bool,
        foreground: bool, mut cancellation: watch::Receiver<bool>,
    ) -> Result<ModelSlot, ModelError> {
        let (key, limit) = endpoint_slots(connection, observed);
        let background_id = {
            let mut state = self.state.lock().map_err(|_| ModelError::new("MODEL_POOL_UNAVAILABLE", "模型调度不可用。", 503))?;
            if let Some(key) = &key { state.local_limits.insert(key.clone(), limit); }
            if foreground {
                if let Some(key) = &key { *state.local_foreground_waiters.entry(key.clone()).or_default() += 1; }
                else { state.api_foreground_waiters += 1;if primary {state.api_primary_foreground_waiters+=1;} }
                None
            } else {
                let mut id=state.next_waiter_id.wrapping_add(1);
                while state.background_waiters.contains_key(&id) {id=id.wrapping_add(1);}
                state.next_waiter_id=id;
                state.background_waiters.insert(id,BackgroundWait{local:key.clone(),primary,since:Instant::now()});
                Some(id)
            }
        };
        let _waiter=ForegroundWaiter {pool:self.clone(),local:key.clone(),background_id,primary};
        self.changed.notify_waiters();
        loop {
            // Subscribe before checking the count, avoiding a lost slot-release wakeup.
            // 先注册唤醒再检查计数，避免释放槽位后的漏唤醒。
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if *cancellation.borrow() {
                return Err(ModelError::cancelled());
            }
            let fairness_wakeup={
                let mut state = self.state.lock().map_err(|_| {
                    ModelError::new("MODEL_POOL_UNAVAILABLE", "模型调度不可用。", 503)
                })?;
                let oldest=state.background_waiters.iter().filter(|(_,waiter)|waiter.local==key
                    && (key.is_some() || waiter.primary || state.api_child_active<state.api_limit.saturating_sub(1)))
                    .min_by_key(|(id,waiter)|(waiter.since,**id)).map(|(id,waiter)|(*id,waiter.since));
                let foreground_waiters=key.as_ref().map_or(state.api_foreground_waiters,|key|state.local_foreground_waiters.get(key).copied().unwrap_or(0));
                let foreground_eligible=foreground_waiters>0 && (key.is_some() || state.api_primary_foreground_waiters>0 || state.api_child_active<state.api_limit.saturating_sub(1));
                let streak=key.as_ref().map_or(state.api_foreground_streak,|key|state.local_foreground_streak.get(key).copied().unwrap_or(0));
                let last_background=key.as_ref().map_or(state.api_last_background,|key|state.local_last_background.get(key).copied().unwrap_or(false));
                let background_turn=oldest.is_some_and(|(_,since)|since.elapsed()>=BACKGROUND_AGING || streak>=FOREGROUND_BURST)
                    && !(last_background && foreground_eligible);
                // Aged background work gets one symmetric admission turn; no in-flight request is preempted.
                // 后台老化后取得一次对称准入机会；前台此时也让出一次，绝不抢占在途请求。
                let priority_allowed=if foreground {!background_turn} else {
                    oldest.is_some_and(|(id,_)|Some(id)==background_id) && (!foreground_eligible || background_turn)
                };
                let available = priority_allowed && key.as_ref().map_or(
                    state.api_active < state.api_limit
                        && (primary || state.api_child_active < state.api_limit.saturating_sub(1)),
                    |key| state.local_active.get(key).copied().unwrap_or(0) < state.local_limits.get(key).copied().unwrap_or(1),
                );
                if available {
                    let next_streak=if foreground && oldest.is_some() {streak.saturating_add(1)} else {0};
                    if let Some(key) = &key {
                        state.local_foreground_streak.insert(key.clone(),next_streak);
                        state.local_last_background.insert(key.clone(),!foreground);
                        *state.local_active.entry(key.clone()).or_default() += 1;
                    } else {
                        state.api_foreground_streak=next_streak;
                        state.api_last_background=!foreground;
                        state.api_active += 1;
                        if !primary {
                            state.api_child_active += 1;
                        }
                    }
                    if let Some(id)=background_id {state.background_waiters.remove(&id);}
                    return Ok(ModelSlot {
                        pool: self.clone(),
                        local: key,
                        primary,
                        started: Instant::now(),
                    });
                }
                oldest.and_then(|(_,since)|since.checked_add(BACKGROUND_AGING)).filter(|at|*at>Instant::now())
            };
            tokio::select! {
                _=changed=>{},
                _=async {if let Some(at)=fairness_wakeup {tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;} else {std::future::pending::<()>().await;}}=>{},
                _=async {if cancellation.changed().await.is_err() {std::future::pending::<()>().await;}}=>{},
            }
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
                state.api_limit = (state.api_limit + 1).min(32);
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
                state.clear_idle_local(key);
            } else {
                state.api_active = state.api_active.saturating_sub(1);
                if !self.primary {
                    state.api_child_active = state.api_child_active.saturating_sub(1);
                }
                if state.api_active==0 && state.api_foreground_waiters==0 && !state.background_waiters.values().any(|waiter|waiter.local.is_none()) {
                    state.api_foreground_streak=0;state.api_last_background=false;
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

    #[test]
    fn capacity_comes_from_owned_runtime_or_matching_server_observation() {
        let relay=json!({"baseUrl":"http://localhost:8080/v1","parallelSlots":64});
        assert_eq!(endpoint_slots(&relay,None),(None,0));
        let report=json!({"backend":"llama.cpp","endpointOrigin":"http://localhost:8080","parallelSlots":4.0});
        assert_eq!(endpoint_slots(&relay,Some(&report)),(Some("http://localhost:8080/v1".into()),4));
        let owned=json!({"providerId":"kynxa-local-fixture","baseUrl":"http://localhost:8080/v1/"});
        assert_eq!(endpoint_slots(&owned,Some(&report)),(Some("http://localhost:8080/v1".into()),1));
        for invalid in [json!({"backend":"llama.cpp","endpointOrigin":"http://other.invalid","parallelSlots":4}),
            json!({"backend":"llama.cpp","endpointOrigin":"http://localhost:8080","parallelSlots":0}),
            json!({"backend":"llama.cpp","endpointOrigin":"http://localhost:8080","parallelSlots":4.5})] {
            assert_eq!(endpoint_slots(&relay,Some(&invalid)),(None,0));
        }
    }

    #[tokio::test]
    async fn unknown_external_services_and_loopback_relays_are_not_forced_to_one_slot() {
        for (endpoint,protocol) in [("http://127.0.0.1:8080/v1","openai-completions"),
            ("http://localhost:1234/v1","openai-completions"),
            ("http://[::1]:8080/v1","openai-completions"),
            ("http://127.0.0.1:11434","ollama")] {
            let pool=Arc::new(ModelSlots::default());
            let connection=json!({"baseUrl":endpoint,"protocol":protocol});
            let (_cancel,token)=watch::channel(false);
            let first=pool.acquire(&connection,"model-a",true,token.clone()).await.unwrap();
            let second=tokio::time::timeout(Duration::from_millis(100),pool.acquire(&connection,"model-b",true,token)).await.unwrap().unwrap();
            assert_eq!(pool.state.lock().unwrap().api_active,2);
            drop(first);drop(second);
            assert_eq!(pool.state.lock().unwrap().api_active,0);
        }
    }

    #[tokio::test]
    async fn reported_external_slots_share_capacity_across_models_and_normalize_trailing_slashes() {
        let pool=Arc::new(ModelSlots::default());
        let connection=json!({"baseUrl":"http://127.0.0.1:8080/v1"});
        let report=json!({"backend":"llama.cpp","endpointOrigin":"http://127.0.0.1:8080","parallelSlots":4});
        let (_owner,token)=watch::channel(false);
        let mut slots=Vec::new();
        for _ in 0..4 {slots.push(pool.acquire_with_observation(&connection,Some(&report),true,true,token.clone()).await.unwrap());}
        let other=json!({"baseUrl":"http://127.0.0.1:8080/v1/"});
        let (cancel,token)=watch::channel(false);
        let mut fifth=Box::pin(pool.acquire_with_observation(&other,Some(&report),true,true,token));
        assert!(tokio::time::timeout(Duration::from_millis(20),&mut fifth).await.is_err());
        cancel.send_replace(true);
        assert_eq!(fifth.await.err().unwrap().code,"ABORT_ERR");
        drop(slots);
        let state=pool.state.lock().unwrap();
        assert!(state.local_active.is_empty());assert!(state.local_limits.is_empty());
    }

    #[tokio::test]
    async fn lower_reported_capacity_preserves_running_requests_and_waits_for_a_real_vacancy() {
        let pool=Arc::new(ModelSlots::default());
        let connection=json!({"baseUrl":"http://127.0.0.1:8080/v1"});
        let mut report=json!({"backend":"llama.cpp","endpointOrigin":"http://127.0.0.1:8080","parallelSlots":2});
        let (_owner,token)=watch::channel(false);
        let a=pool.acquire_with_observation(&connection,Some(&report),true,true,token.clone()).await.unwrap();
        let b=pool.acquire_with_observation(&connection,Some(&report),true,true,token.clone()).await.unwrap();
        report["parallelSlots"]=json!(1);
        let mut waiting=Box::pin(pool.acquire_with_observation(&connection,Some(&report),true,true,token));
        assert!(tokio::time::timeout(Duration::from_millis(10),&mut waiting).await.is_err());
        drop(a);
        assert!(tokio::time::timeout(Duration::from_millis(10),&mut waiting).await.is_err());
        drop(b);
        drop(tokio::time::timeout(Duration::from_millis(100),waiting).await.unwrap().unwrap());
        assert!(pool.state.lock().unwrap().local_limits.is_empty());
    }

    #[tokio::test]
    async fn loopback_api_actually_dispatches_two_http_requests_without_duplicates() {
        use std::sync::atomic::{AtomicUsize,Ordering};
        let calls=Arc::new(AtomicUsize::new(0));let seen=calls.clone();
        let barrier=Arc::new(tokio::sync::Barrier::new(2));
        let app=axum::Router::new().fallback(move || {let barrier=barrier.clone();let seen=seen.clone();async move {
            seen.fetch_add(1,Ordering::SeqCst);barrier.wait().await;"ok"
        }});
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let pool=Arc::new(ModelSlots::default());let connection=json!({"baseUrl":format!("http://{address}/v1")});
        let (_owner,token)=watch::channel(false);let client=reqwest::Client::builder().no_proxy().build().unwrap();
        let request=||async {
            let _slot=pool.acquire(&connection,"fixture",true,token.clone()).await.unwrap();
            client.get(format!("http://{address}/v1/chat/completions")).send().await.unwrap().text().await.unwrap()
        };
        let result=tokio::time::timeout(Duration::from_secs(2),async {tokio::join!(request(),request())}).await;
        server.abort();let _=server.await;
        assert_eq!(result.unwrap(),("ok".into(),"ok".into()));
        assert_eq!(calls.load(Ordering::SeqCst),2);
        assert_eq!(pool.state.lock().unwrap().api_active,0);
    }

    #[tokio::test]
    async fn child_requests_leave_a_slot_for_primary_and_cancel_without_admission() {
        let pool = Arc::new(ModelSlots::default());
        let connection =
            json!({"baseUrl":"https://fixture.invalid/v1","protocol":"openai-completions"});
        let (owner, cancellation) = watch::channel(false);
        let mut children = Vec::new();
        for _ in 0..5 {
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
            json!({"baseUrl":"http://127.0.0.1:19382/v1","providerId":"kynxa-local-fixture","protocol":"openai-completions"});
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

    #[tokio::test]
    async fn foreground_is_admitted_before_queued_local_recovery_without_interrupting_the_current_request() {
        let pool = Arc::new(ModelSlots::default());
        let connection = json!({"baseUrl":"http://127.0.0.1:19383/v1","providerId":"kynxa-local-fixture","protocol":"openai-completions"});
        let (_owner, cancellation) = watch::channel(false);
        let current = pool.acquire_with_priority(&connection, "fixture", true, false, cancellation.clone()).await.unwrap();
        let background_pool = pool.clone();
        let background_connection = connection.clone();
        let background_token = cancellation.clone();
        let recovery = tokio::spawn(async move { background_pool.acquire_with_priority(&background_connection,"fixture",true,false,background_token).await });
        let foreground_pool = pool.clone();
        let foreground_connection = connection.clone();
        let foreground = tokio::spawn(async move { foreground_pool.acquire_with_priority(&foreground_connection,"fixture",true,true,cancellation).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.state.lock().unwrap().local_foreground_waiters.values().sum::<usize>() == 0 { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert!(!foreground.is_finished() && !recovery.is_finished());
        drop(current);
        let admitted = tokio::time::timeout(Duration::from_secs(1),foreground).await.unwrap().unwrap().unwrap();
        assert!(!recovery.is_finished());
        drop(admitted);
        drop(tokio::time::timeout(Duration::from_secs(1),recovery).await.unwrap().unwrap().unwrap());
        assert!(pool.state.lock().unwrap().local_foreground_waiters.is_empty());
    }

    #[tokio::test]
    async fn aged_background_gets_one_local_turn_even_with_a_queued_foreground() {
        let pool=Arc::new(ModelSlots::default());
        let connection=json!({"baseUrl":"http://127.0.0.1:19384/v1","providerId":"kynxa-local-fixture","protocol":"openai-completions"});
        let (_owner,cancellation)=watch::channel(false);
        let current=pool.acquire(&connection,"fixture",true,cancellation.clone()).await.unwrap();
        let background={let pool=pool.clone();let connection=connection.clone();let token=cancellation.clone();tokio::spawn(async move{pool.acquire_with_priority(&connection,"fixture",true,false,token).await})};
        let foreground={let pool=pool.clone();let connection=connection.clone();tokio::spawn(async move{pool.acquire(&connection,"fixture",true,cancellation).await})};
        tokio::time::timeout(Duration::from_secs(1),async {loop {
            let ready={let state=pool.state.lock().unwrap();state.background_waiters.len()==1 && state.local_foreground_waiters.values().sum::<usize>()==1};
            if ready {break;} tokio::task::yield_now().await;
        }}).await.unwrap();
        pool.state.lock().unwrap().background_waiters.values_mut().next().unwrap().since=Instant::now()-BACKGROUND_AGING-Duration::from_secs(1);
        assert!(!background.is_finished() && !foreground.is_finished());
        drop(current);
        let admitted=tokio::time::timeout(Duration::from_secs(1),background).await.unwrap().unwrap().unwrap();
        assert!(!foreground.is_finished(),"Foreground must yield the aged background turn, not race past it");
        assert_eq!(pool.state.lock().unwrap().local_active.values().sum::<usize>(),1);
        drop(admitted);
        drop(tokio::time::timeout(Duration::from_secs(1),foreground).await.unwrap().unwrap().unwrap());
        assert!(pool.state.lock().unwrap().background_waiters.is_empty());
    }

    #[tokio::test]
    async fn foreground_burst_yields_one_api_turn_without_exceeding_capacity_or_primary_reservation() {
        let pool=Arc::new(ModelSlots::default());pool.state.lock().unwrap().api_limit=2;
        let connection=json!({"baseUrl":"https://fixture.invalid/v1","protocol":"openai-completions"});
        let (_owner,cancellation)=watch::channel(false);
        let current=pool.acquire(&connection,"fixture",true,cancellation.clone()).await.unwrap();
        let second=pool.acquire(&connection,"fixture",true,cancellation.clone()).await.unwrap();
        let background={let pool=pool.clone();let connection=connection.clone();let token=cancellation.clone();tokio::spawn(async move{pool.acquire_with_priority(&connection,"fixture",false,false,token).await})};
        let foreground={let pool=pool.clone();let connection=connection.clone();tokio::spawn(async move{pool.acquire(&connection,"fixture",true,cancellation).await})};
        tokio::time::timeout(Duration::from_secs(1),async {loop {
            let ready={let state=pool.state.lock().unwrap();state.background_waiters.len()==1 && state.api_foreground_waiters==1};
            if ready {break;}tokio::task::yield_now().await;
        }}).await.unwrap();
        pool.state.lock().unwrap().api_foreground_streak=FOREGROUND_BURST;
        drop(current);
        let admitted=tokio::time::timeout(Duration::from_secs(1),background).await.unwrap().unwrap().unwrap();
        assert!(!foreground.is_finished());
        {let state=pool.state.lock().unwrap();assert_eq!(state.api_active,2);assert_eq!(state.api_child_active,1);}
        drop(admitted);
        drop(tokio::time::timeout(Duration::from_secs(1),foreground).await.unwrap().unwrap().unwrap());
        drop(second);assert_eq!(pool.state.lock().unwrap().api_active,0);
    }

    #[tokio::test]
    async fn cancelling_an_aged_background_waiter_does_not_leave_a_phantom_fairness_turn() {
        let pool=Arc::new(ModelSlots::default());let connection=json!({"baseUrl":"http://127.0.0.1:19385/v1","providerId":"kynxa-local-fixture"});
        let (_owner,cancellation)=watch::channel(false);let current=pool.acquire(&connection,"fixture",true,cancellation.clone()).await.unwrap();
        let (cancel,token)=watch::channel(false);
        let background={let pool=pool.clone();let connection=connection.clone();tokio::spawn(async move{pool.acquire_with_priority(&connection,"fixture",true,false,token).await})};
        tokio::time::timeout(Duration::from_secs(1),async {while pool.state.lock().unwrap().background_waiters.is_empty(){tokio::task::yield_now().await;}}).await.unwrap();
        pool.state.lock().unwrap().background_waiters.values_mut().next().unwrap().since=Instant::now()-BACKGROUND_AGING-Duration::from_secs(1);
        cancel.send_replace(true);assert_eq!(background.await.unwrap().err().unwrap().code,"ABORT_ERR");
        drop(current);
        drop(tokio::time::timeout(Duration::from_secs(1),pool.acquire(&connection,"fixture",true,cancellation)).await.unwrap().unwrap());
        assert!(pool.state.lock().unwrap().background_waiters.is_empty());
    }

    #[tokio::test]
    async fn multiple_aged_recoveries_still_yield_back_to_foreground_after_one_turn() {
        let pool=Arc::new(ModelSlots::default());let connection=json!({"baseUrl":"http://127.0.0.1:19386/v1","providerId":"kynxa-local-fixture"});
        let (_owner,cancellation)=watch::channel(false);let current=pool.acquire(&connection,"fixture",true,cancellation.clone()).await.unwrap();
        let mut backgrounds=Vec::new();
        for _ in 0..2 {let pool=pool.clone();let connection=connection.clone();let token=cancellation.clone();backgrounds.push(tokio::spawn(async move{pool.acquire_with_priority(&connection,"fixture",true,false,token).await}));}
        let foreground={let pool=pool.clone();let connection=connection.clone();tokio::spawn(async move{pool.acquire(&connection,"fixture",true,cancellation).await})};
        tokio::time::timeout(Duration::from_secs(1),async {loop {
            let ready={let state=pool.state.lock().unwrap();state.background_waiters.len()==2 && state.local_foreground_waiters.values().sum::<usize>()==1};
            if ready {break;}tokio::task::yield_now().await;
        }}).await.unwrap();
        for waiter in pool.state.lock().unwrap().background_waiters.values_mut() {waiter.since=Instant::now()-BACKGROUND_AGING-Duration::from_secs(1);}
        drop(current);
        let mut first=backgrounds.remove(0);let mut second=backgrounds.remove(0);
        let (admitted,remaining)=tokio::select! {value=&mut first=>(value.unwrap().unwrap(),second),value=&mut second=>(value.unwrap().unwrap(),first)};
        assert!(!foreground.is_finished() && !remaining.is_finished());drop(admitted);
        let foreground_slot=tokio::time::timeout(Duration::from_secs(1),foreground).await.unwrap().unwrap().unwrap();
        assert!(!remaining.is_finished(),"A queue of aged recoveries must not starve foreground work in reverse");
        drop(foreground_slot);
        drop(tokio::time::timeout(Duration::from_secs(1),remaining).await.unwrap().unwrap().unwrap());
    }
}
