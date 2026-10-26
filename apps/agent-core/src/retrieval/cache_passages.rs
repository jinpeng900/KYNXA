//! Original chunk bodies are reusable only after a fresh scoped metadata read in the caller's SQLite snapshot.
//! 原文片段仅在当前 SQLite 快照重新核验范围与版本后复用，不替代权限或索引真值。

use super::{cache::{ByteLru, retained_json_bytes}, cache_budget::Feedback};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::{Arc, Mutex, OnceLock}, time::Instant};

pub(crate) const DEFAULT_BYTES: usize = 32 * 1024 * 1024;
struct Entry { body: Value, computed_micros: u64 }
struct State { values: ByteLru<Entry>, computed_micros: u64, saved_micros: u64 }
struct PassageCache(Mutex<State>);
impl PassageCache {
    fn new(bytes: usize) -> Self {
        Self(Mutex::new(State { values: ByteLru::new(bytes), computed_micros: 0, saved_micros: 0 }))
    }
    fn read<E>(&self, identity: (u64, u64), row: &Value, load: impl FnOnce() -> Result<Value, E>) -> Result<Value, E> {
        let key = key(identity, row);
        let generation = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(value) = state.values.get(&key) {
                state.saved_micros = state.saved_micros.saturating_add(value.computed_micros);
                return Ok(value.body.clone());
            }
            state.values.generation()
        };
        let started = Instant::now();
        let body = load()?;
        let computed = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.computed_micros = state.computed_micros.saturating_add(computed);
        // A reclaim or disable always wins over an earlier in-flight SQL read.
        // 回收或禁用必须优先于先前在途的 SQL 读取，禁止旧代重新填回缓存。
        if state.values.generation() == generation {
            let bytes = retained_json_bytes(&body).saturating_add(std::mem::size_of::<Entry>());
            state.values.insert(key, Arc::new(Entry { body: body.clone(), computed_micros: computed }), bytes);
        }
        Ok(body)
    }
    fn feedback(&self) -> Feedback {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let stats = state.values.stats();
        Feedback { hits: stats["hits"].as_u64().unwrap_or(0), misses: stats["misses"].as_u64().unwrap_or(0),
            evictions: stats["evictions"].as_u64().unwrap_or(0), occupied_bytes: state.values.occupied_bytes(),
            computed_micros: state.computed_micros, estimated_saved_micros: state.saved_micros }
    }
}
static CACHE: OnceLock<PassageCache> = OnceLock::new();
pub(crate) fn set_capacity(bytes: usize) {
    CACHE.get_or_init(|| PassageCache::new(0)).0.lock().unwrap_or_else(|error| error.into_inner()).values.set_capacity(bytes);
}
pub(crate) fn reclaim() {
    if let Some(cache) = CACHE.get() {
        let mut state = cache.0.lock().unwrap_or_else(|error| error.into_inner());
        state.values.invalidate("cache-memory-reclaimed");
        state.values.set_capacity(0);
    }
}
pub(crate) fn budget_feedback() -> Feedback { CACHE.get().map_or_else(Feedback::default, PassageCache::feedback) }
pub(crate) fn status() -> Value {
    CACHE.get().map_or_else(|| json!({"state":"not-used"}), |cache| {
        let state = cache.0.lock().unwrap_or_else(|error| error.into_inner());
        let mut stats = state.values.stats();
        stats["measuredComputeMicros"] = json!(state.computed_micros);
        stats["estimatedSavedComputeMicros"] = json!(state.saved_micros);
        stats
    })
}
fn key(identity: (u64, u64), row: &Value) -> String {
    let versions = ["source_id", "scope_key", "content_hash", "source_revision", "binding_revision", "active_generation",
        "derivation_signature", "chunk_id", "chunk_hash", "chunker_version", "tokenizer_version"]
        .into_iter().map(|field| (field.to_owned(), row[field].clone())).collect::<serde_json::Map<_, _>>();
    let descriptor = json!({"version":"scoped-original-passages-v1", "databaseIdentity":identity,"versions":versions});
    format!("{:x}", Sha256::digest(descriptor.to_string().as_bytes()))
}
/// Authorize metadata in the current transaction before lookup; skip missing rows.
/// 查询前在当前事务核验元数据权限，缺失记录不查缓存。
pub(crate) fn read<E>(identity: (u64, u64), row: &Value, load: impl FnOnce() -> Result<Value, E>) -> Result<Value, E> {
    let Some(cache) = CACHE.get() else { return load(); };
    cache.read(identity, row, load)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn original_body_reuses_only_the_current_database_scope_and_source_version() {
        let cache = PassageCache::new(32 * 1024);
        let row = json!({"source_id":"document","scope_key":"project:one","content_hash":"v1",
            "chunk_id":"chunk","chunk_hash":"body-v1","binding_revision":1});
        let calls = Cell::new(0);
        let load = || { calls.set(calls.get() + 1); Ok::<_, ()>(json!({"text":"original","lexical_text":"original"})) };
        cache.read((1, 2), &row, load).unwrap(); cache.read((1, 2), &row, load).unwrap();
        assert_eq!(calls.get(), 1);
        for (field, value) in [("chunk_hash", json!("body-v2")), ("scope_key", json!("project:two")),
            ("binding_revision", json!(2)), ("content_hash", json!("v2"))] {
            let mut changed = row.clone(); changed[field] = value;
            cache.read((1, 2), &changed, load).unwrap();
        }
        cache.read((2, 2), &row, load).unwrap(); assert_eq!(calls.get(), 6);
        cache.0.lock().unwrap().values.set_capacity(0);
        cache.read((1, 2), &row, load).unwrap(); assert_eq!(calls.get(), 7);
        assert_eq!(cache.feedback().occupied_bytes, 0);
    }
}
