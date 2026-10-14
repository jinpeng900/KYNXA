//! Bounded archive expansion publishes ordinary sources through the existing snapshot/index pipeline.
//! 有界展开压缩包成员，经现有快照与索引链路发布，不执行成员或创建归档声明的路径。

use crate::{
    document_formats::{self, ParserFamily},
    gateway_contract::GatewayConfig,
    storage::{ContractError, gateway},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{Cursor, Read},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const VERSION: &str = "rust-archive-members-v1";
const MIB: u64 = 1024 * 1024;

fn failure(code: &'static str) -> ContractError {
    gateway::error(
        "压缩包成员解析未完成，原文件已保留。",
        code,
        if code.contains("LIMIT") { 413 } else { 422 },
    )
}
fn check_cancel(cancel: &Arc<AtomicBool>) -> Result<(), ContractError> {
    if cancel.load(Ordering::Acquire) {
        Err(failure("ABORT_ERR"))
    } else {
        crate::storage::sqlite_store::cancelled()
    }
}

#[derive(Clone)]
struct Limits {
    member_bytes: u64,
    expanded_bytes: u64,
    entries: u64,
    depth: usize,
    ratio: u64,
    timeout: Duration,
}
impl Limits {
    fn from_settings(settings: &Value) -> Self {
        Self {
            member_bytes: settings["maximumDocumentInputBytes"]
                .as_u64()
                .unwrap_or(32 * MIB)
                .min(settings["maximumSourceBytes"].as_u64().unwrap_or(32 * MIB))
                .clamp(1, 32 * MIB),
            expanded_bytes: settings["maximumTotalBytes"]
                .as_u64()
                .unwrap_or(512 * MIB)
                .clamp(1, 2 * 1024 * MIB),
            entries: settings["maximumFiles"]
                .as_u64()
                .unwrap_or(20000)
                .clamp(1, 100000),
            depth: settings["maximumArchiveDepth"]
                .as_u64()
                .unwrap_or(4)
                .clamp(1, 8) as usize,
            ratio: settings["maximumArchiveExpansionRatio"]
                .as_u64()
                .unwrap_or(1000)
                .clamp(1, 10000),
            timeout: Duration::from_millis(
                settings["documentDecoderTimeoutMs"]
                    .as_u64()
                    .unwrap_or(120000)
                    .clamp(1000, 300000),
            ),
        }
    }
}
struct Expansion<'a, F> {
    config: &'a GatewayConfig,
    path: &'a Path,
    settings: &'a Value,
    limits: Limits,
    cancel: &'a Arc<AtomicBool>,
    consume: &'a mut F,
    metadata: Value,
    original_hash: String,
    original_bytes: u64,
    expanded_bytes: u64,
    entries: u64,
    published: u64,
    text_bytes: u64,
    failures: Vec<Value>,
    failed: u64,
    publication_failed: bool,
    deadline: Instant,
}
impl<F: FnMut(Value) -> Result<(), ContractError>> Expansion<'_, F> {
    fn check(&self) -> Result<(), ContractError> {
        check_cancel(self.cancel)?;
        if Instant::now() >= self.deadline {
            Err(failure("ARCHIVE_TIMEOUT"))
        } else {
            Ok(())
        }
    }
    fn reject(&mut self, member: &str, code: &str) {
        self.failed += 1;
        if self.failures.len() < 1000 {
            self.failures.push(json!({"relativePath":member,"errorCode":code,"rawContentHash":self.original_hash,"archivePath":self.path}));
        }
    }
    fn observe(&mut self) -> Result<(), ContractError> {
        self.check()?;
        self.entries += 1;
        if self.entries > self.limits.entries {
            return Err(failure("ARCHIVE_ENTRY_LIMIT"));
        }
        Ok(())
    }
    fn charge(&mut self, count: usize) -> Result<(), ContractError> {
        self.check()?;
        let next = self.expanded_bytes.saturating_add(count as u64);
        if next > self.limits.expanded_bytes
            || next
                > self
                    .original_bytes
                    .saturating_mul(self.limits.ratio)
                    .max(MIB)
        {
            return Err(failure("ARCHIVE_EXPANDED_LIMIT"));
        }
        self.expanded_bytes = next;
        Ok(())
    }
    fn read_member_bytes(
        &mut self,
        reader: &mut dyn Read,
        size: Option<u64>,
    ) -> Result<Vec<u8>, ContractError> {
        if size.is_some_and(|size| size > self.limits.member_bytes) {
            return Err(failure("ARCHIVE_MEMBER_LIMIT"));
        }
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 65536];
        loop {
            self.check()?;
            let read = reader
                .read(&mut buffer)
                .map_err(|_| failure("ARCHIVE_MEMBER_CORRUPT"))?;
            if read == 0 {
                break;
            }
            if bytes.len().saturating_add(read) as u64 > self.limits.member_bytes {
                return Err(failure("ARCHIVE_MEMBER_LIMIT"));
            }
            self.charge(read)?;
            bytes.extend_from_slice(&buffer[..read]);
        }
        if size.is_some_and(|size| size != bytes.len() as u64) {
            return Err(failure("ARCHIVE_MEMBER_CORRUPT"));
        }
        Ok(bytes)
    }
    fn member(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        chain: &[Value],
        depth: usize,
    ) -> Result<(), ContractError> {
        self.check()?;
        let mut next = chain.to_vec();
        next.push(json!({"member":name,"rawContentHash":super::gateway_index::hash_bytes(&bytes)}));
        let logical = next
            .iter()
            .filter_map(|part| part["member"].as_str())
            .collect::<Vec<_>>()
            .join("!/");
        if is_archive(Path::new(name)) {
            if depth >= self.limits.depth {
                self.reject(&logical, "ARCHIVE_NESTING_LIMIT");
                return Ok(());
            }
            if let Err(error) = self.expand(&bytes, name, &next, depth + 1) {
                if error.code() == "ABORT_ERR" || self.publication_failed {
                    return Err(error);
                }
                self.reject(&logical, error.code());
            }
            return Ok(());
        }
        let parsed = super::gateway_documents::parse_bytes(
            Some(self.config),
            Path::new(name),
            &bytes,
            self.settings,
            self.cancel,
        );
        let parsed = match parsed {
            Ok(value) => value,
            Err(error) => {
                if error.code() == "ABORT_ERR" {
                    return Err(error);
                }
                self.reject(&logical, error.code());
                return Ok(());
            }
        };
        let windows = parsed["documentWindows"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![parsed]);
        for (index, value) in windows.into_iter().enumerate() {
            let Some(text) = value["text"]
                .as_str()
                .filter(|text| !text.trim().is_empty())
            else {
                self.reject(&logical, "ARCHIVE_TEXT_UNAVAILABLE");
                continue;
            };
            if self.text_bytes.saturating_add(text.len() as u64) > self.limits.expanded_bytes {
                return Err(failure("ARCHIVE_TEXT_LIMIT"));
            }
            let digest = super::gateway_index::hash(text);
            let part = if value["extraction"]["pageWindow"].is_object() {
                value["extraction"]["pageWindow"].clone()
            } else if value["extraction"]["textRange"].is_object() {
                value["extraction"]["textRange"].clone()
            } else if value["textRange"].is_object() {
                value["textRange"].clone()
            } else {
                json!({"index":index})
            };
            let relative = format!(
                "{}!/{}",
                self.path.file_name().unwrap_or_default().to_string_lossy(),
                logical
            );
            let mut locator = json!({"path":self.path,"relativePath":relative,"root":self.path.parent(),
                "archive":{"version":VERSION,"archivePath":self.path,"rawContentHash":self.original_hash,"chain":next,"part":part,"partIndex":index}});
            if let Some(extraction) = value.get("extraction") {
                locator["extraction"] = extraction.clone();
            }
            let revision = super::gateway_index::hash(
                &json!([digest, locator["archive"], locator["extraction"]]).to_string(),
            );
            if let Err(error) = (self.consume)(
                json!({"title":format!("{}: {}",self.path.file_name().unwrap_or_default().to_string_lossy(),logical),
                "text":text,"contentHash":digest,"sourceRevision":revision,"fileMetadata":self.metadata,"locator":locator}),
            ) {
                self.publication_failed = true;
                return Err(error);
            }
            self.published += 1;
            self.text_bytes += text.len() as u64;
        }
        Ok(())
    }
    fn tar(
        &mut self,
        reader: impl Read,
        chain: &[Value],
        depth: usize,
    ) -> Result<(), ContractError> {
        let reader = CappedStream {
            inner: reader,
            remaining: self
                .limits
                .expanded_bytes
                .saturating_sub(self.expanded_bytes)
                .min(
                    self.original_bytes
                        .saturating_mul(self.limits.ratio)
                        .max(MIB),
                ),
            cancel: self.cancel.clone(),
            deadline: self.deadline,
        };
        let mut archive = tar::Archive::new(reader);
        let mut names = HashSet::new();
        for entry in archive
            .entries()
            .map_err(|_| failure("ARCHIVE_INVALID_INPUT"))?
        {
            self.observe()?;
            let mut entry = entry.map_err(|error| {
                if error.to_string().contains("ARCHIVE_EXPANDED_LIMIT") {
                    failure("ARCHIVE_EXPANDED_LIMIT")
                } else {
                    failure("ARCHIVE_MEMBER_CORRUPT")
                }
            })?;
            let raw = entry
                .path()
                .map_err(|_| failure("ARCHIVE_UNSAFE_MEMBER"))?
                .to_string_lossy()
                .into_owned();
            let name = match member_name(&raw) {
                Ok(name) => name,
                Err(error) => {
                    self.reject(&raw, error.code());
                    continue;
                }
            };
            let kind = entry.header().entry_type();
            if kind.is_dir() {
                continue;
            }
            if !kind.is_file() {
                self.reject(&name, "ARCHIVE_LINK_MEMBER");
                continue;
            }
            if !names.insert(name.to_lowercase()) {
                self.reject(&name, "ARCHIVE_DUPLICATE_MEMBER");
                continue;
            }
            let size = entry.size();
            if size > self.limits.member_bytes {
                return Err(failure("ARCHIVE_MEMBER_LIMIT"));
            }
            let bytes = self.read_member_bytes(&mut entry, Some(size))?;
            self.member(&name, bytes, chain, depth)?;
        }
        Ok(())
    }
    fn expand(
        &mut self,
        bytes: &[u8],
        name: &str,
        chain: &[Value],
        depth: usize,
    ) -> Result<(), ContractError> {
        self.check()?;
        let format = document_formats::detect(Path::new(name))
            .filter(|format| format.family == ParserFamily::Archive)
            .ok_or_else(|| failure("ARCHIVE_FORMAT_UNSUPPORTED"))?
            .name;
        match format {
            "zip" => {
                let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
                    .map_err(|_| failure("ARCHIVE_INVALID_INPUT"))?;
                let mut names = HashSet::new();
                for index in 0..archive.len() {
                    self.observe()?;
                    let mut entry = match archive.by_index(index) {
                        Ok(entry) => entry,
                        Err(zip::result::ZipError::UnsupportedArchive(message))
                            if message.to_lowercase().contains("password") =>
                        {
                            self.reject(&format!("entry-{index}"), "ARCHIVE_ENCRYPTED");
                            continue;
                        }
                        Err(_) => {
                            self.reject(&format!("entry-{index}"), "ARCHIVE_MEMBER_CORRUPT");
                            continue;
                        }
                    };
                    let raw = entry.name().to_owned();
                    let member = match member_name(&raw) {
                        Ok(name) => name,
                        Err(error) => {
                            self.reject(&raw, error.code());
                            continue;
                        }
                    };
                    if entry.is_dir() {
                        continue;
                    }
                    if entry
                        .unix_mode()
                        .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
                    {
                        self.reject(&member, "ARCHIVE_LINK_MEMBER");
                        continue;
                    }
                    if !names.insert(member.to_lowercase()) {
                        self.reject(&member, "ARCHIVE_DUPLICATE_MEMBER");
                        continue;
                    }
                    let size = entry.size();
                    if size > self.limits.member_bytes {
                        self.reject(&member, "ARCHIVE_MEMBER_LIMIT");
                        continue;
                    }
                    let bytes = self.read_member_bytes(&mut entry, Some(size))?;
                    self.member(&member, bytes, chain, depth)?;
                }
                Ok(())
            }
            "tar" => self.tar(Cursor::new(bytes), chain, depth),
            "tar.gz" => self.tar(
                flate2::read::MultiGzDecoder::new(Cursor::new(bytes)),
                chain,
                depth,
            ),
            "tar.bz2" => self.tar(
                bzip2::read::MultiBzDecoder::new(Cursor::new(bytes)),
                chain,
                depth,
            ),
            "gzip" | "bzip2" => {
                let member = decoded_name(name, format);
                self.observe()?;
                let bytes = if format == "gzip" {
                    self.read_member_bytes(
                        &mut flate2::read::MultiGzDecoder::new(Cursor::new(bytes)),
                        None,
                    )?
                } else {
                    self.read_member_bytes(
                        &mut bzip2::read::MultiBzDecoder::new(Cursor::new(bytes)),
                        None,
                    )?
                };
                self.member(&member, bytes, chain, depth)
            }
            _ => {
                let limits = self.limits.clone();
                let remaining = self
                    .limits
                    .expanded_bytes
                    .saturating_sub(self.expanded_bytes);
                let config = self.config.clone();
                let cancel = self.cancel.clone();
                decode_worker(
                    &config,
                    bytes,
                    name,
                    format,
                    &limits,
                    remaining,
                    &cancel,
                    |event| {
                        if event["kind"] == "failure" {
                            self.observe()?;
                            self.reject(
                                event["member"].as_str().unwrap_or("unknown"),
                                event["code"].as_str().unwrap_or("ARCHIVE_PARSE_FAILED"),
                            );
                            return Ok(());
                        }
                        self.observe()?;
                        let member = member_name(event["member"].as_str().unwrap_or(""))?;
                        use base64::Engine;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(event["bytesBase64"].as_str().unwrap_or(""))
                            .map_err(|_| failure("ARCHIVE_INVALID_RESULT"))?;
                        if bytes.len() as u64 > self.limits.member_bytes
                            || event["rawBytes"] != bytes.len() as u64
                        {
                            return Err(failure("ARCHIVE_INVALID_RESULT"));
                        }
                        self.charge(bytes.len())?;
                        self.member(&member, bytes, chain, depth)
                    },
                )
            }
        }
    }
}

/// Include skipped TAR records in the decoded byte/time budget, without allocating their declared body size.
/// TAR 中跳过的记录同样受展开字节与时间额度约束，不能按其声明正文大小分配内存。
struct CappedStream<R> {
    inner: R,
    remaining: u64,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}
impl<R: Read> Read for CappedStream<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "ARCHIVE_CANCELLED_OR_TIMEOUT",
            ));
        }
        if self.remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ARCHIVE_EXPANDED_LIMIT",
            ));
        }
        let count = buffer.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buffer[..count])?;
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

pub(crate) fn is_archive(path: &Path) -> bool {
    document_formats::detect(path).is_some_and(|format| format.family == ParserFamily::Archive)
}

/// Member names are logical identifiers only; reject both POSIX traversal and Windows device/ADS spellings.
/// 成员名仅作为逻辑身份，拒绝 POSIX 越界与 Windows 设备名、备用数据流写法。
fn member_name(raw: &str) -> Result<String, ContractError> {
    let normalized = raw.replace('\\', "/");
    if normalized.is_empty()
        || normalized.len() > 4096
        || normalized.starts_with('/')
        || normalized
            .chars()
            .any(|value| value.is_control() || value == ':')
    {
        return Err(failure("ARCHIVE_UNSAFE_MEMBER"));
    }
    let names = normalized
        .trim_end_matches('/')
        .split('/')
        .collect::<Vec<_>>();
    for name in &names {
        if name.is_empty() || [".", ".."].contains(name) || name.ends_with(['.', ' ']) {
            return Err(failure("ARCHIVE_UNSAFE_MEMBER"));
        }
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem[3..].parse::<u8>().is_ok_and(|n| (1..=9).contains(&n))
        {
            return Err(failure("ARCHIVE_UNSAFE_MEMBER"));
        }
    }
    Ok(names.join("/"))
}
fn decoded_name(name: &str, format: &str) -> String {
    let filename = Path::new(name)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    if format.starts_with("tar.") {
        return format!("{}.tar", filename.split('.').next().unwrap_or("archive"));
    }
    filename
        .rsplit_once('.')
        .map(|(prefix, _)| prefix)
        .filter(|name| !name.is_empty())
        .unwrap_or("content.txt")
        .to_string()
}

/// A failed member is recorded without rolling back successfully staged sibling documents.
/// 单个成员失败被记录，已成功读取的同级文档仍可进入现有发布链路。
pub(crate) fn scan(
    config: &GatewayConfig,
    path: &Path,
    settings: &Value,
    cancel: &Arc<AtomicBool>,
    consume: &mut impl FnMut(Value) -> Result<(), ContractError>,
) -> Result<Value, ContractError> {
    check_cancel(cancel)?;
    for excluded in [&config.data_root, &config.data_home, &config.extension_root] {
        if crate::tools::local_files::within(excluded, path) {
            return Err(gateway::error(
                "应用数据不能再次作为资料导入。",
                "PROTECTED_RETRIEVAL_SOURCE",
                403,
            ));
        }
    }
    let limits = Limits::from_settings(settings);
    let (bytes, metadata) = crate::tools::service_source_reading::read_binary_stable(
        path,
        limits.member_bytes,
        &Some(cancel.clone()),
    )
    .map_err(|_| failure("ARCHIVE_READ_FAILED"))?;
    scan_loaded(config, path, &bytes, metadata, settings, cancel, consume)
}

/// A URL's already-validated response uses identical member limits without inventing a local file permission.
/// 已核验的 URL 响应复用相同成员额度，不虚构本机文件访问授权。
pub(crate) fn scan_bytes(
    config: &GatewayConfig,
    name: &Path,
    bytes: &[u8],
    settings: &Value,
    cancel: &Arc<AtomicBool>,
    consume: &mut impl FnMut(Value) -> Result<(), ContractError>,
) -> Result<Value, ContractError> {
    scan_loaded(config, name, bytes, Value::Null, settings, cancel, consume)
}
fn scan_loaded(
    config: &GatewayConfig,
    path: &Path,
    bytes: &[u8],
    metadata: Value,
    settings: &Value,
    cancel: &Arc<AtomicBool>,
    consume: &mut impl FnMut(Value) -> Result<(), ContractError>,
) -> Result<Value, ContractError> {
    let limits = Limits::from_settings(settings);
    if bytes.len() as u64 > limits.member_bytes {
        return Err(failure("ARCHIVE_INPUT_LIMIT"));
    }
    let original_hash = super::gateway_index::hash_bytes(bytes);
    let deadline = Instant::now() + limits.timeout;
    let mut expansion = Expansion {
        config,
        path,
        settings,
        limits,
        cancel,
        consume,
        metadata,
        original_hash,
        original_bytes: bytes.len() as u64,
        expanded_bytes: 0,
        entries: 0,
        published: 0,
        text_bytes: 0,
        failures: Vec::new(),
        failed: 0,
        publication_failed: false,
        deadline,
    };
    if let Err(error) = expansion.expand(
        bytes,
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("archive"),
        &[],
        0,
    ) {
        if error.code() == "ABORT_ERR" || expansion.publication_failed {
            return Err(error);
        }
        expansion.reject(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("archive"),
            error.code(),
        );
    }
    Ok(
        json!({"scannedFiles":expansion.entries,"publishedFiles":expansion.published,"failedFiles":expansion.failed,
        "sourceBytes":bytes.len(),"textBytes":expansion.text_bytes,"expandedBytes":expansion.expanded_bytes,
        "incomplete":expansion.failed>0,"failures":expansion.failures,"archive":{"version":VERSION,"rawContentHash":expansion.original_hash,
            "maximumExpandedBytes":expansion.limits.expanded_bytes,"maximumEntryBytes":expansion.limits.member_bytes,"maximumEntries":expansion.limits.entries,"maximumDepth":expansion.limits.depth}}),
    )
}

/// Re-read a mounted archive under its existing scope and reject changed containers, members or parser windows.
/// 按原工作权限回读压缩包，并拒绝容器、成员或解析窗口变化后的旧引用。
pub(crate) fn read_member(
    config: &GatewayConfig,
    locator: &Value,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    let path = Path::new(
        locator["path"]
            .as_str()
            .ok_or_else(|| failure("ARCHIVE_INVALID_LOCATOR"))?,
    );
    let mut found = None;
    scan(config, path, limits, cancel, &mut |source| {
        if source["locator"]["archive"] == locator["archive"]
            && source["locator"]["extraction"] == locator["extraction"]
        {
            found = Some(
                json!({"text":source["text"],"extraction":source["locator"]["extraction"],"metadata":source["fileMetadata"]}),
            );
        }
        Ok(())
    })?;
    found.ok_or_else(|| {
        gateway::error(
            "压缩包或成员版本已变化，请重新检索。",
            "STALE_RETRIEVAL_SOURCE",
            409,
        )
    })
}

fn decode_worker(
    config: &GatewayConfig,
    bytes: &[u8],
    name: &str,
    format: &str,
    limits: &Limits,
    remaining: u64,
    cancel: &Arc<AtomicBool>,
    mut consume: impl FnMut(Value) -> Result<(), ContractError>,
) -> Result<(), ContractError> {
    use crate::tools::gateway_process;
    use base64::Engine;
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        process::{Child, ChildStdin, ChildStdout},
        sync::watch,
    };
    struct Worker {
        child: Child,
        _stdin: ChildStdin,
        output: BufReader<ChildStdout>,
        owner: gateway_process::OwnedProcessJob,
        lease: crate::resources::gateway::ResourceLease,
    }
    let runtime = super::gateway_embedding::runtime()?;
    let memory =
        256 * MIB + (bytes.len() as u64).saturating_mul(4) + limits.member_bytes.saturating_mul(3);
    let deadline = Instant::now() + limits.timeout;
    let mut worker=runtime.block_on(async {
        let service=crate::resources::gateway::global().ok_or_else(||failure("DOCUMENT_RESOURCE_FAILED"))?;
        let (sender,receiver)=watch::channel(false);let cancellation=cancel.clone();
        let monitor=tokio::spawn(async move {while !cancellation.load(Ordering::Acquire) {tokio::time::sleep(Duration::from_millis(25)).await;}let _=sender.send(true);});
        let grant=service.acquire(json!({"taskId":gateway::new_id(),"workspaceId":"local","kind":"background","cpuThreads":1,"memoryBytes":memory,"gpuMemoryBytes":0,"waitMs":20000,"ttlMs":30000}),receiver).await;
        monitor.abort();let mut lease=grant.map_err(|_|failure("DOCUMENT_RESOURCE_FAILED"))?;
        lease.retain_unknown_on_drop();
        let mut command=tokio::process::Command::new(&config.toolhost_exe);gateway_process::hide(&mut command);
        command.arg("--document-archive").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        let mut child=match command.spawn() {Ok(child)=>child,Err(_)=>{let _=lease.release_confirmed().await;return Err(failure("ARCHIVE_PARSER_UNAVAILABLE"));}};
        let started=async {
            let owner=gateway_process::OwnedProcessJob::attach(&child).map_err(|_|failure("ARCHIVE_PARSER_UNAVAILABLE"))?;
            let registered=service.request("registerExecutor",json!({"leaseId":lease.allocation["leaseId"],"processId":child.id()}),Duration::from_secs(5)).await.map_err(|_|failure("DOCUMENT_RESOURCE_FAILED"))?;
            if registered["status"]!="registered" {return Err(failure("DOCUMENT_RESOURCE_FAILED"));}
            let mut stdin=child.stdin.take().ok_or_else(||failure("ARCHIVE_INVALID_RESULT"))?;
            let stdout=child.stdout.take().ok_or_else(||failure("ARCHIVE_INVALID_RESULT"))?;
            let mut input=json!({"operation":"document_archive","format":format,"name":name,"bytesBase64":base64::engine::general_purpose::STANDARD.encode(bytes),
                "maximumEntryBytes":limits.member_bytes,"maximumExpandedBytes":remaining.max(1),"maximumEntries":limits.entries,"maximumRatio":limits.ratio}).to_string();input.push('\n');
            stdin.write_all(input.as_bytes()).await.map_err(|_|failure("ARCHIVE_PARSER_FAILED"))?;
            Ok((owner,stdin,stdout))
        }.await;
        match started {
            Ok((owner,stdin,stdout))=>Ok(Worker {child,_stdin:stdin,output:BufReader::new(stdout),owner,lease}),
            Err(error)=>{
                gateway_process::terminate(&mut child).await;
                if child.wait().await.is_ok() {let _=lease.release_confirmed().await;}
                Err(error)
            }
        }
    })?;
    let mut complete = false;
    let mut header = false;
    let result = (|| {
        loop {
            check_cancel(cancel)?;
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| failure("ARCHIVE_TIMEOUT"))?;
            let frame=runtime.block_on(async {
                let read=gateway_process::bounded_line(&mut worker.output,limits.member_bytes as usize*2+65536);
                let observe=async {loop {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    if cancel.load(Ordering::Acquire) {return Err::<(),ContractError>(failure("ABORT_ERR"));}
                    let sample=worker.owner.memory_usage().map_err(|_|failure("ARCHIVE_RESOURCE_UNKNOWN"))?;
                    if sample.private_bytes>memory {return Err(failure("ARCHIVE_MEMORY_LIMIT"));}
                }};
                tokio::select! {
                    value=read=>value.map_err(|_|failure("ARCHIVE_OUTPUT_LIMIT")),
                    _=tokio::time::sleep(remaining)=>Err(failure("ARCHIVE_TIMEOUT")),
                    value=observe=>Err(value.err().unwrap_or_else(||failure("ARCHIVE_RESOURCE_UNKNOWN"))),
                }
            })?;
            let Some(frame) = frame else {
                break;
            };
            let event: Value =
                serde_json::from_str(&frame).map_err(|_| failure("ARCHIVE_INVALID_RESULT"))?;
            match event["kind"].as_str() {
                Some("header")
                    if !header
                        && event["protocolVersion"] == 1
                        && event["boundary"] == "document-archive" =>
                {
                    header = true
                }
                Some("member" | "failure") if header && !complete => consume(event)?,
                Some("complete") if header && !complete => {
                    complete = true;
                    break;
                }
                Some("error") => {
                    return Err(failure(match event["code"].as_str().unwrap_or("") {
                        "ARCHIVE_ENCRYPTED" => "ARCHIVE_ENCRYPTED",
                        "ARCHIVE_VOLUME_REQUIRED" => "ARCHIVE_VOLUME_REQUIRED",
                        "ARCHIVE_EXPANDED_LIMIT" => "ARCHIVE_EXPANDED_LIMIT",
                        "ARCHIVE_ENTRY_LIMIT" => "ARCHIVE_ENTRY_LIMIT",
                        "ARCHIVE_CANCELLED" => "ABORT_ERR",
                        _ => "ARCHIVE_PARSE_FAILED",
                    }));
                }
                _ => return Err(failure("ARCHIVE_INVALID_RESULT")),
            }
        }
        if complete {
            Ok(())
        } else {
            Err(failure("ARCHIVE_INVALID_RESULT"))
        }
    })();
    let finished = runtime.block_on(async {
        if result.is_err() {
            worker
                .owner
                .terminate_and_wait()
                .await
                .map_err(|_| failure("ARCHIVE_CLEANUP_UNCONFIRMED"))?;
        }
        let exit = tokio::time::timeout(Duration::from_secs(5), worker.child.wait())
            .await
            .map_err(|_| failure("ARCHIVE_CLEANUP_UNCONFIRMED"))?
            .map_err(|_| failure("ARCHIVE_CLEANUP_UNCONFIRMED"))?;
        if worker
            .owner
            .has_running_processes()
            .map_err(|_| failure("ARCHIVE_CLEANUP_UNCONFIRMED"))?
        {
            worker
                .owner
                .terminate_and_wait()
                .await
                .map_err(|_| failure("ARCHIVE_CLEANUP_UNCONFIRMED"))?;
        }
        worker
            .lease
            .release_confirmed()
            .await
            .map_err(|_| failure("DOCUMENT_RESOURCE_FAILED"))?;
        if result.is_ok() && !exit.success() {
            Err(failure("ARCHIVE_PARSE_FAILED"))
        } else {
            Ok(())
        }
    });
    result.and(finished)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn configuration(root: &Path) -> GatewayConfig {
        GatewayConfig {
            data_root: root.join("Data"),
            data_home: root.join("Models"),
            extension_root: root.join("Extensions"),
            application_root: root.join("App"),
            port: 0,
            toolhost_exe: root.join("toolhost.exe"),
            vector_extension: root.join("vec.dll"),
            inference_library: root.join("ort.dll"),
        }
    }
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    fn tar_bytes() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, bytes) in [
            ("中文说明.md", "# 蓝色主题\n账户更新使用版本号".as_bytes()),
            ("api.txt", b"English concurrency state evidence".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder.append_data(&mut header, name, bytes).unwrap();
        }
        builder.into_inner().unwrap()
    }
    #[test]
    fn zip_tar_and_compressed_streams_publish_real_members_with_raw_identity() {
        let root =
            std::env::temp_dir().join(format!("kynxa-archive-fixture-{}", gateway::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let config = configuration(&root);
        let limits = crate::storage::settings::apply("storage.settings.defaults", &json!({}))
            .unwrap()["local"]["indexing"]
            .clone();
        let tar = tar_bytes();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar).unwrap();
        let mut bzip = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        bzip.write_all(&tar).unwrap();
        for (name, bytes) in [
            (
                "samples.zip",
                zip_bytes(&[
                    ("说明.md", "# 蓝色主题\n账户更新使用版本号".as_bytes()),
                    ("api.txt", b"English concurrency state evidence"),
                ]),
            ),
            ("samples.tar", tar),
            ("samples.tar.gz", gzip.finish().unwrap()),
            ("samples.tar.bz2", bzip.finish().unwrap()),
        ] {
            let path = root.join(name);
            std::fs::write(&path, &bytes).unwrap();
            let mut sources = Vec::new();
            let report = scan(
                &config,
                &path,
                &limits,
                &Arc::new(AtomicBool::new(false)),
                &mut |source| {
                    sources.push(source);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(report["failedFiles"], 0, "{name}: {report}");
            assert_eq!(sources.len(), 2, "{name}: {report}");
            assert!(
                sources
                    .iter()
                    .any(|source| source["text"].as_str().unwrap().contains("账户更新"))
            );
            assert_eq!(
                sources[0]["locator"]["archive"]["rawContentHash"],
                super::super::gateway_index::hash_bytes(&bytes)
            );
            let original = read_member(
                &config,
                &sources[0]["locator"],
                &limits,
                &Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            assert_eq!(original["text"], sources[0]["text"]);
            std::fs::write(&path, b"changed archive bytes").unwrap();
            assert_eq!(
                read_member(
                    &config,
                    &sources[0]["locator"],
                    &limits,
                    &Arc::new(AtomicBool::new(false))
                )
                .unwrap_err()
                .code(),
                "STALE_RETRIEVAL_SOURCE"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nested_archives_isolate_unsafe_members_and_propagate_real_publication_failures() {
        let root = std::env::temp_dir().join(format!("kynxa-archive-nested-{}", gateway::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let config = configuration(&root);
        let limits = crate::storage::settings::apply("storage.settings.defaults", &json!({}))
            .unwrap()["local"]["indexing"]
            .clone();
        let nested = zip_bytes(&[("inner.md", b"nested document evidence")]);
        let bytes = zip_bytes(&[
            ("../escape.md", b"must not appear"),
            ("safe.md", b"safe evidence"),
            ("nested.zip", &nested),
            ("SAFE.md", b"ambiguous duplicate"),
        ]);
        let path = root.join("nested.zip");
        std::fs::write(&path, bytes).unwrap();
        let mut sources = Vec::new();
        let report = scan(
            &config,
            &path,
            &limits,
            &Arc::new(AtomicBool::new(false)),
            &mut |source| {
                sources.push(source);
                Ok(())
            },
        )
        .unwrap();
        assert!(report["failedFiles"].as_u64().unwrap() >= 1);
        assert_eq!(sources.len(), 2);
        assert!(
            !sources
                .iter()
                .any(|source| source["text"].as_str().unwrap().contains("must not appear"))
        );
        assert_eq!(
            sources
                .iter()
                .find(|source| source["text"] == "nested document evidence")
                .unwrap()["locator"]["archive"]["chain"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let error = scan(
            &config,
            &path,
            &limits,
            &Arc::new(AtomicBool::new(false)),
            &mut |_| {
                Err(gateway::error(
                    "fixture disk full",
                    "RETRIEVAL_LIBRARY_IO_FAILED",
                    500,
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), "RETRIEVAL_LIBRARY_IO_FAILED");
        let token = Arc::new(AtomicBool::new(true));
        assert_eq!(
            scan(&config, &path, &limits, &token, &mut |_| Ok(()))
                .unwrap_err()
                .code(),
            "ABORT_ERR"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn member_names_reject_windows_and_posix_escape_forms() {
        for path in [
            "../x.md",
            "a/../../x",
            "/tmp/a",
            "C:/a",
            "\\\\server\\file",
            "a:stream",
            "CON.txt",
            "x/../b",
            "NUL",
            "com1.txt",
            "a/./b",
            "a./b",
        ] {
            assert!(member_name(path).is_err(), "{path}");
        }
        assert_eq!(member_name("资料\\说明.md").unwrap(), "资料/说明.md");
    }
    #[test]
    fn archive_defaults_follow_real_index_limits() {
        let limits = Limits::from_settings(
            &json!({"maximumFiles":40000,"maximumTotalBytes":768*MIB,"maximumSourceBytes":24*MIB}),
        );
        assert_eq!(limits.entries, 40000);
        assert_eq!(limits.expanded_bytes, 768 * MIB);
        assert_eq!(limits.member_bytes, 24 * MIB);
        assert_eq!(limits.depth, 4);
    }

    #[test]
    fn large_member_windows_keep_original_utf16_range_and_read_identity() {
        let root =
            std::env::temp_dir().join(format!("kynxa-archive-windows-{}", gateway::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let config = configuration(&root);
        let text = "中文长期资料🙂 current source evidence\n".repeat(120);
        let mut limits = crate::storage::settings::apply("storage.settings.defaults", &json!({}))
            .unwrap()["local"]["indexing"]
            .clone();
        limits["maximumDocumentOutputBytes"] = json!(1024);
        let path = root.join("windows.zip");
        std::fs::write(&path, zip_bytes(&[("src/guide.md", text.as_bytes())])).unwrap();
        let mut sources = Vec::new();
        let report = scan(
            &config,
            &path,
            &limits,
            &Arc::new(AtomicBool::new(false)),
            &mut |source| {
                sources.push(source);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(report["failedFiles"], 0);
        assert!(sources.len() > 1);
        assert_eq!(
            sources
                .iter()
                .map(|source| source["text"].as_str().unwrap())
                .collect::<String>(),
            text
        );
        assert!(
            sources[1]["locator"]["archive"]["part"]["startOffset"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(sources[1]["locator"]["archive"]["part"]["unit"], "utf16");
        assert!(
            sources[1]["locator"]["relativePath"]
                .as_str()
                .unwrap()
                .ends_with(".md")
        );
        let reread = read_member(
            &config,
            &sources[1]["locator"],
            &limits,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(reread["text"], sources[1]["text"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
