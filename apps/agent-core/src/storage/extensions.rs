use super::{ContractError, ContractResult, cancellation, managed_files, raw_json};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const DIRECTORIES: &[&str] = &[
    "Agent",
    "Skills",
    "MCP",
    "MCP/npm-cache",
    "MCP/browser-cache",
    "MCP/uv-cache",
    "MCP/uv-tools",
    "MCP/bin",
    "MCP/python",
    "MCP/python-bin",
    "MCP/runtimes",
    "Backups",
    "Backups/Extensions",
    "Backups/Extensions/Migrations",
];
const LEAVES: &[&str] = &[
    "Agent/config.json",
    "extension-layout.json",
    "extension-migration-info.json",
    "extensions-pointer.previous.json",
];
static TEMPORARIES: AtomicU64 = AtomicU64::new(0);
const LAYOUT_BYTES: &[u8] = b"{\"version\":1}";

fn invalid(message: &str, code: &'static str) -> ContractError {
    ContractError::new(message, code, 409)
}

fn io_error(error: std::io::Error) -> ContractError {
    invalid(
        &format!(
            "Extension path is inaccessible; the original is preserved. / 扩展路径不可访问，原配置已保留：{error}"
        ),
        "INVALID_EXTENSION_STORAGE",
    )
}

fn cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        Err(ContractError::new(
            "Extension initialization cancelled. / 扩展初始化已取消。",
            "ABORT_ERR",
            499,
        ))
    } else {
        Ok(())
    }
}

fn absolute(value: &Value) -> Result<PathBuf, ContractError> {
    let value = value
        .as_str()
        .ok_or_else(|| invalid("扩展位置必须为有效绝对路径。", "INVALID_EXTENSION_STORAGE"))?;
    let path = PathBuf::from(value);
    if value.trim().is_empty()
        || value.encode_utf16().count() > 4096
        || value.contains(['\0', '\r', '\n'])
        || !path.is_absolute()
        || value.starts_with("\\\\")
        || path.parent().is_none()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid(
            "扩展位置必须为有效绝对路径，不能使用磁盘根目录。",
            "INVALID_EXTENSION_STORAGE",
        ));
    }
    #[cfg(windows)]
    if value.as_bytes().get(1) != Some(&b':') || !value.as_bytes()[0].is_ascii_alphabetic() {
        return Err(invalid(
            "扩展位置必须为有效绝对路径。",
            "INVALID_EXTENSION_STORAGE",
        ));
    }
    Ok(path)
}

fn filesystem_root(path: &Path) -> PathBuf {
    path.components()
        .take_while(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
        .collect()
}

/// Shared handle checks reject hard links as well as junctions before any extension effect.
/// 扩展操作前共用句柄检查，同时拒绝硬链接、符号链接及联接点。
fn inspect(path: &Path, directory: Option<bool>) -> Result<bool, ContractError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error(error)),
    };
    let file = metadata.as_ref().is_some_and(|metadata| metadata.is_file());
    managed_files::inspect(&filesystem_root(path), path, false, file, u64::MAX).map_err(|_| {
        invalid(
            "扩展位置包含链接或不安全结构，原配置已保留。",
            "INVALID_EXTENSION_STORAGE",
        )
    })?;
    if let Some(metadata) = metadata {
        if directory.is_some_and(|directory| {
            if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file()
            }
        }) {
            return Err(invalid(
                "扩展目录结构无效，原文件已保留。",
                "INVALID_EXTENSION_LAYOUT",
            ));
        }
        if metadata.is_file() {
            let handle = fs::File::open(path).map_err(io_error)?;
            let identity = crate::file_identity::handle_identity(&handle).map_err(io_error)?;
            if identity.is_link || identity.links != 1 {
                return Err(invalid(
                    "扩展位置包含链接或不安全结构，原配置已保留。",
                    "INVALID_EXTENSION_STORAGE",
                ));
            }
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Match the settings client's strict top-level keys while retaining unknown JSON fields.
/// 与设置客户端保持顶层字段唯一性；未知 JSON 字段及原始字符串仍可保留。
fn parse_document(bytes: &[u8], code: &'static str) -> Result<Value, ContractError> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| invalid("扩展配置不是有效 UTF-8，原文件已保留。", code))?;
    // TextDecoder removes the encoding BOM; the legacy explicit codec removes one further BOM.
    // TextDecoder 去除编码 BOM；历史显式编解码再去除一个 BOM。
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut keys = HashSet::new();
    let mut depth = 0i32;
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let start = index;
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                if index >= bytes.len() {
                    return Err(invalid("扩展配置 JSON 无效，原文件已保留。", code));
                }
                let next = bytes[index + 1..]
                    .iter()
                    .copied()
                    .find(|byte| !byte.is_ascii_whitespace());
                if depth == 1 && next == Some(b':') {
                    let key = raw_json::utf16_identity(&source[start..=index])
                        .ok_or_else(|| invalid("扩展配置 JSON 无效。", code))?;
                    if !keys.insert(key) {
                        return Err(invalid("扩展配置含重复字段，原文件已保留。", code));
                    }
                }
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ => {}
        }
        index += 1;
    }
    let value: Value = serde_json::from_str(&raw_json::scalar_projection(source))
        .map_err(|_| invalid("扩展配置 JSON 无效，原文件已保留。", code))?;
    if !value.is_object() {
        return Err(invalid("扩展配置格式无效，原文件已保留。", code));
    }
    Ok(value)
}

fn read_document(path: &Path, code: &'static str) -> Result<Option<Value>, ContractError> {
    if !inspect(path, Some(false))? {
        return Ok(None);
    }
    let bytes = managed_files::read(&filesystem_root(path), path, 16_384)
        .map_err(|_| invalid("扩展配置格式无效，原文件已保留。", code))?
        .unwrap_or_default();
    parse_document(&bytes, code).map(Some)
}

fn layout(root: &Path) -> ContractResult {
    inspect(root, Some(true))?;
    for name in DIRECTORIES {
        inspect(&root.join(name), Some(true))?;
    }
    for name in LEAVES {
        inspect(&root.join(name), Some(false))?;
    }
    let document = read_document(
        &root.join("extension-layout.json"),
        "INVALID_EXTENSION_LAYOUT",
    )?;
    if document
        .as_ref()
        .is_some_and(|document| document["version"].as_f64() != Some(1.0))
    {
        return Err(invalid(
            "扩展配置版本不受当前程序支持，原文件已保留。",
            "UNSUPPORTED_EXTENSION_LAYOUT",
        ));
    }
    Ok(document.unwrap_or(Value::Null))
}

/// A staged receipt exposes the callback fence without transferring file ownership to Node.
/// 暂存回执暴露应用回调栅栏，但文件创建、身份核验及发布所有权仍留在原生服务。
fn stage(root: &Path) -> ContractResult {
    layout(root)?;
    let path = root.join("extension-layout.json");
    if inspect(&path, Some(false))? {
        return Ok(Value::Null);
    }
    let name = format!(
        ".extension-layout.{}-{}.tmp",
        std::process::id(),
        TEMPORARIES.fetch_add(1, Ordering::Relaxed)
    );
    let temporary = root.join(&name);
    cancelled()?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = options.open(&temporary).map_err(io_error)?;
    let identity = crate::file_identity::handle_identity(&handle).map_err(io_error)?;
    let result = (|| {
        handle.write_all(LAYOUT_BYTES).map_err(io_error)?;
        handle.sync_all().map_err(io_error)?;
        Ok(
            json!({"name":name,"volume":identity.volume.to_string(),"identity":identity.identity.to_string()}),
        )
    })();
    drop(handle);
    if result.is_err() {
        fs::remove_file(&temporary).map_err(io_error)?;
    }
    result
}

fn owned_stage(root: &Path, receipt: &Value) -> Result<Option<PathBuf>, ContractError> {
    if receipt.is_null() {
        return Ok(None);
    }
    let name = receipt["name"].as_str().unwrap_or("");
    let prefix = format!(".extension-layout.{}-", std::process::id());
    let sequence = name
        .strip_prefix(&prefix)
        .and_then(|name| name.strip_suffix(".tmp"));
    if !sequence
        .is_some_and(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(invalid("扩展暂存回执无效。", "INVALID_EXTENSION_LAYOUT"));
    }
    let path = root.join(name);
    if !inspect(&path, Some(false))? {
        return Ok(None);
    }
    let bytes = managed_files::read(root, &path, 64).map_err(|_| {
        invalid(
            "扩展暂存文件已变化，现有文件已保留。",
            "INVALID_EXTENSION_STORAGE",
        )
    })?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let handle = options.open(&path).map_err(io_error)?;
    let identity = crate::file_identity::handle_identity(&handle).map_err(io_error)?;
    if identity.is_link
        || identity.links != 1
        || bytes.as_deref() != Some(LAYOUT_BYTES)
        || receipt["volume"].as_str() != Some(identity.volume.to_string().as_str())
        || receipt["identity"].as_str() != Some(identity.identity.to_string().as_str())
    {
        return Err(invalid(
            "扩展暂存文件已变化，现有文件已保留。",
            "INVALID_EXTENSION_STORAGE",
        ));
    }
    Ok(Some(path))
}

fn discard(root: &Path, receipt: &Value) -> ContractResult {
    if let Some(path) = owned_stage(root, receipt)? {
        fs::remove_file(path).map_err(io_error)?;
    }
    Ok(Value::Null)
}

fn publish(root: &Path, receipt: &Value) -> ContractResult {
    layout(root)?;
    if let Some(temporary) = owned_stage(root, receipt)? {
        cancelled()?;
        let path = root.join("extension-layout.json");
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
        // Publication makes the owned file temporarily have two links; remove only its staged name.
        // 发布暂时产生同一文件的两个链接；仅移除本次拥有的暂存名称，保留正式名称。
        fs::remove_file(&temporary).map_err(io_error)?;
    }
    layout(root)
}

fn contains(root: &Path, target: &Path) -> bool {
    let root: Vec<_> = root.components().collect();
    let target: Vec<_> = target.components().collect();
    target.len() >= root.len()
        && root.iter().zip(&target).all(|(left, right)| {
            #[cfg(windows)]
            {
                left.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
            }
            #[cfg(not(windows))]
            {
                left == right
            }
        })
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    if operation == "storage.extensions.home" {
        if !arguments["explicitRoot"].is_null() {
            let root = absolute(&arguments["explicitRoot"])?;
            inspect(&root, None)?;
            return Ok(json!(root));
        }
        if arguments["isolated"] == true {
            return Ok(arguments["fallback"].clone());
        }
        let pointer = absolute(&arguments["pointer"])?;
        let Some(document) = read_document(&pointer, "INVALID_EXTENSION_STORAGE")? else {
            return Ok(arguments["fallback"].clone());
        };
        if document["version"].as_f64() != Some(1.0) {
            return Err(invalid(
                "扩展位置配置版本不受支持。",
                "UNSUPPORTED_EXTENSION_STORAGE",
            ));
        }
        let root = absolute(&document["extensionRoot"])?;
        inspect(&root, None)?;
        return Ok(json!(root));
    }
    if operation == "storage.extensions.control" {
        let pointer = absolute(&arguments["pointer"])?;
        let profile = absolute(&arguments["profile"])?;
        let mut paths = vec![
            pointer.clone(),
            profile.join("storage.json"),
            profile.join("extensions.json"),
        ];
        for directory in [&profile, pointer.parent().unwrap()] {
            for name in [
                "storage-migration.lock",
                "extensions-operation.lock",
                "extension-migration.lock",
            ] {
                let path = directory.join(name);
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        if let Some(target) = arguments.get("target") {
            let target = PathBuf::from(target.as_str().unwrap_or(""));
            return Ok(json!(
                paths.iter().any(|control| contains(&target, control))
            ));
        }
        return Ok(json!(paths));
    }
    let root = absolute(&arguments["root"])?;
    match operation {
        "storage.extensions.absolute" => {
            inspect(&root, None)?;
            Ok(json!(root))
        }
        "storage.extensions.inspect" => layout(&root),
        "storage.extensions.prepare" => {
            layout(&root)?;
            Ok(json!(
                std::iter::once(root.clone())
                    .chain(DIRECTORIES.iter().map(|name| root.join(name)))
                    .collect::<Vec<_>>()
            ))
        }
        "storage.extensions.directory" => {
            let path = absolute(&arguments["path"])?;
            if path != root && !DIRECTORIES.iter().any(|name| path == root.join(name)) {
                return Err(invalid("扩展初始化目录无效。", "INVALID_EXTENSION_LAYOUT"));
            }
            inspect(&path, Some(true))?;
            cancelled()?;
            managed_files::inspect(&root, &path, true, false, u64::MAX)?;
            inspect(&path, Some(true))?;
            Ok(Value::Null)
        }
        "storage.extensions.stage" => stage(&root),
        "storage.extensions.discard" => discard(&root, &arguments["receipt"]),
        "storage.extensions.publish" => publish(&root, &arguments["receipt"]),
        "storage.extensions.cache" => {
            inspect(&root, None)?;
            let mut result = serde_json::Map::new();
            for (key, name) in [
                ("npmCache", "npm-cache"),
                ("browserCache", "browser-cache"),
                ("uvCache", "uv-cache"),
                ("uvTools", "uv-tools"),
                ("python", "python"),
                ("pythonBin", "python-bin"),
                ("uvToolBin", "bin"),
            ] {
                result.insert(key.into(), json!(root.join("MCP").join(name)));
            }
            Ok(Value::Object(result))
        }
        "storage.extensions.managed" => {
            let target = PathBuf::from(arguments["target"].as_str().unwrap_or(""));
            if !contains(&root, &target) {
                return Ok(json!(false));
            }
            let suffix: Vec<_> = target
                .components()
                .skip(root.components().count())
                .collect();
            if suffix.is_empty() {
                return Ok(json!(true));
            }
            let first = suffix[0].as_os_str().to_string_lossy().to_ascii_lowercase();
            Ok(json!(
                ["agent", "skills", "mcp", "backups"].contains(&first.as_str())
                    || suffix.len() == 1
                        && [
                            "extension-migration-info.json",
                            "extensions-pointer.previous.json",
                            "extension-layout.json"
                        ]
                        .contains(&first.as_str())
            ))
        }
        _ => Err(invalid(
            "Unknown extension storage operation. / 未知扩展存储操作。",
            "INVALID_EXTENSION_STORAGE",
        )),
    }
}
