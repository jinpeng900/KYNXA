use super::{ContractError, ContractResult, cancellation, managed_files, raw_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

const CONVERSATION_NAMES: &[&str] = &[
    "Projects",
    "Chats",
    "Trash",
    "Backups",
    "Memory",
    "Knowledge",
    "Retrieval",
    "Index",
    "settings.json",
    "catalog.json",
    ".conversations-v1.json",
    ".catalog-transaction.json",
];
static TEMPORARIES: AtomicU64 = AtomicU64::new(0);
static WORKSPACE_INITIALIZATION: Mutex<()> = Mutex::new(());

fn failure(message: impl Into<String>) -> ContractError {
    ContractError::new(message, "STORAGE_MIGRATION_FAILED", 409)
}
fn io_error(error: std::io::Error) -> ContractError {
    failure(format!(
        "Storage administration failed. / 存储管理失败：{error}"
    ))
}
fn cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        Err(ContractError::new(
            "Storage administration cancelled. / 存储管理已取消。",
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
        .ok_or_else(|| failure("Migration paths must be absolute."))?;
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || value.contains(['\0', '\r', '\n'])
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(failure("Migration paths must be absolute."));
    }
    Ok(path)
}

/// Resolve portable settings against the application location, never the process working directory.
/// 便携配置相对软件所在目录解析，不随进程工作目录或用户账户变化。
pub(crate) fn configured_path(value: &Value, base: &Path) -> Result<PathBuf, ContractError> {
    let value = value
        .as_str()
        .filter(|value| !value.trim().is_empty() && !value.contains(['\0', '\r', '\n']))
        .ok_or_else(|| failure("KYNXA 存储目录配置无效。"))?;
    let path = Path::new(value);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return Err(failure("KYNXA 存储目录配置无效。"));
                }
            }
            component => resolved.push(component.as_os_str()),
        }
    }
    absolute(&json!(resolved))
}
fn root(path: &Path) -> PathBuf {
    path.components()
        .take_while(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
        .collect()
}
fn within(parent: &Path, path: &Path) -> bool {
    let parent = parent.to_string_lossy().to_lowercase();
    let path = path.to_string_lossy().to_lowercase();
    path == parent
        || path.starts_with(
            &(parent
                .trim_end_matches(std::path::MAIN_SEPARATOR)
                .to_owned()
                + std::path::MAIN_SEPARATOR_STR),
        )
}
fn inspect(path: &Path, file: bool, create: bool) -> Result<bool, ContractError> {
    managed_files::inspect(&root(path), path, create, file, u64::MAX)
}
fn optional(path: &Path) -> Result<Option<Vec<u8>>, ContractError> {
    managed_files::read(&root(path), path, u64::MAX)
}
fn ensure(path: &Path) -> Result<(), ContractError> {
    inspect(path, false, true).map(|_| ())
}
fn open_read(path: &Path) -> Result<File, ContractError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let handle = options.open(path).map_err(io_error)?;
    let identity = crate::file_identity::handle_identity(&handle).map_err(io_error)?;
    if identity.is_link || identity.links != 1 || !handle.metadata().map_err(io_error)?.is_file() {
        return Err(ContractError::new(
            "Managed path contains a link or invalid structure. / 托管路径包含链接或无效结构。",
            "INVALID_MEMORY_PATH",
            409,
        ));
    }
    Ok(handle)
}
fn hash(path: &Path) -> Result<String, ContractError> {
    cancelled()?;
    inspect(path, true, false)?;
    let mut file = open_read(path)?;
    let before = crate::file_identity::handle_identity(&file).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    let modified = metadata.modified().ok();
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        cancelled()?;
        let size = file.read(&mut buffer).map_err(io_error)?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    let after = file.metadata().map_err(io_error)?;
    let current = open_read(path)?;
    if before != crate::file_identity::handle_identity(&current).map_err(io_error)?
        || metadata.len() != after.len()
        || modified != after.modified().ok()
    {
        return Err(failure("Source files changed during migration."));
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Copies use private temporary files and verified streaming hashes; old data is never deleted.
/// 复制使用私有临时文件和流式哈希核验；永不删除原始数据。
fn copy_file(source: &Path, target: &Path, allow_existing: bool) -> Result<String, ContractError> {
    let digest = hash(source)?;
    if inspect(target, true, false)? {
        if allow_existing && hash(target)? == digest {
            return Ok(digest);
        }
        return Err(failure(
            "Legacy record and migration backup differ. / 旧版记录与迁移备份不一致。",
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| failure("Invalid backup destination."))?;
    ensure(parent)?;
    let temporary = parent.join(format!(
        ".migration-{}-{}.tmp",
        std::process::id(),
        TEMPORARIES.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut source_handle = open_read(source)?;
        let mut target_handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(io_error)?;
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            cancelled()?;
            let size = source_handle.read(&mut buffer).map_err(io_error)?;
            if size == 0 {
                break;
            }
            target_handle.write_all(&buffer[..size]).map_err(io_error)?;
        }
        target_handle.sync_all().map_err(io_error)?;
        drop(target_handle);
        if hash(&temporary)? != digest || hash(source)? != digest {
            return Err(failure(
                "Copy verification failed; migration was not committed.",
            ));
        }
        if inspect(target, true, false)? {
            return Err(failure("Backup destination changed during migration."));
        }
        fs::rename(&temporary, target).map_err(io_error)?;
        Ok(digest)
    })();
    if let Err(error) = fs::remove_file(&temporary) {
        if error.kind() != std::io::ErrorKind::NotFound && result.is_ok() {
            return Err(io_error(error));
        }
    }
    result
}
fn backup(source: &Path, target: &Path) -> Result<(), ContractError> {
    cancelled()?;
    let metadata = match fs::symlink_metadata(source) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    inspect(source, metadata.is_file(), false)?;
    if metadata.is_dir() {
        ensure(target)?;
        for entry in fs::read_dir(source).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            backup(&entry.path(), &target.join(entry.file_name()))?;
        }
        Ok(())
    } else if metadata.is_file() {
        copy_file(source, target, true).map(|_| ())
    } else {
        Err(failure("Unsupported legacy storage file type."))
    }
}
fn snapshot(entry: &Value) -> Result<Value, ContractError> {
    let source = absolute(&entry["source"])?;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    fn visit(
        source: &Path,
        prefix: &Path,
        entry: &Value,
        files: &mut Vec<String>,
        directories: &mut Vec<String>,
    ) -> Result<(), ContractError> {
        cancelled()?;
        let path = source.join(prefix);
        if !inspect(&path, false, false)? {
            return Ok(());
        }
        for child in fs::read_dir(&path).map_err(io_error)? {
            let child = child.map_err(io_error)?;
            let name = child.file_name().to_string_lossy().into_owned();
            if prefix.as_os_str().is_empty() {
                let lower = name.to_lowercase();
                if entry["include"]
                    .as_array()
                    .is_some_and(|names| !names.iter().any(|value| value.as_str() == Some(&lower)))
                {
                    continue;
                }
                if entry["exclude"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|value| value.as_str() == Some(&lower)))
                {
                    continue;
                }
            }
            let relative = prefix.join(&name);
            let child_path = source.join(&relative);
            let metadata = fs::symlink_metadata(&child_path).map_err(io_error)?;
            inspect(&child_path, metadata.is_file(), false)?;
            if metadata.is_dir() {
                directories.push(relative.to_string_lossy().into_owned());
                visit(source, &relative, entry, files, directories)?;
            } else if metadata.is_file() {
                files.push(relative.to_string_lossy().into_owned());
            } else {
                return Err(failure("Unsupported file type during migration."));
            }
        }
        Ok(())
    }
    visit(&source, Path::new(""), entry, &mut files, &mut directories)?;
    files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    directories.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(json!({"files":files,"directories":directories}))
}
fn roots(arguments: &Value) -> Result<(PathBuf, Vec<Value>), ContractError> {
    let desktop = absolute(&arguments["desktopSource"])?;
    let models = absolute(&arguments["modelSource"])?;
    let target = absolute(&arguments["target"])?;
    absolute(&arguments["pointer"])?;
    if target == root(&target) || target.to_string_lossy().starts_with("\\\\") {
        return Err(failure("Choose an empty directory on a local disk."));
    }
    let standard = models
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("models"));
    let conversations = if standard {
        models.parent().unwrap().to_owned()
    } else {
        models.join("Conversations")
    };
    for source in [desktop.clone(), models.clone()].into_iter().chain(
        CONVERSATION_NAMES
            .iter()
            .map(|name| conversations.join(name)),
    ) {
        if within(&source, &target) || within(&target, &source) {
            return Err(failure("Destination must not overlap source directories."));
        }
    }
    if inspect(&target, false, false)? && fs::read_dir(&target).map_err(io_error)?.next().is_some()
    {
        return Err(failure("Destination must be empty."));
    }
    Ok((
        conversations.clone(),
        vec![
            json!({"source":desktop,"name":"Desktop"}),
            json!({"source":models,"name":"Models","exclude":if standard{Value::Null}else{json!(["conversations"])}}),
            json!({"source":conversations,"name":"","include":CONVERSATION_NAMES.iter().map(|name|name.to_lowercase()).collect::<Vec<_>>()}),
        ],
    ))
}
fn verify(plan: &Value) -> Result<(), ContractError> {
    for (index, entry) in plan["roots"]
        .as_array()
        .ok_or_else(|| failure("Invalid migration plan."))?
        .iter()
        .enumerate()
    {
        if snapshot(entry)? != plan["snapshots"][index] {
            return Err(failure("Source directories changed during migration."));
        }
    }
    for file in plan["verified"]
        .as_array()
        .ok_or_else(|| failure("Invalid migration receipt."))?
    {
        if Some(hash(&absolute(&file["source"])?)?.as_str()) != file["digest"].as_str() {
            return Err(failure("Source files changed during migration."));
        }
    }
    Ok(())
}
fn projection(
    value: &Value,
    keys: &mut Vec<Value>,
    assignments: &mut Vec<Value>,
    desktop: &Path,
    target: &Path,
    local: bool,
    roots: &[Value],
) {
    if let Some(object) = value.as_object() {
        for (key, child) in object {
            keys.push(json!(key));
            if let Some(path) = child
                .as_str()
                .map(Path::new)
                .filter(|path| path.is_absolute())
            {
                let replacement = if !local
                    && key.eq_ignore_ascii_case("folderpath")
                    && within(&desktop.join("Projects"), path)
                {
                    path.strip_prefix(desktop)
                        .ok()
                        .map(|suffix| target.join("Desktop").join(suffix))
                } else if local
                    && ["modelPath", "serverPath", "backendPath"].contains(&key.as_str())
                {
                    roots
                        .iter()
                        .filter(|entry| entry["name"].as_str().is_some_and(|name| !name.is_empty()))
                        .find_map(|entry| {
                            let source = Path::new(entry["source"].as_str().unwrap_or(""));
                            if within(source, path) {
                                path.strip_prefix(source).ok().map(|suffix| {
                                    target.join(entry["name"].as_str().unwrap()).join(suffix)
                                })
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };
                if let Some(replacement) = replacement {
                    assignments.push(json!({"keys":keys,"value":replacement,"folder":!local}));
                }
            } else if child.is_object() || child.is_array() {
                projection(child, keys, assignments, desktop, target, local, roots);
            }
            keys.pop();
        }
    } else if let Some(array) = value.as_array() {
        for (index, child) in array.iter().enumerate() {
            keys.push(json!(index));
            projection(child, keys, assignments, desktop, target, local, roots);
            keys.pop();
        }
    }
}

fn calculate(operation: &str, arguments: &Value) -> ContractResult {
    cancelled()?;
    match operation {
        "storage.administration.conversationRoot" => {
            let data = absolute(&arguments["dataHome"])?;
            let standard = data
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("models"));
            Ok(
                json!({"standard":standard,"path":if standard{data.parent().unwrap().to_owned()}else{data.join("Conversations")}}),
            )
        }
        "storage.administration.workspaceIdentity" => {
            let id = super::identity(&arguments["conversationId"])?;
            let id = id.as_str().unwrap().to_lowercase();
            let uuid = regex::Regex::new(
                r"(?i)^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$",
            )
            .unwrap()
            .is_match(&id);
            let folder = if uuid {
                id.clone()
            } else {
                let digest = managed_files::digest(
                    format!("kynxa-conversation-workspace/v1:{id}").as_bytes(),
                );
                super::identity(&json!(&digest[..32]))?
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            Ok(json!({"id":id,"folderId":folder}))
        }
        "storage.administration.workspaceRoot" => {
            let path = absolute(&arguments["root"])?;
            if path.to_string_lossy().starts_with("\\\\") {
                return Err(ContractError::new(
                    "聊天工具工作区必须是绝对本地目录。",
                    "INVALID_SANDBOX_WORKSPACE",
                    400,
                ));
            }
            Ok(json!(path))
        }
        "storage.administration.workspaceControl" => {
            let Ok(path) = absolute(&arguments["path"]) else {
                return Ok(json!(false));
            };
            Ok(json!(
                path.file_name().is_some_and(|name| name
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".workspace-owner.json"))
                    && arguments["aliases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|value| value.as_str())
                        .any(|alias| within(Path::new(alias), &path))
            ))
        }
        "storage.administration.workspaceEnsure" | "storage.administration.workspaceVerify" => {
            let _guard = WORKSPACE_INITIALIZATION
                .lock()
                .map_err(|_| failure("Workspace initialization lock failed."))?;
            let root = absolute(&arguments["root"])?;
            let folder = root.join("Workspaces");
            let identity = apply("storage.administration.workspaceIdentity", arguments)?;
            let path = folder.join(identity["folderId"].as_str().unwrap());
            if operation.ends_with("workspaceVerify") {
                absolute(&arguments["path"])?;
            }
            let mismatch =
                |message: &str, code: &'static str| ContractError::new(message, code, 409);
            if operation.ends_with("workspaceEnsure") {
                ensure(&path)?;
                let marker = path.join(".workspace-owner.json");
                if !inspect(&marker, true, false)? {
                    if fs::read_dir(&path).map_err(io_error)?.next().is_some() {
                        return Err(mismatch(
                            "已有工具工作文件缺少归属记录，未接管此目录。",
                            "SANDBOX_WORKSPACE_OWNER_MISMATCH",
                        ));
                    }
                    let raw = format!(
                        "{}\n",
                        serde_json::to_string(
                            &json!({"schemaVersion":1,"conversationId":identity["id"]})
                        )
                        .unwrap()
                    );
                    let mut handle = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&marker)
                        .map_err(io_error)?;
                    handle.write_all(raw.as_bytes()).map_err(io_error)?;
                    handle.sync_all().map_err(io_error)?;
                }
            }
            if !inspect(&path, false, false)? {
                return Err(mismatch(
                    "聊天工具工作区不存在或已更改。",
                    "SANDBOX_WORKSPACE_CHANGED",
                ));
            }
            let canonical = fs::canonicalize(&path).map_err(io_error)?;
            let canonical_folder = fs::canonicalize(&folder).map_err(io_error)?;
            let public_path = canonical
                .to_string_lossy()
                .strip_prefix(r"\\?\")
                .unwrap_or(&canonical.to_string_lossy())
                .to_owned();
            let public_folder = canonical_folder
                .to_string_lossy()
                .strip_prefix(r"\\?\")
                .unwrap_or(&canonical_folder.to_string_lossy())
                .to_owned();
            let canonical_expected =
                Path::new(&public_folder).join(identity["folderId"].as_str().unwrap());
            if !within(&canonical_expected, Path::new(&public_path))
                || !within(Path::new(&public_path), &canonical_expected)
            {
                return Err(ContractError::new(
                    "聊天工具工作区解析到其他目录。",
                    "UNSAFE_TOOL_PATH",
                    403,
                ));
            }
            inspect(Path::new(&public_path), false, false)?;
            if let Some(candidate) = arguments["path"].as_str() {
                let candidate = Path::new(candidate);
                if !(within(candidate, &path) && within(&path, candidate)
                    || within(candidate, Path::new(&public_path))
                        && within(Path::new(&public_path), candidate))
                {
                    return Err(ContractError::new(
                        "聊天工具工作区身份不匹配。",
                        "SANDBOX_WORKSPACE_MISMATCH",
                        403,
                    ));
                }
            }
            let marker = Path::new(&public_path).join(".workspace-owner.json");
            let bytes =
                managed_files::read(&self::root(&marker), &marker, 1024)?.ok_or_else(|| {
                    mismatch(
                        "聊天工具工作区缺少有效的归属记录。",
                        "SANDBOX_WORKSPACE_OWNER_MISMATCH",
                    )
                })?;
            let owner: Value = serde_json::from_slice(&bytes).map_err(|_| {
                mismatch(
                    "聊天工具工作区归属记录损坏。",
                    "SANDBOX_WORKSPACE_OWNER_MISMATCH",
                )
            })?;
            if owner["schemaVersion"] != 1 || owner["conversationId"] != identity["id"] {
                return Err(mismatch(
                    "聊天工具工作区属于其他聊天或版本不受支持。",
                    "SANDBOX_WORKSPACE_OWNER_MISMATCH",
                ));
            }
            Ok(json!({"path":public_path,"folder":public_folder}))
        }
        "storage.administration.modelHome" => {
            let application = absolute(&arguments["applicationRoot"])?;
            if let Some(path) = arguments["modelHome"]
                .as_str()
                .filter(|value| !value.is_empty())
            {
                return Ok(json!(configured_path(&json!(path), &application)?));
            }
            if let Some(path) = arguments["dataHome"]
                .as_str()
                .filter(|value| !value.is_empty())
            {
                return Ok(json!(
                    configured_path(&json!(path), &application)?.join("Models")
                ));
            }
            let pointer = application.join("storage.json");
            let Some(bytes) = managed_files::read(&root(&pointer), &pointer, 16 * 1024)? else {
                return Ok(json!(application.join("Data").join("Models")));
            };
            let source =
                std::str::from_utf8(&bytes).map_err(|_| failure("Invalid storage pointer UTF-8."))?;
            let document: Value = serde_json::from_str(&raw_json::scalar_projection(
                source.strip_prefix('\u{feff}').unwrap_or(source),
            ))
            .map_err(|_| failure("Invalid storage pointer JSON."))?;
            if document["version"].as_u64() != Some(1) {
                return Err(failure("KYNXA 存储目录配置版本不受支持。"));
            }
            Ok(json!(
                configured_path(&document["dataRoot"], &application)?.join("Models")
            ))
        }
        "storage.administration.migrationLock" => {
            let path = absolute(&arguments["path"])?;
            match optional(&path) {
                Ok(None) => Ok(json!({"exists":false})),
                Ok(Some(bytes)) => {
                    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                    Ok(json!({"exists":true,"pid":value["pid"].as_u64().filter(|pid|*pid>0)}))
                }
                Err(_) => Ok(json!({"exists":true,"pid":null})),
            }
        }
        "storage.administration.migrationActive" => Ok(json!(
            arguments["exists"] == true
                && (arguments["pid"].is_null() || arguments["probeError"] != "ESRCH")
        )),
        "storage.administration.backup" => {
            backup(
                &absolute(&arguments["source"])?,
                &absolute(&arguments["target"])?,
            )?;
            Ok(Value::Null)
        }
        "storage.administration.prepareMigration" => {
            let (conversations, roots) = roots(arguments)?;
            Ok(json!({"conversationRoot":conversations,"roots":roots}))
        }
        "storage.administration.copyMigration" => {
            let (_, entries) = roots(arguments)?;
            let target = absolute(&arguments["target"])?;
            let pointer = absolute(&arguments["pointer"])?;
            let snapshots = entries
                .iter()
                .map(snapshot)
                .collect::<Result<Vec<_>, _>>()?;
            let old_pointer = optional(&pointer)?;
            ensure(&target)?;
            let mut verified = Vec::new();
            for (index, entry) in entries.iter().enumerate() {
                let source = absolute(&entry["source"])?;
                let destination = target.join(entry["name"].as_str().unwrap());
                ensure(&destination)?;
                for directory in snapshots[index]["directories"].as_array().unwrap() {
                    ensure(&destination.join(directory.as_str().unwrap()))?;
                }
                for file in snapshots[index]["files"].as_array().unwrap() {
                    let source = source.join(file.as_str().unwrap());
                    let digest =
                        copy_file(&source, &destination.join(file.as_str().unwrap()), false)?;
                    verified.push(json!({"source":source,"digest":digest}));
                }
            }
            let mut documents = vec![
                target.join("Desktop").join("projects.json"),
                target.join("catalog.json"),
                target.join(".catalog-transaction.json"),
            ];
            let projects = target.join("Projects");
            if inspect(&projects, false, false)? {
                for entry in fs::read_dir(projects).map_err(io_error)? {
                    let entry = entry.map_err(io_error)?;
                    if entry.file_type().map_err(io_error)?.is_dir() {
                        documents.push(entry.path().join("project.json"));
                    }
                }
            }
            documents.push(target.join("Models").join("local-server.json"));
            Ok(
                json!({"roots":entries,"snapshots":snapshots,"verified":verified,"pointerHash":old_pointer.as_ref().map(|bytes|managed_files::digest(bytes)),"documents":documents}),
            )
        }
        "storage.administration.verifyMigration" => {
            verify(&arguments["plan"])?;
            Ok(Value::Null)
        }
        "storage.administration.relocations" => {
            let desktop = absolute(&arguments["desktopSource"])?;
            let target = absolute(&arguments["target"])?;
            let mut assignments = Vec::new();
            projection(
                &arguments["value"],
                &mut Vec::new(),
                &mut assignments,
                &desktop,
                &target,
                arguments["local"] == true,
                arguments["roots"].as_array().unwrap_or(&Vec::new()),
            );
            Ok(json!(assignments))
        }
        "storage.administration.finishMigration" => {
            let target = absolute(&arguments["target"])?;
            let pointer = absolute(&arguments["pointer"])?;
            let previous = optional(&pointer)?;
            if previous
                .as_ref()
                .map(|bytes| managed_files::digest(bytes))
                .as_deref()
                != arguments["pointerHash"].as_str()
            {
                return Err(failure("Storage pointer changed during migration."));
            }
            if let Some(bytes) = previous {
                let path = target.join("storage-pointer.previous.json");
                if inspect(&path, true, false)? {
                    return Err(failure("Previous pointer backup already exists."));
                }
                managed_files::atomic_write(&target, &path, &bytes, u64::MAX)?;
            }
            let info = arguments["rawInfo"]
                .as_str()
                .ok_or_else(|| failure("Invalid migration information."))?;
            managed_files::atomic_write(
                &target,
                &target.join("migration-info.json"),
                info.as_bytes(),
                1024 * 1024,
            )?;
            let raw = arguments["rawPointer"]
                .as_str()
                .ok_or_else(|| failure("Invalid storage pointer."))?;
            ensure(pointer.parent().unwrap())?;
            if optional(&pointer)?
                .as_ref()
                .map(|bytes| managed_files::digest(bytes))
                .as_deref()
                != arguments["pointerHash"].as_str()
            {
                return Err(failure("Storage pointer changed during migration."));
            }
            managed_files::atomic_write(&root(&pointer), &pointer, raw.as_bytes(), 1024 * 1024)?;
            Ok(Value::Null)
        }
        _ => Err(failure("Unknown storage administration operation.")),
    }
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    calculate(operation, arguments).map_err(|error| {
        if operation.starts_with("storage.administration.workspace") {
            if error.code == "INVALID_MEMORY_PATH" {
                return ContractError::new(
                    "工具路径包含链接或不安全结构，操作已拒绝。",
                    "UNSAFE_TOOL_PATH",
                    403,
                );
            }
            if error.code == "CORRUPT_MEMORY" {
                return ContractError::new(
                    "聊天工具工作区缺少有效的归属记录。",
                    "SANDBOX_WORKSPACE_OWNER_MISMATCH",
                    409,
                );
            }
            if error.code == "STORAGE_MIGRATION_FAILED" {
                return ContractError::new(error.message, "INVALID_SANDBOX_WORKSPACE", 400);
            }
        }
        error
    })
}
