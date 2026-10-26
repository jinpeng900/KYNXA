use super::{
    ContractError, ContractResult, sqlite_channels::chunk_columns, sqlite_lexical::DOMAIN_SQL,
    sqlite_store::*,
};
use rusqlite::{Connection, LoadExtensionGuard, types::Value as SqlValue};
#[cfg(test)]
use rusqlite::{params_from_iter, types::ValueRef};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const CACHE_COUNT: usize = 64;
const CACHE_BYTES: usize = 8 * 1024 * 1024;
#[derive(Default)]
struct Entries {
    entries: VecDeque<(String, Value, Vec<Value>, usize)>,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    descriptors: Entries,
    partitions: Entries,
    hits: usize,
    misses: usize,
    invalidated: usize,
}
static CACHES: OnceLock<Mutex<HashMap<String, Cache>>> = OnceLock::new();

fn cache_key(arguments: &Value) -> String {
    arguments["cacheId"].as_str().unwrap_or("").to_owned()
}
fn with_cache<T>(
    arguments: &Value,
    operation: impl FnOnce(&mut Cache) -> T,
) -> Result<T, ContractError> {
    let mut caches = CACHES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(failure)?;
    let id = cache_key(arguments);
    if caches.len() >= 64 && !caches.contains_key(&id) {
        if let Some(oldest) = caches.keys().next().cloned() {
            caches.remove(&oldest);
        }
    }
    Ok(operation(caches.entry(id).or_default()))
}
fn get(arguments: &Value, key: &str, partition: bool) -> Result<Option<Value>, ContractError> {
    if arguments["cacheId"].is_null() {
        return Ok(None);
    }
    with_cache(arguments, |cache| {
        let entries = if partition {
            &mut cache.partitions
        } else {
            &mut cache.descriptors
        };
        let item = entries
            .entries
            .iter()
            .position(|entry| entry.0 == key)
            .and_then(|index| entries.entries.remove(index));
        if let Some(item) = item {
            let result = item.1.clone();
            entries.entries.push_back(item);
            if !partition {
                cache.hits += 1;
            }
            Some(result)
        } else {
            if !partition {
                cache.misses += 1;
            }
            None
        }
    })
}
fn put(
    arguments: &Value,
    key: String,
    value: Value,
    scopes: Vec<Value>,
    partition: bool,
) -> Result<(), ContractError> {
    if arguments["cacheId"].is_null() {
        return Ok(());
    }
    let bytes = key.len() + value.to_string().len();
    if bytes > CACHE_BYTES {
        return Ok(());
    }
    with_cache(arguments, |cache| {
        let entries = if partition {
            &mut cache.partitions
        } else {
            &mut cache.descriptors
        };
        while entries.entries.len() >= CACHE_COUNT || entries.bytes + bytes > CACHE_BYTES {
            if let Some(item) = entries.entries.pop_front() {
                entries.bytes -= item.3;
            }
        }
        entries.bytes += bytes;
        entries.entries.push_back((key, value, scopes, bytes));
    })
}
pub fn cache_operation(operation: &str, arguments: &Value) -> ContractResult {
    if operation == "storage.index.vectorCacheClose" {
        super::sqlite_search::close_cache(&arguments["cacheId"])?;
        CACHES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(failure)?
            .remove(&cache_key(arguments));
        return Ok(Value::Null);
    }
    with_cache(arguments, |cache| {
        if operation == "storage.index.vectorCacheInvalidate" {
            let scopes = arguments["scopes"].as_array().cloned().unwrap_or_default();
            cache.partitions = Entries::default();
            cache.descriptors.entries.retain(|entry| {
                let remove = entry.2.iter().any(|scope| scopes.contains(scope));
                if remove {
                    cache.invalidated += 1;
                    cache.descriptors.bytes -= entry.3;
                }
                !remove
            });
        }
        json!({"partitionCache":{"entries":cache.partitions.entries.len(),"payloadBytes":cache.partitions.bytes},
            "descriptorCache":{"entries":cache.descriptors.entries.len(),"payloadBytes":cache.descriptors.bytes,"hits":cache.hits,"misses":cache.misses,"invalidated":cache.invalidated}})
    })
}

pub fn vector_table(
    connection: &Connection,
    arguments: &Value,
) -> Result<&'static str, ContractError> {
    if arguments["vectorTable"] == "chunks" {
        return Ok("chunks");
    }
    if arguments["vectorTable"] == "retrieval_vector_rows" {
        return Ok("retrieval_vector_rows");
    }
    Ok(
        if rows(
            connection,
            "SELECT 1 FROM sqlite_master WHERE type='view' AND name='retrieval_vector_rows'",
            vec![],
        )?
        .is_empty()
        {
            "chunks"
        } else {
            "retrieval_vector_rows"
        },
    )
}
fn placeholders(values: &[Value]) -> String {
    vec!["?"; values.len()].join(",")
}
fn values(values: &[Value]) -> Vec<SqlValue> {
    values
        .iter()
        .map(|value| sql_text(value.as_str().unwrap_or("")))
        .collect()
}
fn identity_values(descriptor: &Value) -> Vec<SqlValue> {
    vec![
        sql_text(text(descriptor, "scope_key")),
        sql_text(text(descriptor, "embedding_profile_id")),
        number(&descriptor["dimensions"]),
        sql_text(text(descriptor, "embedding_model_version")),
        sql_text(text(descriptor, "embedding_space_id")),
        sql_text(text(descriptor, "domain")),
    ]
}
fn identity_predicate() -> String {
    identity_predicate_for_vector("c")
}
fn identity_predicate_for_vector(vector_alias: &str) -> String {
    format!(
        "s.scope_key=? AND {vector_alias}.embedding_profile_id=? AND {vector_alias}.dimensions=? AND {vector_alias}.embedding_model_version=? AND {vector_alias}.embedding_space_id=? AND {DOMAIN_SQL}=? AND {vector_alias}.vector IS NOT NULL"
    )
}
pub fn scope_generation(connection: &Connection, arguments: &Value) -> ContractResult {
    let scoped = rows(
        connection,
        "SELECT generation FROM scope_snapshots WHERE scope_key=?",
        vec![sql_text(text(arguments, "scopeKey"))],
    )?;
    Ok(scoped
        .first()
        .map_or(Value::Null, |row| row["generation"].clone()))
}
fn check_generation(connection: &Connection, descriptor: &Value) -> Result<(), ContractError> {
    if descriptor.get("generation").is_some()
        && scope_generation(connection, &json!({"scopeKey":descriptor["scope_key"]}))?
            != descriptor["generation"]
    {
        return Err(ContractError::new(
            "ANN source generation changed. / 向量来源范围版本已变化。",
            "RETRIEVAL_ANN_STALE_BUILD",
            409,
        ));
    }
    Ok(())
}

pub fn descriptors(
    connection: &Connection,
    arguments: &Value,
    scopes: &[Value],
    database_identity: (u64, u64),
) -> ContractResult {
    let table = vector_table(connection, arguments)?;
    let versions = rows(
        connection,
        &format!(
            "SELECT scope_key,generation FROM scope_snapshots WHERE scope_key IN ({}) ORDER BY scope_key",
            placeholders(scopes)
        ),
        values(scopes),
    )?;
    let epoch = rows(
        connection,
        "SELECT value FROM retrieval_metadata WHERE key='index_epoch'",
        vec![],
    )?;
    let key = json!([
        database_identity,
        epoch,
        scopes,
        versions,
        arguments["profile"],
        arguments["dimensions"],
        arguments["model"],
        arguments["modelProvided"],
        arguments["space"],
        arguments["spaceProvided"],
        arguments["domain"],
        arguments["allProfiles"],
        table
    ])
    .to_string();
    if let Some(cached) = get(arguments, &key, false)? {
        return Ok(cached);
    }
    let mut filters = String::new();
    let mut parameters = values(scopes);
    if let Some(domain) = arguments["domain"]
        .as_str()
        .filter(|value| !value.is_empty())
    {
        filters.push_str(&format!(" AND ({DOMAIN_SQL}=? OR {DOMAIN_SQL}='')"));
        parameters.push(sql_text(domain));
    }
    if arguments["allProfiles"] != true {
        filters.push_str(" AND c.embedding_profile_id=? AND c.dimensions=?");
        parameters.extend([
            sql_text(text(arguments, "profile")),
            number(&arguments["dimensions"]),
        ]);
        if arguments["modelProvided"] == true {
            filters.push_str(" AND c.embedding_model_version=?");
            parameters.push(nullable_text(&arguments["model"]));
        }
        if arguments["spaceProvided"] == true {
            filters.push_str(" AND c.embedding_space_id=?");
            parameters.push(nullable_text(&arguments["space"]));
        }
    }
    let result = json!(rows(connection,&format!("SELECT s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,c.embedding_space_id,
        {DOMAIN_SQL} AS domain,count(*) AS count,COALESCE(ss.generation,0) AS generation FROM {table} c
        JOIN sources s ON s.source_id=c.source_id LEFT JOIN scope_snapshots ss ON ss.scope_key=s.scope_key
        WHERE s.scope_key IN ({}){filters} AND c.vector IS NOT NULL
        GROUP BY s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,c.embedding_space_id,domain",placeholders(scopes)),parameters)?);
    if arguments["allProfiles"] != true {
        put(arguments, key, result.clone(), scopes.to_vec(), false)?;
    }
    Ok(result)
}

pub fn partition(
    connection: &Connection,
    arguments: &Value,
    database_identity: (u64, u64),
) -> ContractResult {
    let descriptor = &arguments["descriptor"];
    check_generation(connection, descriptor)?;
    let segments = arguments["segments"]
        .as_u64()
        .filter(|count| *count > 0 && *count <= 1_000_000)
        .ok_or_else(|| failure("invalid partition count"))?;
    let key = json!([database_identity, descriptor, segments]).to_string();
    if let Some(cached) = get(arguments, &key, true)? {
        return Ok(cached);
    }
    let table = vector_table(connection, arguments)?;
    let mut parameters = vec![SqlValue::Integer(segments as i64)];
    parameters.extend(identity_values(descriptor));
    parameters.extend([
        number(
            &descriptor["minimumId"]
                .as_u64()
                .map_or(json!(0), |value| json!(value)),
        ),
        number(
            &descriptor["maximumId"]
                .as_u64()
                .map_or(json!(9_007_199_254_740_991u64), |value| json!(value)),
        ),
    ]);
    let result = json!(rows(connection,&format!("WITH authorized AS MATERIALIZED (
        SELECT c.id,ntile(?) OVER (ORDER BY c.id) AS segment FROM {table} c JOIN sources s ON s.source_id=c.source_id
        WHERE {} AND c.id>=? AND c.id<=?
    ) SELECT min(id) AS minimumId,max(id) AS maximumId,count(*) AS count FROM authorized GROUP BY segment ORDER BY segment",identity_predicate()),parameters)?);
    put(
        arguments,
        key,
        result.clone(),
        vec![descriptor["scope_key"].clone()],
        true,
    )?;
    Ok(result)
}

/// Check keysets and generations per snapshot; release cursors before external work.
/// 每次快照内核对键集和范围版本，外部计算前释放游标。
pub fn vector_page(connection: &Connection, arguments: &Value) -> ContractResult {
    let Some((sql, parameters)) = vector_page_query(connection, arguments)? else {
        return Ok(json!([]));
    };
    Ok(json!(rows(connection, &sql, parameters)?))
}

pub(crate) fn vector_page_query(
    connection: &Connection,
    arguments: &Value,
) -> Result<Option<(String, Vec<SqlValue>)>, ContractError> {
    let table = vector_table(connection, arguments)?;
    let descriptor = &arguments["descriptor"];
    let scopes = arguments["scopes"].as_array().cloned().unwrap_or_default();
    if descriptor.is_object() {
        if arguments["binaryRequiredScope"] == true && !scopes.contains(&descriptor["scope_key"]) {
            return Err(ContractError::new(
                "Vector scope is unauthorized. / 向量范围未授权。",
                "RETRIEVAL_SOURCE_NOT_FOUND",
                404,
            ));
        }
        check_generation(connection, descriptor)?;
        let mut parameters = identity_values(descriptor);
        parameters.push(number(&arguments["lastId"]));
        parameters.extend([
            number(
                &descriptor["minimumId"]
                    .as_u64()
                    .map_or(json!(0), |value| json!(value)),
            ),
            number(
                &descriptor["maximumId"]
                    .as_u64()
                    .map_or(json!(9_007_199_254_740_991u64), |value| json!(value)),
            ),
        ]);
        // Seek chunk IDs before joining vector spaces, so each page avoids sorting all remaining vectors.
        // 先按分块 ID 范围读取，再关联向量空间，避免每页重排所有剩余向量；向量字段仍取所选空间。
        let (source, vector_alias) = if table == "retrieval_vector_rows" {
            (
                "chunks c CROSS JOIN chunk_vector_spaces v ON v.chunk_id=c.chunk_id CROSS JOIN sources s ON s.source_id=c.source_id",
                "v",
            )
        } else {
            (
                "chunks c JOIN sources s ON s.source_id=c.source_id",
                "c",
            )
        };
        let columns = format!(
            "c.id,{}{vector_alias}.dimensions{}",
            if arguments["includeChunkId"] == true {
                "c.chunk_id,"
            } else {
                ""
            },
            if arguments["metadataOnly"] == true {
                String::new()
            } else {
                format!(",{vector_alias}.vector")
            }
        );
        return Ok(Some((
            format!(
                "SELECT {columns} FROM {source} WHERE {} AND c.id>? AND c.id>=? AND c.id<=? ORDER BY c.id LIMIT 256",
                identity_predicate_for_vector(vector_alias)
            ),
            parameters,
        )));
    }
    let ids = arguments["sourceIds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(None);
    }
    if ids.len() > 100
        || ids.iter().any(|id| {
            !id.as_str()
                .is_some_and(|id| !id.is_empty() && id.len() <= 256)
        })
    {
        return Err(ContractError::new(
            "A bounded source batch is required. / 必须提供有界资料批次。",
            "INVALID_RETRIEVAL_INPUT",
            400,
        ));
    }
    if arguments["binaryRequiredScope"] == true && scopes.is_empty() {
        return Err(ContractError::new(
            "Vector scope is unauthorized. / 向量范围未授权。",
            "RETRIEVAL_SOURCE_NOT_FOUND",
            404,
        ));
    }
    let mut parameters = values(&ids);
    let scope_filter = if scopes.is_empty() {
        String::new()
    } else {
        parameters.extend(values(&scopes));
        format!(" AND s.scope_key IN ({})", placeholders(&scopes))
    };
    parameters.extend([
        number(&arguments["lastId"]),
        number(&arguments["lastId"]),
        sql_text(text(arguments, "lastSpace")),
    ]);
    let vector_column = if arguments["metadataOnly"] == true {
        ""
    } else {
        "c.vector,"
    };
    Ok(Some((format!("SELECT c.id,{vector_column}c.embedding_profile_id,c.dimensions,c.embedding_model_version,c.embedding_space_id,s.scope_key,{DOMAIN_SQL} AS domain
        FROM {table} c JOIN sources s ON s.source_id=c.source_id WHERE c.source_id IN ({}){scope_filter} AND c.vector IS NOT NULL
            AND (c.id>? OR (c.id=? AND c.embedding_space_id>?)) ORDER BY c.id,c.embedding_space_id LIMIT 256",placeholders(&ids)),parameters)))
}

#[cfg(test)]
fn physical_identity_token(identity: (u64, u64)) -> String {
    // Arbitrary filesystem u64 identities cannot safely round-trip through JavaScript numbers.
    // 文件系统 u64 身份不能经 JavaScript 数值安全往返，采用原样续传的十六进制不透明令牌。
    format!("{:016x}:{:016x}", identity.0, identity.1)
}

/// Preserve stored float bits in binary pages; use JSON only for offsets and IDs.
/// 二进制分页保留原浮点位，JSON 仅传偏移及身份。
#[cfg(test)]
pub(crate) fn vector_page_binary(arguments: &Value) -> (Value, Vec<u8>) {
    let boundary_before = super::sqlite_connections::boundary_counts();
    let mut bytes = Vec::new();
    let result = (|| -> ContractResult {
        let root = Path::new(arguments["root"].as_str().unwrap_or(""));
        let path = Path::new(arguments["path"].as_str().unwrap_or(""));
        let connection = super::sqlite_connections::acquire(
            root,
            path,
            arguments["timeoutMs"].as_u64().unwrap_or(120_000),
        )?;
        connection.execute_batch("BEGIN").map_err(failure)?;
        let mut authorized = arguments.clone();
        authorized["binaryRequiredScope"] = json!(true);
        let page = (|| -> ContractResult {
            let scopes = arguments["scopes"].as_array().cloned().unwrap_or_default();
            if scopes.is_empty() || scopes.len() > 1000 {
                return Err(ContractError::new(
                    "Vector scope is unauthorized. / 向量范围未授权。",
                    "RETRIEVAL_SOURCE_NOT_FOUND",
                    404,
                ));
            }
            let snapshot =
                super::sqlite_channels::snapshots(&connection, Some(&scopes))?["scopeSnapshots"]
                    .clone();
            let database_identity = json!(physical_identity_token(connection.database_identity()));
            if arguments
                .get("expectedScopeSnapshots")
                .is_some_and(|expected| *expected != snapshot)
                || arguments
                    .get("expectedDatabaseIdentity")
                    .is_some_and(|expected| *expected != database_identity)
            {
                return Err(ContractError::new(
                    "Vector source changed between pages. / 向量来源在分页期间已变化。",
                    "RETRIEVAL_ANN_STALE_BUILD",
                    409,
                ));
            }
            let mut items = Vec::new();
            if let Some((sql, parameters)) = vector_page_query(&connection, &authorized)? {
                let mut statement = connection.prepare_cached(&sql).map_err(failure)?;
                let mut cursor = statement
                    .query(params_from_iter(parameters))
                    .map_err(failure)?;
                while let Some(row) = cursor.next().map_err(failure)? {
                    cancelled()?;
                    let mut metadata = serde_json::Map::new();
                    let mut vector = None;
                    for index in 0..row.as_ref().column_count() {
                        let name = row.as_ref().column_name(index).map_err(failure)?.to_owned();
                        let value = match row.get_ref(index).map_err(failure)? {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(value) => json!(value),
                            ValueRef::Real(value) => json!(value),
                            ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
                            ValueRef::Blob(value) if name == "vector" => {
                                vector = Some(value);
                                continue;
                            }
                            ValueRef::Blob(_) => return Err(failure("unexpected binary column")),
                        };
                        metadata.insert(name, value);
                    }
                    if arguments["metadataOnly"] != true {
                        let vector = vector.ok_or_else(|| failure("missing vector blob"))?;
                        let dimensions = metadata["dimensions"].as_u64().unwrap_or(0);
                        if dimensions == 0
                            || dimensions > 4096
                            || vector.len() != dimensions as usize * 4
                        {
                            return Err(ContractError::new(
                                "Stored vector dimensions do not match. / 已存向量维度不匹配。",
                                "INVALID_RETRIEVAL_INPUT",
                                400,
                            ));
                        }
                        metadata.insert("vectorOffset".into(), json!(bytes.len()));
                        metadata.insert("vectorBytes".into(), json!(vector.len()));
                        bytes.extend_from_slice(vector);
                    }
                    items.push(Value::Object(metadata));
                }
            }
            Ok(
                json!({"items":items,"scopeSnapshots":snapshot,"databaseIdentity":database_identity}),
            )
        })();
        finish(&connection, page)
    })();
    match result {
        Ok(value) => (
            json!({"value":value,"nativeAudit":super::sqlite_connections::boundary_audit(boundary_before)}),
            bytes,
        ),
        Err(error) => (
            json!({"error":{"code":error.code,"message":error.message,"statusCode":error.status_code}}),
            Vec::new(),
        ),
    }
}

#[cfg(test)]
mod binary_identity_tests {
    use super::*;
    #[test]
    fn filesystem_identity_retains_values_above_javascript_integer_precision() {
        let token = physical_identity_token((u64::MAX, (1u64 << 53) + 1));
        assert_eq!(token, "ffffffffffffffff:0020000000000001");
        let wire = serde_json::to_string(&json!({"databaseIdentity":token})).unwrap();
        let decoded: Value = serde_json::from_str(&wire).unwrap();
        let (volume, file) = decoded["databaseIdentity"]
            .as_str()
            .unwrap()
            .split_once(':')
            .unwrap();
        assert_eq!(u64::from_str_radix(volume, 16).unwrap(), u64::MAX);
        assert_eq!(u64::from_str_radix(file, 16).unwrap(), (1u64 << 53) + 1);
    }
}

fn ensure_vector_extension(
    connection: &Connection,
    arguments: &Value,
) -> Result<(), ContractError> {
    if connection
        .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
        .is_ok()
    {
        return Ok(());
    }
    let path = Path::new(arguments["extensionPath"].as_str().unwrap_or(""));
    let parent = path
        .parent()
        .ok_or_else(|| failure("missing packaged vector extension"))?;
    let (package, filename) = ("sqlite-vec-windows-x64", "vec0.dll");
    let legacy_layout = parent.file_name().and_then(|name| name.to_str()) == Some(package);
    let native_layout = parent.file_name().and_then(|name| name.to_str()) == Some("native")
        && parent
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("runtime");
    if path.file_name().and_then(|name| name.to_str()) != Some(filename)
        || !(legacy_layout || native_layout)
        || !inspect_path(parent, path, false, true)?
    {
        return Err(ContractError::new(
            "Packaged vector extension is unavailable. / 随包向量扩展不可用。",
            "RETRIEVAL_VECTOR_UNAVAILABLE",
            503,
        ));
    }
    // The gateway supplies its bundled runtime/native path; preserve legacy packages without accepting arbitrary tool paths.
    // 网关提供随包 runtime/native 路径，兼容旧包布局但不接受任意工具路径；加载后立即撤销扩展加载能力。
    unsafe {
        let _guard = LoadExtensionGuard::new(connection).map_err(failure)?;
        connection
            .load_extension(path, None::<&str>)
            .map_err(failure)?;
    }
    Ok(())
}

pub fn extension_info(connection: &Connection, arguments: &Value) -> ContractResult {
    ensure_vector_extension(connection, arguments)?;
    let version: String = connection
        .prepare_cached("SELECT vec_version()")
        .map_err(failure)?
        .query_row([], |row| row.get(0))
        .map_err(failure)?;
    Ok(json!({"version":version,"owner":"rust-sqlite"}))
}

/// Authorize and rank vectors in one SQLite snapshot without exporting the full vector store.
/// 授权与向量排序在同一 SQLite 快照完成，不搬运整个向量库。
pub fn exact_search(connection: &Connection, arguments: &Value) -> ContractResult {
    exact_search_in_snapshot(connection, arguments, false)
}

pub(crate) fn exact_search_in_snapshot(
    connection: &Connection,
    arguments: &Value,
    verified_generation: bool,
) -> ContractResult {
    let table = vector_table(connection, arguments)?;
    let descriptor = &arguments["descriptor"];
    let scopes = arguments["scopes"].as_array().cloned().unwrap_or_default();
    if !scopes.contains(&descriptor["scope_key"]) {
        return Err(ContractError::new(
            "Vector scope is unauthorized. / 向量范围未授权。",
            "RETRIEVAL_SOURCE_NOT_FOUND",
            404,
        ));
    }
    if !verified_generation {
        check_generation(connection, descriptor)?;
    }
    let query = arguments["queryVector"]
        .as_array()
        .filter(|query| !query.is_empty() && query.len() <= 4096)
        .ok_or_else(|| failure("invalid query vector"))?;
    if descriptor["dimensions"].as_u64() != Some(query.len() as u64) {
        return Err(ContractError::new(
            "Vector dimensions do not match. / 查询向量维度不匹配。",
            "INVALID_RETRIEVAL_INPUT",
            400,
        ));
    }
    let mut bytes = Vec::with_capacity(query.len() * 4);
    for component in query {
        let value = component
            .as_f64()
            .filter(|value| value.is_finite() && (*value as f32).is_finite())
            .ok_or_else(|| failure("invalid query vector component"))? as f32;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    ensure_vector_extension(connection, arguments)?;
    let limit = arguments["limit"].as_u64().unwrap_or(40).clamp(1, 160);
    let mut parameters = vec![SqlValue::Blob(bytes)];
    parameters.extend(identity_values(descriptor));
    parameters.extend([
        SqlValue::Integer(descriptor["minimumId"].as_i64().unwrap_or(0)),
        SqlValue::Integer(
            descriptor["maximumId"]
                .as_i64()
                .unwrap_or(9_007_199_254_740_991),
        ),
        SqlValue::Integer(limit as i64),
    ]);
    let selected = rows(connection, &format!("WITH distances AS MATERIALIZED (
        SELECT c.id,c.chunk_id,vec_distance_cosine(c.vector,?) AS distance FROM {table} c JOIN sources s ON s.source_id=c.source_id
        WHERE {} AND c.id>=? AND c.id<=?
    ), ranked AS MATERIALIZED (SELECT id,chunk_id,distance FROM distances
        WHERE distance BETWEEN -1.7976931348623157e308 AND 1.7976931348623157e308 ORDER BY distance,chunk_id LIMIT ?),
    invalid AS (SELECT count(*) AS count FROM distances
        WHERE distance IS NULL OR distance NOT BETWEEN -1.7976931348623157e308 AND 1.7976931348623157e308)
    SELECT ranked.id,ranked.chunk_id,ranked.distance,invalid.count AS invalid FROM ranked CROSS JOIN invalid
    UNION ALL SELECT NULL,NULL,NULL,invalid.count FROM invalid WHERE NOT EXISTS(SELECT 1 FROM ranked)", identity_predicate()), parameters)?;
    let invalid = selected
        .first()
        .map_or(0, |row| row["invalid"].as_u64().unwrap_or(0));
    let mut items = Vec::new();
    for mut row in selected {
        if row["id"].is_null() {
            continue;
        }
        row.as_object_mut().expect("SQLite row").remove("invalid");
        items.push(row);
    }
    Ok(json!({"items":items,"invalid":invalid}))
}

pub fn verify_matches(
    connection: &Connection,
    arguments: &Value,
    scopes: &[Value],
) -> ContractResult {
    let table = vector_table(connection, arguments)?;
    let matches = arguments["matches"]
        .as_array()
        .filter(|matches| matches.len() <= 160)
        .ok_or_else(|| failure("invalid vector matches"))?;
    if matches.is_empty() {
        return Ok(json!({"items":[],"mismatch":false}));
    }
    let generations = rows(
        connection,
        &format!(
            "SELECT scope_key,generation FROM scope_snapshots WHERE scope_key IN ({})",
            placeholders(scopes)
        ),
        values(scopes),
    )?;
    let generations: HashMap<String, Value> = generations
        .into_iter()
        .map(|row| (text(&row, "scope_key"), row["generation"].clone()))
        .collect();
    let mut parameters = Vec::new();
    let mut eligible = Vec::new();
    let mut mismatch = false;
    for candidate in matches {
        cancelled()?;
        let descriptor = &candidate["identity"];
        if !scopes.contains(&descriptor["scope_key"]) {
            mismatch = true;
            continue;
        }
        if descriptor.get("generation").is_some()
            && generations
                .get(&text(descriptor, "scope_key"))
                .unwrap_or(&Value::Null)
                != &descriptor["generation"]
        {
            mismatch = true;
            continue;
        }
        parameters.extend([
            SqlValue::Integer(eligible.len() as i64),
            number(&candidate["id"]),
        ]);
        parameters.extend(identity_values(descriptor));
        parameters.extend([
            SqlValue::Integer(descriptor["minimumId"].as_i64().unwrap_or(0)),
            SqlValue::Integer(
                descriptor["maximumId"]
                    .as_i64()
                    .unwrap_or(9_007_199_254_740_991),
            ),
        ]);
        parameters.push(SqlValue::Real(
            candidate["distance"].as_f64().unwrap_or(f64::NAN),
        ));
        eligible.push(candidate);
    }
    if eligible.is_empty() {
        return Ok(json!({"items":[],"mismatch":mismatch}));
    }
    let joined = rows(connection, &format!("WITH requested(position,id,scope_key,profile,dimensions,model,space,domain,minimum_id,maximum_id,distance) AS (VALUES {})
        SELECT {},m.position AS match_position,m.distance FROM requested m JOIN {table} c ON c.id=m.id
        JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key=m.scope_key AND c.embedding_profile_id=m.profile AND c.dimensions=m.dimensions
        AND c.embedding_model_version=m.model AND c.embedding_space_id=m.space AND {DOMAIN_SQL}=m.domain
        AND c.id>=m.minimum_id AND c.id<=m.maximum_id AND c.vector IS NOT NULL ORDER BY m.position",
        vec!["(?,?,?,?,?,?,?,?,?,?,?)"; eligible.len()].join(","),
        if arguments["metadataOnly"] == true {
            format!("{},c.embedding_profile_id,c.dimensions,c.embedding_model_version,c.embedding_space_id", super::sqlite_channels::candidate_columns())
        } else { chunk_columns() }), parameters)?;
    if joined.len() != eligible.len() {
        mismatch = true;
    }
    let mut result = Vec::new();
    for mut row in joined {
        if row["embedding_profile_id"] != arguments["profile"]
            || row["dimensions"] != arguments["dimensions"]
            || arguments["modelProvided"] == true
                && row["embedding_model_version"] != arguments["model"]
            || arguments["spaceProvided"] == true && row["embedding_space_id"] != arguments["space"]
        {
            mismatch = true;
            continue;
        }
        row.as_object_mut()
            .expect("SQLite row")
            .remove("match_position");
        result.push(row);
    }
    Ok(json!({"items":result,"mismatch":mismatch}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_pages_seek_ids_and_preserve_selected_vector_space() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(super::super::sqlite_schema::RETRIEVAL_SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::sqlite_schema::VECTOR_SPACES_SCHEMA)
            .unwrap();
        let projection = {
            let mut statement = connection.prepare("PRAGMA table_info(chunks)").unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .map(|column| {
                    let column = column.unwrap();
                    let alias = if [
                        "embedding_profile_id",
                        "dimensions",
                        "vector",
                        "embedding_model_version",
                        "embedding_space_id",
                    ]
                    .contains(&column.as_str())
                    {
                        "v"
                    } else {
                        "c"
                    };
                    format!("{alias}.{column}")
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        connection
            .execute_batch(&format!("CREATE VIEW retrieval_vector_rows AS SELECT {projection} FROM chunks c JOIN chunk_vector_spaces v ON v.chunk_id=c.chunk_id"))
            .unwrap();
        for (source, scope) in [("visible", "project:one"), ("hidden", "project:other")] {
            connection.execute("INSERT INTO sources(source_id,scope_key,source_type,title,locator,text,content_hash,source_revision,binding_revision,active_generation,updated_at) VALUES (?,?,'document','fixture','{}','','hash','1',1,1,'now')", [source, scope]).unwrap();
            connection
                .execute(
                    "INSERT INTO scope_snapshots VALUES (?,7)",
                    [scope],
                )
                .unwrap();
        }
        let old_vector: Vec<u8> = [0.0f32, 1.0, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let mut expected_ids = Vec::new();
        for id in 1..=540i64 {
            let source = if id % 17 == 0 { "hidden" } else { "visible" };
            let domain = if id % 11 == 0 { "code" } else { "knowledge" };
            let model = if id % 13 == 0 { "other-model" } else { "new-model" };
            let chunk_id = format!("chunk-{id}");
            connection.execute("INSERT INTO chunks(id,chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,embedding_profile_id,dimensions,vector,chunker_version,tokenizer_version,embedding_model_version,embedding_space_id,structure_domain) VALUES (?,?,?,?,'','','hash',0,0,1,1,'old-profile',3,?,'v1','v1','old-model','old-space',?)", rusqlite::params![id, chunk_id, source, id, old_vector, domain]).unwrap();
            connection.execute("INSERT INTO chunk_vector_spaces VALUES (?,'old-profile',3,?,'old-model','old-space','now')", rusqlite::params![chunk_id, old_vector]).unwrap();
            let selected_vector: Vec<u8> = [id as f32, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect();
            connection.execute("INSERT INTO chunk_vector_spaces VALUES (?,'new-profile',2,?,?,'new-space','now')", rusqlite::params![chunk_id, selected_vector, model]).unwrap();
            if (11..=530).contains(&id)
                && source == "visible"
                && domain == "knowledge"
                && model == "new-model"
            {
                expected_ids.push(id);
            }
        }
        let mut arguments = json!({
            "scopes":["project:one"],"binaryRequiredScope":true,"lastId":0,
            "descriptor":{"scope_key":"project:one","generation":7,
                "embedding_profile_id":"new-profile","dimensions":2,
                "embedding_model_version":"new-model","embedding_space_id":"new-space",
                "domain":"knowledge","minimumId":11,"maximumId":530}
        });
        let (sql, parameters) = vector_page_query(&connection, &arguments).unwrap().unwrap();
        let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan = statement
            .query_map(params_from_iter(parameters), |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter()
                .all(|step| !(step.contains("TEMP B-TREE") && step.contains("ORDER BY"))),
            "{plan:?}"
        );
        let reference_sql = format!("SELECT c.id,c.dimensions,c.vector FROM retrieval_vector_rows c JOIN sources s ON s.source_id=c.source_id WHERE {} AND c.id>? AND c.id>=? AND c.id<=? ORDER BY c.id LIMIT 256", identity_predicate());
        let mut actual_ids = Vec::new();
        loop {
            let (_, parameters) = vector_page_query(&connection, &arguments).unwrap().unwrap();
            let expected_page = rows(&connection, &reference_sql, parameters).unwrap();
            let actual_page = vector_page(&connection, &arguments).unwrap();
            assert_eq!(actual_page, json!(expected_page));
            let page = actual_page.as_array().unwrap();
            if page.is_empty() {
                break;
            }
            actual_ids.extend(page.iter().map(|row| row["id"].as_i64().unwrap()));
            arguments["lastId"] = page.last().unwrap()["id"].clone();
        }
        assert!(expected_ids.len() > 256);
        assert_eq!(actual_ids, expected_ids);
        arguments["lastId"] = json!(0);
        arguments["metadataOnly"] = json!(true);
        arguments["includeChunkId"] = json!(true);
        let metadata = vector_page(&connection, &arguments).unwrap();
        assert_eq!(metadata[0]["chunk_id"], format!("chunk-{}", expected_ids[0]));
        assert_eq!(metadata[0]["dimensions"], 2);
        assert!(metadata[0].get("vector").is_none());
        arguments["scopes"] = json!(["project:other"]);
        assert_eq!(vector_page(&connection, &arguments).unwrap_err().code, "RETRIEVAL_SOURCE_NOT_FOUND");
        arguments["scopes"] = json!(["project:one"]);
        arguments["descriptor"]["generation"] = json!(6);
        assert_eq!(vector_page(&connection, &arguments).unwrap_err().code, "RETRIEVAL_ANN_STALE_BUILD");
    }
}
