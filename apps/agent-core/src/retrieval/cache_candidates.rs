//! Short-lived, metadata-only reuse for retrieval candidate channels.
//! 检索候选通道仅短期复用元数据，原文、融合、权限和向量就绪状态仍逐次核验。

use super::cache::{ByteLru, retained_json_bytes};
use super::cache_budget::Feedback;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_BYTES: usize = 32 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(60);
const KEY_VERSION: &str = "metadata-candidate-channels-v1";
const MAX_FLIGHTS: usize = 64;
const FLIGHT_BYTES: usize = std::mem::size_of::<Flight>()
    + std::mem::size_of::<String>()
    + std::mem::size_of::<Arc<Flight>>()
    + 512;
static CACHE: OnceLock<CandidateCache> = OnceLock::new();

struct Entry {
    created: Instant,
    channels: Arc<Value>,
    computed_micros: u64,
}

#[derive(Default)]
struct FlightState {
    finished: bool,
    channels: Option<Arc<Value>>,
    computed_micros: u64,
}

#[derive(Default)]
struct Flight {
    state: Mutex<FlightState>,
    ready: Condvar,
}

struct State {
    entries: ByteLru<Entry>,
    flights: HashMap<String, Arc<Flight>>,
    budget: usize,
    hits: u64,
    misses: u64,
    shared: u64,
    expired: u64,
    empty: u64,
    bypassed: u64,
    computed_micros: u64,
    estimated_saved_micros: u64,
}

struct CandidateCache {
    state: Mutex<State>,
    ttl: Duration,
}

impl CandidateCache {
    fn new(bytes: usize, ttl: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                entries: ByteLru::new(bytes),
                flights: HashMap::new(),
                budget: bytes,
                hits: 0,
                misses: 0,
                shared: 0,
                expired: 0,
                empty: 0,
                bypassed: 0,
                computed_micros: 0,
                estimated_saved_micros: 0,
            }),
            ttl,
        }
    }

    fn receipt(&self, status: &str) -> Value {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        json!({
            "status":status,"kind":"metadata-candidate-channels","ttlSeconds":self.ttl.as_secs(),
            "hits":state.hits,"misses":state.misses,"shared":state.shared,
            "expired":state.expired,"emptyNotCached":state.empty,"capacityBypasses":state.bypassed,
            "inFlight":state.flights.len(),"inFlightBytes":state.flights.len()*FLIGHT_BYTES,
            "residentBytes":state.entries.occupied_bytes()+state.flights.len()*FLIGHT_BYTES,
            "budgetBytes":state.budget,"memory":state.entries.stats()
            ,"measuredComputeMicros":state.computed_micros,"estimatedSavedComputeMicros":state.estimated_saved_micros
        })
    }

    fn reuse<E>(
        &self,
        key: String,
        enabled: bool,
        load: impl FnOnce() -> Result<Value, E>,
        cancelled: impl Fn() -> Result<(), E>,
    ) -> Result<(Value, Value), E> {
        cancelled()?;
        if !enabled {
            let started=Instant::now();
            return load().map(|channels| {
                let mut state=self.state.lock().unwrap_or_else(|error|error.into_inner());
                state.misses=state.misses.saturating_add(1);
                state.computed_micros=state.computed_micros.saturating_add(started.elapsed().as_micros().min(u64::MAX as u128) as u64);
                drop(state); (channels,self.receipt("bypass"))
            });
        }
        let mut load = Some(load);
        loop {
            cancelled()?;
            let (flight, leader, generation) = {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                if let Some(entry) = state.entries.get(&key) {
                    if entry.created.elapsed() < self.ttl {
                        state.hits += 1;
                        state.estimated_saved_micros=state.estimated_saved_micros.saturating_add(entry.computed_micros);
                        let channels = entry.channels.as_ref().clone();
                        drop(state);
                        return Ok((channels, self.receipt("hit")));
                    }
                    state.entries.remove(&key);
                    state.expired += 1;
                }
                state.misses += 1;
                let generation = state.entries.generation();
                if let Some(flight) = state.flights.get(&key) {
                    (flight.clone(), false, generation)
                } else {
                    // Cache bookkeeping shares the byte budget; overload keeps the original query path.
                    // 在途登记与缓存值共享字节预算；容量不足时执行原查询，不拒绝或中断任务。
                    if state.flights.len() >= MAX_FLIGHTS
                        || state
                            .budget
                            .saturating_sub(state.flights.len() * FLIGHT_BYTES)
                            < FLIGHT_BYTES
                    {
                        state.bypassed += 1;
                        drop(state);
                        return load.take().expect("candidate loader is consumed once")()
                            .map(|channels| (channels, self.receipt("bypass-capacity")));
                    }
                    let flight = Arc::new(Flight::default());
                    state.flights.insert(key.clone(), flight.clone());
                    let bytes = state
                        .budget
                        .saturating_sub(state.flights.len() * FLIGHT_BYTES);
                    state.entries.set_capacity(bytes);
                    (flight, true, generation)
                }
            };
            if !leader {
                let mut result = flight
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                while !result.finished {
                    drop(result);
                    cancelled()?;
                    result = flight
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if !result.finished {
                        result = flight
                            .ready
                            .wait_timeout(result, Duration::from_millis(20))
                            .unwrap_or_else(|error| error.into_inner())
                            .0;
                    }
                }
                if let Some(channels) = result.channels.clone() {
                    let computed_micros=result.computed_micros;
                    drop(result);
                    cancelled()?;
                    let mut state=self.state.lock().unwrap_or_else(|error|error.into_inner());
                    state.shared+=1;
                    state.estimated_saved_micros=state.estimated_saved_micros.saturating_add(computed_micros);
                    drop(state);
                    return Ok((channels.as_ref().clone(), self.receipt("shared")));
                }
                // Failed or cancelled owners do not poison a waiting request.
                // 计算方失败或取消时，等待方可接替计算，不缓存失败结果。
                continue;
            }
            let guard = FlightGuard {
                cache: self,
                key: &key,
                flight: flight.clone(),
            };
            let started=Instant::now();
            let channels = Arc::new(load.take().expect("candidate loader is consumed once")()?);
            let computed_micros=started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            cancelled()?;
            {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                state.computed_micros=state.computed_micros.saturating_add(computed_micros);
                if nonempty(&channels) {
                    // Pressure reclamation must win over an older in-flight insertion.
                    // 内存压力回收后，不允许先前在途计算重新填回旧代缓存。
                    if state.entries.generation() == generation {
                        let bytes = retained_json_bytes(&channels)
                            .saturating_add(std::mem::size_of::<Entry>());
                        state.entries.insert(
                            key.clone(),
                            Arc::new(Entry {
                                created: Instant::now(),
                                channels: channels.clone(),
                                computed_micros,
                            }),
                            bytes,
                        );
                    }
                } else {
                    state.empty += 1;
                }
            }
            { let mut result=flight.state.lock().unwrap_or_else(|error|error.into_inner());
                result.channels=Some(channels.clone()); result.computed_micros=computed_micros; }
            drop(guard);
            return Ok((channels.as_ref().clone(), self.receipt("miss")));
        }
    }
}

struct FlightGuard<'a> {
    cache: &'a CandidateCache,
    key: &'a str,
    flight: Arc<Flight>,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        {
            let mut state = self
                .cache
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state
                .flights
                .get(self.key)
                .is_some_and(|flight| Arc::ptr_eq(flight, &self.flight))
            {
                state.flights.remove(self.key);
                if state.flights.is_empty() {
                    state.flights.shrink_to_fit();
                }
                let bytes = state
                    .budget
                    .saturating_sub(state.flights.len() * FLIGHT_BYTES);
                state.entries.set_capacity(bytes);
            }
        }
        let mut result = self
            .flight
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        result.finished = true;
        self.flight.ready.notify_all();
    }
}

fn nonempty(channels: &Value) -> bool {
    channels.as_object().is_some_and(|object| {
        object
            .values()
            .any(|rows| rows.as_array().is_some_and(|rows| !rows.is_empty()))
    })
}

fn cache() -> &'static CandidateCache {
    CACHE.get_or_init(|| CandidateCache::new(0, TTL))
}
pub(crate) fn status() -> Value {CACHE.get().map_or_else(||json!({"state":"not-used"}),|cache|cache.receipt("snapshot"))}

pub(crate) fn reclaim() {
    let mut state = cache()
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state.budget = 0;
    state.entries.invalidate("memory-pressure");
    state.entries.set_capacity(0);
}

/// The retrieval I/O boundary supplies an existing hardware grant; this module stays in memory.
/// 检索 I/O 边界提供已有硬件授额；本模块只维护内存状态。
pub(crate) fn set_capacity(bytes: usize) {
    let mut state = cache()
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state.budget = bytes;
    let retained = bytes.saturating_sub(state.flights.len() * FLIGHT_BYTES);
    state.entries.set_capacity(retained);
}

pub(crate) fn admitted() -> bool {
    cache()
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .budget
        > 0
}
pub(crate) fn budget_feedback() -> Feedback {
    let Some(cache)=CACHE.get() else {return Feedback::default();};
    let state=cache.state.lock().unwrap_or_else(|error|error.into_inner());
    Feedback {hits:state.hits.saturating_add(state.shared),misses:state.misses,evictions:state.entries.stats()["evictions"].as_u64().unwrap_or(0),
        occupied_bytes:state.entries.occupied_bytes(),computed_micros:state.computed_micros,estimated_saved_micros:state.estimated_saved_micros}
}

fn key(arguments: &Value, snapshot: &Value, database_identity: (u64, u64)) -> String {
    let mut effective = arguments.clone();
    if let Some(object) = effective.as_object_mut() {
        for transient in [
            "requestId",
            "queryId",
            "timeout",
            "timeoutMs",
            "timeoutMillis",
        ] {
            object.remove(transient);
        }
    }
    let key = json!({"version":KEY_VERSION,"arguments":effective,
        "databaseIdentity":[database_identity.0,database_identity.1],
        "scopeSnapshots":snapshot["scopeSnapshots"]});
    let encoded = serde_json::to_vec(&key).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded))
}

pub(crate) fn reuse<E>(
    arguments: &Value,
    snapshot: &Value,
    database_identity: (u64, u64),
    admitted: bool,
    load: impl FnOnce() -> Result<Value, E>,
    cancelled: impl Fn() -> Result<(), E>,
) -> Result<(Value, Value), E> {
    let key = key(arguments, snapshot, database_identity);
    let ready = arguments["metadataReady"] != false
        && arguments["partial"] != true
        && snapshot["scopeSnapshots"].is_array();
    cache().reuse(key, admitted && ready, load, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn rows() -> Value {
        json!({"lexical":[{"chunk_id":"one","actual_domain":"document","rank":-1.0}],"vector":[]})
    }

    #[test]
    fn reuses_positive_metadata_and_does_not_cache_empty_results() {
        let cache = CandidateCache::new(16384, TTL);
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok::<_, ()>(rows())
        };
        assert_eq!(
            cache
                .reuse("scope-version-a".into(), true, load, || Ok(()))
                .unwrap()
                .1["status"],
            "miss"
        );
        assert_eq!(
            cache
                .reuse("scope-version-a".into(), true, load, || Ok(()))
                .unwrap()
                .1["status"],
            "hit"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        for _ in 0..2 {
            assert_eq!(
                cache
                    .reuse(
                        "empty".into(),
                        true,
                        || Ok::<_, ()>(json!({"lexical":[]})),
                        || Ok(())
                    )
                    .unwrap()
                    .1["status"],
                "miss"
            );
        }
        assert_eq!(cache.receipt("stats")["emptyNotCached"], 2);
    }

    #[test]
    fn expiry_and_reclamation_do_not_change_query_results() {
        let cache = CandidateCache::new(16384, Duration::ZERO);
        let load = || Ok::<_, ()>(rows());
        cache.reuse("key".into(), true, load, || Ok(())).unwrap();
        assert_eq!(
            cache.reuse("key".into(), true, load, || Ok(())).unwrap().1["status"],
            "miss"
        );
        cache
            .state
            .lock()
            .unwrap()
            .entries
            .invalidate("test-pressure");
        assert_eq!(
            cache.reuse("key".into(), true, load, || Ok(())).unwrap().0,
            rows()
        );
    }

    #[test]
    fn failed_owner_is_not_cached_and_does_not_leave_waiters_blocked() {
        let cache = CandidateCache::new(16384, TTL);
        let result = cache.reuse(
            "key".into(),
            true,
            || Err::<Value, _>("unavailable"),
            || Ok(()),
        );
        assert_eq!(result.unwrap_err(), "unavailable");
        assert!(cache.state.lock().unwrap().flights.is_empty());
        assert_eq!(
            cache
                .reuse("key".into(), true, || Ok::<_, &str>(rows()), || Ok(()))
                .unwrap()
                .0,
            rows()
        );
    }

    #[test]
    fn concurrent_same_key_reuses_one_computation() {
        let cache = CandidateCache::new(16384, TTL);
        let calls = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                cache
                    .reuse(
                        "key".into(),
                        true,
                        || {
                            calls.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(40));
                            Ok::<_, ()>(rows())
                        },
                        || Ok(()),
                    )
                    .unwrap()
                    .0
            });
            let second = scope.spawn(|| {
                cache
                    .reuse(
                        "key".into(),
                        true,
                        || {
                            calls.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(40));
                            Ok::<_, ()>(rows())
                        },
                        || Ok(()),
                    )
                    .unwrap()
                    .0
            });
            assert_eq!(first.join().unwrap(), second.join().unwrap());
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cancelled_waiter_keeps_the_owner_and_other_consumers_running() {
        use std::sync::atomic::AtomicBool;
        let cache = CandidateCache::new(16384, TTL);
        let cancel = AtomicBool::new(false);
        let checks = AtomicUsize::new(0);
        let (loaded, loading) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (waiting, waited) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let owner_cache = &cache;
            let owner = scope.spawn(move || {
                owner_cache.reuse(
                    "key".into(),
                    true,
                    || {
                        loaded.send(()).unwrap();
                        released.recv().unwrap();
                        Ok::<_, &str>(rows())
                    },
                    || Ok(()),
                )
            });
            loading.recv_timeout(Duration::from_secs(2)).unwrap();
            let waiter = scope.spawn(|| {
                cache.reuse(
                    "key".into(),
                    true,
                    || panic!("a waiting request must not duplicate the active computation"),
                    || {
                        if checks.fetch_add(1, Ordering::Relaxed) == 2 {
                            waiting.send(()).unwrap();
                        }
                        if cancel.load(Ordering::Acquire) {
                            Err("cancelled")
                        } else {
                            Ok(())
                        }
                    },
                )
            });
            waited.recv_timeout(Duration::from_secs(2)).unwrap();
            cancel.store(true, Ordering::Release);
            assert_eq!(waiter.join().unwrap().unwrap_err(), "cancelled");
            release.send(()).unwrap();
            assert_eq!(owner.join().unwrap().unwrap().0, rows());
        });
        assert_eq!(
            cache
                .reuse("key".into(), true, || Ok::<_, &str>(rows()), || Ok(()))
                .unwrap()
                .1["status"],
            "hit"
        );
    }

    #[test]
    fn incomplete_metadata_is_not_reused_as_a_complete_candidate_result() {
        let snapshot = json!({"scopeSnapshots":[]});
        for arguments in [json!({"metadataReady":false}), json!({"partial":true})] {
            let receipt = reuse(
                &arguments,
                &snapshot,
                (1, 2),
                true,
                || Ok::<_, ()>(rows()),
                || Ok(()),
            )
            .unwrap()
            .1;
            assert_eq!(receipt["status"], "bypass");
        }
    }

    #[test]
    fn flight_saturation_bypasses_cache_without_rejecting_the_lookup() {
        let cache = CandidateCache::new(FLIGHT_BYTES - 1, TTL);
        let (value, receipt) = cache
            .reuse("key".into(), true, || Ok::<_, ()>(rows()), || Ok(()))
            .unwrap();
        assert_eq!(value, rows());
        assert_eq!(receipt["status"], "bypass-capacity");
        assert_eq!(receipt["inFlight"], 0);
        assert!(
            receipt["residentBytes"].as_u64().unwrap() <= receipt["budgetBytes"].as_u64().unwrap()
        );
    }

    #[test]
    fn hit_then_index_publication_and_authorization_change_require_new_candidates() {
        let cache = CandidateCache::new(16384, TTL);
        let mut args = json!({"root":"fixture","path":"fixture/index.sqlite",
            "scopes":["project:one","memory:global"],"query":"alpha","candidateLimit":40});
        let mut snapshot = json!({"scopeSnapshots":[{"scopeKey":"project:one","generation":1,"snapshotId":"epoch:1"},
            {"scopeKey":"memory:global","generation":3,"snapshotId":"epoch:3"}]});
        let first_key = key(&args, &snapshot, (11, 22));
        cache
            .reuse(first_key.clone(), true, || Ok::<_, ()>(rows()), || Ok(()))
            .unwrap();
        assert_eq!(
            cache
                .reuse(
                    first_key,
                    true,
                    || panic!("expected a cache hit"),
                    || Ok::<_, ()>(())
                )
                .unwrap()
                .1["status"],
            "hit"
        );

        // A newly published source or withdrawal is reflected even if it was never in prior hits.
        // 即使新发布或撤销的来源从未出现在旧命中中，整范围版本也会使旧候选失效。
        snapshot["scopeSnapshots"][0]["generation"] = json!(2);
        snapshot["scopeSnapshots"][0]["snapshotId"] = json!("epoch:2");
        let published = json!({"lexical":[{"chunk_id":"newly-published"}]});
        let (current, receipt) = cache
            .reuse(
                key(&args, &snapshot, (11, 22)),
                true,
                || Ok::<_, ()>(published.clone()),
                || Ok(()),
            )
            .unwrap();
        assert_eq!(receipt["status"], "miss");
        assert_eq!(current, published);

        args["scopes"] = json!(["project:other"]);
        snapshot["scopeSnapshots"] =
            json!([{"scopeKey":"project:other","generation":1,"snapshotId":"epoch:1"}]);
        let permitted = json!({"lexical":[{"chunk_id":"other-authorized"}]});
        let (current, receipt) = cache
            .reuse(
                key(&args, &snapshot, (11, 22)),
                true,
                || Ok::<_, ()>(permitted.clone()),
                || Ok(()),
            )
            .unwrap();
        assert_eq!(receipt["status"], "miss");
        assert_eq!(current, permitted);
        assert!(!current.to_string().contains("newly-published"));
    }

    #[test]
    fn cache_key_tracks_full_scope_version_filters_and_database_identity() {
        let args = json!({"root":"fixture","path":"fixture/index.sqlite","requestId":"first",
            "scopes":["project:one","memory:global"],"query":"alpha","candidateLimit":40,
            "model":"model-a","space":"space-a","domain":"document"});
        let snapshot = json!({"scopeSnapshots":[{"scopeKey":"project:one","generation":1,"snapshotId":"epoch:1"},
            {"scopeKey":"memory:global","generation":3,"snapshotId":"epoch:3"}]});
        let original = key(&args, &snapshot, (11, 22));
        let mut retry = args.clone();
        retry["requestId"] = json!("second");
        assert_eq!(original, key(&retry, &snapshot, (11, 22)));
        for (field, value) in [
            ("query", json!("beta")),
            ("candidateLimit", json!(80)),
            ("domain", json!("code")),
            ("space", json!("space-b")),
            ("scopes", json!(["project:other", "memory:global"])),
        ] {
            let mut changed = args.clone();
            changed[field] = value;
            assert_ne!(original, key(&changed, &snapshot, (11, 22)), "{field}");
        }
        // New source publication, removal and memory withdrawal advance the whole scope snapshot.
        // 新资料发布、删除和记忆撤销都会改变整个范围版本，不依赖此前命中哪些来源。
        let mut advanced = snapshot.clone();
        advanced["scopeSnapshots"][1]["generation"] = json!(4);
        advanced["scopeSnapshots"][1]["snapshotId"] = json!("epoch:4");
        assert_ne!(original, key(&args, &advanced, (11, 22)));
        assert_ne!(original, key(&args, &snapshot, (11, 23)));
        assert!(!original.contains("alpha"));
    }
}
