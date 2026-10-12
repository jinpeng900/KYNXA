use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use super::intent::matches;
use super::local_files::{
    absolute_path, inspect, is_link, path_key, path_text, random_identity, read_checked, sha256,
};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::string;

const MAX_FILE_BYTES: u64 = 1024 * 1024;
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
    let Some(metadata) = inspect(path, true, false)? else {
        if !expected.is_null() {
            return Err(conflict("文件已不存在或版本不匹配，请重新读取。"));
        }
        return Ok(None);
    };
    if !metadata.is_file() {
        return Err(invalid("路径不是普通文件。"));
    }
    let bytes = read_bounded(path, MAX_FILE_BYTES)?;
    if !matches(r"^[a-f0-9]{64}$", string(expected)) || sha256(&bytes) != string(expected) {
        return Err(conflict("文件已更新，请重新读取后再修改。"));
    }
    Ok(Some(bytes))
}

fn text_page(text: &str, offset: usize, maximum: usize) -> ServiceResult<Value> {
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

fn read_page(path: &Path, input: &Value) -> ServiceResult<Value> {
    let bytes = read_bounded(
        path,
        bounded(&input["maxBytes"], MAX_FILE_BYTES, 1, MAX_FILE_BYTES)?,
    )?;
    let text = decode_text(&bytes)?;
    let maximum = bounded(&input["maxChars"], 32000, 1, 64000)? as usize;
    let mut page;
    if input.get("startLine").is_some() || input.get("endLine").is_some() {
        let start_line = bounded(&input["startLine"], 1, 1, MAX_FILE_BYTES + 1)? as usize;
        let end_line =
            bounded(&input["endLine"], MAX_FILE_BYTES + 1, 1, MAX_FILE_BYTES + 1)? as usize;
        if end_line < start_line {
            return Err(invalid("endLine 不能小于 startLine。"));
        }
        let units = text.encode_utf16().collect::<Vec<_>>();
        let mut total_lines = 1;
        let mut range_start = (start_line == 1).then_some(0);
        let mut range_end = units.len();
        for (index, unit) in units.iter().enumerate() {
            if *unit != 10 {
                continue;
            }
            total_lines += 1;
            if total_lines == start_line {
                range_start = Some(index + 1);
            }
            if total_lines == end_line + 1 {
                range_end = index + 1;
            }
        }
        let range_start = range_start
            .ok_or_else(|| invalid("startLine 超出当前文件行数，请重新读取文件状态。"))?;
        let offset = bounded(&input["offset"], range_start as u64, 0, MAX_FILE_BYTES)? as usize;
        if offset < range_start || offset > range_end {
            return Err(invalid(
                "offset 不在指定行范围中，请使用该范围返回的 nextOffset。",
            ));
        }
        page = text_page(text, offset, maximum.min(range_end - offset))?;
        let next = page["nextOffset"].as_u64().unwrap() as usize;
        let more = next < range_end;
        page["hasMore"] = json!(more);
        page["lineRangeHasMore"] = json!(more);
        page["hasMoreInFile"] = json!(next < units.len());
        page["nextLine"] = if more {
            json!(
                start_line
                    + units[range_start..next]
                        .iter()
                        .filter(|unit| **unit == 10)
                        .count()
            )
        } else {
            Value::Null
        };
        page["lineRange"] = json!({"startLine":start_line,"endLine":end_line.min(total_lines),"totalLines":total_lines,
            "startOffset":range_start,"endOffset":range_end});
    } else {
        page = text_page(
            text,
            bounded(&input["offset"], 0, 0, MAX_FILE_BYTES)? as usize,
            maximum,
        )?;
    }
    page["path"] = json!(path_text(path));
    page["sha256"] = json!(sha256(&bytes));
    page["bytes"] = json!(bytes.len());
    Ok(page)
}

fn atomic_text(
    path: &Path,
    bytes: &[u8],
    expected: &Value,
    binding: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
) -> ServiceResult<Value> {
    check_hash(path, expected)?;
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
        check_hash(path, expected)?;
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
        return read_page(&path, input);
    }
    if operation == "file.stat" {
        let metadata = inspect(&path, false, true)?.ok_or_else(|| invalid("路径不是普通文件。"))?;
        let hash = if metadata.is_file() && metadata.len() <= MAX_FILE_BYTES {
            json!(sha256(&read_bounded(&path, MAX_FILE_BYTES)?))
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
    let key = path_key(&path);
    let lock = {
        let mut locks = FILE_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = locks
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(Mutex::new(())));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    };
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
            atomic_text(
                &path,
                content.as_bytes(),
                &input["expectedHash"],
                binding,
                cancellation,
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
