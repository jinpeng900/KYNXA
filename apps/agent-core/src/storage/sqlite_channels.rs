use super::{ContractError, ContractResult, sqlite_lexical::DOMAIN_SQL, sqlite_store::*};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};

pub fn chunk_columns() -> String {
    // Candidate rows retain their public metadata shape; vector bytes have a separate owned read operation.
    // 候选行保留公开元数据形状；向量字节通过独立、具有所有权的读取操作获取，不随候选重复编码。
    format!(
        "c.id,c.chunk_id,c.source_id,c.chunk_index,c.text,c.lexical_text,c.chunk_hash,c.start_offset,c.end_offset,c.start_line,c.end_line,
        c.embedding_profile_id,c.dimensions,NULL AS vector,c.chunker_version,c.tokenizer_version,c.embedding_model_version,c.embedding_space_id,
        c.structure_json,c.structure_domain,c.symbol_name,c.qualified_name,c.parse_status,c.unit_start_offset,c.unit_end_offset,c.embedding_projection,
        s.scope_key,s.source_type,s.title,s.locator,s.content_hash,s.source_revision,s.binding_revision,s.active_generation,s.derivation_signature,{DOMAIN_SQL} AS actual_domain"
    )
}

pub(crate) fn candidate_columns() -> String {
    format!("c.id,c.chunk_id,{DOMAIN_SQL} AS actual_domain")
}

fn placeholders(values: &[Value]) -> String {
    vec!["?"; values.len()].join(",")
}
fn parameters(values: &[Value]) -> Vec<SqlValue> {
    values
        .iter()
        .map(|value| sql_text(value.as_str().unwrap_or("")))
        .collect()
}
fn escape_like(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if matches!(character, '\\' | '%' | '_') {
                vec!['\\', character]
            } else {
                vec![character]
            }
        })
        .collect()
}
fn path_clause(column: &str, path: &str) -> (String, Vec<SqlValue>) {
    let escaped = escape_like(path);
    if path.ends_with('/') {
        (
            format!("{column} LIKE ? ESCAPE '\\'"),
            vec![sql_text(format!("{escaped}%"))],
        )
    } else {
        (
            format!("({column}=? OR {column} LIKE ? ESCAPE '\\')"),
            vec![sql_text(path), sql_text(format!("%/{escaped}"))],
        )
    }
}

/// Structure and literal channels share authorized predicates and a single read snapshot.
/// 结构与字面通道共用授权谓词，并在同一读取快照完成候选选择。
pub fn candidates(connection: &Connection, arguments: &Value, scopes: &[Value]) -> ContractResult {
    let columns = if arguments["metadataOnly"] == true {
        candidate_columns()
    } else {
        chunk_columns()
    };
    let limit = arguments["limit"].as_u64().unwrap_or(40).clamp(1, 160);
    let domain = arguments["domain"]
        .as_str()
        .filter(|domain| !domain.is_empty());
    let filter = domain.map_or(String::new(), |_| {
        format!(" AND ({DOMAIN_SQL}=? OR {DOMAIN_SQL}='')")
    });
    let rank = domain.map_or("CASE WHEN 1 THEN 0 END".to_owned(), |_| {
        format!("CASE WHEN {DOMAIN_SQL}=? THEN 0 ELSE 1 END")
    });
    let mut authorized = parameters(scopes);
    if let Some(domain) = domain {
        authorized.push(sql_text(domain));
    }
    let rank_parameters: Vec<_> = domain.into_iter().map(sql_text).collect();
    let mut literal = Vec::new();
    let query = arguments["literalQuery"].as_str().unwrap_or("");
    if !query.is_empty() {
        let mut values = authorized.clone();
        values.push(sql_text(query));
        values.extend(rank_parameters.clone());
        literal = rows(
            connection,
            &format!(
                "SELECT {columns} FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key IN ({}){filter} AND instr(lower(c.text),lower(?))>0 ORDER BY {rank},c.id LIMIT {limit}",
                placeholders(scopes)
            ),
            values,
        )?;
    }
    let mut symbols = Vec::new();
    let mut paths = Vec::new();
    let path = arguments["pathFilter"]
        .as_str()
        .filter(|path| !path.is_empty());
    if let Some(symbol) = arguments["symbol"]
        .as_str()
        .filter(|symbol| !symbol.is_empty())
    {
        let suffix = if symbol.contains('.') {
            " OR (c.qualified_name LIKE ? ESCAPE '\\' AND substr(c.qualified_name,-length(?))=?)"
        } else {
            ""
        };
        let mut values = authorized.clone();
        values.extend([sql_text(symbol), sql_text(symbol)]);
        if !suffix.is_empty() {
            values.extend([
                sql_text(format!("%.{}", escape_like(symbol))),
                sql_text(symbol),
                sql_text(symbol),
            ]);
        }
        let target_rank = if let Some(path) = path {
            let (clause, parameters) = path_clause("ranked.relative_path", path);
            values.extend(parameters);
            format!("CASE WHEN {clause} THEN 0 ELSE 1 END")
        } else {
            "CASE WHEN 1 THEN 1 END".to_owned()
        };
        values.extend(rank_parameters.clone());
        symbols = rows(connection, &format!("WITH ranked AS MATERIALIZED (
            SELECT c.id,c.chunk_id,s.relative_path,
                ROW_NUMBER() OVER (PARTITION BY c.source_id,c.symbol_name,c.unit_start_offset,c.unit_end_offset ORDER BY c.chunk_index) AS unit_rank
            FROM chunks c JOIN sources s ON s.source_id=c.source_id
            WHERE s.scope_key IN ({}){filter} AND c.structure_domain='code' AND c.parse_status IN ('parsed','partial')
                AND (c.symbol_name=? OR c.qualified_name=?{suffix})
        ), candidates AS MATERIALIZED (
            SELECT ranked.id,{target_rank} AS target_rank FROM ranked WHERE ranked.unit_rank=1
            ORDER BY target_rank,ranked.chunk_id LIMIT {limit}
        ) SELECT {columns},candidates.target_rank FROM candidates
            JOIN chunks c ON c.id=candidates.id JOIN sources s ON s.source_id=c.source_id
            ORDER BY candidates.target_rank,{rank},c.chunk_id", placeholders(scopes)), values)?;
        for row in &mut symbols {
            if row["target_rank"] == 0 {
                row["exact_target_match"] = json!(true);
            }
        }
    }
    if let Some(path) = path {
        let (clause, path_parameters) = path_clause("s.relative_path", path);
        let mut values = authorized;
        values.extend(path_parameters);
        values.extend(rank_parameters);
        paths = rows(connection, &format!("WITH ranked AS MATERIALIZED (
            SELECT c.id,ROW_NUMBER() OVER (PARTITION BY c.source_id ORDER BY c.chunk_index) AS source_rank
            FROM chunks c JOIN sources s ON s.source_id=c.source_id
            WHERE s.scope_key IN ({}){filter} AND {clause}
        ) SELECT {columns} FROM ranked JOIN chunks c ON c.id=ranked.id JOIN sources s ON s.source_id=c.source_id
            WHERE ranked.source_rank=1 ORDER BY {rank},s.relative_path,c.chunk_id LIMIT {limit}", placeholders(scopes)), values)?;
    }
    Ok(json!({"literal":literal,"symbols":symbols,"paths":paths}))
}

pub fn source_state(connection: &Connection, arguments: &Value) -> ContractResult {
    let ids = arguments["sourceIds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let states = if ids.is_empty() {
        Vec::new()
    } else {
        rows(
            connection,
            &format!(
                "SELECT s.source_id,s.scope_key,s.source_type,s.source_revision,s.content_hash,s.binding_revision,s.title,s.locator,s.parser_version,s.embedding_input_version,s.embedding_input_signature,
                count(c.id) AS chunk_count,
                count(CASE WHEN c.vector IS NOT NULL THEN 1 END) AS vector_chunks,
                min(CASE WHEN c.vector IS NOT NULL THEN c.embedding_profile_id END) AS embedding_profile_id,
                min(CASE WHEN c.vector IS NOT NULL THEN c.embedding_space_id END) AS embedding_space_id,
                min(CASE WHEN c.vector IS NOT NULL THEN c.embedding_model_version END) AS embedding_model_version
                FROM sources s LEFT JOIN chunks c ON c.source_id=s.source_id WHERE s.source_id IN ({}) GROUP BY s.source_id",
                placeholders(&ids)
            ),
            parameters(&ids),
        )?
    };
    let has_corpus = if let Some(scope) = arguments["scopeKey"].as_str() {
        !rows(connection, "SELECT 1 FROM sources WHERE scope_key=? AND source_type IN ('knowledge','work-file') LIMIT 1", vec![sql_text(scope)])?.is_empty()
    } else {
        false
    };
    Ok(json!({"states":states,"hasCorpusRows":has_corpus}))
}

pub fn scope_version(connection: &Connection, scopes: &[Value]) -> ContractResult {
    let scoped = rows(
        connection,
        &format!(
            "SELECT scope_key,generation FROM scope_snapshots WHERE scope_key IN ({})",
            placeholders(scopes)
        ),
        parameters(scopes),
    )?;
    let keys: Vec<_> = scopes
        .iter()
        .map(|scope| {
            json!(format!(
                "corpus_generation:{}",
                scope.as_str().unwrap_or("")
            ))
        })
        .collect();
    let corpus = rows(
        connection,
        &format!(
            "SELECT key,value FROM retrieval_metadata WHERE key IN ({})",
            placeholders(&keys)
        ),
        parameters(&keys),
    )?;
    let epochs = rows(
        connection,
        "SELECT value FROM retrieval_metadata WHERE key='index_epoch'",
        vec![],
    )?;
    Ok(
        json!({"indexEpoch":epochs.first().map(|row| &row["value"]),"scopes":scopes.iter().map(|scope| {
        let generation = scoped.iter().find(|row| row["scope_key"] == *scope).map_or(json!(0), |row| row["generation"].clone());
        let key = format!("corpus_generation:{}",scope.as_str().unwrap_or(""));
        let corpus_generation = corpus.iter().find(|row| row["key"] == key).and_then(|row| row["value"].as_str()).and_then(|value| value.parse::<f64>().ok()).unwrap_or(0.0);
        json!({"scopeKey":scope,"generation":generation,"corpusGeneration":corpus_generation})
    }).collect::<Vec<_>>()}),
    )
}

pub fn snapshots(connection: &Connection, scopes: Option<&[Value]>) -> ContractResult {
    let current = generation(connection)?;
    let snapshot_id = snapshot(connection)?;
    let (sql, parameters) = if let Some(scopes) = scopes {
        (
            format!(
                "SELECT scope_key AS scopeKey,generation FROM scope_snapshots WHERE scope_key IN ({}) ORDER BY scope_key",
                placeholders(scopes)
            ),
            parameters(scopes),
        )
    } else {
        (
            "SELECT scope_key AS scopeKey,generation FROM scope_snapshots ORDER BY scope_key"
                .to_owned(),
            vec![],
        )
    };
    let epoch = snapshot_id
        .rsplit_once(':')
        .map(|(epoch, _)| epoch)
        .unwrap_or("");
    let mut snapshots = rows(connection, &sql, parameters)?;
    for row in &mut snapshots {
        row["snapshotId"] = json!(format!("{epoch}:{}", row["generation"]));
    }
    Ok(json!({"generation":current,"indexSnapshotId":snapshot_id,"scopeSnapshots":snapshots}))
}

pub fn status(connection: &Connection) -> ContractResult {
    let mut result = snapshots(connection, None)?;
    for (field, sql) in [
        ("sources", "SELECT count(*) AS count FROM sources"),
        ("chunks", "SELECT count(*) AS count FROM chunks"),
        (
            "vectorChunks",
            "SELECT count(*) AS count FROM chunks WHERE vector IS NOT NULL",
        ),
    ] {
        result[field] = rows(connection, sql, vec![])?
            .first()
            .map_or(Value::Null, |row| row["count"].clone());
    }
    let jobs = rows(
        connection,
        "SELECT state,count(*) AS count FROM index_jobs GROUP BY state",
        vec![],
    )?;
    result["jobs"] = Value::Object(
        jobs.iter()
            .map(|row| {
                (
                    row["state"].as_str().unwrap_or("").to_owned(),
                    row["count"].clone(),
                )
            })
            .collect(),
    );
    result["schemaVersion"] = json!(6);
    Ok(result)
}

pub fn list_sources(
    connection: &Connection,
    arguments: &Value,
    scopes: &[Value],
) -> ContractResult {
    let mut values = parameters(scopes);
    let mut filter = String::new();
    for (field, column) in [("sourceType", "source_type"), ("sourceId", "source_id")] {
        if let Some(value) = arguments.get(field) {
            filter.push_str(&format!(" AND {column}=?"));
            values.push(nullable_text(value));
        }
    }
    if let Some(after) = arguments["afterSourceId"].as_str() {
        filter.push_str(" AND source_id>?");
        values.push(sql_text(after));
    }
    let selected = rows(connection,&format!("SELECT source_id AS sourceId,scope_key AS scopeKey,source_type AS sourceType,
        title,locator,content_hash AS contentHash,source_revision AS sourceRevision,binding_revision AS bindingRevision,
        derivation_signature AS derivationSignature,embedding_input_signature AS embeddingInputSignature,
        parser_version AS parserVersion,embedding_input_version AS embeddingInputVersion,
        chunker_version AS chunkerVersion,tokenizer_version AS tokenizerVersion,structure_json AS structure,
        active_generation AS generation,
        (SELECT count(*) FROM chunks c WHERE c.source_id=sources.source_id) AS chunkCount,
        (SELECT count(*) FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS vectorChunks,
        (SELECT CASE WHEN count(DISTINCT c.embedding_profile_id)=1 THEN min(c.embedding_profile_id) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS embeddingProfileId,
        (SELECT CASE WHEN count(DISTINCT c.embedding_model_version)=1 THEN min(c.embedding_model_version) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS embeddingModelVersion,
        (SELECT CASE WHEN count(DISTINCT c.embedding_space_id)=1 THEN min(c.embedding_space_id) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS embeddingSpaceId,
        (SELECT CASE WHEN count(DISTINCT c.dimensions)=1 THEN min(c.dimensions) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS vectorDimensions
        FROM sources WHERE scope_key IN ({}){filter} ORDER BY source_id LIMIT 256",placeholders(scopes)),values)?;
    let next = selected.last().map(|row| row["sourceId"].clone());
    Ok(json!({"rows":selected,"nextSourceId":next}))
}

pub fn checkpoint(connection: &Connection) -> ContractResult {
    let report = rows(connection, "PRAGMA wal_checkpoint(TRUNCATE)", vec![])?;
    if report.first().is_some_and(|row| row["busy"] != 0) {
        return Err(ContractError::new(
            "Retrieval index is busy. / 检索索引正被使用，无法安全迁移。",
            "RETRIEVAL_INDEX_BUSY",
            409,
        ));
    }
    Ok(Value::Null)
}
