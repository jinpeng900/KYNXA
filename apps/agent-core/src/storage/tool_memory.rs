use super::{ContractError, managed_files};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;
const MAX_TOOLS: usize = 4096;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const DOCUMENT_OVERHEAD: usize = 4096;
const EXPERIENCE_RESERVE_BYTES: usize = 256;

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Experience {
    successes: u64,
    failures: u64,
    mean_elapsed_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error_code: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
struct Entry {
    source: String,
    fingerprint: String,
    descriptor: Value,
    #[serde(default)]
    experience: Experience,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    schema_version: u32,
    entries: BTreeMap<String, Entry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentRef<'a> {
    schema_version: u32,
    entries: &'a BTreeMap<String, Entry>,
}

/// Cached declarations and aggregate observations are hints, never permissions or an execution ledger.
/// 工具声明缓存与汇总经验只作选择线索，不授予权限，也不代替正式执行账本。
pub(crate) struct ToolMemory {
    root: PathBuf,
    entries: BTreeMap<String, Entry>,
    disabled_tools: HashSet<String>,
    can_save: bool,
}

fn failure(message: &str, code: &'static str) -> ContractError {
    ContractError::new(message, code, 409)
}

fn valid_source(source: &str) -> bool {
    source == "builtin"
        || source
            .strip_prefix("mcp:")
            .is_some_and(|id| !id.is_empty() && id.len() <= 256 && !id.contains('\0'))
}

fn valid_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn error_code(value: Option<&str>) -> Option<String> {
    value
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 128
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
        .map(str::to_owned)
}

fn declaration(source: &str, descriptor: Value) -> Option<(String, Value)> {
    let name = descriptor["name"]
        .as_str()
        .filter(|name| !name.is_empty() && name.len() <= 1024 && !name.contains('\0'))?
        .to_owned();
    if !descriptor["inputSchema"].is_object() || descriptor["enabled"] == false {
        return None;
    }
    // Persist declaration metadata only; invocation arguments, results, environment and browser identity never enter this file.
    // 仅保存声明元信息；调用实参、结果、环境变量和浏览器身份均不进入此文件。
    let mut fields = serde_json::Map::new();
    for key in [
        "name",
        "description",
        "inputSchema",
        "originalInputSchema",
        "serverId",
        "toolName",
        "annotations",
        "outputSchema",
        "enabled",
        "runtimeCompatible",
        "state",
    ] {
        if let Some(value) = descriptor.get(key) {
            fields.insert(key.into(), value.clone());
        }
    }
    fields.insert("source".into(), Value::String(source.into()));
    Some((name, Value::Object(fields)))
}

fn same_schema(previous: &Value, current: &Value) -> bool {
    previous["inputSchema"] == current["inputSchema"]
        && previous["originalInputSchema"] == current["originalInputSchema"]
        && previous["outputSchema"] == current["outputSchema"]
        && previous["toolName"] == current["toolName"]
}

impl ToolMemory {
    /// A failed disk cache can be bypassed without replacing its corrupt or future-version original.
    /// 磁盘缓存不可读时仅降级为内存缓存，不覆盖损坏或未来版本的原文件。
    pub(crate) fn memory_only(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            entries: BTreeMap::new(),
            disabled_tools: HashSet::new(),
            can_save: false,
        }
    }

    pub(crate) fn load(root: &Path) -> Result<Self, ContractError> {
        let mut memory = Self::memory_only(root);
        let path = root.join("Agent/tool-memory.json");
        if let Some(bytes) = managed_files::read(root, &path, MAX_BYTES)? {
            let document: Document = serde_json::from_slice(&bytes).map_err(|_| {
                failure(
                    "Tool declaration cache is invalid; original preserved. / 工具声明缓存损坏，原文件已保留。",
                    "TOOL_MEMORY_CORRUPT",
                )
            })?;
            if document.schema_version != SCHEMA_VERSION {
                return Err(failure(
                    "Tool declaration cache version is unsupported; original preserved. / 工具声明缓存版本不受支持，原文件已保留。",
                    "TOOL_MEMORY_VERSION_UNSUPPORTED",
                ));
            }
            for (key, entry) in document.entries {
                if !valid_source(&entry.source) || !valid_fingerprint(&entry.fingerprint) {
                    continue;
                }
                let Some((name, descriptor)) = declaration(&entry.source, entry.descriptor) else {
                    continue;
                };
                if key != name {
                    continue;
                }
                let mut experience = entry.experience;
                if !experience.mean_elapsed_ms.is_finite() || experience.mean_elapsed_ms < 0.0 {
                    experience = Experience::default();
                }
                experience.last_error_code = error_code(experience.last_error_code.as_deref());
                memory.entries.insert(
                    name,
                    Entry { descriptor, experience, ..entry },
                );
            }
            memory.trim_to_capacity();
        }
        memory.can_save = true;
        Ok(memory)
    }

    pub(crate) fn reconcile(
        &mut self,
        sources: &HashMap<String, String>,
        disabled_tools: &HashSet<String>,
    ) -> bool {
        self.disabled_tools = disabled_tools.clone();
        let previous_count = self.entries.len();
        self.entries.retain(|name, entry| {
            sources.get(&entry.source) == Some(&entry.fingerprint) && !disabled_tools.contains(name)
        });
        previous_count != self.entries.len()
    }

    pub(crate) fn replace_catalog(
        &mut self,
        source: &str,
        fingerprint: &str,
        descriptors: Vec<Value>,
    ) -> bool {
        if !valid_source(source) || !valid_fingerprint(fingerprint) {
            return false;
        }
        let previous = self.entries.clone();
        self.entries.retain(|_, entry| entry.source != source);
        for descriptor in descriptors {
            let Some((name, descriptor)) = declaration(source, descriptor) else {
                continue;
            };
            if self.disabled_tools.contains(&name)
                || self.entries.get(&name).is_some_and(|entry| entry.source != source)
            {
                continue;
            }
            let experience = previous
                .get(&name)
                .filter(|entry| {
                    entry.source == source
                        && entry.fingerprint == fingerprint
                        && same_schema(&entry.descriptor, &descriptor)
                })
                .map(|entry| entry.experience.clone())
                .unwrap_or_default();
            self.entries.insert(
                name,
                Entry {
                    source: source.into(),
                    fingerprint: fingerprint.into(),
                    descriptor,
                    experience,
                },
            );
        }
        self.trim_to_capacity();
        self.entries != previous
    }

    pub(crate) fn catalog(&self) -> Vec<Value> {
        self.entries
            .values()
            .map(|entry| {
                let mut descriptor = entry.descriptor.clone();
                descriptor["configurationFingerprint"] = Value::String(entry.fingerprint.clone());
                descriptor
            })
            .collect()
    }

    pub(crate) fn record(
        &mut self,
        name: &str,
        succeeded: bool,
        elapsed_ms: u64,
        failure_code: Option<&str>,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(name) else {
            return false;
        };
        let experience = &mut entry.experience;
        let first = experience.successes == 0 && experience.failures == 0;
        if succeeded {
            experience.successes = experience.successes.saturating_add(1);
            experience.last_error_code = None;
        } else {
            experience.failures = experience.failures.saturating_add(1);
            experience.last_error_code = error_code(failure_code);
        }
        let elapsed_ms = elapsed_ms.min(24 * 60 * 60 * 1000) as f64;
        experience.mean_elapsed_ms = if first {
            elapsed_ms
        } else {
            experience.mean_elapsed_ms * 0.75 + elapsed_ms * 0.25
        };
        true
    }

    /// Use this small bounded score only after semantic relevance, never as a popularity-first selector.
    /// 此小幅有界分数仅用于语义相关性之后的次级排序，不能让使用次数压过相关性。
    pub(crate) fn score(&self, name: &str) -> f64 {
        let Some(entry) = self.entries.get(name) else {
            return 0.0;
        };
        let experience = &entry.experience;
        let attempts = experience.successes as f64 + experience.failures as f64;
        let confidence = attempts / (attempts + 4.0);
        let reliability = (experience.successes as f64 + 1.0) / (attempts + 2.0);
        let latency = 1.0 / (1.0 + experience.mean_elapsed_ms / 1000.0);
        ((reliability - 0.5) * 0.16 + latency * 0.02) * confidence
    }

    fn trim_to_capacity(&mut self) {
        // Reserve bounded counter/code growth once; recording a call never re-encodes the entire catalog.
        // 一次性预留计数与错误码增长空间；记录单次调用不重新序列化整份工具目录。
        let mut bytes = DOCUMENT_OVERHEAD;
        let mut count = 0;
        self.entries.retain(|name, entry| {
            let entry_bytes = serde_json::to_vec(entry)
                .map(|encoded| {
                    encoded.len()
                        + serde_json::to_vec(name).map_or(name.len() * 6 + 2, |key| key.len())
                        + EXPERIENCE_RESERVE_BYTES
                        + 8
                })
                .unwrap_or(MAX_BYTES as usize);
            if count >= MAX_TOOLS || bytes.saturating_add(entry_bytes) > MAX_BYTES as usize {
                return false;
            }
            count += 1;
            bytes += entry_bytes;
            true
        });
    }

    pub(crate) fn save(&self) -> Result<(), ContractError> {
        if !self.can_save {
            return Ok(());
        }
        let document = DocumentRef { schema_version: SCHEMA_VERSION, entries: &self.entries };
        let bytes = serde_json::to_vec(&document).map_err(|_| {
            failure(
                "Tool declaration cache could not be encoded. / 工具声明缓存无法编码。",
                "TOOL_MEMORY_WRITE_FAILED",
            )
        })?;
        managed_files::atomic_write(
            &self.root,
            &self.root.join("Agent/tool-memory.json"),
            &bytes,
            MAX_BYTES,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_memory_restarts_and_invalidates_only_changed_declarations() {
        let root = std::env::temp_dir().join(format!("kynxa-tool-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let fingerprint = "a".repeat(64);
        let descriptor = json!({"name":"mcp.fixture.read","description":"Read fixture",
            "inputSchema":{"type":"object","properties":{"path":{"type":"string"}}},
            "arguments":{"credential":"must-not-persist"},"sessionId":"private-session"});
        let mut memory = ToolMemory::load(&root).unwrap();
        assert!(memory.replace_catalog("mcp:fixture", &fingerprint, vec![descriptor.clone()]));
        assert!(memory.record("mcp.fixture.read", true, 40, None));
        let score = memory.score("mcp.fixture.read");
        assert!(score > 0.0 && score < 0.1);
        memory.save().unwrap();
        let original = std::fs::read_to_string(root.join("Agent/tool-memory.json")).unwrap();
        assert!(!original.contains("must-not-persist"));
        assert!(!original.contains("private-session"));
        let mut restored = ToolMemory::load(&root).unwrap();
        assert_eq!(restored.score("mcp.fixture.read"), score);
        assert_eq!(restored.catalog()[0]["configurationFingerprint"], fingerprint);
        assert!(!restored.replace_catalog("mcp:fixture", &fingerprint, vec![descriptor.clone()]));
        assert_eq!(restored.score("mcp.fixture.read"), score);
        let mut renamed = descriptor.clone();
        renamed["description"] = json!("Updated static description");
        assert!(restored.replace_catalog("mcp:fixture", &fingerprint, vec![renamed]));
        assert_eq!(restored.score("mcp.fixture.read"), score);
        let mut changed = descriptor.clone();
        changed["inputSchema"]["properties"]["path"]["type"] = json!("integer");
        assert!(restored.replace_catalog("mcp:fixture", &fingerprint, vec![changed]));
        assert_eq!(restored.score("mcp.fixture.read"), 0.0);
        assert!(restored.record("mcp.fixture.read", false, 5000, Some("TIMEOUT")));
        assert!(restored.score("mcp.fixture.read") < 0.0);
        let mut sources = HashMap::from([("mcp:fixture".into(), fingerprint.clone())]);
        let disabled = HashSet::from(["mcp.fixture.read".into()]);
        assert!(restored.reconcile(&sources, &disabled));
        assert!(restored.catalog().is_empty());
        assert!(!restored.replace_catalog("mcp:fixture", &fingerprint, vec![descriptor.clone()]));
        restored.reconcile(&sources, &HashSet::new());
        assert!(restored.replace_catalog("mcp:fixture", &fingerprint, vec![descriptor.clone()]));
        sources.insert("mcp:fixture".into(), "b".repeat(64));
        assert!(restored.reconcile(&sources, &HashSet::new()));
        assert!(restored.catalog().is_empty());
        restored.replace_catalog("mcp:fixture", &fingerprint, vec![descriptor]);
        assert!(restored.replace_catalog("mcp:fixture", &fingerprint, vec![]));
        assert!(restored.catalog().is_empty());
        restored.replace_catalog("mcp:fixture", &fingerprint, vec![json!({"name":"mcp.fixture.other","inputSchema":{"type":"object"}})]);
        assert!(restored.reconcile(&HashMap::new(), &HashSet::new()));
        assert!(restored.catalog().is_empty());
        let path = root.join("Agent/tool-memory.json");
        std::fs::write(&path, r#"{"schemaVersion":2,"entries":{}}"#).unwrap();
        assert_eq!(ToolMemory::load(&root).err().unwrap().code(), "TOOL_MEMORY_VERSION_UNSUPPORTED");
        ToolMemory::memory_only(&root).save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"schemaVersion":2,"entries":{}}"#);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
