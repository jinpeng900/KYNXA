//! Bounded conversation-scoped snapshots for stable public webpage pagination.
//! 有界、按聊天隔离的公共网页快照；连续分页不重新下载易变页面。

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SNAPSHOT_LIFETIME: Duration = Duration::from_secs(300);
const MAX_SNAPSHOTS: usize = 16;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct PageSnapshot {
    value: Value,
    units: Vec<u16>,
    created: Instant,
    bytes: usize,
}

impl PageSnapshot {
    pub(crate) fn new(mut value: Value) -> Arc<Self> {
        let units = value["fullText"]
            .as_str()
            .unwrap_or("")
            .encode_utf16()
            .collect::<Vec<_>>();
        value["contentHash"] = value["responseHash"].clone();
        value["totalCharacters"] = json!(units.len());
        let bytes = value
            .to_string()
            .len()
            .saturating_add(units.len().saturating_mul(2));
        Arc::new(Self {
            value,
            units,
            created: Instant::now(),
            bytes,
        })
    }

    pub(crate) fn page(&self, input: &Value, cached: bool) -> Value {
        let requested = input["offset"].as_u64().unwrap_or(0).min(usize::MAX as u64) as usize;
        let changed = input["responseHash"]
            .as_str()
            .is_some_and(|hash| self.value["responseHash"].as_str() != Some(hash));
        let unanchored = !cached && requested > 0 && input["responseHash"].as_str().is_none();
        // An expired snapshot may now refer to a different page; restart explicitly rather than mix two versions.
        // 快照过期后正文可能变化；明确从新版第一页恢复，不能把两个版本拼接成同一份证据。
        let mut offset = if changed || unanchored {
            0
        } else {
            requested.min(self.units.len())
        };
        if offset > 0
            && offset < self.units.len()
            && (0xdc00..=0xdfff).contains(&self.units[offset])
        {
            offset -= 1;
        }
        let limit = input["limit"].as_u64().unwrap_or(16000).clamp(1, 16000) as usize;
        let mut end = offset.saturating_add(limit).min(self.units.len());
        if end < self.units.len()
            && end > offset
            && (0xd800..=0xdbff).contains(&self.units[end - 1])
        {
            if end - offset == 1 {
                end += 1;
            } else {
                end -= 1;
            }
        }
        let mut value = self.value.clone();
        value["content"] = json!(
            String::from_utf16(&self.units[offset..end]).expect("validated snapshot boundaries")
        );
        value["offset"] = json!(offset);
        value["nextOffset"] = json!(end);
        value["totalChars"] = json!(self.units.len());
        value["hasMore"] = json!(end < self.units.len());
        value["cache"] = json!({"hit":cached,"scope":"conversation","ageMs":self.created.elapsed().as_millis(),"maxAgeMs":SNAPSHOT_LIFETIME.as_millis()});
        value["pagination"] = json!({
            "requestedOffset":requested,
            "state":if changed {"restarted-content-changed"} else if unanchored {"restarted-cache-miss"} else if requested > self.units.len() {"past-end"} else if offset != requested {"unicode-boundary-adjusted"} else {"page"},
            "restarted":changed || unanchored,
            "source":if cached {"same-response-snapshot"} else {"live-response"},
        });
        value
    }
}

#[derive(Default)]
pub(crate) struct FetchState {
    snapshots: Mutex<HashMap<String, Arc<PageSnapshot>>>,
}

impl FetchState {
    pub(crate) fn get(&self, key: &str) -> Option<Arc<PageSnapshot>> {
        let mut snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        snapshots.retain(|_, snapshot| snapshot.created.elapsed() < SNAPSHOT_LIFETIME);
        snapshots.get(key).cloned()
    }

    pub(crate) fn insert(&self, key: String, snapshot: Arc<PageSnapshot>) {
        let mut snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        snapshots.retain(|candidate, snapshot| {
            candidate != &key && snapshot.created.elapsed() < SNAPSHOT_LIFETIME
        });
        if snapshot.bytes > MAX_SNAPSHOT_BYTES {
            return;
        }
        while snapshots.len() >= MAX_SNAPSHOTS
            || snapshots
                .values()
                .map(|value| value.bytes)
                .sum::<usize>()
                .saturating_add(snapshot.bytes)
                > MAX_SNAPSHOT_BYTES
        {
            let Some(oldest) = snapshots
                .iter()
                .min_by_key(|(_, value)| value.created)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            snapshots.remove(&oldest);
        }
        snapshots.insert(key, snapshot);
    }

    pub(crate) fn clear(&self) {
        self.snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_preserves_unicode_and_reports_end_or_changed_content_without_false_errors() {
        let snapshot = PageSnapshot::new(json!({"fullText":"a😀bc","responseHash":"original"}));
        let first = snapshot.page(&json!({"limit":2}), false);
        assert_eq!(first["content"], "a");
        let next = snapshot.page(&json!({"offset":first["nextOffset"],"limit":1}), true);
        assert_eq!(next["content"], "😀");
        assert_eq!(next["nextOffset"], 3);
        let boundary = snapshot.page(&json!({"offset":2,"limit":1}), true);
        assert_eq!(boundary["content"], "😀");
        assert_eq!(boundary["pagination"]["state"], "unicode-boundary-adjusted");
        let end = snapshot.page(&json!({"offset":9000}), true);
        assert_eq!(end["content"], "");
        assert_eq!(end["hasMore"], false);
        assert_eq!(end["pagination"]["state"], "past-end");
        let changed = snapshot.page(&json!({"offset":4,"responseHash":"old"}), false);
        assert_eq!(changed["offset"], 0);
        assert_eq!(changed["pagination"]["restarted"], true);
        let fresh = snapshot.page(&json!({"offset":9000}), false);
        assert_eq!(fresh["offset"], 0);
        assert_eq!(fresh["pagination"]["state"], "restarted-cache-miss");
        let metadata = crate::models::tool_context::source_metadata(&next);
        assert_eq!(metadata["contentHash"], "original");
        assert_eq!(metadata["totalCharacters"], 5);
    }

    #[test]
    fn snapshots_are_bounded_expire_and_configuration_clear_invalidates_them() {
        let state = FetchState::default();
        for index in 0..20 {
            state.insert(
                index.to_string(),
                PageSnapshot::new(json!({"fullText":"content"})),
            );
        }
        assert_eq!(state.snapshots.lock().unwrap().len(), MAX_SNAPSHOTS);
        assert!(state.get("0").is_none());
        assert!(state.get("19").is_some());
        let mut expired = PageSnapshot::new(json!({"fullText":"expired"}));
        Arc::get_mut(&mut expired).unwrap().created -= SNAPSHOT_LIFETIME;
        state.insert("expired".into(), expired);
        assert!(state.get("expired").is_none());
        state.clear();
        assert!(state.get("19").is_none());
    }
}
