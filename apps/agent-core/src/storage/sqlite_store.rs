use super::{ContractError, cancellation, managed_files};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{
    Connection, OpenFlags, Row, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn failure(error: impl std::fmt::Display) -> ContractError {
    ContractError::new(
        format!("SQLite index service failed. / SQLite 索引服务失败：{error}"),
        "RETRIEVAL_INDEX_FAILED",
        500,
    )
}

/// Upgrade relational constraints in one transaction, preserving rows, column order, indexes and triggers.
/// 在同一事务中升级关系约束，保留全部记录、列序、索引与触发器；旧记录不合格时回滚，不删数据。
pub(crate) fn migrate_integrity(
    connection: &Connection,
    target_version: i64,
    tables: &[(&str, &str)],
    preflight: &[&str],
) -> Result<(), ContractError> {
    let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0)).map_err(failure)?;
    if current >= target_version { return Ok(()); }
    let rejected = |message: &str| ContractError::new(
        format!("Integrity migration requires review; original records are preserved. / 完整性迁移需核对旧记录，原数据已保留：{message}"),
        "STORAGE_INTEGRITY_MIGRATION_REQUIRED", 409);
    if !connection.is_autocommit() { return Err(rejected("an existing transaction is active")); }
    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row|row.get(0)).map_err(failure)?;
    let legacy_alter: i64 = connection.query_row("PRAGMA legacy_alter_table", [], |row|row.get(0)).map_err(failure)?;
    if let Err(error) = connection.execute_batch("PRAGMA foreign_keys=OFF;PRAGMA legacy_alter_table=ON;BEGIN IMMEDIATE") {
        let _ = connection.execute_batch(&format!("PRAGMA foreign_keys={foreign_keys};PRAGMA legacy_alter_table={legacy_alter}"));
        return Err(failure(error));
    }
    let result = (|| {
        let locked_version:i64 = connection.query_row("PRAGMA user_version", [], |row|row.get(0)).map_err(failure)?;
        if locked_version >= target_version {
            connection.execute_batch("COMMIT").map_err(failure)?;
            return Ok(());
        }
        for query in preflight {
            let invalid: bool = connection.query_row(query, [], |row| row.get(0)).map_err(failure)?;
            if invalid { return Err(rejected(query)); }
        }
        let invalid_foreign_key: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)", [], |row|row.get(0)).map_err(failure)?;
        if invalid_foreign_key { return Err(rejected("foreign-key ownership mismatch")); }
        let primary_key = regex::Regex::new(r"(?i)\bTEXT\s+PRIMARY\s+KEY\b").unwrap();
        let mut rebuilt = Vec::new();
        let mut ancillary = Vec::new();
        for (table, constraints) in tables {
            let mut statement = connection.prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name=?").map_err(failure)?;
            let sql = statement.query_map([table], |row| row.get::<_,String>(0)).map_err(failure)?.next().transpose().map_err(failure)?;
            let Some(sql) = sql else { continue; };
            drop(statement);
            let null_primary: bool = connection.query_row(&format!(
                "SELECT EXISTS(SELECT 1 FROM \"{table}\" WHERE {})",
                rows(connection, &format!("PRAGMA table_info(\"{table}\")"), vec![])?.iter()
                    .filter(|column| column["pk"].as_i64().unwrap_or(0)>0)
                    .map(|column| format!("\"{}\" IS NULL", column["name"].as_str().unwrap_or("").replace('"', "\"\"")))
                    .collect::<Vec<_>>().join(" OR ")
            ), [], |row|row.get(0)).map_err(failure)?;
            if null_primary { return Err(rejected(&format!("{table}: null primary key"))); }
            let saved = rows(connection,"SELECT sql FROM sqlite_master WHERE tbl_name=? AND type IN ('index','trigger') AND sql IS NOT NULL",vec![sql_text(*table)])?;
            ancillary.extend(saved.into_iter().filter_map(|row|row["sql"].as_str().map(str::to_owned)));
            let opening = sql.find('(').ok_or_else(||rejected("table definition is invalid"))?;
            let mut definition = primary_key.replace_all(&sql[opening..], "TEXT NOT NULL PRIMARY KEY").into_owned();
            if !constraints.is_empty() {
                let closing = definition.rfind(')').ok_or_else(||rejected("table definition is invalid"))?;
                definition.insert_str(closing, &format!(", {constraints}"));
            }
            let temporary = format!("__kynxa_integrity_{table}");
            connection.execute_batch(&format!("CREATE TABLE \"{temporary}\" {definition};INSERT INTO \"{temporary}\" SELECT * FROM \"{table}\";"))
                .map_err(|error|rejected(&error.to_string()))?;
            rebuilt.push((*table, temporary));
        }
        for (table, _) in &rebuilt { connection.execute_batch(&format!("DROP TABLE \"{table}\";")).map_err(failure)?; }
        for (table, temporary) in &rebuilt { connection.execute_batch(&format!("ALTER TABLE \"{temporary}\" RENAME TO \"{table}\";")).map_err(failure)?; }
        for sql in ancillary { connection.execute_batch(&sql).map_err(failure)?; }
        let invalid_foreign_key: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)", [], |row|row.get(0)).map_err(failure)?;
        if invalid_foreign_key { return Err(rejected("migrated foreign-key ownership mismatch")); }
        connection.execute_batch(&format!("PRAGMA user_version={target_version};COMMIT")).map_err(failure)?;
        Ok(())
    })();
    if result.is_err() { let _ = connection.execute_batch("ROLLBACK"); }
    connection.execute_batch(&format!("PRAGMA foreign_keys={foreign_keys};PRAGMA legacy_alter_table={legacy_alter}")).map_err(failure)?;
    result
}
pub fn cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        Err(ContractError::new(
            "Retrieval cancelled. / 检索已取消。",
            "ABORT_ERR",
            499,
        ))
    } else {
        Ok(())
    }
}
pub fn inspect_path(
    root: &Path,
    path: &Path,
    create: bool,
    file: bool,
) -> Result<bool, ContractError> {
    managed_files::inspect(root, path, create, file, u64::MAX).map_err(|error| {
        if error.code == "INVALID_MEMORY_PATH" { ContractError::new("Unsafe retrieval path; the original is preserved. / 检索路径包含链接或不安全结构，原文件已保留。", "UNSAFE_RETRIEVAL_PATH", 409) } else { error }
    })
}

/// A closing SQLite connection may unlink WAL/SHM between metadata and handle inspection.
/// SQLite 连接关闭可能在元信息与句柄检查之间移除并重建 WAL/SHM；有界重试仍执行完整路径及链接核验。
pub(crate) fn inspect_sidecar(root: &Path, path: &Path) -> Result<bool, ContractError> {
    let mut previous = None;
    for _ in 0..4 {
        match inspect_path(root, path, false, true) {
            Ok(exists) => return Ok(exists),
            Err(error) => {
                if !matches!(error.code, "UNSAFE_RETRIEVAL_PATH" | "STORAGE_IO_FAILED") {
                    return Err(error);
                }
                previous = Some(error);
                std::thread::yield_now();
            }
        }
    }
    Err(previous.unwrap())
}
pub fn text(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap_or("").to_owned()
}
pub fn sql_text(value: impl Into<String>) -> SqlValue {
    SqlValue::Text(value.into())
}
pub fn number(value: &Value) -> SqlValue {
    value
        .as_i64()
        .map(SqlValue::Integer)
        .unwrap_or(SqlValue::Null)
}
pub fn nullable_text(value: &Value) -> SqlValue {
    value
        .as_str()
        .map(|value| sql_text(value.to_owned()))
        .unwrap_or(SqlValue::Null)
}
pub fn sql_execute(
    connection: &Connection,
    sql: &str,
    parameters: Vec<SqlValue>,
) -> Result<usize, ContractError> {
    connection
        .prepare_cached(sql)
        .map_err(failure)?
        .execute(params_from_iter(parameters))
        .map_err(failure)
}
pub(crate) fn row_projection(row: &Row<'_>) -> rusqlite::Result<Value> {
    let mut record = serde_json::Map::new();
    for index in 0..row.as_ref().column_count() {
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(value) => json!(value),
            ValueRef::Real(value) => json!(value),
            ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
            ValueRef::Blob(value) => json!(STANDARD.encode(value)),
        };
        record.insert(row.as_ref().column_name(index)?.to_owned(), value);
    }
    Ok(Value::Object(record))
}
pub fn rows(
    connection: &Connection,
    sql: &str,
    parameters: Vec<SqlValue>,
) -> Result<Vec<Value>, ContractError> {
    connection
        .prepare_cached(sql)
        .map_err(failure)?
        .query_map(params_from_iter(parameters), row_projection)
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)
}

pub fn open(root: &Path, path: &Path, create: bool) -> Result<Connection, ContractError> {
    cancelled()?;
    if create {
        inspect_path(
            root,
            path.parent().ok_or_else(|| failure("missing parent"))?,
            true,
            false,
        )?;
    }
    if !inspect_path(root, path, false, true)? && !create {
        return Err(failure("index is missing"));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let candidate = format!("{}{suffix}", path.to_string_lossy());
        inspect_sidecar(root, Path::new(&candidate))?;
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | if create {
                OpenFlags::SQLITE_OPEN_CREATE
            } else {
                OpenFlags::empty()
            },
    )
    .map_err(failure)?;
    connection.set_prepared_statement_cache_capacity(128);
    connection
        .busy_timeout(Duration::from_secs(3))
        .map_err(failure)?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(failure)?;
    if version > 6 {
        return Err(ContractError::new(
            "A newer retrieval index exists. / 检索索引由更新版本创建，原文件已保留。",
            "UNSUPPORTED_RETRIEVAL_INDEX_VERSION",
            409,
        ));
    }
    connection
        .execute_batch("PRAGMA foreign_keys=ON;PRAGMA synchronous=FULL;")
        .map_err(failure)?;
    let started = Instant::now();
    connection
        .progress_handler(
            256,
            Some(move || cancellation::is_cancelled() || started.elapsed() >= Duration::from_secs(120)),
        )
        .map_err(failure)?;
    Ok(connection)
}

pub fn generation(connection: &Connection) -> Result<i64, ContractError> {
    let value: String = connection
        .prepare_cached("SELECT value FROM retrieval_metadata WHERE key='generation'")
        .map_err(failure)?
        .query_row([], |row| row.get(0))
        .map_err(failure)?;
    value.parse().map_err(failure)
}
pub fn snapshot(connection: &Connection) -> Result<String, ContractError> {
    let epoch: String = connection
        .prepare_cached("SELECT value FROM retrieval_metadata WHERE key='index_epoch'")
        .map_err(failure)?
        .query_row([], |row| row.get(0))
        .map_err(failure)?;
    Ok(format!("{epoch}:{}", generation(connection)?))
}
pub fn next_generation(
    connection: &Connection,
    scope: &str,
    corpus_changed: bool,
    lexical_changed: bool,
) -> Result<i64, ContractError> {
    let next = generation(connection)? + 1;
    sql_execute(
        connection,
        "UPDATE retrieval_metadata SET value=? WHERE key='generation'",
        vec![sql_text(next.to_string())],
    )?;
    sql_execute(
        connection,
        "INSERT INTO scope_snapshots(scope_key,generation) VALUES (?,?) ON CONFLICT(scope_key) DO UPDATE SET generation=excluded.generation",
        vec![sql_text(scope), SqlValue::Integer(next)],
    )?;
    if lexical_changed {
        sql_execute(
            connection,
            "INSERT INTO retrieval_metadata(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            vec![
                sql_text(format!("lexical_generation:{scope}")),
                sql_text(next.to_string()),
            ],
        )?;
    }
    if corpus_changed {
        sql_execute(
            connection,
            "INSERT INTO retrieval_metadata(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            vec![
                sql_text(format!("corpus_generation:{scope}")),
                sql_text(next.to_string()),
            ],
        )?;
    } else {
        sql_execute(
            connection,
            "INSERT OR IGNORE INTO retrieval_metadata(key,value) VALUES (?,?)",
            vec![
                sql_text(format!("corpus_generation:{scope}")),
                sql_text("0"),
            ],
        )?;
    }
    Ok(next)
}
pub fn finish(connection: &Connection, result: super::ContractResult) -> super::ContractResult {
    match result {
        Ok(value) => {
            if let Err(error) = cancelled() {
                connection.execute_batch("ROLLBACK").map_err(failure)?;
                return Err(error);
            }
            connection.execute_batch("COMMIT").map_err(failure)?;
            Ok(value)
        }
        Err(error) => {
            connection.execute_batch("ROLLBACK").map_err(failure)?;
            if cancellation::is_cancelled() {
                return cancelled().map(|_| Value::Null);
            }
            Err(error)
        }
    }
}
