use super::{
    ContractError, ContractResult, index_registry, managed_files, raw_json, sqlite_schema,
    sqlite_store::*,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;

fn metadata_columns(
    connection: &Connection,
    table: &str,
) -> Result<HashSet<String>, ContractError> {
    Ok(
        rows(connection, &format!("PRAGMA table_info({table})"), vec![])?
            .iter()
            .filter_map(|row| row["name"].as_str().map(str::to_owned))
            .collect(),
    )
}
fn corpus_source(value: &Value) -> bool {
    matches!(value.as_str(), Some("knowledge" | "work-file"))
}

fn bootstrap(connection: &Connection, root: &Path, arguments: &Value) -> ContractResult {
    connection
        .execute_batch(sqlite_schema::RETRIEVAL_SCHEMA)
        .map_err(failure)?;
    migrate_integrity(connection, 6, &[
        ("retrieval_metadata", ""), ("sources", ""),
        ("chunks", "UNIQUE(source_id,chunk_index)"), ("scope_snapshots", ""),
        ("index_jobs", ""), ("source_relations", ""),
        ("chunk_vector_spaces", ""), ("source_coverage", ""),
    ], &[
        "SELECT EXISTS(SELECT 1 FROM chunks GROUP BY source_id,chunk_index HAVING COUNT(*)>1)",
    ])?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(failure)?;
    let result = (|| {
        let source_columns = metadata_columns(connection, "sources")?;
        for column in [
            "embedding_signature",
            "derivation_signature",
            "embedding_input_signature",
            "parser_version",
            "embedding_input_version",
            "chunker_version",
            "tokenizer_version",
            "structure_json",
            "relative_path",
        ] {
            if !source_columns.contains(column) {
                connection
                    .execute_batch(&format!(
                        "ALTER TABLE sources ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
                    ))
                    .map_err(failure)?;
            }
        }
        let chunk_columns = metadata_columns(connection, "chunks")?;
        for column in [
            "embedding_model_version",
            "embedding_space_id",
            "structure_json",
            "structure_domain",
            "symbol_name",
            "qualified_name",
            "parse_status",
            "embedding_projection",
        ] {
            if !chunk_columns.contains(column) {
                connection
                    .execute_batch(&format!(
                        "ALTER TABLE chunks ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
                    ))
                    .map_err(failure)?;
            }
        }
        for column in ["unit_start_offset", "unit_end_offset"] {
            if !chunk_columns.contains(column) {
                connection
                    .execute_batch(&format!("ALTER TABLE chunks ADD COLUMN {column} INTEGER"))
                    .map_err(failure)?;
            }
        }
        if !source_columns.contains("relative_path") {
            for row in rows(connection, "SELECT source_id,locator FROM sources", vec![])? {
                cancelled()?;
                let locator: Value = serde_json::from_str(&raw_json::scalar_projection(
                    row["locator"].as_str().unwrap_or(""),
                ))
                .map_err(failure)?;
                if let Some(path) = locator["relativePath"].as_str() {
                    sql_execute(
                        connection,
                        "UPDATE sources SET relative_path=? WHERE source_id=?",
                        vec![
                            sql_text(super::retrieval::normalize_path(path)),
                            sql_text(text(&row, "source_id")),
                        ],
                    )?;
                }
            }
        }
        connection.execute_batch("CREATE INDEX IF NOT EXISTS sources_relative_path ON sources(relative_path,scope_key);
            CREATE INDEX IF NOT EXISTS chunks_symbol ON chunks(symbol_name,structure_domain,parse_status);
            CREATE INDEX IF NOT EXISTS chunks_qualified_name ON chunks(qualified_name,structure_domain,parse_status);
            CREATE INDEX IF NOT EXISTS chunks_space ON chunks(embedding_space_id,embedding_profile_id,dimensions);
            PRAGMA user_version=6;").map_err(failure)?;
        sql_execute(
            connection,
            "INSERT OR IGNORE INTO retrieval_metadata(key,value) VALUES ('index_epoch',?)",
            vec![sql_text(text(arguments, "epoch"))],
        )?;
        connection.execute_batch("INSERT OR IGNORE INTO retrieval_metadata(key,value) SELECT 'corpus_generation:'||scope_key,CAST(generation AS TEXT) FROM scope_snapshots;
            INSERT OR IGNORE INTO retrieval_metadata(key,value) SELECT 'lexical_generation:'||scope_key,CAST(generation AS TEXT) FROM scope_snapshots;").map_err(failure)?;
        let registry_path = root.join("Retrieval/source-identities.json");
        let registry = match managed_files::read(root, &registry_path, 32 * 1024 * 1024)? {
            None => json!({"schemaVersion": 1, "revision": 0, "sources": {}}),
            Some(bytes) => serde_json::from_str::<Value>(&raw_json::scalar_projection(
                &String::from_utf8_lossy(&bytes),
            ))
            .map_err(|_| {
                ContractError::new(
                    "资料身份登记损坏，原文件已保留。",
                    "CORRUPT_RETRIEVAL_IDENTITIES",
                    409,
                )
            })?,
        };
        index_registry::apply("storage.registry.validate", &json!({"registry": registry}))?;
        for entry in registry["sources"].as_object().unwrap().values() {
            if entry["tombstone"] != true {
                continue;
            }
            if let Some(source) = rows(
                connection,
                "SELECT scope_key,source_type FROM sources WHERE source_id=?",
                vec![sql_text(text(entry, "sourceId"))],
            )?
            .first()
            {
                sql_execute(
                    connection,
                    "DELETE FROM sources WHERE source_id=?",
                    vec![sql_text(text(entry, "sourceId"))],
                )?;
                next_generation(
                    connection,
                    source["scope_key"].as_str().unwrap_or(""),
                    corpus_source(&source["source_type"]),
                    true,
                )?;
            }
        }
        connection
            .execute_batch(sqlite_schema::RELATIONS_SCHEMA)
            .map_err(failure)?;
        connection
            .execute_batch(sqlite_schema::VECTOR_SPACES_SCHEMA)
            .map_err(failure)?;
        // Coverage cleanup is part of every source removal, including freshly imported databases.
        // 来源撤销始终清理覆盖记录，首次导入的新库也必须在同一初始化事务准备该表。
        super::source_coverage::initialize(connection)?;
        let columns = metadata_columns(connection, "chunks")?;
        // PRAGMA table_info ordinal order determines the old row codec, including column additions.
        // PRAGMA 列序决定旧行编码，新增列也保持原顺序。
        let projection = rows(connection, "PRAGMA table_info(chunks)", vec![])?
            .iter()
            .map(|row| {
                let column = row["name"].as_str().unwrap();
                format!(
                    "{}.{}",
                    if [
                        "embedding_profile_id",
                        "dimensions",
                        "vector",
                        "embedding_model_version",
                        "embedding_space_id"
                    ]
                    .contains(&column)
                    {
                        "v"
                    } else {
                        "c"
                    },
                    column
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let _ = columns;
        connection.execute_batch(&format!("CREATE VIEW IF NOT EXISTS retrieval_vector_rows AS SELECT {projection} FROM chunks c JOIN chunk_vector_spaces v ON v.chunk_id=c.chunk_id")).map_err(failure)?;
        Ok(
            json!({"epoch": connection.query_row("SELECT value FROM retrieval_metadata WHERE key='index_epoch'", [], |row| row.get::<_, String>(0)).map_err(failure)?}),
        )
    })();
    finish(connection, result)
}

fn migration_batch(connection: &Connection, arguments: &Value) -> ContractResult {
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(failure)?;
    let result = (|| {
        let stale = rows(
            connection,
            "SELECT c.id,c.source_id,c.text,s.title,s.locator,s.scope_key,s.source_type,s.derivation_signature FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE c.tokenizer_version='han-bigram-code-v1' ORDER BY c.id LIMIT 256",
            vec![],
        )?;
        let mut scopes = Vec::new();
        let mut seen_scopes = HashSet::new();
        let mut corpus_scopes = HashSet::new();
        for row in &stale {
            cancelled()?;
            let locator: Value = serde_json::from_str(&raw_json::scalar_projection(
                row["locator"].as_str().unwrap_or(""),
            ))
            .map_err(failure)?;
            let combined = format!(
                "{} {} {}",
                text(row, "title"),
                locator["relativePath"].as_str().unwrap_or(""),
                text(row, "text")
            );
            let lexical = crate::retrieval::lexical::apply(
                "retrieval.lexical-text",
                &json!({"text": combined}),
            )
            .map_err(failure)?;
            sql_execute(
                connection,
                "UPDATE chunks SET lexical_text=?,tokenizer_version=? WHERE id=?",
                vec![
                    sql_text(lexical.as_str().unwrap_or("")),
                    sql_text(text(arguments, "tokenizerVersion")),
                    number(&row["id"]),
                ],
            )?;
            let scope = text(row, "scope_key");
            if seen_scopes.insert(scope.clone()) {
                scopes.push(scope.clone());
            }
            if corpus_source(&row["source_type"]) {
                corpus_scopes.insert(scope);
            }
            if row["derivation_signature"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
            {
                sql_execute(
                    connection,
                    "INSERT OR IGNORE INTO retrieval_metadata(key,value) VALUES (?,?)",
                    vec![
                        sql_text(format!("migration_derivation:{}", text(row, "source_id"))),
                        sql_text(text(row, "derivation_signature")),
                    ],
                )?;
            }
        }
        for scope in scopes {
            next_generation(connection, &scope, corpus_scopes.contains(&scope), true)?;
        }
        let pending = rows(
            connection,
            "SELECT substr(key,22) AS source_id,value AS previous_signature FROM retrieval_metadata WHERE key LIKE 'migration_derivation:%' ORDER BY key LIMIT 256",
            vec![],
        )?;
        if stale.is_empty() && pending.is_empty() {
            sql_execute(
                connection,
                "INSERT INTO retrieval_metadata(key,value) VALUES ('tokenizer_version',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                vec![sql_text(text(arguments, "tokenizerVersion"))],
            )?;
        }
        Ok(json!({"changedChunks": stale.len(), "pending": pending}))
    })();
    finish(connection, result)
}

/// Bootstrap, additive upgrades and revocation cleanup run in native SQLite; legacy byte hashes are acknowledged explicitly.
/// 启动、增量升级及撤销清理由原生 SQLite 执行，历史字节哈希通过显式回执确认。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    managed_files::inspect(root, &root.join("Retrieval"), true, false, u64::MAX)?;
    let connection = open(root, path, operation == "storage.index.bootstrap")?;
    match operation {
        "storage.index.bootstrap" => bootstrap(&connection, root, arguments),
        "storage.index.migrationBatch" => migration_batch(&connection, arguments),
        "storage.index.migrationSource" => {
            let source_id = text(arguments, "sourceId");
            Ok(
                json!({"source": rows(&connection, "SELECT * FROM sources WHERE source_id=?", vec![sql_text(&source_id)])?.into_iter().next(), "chunks": rows(&connection, "SELECT chunk_id,chunk_index,chunk_hash,text,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version,structure_json,embedding_projection FROM chunks WHERE source_id=? ORDER BY chunk_index", vec![sql_text(source_id)])?}),
            )
        }
        "storage.index.completeMigration" => {
            connection
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(failure)?;
            let result = (|| {
                let source_id = text(arguments, "sourceId");
                let previous = text(arguments, "previousSignature");
                if arguments["derivationSignature"].is_string() {
                    let changed = sql_execute(
                        &connection,
                        "UPDATE sources SET derivation_signature=?,tokenizer_version=? WHERE source_id=? AND derivation_signature=?",
                        vec![
                            sql_text(text(arguments, "derivationSignature")),
                            sql_text(text(arguments, "tokenizerVersion")),
                            sql_text(&source_id),
                            sql_text(previous),
                        ],
                    )?;
                    if changed != 1 {
                        return Err(ContractError::new(
                            "词法迁移的来源版本已变化。",
                            "STALE_RETRIEVAL_SOURCE",
                            409,
                        ));
                    }
                }
                sql_execute(
                    &connection,
                    "DELETE FROM retrieval_metadata WHERE key=?",
                    vec![sql_text(format!("migration_derivation:{source_id}"))],
                )?;
                Ok(Value::Null)
            })();
            finish(&connection, result)
        }
        _ => Err(failure("unknown index bootstrap operation")),
    }
}
