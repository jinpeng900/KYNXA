use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, ReadDir};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::{Value, json};

use super::intent::utf16_length;
use super::local_files::{
    FileVersion, absolute_path, file_version, inspect, modified_iso, path_text, random_identity,
    read_checked, sha256, within,
};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, slice, string};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_PAGE_FILES: usize = 1000;
const MAX_PAGE_ENTRIES: usize = 2000;
const MAX_PAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGE_CHARACTERS: usize = 48000;
const MAX_DIRECTORIES: usize = 256;
const MAX_SESSIONS: usize = 16;
const MAX_VERSIONS: usize = 4096;
const MAX_CHANGES: usize = 512;
const MAX_IGNORE_BYTES: u64 = 128 * 1024;
const MAX_ACTIVE_IGNORE_BYTES: u64 = 1024 * 1024;
const IDLE_MS: u64 = 10 * 60 * 1000;
const EXCLUDED: &[&str] = &[
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
static SEARCHES: OnceLock<Mutex<Vec<Weak<Mutex<SearchData>>>>> = OnceLock::new();

fn invalid_cursor() -> ServiceError {
    ServiceError::new(
        "INVALID_TOOL_SEARCH_CURSOR",
        "搜索游标已使用、过期或不属于当前请求、路径和查询；请省略 cursor 重新搜索。",
        409,
    )
}
fn changed() -> ServiceError {
    ServiceError::new(
        "TOOL_SEARCH_CHANGED",
        "搜索期间目录或文件已改变；已返回内容是旧观察，请省略 cursor 从头搜索当前版本。",
        409,
    )
}
fn cancelled(cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<()> {
    if cancellation
        .as_ref()
        .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
    {
        return Err(ServiceError::new("TOOL_CANCELLED", "文件搜索已取消。", 409));
    }
    Ok(())
}

fn validate_input(input: &Value) -> ServiceResult<usize> {
    if !input["query"]
        .as_str()
        .is_some_and(|query| !query.is_empty() && utf16_length(query) <= 200)
    {
        return Err(ServiceError::new(
            "INVALID_TOOL_ARGUMENTS",
            "搜索文本须为 1–200 个字符。",
            400,
        ));
    }
    if input
        .get("mode")
        .is_some_and(|mode| !["content", "path"].contains(&string(mode)))
    {
        return Err(ServiceError::new(
            "INVALID_TOOL_ARGUMENTS",
            "搜索 mode 必须为 content 或 path。",
            400,
        ));
    }
    if input
        .get("respectIgnoreFiles")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(ServiceError::new(
            "INVALID_TOOL_ARGUMENTS",
            "respectIgnoreFiles 必须为布尔值。",
            400,
        ));
    }
    if input["maxMatches"].is_null() {
        return Ok(100);
    }
    input["maxMatches"]
        .as_u64()
        .filter(|maximum| (1..=200).contains(maximum))
        .map(|maximum| maximum as usize)
        .ok_or_else(|| {
            ServiceError::new("INVALID_TOOL_ARGUMENTS", "整数参数须在 1–200 范围内。", 400)
        })
}

struct IgnoreRule {
    path: PathBuf,
    root: PathBuf,
    version: Option<FileVersion>,
    matcher: Option<Gitignore>,
    bytes: u64,
}
struct DirectoryFrame {
    path: PathBuf,
    version: FileVersion,
    reader: ReadDir,
    ignore_count: usize,
}
struct PendingFile {
    path: PathBuf,
    offset: usize,
    line: usize,
    hash: Option<String>,
}
enum Authorization {
    Candidate(PathBuf),
    File(PendingFile),
    Ignore(PathBuf),
}

struct Page {
    matches: Vec<Value>,
    match_characters: usize,
    failure_characters: usize,
    result_full: bool,
    visited_files: usize,
    visited_entries: usize,
    skipped: usize,
    skipped_by_reason: HashMap<String, usize>,
    failures: Vec<Value>,
    failed_entries: usize,
    read_bytes: usize,
    started: Instant,
    maximum_matches: usize,
}

impl Page {
    fn new(maximum_matches: usize) -> Self {
        Self {
            matches: Vec::new(),
            match_characters: 0,
            failure_characters: 0,
            result_full: false,
            visited_files: 0,
            visited_entries: 0,
            skipped: 0,
            skipped_by_reason: HashMap::new(),
            failures: Vec::new(),
            failed_entries: 0,
            read_bytes: 0,
            started: Instant::now(),
            maximum_matches,
        }
    }
    fn limited(&self) -> bool {
        self.result_full
            || self.matches.len() >= self.maximum_matches
            || self.visited_files >= MAX_PAGE_FILES
            || self.visited_entries >= MAX_PAGE_ENTRIES
            || self.read_bytes >= MAX_PAGE_BYTES
            || self.started.elapsed() >= Duration::from_secs(1)
    }
    fn skip(&mut self, reason: &str) {
        self.skipped += 1;
        *self.skipped_by_reason.entry(reason.into()).or_default() += 1;
    }
    fn add(&mut self, value: Value) -> bool {
        let characters = utf16_length(&serde_json::to_string(&value).unwrap()) + 1;
        if (!self.matches.is_empty() || !self.failures.is_empty())
            && self.failure_characters + self.match_characters + characters > MAX_PAGE_CHARACTERS
        {
            self.result_full = true;
            return false;
        }
        self.matches.push(value);
        self.match_characters += characters;
        true
    }
}

struct SearchData {
    root: PathBuf,
    query: String,
    mode: String,
    recursive: bool,
    respect_ignore: bool,
    root_is_directory: bool,
    ignore_root: PathBuf,
    protected: Vec<PathBuf>,
    binding: Value,
    cursor: String,
    directories: Vec<DirectoryFrame>,
    next_path: Option<PathBuf>,
    next_already_visited: bool,
    pending_file: Option<PendingFile>,
    pending_ignores: VecDeque<PathBuf>,
    ignore_rules: Vec<IgnoreRule>,
    ignore_bytes: u64,
    incomplete_ignores: usize,
    ignores_read: usize,
    versions: HashMap<PathBuf, FileVersion>,
    version_order: VecDeque<PathBuf>,
    changes: HashSet<PathBuf>,
    unverified_changes: bool,
    monitor_state: String,
    closed: bool,
    busy: bool,
    idle_deadline: Option<Instant>,
    page: Option<Page>,
    authorization: Option<Authorization>,
    page_number: usize,
    total_files: usize,
    total_entries: usize,
    total_skipped: usize,
    total_failed: usize,
}

impl Default for SearchData {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            query: String::new(),
            mode: "content".into(),
            recursive: true,
            respect_ignore: true,
            root_is_directory: false,
            ignore_root: PathBuf::new(),
            protected: Vec::new(),
            binding: Value::Null,
            cursor: String::new(),
            directories: Vec::new(),
            next_path: None,
            next_already_visited: false,
            pending_file: None,
            pending_ignores: VecDeque::new(),
            ignore_rules: Vec::new(),
            ignore_bytes: 0,
            incomplete_ignores: 0,
            ignores_read: 0,
            versions: HashMap::new(),
            version_order: VecDeque::new(),
            changes: HashSet::new(),
            unverified_changes: false,
            monitor_state: "active".into(),
            closed: false,
            busy: false,
            idle_deadline: None,
            page: None,
            authorization: None,
            page_number: 0,
            total_files: 0,
            total_entries: 0,
            total_skipped: 0,
            total_failed: 0,
        }
    }
}

fn binding(arguments: &Value) -> Value {
    let root = absolute_path(string(&arguments["path"]));
    let input = &arguments["input"];
    let context = &arguments["context"];
    let mut protected = array(&arguments["protectedRoots"])
        .iter()
        .map(|root| path_text(&absolute_path(string(root))))
        .collect::<Vec<_>>();
    protected.sort();
    json!([
        path_text(&root),
        input["query"],
        input.get("mode").unwrap_or(&json!("content")),
        input["respectIgnoreFiles"] != false,
        input["recursive"] != false,
        context["requestId"],
        context["conversationId"],
        context["projectId"],
        context["workspaceRoot"],
        arguments["workspaceRoot"],
        context["permissionMode"],
        protected,
        arguments["contextIdentity"],
        arguments["ownerIdentity"]
    ])
}

impl SearchData {
    fn close(&mut self) {
        self.closed = true;
        self.busy = false;
        self.directories.clear();
        self.ignore_rules.clear();
        self.versions.clear();
        self.changes.clear();
        self.page = None;
        self.authorization = None;
    }
    fn protected(&self, path: &Path) -> bool {
        self.protected.iter().any(|root| within(root, path))
    }
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(path_text)
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
    }
    fn remember(&mut self, path: PathBuf, version: FileVersion) {
        self.version_order.retain(|current| *current != path);
        self.version_order.push_back(path.clone());
        self.versions.insert(path, version);
        if self.version_order.len() > MAX_VERSIONS {
            if let Some(path) = self.version_order.pop_front() {
                self.versions.remove(&path);
            }
        }
    }
    fn observe(&mut self, arguments: &Value) {
        if let Some(state) = arguments["monitorState"].as_str() {
            self.monitor_state = state.into();
        }
        self.unverified_changes |= arguments["overflow"] == true;
        for path in array(&arguments["changes"]) {
            let path = absolute_path(string(path));
            if self.protected(&path)
                || path.strip_prefix(&self.root).ok().is_some_and(|relative| {
                    relative.components().any(|component| {
                        EXCLUDED.contains(
                            &component
                                .as_os_str()
                                .to_string_lossy()
                                .to_lowercase()
                                .as_str(),
                        )
                    })
                })
            {
                continue;
            }
            if self.changes.len() < MAX_CHANGES {
                self.changes.insert(path);
            } else {
                self.unverified_changes = true;
            }
        }
    }
    fn check(&mut self) -> ServiceResult<()> {
        if self.closed {
            return Err(invalid_cursor());
        }
        for frame in &self.directories {
            let metadata = inspect(&frame.path, false, false)?.ok_or_else(changed)?;
            if !metadata.is_dir() || file_version(&frame.path, &metadata)? != frame.version {
                return Err(changed());
            }
        }
        for rule in &self.ignore_rules {
            let metadata = inspect(&rule.path, true, true)?;
            let version = metadata
                .as_ref()
                .map(|metadata| file_version(&rule.path, metadata))
                .transpose()?;
            if version != rule.version {
                return Err(changed());
            }
        }
        for candidate in self.changes.drain().collect::<Vec<_>>() {
            let observed = if self.versions.contains_key(&candidate) {
                candidate
            } else {
                candidate.parent().unwrap_or(&self.root).to_owned()
            };
            let Some(version) = self.versions.get(&observed) else {
                self.unverified_changes = true;
                continue;
            };
            let metadata = inspect(&observed, false, true)?.ok_or_else(changed)?;
            if file_version(&observed, &metadata)? != *version {
                return Err(changed());
            }
        }
        Ok(())
    }
    fn failed(&mut self, path: &Path, code: &str) {
        let relative = self.relative(path);
        let page = self.page.as_mut().unwrap();
        page.skip(code);
        page.failed_entries += 1;
        let failure = json!({"path":relative,"code":code});
        let characters = utf16_length(&serde_json::to_string(&failure).unwrap()) + 1;
        if page.failures.len() < 50
            && page.failure_characters + characters <= 8000
            && page.failure_characters + page.match_characters + characters <= MAX_PAGE_CHARACTERS
        {
            page.failures.push(failure);
            page.failure_characters += characters;
        }
    }
    fn ignored(&self, path: &Path, directory: bool) -> bool {
        if !self.respect_ignore || path == self.root {
            return false;
        }
        let mut ignored = false;
        for rule in &self.ignore_rules {
            if !within(&rule.root, path) {
                continue;
            }
            if let Some(matcher) = &rule.matcher {
                let decision = matcher.matched_path_or_any_parents(path, directory);
                if decision.is_ignore() {
                    ignored = true;
                } else if decision.is_whitelist() {
                    ignored = false;
                }
            }
        }
        ignored
    }
    fn read_ignore(&mut self, path: PathBuf, allowed: bool) -> ServiceResult<()> {
        if !allowed || self.protected(&path) {
            self.incomplete_ignores += 1;
            return Ok(());
        }
        let root = path.parent().unwrap_or(&self.root).to_owned();
        let metadata = match inspect(&path, true, true) {
            Ok(metadata) => metadata,
            Err(error) if error.code != "TOOL_SEARCH_CHANGED" => {
                self.incomplete_ignores += 1;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let Some(metadata) = metadata else {
            self.ignore_rules.push(IgnoreRule {
                path,
                root,
                version: None,
                matcher: None,
                bytes: 0,
            });
            return Ok(());
        };
        if !metadata.is_file()
            || metadata.len() > MAX_IGNORE_BYTES
            || self.ignore_bytes + metadata.len() > MAX_ACTIVE_IGNORE_BYTES
        {
            self.incomplete_ignores += 1;
            return Ok(());
        }
        let bytes = match read_checked(&path, MAX_IGNORE_BYTES, true, "TOOL_SEARCH_CHANGED") {
            Ok(bytes) => bytes,
            Err(error) if error.code == "TOOL_SEARCH_CHANGED" => return Err(error),
            Err(_) => {
                self.incomplete_ignores += 1;
                return Ok(());
            }
        };
        let content = match std::str::from_utf8(&bytes) {
            Ok(content) => content,
            Err(_) => {
                self.incomplete_ignores += 1;
                return Ok(());
            }
        };
        let mut builder = GitignoreBuilder::new(&root);
        for line in content.lines() {
            if builder.add_line(Some(path.clone()), line).is_err() {
                self.incomplete_ignores += 1;
                return Ok(());
            }
        }
        let matcher = match builder.build() {
            Ok(matcher) => matcher,
            Err(_) => {
                self.incomplete_ignores += 1;
                return Ok(());
            }
        };
        let version = file_version(&path, &metadata)?;
        self.remember(path.clone(), version.clone());
        self.ignore_bytes += bytes.len() as u64;
        self.ignores_read += 1;
        self.ignore_rules.push(IgnoreRule {
            path,
            root,
            version: Some(version),
            matcher: Some(matcher),
            bytes: bytes.len() as u64,
        });
        Ok(())
    }
    fn read_file(
        &mut self,
        pending: PendingFile,
        allowed: bool,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<()> {
        let path = pending.path.clone();
        if !allowed || self.protected(&path) {
            self.page.as_mut().unwrap().skip("protected");
            return Ok(());
        }
        let metadata = inspect(&path, false, true)?.ok_or_else(changed)?;
        if !metadata.is_file() {
            return Err(changed());
        }
        if metadata.len() > MAX_FILE_BYTES {
            if pending.hash.is_some() {
                return Err(changed());
            }
            self.page.as_mut().unwrap().skip("fileTooLarge");
            return Ok(());
        }
        let bytes = read_checked(&path, MAX_FILE_BYTES, true, "TOOL_SEARCH_CHANGED")?;
        let hash = sha256(&bytes);
        if pending
            .hash
            .as_ref()
            .is_some_and(|previous| *previous != hash)
        {
            return Err(changed());
        }
        self.remember(path.clone(), file_version(&path, &metadata)?);
        self.page.as_mut().unwrap().read_bytes += bytes.len();
        let content = match std::str::from_utf8(&bytes)
            .ok()
            .filter(|content| !content.contains('\0'))
        {
            Some(content) => content,
            None => {
                self.page.as_mut().unwrap().skip("nonText");
                return Ok(());
            }
        };
        let mut offset = pending.offset;
        let mut line = pending.line;
        while offset < content.len() {
            cancelled(cancellation)?;
            let newline = content[offset..].find('\n').map(|index| offset + index);
            let end = newline.unwrap_or(content.len());
            let text = content[offset..end]
                .strip_suffix('\r')
                .unwrap_or(&content[offset..end]);
            if text.contains(&self.query) {
                let matched = json!({"path":self.relative(&path),"line":line,"text":slice(text,0,400),"sha256":hash});
                if !self.page.as_mut().unwrap().add(matched) {
                    break;
                }
            }
            offset = newline.map(|newline| newline + 1).unwrap_or(content.len());
            line += 1;
            let page = self.page.as_ref().unwrap();
            if page.matches.len() >= page.maximum_matches
                || page.started.elapsed() >= Duration::from_secs(1)
            {
                break;
            }
        }
        self.pending_file = (offset < content.len()).then_some(PendingFile {
            path,
            offset,
            line,
            hash: Some(hash),
        });
        Ok(())
    }
    fn candidate(
        &mut self,
        path: PathBuf,
        allowed: bool,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<()> {
        if !allowed || self.protected(&path) {
            self.page.as_mut().unwrap().skip("protected");
            return Ok(());
        }
        let metadata = match inspect(&path, false, true) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => return Err(changed()),
            Err(error)
                if path != self.root
                    && ["ENOENT", "EACCES", "EPERM"].contains(&error.code.as_str()) =>
            {
                self.failed(&path, &error.code);
                return Ok(());
            }
            Err(error) if error.code == "UNSAFE_TOOL_PATH" => {
                self.page.as_mut().unwrap().skip(&error.code);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if self.ignored(&path, metadata.is_dir()) {
            self.page.as_mut().unwrap().skip("ignoreRule");
            return Ok(());
        }
        self.remember(path.clone(), file_version(&path, &metadata)?);
        if self.mode == "path" && (metadata.is_file() || metadata.is_dir()) {
            let relative = if path == self.root && metadata.is_dir() {
                ".".into()
            } else {
                self.relative(&path)
            };
            let candidate = self.relative(&path).replace('\\', "/");
            let query = self.query.replace('\\', "/");
            let matching = if cfg!(windows) {
                candidate.to_lowercase().contains(&query.to_lowercase())
            } else {
                candidate.contains(&query)
            };
            if matching && !self.page.as_mut().unwrap().add(json!({"path":relative,"type":if metadata.is_dir(){"directory"}else{"file"},"bytes":metadata.len(),"modifiedAt":modified_iso(&metadata)})) {
                self.next_path = Some(path); self.next_already_visited = true; return Ok(());
            }
        }
        if metadata.is_dir() {
            if self.directories.len() >= MAX_DIRECTORIES {
                return Err(ServiceError::new(
                    "TOOL_SEARCH_DEPTH_LIMIT",
                    "目录嵌套超过本次扫描句柄额度，请从更深的子目录继续独立搜索。",
                    409,
                ));
            }
            let frame = DirectoryFrame {
                version: file_version(&path, &metadata)?,
                reader: fs::read_dir(&path).map_err(ServiceError::io)?,
                path: path.clone(),
                ignore_count: self.ignore_rules.len(),
            };
            self.directories.push(frame);
            if self.respect_ignore {
                for name in [".gitignore", ".ignore"] {
                    self.pending_ignores.push_back(path.join(name));
                }
            }
        } else if metadata.is_file() {
            self.page.as_mut().unwrap().visited_files += 1;
            if self.mode != "path" {
                self.read_file(
                    PendingFile {
                        path,
                        offset: 0,
                        line: 1,
                        hash: None,
                    },
                    true,
                    cancellation,
                )?;
            }
        } else {
            self.page.as_mut().unwrap().skip("notRegularFile");
        }
        Ok(())
    }
    fn next_candidate(&mut self) -> ServiceResult<Option<PathBuf>> {
        if let Some(path) = self.next_path.take() {
            if !self.next_already_visited {
                self.page.as_mut().unwrap().visited_entries += 1;
            }
            self.next_already_visited = false;
            return Ok(Some(path));
        }
        while !self.directories.is_empty()
            && self.page.as_ref().unwrap().visited_entries < MAX_PAGE_ENTRIES
        {
            let frame = self.directories.last_mut().unwrap();
            let entry = frame.reader.next();
            let Some(entry) = entry else {
                let frame = self.directories.pop().unwrap();
                let metadata = inspect(&frame.path, false, false)?.ok_or_else(changed)?;
                if file_version(&frame.path, &metadata)? != frame.version {
                    return Err(changed());
                }
                while self.ignore_rules.len() > frame.ignore_count {
                    self.ignore_bytes -= self.ignore_rules.pop().unwrap().bytes;
                }
                continue;
            };
            let entry = entry.map_err(ServiceError::io)?;
            let kind = entry.file_type().map_err(ServiceError::io)?;
            self.page.as_mut().unwrap().visited_entries += 1;
            if kind.is_symlink() {
                self.page.as_mut().unwrap().skip("link");
                continue;
            }
            if kind.is_dir()
                && EXCLUDED.contains(&entry.file_name().to_string_lossy().to_lowercase().as_str())
            {
                self.page.as_mut().unwrap().skip("excludedDirectory");
                continue;
            }
            if kind.is_dir() && !self.recursive {
                self.page.as_mut().unwrap().skip("nonRecursiveDirectory");
                continue;
            }
            return Ok(Some(entry.path()));
        }
        Ok(None)
    }
    fn step(
        &mut self,
        allowed: bool,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<Value> {
        if self.closed {
            return Err(invalid_cursor());
        }
        if let Some(authorization) = self.authorization.take() {
            match authorization {
                Authorization::Candidate(path) => {
                    if let Err(error) = self.candidate(path.clone(), allowed, cancellation) {
                        if path != self.root
                            && ["ENOENT", "EACCES", "EPERM"].contains(&error.code.as_str())
                            || path == self.root
                                && ["EACCES", "EPERM"].contains(&error.code.as_str())
                        {
                            self.failed(&path, &error.code);
                        } else {
                            return Err(error);
                        }
                    }
                }
                Authorization::Ignore(path) => self.read_ignore(path, allowed)?,
                Authorization::File(pending) => {
                    let path = pending.path.clone();
                    let new_file = pending.hash.is_none();
                    if let Err(error) = self.read_file(pending, allowed, cancellation) {
                        if new_file && ["ENOENT", "EACCES", "EPERM"].contains(&error.code.as_str())
                        {
                            self.failed(&path, &error.code);
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
        }
        loop {
            cancelled(cancellation)?;
            if self.page.as_ref().unwrap().limited() {
                return Ok(json!({"pageReady":true}));
            }
            let authorization = if let Some(path) = self.pending_ignores.pop_front() {
                Some(Authorization::Ignore(path))
            } else if let Some(file) = self.pending_file.take() {
                Some(Authorization::File(file))
            } else {
                self.next_candidate()?.map(Authorization::Candidate)
            };
            let Some(authorization) = authorization else {
                return Ok(json!({"pageReady":true}));
            };
            let path = match &authorization {
                Authorization::Candidate(path) | Authorization::Ignore(path) => path,
                Authorization::File(file) => &file.path,
            };
            let result = json!({"authorization":{"path":path_text(path)}});
            self.authorization = Some(authorization);
            return Ok(result);
        }
    }
    fn finish(&mut self) -> ServiceResult<Value> {
        self.check()?;
        let page = self.page.take().ok_or_else(invalid_cursor)?;
        let has_more = self.pending_file.is_some()
            || self.next_path.is_some()
            || !self.directories.is_empty()
            || !self.pending_ignores.is_empty();
        self.total_files += page.visited_files;
        self.total_entries += page.visited_entries;
        self.total_skipped += page.skipped;
        self.total_failed += page.failed_entries;
        self.page_number += 1;
        self.cursor = random_identity()?.replace('-', "");
        self.busy = false;
        self.idle_deadline = has_more.then(|| Instant::now() + Duration::from_millis(IDLE_MS));
        let limit_reasons = [
            page.result_full.then_some("resultCharacters"),
            (page.matches.len() >= page.maximum_matches).then_some("matches"),
            (page.visited_files >= MAX_PAGE_FILES).then_some("files"),
            (page.visited_entries >= MAX_PAGE_ENTRIES).then_some("entries"),
            (page.read_bytes >= MAX_PAGE_BYTES).then_some("readBytes"),
            (page.started.elapsed() >= Duration::from_secs(1)).then_some("duration"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let monitored = self.monitor_state == "active";
        let mut result = json!({"path":path_text(&self.root),"query":self.query,"mode":self.mode,"matches":page.matches,
            "visitedFiles":page.visited_files,"skipped":page.skipped,"skippedByReason":page.skipped_by_reason,
            "truncated":has_more,"hasMore":has_more,"nextCursor":if has_more{json!(self.cursor)}else{Value::Null},
            "failures":page.failures,"failureReportTruncated":page.failed_entries>page.failures.len(),"pageLimitReasons":limit_reasons,
            "page":self.page_number,"totalVisitedFiles":self.total_files,"totalVisitedEntries":self.total_entries,"totalSkipped":self.total_skipped,
            "consistency":{"mode":if monitored && !self.unverified_changes{"live-walk"}else{"live-unverified"},
                "directoryMonitoring":self.monitor_state,"atomicSnapshot":false,"changesDetected":false,"unverifiedChangeNotifications":self.unverified_changes,
                "restartOnChange":true,"note":"Directory events, active directory versions and resumed file hashes are checked; rerun after edits. Results carry file hashes. No atomic whole-repository snapshot."},
            "coverage":{"traversalComplete":!has_more,"fileSizeLimitBytes":MAX_FILE_BYTES,"fileSizeLimitAppliesTo":"content","respectIgnoreFiles":self.respect_ignore,
                "ignoreFileNames":[".gitignore",".ignore"],"ignoreFilesRead":self.ignores_read,"failedEntries":self.total_failed,
                "incompleteIgnoreFiles":self.incomplete_ignores,"excludedDirectories":EXCLUDED,"unreadableOrUnsupportedEntriesSkipped":self.total_skipped>0}});
        if !monitored {
            result["diagnostics"] = json!([{"code":"TOOL_SEARCH_MONITOR_UNAVAILABLE","message":"Active directory versions and resumed file hashes are checked; changes in completed branches may require a fresh search."}]);
        }
        if has_more {
            result["cursorExpiresInMs"] = json!(IDLE_MS);
        } else {
            self.close();
        }
        Ok(result)
    }
}

#[derive(Default)]
pub struct SearchService {
    data: Arc<Mutex<SearchData>>,
}

impl SearchService {
    fn register(&self) -> ServiceResult<()> {
        let mut registry = SEARCHES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap();
        registry.retain(|entry| {
            let Some(entry) = entry.upgrade() else {
                return false;
            };
            if let Ok(mut entry) = entry.try_lock() {
                if entry.closed {
                    return false;
                }
                if entry
                    .idle_deadline
                    .is_some_and(|deadline| deadline <= Instant::now())
                {
                    entry.close();
                    return false;
                }
            }
            true
        });
        while registry.len() >= MAX_SESSIONS {
            let index = registry
                .iter()
                .position(|entry| {
                    entry.upgrade().is_some_and(|entry| {
                        if let Ok(mut entry) = entry.try_lock() {
                            if !entry.busy {
                                entry.close();
                                return true;
                            }
                        }
                        false
                    })
                })
                .ok_or_else(|| {
                    ServiceError::new(
                        "TOOL_SEARCH_BUSY",
                        "并行文件搜索已达到上限，请等待当前搜索页完成。",
                        409,
                    )
                })?;
            registry.remove(index);
        }
        registry.push(Arc::downgrade(&self.data));
        Ok(())
    }
    pub fn apply(
        &mut self,
        operation: &str,
        arguments: &Value,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<Value> {
        cancelled(cancellation)?;
        if operation == "search.init" {
            self.register()?;
        }
        let mut data = self.data.lock().unwrap();
        let result = (|| match operation {
            "search.validate" => Ok(json!(validate_input(&arguments["input"])?)),
            "search.invalidCursor" => Err(invalid_cursor()),
            "search.init" => {
                let input = &arguments["input"];
                validate_input(input)?;
                let query = input["query"]
                    .as_str()
                    .filter(|query| !query.is_empty() && utf16_length(query) <= 200)
                    .ok_or_else(|| {
                        ServiceError::new(
                            "INVALID_TOOL_ARGUMENTS",
                            "搜索文本须为 1–200 个字符。",
                            400,
                        )
                    })?;
                if input
                    .get("mode")
                    .is_some_and(|mode| !["content", "path"].contains(&string(mode)))
                {
                    return Err(ServiceError::new(
                        "INVALID_TOOL_ARGUMENTS",
                        "搜索 mode 必须为 content 或 path。",
                        400,
                    ));
                }
                if input
                    .get("respectIgnoreFiles")
                    .is_some_and(|value| !value.is_boolean())
                {
                    return Err(ServiceError::new(
                        "INVALID_TOOL_ARGUMENTS",
                        "respectIgnoreFiles 必须为布尔值。",
                        400,
                    ));
                }
                let root = absolute_path(string(&arguments["path"]));
                let metadata = inspect(&root, false, true)?.ok_or_else(changed)?;
                *data = SearchData::default();
                data.root = root.clone();
                data.query = query.into();
                data.binding = binding(arguments);
                data.mode = input["mode"].as_str().unwrap_or("content").into();
                data.recursive = input["recursive"] != false;
                data.respect_ignore = input["respectIgnoreFiles"] != false;
                data.root_is_directory = metadata.is_dir();
                data.protected = array(&arguments["protectedRoots"])
                    .iter()
                    .map(|path| absolute_path(string(path)))
                    .collect();
                data.ignore_root = arguments["workspaceRoot"]
                    .as_str()
                    .map(absolute_path)
                    .filter(|workspace| within(workspace, &root))
                    .unwrap_or_else(|| {
                        if metadata.is_dir() {
                            root.clone()
                        } else {
                            root.parent().unwrap_or(&root).to_owned()
                        }
                    });
                data.cursor = random_identity()?.replace('-', "");
                data.next_path = Some(root.clone());
                data.busy = true;
                data.remember(root.clone(), file_version(&root, &metadata)?);
                if data.respect_ignore && metadata.is_dir() && data.ignore_root != root {
                    let mut ancestors = Vec::new();
                    let mut current = root.parent();
                    while let Some(directory) = current {
                        if !within(&data.ignore_root, directory) {
                            break;
                        }
                        ancestors.push(directory.to_owned());
                        if directory == data.ignore_root {
                            break;
                        }
                        current = directory.parent();
                    }
                    for directory in ancestors.into_iter().rev() {
                        for name in [".gitignore", ".ignore"] {
                            data.pending_ignores.push_back(directory.join(name));
                        }
                    }
                }
                Ok(json!({"rootIsDirectory":data.root_is_directory,"eventCapacity":MAX_CHANGES}))
            }
            "search.acquire" => {
                if data.closed
                    || data.binding != binding(arguments)
                    || arguments["input"]["cursor"] != data.cursor
                    || data
                        .idle_deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                {
                    return Err(invalid_cursor());
                }
                if data.busy {
                    return Err(ServiceError::new(
                        "TOOL_SEARCH_BUSY",
                        "该搜索页仍在执行，请等待它完成后使用新的游标。",
                        409,
                    ));
                }
                data.busy = true;
                data.idle_deadline = None;
                Ok(Value::Null)
            }
            "search.pageStart" => {
                if data.closed || !data.busy {
                    return Err(invalid_cursor());
                }
                data.observe(arguments);
                data.check()?;
                let maximum = arguments["maxMatches"].as_u64().unwrap_or(100);
                if !(1..=200).contains(&maximum)
                    || !arguments["maxMatches"].is_null()
                        && arguments["maxMatches"].as_u64().is_none()
                {
                    return Err(ServiceError::new(
                        "INVALID_TOOL_ARGUMENTS",
                        "整数参数须在 1–200 范围内。",
                        400,
                    ));
                }
                data.page = Some(Page::new(maximum as usize));
                data.step(true, cancellation)
            }
            "search.step" => {
                data.observe(arguments);
                data.step(arguments["allowed"] == true, cancellation)
            }
            "search.pageFinish" => {
                data.observe(arguments);
                data.finish()
            }
            "search.close" => {
                data.close();
                Ok(Value::Null)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown filesystem search operation.",
                400,
            )),
        })();
        if let Err(error) = &result {
            if !["INVALID_TOOL_SEARCH_CURSOR", "TOOL_SEARCH_BUSY"].contains(&error.code.as_str()) {
                data.close();
            }
        }
        result.map_err(|error| {
            if ["ENOENT", "ENOTDIR"].contains(&error.code.as_str()) {
                changed()
            } else {
                error
            }
        })
    }
}
