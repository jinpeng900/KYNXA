use super::{ContractError, ContractResult, sqlite_store::*};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};
use std::path::Path;

pub fn handles(operation: &str) -> bool {
    [
        "storage.index.verifyReference",
        "storage.index.relations",
        "storage.index.vectorSpaceStatus",
        "storage.index.readSource",
        "storage.index.removeCoverage",
        "storage.index.channels",
        "storage.index.sourceState",
        "storage.index.scopeVersion",
        "storage.index.snapshots",
        "storage.index.status",
        "storage.index.listSources",
        "storage.index.checkpoint",
        "storage.index.vectorTable",
        "storage.index.vectorDescriptors",
        "storage.index.vectorPartition",
        "storage.index.vectorRows",
        "storage.index.scopeGeneration",
        "storage.index.verifyVectorMatches",
        "storage.index.exactVectorSearch",
        "storage.index.vectorExtensionInfo",
        "storage.index.closeConnections",
        "storage.index.connectionStats",
        "storage.index.vectorCacheStats",
        "storage.index.vectorCacheInvalidate",
        "storage.index.vectorCacheClose",
    ]
    .contains(&operation)
}

fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_RETRIEVAL_INPUT", 400)
}
fn placeholders(scopes: &[Value]) -> String {
    vec!["?"; scopes.len()].join(",")
}
fn parameters(scopes: &[Value]) -> Vec<SqlValue> {
    scopes
        .iter()
        .map(|scope| sql_text(scope.as_str().unwrap_or("")))
        .collect()
}

fn verify_reference(
    connection: &Connection,
    arguments: &Value,
    scopes: &[Value],
) -> ContractResult {
    let reference = &arguments["reference"];
    if !scopes.contains(&reference["scopeKey"]) {
        return Err(ContractError::new(
            "资料不在授权范围内。",
            "RETRIEVAL_SOURCE_NOT_FOUND",
            404,
        ));
    }
    let mut values = vec![sql_text(text(reference, "sourceId"))];
    values.extend(parameters(scopes));
    let Some(row) = rows(connection, &format!("SELECT source_id,scope_key,content_hash,source_revision,derivation_signature FROM sources WHERE source_id=? AND scope_key IN ({})", placeholders(scopes)), values)?.into_iter().next() else { return Ok(json!({"current": false, "reason": "RETRIEVAL_SOURCE_NOT_FOUND"})); };
    if reference["scopeKey"] != row["scope_key"]
        || reference["contentHash"] != row["content_hash"]
        || arguments["revisionRaw"] != row["source_revision"]
        || reference
            .get("derivationSignature")
            .is_some_and(|signature| *signature != row["derivation_signature"])
    {
        return Ok(json!({"current": false, "reason": "STALE_RETRIEVAL_SOURCE"}));
    }
    if reference["chunkId"]
        .as_str()
        .is_some_and(|id| !id.is_empty())
    {
        let chunk = rows(
            connection,
            "SELECT chunk_hash FROM chunks WHERE chunk_id=? AND source_id=?",
            vec![
                sql_text(text(reference, "chunkId")),
                sql_text(text(reference, "sourceId")),
            ],
        )?
        .into_iter()
        .next();
        if chunk.is_none_or(|chunk| chunk["chunk_hash"] != reference["chunkHash"]) {
            return Ok(json!({"current": false, "reason": "STALE_RETRIEVAL_SOURCE"}));
        }
    }
    let mut result = json!({"current": true, "sourceId": row["source_id"]});
    if row["derivation_signature"]
        .as_str()
        .is_some_and(|value| !value.is_empty())
    {
        result["derivationSignature"] = row["derivation_signature"].clone();
    }
    Ok(result)
}

fn relations(connection: &Connection, arguments: &Value, scopes: &[Value]) -> ContractResult {
    if arguments["reference"].is_object() {
        let receipt = verify_reference(connection, arguments, scopes)?;
        if receipt["current"] != true {
            return Err(ContractError::new(
                "关系来源引用过期，请重新检索。",
                if receipt["reason"] == "RETRIEVAL_SOURCE_NOT_FOUND" {
                    "RETRIEVAL_SOURCE_NOT_FOUND"
                } else {
                    "STALE_RETRIEVAL_SOURCE"
                },
                409,
            ));
        }
    }
    let limit = arguments
        .get("limit")
        .map(|value| value.as_u64().ok_or_else(|| invalid("关系导航查询无效。")))
        .transpose()?
        .unwrap_or(40);
    if !(1..=200).contains(&limit)
        || arguments["limitValid"] == false
        || arguments.get("kind").is_some_and(|kind| {
            !kind.as_str().is_some_and(|kind| {
                ["definition", "contains", "lexical-reference", "section"].contains(&kind)
            })
        })
        || arguments.get("symbol").is_some_and(|symbol| {
            !symbol
                .as_str()
                .is_some_and(|symbol| !symbol.is_empty() && symbol.encode_utf16().count() <= 1024)
        })
    {
        return Err(invalid("关系导航查询无效。"));
    }
    let mut conditions = Vec::new();
    let mut values = parameters(scopes);
    for field in ["sourceId", "symbol", "kind"] {
        if arguments[field]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        {
            conditions.push(match field {
                "sourceId" => "AND e.source_id=?",
                "symbol" => "AND (e.from_symbol=? OR e.to_symbol=?)",
                _ => "AND e.kind=?",
            });
            values.push(sql_text(text(arguments, field)));
            if field == "symbol" {
                values.push(sql_text(text(arguments, field)));
            }
        }
    }
    values.push(SqlValue::Integer(limit as i64 + 1));
    let mut selected = rows(
        connection,
        &format!(
            "SELECT e.*,s.scope_key,s.source_revision,s.content_hash,s.title,s.locator FROM source_relations e JOIN sources s ON s.source_id=e.source_id WHERE s.scope_key IN ({}) AND e.derivation_signature=s.derivation_signature {} ORDER BY CASE WHEN e.certainty='syntax' THEN 0 ELSE 1 END,e.source_id,e.start_offset,e.edge_id LIMIT ?",
            placeholders(scopes),
            conditions.join(" ")
        ),
        values,
    )?;
    let has_more = selected.len() > limit as usize;
    selected.truncate(limit as usize);
    Ok(
        json!({"version": "structure-relations-v1", "complete": false, "exhaustiveReferences": false, "hasMore": has_more, "items": selected}),
    )
}

fn vector_status(connection: &Connection, scopes: &[Value]) -> ContractResult {
    if scopes.is_empty() {
        return Err(invalid("必须提供向量范围。"));
    }
    let mut scope_rows = rows(
        connection,
        &format!(
            "SELECT s.scope_key,count(c.id) AS chunks FROM sources s JOIN chunks c ON c.source_id=s.source_id WHERE s.scope_key IN ({}) AND s.source_type NOT IN ('memory','message','conversation') GROUP BY s.scope_key",
            placeholders(scopes)
        ),
        parameters(scopes),
    )?;
    let spaces = rows(
        connection,
        &format!(
            "SELECT s.scope_key,v.embedding_profile_id AS profileId,v.embedding_space_id AS spaceId,v.embedding_model_version AS modelVersion,v.dimensions,count(*) AS vectors FROM chunk_vector_spaces v JOIN chunks c ON c.chunk_id=v.chunk_id JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key IN ({}) AND s.source_type NOT IN ('memory','message','conversation') GROUP BY s.scope_key,v.embedding_profile_id,v.embedding_space_id,v.embedding_model_version,v.dimensions",
            placeholders(scopes)
        ),
        parameters(scopes),
    )?;
    for scope in &mut scope_rows {
        scope["spaces"] = json!(
            spaces
                .iter()
                .filter(|space| space["scope_key"] == scope["scope_key"])
                .map(|space| {
                    let mut space = space.clone();
                    space["complete"] = json!(space["vectors"] == scope["chunks"]);
                    space
                })
                .collect::<Vec<_>>()
        );
    }
    Ok(
        json!({"scopes": scope_rows, "retainedSpacesPerChunk": 2, "authority": "current-source-and-embedding-input", "cleanup": "transactional-replacement-and-source-cascade"}),
    )
}

/// Scoped SQL reads and reference freshness execute in one native snapshot before any original body is exposed.
/// 范围 SQL 读取与引用新鲜度在同一原生快照执行，授权完成前不暴露原文。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    if [
        "storage.index.vectorCacheStats",
        "storage.index.vectorCacheInvalidate",
        "storage.index.vectorCacheClose",
    ]
    .contains(&operation)
    {
        return super::sqlite_vector_reads::cache_operation(operation, arguments);
    }
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    if operation == "storage.index.closeConnections" {
        super::sqlite_connections::close(root, path)?;
        return Ok(Value::Null);
    }
    if operation == "storage.index.connectionStats" {
        return super::sqlite_connections::stats(root, path);
    }
    let writes = matches!(
        operation,
        "storage.index.checkpoint" | "storage.index.removeCoverage"
    );
    let connection = if writes {
        super::sqlite_connections::acquire_write(
            root,
            path,
            arguments["timeoutMs"].as_u64().unwrap_or(120_000),
        )?
    } else {
        super::sqlite_connections::acquire(
            root,
            path,
            arguments["timeoutMs"].as_u64().unwrap_or(120_000),
        )?
    };
    if operation == "storage.index.checkpoint" {
        return super::sqlite_channels::checkpoint(&connection);
    }
    connection
        .execute_batch(if operation == "storage.index.removeCoverage" {
            "BEGIN IMMEDIATE"
        } else {
            "BEGIN"
        })
        .map_err(failure)?;
    let scopes = arguments["scopes"].as_array().cloned().unwrap_or_default();
    let result = match operation {
        "storage.index.channels" => {
            super::sqlite_channels::candidates(&connection, arguments, &scopes)
        }
        "storage.index.sourceState" => super::sqlite_channels::source_state(&connection, arguments),
        "storage.index.scopeVersion" => super::sqlite_channels::scope_version(&connection, &scopes),
        "storage.index.snapshots" => super::sqlite_channels::snapshots(&connection, Some(&scopes)),
        "storage.index.status" => super::sqlite_channels::status(&connection),
        "storage.index.listSources" => {
            super::sqlite_channels::list_sources(&connection, arguments, &scopes)
        }
        "storage.index.vectorTable" => {
            super::sqlite_vector_reads::vector_table(&connection, arguments)
                .map(|table| json!(table))
        }
        "storage.index.vectorDescriptors" => super::sqlite_vector_reads::descriptors(
            &connection,
            arguments,
            &scopes,
            connection.database_identity(),
        ),
        "storage.index.vectorPartition" => super::sqlite_vector_reads::partition(
            &connection,
            arguments,
            connection.database_identity(),
        ),
        "storage.index.vectorRows" => {
            super::sqlite_vector_reads::vector_page(&connection, arguments)
        }
        "storage.index.scopeGeneration" => {
            super::sqlite_vector_reads::scope_generation(&connection, arguments)
        }
        "storage.index.verifyVectorMatches" => {
            super::sqlite_vector_reads::verify_matches(&connection, arguments, &scopes)
        }
        "storage.index.exactVectorSearch" => {
            super::sqlite_vector_reads::exact_search(&connection, arguments)
        }
        "storage.index.vectorExtensionInfo" => {
            super::sqlite_vector_reads::extension_info(&connection, arguments)
        }
        "storage.index.verifyReference" => verify_reference(&connection, arguments, &scopes),
        "storage.index.relations" => relations(&connection, arguments, &scopes),
        "storage.index.vectorSpaceStatus" => vector_status(&connection, &scopes),
        "storage.index.readSource" => {
            if arguments["reference"].is_object() {
                let receipt = verify_reference(&connection, arguments, &scopes)?;
                if receipt["current"] != true {
                    return Err(ContractError::new(
                        if receipt["reason"] == "RETRIEVAL_SOURCE_NOT_FOUND" {
                            "Source is unavailable. / 资料不存在或已撤销。"
                        } else {
                            "Source has changed. / 资料内容已更改，请重新检索。"
                        },
                        if receipt["reason"] == "RETRIEVAL_SOURCE_NOT_FOUND" {
                            "RETRIEVAL_SOURCE_NOT_FOUND"
                        } else {
                            "STALE_RETRIEVAL_SOURCE"
                        },
                        if receipt["reason"] == "RETRIEVAL_SOURCE_NOT_FOUND" {
                            404
                        } else {
                            409
                        },
                    ));
                }
            }
            let mut values = vec![sql_text(text(arguments, "sourceId"))];
            values.extend(parameters(&scopes));
            let source = rows(
                &connection,
                &format!(
                    "SELECT * FROM sources WHERE source_id=? AND scope_key IN ({})",
                    placeholders(&scopes)
                ),
                values,
            )?
            .into_iter()
            .next();
            let chunk = if source.is_some() && arguments["chunkId"].is_string() {
                rows(&connection, "SELECT chunk_id,chunk_hash,start_offset,end_offset,structure_json FROM chunks WHERE chunk_id=? AND source_id=?", vec![sql_text(text(arguments, "chunkId")), sql_text(text(arguments, "sourceId"))])?.into_iter().next()
            } else {
                None
            };
            Ok(json!({"source": source, "chunk": chunk}))
        }
        "storage.index.removeCoverage" => {
            // This operation is deliberately routed as an actual write transaction by its adapter.
            // 此操作由适配层明确作为实际写事务调用，清理仅限授权覆盖行。
            let mut values = vec![sql_text(text(arguments, "sourceId"))];
            values.extend(parameters(&scopes));
            sql_execute(
                &connection,
                &format!(
                    "DELETE FROM source_coverage WHERE source_id=? AND scope_key IN ({})",
                    placeholders(&scopes)
                ),
                values,
            )?;
            Ok(
                json!({"removed": false, "generation": generation(&connection)?, "indexSnapshotId": snapshot(&connection)?}),
            )
        }
        _ => Err(failure("unknown scoped SQLite read")),
    };
    finish(&connection, result)
}
