use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::intent::matches;
use super::local_files::{
    absolute_path, file_version, handle_version, inspect, is_link, open_read, path_key, path_text,
    random_identity, read_checked, sha256,
};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::string;

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_READ_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BINARY_FILE_BYTES: u64 = 64 * 1024 * 1024;
static FILE_LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();

fn invalid(message: &str) -> ServiceError {
    ServiceError::new("INVALID_TOOL_ARGUMENTS", message, 400)
}
fn conflict(message: &str) -> ServiceError {
    ServiceError::new("TOOL_FILE_CONFLICT", message, 409)
}
fn cancelled(cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<()> {
    if cancellation
        .as_ref()
        .is_some_and(|value| value.load(Ordering::Acquire))
    {
        return Err(ServiceError::new("TOOL_CANCELLED", "文件操作已取消。", 409));
    }
    Ok(())
}
fn bounded(value: &Value, fallback: u64, minimum: u64, maximum: u64) -> ServiceResult<u64> {
    if value.is_null() {
        return Ok(fallback);
    }
    value
        .as_u64()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| invalid(&format!("整数参数须在 {minimum}–{maximum} 范围内。")))
}

pub(super) fn canonical(path: &Path, allow_missing: bool) -> ServiceResult<PathBuf> {
    let mut ancestor = path.to_owned();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(mut path) => {
                for part in missing.iter().rev() {
                    path.push(part);
                }
                return Ok(absolute_path(&path_text(&path)));
            }
            Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&ancestor).is_ok_and(|metadata| is_link(&metadata)) {
                    return Err(ServiceError::new(
                        "UNSAFE_TOOL_PATH",
                        "路径链接的目标不存在。",
                        403,
                    ));
                }
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| ServiceError::io(error))?
                    .to_owned();
                missing.push(name);
                ancestor = ancestor.parent().unwrap().to_owned();
            }
            Err(error) => return Err(ServiceError::io(error)),
        }
    }
}

pub(super) fn revalidate(binding: &Value) -> ServiceResult<()> {
    if binding.is_null() {
        return Ok(());
    }
    let changed = || {
        ServiceError::new(
            "TOOL_PATH_CHANGED",
            "路径或工作文件夹的真实目标已改变，请重新读取后重试。",
            409,
        )
    };
    let requested = binding["requestedPath"].as_str().ok_or_else(changed)?;
    let target = binding["path"].as_str().ok_or_else(changed)?;
    if path_text(&canonical(
        &absolute_path(requested),
        binding["allowMissing"] == true,
    )?) != target
    {
        return Err(changed());
    }
    if let Some(requested_workspace) = binding["requestedWorkspaceRoot"].as_str() {
        if path_text(&canonical(&absolute_path(requested_workspace), false)?)
            != string(&binding["workspaceRoot"])
        {
            return Err(changed());
        }
    }
    for (index, root) in binding["protectedRoots"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if path_text(&canonical(&absolute_path(string(root)), true)?)
            != string(&binding["canonicalProtectedRoots"][index])
        {
            return Err(changed());
        }
    }
    inspect(
        &absolute_path(target),
        binding["allowMissing"] == true,
        binding["allowHardLinks"] == true,
    )?;
    Ok(())
}

fn read_bounded(path: &Path, maximum: u64) -> ServiceResult<Vec<u8>> {
    let metadata = inspect(path, false, true)?.ok_or_else(|| invalid("路径不是普通文件。"))?;
    if !metadata.is_file() {
        return Err(invalid("路径不是普通文件。"));
    }
    if metadata.len() > maximum {
        return Err(ServiceError::new(
            "TOOL_FILE_TOO_LARGE",
            "文件超过本次读取的大小上限。",
            400,
        ));
    }
    read_checked(path, maximum, true, "TOOL_FILE_CONFLICT")
}

fn decode_text(bytes: &[u8]) -> ServiceResult<&str> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|text| !text.contains('\0'))
        .ok_or_else(|| ServiceError::new("NON_TEXT_FILE", "文件不是受支持的 UTF-8 文本。", 400))
}

fn check_hash(path: &Path, expected: &Value) -> ServiceResult<Option<Vec<u8>>> {
    check_hash_bounded(path, expected, MAX_FILE_BYTES)
}

fn check_hash_bounded(path: &Path, expected: &Value, maximum: u64) -> ServiceResult<Option<Vec<u8>>> {
    let Some(metadata) = inspect(path, true, false)? else {
        if !expected.is_null() {
            return Err(conflict("文件已不存在或版本不匹配，请重新读取。"));
        }
        return Ok(None);
    };
    if !metadata.is_file() {
        return Err(invalid("路径不是普通文件。"));
    }
    let bytes = read_bounded(path, maximum)?;
    if !matches(r"^[a-f0-9]{64}$", string(expected)) || sha256(&bytes) != string(expected) {
        return Err(conflict("文件已更新，请重新读取后再修改。"));
    }
    Ok(Some(bytes))
}

pub(super) fn text_page(text: &str, offset: usize, maximum: usize) -> ServiceResult<Value> {
    let units = text.encode_utf16().collect::<Vec<_>>();
    if offset > units.len() {
        return Err(invalid("offset 超出当前文件长度，请从 0 重新读取。"));
    }
    let high = |unit: u16| (0xd800..=0xdbff).contains(&unit);
    let low = |unit: u16| (0xdc00..=0xdfff).contains(&unit);
    if offset > 0 && offset < units.len() && high(units[offset - 1]) && low(units[offset]) {
        return Err(invalid(
            "offset 不能位于字符代理对中间，请使用上一页的 nextOffset。",
        ));
    }
    let mut next = (offset + maximum).min(units.len());
    if next > 0 && next < units.len() && high(units[next - 1]) && low(units[next]) {
        if next - offset == 1 {
            next += 1;
        } else {
            next -= 1;
        }
    }
    let more = next < units.len();
    Ok(
        json!({"content":String::from_utf16_lossy(&units[offset..next]),"offset":offset,"nextOffset":next,
        "totalCharacters":units.len(),"hasMore":more,"truncated":offset > 0 || more}),
    )
}

fn read_page(path: &Path, input: &Value, cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<Value> {
    let maximum_bytes = bounded(&input["maxBytes"], MAX_READ_FILE_BYTES, 1, MAX_READ_FILE_BYTES)?;
    let maximum = bounded(&input["maxChars"], 32000, 1, 64000)? as usize;
    let ranged = input.get("startLine").is_some() || input.get("endLine").is_some();
    let start_line = bounded(&input["startLine"], 1, 1, MAX_READ_FILE_BYTES + 1)? as usize;
    let end_line = bounded(&input["endLine"], MAX_READ_FILE_BYTES + 1, 1, MAX_READ_FILE_BYTES + 1)? as usize;
    if end_line < start_line { return Err(invalid("endLine 不能小于 startLine。")); }
    let requested_offset = (!input["offset"].is_null()).then(|| bounded(
        &input["offset"], 0, 0, MAX_READ_FILE_BYTES)).transpose()?.map(|value| value as usize);
    let expected = input["expectedSha256"].as_str();
    if !input["expectedSha256"].is_null() && !expected.is_some_and(|value| matches(r"^[a-f0-9]{64}$", value)) {
        return Err(invalid("expectedSha256 必须是完整的 SHA-256。"));
    }
    let original = inspect(path, false, true)?.filter(|metadata| metadata.is_file())
        .ok_or_else(|| invalid("路径不是普通文件。"))?;
    if original.len() > maximum_bytes {
        return Err(ServiceError::new("TOOL_FILE_TOO_LARGE", "文件超过本次读取的大小上限。", 400));
    }
    let first = file_version(path, &original)?;
    let mut file = open_read(path)?;
    if first != handle_version(&file, &file.metadata().map_err(ServiceError::io)?)? {
        return Err(conflict("文件在读取前已变化，请重新搜索或读取。"));
    }
    // Hash the whole stable file while retaining only one output page and one decoder block.
    // 全文件流式核验版本，只保留一页输出及一个解码块；长行不会变成整行内存副本。
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut pending = Vec::new();
    let mut encoding = None;
    let mut bytes_read = 0u64;
    let mut window = ReadWindow {
        start_line, end_line, requested_offset, maximum, offset: 0, line: 1,
        range_start: (start_line == 1).then_some(0), range_end: None,
        content: String::new(), content_units: 0, page_closed: false, next_line: start_line,
    };
    loop {
        cancelled(cancellation)?;
        let count = file.read(&mut buffer).map_err(ServiceError::io)?;
        let end = count == 0;
        bytes_read += count as u64;
        if bytes_read > maximum_bytes { return Err(conflict("文件在读取期间已增大，请重新读取。")); }
        hash.update(&buffer[..count]);
        pending.extend_from_slice(&buffer[..count]);
        if encoding.is_none() {
            if pending.len() < 3 && !end { continue; }
            let (name, prefix) = if pending.starts_with(&[0xff, 0xfe]) { ("utf-16le", 2) }
                else if pending.starts_with(&[0xef, 0xbb, 0xbf]) { ("utf-8", 3) }
                else { ("utf-8", 0) };
            pending.drain(..prefix);
            encoding = Some(name);
        }
        let encoding = encoding.unwrap();
        let available = if encoding == "utf-16le" {
            let mut available = pending.len() / 2 * 2;
            if !end && available >= 2 && (0xd800..=0xdbff).contains(&u16::from_le_bytes([
                pending[available - 2], pending[available - 1]])) { available -= 2; }
            available
        } else {
            match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(error) if error.error_len().is_none() && !end => error.valid_up_to(),
                Err(_) => return Err(ServiceError::new("NON_TEXT_FILE", "文件不是受支持的 UTF-8 或 UTF-16LE 文本。", 400)),
            }
        };
        let decoded = super::service_source_reading::decode(&pending[..available], encoding)
            .map_err(|_| ServiceError::new("NON_TEXT_FILE", "文本编码或字符边界无效。", 400))?;
        for character in decoded.chars() {
            if character == '\0' { return Err(ServiceError::new("NON_TEXT_FILE", "文件包含二进制内容。", 400)); }
            window.observe(character)?;
        }
        pending.drain(..available);
        if end {
            if !pending.is_empty() { return Err(ServiceError::new("NON_TEXT_FILE", "文本编码或字符边界无效。", 400)); }
            break;
        }
    }
    cancelled(cancellation)?;
    let after = inspect(path, false, true)?.ok_or_else(|| conflict("读取期间文件已移除。"))?;
    if bytes_read != first.size || first != file_version(path, &after)?
        || first != handle_version(&file, &file.metadata().map_err(ServiceError::io)?)? {
        return Err(conflict("文件在读取期间已变化，请重新读取。"));
    }
    let actual_hash = format!("{:x}", hash.finalize());
    if expected.is_some_and(|expected| expected != actual_hash) {
        return Err(conflict("文件已更新，请重新搜索或从新版本读取。"));
    }
    let mut page = window.finish(ranged)?;
    page["path"] = json!(path_text(path));
    page["sha256"] = json!(actual_hash);
    page["bytes"] = json!(bytes_read);
    page["encoding"] = json!(encoding.unwrap_or("utf-8"));
    page["offsetUnit"] = json!("utf16-code-units");
    Ok(page)
}

struct ReadWindow {
    start_line: usize, end_line: usize, requested_offset: Option<usize>, maximum: usize,
    offset: usize, line: usize, range_start: Option<usize>, range_end: Option<usize>,
    content: String, content_units: usize, page_closed: bool, next_line: usize,
}
impl ReadWindow {
    fn observe(&mut self, character: char) -> ServiceResult<()> {
        let length = character.len_utf16();
        if let Some(requested) = self.requested_offset {
            if self.offset < requested && requested < self.offset + length {
                return Err(invalid("offset 不能位于字符代理对中间，请使用上一页的 nextOffset。"));
            }
        }
        if let Some(start) = self.range_start {
            let requested = self.requested_offset.unwrap_or(start);
            if self.line <= self.end_line && self.offset >= requested && !self.page_closed {
                if self.content_units + length <= self.maximum || self.content_units == 0 {
                    self.content.push(character);
                    self.content_units += length;
                    self.next_line = self.line + usize::from(character == '\n');
                } else { self.page_closed = true; }
            }
        }
        self.offset += length;
        if character == '\n' {
            self.line += 1;
            if self.line == self.start_line { self.range_start = Some(self.offset); }
            if self.line == self.end_line + 1 { self.range_end = Some(self.offset); }
        }
        Ok(())
    }
    fn finish(self, ranged: bool) -> ServiceResult<Value> {
        let start = self.range_start.ok_or_else(|| invalid("startLine 超出当前文件行数，请重新读取文件状态。"))?;
        let end = self.range_end.unwrap_or(self.offset);
        let offset = self.requested_offset.unwrap_or(start);
        if offset < start || offset > end {
            return Err(invalid("offset 不在指定范围中，请使用该范围返回的 nextOffset。"));
        }
        let next = offset + self.content_units;
        let more = next < end;
        let mut page = json!({"content":self.content,"offset":offset,"nextOffset":next,
            "totalCharacters":self.offset,"hasMore":more,"truncated":offset > 0 || next < self.offset});
        if ranged {
            page["lineRangeHasMore"] = json!(more);
            page["hasMoreInFile"] = json!(next < self.offset);
            page["nextLine"] = if more { json!(self.next_line) } else { Value::Null };
            page["lineRange"] = json!({"startLine":self.start_line,"endLine":self.end_line.min(self.line),
                "totalLines":self.line,"startOffset":start,"endOffset":end});
        }
        Ok(page)
    }
}

fn atomic_write(
    path: &Path,
    bytes: &[u8],
    expected: &Value,
    binding: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
    maximum_existing_bytes: u64,
) -> ServiceResult<Value> {
    check_hash_bounded(path, expected, maximum_existing_bytes)?;
    let parent = path.parent().ok_or_else(|| invalid("文件父目录不存在。"))?;
    if !inspect(parent, false, false)?.is_some_and(|metadata| metadata.is_dir()) {
        return Err(invalid("文件父目录不存在。"));
    }
    let temporary = parent.join(format!(".kynxa-write-{}.tmp", random_identity()?));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mode = fs::metadata(path)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0o600);
            options.mode(mode);
        }
        let mut file = options.open(&temporary).map_err(ServiceError::io)?;
        file.write_all(bytes).map_err(ServiceError::io)?;
        file.sync_all().map_err(ServiceError::io)?;
        drop(file);
        inspect(parent, false, false)?;
        check_hash_bounded(path, expected, maximum_existing_bytes)?;
        revalidate(binding)?;
        cancelled(cancellation)?;
        // Once the atomic replacement is dispatched, return its actual commit receipt despite later cancellation.
        // 原子替换派发后，即使随后取消也返回真实提交回执。
        fs::rename(&temporary, path).map_err(ServiceError::io)?;
        Ok(json!({"path":path_text(path),"bytes":bytes.len(),"sha256":sha256(bytes)}))
    })();
    if let Err(error) = fs::remove_file(&temporary) {
        if error.kind() != std::io::ErrorKind::NotFound && result.is_ok() {
            return Err(ServiceError::io(error));
        }
    }
    result
}

fn file_lock(path: &Path) -> Arc<Mutex<()>> {
    let key = path_key(path);
    let mut locks = FILE_LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = locks.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| Arc::new(Mutex::new(())));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Commit generated document bytes through the same per-file transaction as text writes.
/// 生成文档字节复用文本写入的逐文件事务；独立额度不扩大公开文本工具的 1 MiB 合同。
pub(super) fn commit_binary(
    path: &Path,
    bytes: &[u8],
    expected: &Value,
    binding: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
) -> ServiceResult<Value> {
    if bytes.len() as u64 > MAX_BINARY_FILE_BYTES {
        return Err(ServiceError::new("TOOL_FILE_TOO_LARGE", "生成文档超过 64 MiB 上限。", 400));
    }
    if binding["path"].as_str() != Some(path_text(path).as_str()) {
        return Err(ServiceError::new("TOOL_PATH_CHANGED", "生成文档与已授权目标不一致。", 409));
    }
    let lock = file_lock(path);
    let _guard = lock.lock().unwrap();
    cancelled(cancellation)?;
    revalidate(binding)?;
    atomic_write(path, bytes, expected, binding, cancellation, MAX_BINARY_FILE_BYTES)
}

/// Export immutable attachment bytes without replacing a different existing file.
/// 附件原件使用独立的 256 MiB 合同；原子发布不能覆盖其他内容的现有文件。
pub(super) fn commit_binary_new(
    path: &Path,
    bytes: &[u8],
    binding: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
    maximum_bytes: u64,
) -> ServiceResult<Value> {
    if bytes.len() as u64 > maximum_bytes {
        return Err(ServiceError::new("TOOL_FILE_TOO_LARGE", "附件超过允许的大小上限。", 400));
    }
    if binding["path"].as_str() != Some(path_text(path).as_str()) {
        return Err(ServiceError::new("TOOL_PATH_CHANGED", "附件导出目标与已授权路径不一致。", 409));
    }
    let lock = file_lock(path);
    let _guard = lock.lock().unwrap();
    let digest = sha256(bytes);
    let receipt = |reused: bool| json!({"path":path_text(path),"bytes":bytes.len(),"sha256":digest,
        "executed":!reused,"reused":reused,"created":!reused});
    let reuse_existing = || -> ServiceResult<Option<Value>> {
        cancelled(cancellation)?;
        revalidate(binding)?;
        let Some(metadata) = inspect(path, true, false)? else { return Ok(None); };
        if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
            return Err(conflict("导出目标已存在且内容不同，请选择其他文件名。"));
        }
        let existing = read_checked(path, maximum_bytes, false, "TOOL_FILE_CONFLICT")?;
        if existing != bytes || sha256(&existing) != digest {
            return Err(conflict("导出目标已存在且内容不同，请选择其他文件名。"));
        }
        revalidate(binding)?;
        cancelled(cancellation)?;
        Ok(Some(receipt(true)))
    };
    if let Some(existing) = reuse_existing()? { return Ok(existing); }
    let parent = path.parent().ok_or_else(|| invalid("文件父目录不存在。"))?;
    if !inspect(parent, false, false)?.is_some_and(|metadata| metadata.is_dir()) {
        return Err(invalid("文件父目录不存在。"));
    }
    let temporary = parent.join(format!(".kynxa-write-{}.tmp", random_identity()?));
    let mut created = false;
    let mut result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        cancelled(cancellation)?;
        let mut file = options.open(&temporary).map_err(ServiceError::io)?;
        created = true;
        for chunk in bytes.chunks(1024 * 1024) {
            cancelled(cancellation)?;
            file.write_all(chunk).map_err(ServiceError::io)?;
        }
        file.sync_all().map_err(ServiceError::io)?;
        let staged_version = handle_version(&file, &file.metadata().map_err(ServiceError::io)?)?;
        drop(file);
        let staged_metadata = inspect(&temporary, false, false)?
            .ok_or_else(|| conflict("附件导出暂存文件已变化。"))?;
        let current_version = file_version(&temporary, &staged_metadata)?;
        if (current_version.volume, current_version.identity, current_version.size, current_version.links)
            != (staged_version.volume, staged_version.identity, staged_version.size, staged_version.links) {
            return Err(conflict("附件导出暂存文件已变化。"));
        }
        inspect(parent, false, false)?;
        revalidate(binding)?;
        cancelled(cancellation)?;
        // Publish the fsynced sibling with the existing no-clobber hard-link pattern.
        // 复用现有硬链接发布方式，目标即使在最后核验后出现也不会被覆盖。
        match fs::hard_link(&temporary, path) {
            Ok(()) => Ok(receipt(false)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                reuse_existing()?.ok_or_else(|| conflict("导出目标在提交期间发生变化，请重试。"))
            }
            Err(error) => Err(ServiceError::io(error)),
        }
    })();
    if created {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                if let Ok(committed) = &mut result {
                    // An actual publication remains successful even if its temporary name needs cleanup.
                    // 已实际发布的文件仍返回成功回执，不能因暂存名称清理失败诱发重复执行。
                    committed["temporaryCleanupPending"] = json!(true);
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod read_window_tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("kynxa-file-read-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap(); Self(root)
        }
        fn path(&self) -> PathBuf { self.0.join("sample.log") }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.0.starts_with(std::env::temp_dir()) { let _ = fs::remove_dir_all(&self.0); }
        }
    }

    #[test]
    fn large_search_match_reads_current_line_without_raising_write_limits() {
        let fixture = Fixture::new(); let path = fixture.path();
        let mut text = "ordinary log line\n".repeat(128 * 1024);
        text.push_str("unique-needle 🔬\nlast line\n");
        assert!(text.len() > 2 * 1024 * 1024);
        fs::write(&path,&text).unwrap();
        let mut search = super::super::service_search::SearchService::default();
        search.apply("search.init",&json!({"path":path,"input":{"query":"unique-needle"}}),&None).unwrap();
        let mut step = search.apply("search.pageStart",&json!({"maxMatches":10}),&None).unwrap();
        while step["pageReady"] != true {
            assert!(step["authorization"].is_object());
            step = search.apply("search.step",&json!({"allowed":true}),&None).unwrap();
        }
        let result = search.apply("search.pageFinish",&json!({}),&None).unwrap();
        let found = &result["matches"][0];
        assert!(found.is_object(),"{result}");
        let line = found["line"].as_u64().unwrap();
        let page = read_page(&path,&json!({"startLine":line,"endLine":line,"expectedSha256":found["sha256"]}),&None).unwrap();
        assert_eq!(page["content"],"unique-needle 🔬\n");
        assert_eq!(page["sha256"],sha256(text.as_bytes()));
        assert_eq!(page["lineRange"]["startLine"],line);
        assert_eq!(page["hasMore"],false); assert_eq!(page["hasMoreInFile"],true);
        assert_eq!(read_page(&path,&json!({"maxBytes":MAX_FILE_BYTES}),&None).unwrap_err().code,"TOOL_FILE_TOO_LARGE");
        assert_eq!(apply("file.write",&json!({"path":path,"input":{"content":"small","expectedHash":page["sha256"]}}),&None).unwrap_err().code,"TOOL_FILE_TOO_LARGE");
        fs::write(&path,"changed\n").unwrap();
        assert_eq!(read_page(&path,&json!({"expectedSha256":page["sha256"]}),&None).unwrap_err().code,"TOOL_FILE_CONFLICT");
    }

    #[test]
    fn streaming_utf8_and_utf16_keep_surrogates_and_line_offsets_across_blocks() {
        let fixture = Fixture::new(); let path = fixture.path();
        for (prefix, utf16) in [(65535,false),(32766,true)] {
            let text = format!("{}🔬\r\n目标行 🔬\n尾部", "x".repeat(prefix));
            let bytes = if utf16 {
                [vec![0xff,0xfe],text.encode_utf16().flat_map(u16::to_le_bytes).collect()].concat()
            } else { text.as_bytes().to_vec() };
            fs::write(&path,&bytes).unwrap();
            let first = read_page(&path,&json!({"offset":prefix,"maxChars":1}),&None).unwrap();
            assert_eq!(first["content"],"🔬"); assert_eq!(first["nextOffset"],prefix+2);
            assert_eq!(first["sha256"],sha256(&bytes));
            assert_eq!(read_page(&path,&json!({"offset":prefix+1}),&None).unwrap_err().code,"INVALID_TOOL_ARGUMENTS");
            let line = read_page(&path,&json!({"startLine":2,"endLine":2,"maxChars":3}),&None).unwrap();
            assert_eq!(line["content"],"目标行"); assert_eq!(line["nextLine"],2);
            let rest = read_page(&path,&json!({"startLine":2,"endLine":2,"offset":line["nextOffset"],
                "expectedSha256":line["sha256"]}),&None).unwrap();
            assert_eq!(rest["content"]," 🔬\n"); assert_eq!(rest["hasMore"],false);
            assert_eq!(rest["lineRange"]["startOffset"],prefix+4);
            assert_eq!(rest["totalCharacters"],text.encode_utf16().count());
        }
    }

    #[test]
    fn long_lines_keep_output_bounded_and_cancelled_reads_do_not_change_files() {
        let fixture = Fixture::new(); let path = fixture.path();
        let text = "文".repeat(1024 * 1024); fs::write(&path,&text).unwrap();
        let page = read_page(&path,&json!({"startLine":1,"endLine":1,"maxChars":64000}),&None).unwrap();
        assert_eq!(page["content"].as_str().unwrap().encode_utf16().count(),64000);
        assert_eq!(page["nextOffset"],64000); assert_eq!(page["hasMore"],true);
        let cancelled = Some(Arc::new(AtomicBool::new(true)));
        assert_eq!(read_page(&path,&json!({}),&cancelled).unwrap_err().code,"TOOL_CANCELLED");
        assert_eq!(fs::metadata(&path).unwrap().len(),text.len() as u64);
    }
}

#[cfg(test)]
mod binary_commit_tests {
    use super::*;

    #[test]
    fn attachment_export_reuses_exact_bytes_and_preserves_different_existing_content() {
        let root = std::env::temp_dir().join(format!("kynxa-attachment-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let root = canonical(&root, false).unwrap();
        let path = root.join("original.bin");
        let binding = json!({"requestedPath":path_text(&path),"path":path_text(&path),"allowMissing":true,"allowHardLinks":false});
        let original = b"\0original attachment\xff";
        let maximum = original.len() as u64;
        let created = commit_binary_new(&path, original, &binding, &None, maximum).unwrap();
        assert_eq!(created["executed"], true);
        assert_eq!(created["reused"], false);
        assert_eq!(created["sha256"], sha256(original));
        let version = file_version(&path, &fs::metadata(&path).unwrap()).unwrap();
        let reused = commit_binary_new(&path, original, &binding, &None, maximum).unwrap();
        assert_eq!(reused["reused"], true);
        assert_eq!(reused["executed"], false);
        assert_eq!(file_version(&path, &fs::metadata(&path).unwrap()).unwrap(), version);
        let mut different = original.to_vec();
        different[1] = b'X';
        assert_eq!(commit_binary_new(&path, &different, &binding, &None, maximum).unwrap_err().code, "TOOL_FILE_CONFLICT");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cancelled_attachment_export_creates_neither_destination_nor_temporary_file() {
        let root = std::env::temp_dir().join(format!("kynxa-attachment-cancel-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let root = canonical(&root, false).unwrap();
        let path = root.join("cancelled.bin");
        let binding = json!({"requestedPath":path_text(&path),"path":path_text(&path),"allowMissing":true,"allowHardLinks":false});
        let cancellation = Some(Arc::new(AtomicBool::new(true)));
        assert_eq!(commit_binary_new(&path, b"original", &binding, &cancellation, b"original".len() as u64).unwrap_err().code, "TOOL_CANCELLED");
        assert!(!path.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn document_binary_commit_preserves_versions_cancellation_and_text_limits() {
        let root = std::env::temp_dir().join(format!("kynxa-document-commit-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let root = canonical(&root, false).unwrap();
        let path = root.join("document.docx");
        let binding = json!({"requestedPath":path_text(&path),"path":path_text(&path),"allowMissing":true,"allowHardLinks":false});
        let first = vec![1; 2 * 1024 * 1024];
        let created = commit_binary(&path, &first, &Value::Null, &binding, &None).unwrap();
        assert_eq!(created["bytes"], first.len());
        assert_eq!(created["sha256"], sha256(&first));
        assert_eq!(fs::read(&path).unwrap(), first);
        let metadata = apply("file.stat", &json!({"path":path_text(&path),"pathBinding":binding}), &None).unwrap();
        assert_eq!(metadata["sha256"], created["sha256"]);
        let second = vec![2; first.len() + 1024];
        let updated = commit_binary(&path, &second, &created["sha256"], &binding, &None).unwrap();
        assert_eq!(updated["sha256"], sha256(&second));
        assert_eq!(commit_binary(&path, &first, &created["sha256"], &binding, &None).unwrap_err().code, "TOOL_FILE_CONFLICT");
        let cancellation = Some(Arc::new(AtomicBool::new(true)));
        assert_eq!(commit_binary(&path, &first, &updated["sha256"], &binding, &cancellation).unwrap_err().code, "TOOL_CANCELLED");
        assert_eq!(fs::read(&path).unwrap(), second);
        assert_eq!(apply("file.write", &json!({"path":path_text(&path),"pathBinding":binding,
            "input":{"content":"text","expectedHash":updated["sha256"]}}), &None).unwrap_err().code, "TOOL_FILE_TOO_LARGE");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(&root).unwrap();
    }
}

pub fn apply(
    operation: &str,
    arguments: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
) -> ServiceResult<Value> {
    cancelled(cancellation)?;
    if operation == "file.listFinish" {
        let mut result = arguments["result"].clone();
        let limit = bounded(&result["limit"], 200, 1, 500)? as usize;
        let entries = result["entries"]
            .as_array_mut()
            .ok_or_else(|| invalid("目录列表无效。"))?;
        let truncated = entries.len() > limit;
        entries.truncate(limit);
        result["truncated"] = json!(truncated);
        result.as_object_mut().unwrap().remove("limit");
        return Ok(result);
    }
    let path = absolute_path(string(&arguments["path"]));
    let input = &arguments["input"];
    let binding = &arguments["pathBinding"];
    revalidate(binding)?;
    if operation == "file.read" {
        return read_page(&path, input, cancellation);
    }
    if operation == "file.stat" {
        let metadata = inspect(&path, false, true)?.ok_or_else(|| invalid("路径不是普通文件。"))?;
        // Hash generated Office files too, so their next replacement can obtain an exact expectedHash.
        // 元信息也覆盖生成的 Office 文件，保证下次替换可取得准确 expectedHash；文本读取额度不变。
        let hash = if metadata.is_file() && metadata.len() <= MAX_BINARY_FILE_BYTES {
            json!(sha256(&read_bounded(&path, MAX_BINARY_FILE_BYTES)?))
        } else {
            Value::Null
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        return Ok(
            json!({"path":path_text(&path),"type":if metadata.is_dir(){"directory"}else if metadata.is_file(){"file"}else{"other"},
            "bytes":metadata.len(),"modifiedAtMs":modified,"sha256":hash}),
        );
    }
    if operation == "file.list" {
        if !inspect(&path, false, false)?.is_some_and(|metadata| metadata.is_dir()) {
            return Err(invalid("路径不是目录。"));
        }
        bounded(&input["limit"], 200, 1, 500)?;
        let entries = fs::read_dir(&path).map_err(ServiceError::io)?.map(|entry| {
            cancelled(cancellation)?;
            let entry = entry.map_err(ServiceError::io)?; let kind = entry.file_type().map_err(ServiceError::io)?;
            Ok(json!({"name":entry.file_name().to_string_lossy(),"type":if kind.is_symlink(){"link"}else if kind.is_dir(){"directory"}else if kind.is_file(){"file"}else{"other"}}))
        }).collect::<ServiceResult<Vec<_>>>()?;
        return Ok(
            json!({"path":path_text(&path),"entries":entries,"limit":bounded(&input["limit"],200,1,500)?}),
        );
    }
    let lock = file_lock(&path);
    let _guard = lock.lock().unwrap();
    cancelled(cancellation)?;
    revalidate(binding)?;
    match operation {
        "file.write" | "file.edit" => {
            let content = if operation == "file.edit" {
                let original = check_hash(&path, &input["expectedHash"])?
                    .ok_or_else(|| invalid("路径不是普通文件。"))?;
                let full = decode_text(&original)?;
                let old = input["oldText"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid("请提供有效的替换文本。"))?;
                let new = input["newText"]
                    .as_str()
                    .ok_or_else(|| invalid("请提供有效的替换文本。"))?;
                let index = full.find(old).ok_or_else(|| {
                    ServiceError::new(
                        "TOOL_EDIT_NOT_UNIQUE",
                        "替换文本必须在文件中恰好出现一次。",
                        409,
                    )
                })?;
                let next_character = index + full[index..].chars().next().unwrap().len_utf8();
                if full[next_character..].contains(old) {
                    return Err(ServiceError::new(
                        "TOOL_EDIT_NOT_UNIQUE",
                        "替换文本必须在文件中恰好出现一次。",
                        409,
                    ));
                }
                format!("{}{new}{}", &full[..index], &full[index + old.len()..])
            } else {
                input["content"]
                    .as_str()
                    .ok_or_else(|| invalid("请提供有效的 UTF-8 文本。"))?
                    .to_owned()
            };
            if content.contains('\0') || arguments["invalidText"] == true {
                return Err(invalid("请提供有效的 UTF-8 文本。"));
            }
            if content.len() as u64 > MAX_FILE_BYTES {
                return Err(ServiceError::new(
                    "TOOL_FILE_TOO_LARGE",
                    "文件内容超过工具的 1 MiB 上限。",
                    400,
                ));
            }
            atomic_write(
                &path,
                content.as_bytes(),
                &input["expectedHash"],
                binding,
                cancellation,
                MAX_FILE_BYTES,
            )
        }
        "file.delete" => {
            if let Some(requested) = binding["requestedPath"].as_str() {
                if is_link(&fs::symlink_metadata(requested).map_err(ServiceError::io)?) {
                    return Err(ServiceError::new(
                        "UNSAFE_TOOL_PATH",
                        "此路径是链接入口，本次没有删除链接或其真实目标。",
                        403,
                    ));
                }
            }
            let workspace = binding
                .get("workspaceRoot")
                .filter(|value| !value.is_null())
                .unwrap_or(&arguments["workspaceRoot"]);
            if path.parent().is_none()
                || !workspace.is_null()
                    && path_key(&absolute_path(string(workspace))) == path_key(&path)
            {
                return Err(ServiceError::new(
                    "UNSAFE_TOOL_PATH",
                    "不能删除工作或文件系统根目录。",
                    403,
                ));
            }
            let metadata =
                inspect(&path, false, false)?.ok_or_else(|| invalid("路径不是普通文件。"))?;
            if metadata.is_dir() {
                if !input["expectedHash"].is_null() {
                    return Err(invalid("删除空目录时 expectedHash 必须为 null。"));
                }
                cancelled(cancellation)?;
                fs::remove_dir(&path).map_err(ServiceError::io)?;
            } else {
                check_hash(&path, &input["expectedHash"])?;
                cancelled(cancellation)?;
                fs::remove_file(&path).map_err(ServiceError::io)?;
            }
            Ok(json!({"path":path_text(&path),"deleted":true}))
        }
        "file.mkdir" => {
            if let Some(metadata) = inspect(&path, true, false)? {
                if !metadata.is_dir() {
                    return Err(conflict("此路径已存在其他文件。"));
                }
                return Ok(json!({"path":path_text(&path),"created":false}));
            }
            if !inspect(
                path.parent().ok_or_else(|| invalid("父目录不存在。"))?,
                false,
                false,
            )?
            .is_some_and(|metadata| metadata.is_dir())
            {
                return Err(invalid("父目录不存在。"));
            }
            cancelled(cancellation)?;
            fs::create_dir(&path).map_err(ServiceError::io)?;
            Ok(json!({"path":path_text(&path),"created":true}))
        }
        _ => Err(ServiceError::new("TOOL_NOT_FOUND", "未知文件工具。", 404)),
    }
}
