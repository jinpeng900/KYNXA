//! Binary Office decoders read live structured storage without Office or subprocesses.
//! 旧版 Office 解码读取真实活动结构，不要求 Office，也不创建转换子进程。

use super::gateway_office_documents::{cancelled, failure};
use crate::storage::ContractError;
use serde_json::Value;
use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[path = "gateway_legacy_powerpoint.rs"]
mod powerpoint;
#[path = "gateway_legacy_word.rs"]
mod word;

/// Registry canonical names select exactly one versioned binary parser.
/// 注册表规范格式名选择唯一版本化二进制解析器；别名在入口统一归一化。
pub(crate) fn decode(
    format: &str,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    match format {
        "doc" => word::decode(bytes, limits, cancel, digest),
        "ppt" => powerpoint::decode(bytes, limits, cancel, digest),
        _ => Err(failure("DOCUMENT_FORMAT_UNSUPPORTED")),
    }
}

/// Compound streams are not compressed; their declared sizes are checked before allocation.
/// CFB 流没有压缩；分配前核验实际容器、单流和总量，读取与建目录期间均支持取消。
pub(super) struct CompoundDocument<'a> {
    inner: cfb::CompoundFile<CancelReader<'a>>,
    maximum_stream_bytes: u64,
}

struct CancelReader<'a> {
    cursor: Cursor<&'a [u8]>,
    cancel: Arc<AtomicBool>,
}
impl Read for CancelReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::other("ABORT_ERR"));
        }
        self.cursor.read(buffer)
    }
}
impl Seek for CancelReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::other("ABORT_ERR"));
        }
        self.cursor.seek(position)
    }
}

impl<'a> CompoundDocument<'a> {
    pub(super) fn open(
        bytes: &'a [u8],
        limits: &Value,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Self, ContractError> {
        cancelled(cancel)?;
        let maximum_input_bytes = limits["maximumDocumentInputBytes"]
            .as_u64()
            .unwrap_or(32 * 1024 * 1024)
            .min(
                limits["maximumSourceBytes"]
                    .as_u64()
                    .unwrap_or(32 * 1024 * 1024),
            );
        if bytes.len() as u64 > maximum_input_bytes {
            return Err(failure("DOCUMENT_INPUT_LIMIT"));
        }
        if !bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        let maximum_stream_bytes = limits["maximumArchiveEntryBytes"]
            .as_u64()
            .unwrap_or(64 * 1024 * 1024)
            .clamp(1, 1024 * 1024 * 1024);
        let maximum_total_bytes = limits["maximumArchiveExpandedBytes"]
            .as_u64()
            .unwrap_or(256 * 1024 * 1024)
            .clamp(1, 1024 * 1024 * 1024);
        let maximum_entries = limits["maximumArchiveEntries"]
            .as_u64()
            .unwrap_or(8192)
            .clamp(1, 65536);
        let inner = cfb::CompoundFile::open(CancelReader {
            cursor: Cursor::new(bytes),
            cancel: cancel.clone(),
        })
        .map_err(|_| io_failure(cancel))?;
        let mut entries = 0u64;
        let mut total_bytes = 0u64;
        for entry in inner.walk() {
            cancelled(cancel)?;
            entries += 1;
            if entry.is_stream() {
                total_bytes = total_bytes
                    .checked_add(entry.len())
                    .ok_or_else(|| failure("DOCUMENT_ARCHIVE_LIMIT"))?;
                if entry.len() > maximum_stream_bytes || entry.len() > bytes.len() as u64 {
                    return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
                }
            }
            if entries > maximum_entries || total_bytes > maximum_total_bytes {
                return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
            }
        }
        Ok(Self {
            inner,
            maximum_stream_bytes,
        })
    }

    pub(super) fn read(
        &mut self,
        name: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, ContractError> {
        cancelled(cancel)?;
        let mut stream = self
            .inner
            .open_stream(name)
            .map_err(|_| io_failure(cancel))?;
        if stream.len() > self.maximum_stream_bytes {
            return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
        }
        let expected_bytes = stream.len();
        let mut bytes = Vec::with_capacity((expected_bytes as usize).min(65536));
        let mut buffer = [0u8; 65536];
        loop {
            cancelled(cancel)?;
            let count = stream.read(&mut buffer).map_err(|_| io_failure(cancel))?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) as u64 > self.maximum_stream_bytes {
                return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        if bytes.len() as u64 != expected_bytes {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        Ok(bytes)
    }
}

fn io_failure(cancel: &Arc<AtomicBool>) -> ContractError {
    failure(if cancel.load(Ordering::Acquire) {
        "ABORT_ERR"
    } else {
        "DOCUMENT_PARSE_FAILED"
    })
}
pub(super) fn range(bytes: &[u8], offset: usize, count: usize) -> Result<&[u8], ContractError> {
    let end = offset
        .checked_add(count)
        .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))
}
pub(super) fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, ContractError> {
    let raw = range(bytes, offset, 2)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}
pub(super) fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, ContractError> {
    let raw = range(bytes, offset, 4)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}
pub(super) fn decode_utf16(bytes: &[u8]) -> Result<String, ContractError> {
    if bytes.len() % 2 != 0 {
        return Err(failure("DOCUMENT_TEXT_ENCODING_UNSUPPORTED"));
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|raw| u16::from_le_bytes([raw[0], raw[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| failure("DOCUMENT_TEXT_ENCODING_UNSUPPORTED"))
}
