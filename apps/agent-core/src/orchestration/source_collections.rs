use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

#[derive(Default)]
struct Collection {
    sequence: u64,
    positions: HashMap<String, u64>,
    rows: BTreeMap<u64, (String, String)>,
}

/// SDK Map/Set compatibility keeps original JSON bytes, while native collections own all entries.
/// SDK Map/Set 兼容层保留原始 JSON 字节；全部条目由原生集合拥有，适配层不复制权威状态。
#[derive(Default)]
pub struct SourceCollections {
    sequence: u64,
    collections: HashMap<u64, Collection>,
}
impl SourceCollections {
    pub fn create(&mut self) -> u64 {
        self.sequence += 1;
        self.collections
            .insert(self.sequence, Collection::default());
        self.sequence
    }
    pub fn set(&mut self, id: u64, key: String, wire: String) -> Result<(), String> {
        let collection = self
            .collections
            .get_mut(&id)
            .ok_or("SOURCE_COLLECTION_NOT_FOUND")?;
        let position = if let Some(position) = collection.positions.get(&key) {
            *position
        } else {
            collection.sequence += 1;
            collection
                .positions
                .insert(key.clone(), collection.sequence);
            collection.sequence
        };
        collection.rows.insert(position, (key, wire));
        Ok(())
    }
    pub fn get(&self, id: u64, key: &str) -> Option<Value> {
        self.wire(id, key)
            .and_then(|wire| serde_json::from_str(&wire).ok())
    }
    pub fn wire(&self, id: u64, key: &str) -> Option<String> {
        let collection = self.collections.get(&id)?;
        collection
            .rows
            .get(collection.positions.get(key)?)
            .map(|(_, wire)| wire.clone())
    }
    pub fn delete(&mut self, id: u64, key: &str) -> bool {
        self.collections.get_mut(&id).is_some_and(|collection| {
            collection
                .positions
                .remove(key)
                .is_some_and(|position| collection.rows.remove(&position).is_some())
        })
    }
    pub fn clear(&mut self, id: u64) {
        if let Some(collection) = self.collections.get_mut(&id) {
            collection.positions.clear();
            collection.rows.clear();
        }
    }
    pub fn values(&self, id: u64) -> Vec<Value> {
        self.collections
            .get(&id)
            .map(|collection| {
                collection
                    .rows
                    .values()
                    .filter_map(|(_, wire)| serde_json::from_str(wire).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn keys(&self, id: u64) -> Vec<String> {
        self.collections
            .get(&id)
            .map(|collection| {
                collection
                    .rows
                    .values()
                    .map(|(key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn size(&self, id: u64) -> usize {
        self.collections
            .get(&id)
            .map(|collection| collection.rows.len())
            .unwrap_or(0)
    }
    pub fn clone_collection(&mut self, id: u64) -> u64 {
        let values = self
            .collections
            .get(&id)
            .map(|collection| collection.rows.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let cloned = self.create();
        for (key, wire) in values {
            let _ = self.set(cloned, key, wire);
        }
        cloned
    }
    pub fn collect(&mut self, retained: &std::collections::HashSet<u64>) {
        self.collections.retain(|id, _| retained.contains(id));
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let id = args["id"].as_u64().unwrap_or(0);
        let key = args["key"].as_str().unwrap_or("");
        match operation {
            "orchestration.sourceManagement.sync.collection.new" => Ok(json!(self.create())),
            "orchestration.sourceManagement.sync.collection.size" => Ok(json!(self.size(id))),
            "orchestration.sourceManagement.sync.collection.get" => Ok(json!(self.wire(id, key))),
            "orchestration.sourceManagement.sync.collection.has" => {
                Ok(json!(self.wire(id, key).is_some()))
            }
            "orchestration.sourceManagement.sync.collection.set" => {
                self.set(
                    id,
                    key.into(),
                    args["wire"].as_str().unwrap_or("null").into(),
                )?;
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.sync.collection.delete" => {
                Ok(json!(self.delete(id, key)))
            }
            "orchestration.sourceManagement.sync.collection.clear" => {
                self.clear(id);
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.sync.collection.page" => {
                let Some(collection) = self.collections.get(&id) else {
                    return Ok(json!([]));
                };
                let cursor = args["cursor"].as_u64().unwrap_or(0);
                let values: Vec<Value> = collection.rows.range((std::ops::Bound::Excluded(cursor), std::ops::Bound::Unbounded)).take(64).map(|(position, (key, wire))| json!({"cursor": position, "key": key, "wire": wire})).collect();
                Ok(json!(values))
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
