//! Small process-wide cache primitives store only rebuildable values and bounded accounting.
//! 小型进程共享缓存仅保存可重建的值和有界计量，不承担正式数据或权限真值。

use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

/// Count retained JSON containers and string capacities, rather than only their serialized text.
/// 计入 JSON 容器、树节点和字符串容量，避免只按序列化长度低估实际常驻内存。
pub(crate) fn retained_json_bytes(value: &Value) -> usize {
    let own = std::mem::size_of::<Value>();
    own.saturating_add(match value {
        Value::String(text) => text.capacity(),
        Value::Array(items) => items.capacity().saturating_mul(own).saturating_add(
            items
                .iter()
                .map(|item| retained_json_bytes(item).saturating_sub(own))
                .sum::<usize>(),
        ),
        Value::Object(entries) => entries
            .iter()
            .map(|(key, value)| {
                key.capacity()
                    .saturating_add(std::mem::size_of::<String>() + own + 64)
                    .saturating_add(retained_json_bytes(value).saturating_sub(own))
            })
            .sum(),
        _ => 0,
    })
}

struct Entry<V> {
    value: Arc<V>,
    bytes: usize,
    touched: u64,
}

/// The caller owns synchronization and supplies the complete retained value size, including owned strings.
/// 调用方负责同步，并提供保留值的完整字节数，含其拥有的字符串；键与节点开销在此统一计入。
pub(crate) struct ByteLru<V> {
    entries: HashMap<String, Entry<V>>,
    capacity: usize,
    bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    invalidations: u64,
    invalidation_reason: &'static str,
    generation: u64,
}

impl<V> ByteLru<V> {
    pub(crate) fn new(capacity_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity_bytes,
            bytes: 0,
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            invalidations: 0,
            invalidation_reason: "none",
            generation: 0,
        }
    }

    pub(crate) fn get(&mut self, key: &str) -> Option<Arc<V>> {
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(key) {
            entry.touched = self.clock;
            self.hits = self.hits.saturating_add(1);
            Some(entry.value.clone())
        } else {
            self.misses = self.misses.saturating_add(1);
            None
        }
    }

    pub(crate) fn insert(&mut self, key: String, value: Arc<V>, value_bytes: usize) -> bool {
        let bytes = value_bytes
            .saturating_add(key.capacity())
            .saturating_add(std::mem::size_of::<Entry<V>>())
            .saturating_add(std::mem::size_of::<String>())
            .saturating_add(64);
        if bytes > self.capacity {
            return false;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        while self.bytes.saturating_add(bytes) > self.capacity {
            self.evict_one();
        }
        self.clock = self.clock.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                touched: self.clock,
            },
        );
        true
    }

    fn evict_one(&mut self) {
        let key = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.touched)
            .map(|(key, _)| key.clone());
        if let Some(key) = key {
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
                self.evictions = self.evictions.saturating_add(1);
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<Arc<V>> {
        let entry = self.entries.remove(key)?;
        self.bytes = self.bytes.saturating_sub(entry.bytes);
        self.invalidations = self.invalidations.saturating_add(1);
        if self.entries.is_empty() {
            self.entries.shrink_to_fit();
        }
        Some(entry.value)
    }

    pub(crate) fn set_capacity(&mut self, capacity_bytes: usize) {
        self.capacity = capacity_bytes;
        while self.bytes > self.capacity {
            self.evict_one();
        }
        if capacity_bytes == 0 || self.entries.capacity() > self.entries.len().saturating_mul(4) {
            self.entries.shrink_to_fit();
        }
    }

    pub(crate) fn invalidate(&mut self, reason: &'static str) {
        self.invalidations = self.invalidations.saturating_add(self.entries.len() as u64);
        self.entries = HashMap::new();
        self.bytes = 0;
        self.generation = self.generation.saturating_add(1);
        self.invalidation_reason = reason;
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn occupied_bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn stats(&self) -> Value {
        json!({"entries":self.entries.len(),"bytes":self.bytes,"capacityBytes":self.capacity,
            "hits":self.hits,"misses":self.misses,"evictions":self.evictions,
            "invalidations":self.invalidations,"lastInvalidationReason":self.invalidation_reason})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_lru_counts_owned_values_and_keys_and_evicts_the_least_recently_read() {
        let mut cache = ByteLru::new(600);
        assert!(cache.insert("a".into(), Arc::new(1), 150));
        assert!(cache.insert("b".into(), Arc::new(2), 150));
        assert_eq!(*cache.get("a").unwrap(), 1);
        assert!(cache.insert("c".into(), Arc::new(3), 150));
        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());
        assert!(cache.occupied_bytes() <= 600);
        assert!(!cache.insert("oversized".into(), Arc::new(4), 601));
        cache.set_capacity(0);
        assert_eq!(cache.occupied_bytes(), 0);
    }

    #[test]
    fn invalidation_changes_generation_and_never_keeps_an_old_value() {
        let mut cache = ByteLru::new(1024);
        cache.insert("private-hash".into(), Arc::new(7), 12);
        let generation = cache.generation();
        cache.invalidate("resource-pressure");
        assert!(cache.generation() > generation);
        assert!(cache.get("private-hash").is_none());
        assert_eq!(cache.stats()["lastInvalidationReason"], "resource-pressure");
    }
}
