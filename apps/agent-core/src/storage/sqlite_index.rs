use super::{ContractError, ContractResult, sqlite_store::*};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{
    Connection, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;

fn domain_failure(message: &str, code: &'static str) -> ContractError {
    ContractError::new(message, code, 409)
}
fn corpus_source(value: &Value) -> bool {
    matches!(value.as_str(), Some("knowledge" | "work-file"))
}

fn decode_vector(value: &Value) -> Result<SqlValue, ContractError> {
    if let Some(values) = value.as_array() {
        if values.is_empty() || values.len() > 4096 {
            return Err(failure("invalid float32 vector dimensions"));
        }
        let values = values
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .map(|value| value as f32)
                    .ok_or_else(|| failure("invalid vector value"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.iter().any(|value| !value.is_finite()) || values.iter().all(|value| *value == 0.0)
        {
            return Err(failure("invalid finite nonzero vector"));
        }
        return Ok(SqlValue::Blob(
            values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        ));
    }
    let Some(encoded) = value.as_str() else {
        return Ok(SqlValue::Null);
    };
    let bytes = STANDARD.decode(encoded).map_err(failure)?;
    if bytes.is_empty() || bytes.len() % 4 != 0 || bytes.len() > 4096 * 4 {
        return Err(ContractError::new(
            "Invalid float32 embedding vector. / float32 嵌入向量无效。",
            "INVALID_RETRIEVAL_INPUT",
            400,
        ));
    }
    let values: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    if values.iter().any(|value| !value.is_finite()) || values.iter().all(|value| *value == 0.0) {
        return Err(ContractError::new(
            "Invalid float32 embedding vector. / float32 嵌入向量无效。",
            "INVALID_RETRIEVAL_INPUT",
            400,
        ));
    }
    Ok(SqlValue::Blob(bytes))
}

struct StoredVectorRow {
    metadata: Value,
    vector: SqlValue,
}
impl std::ops::Deref for StoredVectorRow {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.metadata
    }
}

/// Retained vectors never leave Rust: keep their owned BLOB alongside metadata, without a base64 round trip.
/// 保留向量始终留在 Rust，元信息旁持有原生 BLOB，不进行 base64 编解码往返。
fn stored_vectors(
    connection: &Connection,
    sql: &str,
    source_id: &str,
) -> Result<Vec<StoredVectorRow>, ContractError> {
    connection
        .prepare_cached(sql)
        .map_err(failure)?
        .query_map(params_from_iter([source_id]), |row| {
            let mut metadata = serde_json::Map::new();
            let mut vector = SqlValue::Null;
            for index in 0..row.as_ref().column_count() {
                let name = row.as_ref().column_name(index)?;
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
                    ValueRef::Blob(value) if name == "vector" => {
                        vector = SqlValue::Blob(value.to_vec());
                        Value::Bool(true)
                    }
                    ValueRef::Blob(_) => Value::Null,
                };
                metadata.insert(name.to_owned(), value);
            }
            Ok(StoredVectorRow {
                metadata: Value::Object(metadata),
                vector,
            })
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)
}

fn validate_version(
    current: &Value,
    source: &Value,
    revision_raw: &Value,
) -> Result<(), ContractError> {
    if current.is_null() {
        return Ok(());
    }
    if current["scope_key"] != source["scopeKey"] {
        return Err(domain_failure(
            "Source identity belongs to another scope. / 资料身份已属于其他范围。",
            "RETRIEVAL_SCOPE_MISMATCH",
        ));
    }
    let previous_raw = current["source_revision"].as_str().unwrap_or("null");
    let previous_revision: Value =
        serde_json::from_str(&super::raw_json::scalar_projection(previous_raw)).map_err(failure)?;
    if (source["sourceRevision"].is_number()
        && previous_revision.is_number()
        && source["sourceRevision"].as_f64() < previous_revision.as_f64())
        || source["bindingRevision"].as_u64().unwrap_or(0)
            < current["binding_revision"].as_u64().unwrap_or(0)
    {
        return Err(domain_failure(
            "An outdated source update was rejected. / 已拒绝过期资料更新。",
            "STALE_RETRIEVAL_SOURCE",
        ));
    }
    let same_revision = if previous_revision.is_string() {
        revision_raw.as_str() == Some(previous_raw)
    } else {
        source["sourceRevision"] == previous_revision
    };
    if same_revision && source["contentHash"] != current["content_hash"] {
        return Err(domain_failure(
            "Source changed without a new revision. / 资料内容已改变但版本未更新。",
            "RETRIEVAL_SOURCE_CHANGED",
        ));
    }
    Ok(())
}

fn publish_relations(
    connection: &Connection,
    source: &Value,
    relations: &Value,
) -> Result<Value, ContractError> {
    sql_execute(
        connection,
        "DELETE FROM source_relations WHERE source_id=?",
        vec![sql_text(text(source, "sourceId"))],
    )?;
    for edge in relations["edges"].as_array().into_iter().flatten() {
        cancelled()?;
        sql_execute(
            connection,
            "INSERT INTO source_relations VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            vec![
                sql_text(text(edge, "edgeId")),
                sql_text(text(source, "sourceId")),
                sql_text(text(source, "derivationSignature")),
                sql_text(text(edge, "kind")),
                sql_text(text(edge, "fromSymbol")),
                sql_text(text(edge, "toSymbol")),
                sql_text(text(edge, "chunkId")),
                sql_text(text(edge, "chunkHash")),
                number(&edge["startOffset"]),
                number(&edge["endOffset"]),
                sql_text(text(edge, "certainty")),
            ],
        )?;
    }
    Ok(
        json!({"edges": relations["edges"].as_array().map_or(0, Vec::len), "truncated": relations["truncated"] == true}),
    )
}

/// Publish sources, chunks, alternate spaces, relations and all generation counters in one transaction.
/// 来源、分块、多向量空间、关系及全部代次计数在同一事务中发布。
fn publish(connection: &Connection, arguments: &Value) -> ContractResult {
    let sources = arguments["sources"]
        .as_array()
        .filter(|sources| sources.len() <= 100)
        .ok_or_else(|| failure("invalid bounded batch"))?;
    let source_scopes: Vec<Value> = sources
        .iter()
        .map(|prepared| prepared["source"]["scopeKey"].clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let before_snapshot = super::sqlite_channels::snapshots(connection, Some(&source_scopes))?;
    let mut published = Vec::new();
    let mut changed_scopes = Vec::new();
    let mut seen_scopes = HashSet::new();
    let timestamp = arguments["createdAt"].as_str().unwrap_or("");
    for prepared in sources {
        cancelled()?;
        let source = &prepared["source"];
        let chunks = prepared["chunks"]
            .as_array()
            .ok_or_else(|| failure("missing chunks"))?;
        let source_id = text(source, "sourceId");
        let current = rows(
            connection,
            "SELECT * FROM sources WHERE source_id=?",
            vec![sql_text(&source_id)],
        )?
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
        validate_version(&current, source, &prepared["sourceRevisionJson"])?;
        let previous_chunks = if current.is_null() {
            Vec::new()
        } else {
            stored_vectors(
                connection,
                "SELECT * FROM chunks WHERE source_id=? ORDER BY chunk_index",
                &source_id,
            )?
        };
        let same_topology = previous_chunks.len() == chunks.len()
            && previous_chunks.iter().zip(chunks).all(|(old, chunk)| {
                old["chunk_id"] == chunk["chunkId"]
                    && old["chunk_hash"] == chunk["chunkHash"]
                    && old["start_offset"] == chunk["startOffset"]
                    && old["end_offset"] == chunk["endOffset"]
            });
        let retains_spaces = current["content_hash"] == source["contentHash"]
            && current["embedding_input_signature"] == source["embeddingInputSignature"]
            && same_topology
            && previous_chunks
                .iter()
                .zip(chunks)
                .all(|(old, chunk)| old["embedding_projection"] == chunk["projectionJson"]);
        let mut retained_spaces: HashMap<String, Vec<StoredVectorRow>> = HashMap::new();
        if retains_spaces {
            for vector in stored_vectors(
                connection,
                "SELECT v.* FROM chunk_vector_spaces v JOIN chunks c ON c.chunk_id=v.chunk_id WHERE c.source_id=? ORDER BY v.updated_at DESC,v.embedding_space_id",
                &source_id,
            )? {
                let spaces = retained_spaces
                    .entry(text(&vector, "chunk_id"))
                    .or_default();
                if spaces.len() < 2 {
                    spaces.push(vector);
                }
            }
        }
        let has_new_vectors = prepared["vectors"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|vector| !vector.is_null());
        let can_retain_vectors = !has_new_vectors
            && current["embedding_input_signature"] == source["embeddingInputSignature"]
            && same_topology
            && previous_chunks.iter().all(|row| {
                row["vector"].is_null()
                    || [
                        ("embeddingProfileId", "embedding_profile_id"),
                        ("embeddingModelVersion", "embedding_model_version"),
                        ("embeddingSpaceId", "embedding_space_id"),
                    ]
                    .iter()
                    .all(|(new, old)| source.get(new).is_none() || source[new] == row[old])
            });
        let existing_vectors = previous_chunks.iter().any(|row| !row["vector"].is_null());
        if !current.is_null()
            && current["derivation_signature"] == source["derivationSignature"]
            && if has_new_vectors {
                current["embedding_signature"] == prepared["embeddingSignature"]
            } else {
                !existing_vectors || can_retain_vectors
            }
        {
            if rows(
                connection,
                "SELECT 1 FROM source_relations WHERE source_id=? LIMIT 1",
                vec![sql_text(&source_id)],
            )?
            .is_empty()
            {
                publish_relations(connection, source, &prepared["relations"])?;
            }
            published.push(json!({"sourceId": source_id, "generation": current["active_generation"], "unchanged": true, "sourceRef": prepared["sourceRef"]}));
            continue;
        }
        let lexical_changed = current["derivation_signature"] != source["derivationSignature"];
        let corpus_changed = (corpus_source(&source["sourceType"])
            || corpus_source(&current["source_type"]))
            && lexical_changed;
        let scope = text(source, "scopeKey");
        let next = next_generation(connection, &scope, corpus_changed, lexical_changed)?;
        if seen_scopes.insert(scope.clone()) {
            changed_scopes.push(scope);
        }
        let retained_signature = if !has_new_vectors && can_retain_vectors {
            text(&current, "embedding_signature")
        } else {
            String::new()
        };
        let signature = prepared["embeddingSignature"]
            .as_str()
            .unwrap_or(&retained_signature);
        sql_execute(
            connection,
            "INSERT INTO sources(source_id,scope_key,source_type,title,locator,text,content_hash,source_revision,binding_revision,active_generation,updated_at,embedding_signature,derivation_signature,embedding_input_signature,parser_version,embedding_input_version,chunker_version,tokenizer_version,structure_json,relative_path) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(source_id) DO UPDATE SET scope_key=excluded.scope_key,source_type=excluded.source_type,title=excluded.title,locator=excluded.locator,text=excluded.text,content_hash=excluded.content_hash,source_revision=excluded.source_revision,binding_revision=excluded.binding_revision,active_generation=excluded.active_generation,updated_at=excluded.updated_at,embedding_signature=excluded.embedding_signature,derivation_signature=excluded.derivation_signature,embedding_input_signature=excluded.embedding_input_signature,parser_version=excluded.parser_version,embedding_input_version=excluded.embedding_input_version,chunker_version=excluded.chunker_version,tokenizer_version=excluded.tokenizer_version,structure_json=excluded.structure_json,relative_path=excluded.relative_path",
            vec![
                sql_text(&source_id),
                sql_text(text(source, "scopeKey")),
                sql_text(text(source, "sourceType")),
                sql_text(text(source, "title")),
                sql_text(text(prepared, "locatorJson")),
                sql_text(text(source, "text")),
                sql_text(text(source, "contentHash")),
                sql_text(text(prepared, "sourceRevisionJson")),
                SqlValue::Integer(source["bindingRevision"].as_i64().unwrap_or(0)),
                SqlValue::Integer(next),
                sql_text(timestamp),
                sql_text(signature),
                sql_text(text(source, "derivationSignature")),
                sql_text(text(source, "embeddingInputSignature")),
                sql_text(text(source, "parserVersion")),
                sql_text(text(source, "embeddingInputVersion")),
                sql_text(text(source, "chunkerVersion")),
                sql_text(text(source, "tokenizerVersion")),
                sql_text(text(prepared, "structureJson")),
                sql_text(text(prepared, "relativePath")),
            ],
        )?;
        sql_execute(
            connection,
            "DELETE FROM chunks WHERE source_id=?",
            vec![sql_text(&source_id)],
        )?;
        for (index, chunk) in chunks.iter().enumerate() {
            cancelled()?;
            let vector = &prepared["vectors"][index];
            let retained_vector = if can_retain_vectors {
                previous_chunks.get(index)
            } else {
                None
            };
            let retained = retained_vector
                .map(|row| &row.metadata)
                .unwrap_or(&Value::Null);
            let has_vector = !vector.is_null();
            let blob = if has_vector {
                decode_vector(vector)?
            } else {
                retained_vector
                    .map(|row| row.vector.clone())
                    .unwrap_or(SqlValue::Null)
            };
            let vector_profile = if has_vector {
                &source["embeddingProfileId"]
            } else {
                &retained["embedding_profile_id"]
            };
            let vector_dimensions = if has_vector {
                &prepared["dimensions"]
            } else {
                &retained["dimensions"]
            };
            let vector_model = if has_vector {
                text(source, "embeddingModelVersion")
            } else {
                text(retained, "embedding_model_version")
            };
            let vector_space = if has_vector {
                text(source, "embeddingSpaceId")
            } else {
                text(retained, "embedding_space_id")
            };
            let structure = &chunk["structure"];
            sql_execute(
                connection,
                "INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,embedding_profile_id,dimensions,vector,chunker_version,tokenizer_version,embedding_model_version,embedding_space_id,structure_json,structure_domain,symbol_name,qualified_name,parse_status,unit_start_offset,unit_end_offset,embedding_projection) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                vec![
                    sql_text(text(chunk, "chunkId")),
                    sql_text(&source_id),
                    number(&chunk["chunkIndex"]),
                    sql_text(text(chunk, "text")),
                    sql_text(text(chunk, "lexicalText")),
                    sql_text(text(chunk, "chunkHash")),
                    number(&chunk["startOffset"]),
                    number(&chunk["endOffset"]),
                    number(&chunk["startLine"]),
                    number(&chunk["endLine"]),
                    nullable_text(vector_profile),
                    number(vector_dimensions),
                    blob.clone(),
                    sql_text(text(chunk, "chunkerVersion")),
                    sql_text(text(chunk, "tokenizerVersion")),
                    sql_text(&vector_model),
                    sql_text(&vector_space),
                    sql_text(text(chunk, "structureJson")),
                    sql_text(text(structure, "domain")),
                    sql_text(text(structure, "symbolName")),
                    sql_text(text(structure, "qualifiedName")),
                    sql_text(text(structure, "parseStatus")),
                    number(&structure["unitStartOffset"]),
                    number(&structure["unitEndOffset"]),
                    sql_text(text(chunk, "projectionJson")),
                ],
            )?;
            let chunk_id = text(chunk, "chunkId");
            let mut spaces = retained_spaces.remove(&chunk_id).unwrap_or_default();
            let has_active_space = !matches!(blob, SqlValue::Null)
                && vector_profile
                    .as_str()
                    .is_some_and(|value| !value.is_empty());
            if has_active_space {
                // The inserted row already owns these validated values; avoid reading and base64-encoding it again.
                // 刚插入的行已拥有这些校验后的字段；不再逐块回查并重新编码向量。
                spaces
                    .retain(|vector| vector["embedding_space_id"].as_str() != Some(&vector_space));
                sql_execute(
                    connection,
                    "INSERT OR REPLACE INTO chunk_vector_spaces VALUES (?,?,?,?,?,?,?)",
                    vec![
                        sql_text(&chunk_id),
                        nullable_text(vector_profile),
                        number(vector_dimensions),
                        blob,
                        sql_text(&vector_model),
                        sql_text(&vector_space),
                        sql_text(timestamp),
                    ],
                )?;
            }
            for vector in spaces
                .into_iter()
                .take(if has_active_space { 1 } else { 2 })
            {
                sql_execute(
                    connection,
                    "INSERT OR REPLACE INTO chunk_vector_spaces VALUES (?,?,?,?,?,?,?)",
                    vec![
                        sql_text(&chunk_id),
                        sql_text(text(&vector, "embedding_profile_id")),
                        number(&vector["dimensions"]),
                        vector.vector.clone(),
                        sql_text(text(&vector, "embedding_model_version")),
                        sql_text(text(&vector, "embedding_space_id")),
                        sql_text(text(&vector, "updated_at")),
                    ],
                )?;
            }
        }
        let relations = publish_relations(connection, source, &prepared["relations"])?;
        published.push(json!({"sourceId": source_id, "generation": next, "chunks": chunks.len(), "relations": relations, "sourceRef": prepared["sourceRef"]}));
    }
    sql_execute(
        connection,
        "UPDATE index_jobs SET state=?,completed_count=? WHERE job_id=?",
        vec![
            sql_text("completed"),
            SqlValue::Integer(sources.len() as i64),
            sql_text(text(arguments, "jobId")),
        ],
    )?;
    let changed: Vec<Value> = changed_scopes.iter().map(|scope| json!(scope)).collect();
    let committed = super::sqlite_channels::snapshots(connection, Some(&changed))?;
    Ok(
        json!({"jobId": arguments["jobId"], "generation": committed["generation"], "indexSnapshotId": committed["indexSnapshotId"],
        "sources": published, "changedScopes": changed_scopes, "beforeScopeSnapshots":before_snapshot["scopeSnapshots"],
        "scopeSnapshots":committed["scopeSnapshots"]}),
    )
}

fn remove(connection: &Connection, arguments: &Value, whole_scope: bool) -> ContractResult {
    let scope = text(arguments, "scopeKey");
    let authorized = vec![json!(scope)];
    let before_snapshot = super::sqlite_channels::snapshots(connection, Some(&authorized))?;
    let removed = if whole_scope {
        sql_execute(
            connection,
            "DELETE FROM sources WHERE scope_key=?",
            vec![sql_text(&scope)],
        )?
    } else {
        sql_execute(
            connection,
            "DELETE FROM sources WHERE source_id=? AND scope_key=?",
            vec![sql_text(text(arguments, "sourceId")), sql_text(&scope)],
        )?
    };
    if whole_scope {
        sql_execute(
            connection,
            "DELETE FROM source_coverage WHERE scope_key=?",
            vec![sql_text(&scope)],
        )?;
    } else {
        sql_execute(
            connection,
            "DELETE FROM source_coverage WHERE source_id=? AND scope_key=?",
            vec![sql_text(text(arguments, "sourceId")), sql_text(&scope)],
        )?;
    }
    let next = next_generation(connection, &scope, arguments["corpusChanged"] == true, true)?;
    let committed = super::sqlite_channels::snapshots(connection, Some(&authorized))?;
    Ok(if whole_scope {
        json!({"removed": removed, "generation": next, "indexSnapshotId": committed["indexSnapshotId"], "scopeKey": scope,
            "beforeScopeSnapshots":before_snapshot["scopeSnapshots"],"scopeSnapshots":committed["scopeSnapshots"]})
    } else {
        json!({"removed": removed > 0, "generation": next, "indexSnapshotId": committed["indexSnapshotId"],
            "beforeScopeSnapshots":before_snapshot["scopeSnapshots"],"scopeSnapshots":committed["scopeSnapshots"]})
    })
}

fn retire_chat_batch(connection: &Connection, arguments: &Value) -> ContractResult {
    let scope = text(arguments, "scopeKey");
    let id = scope
        .strip_prefix("chat:")
        .ok_or_else(|| failure("invalid retired chat scope"))?;
    super::identity(&json!(id))?;
    // Small atomic batches make progress even for a large chat; formal catalog revocation already hides the scope.
    // 小批原子清理使大聊天也能持续前进；正式目录已撤销此scope，不会在分批期间重新暴露残留内容。
    let chunks = sql_execute(
        connection,
        "DELETE FROM chunks WHERE id IN (SELECT c.id FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key=? ORDER BY c.id LIMIT 128)",
        vec![sql_text(&scope)],
    )?;
    let sources = sql_execute(
        connection,
        "DELETE FROM sources WHERE source_id IN (SELECT s.source_id FROM sources s WHERE s.scope_key=? AND NOT EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id) ORDER BY s.source_id LIMIT 16)",
        vec![sql_text(&scope)],
    )?;
    let coverage = sql_execute(
        connection,
        "DELETE FROM source_coverage WHERE rowid IN (SELECT rowid FROM source_coverage WHERE scope_key=? LIMIT 128)",
        vec![sql_text(&scope)],
    )?;
    let changed = chunks + sources + coverage > 0;
    if changed {
        next_generation(connection, &scope, false, true)?;
    }
    let pending = rows(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sources WHERE scope_key=?) OR EXISTS(SELECT 1 FROM source_coverage WHERE scope_key=?) AS pending",
        vec![sql_text(&scope), sql_text(&scope)],
    )?[0]["pending"]
        == 1;
    Ok(
        json!({"removed":sources,"removedChunks":chunks,"removedCoverage":coverage,"hasMore":pending,"complete":!pending}),
    )
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    let timeout = if operation == "storage.index.retireChat" {
        100
    } else {
        120_000
    };
    let connection = super::sqlite_connections::acquire_write(root, path, timeout)?;
    if operation == "storage.index.startJob" {
        sql_execute(
            &connection,
            "INSERT INTO index_jobs VALUES (?,?,?,?,?,?)",
            vec![
                sql_text(text(arguments, "jobId")),
                sql_text("running"),
                number(&arguments["sourceCount"]),
                SqlValue::Integer(0),
                sql_text(text(arguments, "createdAt")),
                SqlValue::Null,
            ],
        )?;
        return Ok(Value::Null);
    }
    if operation == "storage.index.failJob" {
        sql_execute(
            &connection,
            "UPDATE index_jobs SET state=?,error_code=? WHERE job_id=?",
            vec![
                sql_text("failed"),
                sql_text(text(arguments, "errorCode")),
                sql_text(text(arguments, "jobId")),
            ],
        )?;
        return Ok(Value::Null);
    }

    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(failure)?;
    let result = match operation {
        "storage.index.publish" => publish(&connection, arguments),
        "storage.index.remove" => remove(&connection, arguments, false),
        "storage.index.invalidate" => remove(&connection, arguments, true),
        "storage.index.retireChat" => retire_chat_batch(&connection, arguments),
        _ => Err(failure("unknown transaction")),
    };
    finish(&connection, result)
}
