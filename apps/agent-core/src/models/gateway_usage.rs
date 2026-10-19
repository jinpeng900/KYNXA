//! Optional provider-reported usage telemetry; this is not the formal conversation journal.
//! 可选的供应商真实用量遥测；这不是正式会话日志。

use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use uuid::Uuid;

const MAXIMUM_QUEUED_RECORDS: usize = 128;
const MAXIMUM_LABEL_CHARACTERS: usize = 160;
const WRITE_ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_millis(100);
static UNRECORDED_CALLS: AtomicU64 = AtomicU64::new(0);
static WARNING_CODES: AtomicU8 = AtomicU8::new(0);

fn configured_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("KYNXA_USAGE_LOG")?);
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return None;
    }
    std::path::absolute(path).ok()
}

/// Read only configuration; discovering telemetry never creates a file or starts its writer.
/// 只读取配置；发现遥测能力时不会创建文件或启动写盘线程。
pub(crate) fn telemetry_configuration() -> Value {
    let path = configured_path();
    json!({"protocol":1,"enabled":path.is_some(),"logPath":path})
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenCounts {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cache_hit_tokens: Option<u64>,
    cache_miss_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    #[serde(skip)]
    reported_total_tokens: Option<u64>,
}

impl TokenCounts {
    fn observe(&mut self, protocol: &str, usage: &Value) {
        let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64);
        let input = count("/prompt_tokens").or_else(|| count("/input_tokens"));
        let cache_read = count("/cache_read_input_tokens");
        let cache_create = count("/cache_creation_input_tokens");
        let input = if protocol == "anthropic-messages" {
            // Anthropic reports uncached input separately; add only provider-reported cache components.
            // Anthropic 单独报告未缓存输入；这里只加回供应商明确报告的缓存读取/创建部分。
            input.and_then(|input| {
                input
                    .checked_add(cache_read.unwrap_or(0))?
                    .checked_add(cache_create.unwrap_or(0))
            })
        } else {
            input
        };
        update(&mut self.input_tokens, input);
        update(
            &mut self.output_tokens,
            count("/completion_tokens").or_else(|| count("/output_tokens")),
        );
        update(
            &mut self.cache_hit_tokens,
            count("/prompt_cache_hit_tokens")
                .or_else(|| count("/prompt_tokens_details/cached_tokens"))
                .or_else(|| count("/input_tokens_details/cached_tokens"))
                .or(cache_read),
        );
        update(
            &mut self.cache_miss_tokens,
            count("/prompt_cache_miss_tokens")
                .or_else(|| self.input_tokens?.checked_sub(self.cache_hit_tokens?)),
        );
        update(&mut self.reported_total_tokens, count("/total_tokens"));
        self.total_tokens = self
            .reported_total_tokens
            .or_else(|| self.input_tokens?.checked_add(self.output_tokens?));
        update(
            &mut self.reasoning_tokens,
            count("/completion_tokens_details/reasoning_tokens")
                .or_else(|| count("/output_tokens_details/reasoning_tokens"))
                .or_else(|| count("/reasoning_tokens")),
        );
        if self
            .input_tokens
            .zip(self.cache_hit_tokens)
            .is_some_and(|(input, hit)| hit > input)
        {
            self.cache_hit_tokens = None;
            self.cache_miss_tokens = None;
        }
        if self
            .input_tokens
            .zip(self.cache_miss_tokens)
            .is_some_and(|(input, miss)| miss > input)
        {
            self.cache_miss_tokens = None;
        }
        if self
            .input_tokens
            .zip(self.cache_hit_tokens.zip(self.cache_miss_tokens))
            .is_some_and(|(input, (hit, miss))| hit.checked_add(miss) != Some(input))
        {
            self.cache_hit_tokens = None;
            self.cache_miss_tokens = None;
        }
    }

    fn is_missing(&self) -> bool {
        self.input_tokens.is_none() && self.output_tokens.is_none() && self.total_tokens.is_none()
    }
}

fn update(target: &mut Option<u64>, observed: Option<u64>) {
    if observed.is_some() {
        *target = observed;
    }
}

fn label(value: &Value) -> Option<String> {
    let value = value.as_str()?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.chars().take(MAXIMUM_LABEL_CHARACTERS).collect())
}

#[derive(Default)]
struct AttemptState {
    started: Option<Instant>,
    call_id: Option<Uuid>,
    counts: TokenCounts,
    actual_model: Option<String>,
    finish_reason: Option<String>,
}

struct AttemptDetails {
    path: PathBuf,
    provider_id: Option<String>,
    requested_model: Option<String>,
    protocol: String,
    state: Mutex<AttemptState>,
}

/// The guard emits once even when an owning future is dropped; pre-dispatch failures emit nothing.
/// 即使拥有者 future 被丢弃也只提交一次终态；HTTP 派发前失败不产生记录。
#[derive(Default)]
pub(super) struct UsageAttempt {
    details: Option<AttemptDetails>,
}

impl UsageAttempt {
    pub(super) fn configured(connection: &Value, body: &Value) -> Self {
        let Some(path) = configured_path() else {
            return Self::default();
        };
        Self::at_path(path, connection, body)
    }

    pub(super) fn at_path(path: PathBuf, connection: &Value, body: &Value) -> Self {
        if !path.is_absolute() {
            warn("MODEL_USAGE_LOG_PATH_INVALID");
            return Self::default();
        }
        Self {
            details: Some(AttemptDetails {
                path,
                provider_id: label(&connection["providerId"]),
                requested_model: label(&body["model"]),
                protocol: connection["protocol"]
                    .as_str()
                    .unwrap_or("openai-completions")
                    .to_owned(),
                state: Mutex::new(AttemptState::default()),
            }),
        }
    }

    pub(super) fn dispatched(&self) {
        let Some(details) = &self.details else {
            return;
        };
        let mut state = details
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.started.is_none() {
            state.started = Some(Instant::now());
            state.call_id = Some(Uuid::new_v4());
        }
    }

    pub(super) fn observe(&self, raw: &Value, usage: &Value, finish: &Value) {
        let Some(details) = &self.details else {
            return;
        };
        let mut state = details
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.counts.observe(&details.protocol, usage);
        for item in [raw, &raw["message"], &raw["response"]] {
            state.counts.observe(&details.protocol, &item["usage"]);
            if details.protocol == "ollama" {
                state.counts.observe(
                    &details.protocol,
                    &json!({
                        "prompt_tokens":item["prompt_eval_count"],
                        "completion_tokens":item["eval_count"]
                    }),
                );
            }
            if let Some(model) = label(&item["model"]) {
                state.actual_model = Some(model);
            }
            for reason in [
                &item["choices"][0]["finish_reason"],
                &item["stop_reason"],
                &item["done_reason"],
                &item["delta"]["stop_reason"],
                finish,
            ] {
                if let Some(reason) = label(reason) {
                    state.finish_reason = Some(reason);
                }
            }
        }
    }

    pub(super) fn observe_frame(&self, frame: &Value, usage: &Value) {
        if self.details.is_none() {
            return;
        }
        // Only the opted-in observer parses frame metadata; no body text enters its state or output.
        // 仅启用的遥测解析帧元数据；正文从不进入其状态或输出。
        let raw = serde_json::from_str(frame["data"].as_str().unwrap_or("")).unwrap_or(Value::Null);
        self.observe(&raw, usage, &Value::Null);
    }

    pub(super) async fn complete(&mut self, status: &'static str) {
        let Some(record) = self.take_record(status) else {
            return;
        };
        if let Some(acknowledgement) = enqueue(record) {
            if tokio::time::timeout(WRITE_ACKNOWLEDGEMENT_TIMEOUT, acknowledgement)
                .await
                .is_err()
            {
                warn("MODEL_USAGE_LOG_FLUSH_PENDING");
            }
        }
    }

    fn take_record(&mut self, status: &'static str) -> Option<QueuedRecord> {
        let details = self.details.take()?;
        let state = details
            .state
            .into_inner()
            .unwrap_or_else(|error| error.into_inner());
        let started = state.started?;
        let status = if status == "completed" && state.counts.is_missing() {
            "missing_usage"
        } else {
            status
        };
        Some(QueuedRecord {
            path: details.path,
            event: UsageEvent {
                schema_version: 1,
                event: "call_completed",
                call_id: state.call_id?.to_string(),
                timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                provider_id: details.provider_id,
                requested_model: details.requested_model,
                actual_model: state.actual_model,
                status,
                counts: state.counts,
                upstream_end_to_end_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                finish_reason: state.finish_reason,
                unrecorded_calls: 0,
            },
            acknowledgement: None,
        })
    }
}

impl Drop for UsageAttempt {
    fn drop(&mut self) {
        if let Some(record) = self.take_record("cancelled") {
            let _ = enqueue(record);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageEvent {
    schema_version: u32,
    event: &'static str,
    call_id: String,
    timestamp: String,
    provider_id: Option<String>,
    requested_model: Option<String>,
    actual_model: Option<String>,
    status: &'static str,
    #[serde(flatten)]
    counts: TokenCounts,
    upstream_end_to_end_ms: u64,
    finish_reason: Option<String>,
    unrecorded_calls: u64,
}

struct QueuedRecord {
    path: PathBuf,
    event: UsageEvent,
    acknowledgement: Option<oneshot::Sender<()>>,
}

fn enqueue(mut record: QueuedRecord) -> Option<oneshot::Receiver<()>> {
    static WRITER: OnceLock<Option<SyncSender<QueuedRecord>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (sender, records) = sync_channel::<QueuedRecord>(MAXIMUM_QUEUED_RECORDS);
        match std::thread::Builder::new()
            .name("kynxa-model-usage".into())
            .spawn(move || {
                while let Ok(mut record) = records.recv() {
                    // A cumulative process counter makes later successful rows reveal earlier gaps.
                    // 进程累计计数让后续成功行显式暴露此前未记录的调用，不能假装覆盖完整。
                    record.event.unrecorded_calls = UNRECORDED_CALLS.load(Ordering::Relaxed);
                    if append_record(&record).is_err() {
                        UNRECORDED_CALLS.fetch_add(1, Ordering::Relaxed);
                        warn("MODEL_USAGE_LOG_WRITE_FAILED");
                    }
                    if let Some(acknowledgement) = record.acknowledgement {
                        let _ = acknowledgement.send(());
                    }
                }
            }) {
            Ok(_) => Some(sender),
            Err(_) => None,
        }
    });
    let (acknowledgement, received) = oneshot::channel();
    record.acknowledgement = Some(acknowledgement);
    if writer
        .as_ref()
        .is_none_or(|writer| writer.try_send(record).is_err())
    {
        UNRECORDED_CALLS.fetch_add(1, Ordering::Relaxed);
        warn("MODEL_USAGE_LOG_QUEUE_UNAVAILABLE");
        return None;
    }
    Some(received)
}

fn append_record(record: &QueuedRecord) -> std::io::Result<()> {
    if let Some(parent) = record.path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec(&record.event)?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&record.path)?;
    if file.metadata()?.len() > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last_byte = [0_u8];
        file.read_exact(&mut last_byte)?;
        if last_byte[0] != b'\n' {
            // Keep an interrupted old line distinct so the next complete record remains readable.
            // 将旧的中断尾行单独封口，避免下一条完整记录与坏尾行拼接后也无法读取。
            file.write_all(b"\n")?;
        }
    }
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_data()
}

fn warn(code: &'static str) {
    let bit = match code {
        "MODEL_USAGE_LOG_PATH_INVALID" => 1,
        "MODEL_USAGE_LOG_FLUSH_PENDING" => 2,
        "MODEL_USAGE_LOG_WRITE_FAILED" => 4,
        _ => 8,
    };
    if WARNING_CODES.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        eprintln!("{code}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_preserves_zero_unknowns_and_protocol_cache_semantics() {
        let mut counts = TokenCounts::default();
        counts.observe(
            "openai-completions",
            &json!({"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,
                "prompt_tokens_details":{"cached_tokens":0},
                "completion_tokens_details":{"reasoning_tokens":0}}),
        );
        assert_eq!(counts.cache_hit_tokens, Some(0));
        assert_eq!(counts.cache_miss_tokens, Some(100));
        assert_eq!(counts.reasoning_tokens, Some(0));
        counts.observe("openai-completions", &json!({"total_tokens":105}));
        assert_eq!(counts.input_tokens, Some(100));

        let mut anthropic = TokenCounts::default();
        anthropic.observe(
            "anthropic-messages",
            &json!({"input_tokens":10,"cache_read_input_tokens":80,"cache_creation_input_tokens":20}),
        );
        anthropic.observe("anthropic-messages", &json!({"output_tokens":5}));
        assert_eq!(anthropic.input_tokens, Some(110));
        assert_eq!(anthropic.cache_hit_tokens, Some(80));
        assert_eq!(anthropic.cache_miss_tokens, Some(30));
        assert_eq!(anthropic.total_tokens, Some(115));

        let mut unknown = TokenCounts::default();
        unknown.observe("ollama", &json!({"prompt_tokens":3,"completion_tokens":5}));
        assert_eq!(unknown.total_tokens, Some(8));
        assert_eq!(unknown.cache_hit_tokens, None);
        assert_eq!(unknown.cache_miss_tokens, None);
        assert_eq!(unknown.reasoning_tokens, None);
    }

    #[test]
    fn contradictory_provider_cache_fields_are_unknown() {
        let mut counts = TokenCounts::default();
        counts.observe(
            "openai-completions",
            &json!({"prompt_tokens":10,"prompt_cache_hit_tokens":8,"prompt_cache_miss_tokens":8}),
        );
        assert!(counts.cache_hit_tokens.is_none());
        assert!(counts.cache_miss_tokens.is_none());
    }
}
