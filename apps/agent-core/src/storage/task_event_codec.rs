//! Lossless storage encoding for stream events in the existing task ledger.
//! 在现有任务账本内无损编码流事件；公开事件、逻辑序号和旧记录格式均保持不变。

use super::TaskEvent;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::io::{Read, Write};

const ENCODING: &str = "zlib-task-event-v1";

pub(super) fn store(event: &TaskEvent, raw: String) -> String {
    if raw.len() < 512 || !matches!(event.kind.as_str(), "text_delta" | "reasoning_delta" | "terminal_output") {
        return raw;
    }
    let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
    // Encoding is optional: failure cannot reject an otherwise valid original event.
    // 压缩仅是存储优化；编码失败不能拒绝原本有效的完整事件。
    if compressor.write_all(raw.as_bytes()).is_err() { return raw; }
    let Ok(bytes) = compressor.finish() else { return raw; };
    let packed = json!({"kind":event.kind,"streamEncoding":ENCODING,
        "originalBytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(raw.as_bytes())),
        "data":STANDARD.encode(bytes)}).to_string();
    if packed.len().saturating_add(32) < raw.len() { packed } else { raw }
}

pub(super) fn expand(raw: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if !raw.contains("\"streamEncoding\"") { return Some(Cow::Borrowed(raw)); }
    let packed: Value = serde_json::from_str(raw).ok()?;
    let Some(encoding) = packed.get("streamEncoding") else { return Some(Cow::Borrowed(raw)); };
    if encoding != ENCODING || !matches!(packed["kind"].as_str(), Some("text_delta" | "reasoning_delta" | "terminal_output")) {
        return None;
    }
    let length = packed["originalBytes"].as_u64().filter(|length| *length <= maximum_bytes as u64)? as usize;
    let encoded = packed["data"].as_str().filter(|encoded| encoded.len() <= maximum_bytes.saturating_mul(2))?;
    let bytes = STANDARD.decode(encoded).ok()?;
    let mut original = Vec::with_capacity(length);
    // Bound decompression by the original record contract, never trust a compressed size declaration.
    // 按原记录合同限制解压容量，不信任压缩数据自报长度；损坏记录保留并明确失败。
    ZlibDecoder::new(bytes.as_slice()).take(maximum_bytes as u64 + 1).read_to_end(&mut original).ok()?;
    if original.len() != length || format!("{:x}", Sha256::digest(&original)) != packed["sha256"].as_str()? {
        return None;
    }
    let original = String::from_utf8(original).ok()?;
    let event: TaskEvent = serde_json::from_str(&original).ok()?;
    if event.kind != packed["kind"] { return None; }
    Some(Cow::Owned(original))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_event_codec_roundtrips_original_bytes_and_keeps_control_events_plain() {
        let event = TaskEvent { task_id:"fixture-task".into(), sequence:17, event_id:"fixture-event".into(),
            kind:"text_delta".into(), payload:json!({"type":"text_delta","delta":"圣堂 🏰\n".repeat(100),
                "eventSequence":8,"requestId":"fixture-request","replyMessageId":"fixture-reply"}), created_at_ms:123 };
        let raw = serde_json::to_string(&event).unwrap();
        let packed = store(&event, raw.clone());
        assert!(packed.len() < raw.len());
        assert_eq!(serde_json::from_str::<Value>(&packed).unwrap()["kind"], "text_delta");
        assert_eq!(expand(&packed, super::super::MAX_RECORD_BYTES).unwrap(), raw);
        let mut control = event;
        control.kind = "task.operation.receipt".into();
        assert_eq!(store(&control, raw.clone()), raw);
        assert_eq!(expand(&raw, super::super::MAX_RECORD_BYTES).unwrap(), raw);
    }

    #[test]
    fn stream_event_codec_rejects_corruption_future_formats_and_expansion_overflow() {
        let event = TaskEvent { task_id:"task".into(), sequence:1, event_id:"event".into(), kind:"reasoning_delta".into(),
            payload:json!({"delta":"preserved output ".repeat(200)}), created_at_ms:1 };
        let packed = store(&event, serde_json::to_string(&event).unwrap());
        let mut corrupt: Value = serde_json::from_str(&packed).unwrap();
        corrupt["sha256"] = json!("0".repeat(64));
        assert!(expand(&corrupt.to_string(), 65536).is_none());
        corrupt["streamEncoding"] = json!("zlib-task-event-v999");
        assert!(expand(&corrupt.to_string(), 65536).is_none());
        assert!(expand(&packed, 128).is_none());
    }
}
