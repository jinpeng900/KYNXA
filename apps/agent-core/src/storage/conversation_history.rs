use super::{ContractError, ContractResult, conversation_logs, managed_files};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

const MIB: usize = 1024 * 1024;
const MAX_RECORD_BYTES: usize = 128 * MIB;
const MAX_INDEX_BYTES: usize = 64 * MIB;
const MAX_CACHE_BYTES: usize = 96 * MIB;
const MAX_PROJECTION_BYTES: usize = 32 * MIB;
static HISTORY_CACHE: Mutex<Vec<(String, Arc<HistoryIndex>)>> = Mutex::new(Vec::new());

#[derive(Clone)]
struct RecordRange {
    offset: u64,
    length: usize,
    streaming: bool,
    projection: Option<Arc<Value>>,
    projection_bytes: usize,
}
#[derive(Clone)]
struct HistoryIndex {
    snapshot: String,
    hash_state: Sha256,
    file_bytes: u64,
    version_token: Option<String>,
    records: Vec<RecordRange>,
    identities: HashMap<String, (usize, String)>,
    bytes: usize,
    has_streaming: bool,
    projection_bytes: usize,
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    version: u8,
    owner: String,
    snapshot: String,
    before: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    direction: Option<String>,
}
#[derive(Deserialize)]
struct Event<'a> {
    #[serde(borrow)]
    message: &'a RawValue,
}
#[derive(Deserialize)]
struct MessageStatus<'a> {
    #[serde(rename = "Status", borrow)]
    status: Option<&'a str>,
}

fn invalid(message: &str) -> ContractError {
    conversation_logs::error(message, "INVALID_CONVERSATION_HISTORY", 400)
}
fn changed() -> ContractError {
    conversation_logs::error(
        "历史已更新，请从最新页重新读取。 / History changed; restart from the latest page.",
        "CONVERSATION_HISTORY_CHANGED",
        409,
    )
}
fn bounded(
    arguments: &Value,
    field: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, ContractError> {
    if arguments[field].is_null() {
        return Ok(default);
    }
    arguments[field]
        .as_u64()
        .filter(|value| (1..=maximum as u64).contains(value))
        .map(|value| value as usize)
        .ok_or_else(|| invalid("分页数量或字节额度无效。"))
}

fn source_hash(reader: &mut managed_files::StableReader) -> Result<String, ContractError> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        conversation_logs::check_cancelled()?;
        let count = reader
            .file
            .read(&mut buffer)
            .map_err(conversation_logs::io_error)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    reader.verify()?;
    reader
        .file
        .seek(SeekFrom::Start(0))
        .map_err(conversation_logs::io_error)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn build_index(
    reader: &mut managed_files::StableReader,
    version_token: Option<String>,
) -> Result<HistoryIndex, ContractError> {
    let mut positions: HashMap<String, (usize, String)> = HashMap::new();
    let mut records = Vec::new();
    let mut offset = 0u64;
    let mut line = Vec::new();
    let mut hash = Sha256::new();
    let mut buffered = BufReader::with_capacity(64 * 1024, &mut reader.file);
    loop {
        conversation_logs::check_cancelled()?;
        line.clear();
        let count = Read::by_ref(&mut buffered)
            .take((MAX_RECORD_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(conversation_logs::io_error)?;
        if count == 0 {
            break;
        }
        if count > MAX_RECORD_BYTES {
            return Err(conversation_logs::error(
                "单条历史事件超过分页读取容量，原文已保留。",
                "CONVERSATION_HISTORY_RECORD_TOO_LARGE",
                413,
            ));
        }
        hash.update(&line);
        let raw = std::str::from_utf8(&line).map_err(|encoding| {
            // Only an unfinished UTF-8 scalar at the unclosed final record is a crash tail.
            // 仅未闭合末条记录中被截断的 UTF-8 字符属于崩溃残尾；完整非法字节仍拒绝读取。
            if !line.ends_with(b"\n") && encoding.error_len().is_none() {
                conversation_logs::error(
                    "历史末尾尚未完整写入，请使用正式日志恢复入口。",
                    "CONVERSATION_HISTORY_TAIL_INCOMPLETE",
                    409,
                )
            } else {
                invalid("历史包含无效 UTF-8 编码。")
            }
        })?;
        let (id, role) = conversation_logs::event_identity(raw).map_err(|error| {
            if !line.ends_with(b"\n") {
                conversation_logs::error(
                    "历史末尾尚未完整写入，请使用正式日志恢复入口。",
                    "CONVERSATION_HISTORY_TAIL_INCOMPLETE",
                    409,
                )
            } else {
                error
            }
        })?;
        let event: Event<'_> = serde_json::from_str(raw).map_err(|_| invalid("历史事件无效。"))?;
        let status: MessageStatus<'_> =
            serde_json::from_str(event.message.get()).map_err(|_| invalid("历史状态无效。"))?;
        let range = RecordRange {
            offset,
            length: count,
            streaming: status.status == Some("streaming"),
            projection: None,
            projection_bytes: 0,
        };
        if let Some((position, saved_role)) = positions.get(&id) {
            if *saved_role != role {
                return Err(invalid("消息 ID 被用于不同角色。"));
            }
            records[*position] = range;
        } else {
            let bytes = (records.len() + 1).saturating_mul(160);
            if bytes > MAX_INDEX_BYTES {
                return Err(conversation_logs::error(
                    "历史位置索引超过当前进程额度，原文已保留。",
                    "CONVERSATION_HISTORY_INDEX_LIMIT",
                    413,
                ));
            }
            positions.insert(id, (records.len(), role));
            records.push(range);
        }
        offset += count as u64;
    }
    drop(buffered);
    reader.verify()?;
    if version_token.is_some() && reader.version_token()? != version_token {
        return Err(changed());
    }
    let has_streaming = records.iter().any(|record: &RecordRange| record.streaming);
    let identity_bytes = positions.capacity()
        * (std::mem::size_of::<String>() * 2 + std::mem::size_of::<usize>() + 16)
        + positions
            .iter()
            .map(|(id, (_, role))| id.capacity() + role.capacity())
            .sum::<usize>();
    Ok(HistoryIndex {
        snapshot: format!("{:x}", hash.clone().finalize()),
        hash_state: hash,
        file_bytes: offset,
        version_token,
        bytes: records.capacity() * std::mem::size_of::<RecordRange>() + identity_bytes,
        records,
        identities: positions,
        has_streaming,
        projection_bytes: 0,
    })
}

/// A completed read keeps source identity and requested records, reopening only for final verification.
/// 完成的读取仅保留来源身份与指定记录，最终复核时短时重开文件，不阻止目录所有者移动会话。
pub(crate) struct HistoryRead {
    pub messages: Vec<String>,
    pub projected_messages: Vec<Value>,
    source: Option<managed_files::StableFileVersion>,
    index: Option<Arc<HistoryIndex>>,
    root: std::path::PathBuf,
    path: std::path::PathBuf,
}
impl HistoryRead {
    /// Append a checked message and advance the existing offsets/hash without rereading the whole log.
    /// 追加已核验消息并更新现有偏移与哈希，不为每次工具回执重新读取整个历史日志。
    pub(crate) fn append_message(&mut self, raw_message: &str, unchanged: bool) -> ContractResult {
        let _guard = conversation_logs::CONVERSATION_TRANSACTIONS
            .lock()
            .map_err(|_| invalid("会话存储锁失效。"))?;
        self.verify()?;
        let event =
            format!("{{\"version\":1,\"type\":\"message.upsert\",\"message\":{raw_message}}}\n");
        let (id, role) = conversation_logs::event_identity(&event)?;
        let status: MessageStatus<'_> =
            serde_json::from_str(raw_message).map_err(|_| invalid("历史状态无效。"))?;
        if let Some((_, saved_role)) = self
            .index
            .as_ref()
            .and_then(|index| index.identities.get(&id))
        {
            if saved_role != &role {
                return Err(conversation_logs::error(
                    "消息 ID 已用于其他角色。",
                    "INVALID_CONVERSATION_DATA",
                    400,
                ));
            }
            if unchanged || self.messages.iter().any(|previous| previous == raw_message) {
                return Ok(json!({"appended":false,"hash":self.index.as_ref().unwrap().snapshot}));
            }
        }
        conversation_logs::check_cancelled()?;
        managed_files::inspect(
            &self.root,
            self.path.parent().unwrap(),
            true,
            false,
            u64::MAX,
        )?;
        managed_files::inspect(&self.root, &self.path, false, true, u64::MAX)?;
        let mut handle = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(conversation_logs::io_error)?;
        std::io::Write::write_all(&mut handle, event.as_bytes())
            .map_err(conversation_logs::io_error)?;
        handle.sync_all().map_err(conversation_logs::io_error)?;
        drop(handle);
        let reader =
            managed_files::open_reader(&self.root, &self.path, u64::MAX)?.ok_or_else(changed)?;
        reader.verify()?;
        let version_token = reader.version_token()?;
        let mut index = self
            .index
            .as_deref()
            .cloned()
            .unwrap_or_else(|| HistoryIndex {
                snapshot: managed_files::digest(&[]),
                hash_state: Sha256::new(),
                file_bytes: 0,
                version_token: None,
                records: Vec::new(),
                identities: HashMap::new(),
                bytes: 0,
                has_streaming: false,
                projection_bytes: 0,
            });
        let range = RecordRange {
            offset: index.file_bytes,
            length: event.len(),
            streaming: status.status == Some("streaming"),
            projection: None,
            projection_bytes: 0,
        };
        if let Some((position, _)) = index.identities.get(&id) {
            index.projection_bytes -= index.records[*position].projection_bytes;
            index.records[*position] = range;
        } else {
            let position = index.records.len();
            index.records.push(range);
            index.identities.insert(id, (position, role));
        }
        index.file_bytes += event.len() as u64;
        index.hash_state.update(event.as_bytes());
        index.snapshot = format!("{:x}", index.hash_state.clone().finalize());
        index.version_token = version_token;
        index.has_streaming = index.records.iter().any(|record| record.streaming);
        index.bytes = index.records.capacity() * std::mem::size_of::<RecordRange>()
            + index.identities.capacity()
                * (std::mem::size_of::<String>() * 2 + std::mem::size_of::<usize>() + 16)
            + index
                .identities
                .iter()
                .map(|(id, (_, role))| id.capacity() + role.capacity())
                .sum::<usize>()
            + index.projection_bytes;
        let receipt = json!({"appended":true,"hash":index.snapshot});
        let index = Arc::new(index);
        let owner = managed_files::digest(self.path.to_string_lossy().as_bytes());
        cache_index(&owner, &index)?;
        self.source = Some(reader.into_version());
        self.index = Some(index);
        self.messages = vec![raw_message.to_owned()];
        self.projected_messages.clear();
        Ok(receipt)
    }
    pub fn message_ids(&self) -> Vec<String> {
        let Some(index) = &self.index else {
            return Vec::new();
        };
        let mut identities = index.identities.iter().collect::<Vec<_>>();
        identities.sort_unstable_by_key(|(_, (position, _))| *position);
        identities.into_iter().map(|(id, _)| id.clone()).collect()
    }
    pub fn verify(&mut self) -> Result<(), ContractError> {
        let Some(source) = &self.source else {
            if managed_files::inspect(&self.root, &self.path, false, true, u64::MAX)? {
                return Err(changed());
            }
            return Ok(());
        };
        let Some(mut reader) = source.reopen()? else {
            return Err(changed());
        };
        let index = self.index.as_ref().unwrap();
        if let Some(token) = &index.version_token {
            if reader.version_token()?.as_ref() != Some(token) {
                return Err(changed());
            }
        } else {
            reader
                .file
                .seek(SeekFrom::Start(0))
                .map_err(conversation_logs::io_error)?;
            if source_hash(&mut reader)? != index.snapshot {
                return Err(changed());
            }
        }
        reader.verify()
    }
}

pub(crate) fn read_latest(
    root: &Path,
    location: &Value,
    message_ids: Option<&HashSet<String>>,
    user_only: bool,
) -> Result<HistoryRead, ContractError> {
    read_snapshot(root, location, message_ids, user_only, true, None)
}

/// Reuse normalized message bodies inside the versioned offset cache, without changing raw journals.
/// 在已有版本化偏移缓存内复用规范化消息，不改写正式原文日志。
/// Projectors share one storage representation; transport filtering and live scope checks stay with callers.
/// 投影器共用同一存储消息表示；公开字段过滤与即时范围核验仍由调用方负责。
pub(crate) fn read_projected(
    root: &Path,
    location: &Value,
    project: fn(&str) -> Result<Value, ContractError>,
) -> Result<HistoryRead, ContractError> {
    read_snapshot(root, location, None, false, true, Some(project))
}

/// Use the log-version index for catalog message IDs without decoding all bodies.
/// 目录消息身份复用日志版本索引，不解码全部正文。
pub(crate) fn read_metadata(root: &Path, location: &Value) -> Result<HistoryRead, ContractError> {
    read_snapshot(root, location, None, false, false, None)
}

fn read_snapshot(
    root: &Path,
    location: &Value,
    message_ids: Option<&HashSet<String>>,
    user_only: bool,
    include_messages: bool,
    project: Option<fn(&str) -> Result<Value, ContractError>>,
) -> Result<HistoryRead, ContractError> {
    conversation_logs::check_cancelled()?;
    let path = conversation_logs::directory(root, location)?.join("events.jsonl");
    // Hold the writer lock only while opening; recheck versions after appends or moves.
    // 仅打开时持写锁，追加或移动后须重核版本。
    let reader = {
        let _guard = conversation_logs::CONVERSATION_TRANSACTIONS
            .lock()
            .map_err(|_| invalid("会话存储锁失效。"))?;
        managed_files::open_reader(root, &path, u64::MAX)?
    };
    let mut result = HistoryRead {
        messages: Vec::new(),
        projected_messages: Vec::new(),
        source: None,
        index: None,
        root: root.to_path_buf(),
        path: path.clone(),
    };
    let Some(mut reader) = reader else {
        return Ok(result);
    };
    let owner = managed_files::digest(path.to_string_lossy().as_bytes());
    let index = indexed(&mut reader, &owner)?;
    if !include_messages {
        reader.verify()?;
        result.source = Some(reader.into_version());
        result.index = Some(index);
        result.verify()?;
        return Ok(result);
    }
    let mut selected: Vec<usize> = if let Some(ids) = message_ids {
        ids.iter()
            .filter_map(|id| index.identities.get(&id.to_ascii_lowercase()))
            .filter(|(_, role)| !user_only || role == "user")
            .map(|(position, _)| *position)
            .collect()
    } else if user_only {
        index
            .identities
            .values()
            .filter(|(_, role)| role == "user")
            .map(|(position, _)| *position)
            .collect()
    } else {
        (0..index.records.len()).collect()
    };
    selected.sort_unstable();
    selected.dedup();
    let mut updated_index: Option<HistoryIndex> = None;
    for position in selected {
        conversation_logs::check_cancelled()?;
        let range = &index.records[position];
        if project.is_some() {
            if let Some(message) = &range.projection {
                result.projected_messages.push(message.as_ref().clone());
                continue;
            }
        }
        reader
            .file
            .seek(SeekFrom::Start(range.offset))
            .map_err(conversation_logs::io_error)?;
        let mut bytes = vec![0; range.length];
        reader
            .file
            .read_exact(&mut bytes)
            .map_err(conversation_logs::io_error)?;
        let raw = std::str::from_utf8(&bytes).map_err(|_| invalid("历史编码无效。"))?;
        let event: Event<'_> = serde_json::from_str(raw).map_err(|_| invalid("历史事件无效。"))?;
        if let Some(project) = project {
            let message = project(event.message.get())?;
            let bytes = projection_bytes(&message).saturating_add(2 * std::mem::size_of::<usize>());
            let retained = updated_index.as_ref().unwrap_or(index.as_ref());
            if retained.projection_bytes.saturating_add(bytes) <= MAX_PROJECTION_BYTES
                && retained.bytes.saturating_add(bytes) <= MAX_CACHE_BYTES
            {
                let updated = updated_index.get_or_insert_with(|| index.as_ref().clone());
                updated.records[position].projection = Some(Arc::new(message.clone()));
                updated.records[position].projection_bytes = bytes;
                updated.projection_bytes += bytes;
                updated.bytes += bytes;
            }
            result.projected_messages.push(message);
        } else {
            result.messages.push(event.message.get().to_string());
        }
    }
    reader.verify()?;
    result.source = Some(reader.into_version());
    result.index = Some(index);
    result.verify()?;
    if let Some(index) = updated_index {
        let index = Arc::new(index);
        cache_index(&owner, &index)?;
        result.index = Some(index);
    }
    Ok(result)
}

fn projection_bytes(value: &Value) -> usize {
    let heap = match value {
        Value::String(text) => text.capacity(),
        Value::Array(items) => items.capacity().saturating_mul(std::mem::size_of::<Value>())
            .saturating_add(items.iter().map(|item| projection_bytes(item)
                .saturating_sub(std::mem::size_of::<Value>())).sum::<usize>()),
        // Conservatively charge ordered-map entries and buckets, not only serialized text length.
        // 保守计入有序映射条目和桶的管理开销，不只计算序列化后的文本长度。
        Value::Object(fields) => fields.len().saturating_mul(192)
            .saturating_add(fields.iter().map(|(key, value)| key.capacity()
                .saturating_add(projection_bytes(value))).sum::<usize>()),
        _ => 0,
    };
    std::mem::size_of::<Value>().saturating_add(heap)
}

fn indexed(
    reader: &mut managed_files::StableReader,
    owner: &str,
) -> Result<Arc<HistoryIndex>, ContractError> {
    let version_token = reader.version_token()?;
    // Reuse offsets after Windows identity/change-stamp checks; unknown stamps require full hashing.
    // Windows 身份及变更时间核验后复用偏移，变更时间未知时仍核对完整哈希。
    let byte_snapshot = if version_token.is_none() {
        Some(source_hash(reader)?)
    } else {
        None
    };
    {
        let mut cache = HISTORY_CACHE
            .lock()
            .map_err(|_| invalid("历史缓存锁失效。"))?;
        if let Some(position) = cache.iter().position(|(key, index)| {
            key == owner
                && if let Some(token) = &version_token {
                    index.version_token.as_ref() == Some(token)
                } else {
                    byte_snapshot.as_ref() == Some(&index.snapshot)
                }
        }) {
            let entry = cache.remove(position);
            let result = Arc::clone(&entry.1);
            cache.push(entry);
            return Ok(result);
        }
    }
    let index = Arc::new(build_index(reader, version_token)?);
    if byte_snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot != &index.snapshot)
    {
        return Err(changed());
    }
    cache_index(owner, &index)?;
    Ok(index)
}

fn cache_index(owner: &str, index: &Arc<HistoryIndex>) -> Result<(), ContractError> {
    if index.bytes > MAX_CACHE_BYTES {
        return Ok(());
    }
    let mut cache = HISTORY_CACHE
        .lock()
        .map_err(|_| invalid("历史缓存锁失效。"))?;
    cache.retain(|(key, _)| key != owner);
    while !cache.is_empty()
        && (cache.len() >= 128
            || cache.iter().map(|(_, index)| index.bytes).sum::<usize>() + index.bytes
                > MAX_CACHE_BYTES)
    {
        cache.remove(0);
    }
    cache.push((owner.to_owned(), Arc::clone(&index)));
    Ok(())
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    if matches!(
        operation,
        "storage.history.cacheBudget" | "storage.history.cachePlan"
    ) {
        let max_bytes = bounded(arguments, "maxBytes", 128 * MIB, 512 * MIB)?;
        let max_entries = bounded(arguments, "maxEntries", 32, 256)?;
        if operation == "storage.history.cacheBudget" {
            return Ok(json!({ "maxBytes": max_bytes, "maxEntries": max_entries }));
        }
        let entries = arguments["entries"]
            .as_array()
            .filter(|entries| entries.len() <= 257)
            .ok_or_else(|| invalid("历史缓存条目无效。"))?;
        let mut retained_bytes = 0u64;
        let mut retained_entries = 0usize;
        let mut evict = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let bytes = entry
                .as_u64()
                .filter(|bytes| *bytes <= 9_007_199_254_740_991)
                .ok_or_else(|| invalid("历史缓存容量无效。"))?;
            if bytes > max_bytes as u64 {
                evict.push(index);
            } else {
                retained_bytes = retained_bytes.saturating_add(bytes);
                retained_entries += 1;
            }
        }
        // Choose oldest-first eviction from size facts without copying original bodies.
        // 根据容量事实从旧到新淘汰，不复制原始正文。
        for (index, entry) in entries.iter().enumerate() {
            if retained_bytes <= max_bytes as u64 && retained_entries <= max_entries {
                break;
            }
            let bytes = entry.as_u64().unwrap();
            if bytes <= max_bytes as u64 {
                evict.push(index);
                retained_bytes -= bytes;
                retained_entries -= 1;
            }
        }
        return Ok(
            json!({"evict":evict,"retainedBytes":retained_bytes,"retainedEntries":retained_entries}),
        );
    }
    if operation != "storage.history.page" {
        return Err(invalid("未知历史分页操作。"));
    }
    read_page(arguments).map(|(page, _)| page)
}

/// Keep each page's source proof outside the evictable cache until its caller finishes scope checks.
/// 分页来源凭证独立于可淘汰缓存保留，直至调用方完成归属核验。
pub(crate) fn read_page(arguments: &Value) -> Result<(Value, HistoryRead), ContractError> {
    conversation_logs::check_cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = conversation_logs::directory(root, &arguments["location"])?.join("events.jsonl");
    let owner = managed_files::digest(path.to_string_lossy().as_bytes());
    let limit = bounded(arguments, "limit", 200, 2000)?;
    let maximum_bytes = bounded(arguments, "maxBytes", 16 * MIB, 64 * MIB)?;
    let cursor = if let Some(value) = arguments["cursor"].as_str() {
        if value.len() > 2048 {
            return Err(invalid("历史游标过长。"));
        }
        let cursor: Cursor = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| invalid("历史游标无效。"))?,
        )
        .map_err(|_| invalid("历史游标无效。"))?;
        let valid_direction = cursor.version == 1 && cursor.direction.is_none()
            || cursor.version == 2 && cursor.direction.as_deref() == Some("newer");
        if !valid_direction || cursor.owner != owner {
            return Err(invalid("历史游标不属于当前聊天。"));
        }
        Some(cursor)
    } else if arguments["cursor"].is_null() {
        None
    } else {
        return Err(invalid("历史游标必须为字符串。"));
    };
    let opened = {
        let _guard = conversation_logs::CONVERSATION_TRANSACTIONS
            .lock()
            .map_err(|_| invalid("会话存储锁失效。"))?;
        managed_files::open_reader(root, &path, u64::MAX)?
    };
    let Some(mut reader) = opened else {
        if cursor.is_some() {
            return Err(changed());
        }
        return Ok(
            (json!({"rawMessages":[],"totalMessages":0,"hasMore":false,"nextCursor":null,
            "hasNewer":false,"newerCursor":null,"startIndex":0,"endIndex":0,
            "snapshot":null,"returnedBytes":0,"byteBudgetExceeded":false,"hasStreamingMessages":false}),
            HistoryRead {
                messages: Vec::new(), projected_messages: Vec::new(), source: None, index: None,
                root: root.to_path_buf(), path,
            }),
        );
    };
    let index = indexed(&mut reader, &owner)?;
    let snapshot = &index.snapshot;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.snapshot != *snapshot)
    {
        return Err(changed());
    }
    let boundary = cursor
        .as_ref()
        .map_or(index.records.len(), |cursor| cursor.before);
    if boundary > index.records.len() {
        return Err(invalid("历史游标范围无效。"));
    }
    let newer = cursor
        .as_ref()
        .is_some_and(|cursor| cursor.direction.is_some());
    let mut start = boundary;
    let mut end = boundary;
    let mut bytes = 0;
    let mut messages = Vec::new();
    // Use the same owner/version checks for both paging directions.
    // 双向分页共用归属及版本核验，限制正文驻留不影响导航。
    let positions: Box<dyn Iterator<Item = usize>> = if newer {
        Box::new(boundary..boundary.saturating_add(limit).min(index.records.len()))
    } else {
        Box::new((boundary.saturating_sub(limit)..boundary).rev())
    };
    for position in positions {
        conversation_logs::check_cancelled()?;
        let range = &index.records[position];
        // Return an oversized first message intact within the record cap; never silently truncate.
        // 首条消息可超页预算但不可超记录硬限，不静默截断。
        if !messages.is_empty() && bytes + range.length > maximum_bytes {
            break;
        }
        reader
            .file
            .seek(SeekFrom::Start(range.offset))
            .map_err(conversation_logs::io_error)?;
        let mut buffer = vec![0u8; range.length];
        reader
            .file
            .read_exact(&mut buffer)
            .map_err(conversation_logs::io_error)?;
        let raw = std::str::from_utf8(&buffer).map_err(|_| invalid("历史编码无效。"))?;
        let event: Event<'_> = serde_json::from_str(raw).map_err(|_| invalid("历史事件无效。"))?;
        messages.push(event.message.get().to_owned());
        bytes += range.length;
        if newer {
            end = position + 1;
        } else {
            start = position;
        }
    }
    reader.verify()?;
    if index.version_token.is_some() && reader.version_token()? != index.version_token {
        return Err(changed());
    }
    if !newer {
        messages.reverse();
    }
    let next = if start > 0 {
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    version: 1,
                    owner: owner.clone(),
                    snapshot: snapshot.clone(),
                    before: start,
                    direction: None,
                })
                .map_err(|_| invalid("无法编码历史游标。"))?,
            ),
        )
    } else {
        None
    };
    let newer_cursor = if end < index.records.len() {
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    version: 2,
                    owner,
                    snapshot: snapshot.clone(),
                    before: end,
                    direction: Some("newer".to_owned()),
                })
                .map_err(|_| invalid("无法编码历史游标。"))?,
            ),
        )
    } else {
        None
    };
    let page = json!({"rawMessages":messages,"totalMessages":index.records.len(),"hasMore":start>0,
        "nextCursor":next,"hasNewer":newer_cursor.is_some(),"newerCursor":newer_cursor,
        "startIndex":start,"endIndex":end,
        "snapshot":snapshot,"returnedBytes":bytes,"byteBudgetExceeded":bytes>maximum_bytes,
        "hasStreamingMessages":index.has_streaming});
    Ok((page, HistoryRead {
        messages: Vec::new(), projected_messages: Vec::new(), source: Some(reader.into_version()),
        index: Some(index), root: root.to_path_buf(), path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, FileTimes, OpenOptions};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_HISTORY: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static PROJECTED_RECORDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn project_test_message(raw: &str) -> Result<Value, ContractError> {
        PROJECTED_RECORDS.with(|count| count.set(count.get() + 1));
        serde_json::from_str(raw).map_err(|_| invalid("测试消息无效。"))
    }

    #[test]
    fn history_pages_verify_evicted_and_over_budget_sources_without_caching_them() {
        let root = std::env::temp_dir().join(format!("kynxa-history-page-budget-{}", uuid::Uuid::new_v4()));
        let location = json!({"Id":"page-budget","ProjectId":null});
        let path = conversation_logs::directory(&root, &location).unwrap().join("events.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = ["first", "second"].iter().map(|id| format!(
            "{{\"version\":1,\"type\":\"message.upsert\",\"message\":{{\"Id\":\"{id}\",\"Role\":\"user\",\"Content\":\"{id}\",\"Status\":\"completed\"}}}}\n"
        )).collect::<String>();
        fs::write(&path, &original).unwrap();
        let owner = managed_files::digest(path.to_string_lossy().as_bytes());
        let (page, mut snapshot) = read_page(&json!({"root":root,"location":location,"limit":1})).unwrap();
        assert_eq!(serde_json::from_str::<Value>(page["rawMessages"][0].as_str().unwrap()).unwrap()["Id"], "second");
        let mut oversized = snapshot.index.as_deref().unwrap().clone();
        oversized.bytes = MAX_CACHE_BYTES + 1;
        let oversized = Arc::new(oversized);
        HISTORY_CACHE.lock().unwrap().retain(|(key, _)| key != &owner);
        cache_index(&owner, &oversized).unwrap();
        assert!(!HISTORY_CACHE.lock().unwrap().iter().any(|(key, _)| key == &owner),
            "An over-budget index must remain outside the shared cache.");
        snapshot.index = Some(oversized);
        snapshot.verify().unwrap();
        let (previous, mut previous_snapshot) = read_page(&json!({
            "root":root,"location":location,"limit":1,"cursor":page["nextCursor"]
        })).unwrap();
        HISTORY_CACHE.lock().unwrap().retain(|(key, _)| key != &owner);
        previous_snapshot.verify().unwrap();
        assert_eq!(previous["snapshot"], page["snapshot"]);
        assert_eq!(serde_json::from_str::<Value>(previous["rawMessages"][0].as_str().unwrap()).unwrap()["Id"], "first");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_pages_without_cache_still_reject_changed_replaced_and_new_sources() {
        let root = std::env::temp_dir().join(format!("kynxa-history-page-proof-{}", uuid::Uuid::new_v4()));
        let location = json!({"Id":"page-proof","ProjectId":null});
        let path = conversation_logs::directory(&root, &location).unwrap().join("events.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let arguments = json!({"root":root,"location":location});
        let (_, mut absent) = read_page(&arguments).unwrap();
        let original = "{\"version\":1,\"type\":\"message.upsert\",\"message\":{\"Id\":\"source\",\"Role\":\"user\",\"Content\":\"before\",\"Status\":\"completed\"}}\n";
        fs::write(&path, original).unwrap();
        assert_eq!(absent.verify().unwrap_err().code(), "CONVERSATION_HISTORY_CHANGED");
        let owner = managed_files::digest(path.to_string_lossy().as_bytes());
        let (_, mut replaced) = read_page(&arguments).unwrap();
        HISTORY_CACHE.lock().unwrap().retain(|(key, _)| key != &owner);
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::rename(&path, path.with_extension("previous")).unwrap();
        fs::write(&path, original).unwrap();
        OpenOptions::new().write(true).open(&path).unwrap()
            .set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert_eq!(replaced.verify().unwrap_err().code(), "MEMORY_CONFLICT");
        let (_, mut changed_snapshot) = read_page(&arguments).unwrap();
        HISTORY_CACHE.lock().unwrap().retain(|(key, _)| key != &owner);
        // Exercise the byte-verified fallback even on Windows volumes with observable change stamps.
        // 即使 Windows 卷提供变更时间，也覆盖缺少该标识时的完整字节核验。
        Arc::make_mut(changed_snapshot.index.as_mut().unwrap()).version_token = None;
        fs::write(&path, original.replace("before", "after!")).unwrap();
        OpenOptions::new().write(true).open(&path).unwrap()
            .set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert_eq!(changed_snapshot.verify().unwrap_err().code(), "CONVERSATION_HISTORY_CHANGED");
        let (_, mut removed) = read_page(&arguments).unwrap();
        HISTORY_CACHE.lock().unwrap().retain(|(key, _)| key != &owner);
        fs::remove_file(&path).unwrap();
        assert_eq!(removed.verify().unwrap_err().code(), "CONVERSATION_HISTORY_CHANGED");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn projected_bodies_reuse_unchanged_records_and_refresh_only_the_appended_message() {
        let root = std::env::temp_dir().join(format!("kynxa-history-projected-{}", uuid::Uuid::new_v4()));
        let location = json!({"Id":"projected-chat","ProjectId":null});
        let path = conversation_logs::directory(&root, &location).unwrap().join("events.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = ["first", "second"].iter().map(|id| format!(
            "{{\"version\":1,\"type\":\"message.upsert\",\"message\":{{\"Id\":\"{id}\",\"Role\":\"user\",\"Content\":\"{id}\",\"Status\":\"completed\"}}}}\n"
        )).collect::<String>();
        fs::write(&path, &original).unwrap();
        PROJECTED_RECORDS.with(|count| count.set(0));
        let first = read_projected(&root, &location, project_test_message).unwrap();
        assert_eq!(PROJECTED_RECORDS.with(|count| count.get()), 2);
        let mut second = read_projected(&root, &location, project_test_message).unwrap();
        assert_eq!(second.projected_messages, first.projected_messages);
        assert_eq!(PROJECTED_RECORDS.with(|count| count.get()), 2,
            "An unchanged read must not decode or normalize the same message bodies again.");
        let before = Arc::clone(first.index.as_ref().unwrap().records[0].projection.as_ref().unwrap());
        second.append_message(&json!({"Id":"second","Role":"user","Content":"updated","Status":"completed"}).to_string(), false).unwrap();
        let third = read_projected(&root, &location, project_test_message).unwrap();
        assert_eq!(PROJECTED_RECORDS.with(|count| count.get()), 3);
        assert_eq!(third.projected_messages[0]["Content"], "first");
        assert_eq!(third.projected_messages[1]["Content"], "updated");
        assert!(Arc::ptr_eq(&before, third.index.as_ref().unwrap().records[0].projection.as_ref().unwrap()));
        assert!(third.index.as_ref().unwrap().bytes >= third.index.as_ref().unwrap().projection_bytes);
        assert!(third.index.as_ref().unwrap().projection_bytes <= MAX_PROJECTION_BYTES);
        assert!(fs::read(&path).unwrap().starts_with(original.as_bytes()));
        let raw = read_latest(&root, &location, None, false).unwrap();
        assert_eq!(raw.messages.len(), 2);
        assert_eq!(serde_json::from_str::<Value>(&raw.messages[1]).unwrap()["Content"], "updated");
        assert!(first.index.as_ref().unwrap().records[0].projection.is_some());
        assert!(third.index.as_ref().unwrap().records[1].projection.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn projected_bodies_recheck_external_edit_identity_and_missing_sources() {
        let root = std::env::temp_dir().join(format!("kynxa-history-projected-version-{}", uuid::Uuid::new_v4()));
        let location = json!({"Id":"version-chat","ProjectId":null});
        let path = conversation_logs::directory(&root, &location).unwrap().join("events.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "{\"version\":1,\"type\":\"message.upsert\",\"message\":{\"Id\":\"user-message\",\"Role\":\"user\",\"Content\":\"before\",\"Status\":\"completed\"}}\n";
        fs::write(&path, original).unwrap();
        let mut first = read_projected(&root, &location, project_test_message).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, original.replace("before", "after!")).unwrap();
        OpenOptions::new().write(true).open(&path).unwrap()
            .set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(first.verify().is_err());
        let mut second = read_projected(&root, &location, project_test_message).unwrap();
        assert_eq!(second.projected_messages[0]["Content"], "after!");
        fs::rename(&path, path.with_extension("previous")).unwrap();
        fs::write(&path, original).unwrap();
        OpenOptions::new().write(true).open(&path).unwrap()
            .set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(second.verify().is_err());
        let mut third = read_projected(&root, &location, project_test_message).unwrap();
        assert_eq!(third.projected_messages[0]["Content"], "before");
        assert!(!Arc::ptr_eq(second.index.as_ref().unwrap(), third.index.as_ref().unwrap()));
        fs::remove_file(&path).unwrap();
        assert!(third.verify().is_err());
        assert!(read_projected(&root, &location, project_test_message).unwrap().projected_messages.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn indexed_append_keeps_exact_log_hash_ids_and_conflict_checks() {
        let root = std::env::temp_dir().join(format!(
            "kynxa-history-append-{}-{}",
            std::process::id(),
            NEXT_HISTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let chat = "11111111-1111-4111-8111-111111111111";
        let target = "22222222-2222-4222-8222-222222222222";
        let location = json!({"Id":chat,"ProjectId":null});
        let path = root.join("Chats").join(chat).join("events.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = format!(
            r#"{{"version":1,"type":"message.upsert","message":{{"Id":"{target}","Role":"assistant","Status":"streaming","Content":"before"}}}}"#
        ) + "\n";
        fs::write(&path, &original).unwrap();
        let ids = HashSet::from([target.to_owned()]);
        let mut read = read_latest(&root, &location, Some(&ids), false).unwrap();
        let updated =
            json!({"Id":target,"Role":"assistant","Status":"completed","Content":"after"})
                .to_string();
        let receipt = read.append_message(&updated, false).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with(original.as_bytes()));
        assert_eq!(receipt["hash"], managed_files::digest(&bytes));
        let cached = read.index.as_ref().unwrap().clone();
        let mut next = read_latest(&root, &location, Some(&ids), false).unwrap();
        assert_eq!(next.messages, vec![updated.clone()]);
        assert!(!next.index.as_ref().unwrap().has_streaming);
        if next.index.as_ref().unwrap().version_token.is_some() {
            assert!(Arc::ptr_eq(&cached, next.index.as_ref().unwrap()));
        }
        assert_eq!(
            next.append_message(&updated, false).unwrap()["appended"],
            false
        );
        let conflict = json!({"Id":target,"Role":"user","Content":"different role"}).to_string();
        assert_eq!(
            next.append_message(&conflict, false).unwrap_err().code(),
            "INVALID_CONVERSATION_DATA"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::write(&path, &original).unwrap();
        assert!(next.append_message(&updated, false).is_err());
        drop(next);
        drop(read);
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn completed_snapshot_releases_directory_and_rejects_replaced_or_new_sources() {
        let root = std::env::temp_dir().join(format!(
            "kynxa-history-lifetime-{}-{}",
            std::process::id(),
            NEXT_HISTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let location = json!({"Id":"source-chat","ProjectId":null});
        let folder = conversation_logs::directory(&root, &location).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("events.jsonl");
        let raw = "{\"version\":1,\"type\":\"message.upsert\",\"message\":{\"Id\":\"saved-user\",\"Role\":\"user\",\"Content\":\"原始内容\",\"Status\":\"completed\"}}\n";
        let mut absent = read_latest(&root, &location, None, false).unwrap();
        absent.verify().unwrap();
        fs::write(&path, raw).unwrap();
        assert_eq!(
            absent.verify().unwrap_err().code,
            "CONVERSATION_HISTORY_CHANGED"
        );
        let mut messages = read_latest(&root, &location, None, false).unwrap();
        let mut metadata = read_metadata(&root, &location).unwrap();
        messages.verify().unwrap();
        metadata.verify().unwrap();
        let destination = root.join("moved-source");
        let source = folder.clone();
        let moved = destination.clone();
        // Both returned snapshots remain alive while the real Windows directory rename executes.
        // 两个已返回快照保持存活时执行真实 Windows 目录移动，验证文件句柄已及时释放。
        std::thread::spawn(move || fs::rename(source, moved))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(messages.messages.len(), 1);
        assert_eq!(metadata.message_ids(), vec!["saved-user"]);
        assert!(messages.verify().is_err());
        assert!(metadata.verify().is_err());
        fs::create_dir(&folder).unwrap();
        fs::write(&path, raw).unwrap();
        assert_eq!(messages.verify().unwrap_err().code, "MEMORY_CONFLICT");
        assert_eq!(metadata.verify().unwrap_err().code, "MEMORY_CONFLICT");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn offsets_reuse_unchanged_bytes_and_detect_edits_with_restored_write_time() {
        let root = std::env::temp_dir().join(format!(
            "kynxa-history-version-{}-{}",
            std::process::id(),
            NEXT_HISTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("events.jsonl");
        let raw = "{\"version\":1,\"type\":\"message.upsert\",\"message\":{\"Id\":\"cached-history\",\"Role\":\"user\",\"Content\":\"before\",\"Status\":\"completed\"}}\n";
        fs::write(&path, raw).unwrap();
        let owner = root.to_string_lossy();
        let mut reader = managed_files::open_reader(&root, &path, u64::MAX)
            .unwrap()
            .unwrap();
        let first = indexed(&mut reader, &owner).unwrap();
        assert_eq!(first.snapshot, managed_files::digest(raw.as_bytes()));
        drop(reader);
        let mut reader = managed_files::open_reader(&root, &path, u64::MAX)
            .unwrap()
            .unwrap();
        let second = indexed(&mut reader, &owner).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        drop(reader);
        let previous = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, raw.replace("before", "after!")).unwrap();
        let writer = OpenOptions::new().write(true).open(&path).unwrap();
        writer
            .set_times(FileTimes::new().set_modified(previous))
            .unwrap();
        drop(writer);
        let mut reader = managed_files::open_reader(&root, &path, u64::MAX)
            .unwrap()
            .unwrap();
        let third = indexed(&mut reader, &owner).unwrap();
        assert!(!Arc::ptr_eq(&first, &third));
        assert_ne!(first.snapshot, third.snapshot);
        drop(reader);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cache_eviction_preserves_smaller_recent_entries_when_one_history_exceeds_budget() {
        let plan = apply(
            "storage.history.cachePlan",
            &json!({
                "maxBytes": 1000, "maxEntries": 2, "entries": [300, 300, 4000, 350]
            }),
        )
        .unwrap();
        assert_eq!(plan["evict"], json!([2, 0]));
        assert_eq!(plan["retainedEntries"], 2);
        assert_eq!(plan["retainedBytes"], 650);
    }
}
