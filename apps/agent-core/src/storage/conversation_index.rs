use super::{ContractError, ContractResult, managed_files};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

static INDEX_TRANSACTIONS: Mutex<()> = Mutex::new(());
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
fn failure(message: impl Into<String>, code: &'static str, status: u16) -> ContractError {
    ContractError::new(message, code, status)
}
fn io_error(error: impl std::fmt::Display) -> ContractError {
    failure(
        format!("Conversation index failed. / 会话索引失败：{error}"),
        "CONVERSATION_INDEX_FAILED",
        500,
    )
}
fn invalid() -> ContractError {
    failure(
        "无法从无效的会话目录构建索引。",
        "INVALID_INDEX_DOCUMENT",
        500,
    )
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        _ => true,
    }
}
fn required_text(value: &Value) -> Result<&str, ContractError> {
    value
        .as_str()
        .filter(|value| !super::trim(value).is_empty())
        .ok_or_else(invalid)
}
fn cancelled() -> Result<(), ContractError> {
    if super::cancellation::is_cancelled() {
        Err(failure("会话索引已取消。", "ABORT_ERR", 499))
    } else {
        Ok(())
    }
}

pub(crate) fn inspect(root: &Path) -> Result<(PathBuf, bool), ContractError> {
    let directory = root.join("Index");
    let path = directory.join("search.sqlite");
    managed_files::inspect(root, &directory, false, false, u64::MAX).map_err(|_| {
        failure(
            "索引位置包含链接或不兼容的文件类型，原位置已保留。",
            "UNSAFE_INDEX_PATH",
            500,
        )
    })?;
    let exists = managed_files::inspect(root, &path, false, true, u64::MAX).map_err(|_| {
        failure(
            "索引位置包含链接或不兼容的文件类型，原位置已保留。",
            "UNSAFE_INDEX_PATH",
            500,
        )
    })?;
    for suffix in ["-wal", "-shm", "-journal"] {
        if managed_files::inspect(
            root,
            Path::new(&format!("{}{suffix}", path.to_string_lossy())),
            false,
            true,
            u64::MAX,
        )? {
            return Err(failure(
                "索引正在被其他程序使用，请关闭该程序后重试。",
                "CONVERSATION_INDEX_BUSY",
                500,
            ));
        }
    }
    if exists {
        let mut buffer = [0u8; 100];
        let count = File::open(&path)
            .map_err(io_error)?
            .read(&mut buffer)
            .map_err(io_error)?;
        if count >= 64
            && &buffer[..16] == b"SQLite format 3\0"
            && u32::from_be_bytes(buffer[60..64].try_into().unwrap()) > 1
        {
            return Err(failure(
                "索引由更新版本的软件创建，请使用新版软件打开。",
                "UNSUPPORTED_INDEX_VERSION",
                409,
            ));
        }
    }
    Ok((path, exists))
}

fn projection(document: &Value) -> ContractResult {
    if document["Version"] != 1
        || !super::safe_integer(&document["Revision"], 0, super::MAX_SAFE_INTEGER)
        || !document["Projects"].is_array()
        || !document["Chats"].is_array()
    {
        return Err(invalid());
    }
    let mut projects = Vec::new();
    let mut sessions = Vec::new();
    let mut project_ids = std::collections::HashSet::new();
    let mut session_ids = std::collections::HashSet::new();
    let mut add_session = |chat: &Value,
                           order: usize,
                           project: Option<&Value>|
     -> Result<(), ContractError> {
        let id = required_text(&chat["Id"])?;
        if !session_ids.insert(id.to_lowercase()) {
            return Err(invalid());
        }
        sessions.push(json!({"id": id, "project_id": project.map(|project| project["id"].clone()), "title": required_text(&chat["Title"])?, "sort_order": order, "pinned": u8::from(truthy(&chat["IsPinned"])), "archived": u8::from(truthy(&chat["IsArchived"])), "workspace_path": project.map(|project| project["workspace_path"].clone()).unwrap_or(Value::Null)}));
        Ok(())
    };
    for (order, source) in document["Projects"].as_array().unwrap().iter().enumerate() {
        let id = required_text(&source["Id"])?;
        if !project_ids.insert(id.to_lowercase())
            || !source["Chats"].is_array()
            || !source["FolderPath"].is_null() && !source["FolderPath"].is_string()
        {
            return Err(invalid());
        }
        let project = json!({"id": id, "name": required_text(&source["Name"])?, "workspace_path": source["FolderPath"], "sort_order": order, "pinned": u8::from(truthy(&source["IsPinned"])), "archived": u8::from(truthy(&source["IsArchived"]))});
        for (index, chat) in source["Chats"].as_array().unwrap().iter().enumerate() {
            add_session(chat, index, Some(&project))?;
        }
        projects.push(project);
    }
    for (order, chat) in document["Chats"].as_array().unwrap().iter().enumerate() {
        add_session(chat, order, None)?;
    }
    Ok(
        json!({"schemaVersion": 1, "catalogVersion": document["Version"], "revision": document["Revision"], "projects": projects, "sessions": sessions}),
    )
}

fn state(path: &Path, data: &Value) -> Result<(bool, bool), ContractError> {
    let result = (|| -> rusqlite::Result<(bool, bool)> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != 1 {
            return Ok((true, false));
        }
        let mut check = connection.prepare("PRAGMA quick_check")?;
        if check
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|value| value != "ok")
        {
            return Ok((true, false));
        }
        connection.prepare(
            "SELECT id,name,workspace_path,sort_order,pinned,archived FROM projects LIMIT 0",
        )?;
        connection.prepare("SELECT id,project_id,title,sort_order,pinned,archived,workspace_path FROM sessions LIMIT 0")?;
        let fingerprint: String = connection.query_row(
            "SELECT value FROM metadata WHERE key='fingerprint'",
            [],
            |row| row.get(0),
        )?;
        let revision: String = connection.query_row(
            "SELECT value FROM metadata WHERE key='catalog_revision'",
            [],
            |row| row.get(0),
        )?;
        let projects: i64 =
            connection.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        let sessions: i64 =
            connection.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok((
            false,
            fingerprint == data["fingerprint"].as_str().unwrap_or("")
                && revision == data["revision"].to_string()
                && projects as usize == data["projects"].as_array().unwrap().len()
                && sessions as usize == data["sessions"].as_array().unwrap().len(),
        ))
    })();
    Ok(result.unwrap_or((true, false)))
}

fn write_database(path: &Path, data: &Value) -> Result<(), ContractError> {
    let connection = Connection::open(path).map_err(io_error)?;
    connection.execute_batch("PRAGMA journal_mode=DELETE;PRAGMA synchronous=FULL;PRAGMA foreign_keys=ON;BEGIN IMMEDIATE;
        CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE projects(id TEXT PRIMARY KEY COLLATE NOCASE,name TEXT NOT NULL,workspace_path TEXT,sort_order INTEGER NOT NULL,pinned INTEGER NOT NULL,archived INTEGER NOT NULL);
        CREATE TABLE sessions(id TEXT PRIMARY KEY COLLATE NOCASE,project_id TEXT REFERENCES projects(id),title TEXT NOT NULL,sort_order INTEGER NOT NULL,pinned INTEGER NOT NULL,archived INTEGER NOT NULL,workspace_path TEXT);
        CREATE INDEX sessions_project_order ON sessions(project_id,sort_order);PRAGMA user_version=1;").map_err(io_error)?;
    for row in data["projects"].as_array().unwrap() {
        cancelled()?;
        connection
            .execute(
                "INSERT INTO projects VALUES (?,?,?,?,?,?)",
                params![
                    row["id"].as_str(),
                    row["name"].as_str(),
                    row["workspace_path"].as_str(),
                    row["sort_order"].as_i64(),
                    row["pinned"].as_i64(),
                    row["archived"].as_i64()
                ],
            )
            .map_err(io_error)?;
    }
    for row in data["sessions"].as_array().unwrap() {
        cancelled()?;
        connection
            .execute(
                "INSERT INTO sessions VALUES (?,?,?,?,?,?,?)",
                params![
                    row["id"].as_str(),
                    row["project_id"].as_str(),
                    row["title"].as_str(),
                    row["sort_order"].as_i64(),
                    row["pinned"].as_i64(),
                    row["archived"].as_i64(),
                    row["workspace_path"].as_str()
                ],
            )
            .map_err(io_error)?;
    }
    connection
        .execute(
            "INSERT INTO metadata VALUES ('catalog_revision',?)",
            [data["revision"].to_string()],
        )
        .map_err(io_error)?;
    connection
        .execute(
            "INSERT INTO metadata VALUES ('fingerprint',?)",
            [data["fingerprint"].as_str().unwrap_or("")],
        )
        .map_err(io_error)?;
    cancelled()?;
    connection.execute_batch("COMMIT").map_err(io_error)?;
    Ok(())
}

/// Derived metadata publication checks newer formats, preserves corrupt originals and releases all SQLite handles.
/// 派生元信息发布检查新版本，保留损坏原文件，并在替换前释放所有 SQLite 句柄。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    apply_service(operation, arguments).map_err(|error| {
        if error.code == "INVALID_MEMORY_PATH" {
            failure(
                "索引位置包含链接或不兼容的文件类型，原位置已保留。",
                "UNSAFE_INDEX_PATH",
                500,
            )
        } else {
            error
        }
    })
}

fn apply_service(operation: &str, arguments: &Value) -> ContractResult {
    if operation == "storage.catalogIndex.project" {
        return projection(&arguments["document"]);
    }
    let _guard = INDEX_TRANSACTIONS
        .lock()
        .map_err(|_| io_error("index lock failed"))?;
    cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let (path, exists) = inspect(root)?;
    if operation == "storage.catalogIndex.inspect" {
        return Ok(json!({"path": path.to_string_lossy(), "exists": exists}));
    }
    if operation != "storage.catalogIndex.rebuild" {
        return Err(invalid());
    }
    let data = &arguments["data"];
    let original = if exists {
        managed_files::read(root, &path, u64::MAX)?
    } else {
        None
    };
    let (corrupt, unchanged) = if exists {
        state(&path, data)?
    } else {
        (false, false)
    };
    if unchanged {
        return Ok(json!({"path": path.to_string_lossy(), "rebuilt": false}));
    }
    let directory = path.parent().unwrap();
    managed_files::inspect(root, directory, true, false, u64::MAX)?;
    let identity = format!(
        "{}-{}",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    );
    let temporary = directory.join(format!(".search-{identity}.tmp"));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error)?;
    let result = (|| {
        write_database(&temporary, data)?;
        inspect(root)?;
        let current = managed_files::read(root, &path, u64::MAX)?;
        if current != original {
            return Err(failure(
                "索引在更新期间变化，现有索引已保留，请重试。",
                "CONVERSATION_INDEX_CHANGED",
                409,
            ));
        }
        let recovered = if corrupt {
            let backup = directory.join(format!("search.sqlite.recovered-{identity}"));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)
                .map_err(io_error)?;
            file.write_all(original.as_ref().unwrap())
                .map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            Some(backup)
        } else {
            None
        };
        cancelled()?;
        fs::rename(&temporary, &path).map_err(io_error)?;
        let mut receipt = json!({"path": path.to_string_lossy(), "rebuilt": true});
        if let Some(recovered) = recovered {
            receipt["recoveredPath"] = json!(recovered.to_string_lossy());
        }
        Ok(receipt)
    })();
    for candidate in [
        &temporary,
        &PathBuf::from(format!("{}-journal", temporary.to_string_lossy())),
    ] {
        if managed_files::inspect(root, candidate, false, true, u64::MAX)? {
            fs::remove_file(candidate).map_err(io_error)?;
        }
    }
    result
}
