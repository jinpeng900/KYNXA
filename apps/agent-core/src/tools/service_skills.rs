use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde_json::{Value, json};

use super::intent::{matches, utf16_length};
use super::local_files::{absolute_path, inspect, path_text, read_checked, sha256, within};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::string;

const FILE_BYTES: u64 = 2 * 1024 * 1024;
const TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FILES: usize = 128;
const MAX_ENTRIES: usize = 512;
const MAX_DEPTH: usize = 8;

fn cancelled(cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<()> {
    if cancellation
        .as_ref()
        .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
    {
        return Err(ServiceError::new("TOOL_CANCELLED", "技能读取已取消。", 409));
    }
    Ok(())
}

pub fn private_resource(path: &str) -> bool {
    path.replace('\\', "/").split('/').any(|part| {
        let lower = part.to_lowercase();
        [".git", ".ssh", ".aws", ".azure", ".gnupg", ".kube", ".codex", ".config", ".npm", ".docker"].contains(&lower.as_str())
            || matches(r"(?i)^\.env(?:[._-]|$)|^(?:id_(?:rsa|dsa|ecdsa|ed25519)|authorized_keys|known_hosts)$|\.(?:pem|key|pfx|p12|jks|keystore)$|^(?:connections|credentials|secrets|tokens)(?:[._-][^.]+)*\.(?:json|ya?ml|toml|ini|conf|env)$", part)
    })
}

pub fn mime_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "md" => "text/markdown",
        "txt" | "ps1" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "js" | "mjs" | "cjs" => "text/javascript",
        "py" => "text/x-python",
        "sh" => "text/x-shellscript",
        "html" => "text/html",
        "css" => "text/css",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

pub fn resolve_resource(root: &Path, value: &Value) -> ServiceResult<(PathBuf, String)> {
    // Normalize the same public drive-path form after Windows canonicalization adds an extended prefix.
    // Windows 真实路径增加扩展前缀后，统一使用相同的公开盘符路径形式比较包内路径。
    let normalized_root = absolute_path(&path_text(root));
    let root = normalized_root.as_path();
    let path = value
        .as_str()
        .filter(|value| {
            !value.trim().is_empty()
                && utf16_length(value) <= 2048
                && !value.contains(['\0', '\r', '\n', ':'])
        })
        .ok_or_else(|| {
            ServiceError::new(
                "INVALID_SKILL_RESOURCE_PATH",
                "Skill resource requires a relative package path.",
                400,
            )
        })?;
    if Path::new(path).is_absolute() || path.starts_with(['/', '\\']) {
        return Err(ServiceError::new(
            "INVALID_SKILL_RESOURCE_PATH",
            "Skill resource requires a relative package path.",
            400,
        ));
    }
    let portable = path.replace('\\', "/");
    let parts = portable.split('/').collect::<Vec<_>>();
    if parts.iter().any(|part| {
        *part == ".."
            || (*part != "." && part.ends_with(['.', ' ']))
            || matches(r"(?i)^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)", part)
    }) {
        return Err(ServiceError::new(
            "UNSAFE_SKILL_RESOURCE_PATH",
            "Skill resource path crosses a package boundary or uses an unsafe name.",
            403,
        ));
    }
    let target = absolute_path(&path_text(
        &parts
            .iter()
            .fold(root.to_owned(), |path, part| path.join(part)),
    ));
    let relative = target
        .strip_prefix(root)
        .ok()
        .map(path_text)
        .unwrap_or_default()
        .replace('\\', "/");
    if !within(root, &target) || relative.is_empty() {
        return Err(ServiceError::new(
            "UNSAFE_SKILL_RESOURCE_PATH",
            "Skill resource must be a file inside this package.",
            403,
        ));
    }
    if private_resource(&relative) {
        return Err(ServiceError::new(
            "PROTECTED_SKILL_RESOURCE",
            "Credential resources cannot be read or copied from skills.",
            403,
        ));
    }
    Ok((target, relative))
}

#[cfg(all(test, windows))]
mod canonical_resource_tests {
    #[test]
    fn canonical_windows_prefix_does_not_turn_a_package_file_into_an_escape() {
        let (path, relative) = super::resolve_resource(
            std::path::Path::new(r"\\?\C:\fixture\skill"),
            &serde_json::json!("scripts/check.mjs"),
        )
        .unwrap();
        assert_eq!(relative, "scripts/check.mjs");
        assert_eq!(
            super::path_text(&path),
            r"C:\fixture\skill\scripts\check.mjs"
        );
        assert!(
            super::resolve_resource(
                std::path::Path::new(r"\\?\C:\fixture\skill"),
                &serde_json::json!("../secret.txt")
            )
            .is_err()
        );
    }
}

fn bounded(
    value: &Value,
    parameter: &str,
    fallback: u64,
    minimum: u64,
    maximum: u64,
) -> ServiceResult<u64> {
    if value.is_null() {
        return Ok(fallback);
    }
    value
        .as_u64()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| {
            ServiceError::new(
                "INVALID_SKILL_RESOURCE_PAGINATION",
                &format!("skill.resource.read 的 {parameter} 必须在 {minimum}–{maximum} 范围内。"),
                400,
            )
            .with_details(json!({"tool":"skill.resource.read","parameter":parameter,"minimum":minimum,"maximum":maximum}))
        })
}

fn read_file(arguments: &Value) -> ServiceResult<Value> {
    let path = absolute_path(string(&arguments["path"]));
    let maximum = arguments["maxBytes"].as_u64().unwrap_or(FILE_BYTES);
    let bytes = read_checked(&path, maximum, false, "APP_SKILL_CHANGED")?;
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(json!({"bytesHex":hex,"size":bytes.len(),"sha256":sha256(&bytes)}))
}

fn read_resource(arguments: &Value) -> ServiceResult<Value> {
    let root = absolute_path(string(&arguments["root"]));
    let (path, relative) = resolve_resource(&root, &arguments["path"])?;
    let offset = bounded(&arguments["offset"], "offset", 0, 0, FILE_BYTES)? as usize;
    let limit = bounded(&arguments["limit"], "limit", 16000, 1, 16000)? as usize;
    inspect(&path, false, false)?;
    let canonical_root = fs::canonicalize(&root).map_err(ServiceError::io)?;
    let canonical_path = fs::canonicalize(&path).map_err(ServiceError::io)?;
    if !within(&canonical_root, &canonical_path) {
        return Err(ServiceError::new(
            "UNSAFE_SKILL_RESOURCE_PATH",
            "Skill resource crosses its canonical package boundary.",
            403,
        ));
    }
    let canonical_relative = canonical_path
        .strip_prefix(&canonical_root)
        .map(path_text)
        .unwrap_or_default()
        .replace('\\', "/");
    resolve_resource(&canonical_root, &json!(canonical_relative))?;
    let bytes = read_checked(&path, FILE_BYTES, false, "APP_SKILL_CHANGED")?;
    let mut result = json!({"path":relative,"mimeType":mime_type(&relative),"byteLength":bytes.len(),"sha256":sha256(&bytes)});
    match std::str::from_utf8(&bytes)
        .ok()
        .filter(|content| !content.contains('\0'))
    {
        None => {
            result["kind"] = json!("binary");
            result["encoding"] = Value::Null;
            result["content"] = Value::Null;
            result["readable"] = json!(false);
            result["message"] = json!(
                "Binary asset retained in the package; the text reader does not decode it or expose base64."
            );
        }
        Some(content) => {
            let content = content.strip_prefix('\u{feff}').unwrap_or(content);
            let characters = content.chars().collect::<Vec<_>>();
            let next_offset = characters.len().min(offset.saturating_add(limit));
            result["kind"] = json!("text");
            result["encoding"] = json!("utf-8");
            result["readable"] = json!(true);
            result["content"] = json!(
                characters
                    .get(offset..next_offset)
                    .unwrap_or(&[])
                    .iter()
                    .collect::<String>()
            );
            result["offset"] = json!(offset);
            result["nextOffset"] = json!(next_offset);
            result["totalChars"] = json!(characters.len());
            result["hasMore"] = json!(next_offset < characters.len());
        }
    }
    Ok(result)
}

fn diagnostic(code: &str, message: &str, path: Option<&str>, severity: &str) -> Value {
    let mut result = json!({"code":code,"message":message,"severity":severity});
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        result["path"] = json!(path);
    }
    result
}

struct Candidate {
    path: PathBuf,
    relative: String,
    depth: usize,
    is_directory: bool,
    size: u64,
}

#[derive(Default)]
pub struct SkillInventory {
    root: PathBuf,
    pending: VecDeque<(PathBuf, usize)>,
    authorization: Option<Candidate>,
    files: Vec<Value>,
    diagnostics: Vec<Value>,
    visited: usize,
    total_bytes: u64,
    truncated: bool,
    stopped: bool,
    hashes: bool,
}

impl SkillInventory {
    fn fail_candidate(&mut self, candidate: &Candidate, error: ServiceError) {
        self.diagnostics.push(diagnostic(
            &error.code,
            "Skill resource is unavailable or failed the package safety checks.",
            Some(&candidate.relative),
            "error",
        ));
    }

    fn accept(
        &mut self,
        allowed: bool,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<()> {
        let Some(candidate) = self.authorization.take() else {
            return Ok(());
        };
        if !allowed {
            self.diagnostics.push(diagnostic(
                "PROTECTED_SKILL_RESOURCE",
                "Private application data cannot be exposed through skill resources.",
                Some(&candidate.relative),
                "error",
            ));
            return Ok(());
        }
        cancelled(cancellation)?;
        if candidate.is_directory {
            if candidate.depth > MAX_DEPTH {
                self.truncated = true;
                self.diagnostics.push(diagnostic(
                    "SKILL_PACKAGE_DEPTH",
                    "Skill package nesting exceeds the safe inventory limit.",
                    Some(&candidate.relative),
                    "error",
                ));
                return Ok(());
            }
            let entries = match fs::read_dir(&candidate.path) {
                Ok(entries) => entries,
                Err(error) => {
                    self.fail_candidate(&candidate, ServiceError::io(error));
                    return Ok(());
                }
            };
            let mut paths = Vec::new();
            for entry in entries {
                cancelled(cancellation)?;
                self.visited += 1;
                if self.visited > MAX_ENTRIES {
                    self.truncated = true;
                    self.stopped = true;
                    break;
                }
                match entry {
                    Ok(entry) => paths.push(entry.path()),
                    Err(error) => {
                        self.fail_candidate(&candidate, ServiceError::io(error));
                    }
                }
            }
            paths.sort();
            for path in paths.into_iter().rev() {
                self.pending.push_front((path, candidate.depth + 1));
            }
            return Ok(());
        }
        if self.files.len() >= MAX_FILES
            || candidate.size > FILE_BYTES
            || self.total_bytes + candidate.size > TOTAL_BYTES
        {
            self.truncated = true;
            self.stopped = true;
            self.diagnostics.push(diagnostic(
                "SKILL_PACKAGE_LIMIT",
                "Skill package exceeds its file count or byte budget.",
                Some(&candidate.relative),
                "error",
            ));
            return Ok(());
        }
        let mut file = json!({"path":path_text(&candidate.path),"relativePath":candidate.relative,"size":candidate.size,"mimeType":mime_type(&candidate.relative)});
        if self.hashes {
            match read_checked(&candidate.path, FILE_BYTES, false, "APP_SKILL_CHANGED") {
                Ok(bytes) => {
                    file["sha256"] = json!(sha256(&bytes));
                    file["size"] = json!(bytes.len());
                }
                Err(error) => {
                    self.fail_candidate(&candidate, error);
                    return Ok(());
                }
            }
        }
        self.total_bytes += file["size"].as_u64().unwrap_or(0);
        self.files.push(file);
        Ok(())
    }

    fn next(&mut self, cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<Value> {
        while !self.stopped {
            cancelled(cancellation)?;
            let Some((path, depth)) = self.pending.pop_front() else {
                break;
            };
            let relative = path
                .strip_prefix(&self.root)
                .map(path_text)
                .unwrap_or_default()
                .replace('\\', "/");
            if !relative.is_empty() && private_resource(&relative) {
                self.diagnostics.push(diagnostic(
                    "PROTECTED_SKILL_RESOURCE",
                    "Credential resources are excluded from skill access and snapshots.",
                    Some(&relative),
                    "error",
                ));
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if ["node_modules", ".venv", "venv", "__pycache__"].contains(&name.as_str()) {
                self.diagnostics.push(diagnostic("SKILL_DEPENDENCIES_EXCLUDED", "Installed dependencies are excluded; requirements must be checked without auto-installing.", Some(&relative), "warning"));
                continue;
            }
            let metadata = match inspect(&path, false, false) {
                Ok(Some(metadata)) => metadata,
                Ok(None) => continue,
                Err(error) => {
                    self.diagnostics.push(diagnostic(
                        &error.code,
                        "Skill resource is unavailable or failed the package safety checks.",
                        Some(&relative),
                        "error",
                    ));
                    continue;
                }
            };
            if !metadata.is_dir() && !metadata.is_file() {
                self.diagnostics.push(diagnostic(
                    "UNSAFE_SKILL_RESOURCE",
                    "Only regular skill resource files are supported.",
                    Some(&relative),
                    "error",
                ));
                continue;
            }
            if !relative.is_empty() {
                if let Err(error) = resolve_resource(&self.root, &json!(relative)) {
                    self.diagnostics.push(diagnostic(
                        &error.code,
                        "Skill resource is unavailable or failed the package safety checks.",
                        Some(&relative),
                        "error",
                    ));
                    continue;
                }
            }
            let canonical = fs::canonicalize(&path).map_err(ServiceError::io)?;
            self.authorization = Some(Candidate {
                path: path.clone(),
                relative,
                depth,
                is_directory: metadata.is_dir(),
                size: metadata.len(),
            });
            return Ok(
                json!({"authorization":{"path":path_text(&path),"canonicalPath":path_text(&canonical)}}),
            );
        }
        if self.visited > MAX_ENTRIES {
            self.diagnostics.push(diagnostic(
                "SKILL_PACKAGE_ENTRIES",
                "Skill package exceeds its directory entry budget.",
                None,
                "error",
            ));
        }
        self.files.sort_by(|left, right| {
            string(&left["relativePath"]).cmp(string(&right["relativePath"]))
        });
        Ok(
            json!({"inventory":{"skillRoot":path_text(&self.root),"files":self.files,"totalBytes":self.total_bytes,
            "truncated":self.truncated,"valid":!self.truncated && !self.diagnostics.iter().any(|diagnostic| diagnostic["severity"] == "error"),"diagnostics":self.diagnostics}}),
        )
    }

    pub fn apply(
        &mut self,
        operation: &str,
        arguments: &Value,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<Value> {
        cancelled(cancellation)?;
        match operation {
            "skill.resolve" => {
                let (path, relative) = resolve_resource(
                    &absolute_path(string(&arguments["root"])),
                    &arguments["path"],
                )?;
                Ok(json!({"path":path_text(&path),"relativePath":relative}))
            }
            "skill.private" => Ok(json!(private_resource(string(&arguments["path"])))),
            "skill.mime" => Ok(json!(mime_type(string(&arguments["path"])))),
            "skill.runtime" => Ok(json!(match Path::new(string(&arguments["path"]))
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_lowercase()
                .as_str()
            {
                "mjs" | "cjs" | "js" => "node",
                "py" => "python",
                "ps1" => "powershell",
                "sh" | "bash" => "bash",
                "cmd" | "bat" => "cmd",
                _ => "unknown",
            })),
            "skill.readFile" => read_file(arguments),
            "skill.readResource" => read_resource(arguments),
            "skill.inventoryInit" => {
                *self = Self::default();
                self.root = absolute_path(string(&arguments["root"]));
                self.hashes = arguments["hashes"] == true;
                if !inspect(&self.root, false, false)?.is_some_and(|metadata| metadata.is_dir()) {
                    return Err(ServiceError::new(
                        "INVALID_APP_SKILL",
                        "Skill package root is not a directory.",
                        400,
                    ));
                }
                self.pending.push_back((self.root.clone(), 0));
                self.next(cancellation)
            }
            "skill.inventoryNext" => {
                self.accept(arguments["allowed"] == true, cancellation)?;
                self.next(cancellation)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown skill resource operation.",
                400,
            )),
        }
    }
}

#[cfg(test)]
mod pagination_tests {
    use super::SkillInventory;
    use serde_json::json;

    #[test]
    fn resource_page_rejects_invalid_offset_and_limit_at_the_service_boundary() {
        let root = std::env::temp_dir().join("kynxa-skill-pagination-test");
        for (arguments, parameter) in [
            (json!({"root":root,"path":"notes.md","offset":2_097_153}), "offset"),
            (json!({"root":root,"path":"notes.md","limit":0}), "limit"),
            (json!({"root":root,"path":"notes.md","limit":16_001}), "limit"),
            (json!({"root":root,"path":"notes.md","limit":"128"}), "limit"),
        ] {
            let error = SkillInventory::default()
                .apply("skill.readResource", &arguments, &None)
                .unwrap_err();
            assert_eq!(error.code, "INVALID_SKILL_RESOURCE_PAGINATION");
            assert!(error.message.contains(parameter), "{}", error.message);
            assert_eq!(error.details["parameter"], parameter);
            assert_eq!(error.details["tool"], "skill.resource.read");
        }
    }
}
