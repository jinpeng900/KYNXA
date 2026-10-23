use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::local_files::{
    absolute_path, file_version, inspect, path_key, path_text, random_identity, read_checked,
    sha256,
};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::string;

struct CachedMetadata {
    identity: String,
    checked: Instant,
    metadata: Value,
}

/// Run filesystem I/O separately; keep lookup, commit and statistics in foreground state.
/// 文件系统 I/O 单独执行；查找、提交和统计留在前台状态中。
#[derive(Default)]
pub struct SkillMetadata {
    cache: HashMap<String, CachedMetadata>,
    order: VecDeque<String>,
    prepared: HashMap<String, (String, String, String)>,
    prepared_order: VecDeque<String>,
    reads: usize,
    hits: usize,
}

fn changed() -> ServiceError {
    ServiceError::new("APP_SKILL_CHANGED", "应用技能已变化，请重新读取。", 409)
}

impl SkillMetadata {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "metadata.inspect" => {
                let path = absolute_path(string(&arguments["path"]));
                let Some(metadata) = inspect(&path, true, false)? else {
                    return Ok(Value::Null);
                };
                if !metadata.is_file() || metadata.len() > 256 * 1024 {
                    return Err(ServiceError::new(
                        "INVALID_APP_SKILL",
                        "应用技能文件过大或结构无效。",
                        400,
                    ));
                }
                Ok(
                    json!({"identity":format!("{:?}",file_version(&path,&metadata)?),"canonicalPath":path_text(&std::fs::canonicalize(&path).map_err(ServiceError::io)?)}),
                )
            }
            "metadata.read" => {
                let path = absolute_path(string(&arguments["path"]));
                let metadata = inspect(&path, false, false)?.ok_or_else(changed)?;
                let identity = format!("{:?}", file_version(&path, &metadata)?);
                if identity != string(&arguments["inspection"]["identity"]) {
                    return Err(changed());
                }
                let bytes = read_checked(&path, 256 * 1024, false, "APP_SKILL_CHANGED")?;
                let metadata = inspect(&path, false, false)?.ok_or_else(changed)?;
                if identity != format!("{:?}", file_version(&path, &metadata)?) {
                    return Err(changed());
                }
                let content = std::str::from_utf8(&bytes).map_err(|_| {
                    ServiceError::new("INVALID_APP_SKILL", "应用技能须使用 UTF-8 文本。", 400)
                })?;
                if content.contains('\0') {
                    return Err(ServiceError::new(
                        "INVALID_APP_SKILL",
                        "应用技能不能包含二进制文本。",
                        400,
                    ));
                }
                Ok(
                    json!({"identity":identity,"sha256":sha256(&bytes),"content":content.strip_prefix('\u{feff}').unwrap_or(content)}),
                )
            }
            "metadata.lookup" => {
                let key = path_key(&absolute_path(string(&arguments["path"])));
                let inspection = &arguments["inspection"];
                if inspection.is_null() {
                    self.cache.remove(&key);
                    self.order.retain(|cached| *cached != key);
                    return Ok(json!({"missing":true}));
                }
                let identity = string(&inspection["identity"]);
                if arguments["includeContent"] != true {
                    if let Some(cached) = self.cache.get(&key) {
                        if cached.identity == identity
                            && cached.checked.elapsed() < Duration::from_secs(60)
                        {
                            self.hits += 1;
                            self.order.retain(|cached| *cached != key);
                            self.order.push_back(key);
                            return Ok(json!({"cached":cached.metadata}));
                        }
                    }
                }
                self.reads += 1;
                let id = random_identity()?;
                self.prepared
                    .insert(id.clone(), (key, identity.into(), String::new()));
                self.prepared_order.push_back(id.clone());
                while self.prepared_order.len() > 256 {
                    if let Some(id) = self.prepared_order.pop_front() {
                        self.prepared.remove(&id);
                    }
                }
                Ok(json!({"readId":id}))
            }
            "metadata.acceptRead" => {
                let key = path_key(&absolute_path(string(&arguments["path"])));
                let prepared = self
                    .prepared
                    .get_mut(string(&arguments["readId"]))
                    .ok_or_else(changed)?;
                let data = &arguments["data"];
                if prepared.0 != key || prepared.1 != string(&data["identity"]) {
                    return Err(changed());
                }
                prepared.2 = string(&data["sha256"]).into();
                Ok(Value::Null)
            }
            "metadata.commit" => {
                let key = path_key(&absolute_path(string(&arguments["path"])));
                let id = string(&arguments["readId"]);
                let prepared = self.prepared.remove(id).ok_or_else(changed)?;
                self.prepared_order.retain(|known| known != id);
                if prepared.0 != key
                    || prepared.1 != string(&arguments["identity"])
                    || prepared.2.is_empty()
                    || prepared.2 != string(&arguments["metadata"]["sha256"])
                {
                    return Err(changed());
                }
                self.cache.insert(
                    key.clone(),
                    CachedMetadata {
                        identity: prepared.1,
                        checked: Instant::now(),
                        metadata: arguments["metadata"].clone(),
                    },
                );
                self.order.retain(|known| *known != key);
                self.order.push_back(key);
                while self.order.len() > 256 {
                    if let Some(key) = self.order.pop_front() {
                        self.cache.remove(&key);
                    }
                }
                Ok(arguments["metadata"].clone())
            }
            "metadata.stats" => {
                Ok(json!({"reads":self.reads,"hits":self.hits,"cacheSize":self.cache.len()}))
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown skill metadata operation.",
                400,
            )),
        }
    }
}
