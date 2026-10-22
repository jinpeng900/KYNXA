use super::{
    ContractError, ContractResult, sqlite_channels, sqlite_connections, sqlite_lexical,
    sqlite_store::*, sqlite_vector_reads,
};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const MAX_PENDING_QUERIES: usize = 64;
static NEXT_QUERY: AtomicU64 = AtomicU64::new(1);
static PENDING: OnceLock<Mutex<HashMap<u64, Query>>> = OnceLock::new();
struct Query {
    root: PathBuf,
    path: PathBuf,
    request_id: String,
    database_identity: (u64, u64),
    started: Instant,
    arguments: Value,
    snapshot: Value,
    channels: Value,
    descriptors: Value,
    invalid: u64,
}

pub fn handles(operation: &str) -> bool {
    matches!(
        operation,
        "storage.index.queryBegin"
            | "storage.index.queryVectors"
            | "storage.index.queryFinish"
            | "storage.index.queryDiscard"
    )
}
fn stale() -> ContractError {
    ContractError::new(
        "Retrieval sources changed; reacquire this query. / 检索来源已变化，请重新获取本次查询。",
        "RETRIEVAL_QUERY_CHANGED",
        409,
    )
}
fn invalid() -> ContractError {
    ContractError::new(
        "Invalid retrieval query context. / 检索查询上下文无效或已过期。",
        "INVALID_RETRIEVAL_QUERY_CONTEXT",
        409,
    )
}
fn values(scopes: &[Value]) -> Vec<SqlValue> {
    scopes
        .iter()
        .map(|scope| sql_text(scope.as_str().unwrap_or("")))
        .collect()
}
fn scopes(arguments: &Value) -> Result<Vec<Value>, ContractError> {
    arguments["scopes"]
        .as_array()
        .filter(|items| {
            !items.is_empty()
                && items.len() <= 1000
                && items
                    .iter()
                    .all(|scope| scope.as_str().is_some_and(|scope| !scope.is_empty()))
        })
        .cloned()
        .ok_or_else(invalid)
}
fn pending() -> &'static Mutex<HashMap<u64, Query>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}
pub(crate) fn close_cache(cache_id: &Value) -> Result<(), ContractError> {
    if let Some(queries) = PENDING.get() {
        queries
            .lock()
            .map_err(failure)?
            .retain(|_, query| query.arguments["cacheId"] != *cache_id);
    }
    Ok(())
}
fn insert(query: Query) -> Result<u64, ContractError> {
    let mut queries = pending().lock().map_err(failure)?;
    queries.retain(|_, query| query.started.elapsed() < Duration::from_secs(120));
    if queries.len() >= MAX_PENDING_QUERIES {
        return Err(ContractError::new(
            "Retrieval query capacity is busy. / 检索查询额度繁忙。",
            "RETRIEVAL_CONNECTION_CAPACITY",
            503,
        ));
    }
    let id = NEXT_QUERY.fetch_add(1, Ordering::Relaxed);
    queries.insert(id, query);
    Ok(id)
}
fn take(arguments: &Value) -> Result<(u64, Query), ContractError> {
    let id = arguments["queryId"].as_u64().ok_or_else(invalid)?;
    let mut queries = pending().lock().map_err(failure)?;
    let query = queries.get(&id).ok_or_else(invalid)?;
    if query.root != Path::new(arguments["root"].as_str().unwrap_or(""))
        || query.path != Path::new(arguments["path"].as_str().unwrap_or(""))
        || query.request_id != text(arguments, "requestId")
    {
        return Err(invalid());
    }
    let query = queries.remove(&id).ok_or_else(invalid)?;
    if query.started.elapsed() >= Duration::from_secs(120) {
        return Err(invalid());
    }
    Ok((id, query))
}
fn ids(channels: &Value) -> Vec<Value> {
    let mut seen = HashSet::new();
    channels
        .as_object()
        .into_iter()
        .flat_map(|channels| channels.values())
        .flat_map(|rows| rows.as_array().into_iter().flatten())
        .filter_map(|row| row["chunk_id"].as_str())
        .filter(|id| seen.insert((*id).to_owned()))
        .map(|id| json!(id))
        .collect()
}
fn check_current(
    connection: &Connection,
    query: &Query,
    identity: (u64, u64),
) -> Result<(), ContractError> {
    if query.database_identity != identity {
        return Err(stale());
    }
    let current = sqlite_channels::snapshots(connection, Some(&scopes(&query.arguments)?))?;
    // Scope snapshots originate before candidate generation, not from a later unrelated metadata receipt.
    // 范围快照来自候选生成前，不能用事后读取的元信息冒充候选所属版本。
    if current["scopeSnapshots"] != query.snapshot["scopeSnapshots"] {
        return Err(stale());
    }
    Ok(())
}
fn identity_arguments(arguments: &Value) -> Value {
    json!({"profile":arguments["profile"],"dimensions":arguments["queryVector"].as_array().map_or(0, Vec::len),
        "modelProvided":arguments["modelProvided"],"model":arguments["model"],
        "spaceProvided":arguments["spaceProvided"],"space":arguments["space"],"metadataOnly":true})
}
fn collect_vectors(
    connection: &Connection,
    query: &mut Query,
    input: &Value,
) -> Result<(), ContractError> {
    let mut matches = input["matches"].as_array().cloned().unwrap_or_default();
    if matches.len() > 160_000 {
        return Err(invalid());
    }
    let descriptors = input["exactDescriptors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let authorized = query.descriptors.as_array().ok_or_else(invalid)?;
    for descriptor in &descriptors {
        cancelled()?;
        let belongs = authorized.iter().any(|original| {
            [
                "scope_key",
                "embedding_profile_id",
                "dimensions",
                "embedding_model_version",
                "embedding_space_id",
                "domain",
                "generation",
            ]
            .iter()
            .all(|field| original[field] == descriptor[field])
        });
        if !belongs {
            return Err(invalid());
        }
        let result = sqlite_vector_reads::exact_search_in_snapshot(
            connection,
            &json!({"vectorTable":query.arguments["vectorTable"],
            "descriptor":descriptor,"scopes":query.arguments["scopes"],"queryVector":query.arguments["queryVector"],
            "limit":query.arguments["candidateLimit"],"extensionPath":input["extensionPath"]}),
            true,
        )?;
        query.invalid += result["invalid"].as_u64().unwrap_or(0);
        for row in result["items"].as_array().into_iter().flatten() {
            matches.push(json!({"id":row["id"],"distance":row["distance"],"identity":descriptor,
                "domainRank":if query.arguments["domain"].is_string() && query.arguments["domain"] != descriptor["domain"] { 1 } else { 0 }}));
        }
    }
    let ranked = super::index_validation::rank_vectors(
        &json!({"items":matches,"limit":query.arguments["candidateLimit"]}),
    )?;
    let mut args = identity_arguments(&query.arguments);
    args["vectorTable"] = query.arguments["vectorTable"].clone();
    args["matches"] = ranked;
    let verified =
        sqlite_vector_reads::verify_matches(connection, &args, &scopes(&query.arguments)?)?;
    if verified["mismatch"] == true {
        return Err(stale());
    }
    query.channels["vector"] = verified["items"].clone();
    Ok(())
}
fn candidate_channels(
    connection: &mut sqlite_connections::SqliteConnectionLease,
    args: &Value,
    scopes: &[Value],
) -> ContractResult {
    let lexical = if args["expression"]
        .as_str()
        .is_some_and(|expression| !expression.is_empty())
    {
        sqlite_lexical::query(connection, args)?["items"].clone()
    } else {
        json!([])
    };
    let mut channels = sqlite_channels::candidates(connection, args, scopes)?;
    channels["lexical"] = lexical;
    if let Some(literal) = channels["literal"].as_array().cloned() {
        channels["lexical"]
            .as_array_mut()
            .ok_or_else(invalid)?
            .extend(literal);
    }
    let object = channels.as_object_mut().ok_or_else(invalid)?;
    object.remove("literal");
    let symbols = object.remove("symbols").unwrap_or(json!([]));
    let paths = object.remove("paths").unwrap_or(json!([]));
    object.insert("symbol".to_owned(), symbols);
    object.insert("path".to_owned(), paths);
    object.insert("vector".to_owned(), json!([]));
    Ok(channels)
}

fn begin(arguments: &Value) -> ContractResult {
    cancelled()?;
    let scopes = scopes(arguments)?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    // The I/O boundary has admitted memory already; storage only checks in-memory eligibility.
    // I/O 边界已完成内存准入，存储层只读取内存中的缓存可用状态。
    let cache_admitted = crate::retrieval::cache_candidates::admitted();
    let mut connection = sqlite_connections::acquire(
        root,
        path,
        arguments["timeoutMs"].as_u64().unwrap_or(120_000),
    )?;
    connection.execute_batch("BEGIN").map_err(failure)?;
    let mut snapshot = sqlite_channels::snapshots(&connection, Some(&scopes))?;
    let mut args = arguments.clone();
    args["limit"] = json!(
        arguments["candidateLimit"]
            .as_u64()
            .unwrap_or(40)
            .clamp(1, 160)
    );
    args["metadataOnly"] = Value::Bool(true);
    let identity = connection.database_identity();
    let (channels, cache_receipt) = crate::retrieval::cache_candidates::reuse(
        &args,
        &snapshot,
        identity,
        cache_admitted,
        || candidate_channels(&mut connection, &args, &scopes),
        cancelled,
    )?;
    snapshot["candidateCache"] = cache_receipt;
    let has_vector = arguments["queryVector"].is_array() && arguments["profile"].is_string();
    let descriptors = if has_vector {
        let mut descriptor_args = identity_arguments(arguments);
        descriptor_args["cacheId"] = arguments["cacheId"].clone();
        descriptor_args["vectorTable"] = arguments["vectorTable"].clone();
        descriptor_args["domain"] = arguments["domain"].clone();
        sqlite_vector_reads::descriptors(
            &connection,
            &descriptor_args,
            &scopes,
            connection.database_identity(),
        )?
    } else {
        json!([])
    };
    let mut query = Query {
        root: root.to_owned(),
        path: path.to_owned(),
        request_id: text(arguments, "requestId"),
        database_identity: connection.database_identity(),
        started: Instant::now(),
        arguments: arguments.clone(),
        snapshot,
        channels,
        descriptors,
        invalid: 0,
    };
    let direct = has_vector && arguments["directExact"] == true;
    let mut scan_limited = false;
    if direct {
        let count: u64 = query
            .descriptors
            .as_array()
            .into_iter()
            .flatten()
            .map(|descriptor| descriptor["count"].as_u64().unwrap_or(0))
            .sum();
        if count <= arguments["exactScanLimit"].as_u64().unwrap_or(500_000) {
            let input = json!({"exactDescriptors":query.descriptors,"extensionPath":arguments["extensionPath"]});
            collect_vectors(&connection, &mut query, &input)?;
        } else {
            scan_limited = true;
        }
    }
    let result = json!({"candidateIds":ids(&query.channels),"descriptors":query.descriptors,
        "databaseIdentity":format!("{:016x}:{:016x}",query.database_identity.0,query.database_identity.1),
        "vectorsReady":direct,"exactScanLimited":scan_limited,"invalid":query.invalid,"structuredChannels":{"symbolCandidates":query.channels["symbol"].as_array().map_or(0, Vec::len),
        "pathCandidates":query.channels["path"].as_array().map_or(0, Vec::len)}});
    connection.execute_batch("COMMIT").map_err(failure)?;
    let query_id = insert(query)?;
    let mut result = result;
    result["queryId"] = json!(query_id);
    Ok(result)
}
fn finish(connection: &Connection, query: &Query, arguments: &Value) -> ContractResult {
    let mut fusion = Value::Object(query.channels.as_object().ok_or_else(invalid)?.iter().map(|(channel, rows)| {
        (channel.to_owned(), json!(rows.as_array().into_iter().flatten().map(|row| json!({"chunkId":row["chunk_id"],
            "actualDomain":row["actual_domain"],"exactTargetMatch":row["exact_target_match"]==true})).collect::<Vec<_>>()))
    }).collect());
    fusion["preferredDomain"] = query.arguments["preferredDomain"].clone();
    fusion["rrfK"] = query.arguments["rrfK"].clone();
    fusion["collationOrder"] = arguments["collationOrder"].clone();
    fusion["limit"] = json!(
        query.arguments["resultLimit"]
            .as_u64()
            .unwrap_or(8)
            .clamp(1, 192)
    );
    let fused = crate::retrieval::fusion::apply(&fusion);
    let requested = fused.as_array().ok_or_else(invalid)?;
    if requested.is_empty() {
        return Ok(
            json!({"rows":[],"snapshot":query.snapshot,"strategy":"lexical","invalid":query.invalid}),
        );
    }
    let scopes = scopes(&query.arguments)?;
    let mut params: Vec<SqlValue> = requested
        .iter()
        .map(|item| sql_text(text(item, "chunkId")))
        .collect();
    params.extend(values(&scopes));
    let selected = rows(
        connection,
        &format!(
            "SELECT {} FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE c.chunk_id IN ({}) AND s.scope_key IN ({})",
            sqlite_channels::chunk_columns().replace("c.text,c.lexical_text,", ""),
            vec!["?"; requested.len()].join(","),
            vec!["?"; scopes.len()].join(",")
        ),
        params,
    )?;
    if selected.len() != requested.len() {
        return Err(stale());
    }
    let mut selected: HashMap<String, Value> = selected
        .into_iter()
        .map(|row| (text(&row, "chunk_id"), row))
        .collect();
    let mut output = Vec::with_capacity(requested.len());
    for item in requested {
        let mut row = selected.remove(&text(item, "chunkId")).ok_or_else(stale)?;
        // Recheck scoped metadata above before looking up an immutable body in the process cache.
        // 先在当前快照重新核验范围与来源版本，再复用不可变正文；删除和撤权不能命中旧正文。
        let load_body = || {
            connection.prepare_cached("SELECT text,lexical_text FROM chunks WHERE chunk_id=?")
                .map_err(failure)?.query_row([text(&row, "chunk_id")], |record| {
                    Ok(json!({"text":record.get::<_, String>(0)?,"lexical_text":record.get::<_, String>(1)?}))
                }).map_err(failure)
        };
        let body = if query.arguments["passageCacheAdmitted"] == true {
            crate::retrieval::cache_passages::read(query.database_identity, &row, load_body)?
        } else { load_body()? };
        row["text"] = body["text"].clone();
        row["lexical_text"] = body["lexical_text"].clone();
        row["score"] = item["score"].clone();
        for channel in ["lexical", "vector", "symbol", "path"] {
            if let Some(position) = item["positions"][channel].as_u64() {
                row[format!("{channel}_position")] = json!(position + 1);
                if let Some(candidate) = query.channels[channel]
                    .as_array()
                    .and_then(|rows| rows.get(position as usize))
                {
                    for field in ["lexical_rank", "distance", "exact_target_match"] {
                        if let Some(value) = candidate.get(field) {
                            row[field] = value.clone();
                        }
                    }
                }
            }
        }
        output.push(row);
    }
    Ok(
        json!({"rows":output,"snapshot":query.snapshot,"strategy":if query.channels["vector"].as_array().is_some_and(|items|!items.is_empty()) { "hybrid" } else { "lexical" },"invalid":query.invalid}),
    )
}

/// Query contexts retain bounded identifiers, never body text or open transactions across SDK awaits.
/// 查询上下文只保留有界标识；等待 SDK 时不保留正文、数据库游标或活动事务。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    if operation == "storage.index.queryBegin" {
        return begin(arguments);
    }
    let (id, mut query) = take(arguments)?;
    if operation == "storage.index.queryDiscard" {
        return Ok(Value::Null);
    }
    let connection = sqlite_connections::acquire(
        &query.root,
        &query.path,
        arguments["timeoutMs"].as_u64().unwrap_or(120_000),
    )?;
    connection.execute_batch("BEGIN").map_err(failure)?;
    check_current(&connection, &query, connection.database_identity())?;
    let result = if operation == "storage.index.queryVectors" {
        collect_vectors(&connection, &mut query, arguments)?;
        json!({"candidateIds":ids(&query.channels),"invalid":query.invalid})
    } else {
        finish(&connection, &query, arguments)?
    };
    connection.execute_batch("COMMIT").map_err(failure)?;
    if operation == "storage.index.queryVectors" {
        pending().lock().map_err(failure)?.insert(id, query);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        path: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "kynxa-query-{}-{}",
                std::process::id(),
                NEXT_QUERY.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("Index").join("retrieval.sqlite");
            super::super::sqlite_bootstrap::apply(
                "storage.index.bootstrap",
                &json!({"root":root,"path":path,"epoch":"fixture"}),
            )
            .unwrap();
            let writer = Connection::open(&path).unwrap();
            writer.execute_batch("INSERT INTO sources(source_id,scope_key,source_type,title,locator,text,content_hash,source_revision,binding_revision,active_generation,updated_at)
                VALUES ('source','project:one','document','title','{}','alpha evidence','hash','1',1,1,'fixture');
                INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version)
                VALUES ('chunk','source',0,'alpha evidence','alpha evidence','hash',0,14,1,1,'fixture','fixture');
                INSERT INTO scope_snapshots(scope_key,generation) VALUES ('project:one',1),('project:other',1);").unwrap();
            Self { root, path }
        }
        fn arguments(&self) -> Value {
            json!({"root":self.root,"path":self.path,"requestId":"owner","query":"alpha","expression":"\"alpha\"",
                "scopes":["project:one"],"candidateLimit":128,"resultLimit":8,"vectorTable":"chunks"})
        }
        fn continuation(&self, id: &Value) -> Value {
            let mut args = self.arguments();
            args["queryId"] = id.clone();
            args["collationOrder"] = json!(["chunk"]);
            args
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            sqlite_connections::close(&self.root, &self.path).unwrap();
            assert!(self.root.starts_with(std::env::temp_dir()));
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn query_projection_defers_bodies_and_keeps_original_scope_snapshot() {
        let fixture = Fixture::new();
        let prepared = apply("storage.index.queryBegin", &fixture.arguments()).unwrap();
        assert_eq!(prepared["candidateIds"], json!(["chunk"]));
        assert!(!prepared.to_string().contains("alpha evidence"));
        let writer = Connection::open(&fixture.path).unwrap();
        writer
            .execute(
                "UPDATE scope_snapshots SET generation=2 WHERE scope_key='project:other'",
                [],
            )
            .unwrap();
        let result = apply(
            "storage.index.queryFinish",
            &fixture.continuation(&prepared["queryId"]),
        )
        .unwrap();
        assert_eq!(result["rows"][0]["text"], "alpha evidence");
        assert_eq!(result["snapshot"]["scopeSnapshots"][0]["generation"], 1);
    }
    #[test]
    fn query_continuation_rejects_scope_updates_and_cross_request_consumption() {
        let fixture = Fixture::new();
        let prepared = apply("storage.index.queryBegin", &fixture.arguments()).unwrap();
        let mut other = fixture.continuation(&prepared["queryId"]);
        other["requestId"] = json!("other");
        assert_eq!(
            apply("storage.index.queryFinish", &other).unwrap_err().code,
            "INVALID_RETRIEVAL_QUERY_CONTEXT"
        );
        let writer = Connection::open(&fixture.path).unwrap();
        writer
            .execute(
                "UPDATE scope_snapshots SET generation=2 WHERE scope_key='project:one'",
                [],
            )
            .unwrap();
        assert_eq!(
            apply(
                "storage.index.queryFinish",
                &fixture.continuation(&prepared["queryId"])
            )
            .unwrap_err()
            .code,
            "RETRIEVAL_QUERY_CHANGED"
        );
    }
    #[test]
    fn cancelled_continuation_cannot_leak_a_body_or_poison_the_next_query() {
        let fixture = Fixture::new();
        let prepared = apply("storage.index.queryBegin", &fixture.arguments()).unwrap();
        let id = format!("query-cancel:{}", prepared["queryId"]);
        super::super::cancellation::retain(&id).unwrap();
        {
            let token = super::super::cancellation::CancellationLease::acquire(&id).unwrap();
            let _context = token.enter();
            super::super::cancellation::cancel(&id).unwrap();
            assert_eq!(
                apply(
                    "storage.index.queryFinish",
                    &fixture.continuation(&prepared["queryId"])
                )
                .unwrap_err()
                .code,
                "ABORT_ERR"
            );
        }
        super::super::cancellation::release(&id).unwrap();
        let prepared = apply("storage.index.queryBegin", &fixture.arguments()).unwrap();
        assert_eq!(
            apply(
                "storage.index.queryFinish",
                &fixture.continuation(&prepared["queryId"])
            )
            .unwrap()["rows"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn binary_vector_page_preserves_bits_and_rejects_changed_snapshot() {
        let fixture = Fixture::new();
        let writer = Connection::open(&fixture.path).unwrap();
        let bytes: Vec<u8> = [0x8000_0000u32, 1u32]
            .iter()
            .flat_map(|bits| bits.to_le_bytes())
            .collect();
        writer.execute("UPDATE chunks SET dimensions=2,embedding_profile_id='fixture',embedding_model_version='v1',embedding_space_id='space',vector=?", [bytes.clone()]).unwrap();
        let mut args = json!({"root":fixture.root,"path":fixture.path,"vectorTable":"chunks","sourceIds":["source"],
            "scopes":["project:one"],"lastId":0,"lastSpace":""});
        let (metadata, actual) = super::super::sqlite_vector_reads::vector_page_binary(&args);
        assert_eq!(actual, bytes);
        assert_eq!(metadata["value"]["items"][0]["vectorOffset"], 0);
        assert_eq!(metadata["value"]["items"][0]["vectorBytes"], 8);
        assert!(
            !metadata["value"]["items"][0]
                .as_object()
                .unwrap()
                .contains_key("vector")
        );
        let mut metadata_arguments = args.clone();
        metadata_arguments["metadataOnly"] = json!(true);
        let (metadata_page, no_blob) =
            super::super::sqlite_vector_reads::vector_page_binary(&metadata_arguments);
        assert!(no_blob.is_empty());
        assert_eq!(
            metadata_page["value"]["items"][0]["id"],
            metadata["value"]["items"][0]["id"]
        );
        assert!(
            metadata_page["value"]["items"][0]
                .get("vectorOffset")
                .is_none()
        );
        args["expectedScopeSnapshots"] = metadata["value"]["scopeSnapshots"].clone();
        args["expectedDatabaseIdentity"] = metadata["value"]["databaseIdentity"].clone();
        writer
            .execute(
                "UPDATE scope_snapshots SET generation=2 WHERE scope_key='project:one'",
                [],
            )
            .unwrap();
        let (failure, leaked) = super::super::sqlite_vector_reads::vector_page_binary(&args);
        assert_eq!(failure["error"]["code"], "RETRIEVAL_ANN_STALE_BUILD");
        assert!(leaked.is_empty());
    }
}
