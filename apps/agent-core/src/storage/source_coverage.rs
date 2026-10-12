use super::{ContractError, ContractResult, cancellation, document_coverage, retrieval};
use rusqlite::{
    Connection, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;

fn failure(error: impl std::fmt::Display) -> ContractError {
    ContractError::new(
        format!("Coverage service failed. / 来源覆盖服务失败：{error}"),
        "RETRIEVAL_INDEX_FAILED",
        500,
    )
}
fn invalid() -> ContractError {
    ContractError::new(
        "Invalid scoped source coverage. / 来源覆盖范围或状态无效。",
        "INVALID_RETRIEVAL_INPUT",
        400,
    )
}
fn cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        Err(ContractError::new(
            "Coverage operation cancelled. / 来源覆盖操作已取消。",
            "ABORT_ERR",
            499,
        ))
    } else {
        Ok(())
    }
}
fn string(value: &str) -> SqlValue {
    SqlValue::Text(value.to_owned())
}
fn rows(
    connection: &Connection,
    sql: &str,
    parameters: Vec<SqlValue>,
) -> Result<Vec<Value>, ContractError> {
    connection
        .prepare_cached(sql)
        .map_err(failure)?
        .query_map(params_from_iter(parameters), |row| {
            let mut values = serde_json::Map::new();
            for index in 0..row.as_ref().column_count() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Blob(_) => Value::Null,
                };
                values.insert(row.as_ref().column_name(index)?.to_owned(), value);
            }
            Ok(Value::Object(values))
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)
}
fn execute(
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
fn placeholders(values: &[Value]) -> String {
    vec!["?"; values.len()].join(",")
}
fn parameters(values: &[Value]) -> Vec<SqlValue> {
    values
        .iter()
        .map(|value| string(value.as_str().unwrap_or("")))
        .collect()
}
fn valid_source_type(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|value| !value.is_empty() && value.encode_utf16().count() <= 100)
}
fn state(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        [
            "pending",
            "ready",
            "partial",
            "failed",
            "skipped",
            "unverified",
            "disabled",
        ]
        .contains(&value)
    })
}

pub(crate) fn initialize(connection: &Connection) -> Result<(), ContractError> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS source_coverage (scope_key TEXT NOT NULL,source_id TEXT NOT NULL,relative_path TEXT NOT NULL,status TEXT NOT NULL,lexical TEXT NOT NULL,semantic TEXT NOT NULL,parser TEXT NOT NULL,error_code TEXT,source_revision TEXT,updated_at TEXT NOT NULL,PRIMARY KEY(scope_key,source_id));").map_err(failure)?;
    let columns = rows(connection, "PRAGMA table_info(source_coverage)", vec![])?;
    if !columns
        .iter()
        .any(|column| column["name"] == "document_coverage")
    {
        connection
            .execute_batch("ALTER TABLE source_coverage ADD COLUMN document_coverage TEXT")
            .map_err(failure)?;
    }
    if !columns.iter().any(|column| column["name"] == "source_type") {
        connection
            .execute_batch(
                "ALTER TABLE source_coverage ADD COLUMN source_type TEXT NOT NULL DEFAULT ''",
            )
            .map_err(failure)?;
    }
    let has_type = rows(connection, "PRAGMA table_info(sources)", vec![])?
        .iter()
        .any(|column| column["name"] == "source_type");
    let published_type = if has_type {
        "(SELECT source_type FROM sources WHERE sources.source_id=source_coverage.source_id AND sources.scope_key=source_coverage.scope_key)"
    } else {
        "NULL"
    };
    connection.execute_batch(&format!("UPDATE source_coverage SET source_type=COALESCE({published_type},CASE WHEN source_revision='null' THEN 'work-file' ELSE '' END) WHERE source_type=''" )).map_err(failure)?;
    Ok(())
}

fn record(connection: &Connection, arguments: &Value, scopes: &[Value]) -> ContractResult {
    let entries = arguments["entries"]
        .as_array()
        .filter(|entries| entries.len() <= 100)
        .ok_or_else(invalid)?;
    for entry in entries {
        cancelled()?;
        retrieval::apply(
            "storage.retrieval.sourceId",
            &json!({"value": entry["sourceId"]}),
        )?;
        if !scopes.contains(&entry["scopeKey"])
            || !entry["relativePath"]
                .as_str()
                .is_some_and(|value| value.encode_utf16().count() <= 4096)
            || ["status", "lexical", "semantic", "parser"]
                .iter()
                .any(|field| !state(&entry[field]))
            || entry
                .get("sourceType")
                .is_some_and(|value| !valid_source_type(value))
        {
            return Err(invalid());
        }
        if let Some(code) = entry.get("errorCode") {
            if !code.as_str().is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.as_bytes()[0].is_ascii_uppercase()
                    && value.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            }) {
                return Err(invalid());
            }
        }
        if entry["coverageRaw"].is_string() {
            document_coverage::validate(&entry["documentCoverage"])?;
        }
        let mut values: Vec<SqlValue> = [
            "scopeKey",
            "sourceId",
            "relativePath",
            "status",
            "lexical",
            "semantic",
            "parser",
        ]
        .iter()
        .map(|field| string(entry[field].as_str().unwrap_or("")))
        .collect();
        values.push(
            entry["errorCode"]
                .as_str()
                .map(string)
                .unwrap_or(SqlValue::Null),
        );
        values.push(string(entry["revisionRaw"].as_str().unwrap_or("null")));
        values.push(string(arguments["now"].as_str().unwrap_or("")));
        values.push(
            entry["coverageRaw"]
                .as_str()
                .map(string)
                .unwrap_or(SqlValue::Null),
        );
        values.push(string(entry["sourceType"].as_str().unwrap_or("")));
        execute(
            connection,
            "INSERT INTO source_coverage (scope_key,source_id,relative_path,status,lexical,semantic,parser,error_code,source_revision,updated_at,document_coverage,source_type) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(scope_key,source_id) DO UPDATE SET relative_path=excluded.relative_path,status=excluded.status,lexical=excluded.lexical,semantic=excluded.semantic,parser=excluded.parser,error_code=excluded.error_code,source_revision=excluded.source_revision,updated_at=excluded.updated_at,document_coverage=excluded.document_coverage,source_type=CASE WHEN excluded.source_type<>'' THEN excluded.source_type ELSE source_coverage.source_type END",
            values,
        )?;
    }
    Ok(json!({"recorded": entries.len()}))
}

fn reconcile(connection: &Connection, arguments: &Value, scopes: &[Value]) -> ContractResult {
    let types = arguments["sourceTypes"]
        .as_array()
        .filter(|types| !types.is_empty() && types.iter().all(valid_source_type))
        .ok_or_else(invalid)?;
    let ids = arguments["sourceIds"]
        .as_array()
        .filter(|ids| ids.len() <= 2_000_000)
        .ok_or_else(invalid)?;
    let mut keep = HashSet::new();
    for id in ids {
        retrieval::apply("storage.retrieval.sourceId", &json!({"value": id}))?;
        keep.insert(id.as_str().unwrap());
    }
    let mut params = parameters(scopes);
    params.extend(parameters(types));
    let candidates = rows(
        connection,
        &format!(
            "SELECT scope_key,source_id FROM source_coverage WHERE scope_key IN ({}) AND source_type IN ({})",
            placeholders(scopes),
            placeholders(types)
        ),
        params,
    )?;
    let mut removed = 0;
    for row in candidates {
        cancelled()?;
        if !keep.contains(row["source_id"].as_str().unwrap_or("")) {
            removed += execute(
                connection,
                "DELETE FROM source_coverage WHERE scope_key=? AND source_id=?",
                vec![
                    string(row["scope_key"].as_str().unwrap()),
                    string(row["source_id"].as_str().unwrap()),
                ],
            )?;
        }
    }
    Ok(json!({"removed": removed}))
}

fn query(connection: &Connection, arguments: &Value, scopes: &[Value]) -> ContractResult {
    let limit = match arguments.get("limit") {
        None => 100,
        Some(value) => value.as_u64().ok_or_else(invalid)?,
    };
    if !(1..=1000).contains(&limit) {
        return Err(invalid());
    }
    let places = placeholders(scopes);
    let totals = rows(connection, &format!("SELECT COUNT(*) AS discovered,COUNT(DISTINCT scope_key || ':' || relative_path) AS files,SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END) AS failed,SUM(CASE WHEN status='skipped' THEN 1 ELSE 0 END) AS skipped FROM source_coverage WHERE scope_key IN ({places})"), parameters(scopes))?.remove(0);
    let ready = rows(connection, &format!("SELECT SUM(CASE WHEN v.lexical='ready' AND s.source_id IS NOT NULL AND v.source_revision=s.source_revision THEN 1 ELSE 0 END) AS lexical,SUM(CASE WHEN v.semantic='ready' AND s.source_id IS NOT NULL AND v.source_revision=s.source_revision AND EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id) AND NOT EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id AND c.vector IS NULL) THEN 1 ELSE 0 END) AS semantic FROM source_coverage v LEFT JOIN sources s ON s.source_id=v.source_id AND s.scope_key=v.scope_key WHERE v.scope_key IN ({places})"), parameters(scopes))?.remove(0);
    let source_id = arguments["sourceId"]
        .as_str()
        .filter(|value| !value.is_empty());
    let mut params = parameters(scopes);
    if let Some(id) = source_id {
        params.push(string(id));
    }
    params.push(SqlValue::Integer(limit as i64 + 1));
    let mut selected = rows(
        connection,
        &format!(
            "SELECT v.*,s.source_id AS published_source_id,s.source_revision AS published_revision,(EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id) AND NOT EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id AND c.vector IS NULL)) AS complete_vectors FROM source_coverage v LEFT JOIN sources s ON s.source_id=v.source_id AND s.scope_key=v.scope_key WHERE v.scope_key IN ({places}) {} ORDER BY v.relative_path,v.source_id LIMIT ?",
            if source_id.is_some() {
                "AND v.source_id=?"
            } else {
                ""
            }
        ),
        params,
    )?;
    let has_more = selected.len() > limit as usize;
    selected.truncate(limit as usize);
    let items = selected.iter().map(|row| {
        let current = !row["published_source_id"].is_null() && row["source_revision"] == row["published_revision"];
        let lexical = if row["lexical"] == "ready" && !current { json!("unverified") } else { row["lexical"].clone() };
        let semantic = if row["semantic"] == "ready" && (!current || row["complete_vectors"] != 1) { json!("unverified") } else { row["semantic"].clone() };
        let status = if row["status"] == "ready" && (lexical == "unverified" || semantic == "unverified") { json!("unverified") } else { row["status"].clone() };
        let mut item = json!({"sourceId": row["source_id"], "scopeKey": row["scope_key"], "relativePath": row["relative_path"], "status": status, "lexical": lexical, "semantic": semantic, "parser": row["parser"], "revisionRaw": row["source_revision"], "coverageRaw": row["document_coverage"], "updatedAt": row["updated_at"]});
        if row["error_code"].as_str().is_some_and(|value| !value.is_empty()) { item["errorCode"] = row["error_code"].clone(); }
        item
    }).collect::<Vec<_>>();
    let mut counts = json!({});
    for field in ["discovered", "files", "failed", "skipped"] {
        counts[field] = totals[field].as_i64().unwrap_or(0).into();
    }
    for field in ["lexical", "semantic"] {
        counts[field] = ready[field].as_i64().unwrap_or(0).into();
    }
    Ok(json!({"counts": counts, "hasMore": has_more, "complete": false, "items": items}))
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    let timeout_ms = arguments["timeoutMs"].as_u64().unwrap_or(120_000);
    let connection = if operation == "storage.coverage.query" {
        super::sqlite_connections::acquire(root, path, timeout_ms)?
    } else {
        super::sqlite_connections::acquire_write(root, path, timeout_ms)?
    };
    connection
        .execute_batch(if operation == "storage.coverage.query" {
            "BEGIN"
        } else {
            "BEGIN IMMEDIATE"
        })
        .map_err(failure)?;
    let scopes = arguments["scopes"].as_array().cloned().unwrap_or_default();
    let result = match operation {
        "storage.coverage.initialize" => initialize(&connection).map(|_| Value::Null),
        "storage.coverage.record" => record(&connection, arguments, &scopes),
        "storage.coverage.reconcile" => reconcile(&connection, arguments, &scopes),
        "storage.coverage.query" => query(&connection, arguments, &scopes),
        _ => Err(invalid()),
    };
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
