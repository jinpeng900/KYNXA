use super::{ContractError, ContractResult, cancellation};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

static FILE_TRANSACTIONS: Mutex<()> = Mutex::new(());
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn check_cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        return Err(ContractError::new(
            "Storage operation cancelled. / 存储操作已取消。",
            "ABORT_ERR",
            499,
        ));
    }
    Ok(())
}

fn io_failure(error: std::io::Error) -> ContractError {
    ContractError::new(
        format!("Managed file operation failed. / 托管文件操作失败：{error}"),
        "STORAGE_IO_FAILED",
        500,
    )
}

fn unsafe_path() -> ContractError {
    ContractError::new(
        "Managed path contains a link or invalid structure. / 托管路径包含链接或无效结构。",
        "INVALID_MEMORY_PATH",
        409,
    )
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Validate every ancestor before creating children; a missing final file does not authorize an alias.
/// 创建子项前检查所有祖先；目标文件缺失不代表可接受链接或联接点。
pub fn inspect(
    root: &Path,
    path: &Path,
    create: bool,
    file: bool,
    maximum_bytes: u64,
) -> Result<bool, ContractError> {
    if !root.is_absolute()
        || !path.is_absolute()
        || !path.starts_with(root)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(unsafe_path());
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        check_cancelled()?;
        current.push(part);
        let final_file = file && current == path;
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if is_link(&metadata)
                    || if final_file {
                        !metadata.is_file()
                    } else {
                        !metadata.is_dir()
                    }
                {
                    return Err(unsafe_path());
                }
                if final_file && metadata.len() > maximum_bytes {
                    return Err(ContractError::new(
                        "Managed file exceeds its capacity; the original is preserved. / 托管文件超过容量，原文件已保留。",
                        "CORRUPT_MEMORY",
                        500,
                    ));
                }
                if final_file {
                    let handle = std::fs::File::open(&current).map_err(io_failure)?;
                    let identity =
                        crate::file_identity::handle_identity(&handle).map_err(io_failure)?;
                    if identity.is_link || identity.links != 1 {
                        return Err(unsafe_path());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if create && !final_file && current.starts_with(root) {
                    match fs::create_dir(&current) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(io_failure(error)),
                    }
                    let metadata = fs::symlink_metadata(&current).map_err(io_failure)?;
                    if is_link(&metadata) || !metadata.is_dir() {
                        return Err(unsafe_path());
                    }
                } else {
                    return Ok(false);
                }
            }
            Err(error) => return Err(io_failure(error)),
        }
    }
    Ok(true)
}

/// A bounded reader holds one checked object and verifies its identity again after range reads.
/// 有界读取持有已校验文件对象，窗口读取结束后再核验身份，避免绕过既有路径与版本保护。
pub struct StableReader {
    pub file: std::fs::File,
    root: PathBuf,
    path: PathBuf,
    identity: crate::file_identity::HandleIdentity,
    metadata: fs::Metadata,
}

impl StableReader {
    /// Cache only against an observable change stamp, object identity and size; unsupported files stay byte-verified.
    /// 缓存绑定可观测变更时间、文件身份及大小；不支持该时间的文件仍逐次校验完整字节。
    pub fn version_token(&self) -> Result<Option<String>, ContractError> {
        let Some(stamp) = crate::file_identity::change_stamp(&self.file).map_err(io_failure)?
        else {
            return Ok(None);
        };
        let metadata = self.file.metadata().map_err(io_failure)?;
        Ok(Some(format!(
            "{}:{}:{}:{stamp}:{:?}:{:?}",
            self.identity.volume,
            self.identity.identity,
            metadata.len(),
            metadata.modified().ok(),
            metadata.created().ok()
        )))
    }

    pub fn verify(&self) -> Result<(), ContractError> {
        check_cancelled()?;
        inspect(&self.root, &self.path, false, true, u64::MAX)?;
        let current_file = read_options().open(&self.path).map_err(io_failure)?;
        let current_identity =
            crate::file_identity::handle_identity(&current_file).map_err(io_failure)?;
        let current = current_file.metadata().map_err(io_failure)?;
        let opened_after = self.file.metadata().map_err(io_failure)?;
        if current_identity.is_link
            || current_identity.links != 1
            || self.identity.volume != current_identity.volume
            || self.identity.identity != current_identity.identity
            || self.metadata.len() != current.len()
            || self.metadata.modified().ok() != current.modified().ok()
            || self.metadata.created().ok() != current.created().ok()
            || self.metadata.len() != opened_after.len()
            || self.metadata.modified().ok() != opened_after.modified().ok()
        {
            return Err(ContractError::new(
                "Managed file changed while reading. / 托管文件读取期间已改变。",
                "MEMORY_CONFLICT",
                409,
            ));
        }
        Ok(())
    }
}

fn read_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000);
    }
    options
}

pub fn open_reader(
    root: &Path,
    path: &Path,
    maximum_bytes: u64,
) -> Result<Option<StableReader>, ContractError> {
    if !inspect(root, path, false, true, maximum_bytes)? {
        return Ok(None);
    }
    let file = read_options().open(path).map_err(io_failure)?;
    let original_identity = crate::file_identity::handle_identity(&file).map_err(io_failure)?;
    if original_identity.is_link || original_identity.links != 1 {
        return Err(unsafe_path());
    }
    let original = file.metadata().map_err(io_failure)?;
    Ok(Some(StableReader {
        file,
        root: root.to_owned(),
        path: path.to_owned(),
        identity: original_identity,
        metadata: original,
    }))
}

pub fn read(
    root: &Path,
    path: &Path,
    maximum_bytes: u64,
) -> Result<Option<Vec<u8>>, ContractError> {
    let Some(reader) = open_reader(root, path, maximum_bytes)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    (&reader.file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_failure)?;
    reader.verify()?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(ContractError::new(
            "Managed file exceeds its capacity. / 托管文件超过容量。",
            "CORRUPT_MEMORY",
            500,
        ));
    }
    Ok(Some(bytes))
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Fsync an exclusively created sibling, then replace the canonical file and return its actual receipt.
/// 独占创建同目录临时文件并同步落盘，再替换正式文件，返回实际提交回执。
pub fn atomic_write(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    maximum_bytes: u64,
) -> Result<(), ContractError> {
    if bytes.len() as u64 > maximum_bytes {
        return Err(ContractError::new(
            "The scope has reached its storage capacity. / 此作用域已达存储容量上限。",
            "MEMORY_CAPACITY_EXCEEDED",
            409,
        ));
    }
    let parent = path.parent().ok_or_else(unsafe_path)?;
    inspect(root, parent, true, false, maximum_bytes)?;
    inspect(root, path, false, true, maximum_bytes)?;
    let temporary = parent.join(format!(
        ".kynxa-{}-{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut created = false;
    let result = (|| {
        check_cancelled()?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(&temporary).map_err(io_failure)?;
        created = true;
        handle.write_all(bytes).map_err(io_failure)?;
        handle.sync_all().map_err(io_failure)?;
        drop(handle);
        inspect(root, parent, false, false, maximum_bytes)?;
        inspect(root, path, false, true, maximum_bytes)?;
        check_cancelled()?;
        fs::rename(&temporary, path).map_err(io_failure)?;
        Ok(())
    })();
    if created && temporary.exists() {
        fs::remove_file(&temporary).map_err(io_failure)?;
    }
    result
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let _transaction = FILE_TRANSACTIONS.lock().map_err(|_| {
        ContractError::new(
            "Storage transaction lock failed. / 存储事务锁失效。",
            "STORAGE_IO_FAILED",
            500,
        )
    })?;
    check_cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    let maximum_bytes = arguments["maximumBytes"]
        .as_u64()
        .unwrap_or(8 * 1024 * 1024);
    match operation {
        "storage.files.inspect" => Ok(json!(inspect(
            root,
            path,
            arguments["create"] == true,
            arguments["file"] == true,
            maximum_bytes
        )?)),
        "storage.files.read" => match read(root, path, maximum_bytes)? {
            None => Ok(json!({"exists": false, "raw": null, "hash": null})),
            Some(bytes) => {
                let hash = digest(&bytes);
                let raw = String::from_utf8_lossy(&bytes);
                Ok(json!({"exists": true, "raw": raw, "hash": hash}))
            }
        },
        "storage.files.commit" => {
            let previous = read(root, path, maximum_bytes)?;
            if arguments.get("expectedHash").is_some() {
                let actual = previous.as_ref().map(|bytes| digest(bytes));
                if actual.as_deref() != arguments["expectedHash"].as_str() {
                    return Err(ContractError::new(
                        "The persisted scope changed; reload before retrying. / 正式范围已改变，请重新读取后重试。",
                        "MEMORY_CONFLICT",
                        409,
                    ));
                }
            }
            let raw = arguments["raw"].as_str().ok_or_else(unsafe_path)?;
            atomic_write(root, path, raw.as_bytes(), maximum_bytes)?;
            Ok(json!({"hash": digest(raw.as_bytes()), "bytes": raw.len()}))
        }
        "storage.files.preserveCorrupt" => {
            let backup = Path::new(arguments["backup"].as_str().unwrap_or(""));
            let bytes = read(root, path, maximum_bytes)?.ok_or_else(unsafe_path)?;
            if arguments["expectedHash"].as_str() != Some(digest(&bytes).as_str()) {
                return Err(ContractError::new(
                    "Summary changed before recovery. / 摘要恢复前已改变。",
                    "SUMMARY_SOURCE_CHANGED",
                    409,
                ));
            }
            inspect(root, backup, false, true, maximum_bytes)?;
            let mut handle = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(backup)
                .map_err(io_failure)?;
            handle.write_all(&bytes).map_err(io_failure)?;
            handle.sync_all().map_err(io_failure)?;
            fs::remove_file(path).map_err(io_failure)?;
            Ok(Value::Null)
        }
        _ => Err(ContractError::new(
            "Unknown managed file operation. / 未知托管文件操作。",
            "INVALID_MEMORY",
            400,
        )),
    }
}
