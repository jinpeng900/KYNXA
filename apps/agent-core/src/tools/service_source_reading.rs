use std::collections::{BTreeMap, BinaryHeap, HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::local_files::{
    absolute_path, file_version, handle_version, inspect, metadata_projection, open_read,
    path_text, read_checked, within,
};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string, truthy};

const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "mjs", "cjs", "js", "jsx", "ts", "mts", "cts", "tsx", "cs", "xaml",
    "html", "css", "json", "yaml", "yml", "xml", "toml", "csv", "py", "pyi", "rs", "go", "java",
    "cpp", "c", "h", "sql", "ps1", "log", "rst", "adoc", "asciidoc", "tex", "bib", "org",
    "ini", "cfg", "conf", "env", "properties", "tsv", "ndjson", "jsonl", "ipynb", "srt", "vtt",
    "scss", "sass", "less", "vue", "svelte", "kt", "kts", "swift", "rb", "php", "sh", "bat",
    "cmd", "lua", "r", "rmd", "jl", "dart", "scala", "clj", "ex", "exs", "erl", "hrl",
];
const IGNORED: &[&str] = &[
    ".git",
    ".vs",
    ".idea",
    "node_modules",
    "bin",
    "obj",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".kynxa",
    ".sandbox-runtime",
    ".sandbox-temp",
];
const WINDOW: usize = 128 * 1024;
const WINDOW_VERSION: &str = "text-file-window-v1";
fn number(value: &Value, fallback: u64) -> u64 {
    value.as_u64().unwrap_or(fallback)
}
fn error(code: &str) -> ServiceError {
    let status = if code.contains("LIMIT") {
        413
    } else if code.starts_with("STALE") || code == "RETRIEVAL_FULL_SCAN_REQUIRED" {
        409
    } else if code.contains("PROTECTED") || code == "UNSAFE_TOOL_PATH" {
        403
    } else {
        400
    };
    ServiceError::new(
        code,
        "来源读取或扫描未通过当前原始文件、范围及预算验证。",
        status,
    )
}
fn cancelled(token: &Option<Arc<AtomicBool>>) -> ServiceResult<()> {
    if crate::service_cancellation::is_cancelled()
        || token
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Acquire))
    {
        Err(ServiceError::new("ABORT_ERR", "来源读取已取消。", 409))
    } else {
        Ok(())
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn document_format(path: &Path) -> Option<&'static str> {
    crate::document_formats::detect(path).map(|format| format.name)
}
fn version(path: &Path) -> Value {
    crate::document_formats::detect(path).map_or(Value::Null, |format| json!(format.version))
}
pub(crate) fn supported(path: &Path) -> bool {
    document_format(path).is_some()
        || TEXT_EXTENSIONS.contains(
            &path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_lowercase()
                .as_str(),
        )
}
fn metadata(path: &Path, info: std::fs::Metadata) -> ServiceResult<Value> {
    let fact = metadata_projection(path, info)?;
    Ok(
        json!({"sizeBytes": fact["size"], "mtimeMs": fact["mtimeMs"], "ctimeMs": fact["ctimeMs"], "device": fact["dev"], "inode": fact["ino"]}),
    )
}
// Public metadata follows Node Number round-trips; opened object identity remains exact FileVersion below.
// 公开元数据遵循 Node Number 往返；后文打开文件对象的身份仍按精确 FileVersion 核验。
fn same(left: &Value, right: &Value) -> bool {
    !left.is_null()
        && ["sizeBytes", "mtimeMs", "ctimeMs", "device", "inode"]
            .iter()
            .all(|field| {
                left[*field]
                    .as_f64()
                    .zip(right[*field].as_f64())
                    .map(|(left, right)| left == right)
                    .unwrap_or(left[*field] == right[*field])
            })
}
fn classification(args: &Value) -> Value {
    let raw = string(&args["code"]);
    let code = if regex::Regex::new("^[A-Z][A-Z0-9_]{0,127}$")
        .unwrap()
        .is_match(raw)
    {
        raw
    } else {
        "RETRIEVAL_SOURCE_FAILED"
    };
    let (category, retryable) = if args["name"] == "AbortError" {
        ("cancelled", false)
    } else if [
        "EACCES",
        "EPERM",
        "UNSAFE_TOOL_PATH",
        "PROTECTED_RETRIEVAL_SOURCE",
    ]
    .contains(&code)
    {
        ("permission", false)
    } else if code.starts_with("OCR_") {
        ("ocr-unavailable", false)
    } else if code.contains("STALE") || code.contains("SOURCE_CHANGED") || code == "ENOENT" {
        ("source-changed", true)
    } else if code.contains("RESOURCE") || code.contains("DECODER_BUSY") {
        ("resource", true)
    } else if [
        "EBUSY",
        "EAGAIN",
        "EMFILE",
        "ENFILE",
        "ETIMEDOUT",
        "DOCUMENT_DECODER_TIMEOUT",
    ]
    .contains(&code)
    {
        ("temporary", true)
    } else if code.contains("LIMIT")
        || code.contains("BUDGET")
        || code == "INVALID_RETRIEVAL_SOURCE"
    {
        ("limit", false)
    } else if code.contains("UNSUPPORTED") {
        ("unsupported", false)
    } else {
        ("invalid-source", false)
    };
    json!({"errorCode": code, "category": category, "retryable": retryable})
}
fn limits_key(options: &Value) -> String {
    json!([
        number(&options["maximumSourceBytes"], 32 * 1024 * 1024),
        number(&options["maximumDocumentInputBytes"], 32 * 1024 * 1024),
        number(&options["maximumDocumentOutputBytes"], 2 * 1024 * 1024),
        number(&options["maximumPdfPages"], 100),
        if options["ocrDisabled"] == true {
            "disabled"
        } else {
            "available"
        }
    ])
    .to_string()
}
fn add(stats: &mut Value, field: &str, value: u64) {
    stats[field] = json!(number(&stats[field], 0).saturating_add(value));
}
fn limit(
    stats: &mut Value,
    options: &Value,
    dimension: &str,
    maximum: u64,
    observed: u64,
) -> ServiceResult<bool> {
    stats["incomplete"] = json!(true);
    stats["limit"] = json!({"dimension": dimension, "limit": maximum, "observed": observed, "unvisitedCountKnown": false});
    if truthy(&options["allowPartial"]) {
        Ok(false)
    } else {
        Err(error("RETRIEVAL_SCAN_LIMIT").with_details(json!({"limit": stats["limit"]})))
    }
}
fn record_failure(stats: &mut Value, relative: &str, failure: &Value, directory: bool) {
    add(stats, "failedFiles", 1);
    if !stats["failures"].is_array() {
        stats["failures"] = json!([]);
    }
    if array(&stats["failures"]).len() >= 10000 {
        stats["failureReportTruncated"] = json!(true);
        return;
    }
    let mut row = classification(failure);
    row["relativePath"] = json!(relative);
    row["attempts"] = json!(number(&failure["attempts"], 1));
    if directory {
        row["directory"] = json!(true);
    }
    if !failure["documentCoverage"].is_null() {
        row["documentCoverage"] = failure["documentCoverage"].clone();
    }
    stats["failures"].as_array_mut().unwrap().push(row);
}

/// Full scan ownership includes traversal/ignore rules, budgets, retries and reuse; SDK bodies remain external.
/// 完整扫描拥有遍历/忽略规则、预算、重试与复用；SDK 正文留在外部，失败清单不证明删除。
#[derive(Default)]
pub(crate) struct SourceReadingState {
    root: PathBuf,
    options: Value,
    stats: Value,
    directories: VecDeque<(PathBuf, Vec<Arc<Gitignore>>)>,
    entries: VecDeque<(PathBuf, Vec<Arc<Gitignore>>)>,
    documents: BTreeMap<PathBuf, Vec<Arc<Gitignore>>>,
    entries_truncated: bool,
    dirty: VecDeque<String>,
    dirty_mode: bool,
    single_file: bool,
    single_claimed: bool,
    window_count: u64,
    bytes: u64,
    visited: u64,
    cursors: HashMap<u64, Value>,
    sequence: u64,
}
impl SourceReadingState {
    fn excludes(&self, path: &Path) -> bool {
        array(&self.options["excludedRoots"])
            .iter()
            .any(|root| within(&absolute_path(string(root)), path))
    }
    fn next(&mut self, token: &Option<Arc<AtomicBool>>) -> ServiceResult<Value> {
        loop {
            cancelled(token)?;
            if self.single_file {
                if self.single_claimed {
                    return Ok(Value::Null);
                }
                self.single_claimed = true;
                add(&mut self.stats, "discoveredFiles", 1);
                return Ok(
                    json!({"path": path_text(&self.root), "relativePath": "", "single": true}),
                );
            }
            if self.dirty_mode {
                let Some(relative) = self.dirty.pop_front() else {
                    return Ok(Value::Null);
                };
                let candidate = absolute_path(&path_text(&self.root.join(&relative)));
                if !within(&self.root, &candidate) || candidate == self.root {
                    return Err(error("RETRIEVAL_FULL_SCAN_REQUIRED"));
                }
                add(&mut self.stats, "visitedEntries", 1);
                let mut ancestors = Vec::new();
                let mut ancestor = candidate.parent();
                while let Some(directory) =
                    ancestor.filter(|directory| within(&self.root, directory))
                {
                    ancestors.push(directory.to_owned());
                    if directory == self.root {
                        break;
                    }
                    ancestor = directory.parent();
                }
                ancestors.reverse();
                let mut protected = self.excludes(&candidate)
                    || super::sensitive_files::is_sensitive(&path_text(&candidate));
                for directory in ancestors {
                    if directory != self.root
                        && IGNORED.contains(
                            &directory
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("")
                                .to_lowercase()
                                .as_str(),
                        )
                    {
                        protected = true;
                    }
                    match load_ignore(&directory) {
                        Ok(Some(rule)) => {
                            if rule
                                .matched_path_or_any_parents(&candidate, false)
                                .is_ignore()
                            {
                                protected = true;
                            }
                        }
                        Ok(None) => {}
                        Err(failure) => {
                            record_failure(
                                &mut self.stats,
                                &relative,
                                &json!({"code": failure.code}),
                                false,
                            );
                            return Ok(
                                json!({"path": path_text(&candidate), "relativePath": relative, "failed": true, "errorCode": failure.code}),
                            );
                        }
                    }
                }
                if protected {
                    return Ok(
                        json!({"path": path_text(&candidate), "relativePath": relative, "missing": true}),
                    );
                }
                match inspect(&candidate, true, false) {
                    Ok(None) => {
                        return Ok(
                            json!({"path": path_text(&candidate), "relativePath": relative, "missing": true}),
                        );
                    }
                    Ok(Some(info)) if info.is_dir() => {
                        return Err(error("RETRIEVAL_FULL_SCAN_REQUIRED"));
                    }
                    Ok(Some(info)) => {
                        if !supported(&candidate) {
                            return Ok(
                                json!({"path": path_text(&candidate), "relativePath": relative, "missing": true}),
                            );
                        }
                        add(&mut self.stats, "discoveredFiles", 1);
                        return Ok(
                            json!({"path": path_text(&candidate), "relativePath": relative, "metadata": metadata(&candidate, info)?}),
                        );
                    }
                    Err(failure) => {
                        record_failure(
                            &mut self.stats,
                            &relative,
                            &json!({"code": failure.code}),
                            false,
                        );
                        return Ok(
                            json!({"path": path_text(&candidate), "relativePath": relative, "failed": true, "errorCode": failure.code}),
                        );
                    }
                }
            }
            // SDK documents wait until all admitted text directories have been traversed.
            // SDK 文档延后到已准入文本目录遍历完成；资源等待不能阻住后续可读文本。
            let queued = self.entries.pop_front().or_else(|| {
                if self.directories.is_empty() {
                    self.documents.pop_first()
                } else {
                    None
                }
            });
            if let Some((candidate, rules)) = queued {
                add(&mut self.stats, "visitedEntries", 1);
                self.visited += 1;
                if self.visited > number(&self.options["maximumEntries"], 20000)
                    && !limit(
                        &mut self.stats,
                        &self.options,
                        "entries",
                        number(&self.options["maximumEntries"], 20000),
                        self.visited,
                    )?
                {
                    return Ok(Value::Null);
                }
                let info = match fs::symlink_metadata(&candidate) {
                    Ok(info) => info,
                    Err(failure) => {
                        record_failure(
                            &mut self.stats,
                            &relative_path(&self.root, &candidate),
                            &json!({"code": ServiceError::io(failure).code}),
                            false,
                        );
                        continue;
                    }
                };
                if super::local_files::is_link(&info)
                    || super::sensitive_files::is_sensitive(&path_text(&candidate))
                    || rules.iter().any(|rule| {
                        rule.matched_path_or_any_parents(&candidate, info.is_dir())
                            .is_ignore()
                    })
                {
                    continue;
                }
                if info.is_dir() {
                    if !IGNORED.contains(
                        &candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("")
                            .to_lowercase()
                            .as_str(),
                    ) {
                        self.directories.push_back((candidate, rules));
                    }
                    continue;
                }
                if !info.is_file() || !supported(&candidate) {
                    continue;
                }
                add(&mut self.stats, "discoveredFiles", 1);
                let info = match inspect(&candidate, false, false) {
                    Ok(Some(info)) => info,
                    Ok(None) => continue,
                    Err(failure) => {
                        record_failure(
                            &mut self.stats,
                            &relative_path(&self.root, &candidate),
                            &json!({"code": failure.code}),
                            false,
                        );
                        continue;
                    }
                };
                return Ok(
                    json!({"path": path_text(&candidate), "relativePath": relative_path(&self.root, &candidate), "metadata": metadata(&candidate, info)?}),
                );
            }
            let Some((directory, mut rules)) = self.directories.pop_front() else {
                if self.entries_truncated {
                    let maximum = number(&self.options["maximumEntries"], 20000);
                    limit(
                        &mut self.stats,
                        &self.options,
                        "entries",
                        maximum,
                        maximum.saturating_add(1),
                    )?;
                }
                return Ok(Value::Null);
            };
            if self.excludes(&directory) {
                continue;
            }
            let failure = (|| {
                inspect(&directory, false, false)?;
                if let Some(rule) = load_ignore(&directory)? {
                    rules.push(Arc::new(rule));
                }
                // Retain a bounded stable prefix, rather than an OS-dependent read_dir prefix.
                // 只保留有界稳定前缀，避免按操作系统 read_dir 顺序截断，也不缓存整棵树正文。
                let maximum = number(&self.options["maximumEntries"], 20000);
                let capacity = maximum.saturating_sub(self.visited).saturating_add(1) as usize;
                let mut candidates = BinaryHeap::new();
                for entry in fs::read_dir(&directory).map_err(ServiceError::io)? {
                    cancelled(token)?;
                    let entry = entry.map_err(ServiceError::io)?;
                    let path = entry.path();
                    let document = document_format(&path).is_some()
                        && entry.file_type().is_ok_and(|kind| kind.is_file());
                    candidates.push((document, path));
                    if candidates.len() > capacity {
                        candidates.pop();
                        self.entries_truncated = true;
                    }
                }
                for (document, path) in candidates.into_sorted_vec() {
                    if document {
                        self.documents.insert(path, rules.clone());
                        if self.documents.len() as u64 > maximum.saturating_add(1) {
                            self.documents.pop_last();
                            self.entries_truncated = true;
                        }
                    } else {
                        self.entries.push_back((path, rules.clone()));
                    }
                }
                Ok::<(), ServiceError>(())
            })();
            if let Err(failure) = failure {
                if failure.code == "ABORT_ERR" || directory == self.root {
                    return Err(failure);
                }
                record_failure(
                    &mut self.stats,
                    &relative_path(&self.root, &directory),
                    &json!({"code": failure.code}),
                    true,
                );
                self.stats["incomplete"] = json!(true);
            }
        }
    }
    pub(crate) fn apply(
        &mut self,
        operation: &str,
        args: &Value,
        token: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<Value> {
        let operation = operation
            .strip_prefix("source.reading.")
            .unwrap_or(operation);
        match operation {
            "classify" => Ok(classification(args)),
            "retry" => Ok(
                json!({"retry": args["aborted"] != true && number(&args["attempt"], 1) < 2 && classification(&args["error"])["retryable"] == true, "delayMs": 25}),
            ),
            "version" => Ok(version(Path::new(string(&args["path"])))),
            "sameMetadata" => Ok(json!(same(&args["previous"], &args["current"]))),
            "filePolicy" => {
                let path = absolute_path(string(&args["path"]));
                let root = args["root"].as_str().map(absolute_path);
                if root.as_ref().is_some_and(|root| !within(root, &path))
                    || super::sensitive_files::is_sensitive(&path_text(&path))
                    || array(&args["excludedRoots"])
                        .iter()
                        .any(|root| within(&absolute_path(string(root)), &path))
                {
                    return Err(error("PROTECTED_RETRIEVAL_SOURCE"));
                }
                if !supported(&path) || args["window"] == true && document_format(&path).is_some() {
                    return Err(error(if args["window"] == true {
                        "PROTECTED_RETRIEVAL_SOURCE"
                    } else {
                        "UNSUPPORTED_RETRIEVAL_SOURCE"
                    }));
                }
                Ok(
                    json!({"format": document_format(&path), "version": version(&path), "inputBytes": number(&args["maximumSourceBytes"], 32 * 1024 * 1024).min(number(&args["maximumDocumentInputBytes"], 32 * 1024 * 1024)),
                    "outputBytes": number(&args["maximumSourceBytes"], 32 * 1024 * 1024).min(number(&args["maximumDocumentOutputBytes"], 2 * 1024 * 1024)), "maximumPages": number(&args["maximumPdfPages"], 100)}),
                )
            }
            "bodyPlan" => {
                if args["containsNull"] == true {
                    return Err(error("UNSUPPORTED_RETRIEVAL_SOURCE"));
                }
                Ok(json!({"windowed": number(&args["bytes"], 0) > 2 * 1024 * 1024}))
            }
            "oversized" => Err(error("INVALID_RETRIEVAL_SOURCE")),
            "importPlan" => Ok(
                json!({"skip": truthy(&args["file"]["failed"]) || truthy(&args["file"]["skipped"]), "windows": args["file"]["windows"].is_array(), "load": args["file"].get("text").is_none() && !args["file"]["windows"].is_array(), "pageWindow": if args["file"]["extraction"]["pageWindow"].is_object() { json!({"startPage": args["file"]["extraction"]["pageWindow"]["startPage"], "endPage": args["file"]["extraction"]["pageWindow"]["endPage"], "rawContentHash": args["file"]["extraction"]["rawContentHash"]}) } else { Value::Null }}),
            ),
            "windowTitle" => Ok(json!(format!(
                "{} [{}-{}]",
                string(&args["title"]),
                args["window"]["startOffset"],
                args["window"]["endOffset"]
            ))),
            "scanOpenStats" => {
                self.stats = args
                    .get("stats")
                    .filter(|stats| stats.is_object())
                    .cloned()
                    .unwrap_or(json!({}));
                Ok(Value::Null)
            }
            "dirtyFailure" => Ok(
                json!({"missing": (["ENOENT", "PROTECTED_RETRIEVAL_SOURCE"].contains(&string(&args["code"]))), "code": if args["code"].is_string() { args["code"].clone() } else { json!("RETRIEVAL_SOURCE_FAILED") }}),
            ),
            "scanOpen" => {
                cancelled(token)?;
                self.root = absolute_path(string(&args["path"]));
                self.options = args["options"].clone();
                self.stats = args
                    .get("stats")
                    .filter(|stats| stats.is_object())
                    .cloned()
                    .unwrap_or(json!({}));
                let info = inspect(&self.root, false, false)?
                    .ok_or_else(|| error("INVALID_RETRIEVAL_SOURCE"))?;
                self.single_file = info.is_file();
                self.dirty_mode = args["dirty"].is_array();
                if !self.single_file && !info.is_dir() || self.dirty_mode && !info.is_dir() {
                    return Err(error("INVALID_RETRIEVAL_SOURCE"));
                }
                self.single_claimed = false;
                self.window_count = 0;
                self.bytes = 0;
                self.visited = 0;
                self.directories.clear();
                self.entries.clear();
                self.documents.clear();
                self.entries_truncated = false;
                self.dirty = array(&args["dirty"])
                    .iter()
                    .map(|value| string(value).into())
                    .collect();
                self.dirty.make_contiguous().sort_by(|left, right| {
                    document_format(Path::new(left))
                        .is_some()
                        .cmp(&document_format(Path::new(right)).is_some())
                        .then_with(|| left.cmp(right))
                });
                if !self.single_file {
                    for field in [
                        "fileReads",
                        "reusedFiles",
                        "scannedFiles",
                        "visitedEntries",
                        "bytesRead",
                    ] {
                        if self.stats[field].is_null() {
                            self.stats[field] = json!(0);
                        }
                    }
                    self.directories.push_back((self.root.clone(), vec![]));
                }
                Ok(Value::Null)
            }
            "scanNext" => self.next(token),
            "scanStats" => Ok(self.stats.clone()),
            "scanClose" => {
                self.directories.clear();
                self.entries.clear();
                self.documents.clear();
                self.dirty.clear();
                Ok(Value::Null)
            }
            "scanFailure" => {
                if args["aborted"] == true
                    || args["error"]["name"] == "AbortError"
                    || args["error"]["code"] == "RETRIEVAL_SCAN_LIMIT"
                {
                    return Ok(json!({"propagate": true, "cache": false}));
                }
                if self.dirty_mode
                    && ["ENOENT", "PROTECTED_RETRIEVAL_SOURCE"]
                        .contains(&string(&args["error"]["code"]))
                {
                    return Ok(json!({"propagate": false, "cache": false}));
                }
                record_failure(
                    &mut self.stats,
                    string(&args["relativePath"]),
                    &args["error"],
                    args["directory"] == true,
                );
                let class = classification(&args["error"]);
                Ok(
                    json!({"cache": class["retryable"] == false && ["unsupported", "invalid-source", "ocr-unavailable", "limit"].contains(&string(&class["category"])), "classification": class,
                    "propagate": args["aborted"] == true || args["error"]["name"] == "AbortError" || args["error"]["code"] == "RETRIEVAL_SCAN_LIMIT"}),
                )
            }
            "cacheRecord" => {
                let class = classification(&args["error"]);
                let mut cached_error = class.clone();
                cached_error["code"] = class["errorCode"].clone();
                if !args["error"]["documentCoverage"].is_null() {
                    cached_error["details"] =
                        json!({"documentCoverage": args["error"]["documentCoverage"]});
                }
                Ok(
                    json!({"metadata": args["metadata"], "extractionVersion": version(Path::new(string(&args["path"]))), "limitsKey": limits_key(&args["options"]), "cachedAt": args["now"], "error": cached_error,
                    "evict": array(&args["keys"]).iter().take(array(&args["keys"]).len().saturating_sub(255)).collect::<Vec<_>>()}),
                )
            }
            "reusePlan" => {
                let previous = &args["previous"];
                let failed = &args["failed"];
                let current = &args["metadata"];
                let path = Path::new(string(&args["path"]));
                let version = version(path);
                let opts = &args["options"];
                let failed_reused = args["changed"] != true
                    && !failed.is_null()
                    && same(&failed["metadata"], current)
                    && (failed["error"]["category"] != "ocr-unavailable"
                        || args["now"].as_f64().unwrap_or(0.0)
                            - failed["cachedAt"].as_f64().unwrap_or(0.0)
                            < 5000.0)
                    && failed["extractionVersion"] == version
                    && failed["limitsKey"] == limits_key(opts);
                if failed_reused {
                    record_failure(
                        &mut self.stats,
                        string(&args["relativePath"]),
                        &failed["error"],
                        false,
                    );
                    add(&mut self.stats, "reusedFailures", 1);
                    return Ok(json!({"failure": true}));
                }
                let maximum = if version.is_null() {
                    number(&opts["maximumSourceBytes"], 32 * 1024 * 1024)
                } else {
                    number(&opts["maximumSourceBytes"], 32 * 1024 * 1024).min(32 * 1024 * 1024)
                };
                let reused = args["changed"] != true
                    && !previous.is_null()
                    && same(&previous["metadata"], current)
                    && previous["extraction"]
                        .get("version")
                        .cloned()
                        .unwrap_or(Value::Null)
                        == version
                    && (version.is_null()
                        || previous["extraction"]["format"] != "pdf"
                        || previous["documentComplete"] == true
                        || previous["extraction"]["complete"] == true)
                    && previous["textBytes"].as_u64().is_some_and(|bytes| {
                        bytes <= 9_007_199_254_740_991
                            && (previous["documentWindows"].is_array() || bytes <= maximum)
                    })
                    && (version.is_null()
                        || number(&current["sizeBytes"], 0)
                            <= number(&opts["maximumDocumentInputBytes"], 32 * 1024 * 1024)
                            && (previous["documentWindows"].is_array()
                                || number(&previous["textBytes"], 0)
                                    <= number(
                                        &opts["maximumDocumentOutputBytes"],
                                        2 * 1024 * 1024,
                                    ))
                            && previous["extraction"]["pages"]
                                .as_array()
                                .map(|pages| pages.len() as u64)
                                .unwrap_or(number(&previous["extraction"]["pageCount"], 0))
                                <= number(&opts["maximumPdfPages"], 100))
                    && array(&previous["documentWindows"]).iter().all(|window| {
                        array(&window["extraction"]["pages"]).len() as u64
                            <= number(&opts["maximumPdfPages"], 100)
                            && number(&window["textBytes"], 0)
                                <= number(&opts["maximumDocumentOutputBytes"], 2 * 1024 * 1024)
                    });
                Ok(
                    json!({"reused": reused, "oversized": !previous.is_null() && args["changed"] != true && number(&current["sizeBytes"], 0) > maximum, "document": !version.is_null()}),
                )
            }
            "scanObserved" => {
                add(&mut self.stats, "scannedFiles", 1);
                if self.single_file {
                    return Ok(Value::Null);
                }
                if args["reused"] == true {
                    add(&mut self.stats, "reusedFiles", 1);
                } else {
                    add(&mut self.stats, "fileReads", 1);
                    add(&mut self.stats, "bytesRead", number(&args["size"], 0));
                }
                Ok(Value::Null)
            }
            "scanPublish" => {
                if self.dirty_mode {
                    return Ok(json!(true));
                }
                let next = self.window_count + 1;
                let bytes = self.bytes.saturating_add(number(&args["textBytes"], 0));
                if next > number(&self.options["maximumFiles"], 2048)
                    && !limit(
                        &mut self.stats,
                        &self.options,
                        "files",
                        number(&self.options["maximumFiles"], 2048),
                        next,
                    )?
                {
                    return Ok(json!(false));
                }
                if bytes > number(&self.options["maximumBytes"], 32 * 1024 * 1024)
                    && !limit(
                        &mut self.stats,
                        &self.options,
                        "bytes",
                        number(&self.options["maximumBytes"], 32 * 1024 * 1024),
                        bytes,
                    )?
                {
                    return Ok(json!(false));
                }
                self.window_count = next;
                self.bytes = bytes;
                if self.single_file || args["additionalRead"] == true {
                    add(&mut self.stats, "fileReads", 1);
                    add(&mut self.stats, "bytesRead", number(&args["size"], 0));
                }
                if args["pdf"] == true {
                    add(&mut self.stats, "documentWindows", 1);
                }
                Ok(json!(true))
            }
            "documentOpen" => {
                self.sequence += 1;
                self.cursors.insert(self.sequence, Value::Null);
                Ok(json!(self.sequence))
            }
            "documentObserved" => {
                let id = number(&args["id"], 0);
                let previous = self
                    .cursors
                    .get_mut(&id)
                    .ok_or_else(|| error("STALE_RETRIEVAL_SOURCE"))?;
                if !previous.is_null()
                    && (previous["hash"] != args["hash"]
                        || !same(&previous["metadata"], &args["metadata"]))
                {
                    return Err(error("STALE_RETRIEVAL_SOURCE"));
                }
                if previous.is_null() {
                    *previous = json!({"hash": args["hash"], "metadata": args["metadata"]});
                }
                let window = &args["window"];
                let ocr = args["ocrJson"].as_str().unwrap_or("null");
                let key = digest(
                    format!(
                        "[{},{},{},{},{}]",
                        args["hash"], window["startPage"], window["endPage"], args["version"], ocr
                    )
                    .as_bytes(),
                );
                Ok(
                    json!({"key": key, "next": if window["nextPage"].is_null() { Value::Null } else { json!({"startPage": window["nextPage"], "rawContentHash": args["hash"]}) }}),
                )
            }
            "documentClose" => {
                self.cursors.remove(&number(&args["id"], 0));
                Ok(Value::Null)
            }
            "continuationPlan" => Ok(
                json!({"cached": args["documentWindows"].is_array(), "continue": args["extraction"]["format"] == "pdf" && !args["extraction"]["pageWindow"]["nextPage"].is_null(),
                "cursor": {"startPage": args["extraction"]["pageWindow"]["nextPage"], "rawContentHash": args["extraction"]["rawContentHash"]}}),
            ),
            _ => Err(error("AGENT_CORE_UNKNOWN_OPERATION")),
        }
    }
}
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(path_text)
        .unwrap_or_else(|_| path_text(path))
}
fn load_ignore(directory: &Path) -> ServiceResult<Option<Gitignore>> {
    let path = directory.join(".gitignore");
    let bytes = match read_checked(&path, 128 * 1024, false, "STALE_RETRIEVAL_SOURCE") {
        Ok(bytes) => bytes,
        Err(failure)
            if [
                "ENOENT",
                "SKILL_RESOURCE_NOT_FOUND",
                "SKILL_RESOURCE_LIMIT",
                "UNSAFE_TOOL_PATH",
            ]
            .contains(&failure.code.as_str()) =>
        {
            return Ok(None);
        }
        Err(failure) => return Err(failure),
    };
    let mut builder = GitignoreBuilder::new(directory);
    for line in String::from_utf8_lossy(&bytes).lines() {
        builder
            .add_line(None, line)
            .map_err(|_| error("INVALID_RETRIEVAL_SOURCE"))?;
    }
    builder
        .build()
        .map(Some)
        .map_err(|_| error("INVALID_RETRIEVAL_SOURCE"))
}

fn stable_file(
    path: &Path,
    maximum: u64,
) -> ServiceResult<(File, super::local_files::FileVersion)> {
    let original = inspect(path, false, false)?.ok_or_else(|| error("INVALID_RETRIEVAL_SOURCE"))?;
    if !original.is_file() || original.len() > maximum {
        return Err(error("INVALID_RETRIEVAL_SOURCE")
            .with_details(json!({"observedBytes": original.len()})));
    }
    let first = file_version(path, &original)?;
    let file = open_read(path)?;
    let opened = handle_version(&file, &file.metadata().map_err(ServiceError::io)?)?;
    if first != opened || opened.links != 1 {
        return Err(error("UNSAFE_TOOL_PATH"));
    }
    Ok((file, first))
}
fn final_metadata(
    path: &Path,
    file: &File,
    first: &super::local_files::FileVersion,
) -> ServiceResult<Value> {
    let after = inspect(path, false, false)?.ok_or_else(|| error("STALE_RETRIEVAL_SOURCE"))?;
    if file_version(path, &after)? != *first
        || handle_version(file, &file.metadata().map_err(ServiceError::io)?)? != *first
    {
        return Err(error("STALE_RETRIEVAL_SOURCE"));
    }
    metadata(path, after)
}
pub(crate) fn decode(bytes: &[u8], encoding: &str) -> ServiceResult<String> {
    if encoding == "utf-16le" {
        if bytes.len() % 2 != 0 {
            return Err(error("UNSUPPORTED_RETRIEVAL_SOURCE"));
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).map_err(|_| error("UNSUPPORTED_RETRIEVAL_SOURCE"))
    } else {
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| error("UNSUPPORTED_RETRIEVAL_SOURCE"))
    }
}
fn append_window(
    text: &str,
    encoding: &str,
    offset: &mut usize,
    line: &mut usize,
    byte: &mut usize,
    windows: &mut Vec<Value>,
) {
    let characters = text.encode_utf16().count();
    let bytes = if encoding == "utf-16le" {
        characters * 2
    } else {
        text.len()
    };
    let end_line = *line + text.bytes().filter(|byte| *byte == b'\n').count();
    windows.push(json!({"version": WINDOW_VERSION, "encoding": encoding, "offsetUnit": "utf16-code-units", "startOffset": *offset, "endOffset": *offset + characters, "startLine": *line, "endLine": end_line,
        "startByte": *byte, "endByte": *byte + bytes, "contentHash": digest(text.as_bytes()), "textBytes": text.len()}));
    *offset += characters;
    *line = end_line;
    *byte += bytes;
}

/// Read-only source I/O runs on native worker threads with the actual cancellation token and object identity.
/// 只读来源 I/O 在原生工作线程使用真实取消标记与文件对象身份；不能用缓存或路径检查代替打开句柄校验。
/// Binary native decoders reuse the same opened-file identity checks without JSON/base64 transport.
/// 原生二进制解码器复用相同文件句柄身份检查，不经过 JSON/base64 传输。
pub(crate) fn read_binary_stable(path:&Path,maximum:u64,token:&Option<Arc<AtomicBool>>)->ServiceResult<(Vec<u8>,Value)> {
    cancelled(token)?;let (mut file,first)=stable_file(path,maximum)?;
    let mut bytes=Vec::new();let mut buffer=[0u8;64*1024];
    loop {cancelled(token)?;let count=file.read(&mut buffer).map_err(ServiceError::io)?;if count==0{break;}
        if bytes.len() as u64+count as u64>maximum{return Err(error("STALE_RETRIEVAL_SOURCE"));}bytes.extend_from_slice(&buffer[..count]);}
    if bytes.len() as u64!=first.size{return Err(error("STALE_RETRIEVAL_SOURCE"));}
    Ok((bytes,final_metadata(path,&file,&first)?))
}
pub(crate) fn execute_io(
    operation: &str,
    args: &Value,
    token: &Option<Arc<AtomicBool>>,
) -> ServiceResult<Value> {
    cancelled(token)?;
    let operation = operation
        .strip_prefix("tools.sourceIO.")
        .unwrap_or(operation);
    let path = absolute_path(string(&args["path"]));
    let maximum = number(&args["maximumSourceBytes"], 32 * 1024 * 1024);
    match operation {
        "metadata" => {
            let info =
                inspect(&path, false, false)?.ok_or_else(|| error("INVALID_RETRIEVAL_SOURCE"))?;
            Ok(json!({"isFile": info.is_file(), "metadata": metadata(&path, info)?}))
        }
        "ordinaryRead" => {
            let (mut file, first) = stable_file(&path, maximum)?;
            let mut buffer = [0u8; 64 * 1024];
            let mut bytes = Vec::new();
            loop {
                cancelled(token)?;
                let count = file.read(&mut buffer).map_err(ServiceError::io)?;
                if count == 0 {
                    break;
                }
                if bytes.len() as u64 + count as u64 > maximum {
                    return Err(error("STALE_RETRIEVAL_SOURCE"));
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            if bytes.len() as u64 != first.size {
                return Err(error("STALE_RETRIEVAL_SOURCE"));
            }
            let metadata = final_metadata(&path, &file, &first)?;
            if args["decodeText"] == true {
                let text = if bytes.starts_with(&[0xff, 0xfe]) {
                    decode(&bytes[2..], "utf-16le")?
                } else {
                    decode(&bytes, "utf-8")?
                        .strip_prefix('\u{feff}')
                        .map(str::to_owned)
                        .unwrap_or(decode(&bytes, "utf-8")?)
                };
                if text.contains('\0') {
                    return Err(error("UNSUPPORTED_RETRIEVAL_SOURCE"));
                }
                Ok(json!({"text": text, "metadata": metadata, "rawContentHash": digest(&bytes)}))
            } else {
                Ok(json!({"bytesBase64": STANDARD.encode(bytes), "metadata": metadata}))
            }
        }
        "describeTextWindows" => {
            let (mut file, first) = stable_file(&path, maximum)?;
            let mut raw = Sha256::new();
            let mut text_hash = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut pending_bytes = Vec::new();
            let mut pending = String::new();
            let mut encoding = "utf-8";
            let mut initialized = false;
            let mut byte = 0;
            let mut offset = 0;
            let mut line = 1;
            let mut text_bytes = 0usize;
            let mut windows = Vec::new();
            loop {
                cancelled(token)?;
                let count = file.read(&mut buffer).map_err(ServiceError::io)?;
                if count == 0 {
                    break;
                }
                raw.update(&buffer[..count]);
                let mut bytes = &buffer[..count];
                if !initialized {
                    initialized = true;
                    if bytes.starts_with(&[0xff, 0xfe]) {
                        encoding = "utf-16le";
                        byte = 2;
                        bytes = &bytes[2..];
                    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                        byte = 3;
                        bytes = &bytes[3..];
                    }
                }
                pending_bytes.extend_from_slice(bytes);
                let available = if encoding == "utf-16le" {
                    let mut end = pending_bytes.len() / 2 * 2;
                    if end >= 2
                        && (0xd800..=0xdbff).contains(&u16::from_le_bytes([
                            pending_bytes[end - 2],
                            pending_bytes[end - 1],
                        ]))
                    {
                        end -= 2;
                    }
                    end
                } else {
                    match std::str::from_utf8(&pending_bytes) {
                        Ok(_) => pending_bytes.len(),
                        Err(failure) if failure.error_len().is_none() => failure.valid_up_to(),
                        Err(_) => return Err(error("UNSUPPORTED_RETRIEVAL_SOURCE")),
                    }
                };
                let text = decode(&pending_bytes[..available], encoding)?;
                pending_bytes.drain(..available);
                if text.contains('\0') {
                    return Err(error("UNSUPPORTED_RETRIEVAL_SOURCE"));
                }
                text_hash.update(text.as_bytes());
                text_bytes += text.len();
                pending.push_str(&text);
                while pending.encode_utf16().count() >= WINDOW {
                    let mut units = 0;
                    let mut end = 0;
                    for (index, character) in pending.char_indices() {
                        if units + character.len_utf16() > WINDOW {
                            break;
                        }
                        units += character.len_utf16();
                        end = index + character.len_utf8();
                        if units == WINDOW {
                            break;
                        }
                    }
                    let remaining = pending.split_off(end);
                    append_window(
                        &pending,
                        encoding,
                        &mut offset,
                        &mut line,
                        &mut byte,
                        &mut windows,
                    );
                    pending = remaining;
                }
            }
            if !pending_bytes.is_empty() {
                return Err(error("UNSUPPORTED_RETRIEVAL_SOURCE"));
            }
            if !pending.is_empty() {
                append_window(
                    &pending,
                    encoding,
                    &mut offset,
                    &mut line,
                    &mut byte,
                    &mut windows,
                );
            }
            if byte as u64 != first.size {
                return Err(error("STALE_RETRIEVAL_SOURCE"));
            }
            let metadata = final_metadata(&path, &file, &first)?;
            let raw_hash = format!("{:x}", raw.finalize());
            let content_hash = format!("{:x}", text_hash.finalize());
            let fields = [
                "version",
                "encoding",
                "offsetUnit",
                "startOffset",
                "endOffset",
                "startLine",
                "endLine",
                "startByte",
                "endByte",
                "contentHash",
                "textBytes",
                "rawContentHash",
                "textContentHash",
                "totalCharacters",
                "metadata",
            ];
            let wires: Vec<String> = windows.into_iter().map(|mut window| { window["rawContentHash"] = json!(raw_hash); window["textContentHash"] = json!(content_hash); window["totalCharacters"] = json!(offset); window["metadata"] = metadata.clone(); format!("{{{}}}", fields.iter().map(|field| if *field == "metadata" { format!("\"metadata\":{{\"sizeBytes\":{},\"mtimeMs\":{},\"ctimeMs\":{},\"device\":{},\"inode\":{}}}", metadata["sizeBytes"], metadata["mtimeMs"], metadata["ctimeMs"], metadata["device"], metadata["inode"]) } else { format!("{}:{}", json!(field), window[*field]) }).collect::<Vec<_>>().join(",")) }).collect();
            Ok(
                json!({"rawContentHash": raw_hash, "contentHash": content_hash, "textBytes": text_bytes, "metadata": metadata, "windowsWire": format!("[{}]", wires.join(","))}),
            )
        }
        "readTextWindow" => {
            let window = &args["window"];
            let (mut file, first) = stable_file(&path, maximum)?;
            let before = metadata(&path, file.metadata().map_err(ServiceError::io)?)?;
            if !same(&before, &window["metadata"]) {
                return Err(error("STALE_RETRIEVAL_SOURCE"));
            }
            let start = window["startByte"]
                .as_u64()
                .ok_or_else(|| error("INVALID_RETRIEVAL_WINDOW"))?;
            let end = window["endByte"]
                .as_u64()
                .ok_or_else(|| error("INVALID_RETRIEVAL_WINDOW"))?;
            if window["version"] != WINDOW_VERSION
                || !["utf-8", "utf-16le"].contains(&string(&window["encoding"]))
                || end <= start
                || end - start > WINDOW as u64 * 4
                || end > first.size
            {
                return Err(error("INVALID_RETRIEVAL_WINDOW"));
            }
            file.seek(SeekFrom::Start(start))
                .map_err(ServiceError::io)?;
            let mut bytes = vec![0; (end - start) as usize];
            file.read_exact(&mut bytes)
                .map_err(|_| error("STALE_RETRIEVAL_SOURCE"))?;
            cancelled(token)?;
            let text = decode(&bytes, string(&window["encoding"]))
                .map_err(|_| error("STALE_RETRIEVAL_SOURCE"))?;
            if text.encode_utf16().count() as u64
                != number(&window["endOffset"], 0).saturating_sub(number(&window["startOffset"], 0))
                || digest(text.as_bytes()) != window["contentHash"]
            {
                return Err(error("STALE_RETRIEVAL_SOURCE"));
            }
            Ok(
                json!({"text": text, "contentHash": window["contentHash"], "textBytes": text.len(), "metadata": final_metadata(&path, &file, &first)?}),
            )
        }
        _ => Err(error("AGENT_CORE_UNKNOWN_OPERATION")),
    }
}

#[cfg(test)]
mod tests {
    use super::{same, SourceReadingState};
    use serde_json::{Value, json};

    #[test]
    fn source_metadata_preserves_exact_number_round_trips_and_detects_actual_changes() {
        // A real window timestamp once changed by one ULP during JSON parsing and rejected an unchanged file.
        // 真实窗口时间曾在 JSON 解析时偏移一个浮点精度单位，误拒绝未变化文件；往返必须精确且真实变化仍须拒绝。
        let observed = json!({
            "sizeBytes": 2_835_021,
            "mtimeMs": 1_791_721_997_237.2227,
            "ctimeMs": 1_791_721_997_234.661,
            "device": 881_255_897,
            "inode": 14_355_223_813_248_250u64
        });
        let mut restored: Value = serde_json::from_str(
            r#"{"sizeBytes":2835021,"mtimeMs":1791721997237.2227,"ctimeMs":1791721997234.661,"device":881255897,"inode":14355223813248250}"#,
        )
        .unwrap();
        assert_eq!(observed["mtimeMs"].as_f64(), restored["mtimeMs"].as_f64());
        assert!(same(&observed, &restored));
        restored["mtimeMs"] = json!(1_791_721_997_237.223);
        assert!(!same(&observed, &restored));
    }

    #[test]
    fn image_sources_are_binary_documents_and_share_the_native_parser_version() {
        let mut state = SourceReadingState::default();
        let root = std::env::temp_dir();
        for extension in ["png", "JPG", "jpeg", "bmp", "gif", "tif", "tiff", "webp"] {
            let path = root.join(format!("authorized-picture.{extension}"));
            let policy = state.apply("filePolicy", &json!({"path":path}), &None).unwrap();
            assert!(policy["format"].is_string());
            assert_eq!(policy["version"], "windows-image-ocr-text-v1");
            assert!(state.apply("filePolicy", &json!({"path":path,"window":true}), &None).is_err());
            assert_eq!(super::super::calculate("tools.pathDomain", &json!({"path":path})).unwrap(), "knowledge");
        }
        assert_eq!(state.apply("version", &json!({"path":root.join("a.pdf")}), &None).unwrap(), "rust-lopdf-text-v1");
        assert_eq!(state.apply("version", &json!({"path":root.join("a.docx")}), &None).unwrap(), "rust-docx-text-v1");
        assert!(state.apply("filePolicy", &json!({"path":root.join("unsupported-media.avi")}), &None).is_err());
        let references = super::super::calculate("tools.fileReferences", &json!({"message":"检索 account-guide.png 的密码说明"})).unwrap();
        assert_eq!(references[0]["domain"], "knowledge");
        assert!(super::super::calculate("tools.fileReferences", &json!({"message":"图片清晰度如何改善"})).unwrap().as_array().unwrap().is_empty());
    }
}
