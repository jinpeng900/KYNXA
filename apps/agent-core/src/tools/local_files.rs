use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::service_error::{ServiceError, ServiceResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileVersion {
    pub volume: u64,
    pub identity: u64,
    pub size: u64,
    pub modified: u128,
    pub created: u128,
    pub links: u64,
}

pub fn path_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    // Windows canonicalization uses extended paths; the public contract keeps ordinary local drive paths.
    // Windows 真实路径解析使用扩展前缀；公开合同仍返回普通本机盘符路径。
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}

pub fn absolute_path(value: &str) -> PathBuf {
    let path = Path::new(value);
    let source = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut result = PathBuf::new();
    for component in source.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result
}

pub fn path_key(path: &Path) -> String {
    let value = path_text(path);
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

pub fn within(root: &Path, path: &Path) -> bool {
    let root = path_key(&absolute_path(&path_text(root)));
    let path = path_key(&absolute_path(&path_text(path)));
    path == root
        || path.starts_with(
            &(root.trim_end_matches(std::path::MAIN_SEPARATOR).to_owned()
                + std::path::MAIN_SEPARATOR_STR),
        )
}

fn unsafe_path() -> ServiceError {
    ServiceError::new(
        "UNSAFE_TOOL_PATH",
        "工具路径包含链接或不安全结构，操作已拒绝。",
        403,
    )
}

pub fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

pub fn inspect(
    path: &Path,
    allow_missing: bool,
    allow_hard_links: bool,
) -> ServiceResult<Option<Metadata>> {
    let path = absolute_path(&path_text(path));
    let mut current = PathBuf::new();
    let components = path.components().collect::<Vec<_>>();
    let mut final_metadata = None;
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        // A bare Windows drive prefix is not an independently addressable root.
        // Windows 盘符前缀本身并非可独立访问的根目录。
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_missing => {
                return Ok(None);
            }
            Err(error) => return Err(ServiceError::io(error)),
        };
        if is_link(&metadata) || index + 1 < components.len() && !metadata.is_dir() {
            return Err(unsafe_path());
        }
        if metadata.is_file() && !allow_hard_links && file_version(&current, &metadata)?.links > 1 {
            return Err(unsafe_path());
        }
        final_metadata = Some(metadata);
    }
    Ok(final_metadata)
}

pub fn open_read(path: &Path) -> ServiceResult<File> {
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
        // The Linux runtime is a development/test target; production runs on Windows.
        // Linux 为开发和测试运行时；正式产品运行于 Windows。
        options.custom_flags(0x20000);
    }
    options.open(path).map_err(ServiceError::io)
}

fn timestamp(value: std::io::Result<std::time::SystemTime>) -> u128 {
    value
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub fn handle_version(file: &File, metadata: &Metadata) -> ServiceResult<FileVersion> {
    #[cfg(windows)]
    {
        let information = crate::file_identity::handle_identity(file).map_err(ServiceError::io)?;
        if information.is_link {
            return Err(unsafe_path());
        }
        return Ok(FileVersion {
            volume: information.volume,
            identity: information.identity,
            size: metadata.len(),
            modified: timestamp(metadata.modified()),
            created: timestamp(metadata.created()),
            links: information.links,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = file;
        Ok(FileVersion {
            volume: metadata.dev(),
            identity: metadata.ino(),
            size: metadata.len(),
            modified: timestamp(metadata.modified()),
            created: (metadata.ctime() as i128 * 1_000_000_000 + metadata.ctime_nsec() as i128)
                as u128,
            links: metadata.nlink(),
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        Ok(FileVersion {
            volume: 0,
            identity: 0,
            size: metadata.len(),
            modified: timestamp(metadata.modified()),
            created: timestamp(metadata.created()),
            links: 1,
        })
    }
}

pub fn file_version(path: &Path, metadata: &Metadata) -> ServiceResult<FileVersion> {
    if metadata.is_dir() {
        return Ok(FileVersion {
            volume: 0,
            identity: 0,
            size: 0,
            modified: timestamp(metadata.modified()),
            created: timestamp(metadata.created()),
            links: 1,
        });
    }
    handle_version(&open_read(path)?, metadata)
}

pub fn modified_iso(metadata: &Metadata) -> String {
    let milliseconds = match metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
    {
        Ok(duration) => duration.as_millis() as i64,
        Err(error) => -(error.duration().as_millis() as i64),
    };
    let mut days = milliseconds.div_euclid(86_400_000);
    let time = milliseconds.rem_euclid(86_400_000);
    let leap = |year: i64| year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut year = 1970;
    while days < 0 {
        year -= 1;
        days += if leap(year) { 366 } else { 365 };
    }
    while days >= if leap(year) { 366 } else { 365 } {
        days -= if leap(year) { 366 } else { 365 };
        year += 1;
    }
    let month_days = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while days >= month_days[month] {
        days -= month_days[month];
        month += 1;
    }
    format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        month + 1,
        days + 1,
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    )
}

pub fn read_checked(
    path: &Path,
    maximum_bytes: u64,
    allow_hard_links: bool,
    changed_code: &str,
) -> ServiceResult<Vec<u8>> {
    let changed = || ServiceError::new(changed_code, "文件在读取期间已变化，请重新读取。", 409);
    let original = inspect(path, true, allow_hard_links)?.ok_or_else(|| {
        ServiceError::new(
            "SKILL_RESOURCE_NOT_FOUND",
            "Skill resource was not found.",
            404,
        )
    })?;
    if !original.is_file() || original.len() > maximum_bytes {
        return Err(ServiceError::new(
            "SKILL_RESOURCE_LIMIT",
            "Skill resource is not a regular file or exceeds its size limit.",
            400,
        ));
    }
    let original_version = file_version(path, &original)?;
    let file = open_read(path)?;
    let metadata = file.metadata().map_err(ServiceError::io)?;
    let first = handle_version(&file, &metadata)?;
    if !metadata.is_file() || first != original_version || !allow_hard_links && first.links > 1 {
        return Err(changed());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
    (&file)
        .take(metadata.len() + 1)
        .read_to_end(&mut bytes)
        .map_err(ServiceError::io)?;
    let after = handle_version(&file, &file.metadata().map_err(ServiceError::io)?)?;
    let path_metadata = inspect(path, false, allow_hard_links)?.ok_or_else(changed)?;
    if bytes.len() as u64 != first.size
        || first != after
        || first != file_version(path, &path_metadata)?
    {
        return Err(changed());
    }
    Ok(bytes)
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn random_identity() -> ServiceResult<String> {
    let mut bytes = [0_u8; 16];
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                provider: *mut std::ffi::c_void,
                buffer: *mut u8,
                length: u32,
                flags: u32,
            ) -> i32;
        }
        // The OS cryptographic RNG receives a fixed-size valid stack buffer; no user secrets are read.
        // 系统加密随机数源接收固定大小且有效的栈缓冲区，不读取用户凭据。
        if unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                2,
            )
        } < 0
        {
            return Err(ServiceError::new(
                "TOOL_RANDOM_FAILED",
                "无法生成工具服务身份。",
                500,
            ));
        }
    }
    #[cfg(not(windows))]
    {
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(ServiceError::io)?;
    }
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let text = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &text[..8],
        &text[8..12],
        &text[12..16],
        &text[16..20],
        &text[20..]
    ))
}

pub fn ensure_directory(path: &Path) -> ServiceResult<()> {
    if let Some(metadata) = inspect(path, true, false)? {
        if metadata.is_dir() {
            return Ok(());
        }
        return Err(unsafe_path());
    }
    let parent = path.parent().ok_or_else(unsafe_path)?;
    ensure_directory(parent)?;
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(ServiceError::io(error)),
    }
    if !inspect(path, false, false)?.is_some_and(|metadata| metadata.is_dir()) {
        return Err(unsafe_path());
    }
    Ok(())
}

pub fn atomic_json(path: &Path, value: &Value) -> ServiceResult<()> {
    let parent = path.parent().ok_or_else(unsafe_path)?;
    ensure_directory(parent)?;
    inspect(path, true, false)?;
    let temporary = parent.join(format!(
        "{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        random_identity()?
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(ServiceError::io)?;
        file.write_all(serde_json::to_string_pretty(value).unwrap().as_bytes())
            .map_err(ServiceError::io)?;
        file.sync_all().map_err(ServiceError::io)?;
        inspect(path, true, false)?;
        fs::rename(&temporary, path).map_err(ServiceError::io)
    })();
    if let Err(error) = fs::remove_file(&temporary) {
        if error.kind() != std::io::ErrorKind::NotFound && result.is_ok() {
            return Err(ServiceError::io(error));
        }
    }
    result
}

pub fn inspect_projection(arguments: &Value) -> ServiceResult<Value> {
    let path = absolute_path(arguments["path"].as_str().unwrap_or(""));
    let Some(metadata) = inspect(
        &path,
        arguments["allowMissing"] == true,
        arguments["allowHardLinks"] == true,
    )?
    else {
        return Ok(Value::Null);
    };
    let canonical = fs::canonicalize(&path).map_err(ServiceError::io)?;
    Ok(
        json!({"path":path_text(&path),"canonicalPath":path_text(&canonical),"file":metadata.is_file(),"directory":metadata.is_dir(),
        "size":metadata.len(),"identity":format!("{:?}",file_version(&path,&metadata)?)}),
    )
}

pub(super) fn metadata_projection(path: &Path, metadata: Metadata) -> ServiceResult<Value> {
    let version = file_version(path, &metadata)?;
    let milliseconds =
        |value: std::io::Result<std::time::SystemTime>| timestamp(value) as f64 / 1_000_000.0;
    Ok(
        json!({"file":metadata.is_file(),"directory":metadata.is_dir(),"size":metadata.len(),"nlink":version.links,
        "dev":version.volume,"ino":version.identity,"mtimeMs":milliseconds(metadata.modified()),"ctimeMs":milliseconds(metadata.created()),
        "birthtimeMs":milliseconds(metadata.created()),"identity":format!("{:?}",version)}),
    )
}

/// Tool paths share one canonical scope contract; synchronous calls below only calculate policy.
/// 工具路径共用一个真实范围合同；下方同步操作仅计算策略，I/O 操作走异步服务。
pub fn path_operation(operation: &str, arguments: &Value) -> ServiceResult<Value> {
    use super::intent::matches;
    let path = absolute_path(arguments["path"].as_str().unwrap_or(""));
    let invalid = || ServiceError::new("INVALID_TOOL_ARGUMENTS", "请提供有效文件路径。", 400);
    match operation {
        "tools.localPath.within" => Ok(json!(within(
            &absolute_path(arguments["root"].as_str().unwrap_or("")),
            &path
        ))),
        "tools.localPath.credential" => {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            let data = absolute_path(arguments["dataHome"].as_str().unwrap_or(""));
            let app = arguments["appDataRoot"].as_str().map(absolute_path);
            Ok(json!(
                matches(r"(?i)^connections(?:[.\-_]|$)", &name)
                    && (within(&data, &path)
                        || app
                            .as_ref()
                            .is_some_and(|root| within(&root.join("Backups"), &path)))
                    || matches(r"(?i)^config(?:[.\-_]|$)", &name)
                        && app
                            .as_ref()
                            .is_some_and(|root| within(&root.join("Agent"), &path)
                                || within(&root.join("Backups"), &path))
            ))
        }
        "tools.localPath.inspect" => match inspect(
            &path,
            arguments["allowMissing"] == true,
            arguments["allowHardLinks"] == true,
        )? {
            None => Ok(Value::Null),
            Some(metadata) => metadata_projection(&path, metadata),
        },
        "tools.localPath.ensure" => {
            ensure_directory(&path)?;
            Ok(Value::Null)
        }
        "tools.localPath.bind" => {
            let missing = arguments["allowMissing"] == true;
            let hard = arguments["allowHardLinks"] == true;
            let canonical = super::service_filesystem::canonical(&path, missing)?;
            let workspace = arguments["workspaceRoot"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(absolute_path);
            let canonical_workspace = workspace
                .as_ref()
                .map(|path| super::service_filesystem::canonical(path, false))
                .transpose()?;
            let protected = arguments["protectedRoots"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut canonical_protected = Vec::new();
            for root in &protected {
                let root = absolute_path(root.as_str().unwrap_or(""));
                let resolved = super::service_filesystem::canonical(&root, true)?;
                if within(&root, &path) || within(&resolved, &canonical) {
                    return Err(ServiceError::new(
                        "PROTECTED_APP_DATA",
                        "正式应用数据由专用服务管理，文件工具不能访问或改写。",
                        403,
                    ));
                }
                canonical_protected.push(path_text(&resolved));
            }
            inspect(&canonical, missing, hard)?;
            Ok(
                json!({"requestedPath":path_text(&path),"path":path_text(&canonical),"requestedWorkspaceRoot":workspace.as_ref().map(|path|path_text(path)),"workspaceRoot":canonical_workspace.as_ref().map(|path|path_text(path)),"outsideWorkspace":!canonical_workspace.as_ref().is_some_and(|root|within(root,&canonical)),"allowMissing":missing,"allowHardLinks":hard,"protectedRoots":protected,"canonicalProtectedRoots":canonical_protected}),
            )
        }
        "tools.localPath.revalidate" => {
            let binding = &arguments["binding"];
            if !binding["requestedPath"].is_string() || !binding["path"].is_string() {
                return Err(ServiceError::new(
                    "TOOL_PATH_CHANGED",
                    "缺少已授权路径的真实目标。",
                    409,
                ));
            }
            super::service_filesystem::revalidate(binding)?;
            let path = absolute_path(binding["path"].as_str().unwrap());
            match inspect(
                &path,
                binding["allowMissing"] == true,
                binding["allowHardLinks"] == true,
            )? {
                None => Ok(Value::Null),
                Some(metadata) => metadata_projection(&path, metadata),
            }
        }
        "tools.localPath.resolve" => {
            let input = &arguments["input"];
            let value = if input["path"].is_null() {
                "."
            } else {
                input["path"].as_str().ok_or_else(invalid)?
            };
            if value.trim().is_empty()
                || value.encode_utf16().count() > 4096
                || value.contains(['\0', '\r', '\n'])
            {
                return Err(invalid());
            }
            if cfg!(windows) {
                let no_drive = if value.as_bytes().get(1) == Some(&b':') {
                    &value[2..]
                } else {
                    value
                };
                if value.starts_with("\\\\")
                    || matches(r"(?i)^[a-z]:(?:$|[^\\/])", value)
                    || no_drive.contains(':')
                    || value.split(['\\', '/']).any(|part| {
                        part != "."
                            && part != ".."
                            && (part.ends_with(['.', ' '])
                                || matches(
                                    r"(?i)^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)",
                                    part,
                                ))
                    })
                {
                    return Err(unsafe_path());
                }
            }
            let workspace = arguments["context"]["workspaceRoot"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(absolute_path);
            if !Path::new(value).is_absolute() && workspace.is_none() {
                return Err(ServiceError::new(
                    "WORKSPACE_REQUIRED",
                    "当前聊天没有关联工作文件夹，不能使用相对路径。",
                    400,
                ));
            }
            let path = if Path::new(value).is_absolute() {
                absolute_path(value)
            } else {
                absolute_path(&path_text(&workspace.as_ref().unwrap().join(value)))
            };
            if arguments["protectedRoots"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|root| within(&absolute_path(root.as_str().unwrap_or("")), &path))
            {
                return Err(ServiceError::new(
                    "PROTECTED_APP_DATA",
                    "正式应用数据由专用服务管理，文件工具不能访问或改写。",
                    403,
                ));
            }
            let outside = !workspace.as_ref().is_some_and(|root| within(root, &path));
            if arguments["deferScopeCheck"] != true
                && outside
                && !input["reason"]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty())
            {
                return Err(ServiceError::new(
                    "OUTSIDE_WORKSPACE_REASON_REQUIRED",
                    "访问工作范围以外的路径必须说明原因。",
                    403,
                ));
            }
            Ok(json!({"path":path_text(&path),"outsideWorkspace":outside}))
        }
        "tools.localPath.object" => {
            if !arguments["value"].is_object() {
                return Err(ServiceError::new(
                    "INVALID_TOOL_ARGUMENTS",
                    "工具输入必须为对象。",
                    400,
                ));
            }
            Ok(Value::Null)
        }
        "tools.localPath.integer" => {
            let value = if arguments["value"].is_null() {
                &arguments["fallback"]
            } else {
                &arguments["value"]
            };
            let min = arguments["min"].as_i64().unwrap_or(0);
            let max = arguments["max"].as_i64().unwrap_or(0);
            let result = value
                .as_i64()
                .filter(|value| {
                    *value >= min && *value <= max && value.unsigned_abs() <= 9_007_199_254_740_991
                })
                .ok_or_else(|| {
                    ServiceError::new(
                        "INVALID_TOOL_ARGUMENTS",
                        &format!("整数参数须在 {min}–{max} 范围内。"),
                        400,
                    )
                })?;
            Ok(json!(result))
        }
        "tools.localPath.excerpt" => {
            let units = if let Some(units) = arguments["units"].as_array() {
                units
                    .iter()
                    .filter_map(|value| value.as_u64().map(|value| value as u16))
                    .collect::<Vec<_>>()
            } else {
                arguments["text"]
                    .as_str()
                    .unwrap_or("")
                    .encode_utf16()
                    .collect::<Vec<_>>()
            };
            let maximum = arguments["maximum"].as_i64().unwrap_or(0).max(0) as usize;
            let marker = "\n… [middle omitted] …\n";
            let marker_length = marker.encode_utf16().count();
            if maximum == 0 {
                return Ok(json!({"empty":true}));
            }
            if units.len() <= maximum {
                return Ok(json!({"whole":true}));
            }
            if maximum <= marker_length {
                return Ok(json!({"marker":marker,"markerEnd":maximum}));
            }
            let available = maximum - marker_length;
            let mut head = available.div_ceil(2);
            let mut tail = units.len() - available / 2;
            if head > 0 && (0xd800..=0xdbff).contains(&units[head - 1]) {
                head -= 1;
            }
            if units
                .get(tail)
                .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
            {
                tail += 1;
            }
            Ok(json!({"head":head,"tail":tail,"marker":marker}))
        }
        _ => Err(ServiceError::new(
            "AGENT_CORE_UNKNOWN_OPERATION",
            "Unknown path operation.",
            400,
        )),
    }
}
