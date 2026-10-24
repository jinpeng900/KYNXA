//! Rebuildable protocol projections; formal history and private continuation verification remain authoritative.
//! 可重建的协议投影缓存；正式历史与私有续传核验仍是权威来源。

use std::{
    collections::VecDeque,
    io::{self, Write},
    mem::size_of,
    sync::{Arc, Mutex, OnceLock},
};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{HistoryProjection, ModelError};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 8;
const ARC_OVERHEAD_BYTES: usize = 3 * size_of::<usize>();
type Version = [u8; 32];

struct Entry {
    version: Version,
    projection: Arc<[u8]>,
}

struct State {
    entries: VecDeque<Entry>,
    reserved_bytes: usize,
    occupied_bytes: usize,
    limit_bytes: usize,
    limit_entries: usize,
}

struct ProjectionCache(Mutex<State>);

impl ProjectionCache {
    fn new(limit_bytes: usize, limit_entries: usize) -> Self {
        let entries = VecDeque::with_capacity(limit_entries);
        let reserved_bytes = size_of::<OnceLock<Self>>()
            .saturating_add(entries.capacity().saturating_mul(size_of::<Entry>()));
        Self(Mutex::new(State {
            entries, reserved_bytes, occupied_bytes: reserved_bytes, limit_bytes, limit_entries,
        }))
    }

    fn get(&self, version: &Version) -> Option<Arc<[u8]>> {
        let mut state = self.0.lock().ok()?;
        let position = state.entries.iter().position(|entry| entry.version == *version)?;
        let entry = state.entries.remove(position)?;
        let projection = Arc::clone(&entry.projection);
        state.entries.push_back(entry);
        Some(projection)
    }

    fn project(
        &self,
        version: Version,
        build: impl FnOnce() -> Result<HistoryProjection, ModelError>,
    ) -> Result<HistoryProjection, ModelError> {
        if let Some(bytes) = self.get(&version) {
            if let Ok(projection) = serde_json::from_slice(&bytes) {
                return Ok(projection);
            }
        }
        let projection = build()?;
        let payload_limit = self.0.lock().ok().map(|state| {
            state.limit_bytes.saturating_sub(
                state.reserved_bytes + ARC_OVERHEAD_BYTES,
            )
        });
        if let Some(limit) = payload_limit {
            let mut writer = LimitedBytes { bytes: Vec::new(), limit };
            if serde_json::to_writer(&mut writer, &projection).is_ok() {
                self.insert(version, Arc::from(writer.bytes.into_boxed_slice()));
            }
        }
        Ok(projection)
    }

    fn insert(&self, version: Version, projection: Arc<[u8]>) {
        let Ok(mut state) = self.0.lock() else { return; };
        // Compact bytes make the shared bound include payload, reserved entries and Arc control blocks.
        // 紧凑字节让共享上限计入正文、预留条目槽和 Arc 控制块，不漏算 JSON 树的结构开销。
        let charged_bytes = projection.len().saturating_add(ARC_OVERHEAD_BYTES);
        if state.limit_entries == 0 || state.reserved_bytes.saturating_add(charged_bytes) > state.limit_bytes {
            return;
        }
        if let Some(position) = state.entries.iter().position(|entry| entry.version == version) {
            let previous = state.entries.remove(position).unwrap();
            state.occupied_bytes -= previous.projection.len() + ARC_OVERHEAD_BYTES;
        }
        while state.entries.len() >= state.limit_entries
            || state.occupied_bytes.saturating_add(charged_bytes) > state.limit_bytes
        {
            let Some(oldest) = state.entries.pop_front() else { return; };
            state.occupied_bytes -= oldest.projection.len() + ARC_OVERHEAD_BYTES;
        }
        state.occupied_bytes += charged_bytes;
        state.entries.push_back(Entry { version, projection });
    }
}

struct LimitedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self.bytes.len().saturating_add(bytes.len());
        if required > self.limit {
            return Err(io::Error::other("projection exceeds optional cache capacity"));
        }
        if required > self.bytes.capacity() {
            let capacity = self.bytes.capacity().saturating_mul(2).max(required).min(self.limit);
            self.bytes.reserve_exact(capacity.saturating_sub(self.bytes.len()));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

struct VersionWriter(Sha256);

impl Write for VersionWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn version(
    protocol: &str,
    formal_history: &[Value],
    available_tools: &[String],
    runtime_origin: Option<&Value>,
) -> Option<Version> {
    // Include private transcripts and exact tool order; IDs alone cannot identify mutable saved messages.
    // 私有转录与原工具顺序一并绑定；消息 ID 本身不能代表可变的已保存内容。
    let mut writer = VersionWriter(Sha256::new());
    serde_json::to_writer(&mut writer, &(protocol, formal_history, available_tools, runtime_origin)).ok()?;
    Some(writer.0.finalize().into())
}

pub(super) fn project(
    protocol: &str,
    formal_history: &[Value],
    available_tools: &[String],
    runtime_origin: Option<&Value>,
    build: impl FnOnce() -> Result<HistoryProjection, ModelError>,
) -> Result<HistoryProjection, ModelError> {
    let Some(version) = version(protocol, formal_history, available_tools, runtime_origin) else {
        return build();
    };
    static CACHE: OnceLock<ProjectionCache> = OnceLock::new();
    CACHE.get_or_init(|| ProjectionCache::new(MAX_BYTES, MAX_ENTRIES)).project(version, build)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::models::gateway_protocol::{build_history_projection, continuation_span, protocol, restore_continuation};

    fn history() -> Vec<Value> {
        vec![
            json!({"Id":"user-1","Role":"user","Content":"Read the authorized file","Status":"completed"}),
            json!({"Id":"assistant-1","Role":"assistant","ReplyTo":"user-1","Status":"completed","Content":"Delivered",
                "ModelTranscript":{"origin":{"route":"fixture-a"},"rounds":[{"round":1,"text":"reading",
                    "calls":[{"id":"call-1","name":"filesystem.read","arguments":{"path":"fixture.txt"}}],
                    "contextBefore":[{"message":{"role":"user","content":"saved runtime"},"source":{
                        "projectionOrigin":"broker-control","runtimeContext":true,"snapshotHash":"a".repeat(64),
                        "authorityHash":"c".repeat(64),"publicText":"saved runtime","instructionAuthority":false,"runtimeKind":"runtime-snapshot"}}]}]},
                "ToolActivities":[{"toolCallId":"call-1","name":"filesystem.read","round":1,"status":"completed",
                    "arguments":{"path":"fixture.txt"},"result":"original successful receipt"}],
                "ToolRun":{"taskCompletion":{"completionState":"completed"}}}),
        ]
    }

    fn load(
        cache: &ProjectionCache,
        history: &[Value],
        protocol: &str,
        tools: &[String],
        origin: Option<&Value>,
        builds: &Cell<usize>,
    ) -> HistoryProjection {
        cache.project(version(protocol, history, tools, origin).unwrap(), || {
            builds.set(builds.get() + 1);
            build_history_projection(protocol, history, tools, origin)
        }).unwrap()
    }

    #[test]
    fn history_projection_cache_reuses_real_projection_without_mutating_inputs_or_tool_pairs() {
        let history = history();
        let original = history.clone();
        let tools = vec!["filesystem.read".into()];
        for protocol in ["openai-completions", "anthropic-messages", "openai-responses"] {
            let cache = ProjectionCache::new(MAX_BYTES, MAX_ENTRIES);
            let builds = Cell::new(0);
            let cold = load(&cache, &history, protocol, &tools, None, &builds);
            let warm = load(&cache, &history, protocol, &tools, None, &builds);
            assert_eq!(builds.get(), 1, "cold=1 build, warm=0 builds: {protocol}");
            assert_eq!(serde_json::to_value(&cold).unwrap(), serde_json::to_value(&warm).unwrap());
            assert!(protocol::complete_pairs(&warm.messages));
            assert!(warm.messages.iter().any(|message| message.to_string().contains("original successful receipt")));
            assert_eq!(history, original);
        }
    }

    #[test]
    fn history_projection_cache_invalidates_full_content_private_transcripts_tools_and_routes() {
        let cache = ProjectionCache::new(MAX_BYTES, MAX_ENTRIES);
        let mut history = history();
        let tools = vec!["filesystem.read".into()];
        let origin = json!({"route":"fixture-a"});
        let other_origin = json!({"route":"fixture-b"});
        let builds = Cell::new(0);
        let protocol = "openai-completions";
        let original = load(&cache, &history, protocol, &tools, Some(&origin), &builds);
        assert!(original.messages.iter().any(|message| message["content"] == "saved runtime"));
        let isolated = load(&cache, &history, protocol, &tools, Some(&other_origin), &builds);
        assert!(!isolated.messages.iter().any(|message| message["content"] == "saved runtime"));
        let revoked = load(&cache, &history, protocol, &[], Some(&origin), &builds);
        assert!(!revoked.messages.iter().any(|message| !super::super::call_ids(message).is_empty()));
        assert!(protocol::complete_pairs(&revoked.messages));

        history[0]["Content"] = json!("Updated content with the same stable message ID");
        let updated = load(&cache, &history, protocol, &tools, Some(&origin), &builds);
        assert_eq!(updated.messages[0]["content"], history[0]["Content"]);
        history[1]["ModelTranscript"]["rounds"][0]["contextBefore"][0]["message"]["content"] = json!("changed private runtime");
        let changed = load(&cache, &history, protocol, &tools, Some(&origin), &builds);
        let direct = build_history_projection(protocol, &history, &tools, Some(&origin)).unwrap();
        assert_eq!(serde_json::to_value(changed).unwrap(), serde_json::to_value(direct).unwrap());
        history.extend([
            json!({"Id":"user-2","Role":"user","Content":"Next question","Status":"completed"}),
            json!({"Id":"assistant-2","Role":"assistant","ReplyTo":"user-2","Content":"Next answer","Status":"completed"}),
        ]);
        let appended = load(&cache, &history, protocol, &tools, Some(&origin), &builds);
        assert!(appended.messages.iter().any(|message| message["content"] == "Next answer"));
        history.drain(..2);
        let deleted = load(&cache, &history, protocol, &tools, Some(&origin), &builds);
        assert!(!serde_json::to_string(&deleted).unwrap().contains("original successful receipt"));
        assert_eq!(builds.get(), 7);
    }

    #[test]
    fn history_projection_cache_eviction_oversize_and_failures_rebuild_without_losing_history() {
        let cache = ProjectionCache::new(64 * 1024, 1);
        let history = history();
        let tools = vec!["filesystem.read".into()];
        let builds = Cell::new(0);
        let first = load(&cache, &history, "openai-completions", &tools, None, &builds);
        load(&cache, &history, "anthropic-messages", &tools, None, &builds);
        let rebuilt = load(&cache, &history, "openai-completions", &tools, None, &builds);
        assert_eq!(builds.get(), 3);
        assert_eq!(serde_json::to_value(&first).unwrap(), serde_json::to_value(rebuilt).unwrap());
        let state = cache.0.lock().unwrap();
        assert!(state.occupied_bytes <= state.limit_bytes);
        assert_eq!(state.entries.len(), 1);
        drop(state);

        let tiny = ProjectionCache::new(256, 1);
        let uncached = Cell::new(0);
        for _ in 0..2 {
            load(&tiny, &history, "openai-completions", &tools, None, &uncached);
        }
        assert_eq!(uncached.get(), 2);
        assert!(tiny.0.lock().unwrap().entries.is_empty());
        let failed_version = [255; 32];
        assert!(cache.project(failed_version, || Err(ModelError::new("FIXTURE_FAILURE", "fixture", 500))).is_err());
        assert!(cache.get(&failed_version).is_none());
    }

    #[test]
    fn history_projection_cache_does_not_cache_verified_private_signatures_or_replay_authority() {
        let cache = ProjectionCache::new(MAX_BYTES, MAX_ENTRIES);
        let history = history();
        let tools = vec!["filesystem.read".into()];
        let builds = Cell::new(0);
        let mut restored = load(&cache, &history, "anthropic-messages", &tools, None, &builds);
        let assistant = &history[1];
        let round = &assistant["ModelTranscript"]["rounds"][0];
        let continuation = [json!({"role":"assistant","content":[
            {"type":"thinking","thinking":"opaque provider continuation","signature":"original-provider-signature"},
            {"type":"text","text":"reading"},{"type":"tool_use","id":"call-1","name":"filesystem_read","input":{"path":"fixture.txt"}}]})];
        let span = continuation_span(&restored, assistant, round).unwrap();
        assert!(restore_continuation(&mut restored, assistant, round, span, &continuation));
        assert!(restored.messages.iter().any(|message| message["content"][0]["signature"] == "original-provider-signature"));
        assert!(protocol::complete_pairs(&restored.messages));
        let cached = load(&cache, &history, "anthropic-messages", &tools, None, &builds);
        assert_eq!(builds.get(), 1);
        assert!(!serde_json::to_string(&cached).unwrap().contains("original-provider-signature"));
        assert!(protocol::complete_pairs(&cached.messages));
    }
}
