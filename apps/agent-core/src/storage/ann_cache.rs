use super::{ContractError, ContractResult, cancellation, managed_files};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static CACHE_TRANSACTIONS: Mutex<()> = Mutex::new(());

fn failure(message: impl Into<String>, code: &'static str) -> ContractError {
    ContractError::new(message, code, 409)
}
fn io_error(error: std::io::Error) -> ContractError {
    failure(
        format!("ANN cache IO failed. / 向量缓存文件操作失败：{error}"),
        "RETRIEVAL_ANN_CACHE_WRITE_FAILED",
    )
}
fn check_cancelled() -> Result<(), ContractError> {
    if cancellation::is_cancelled() {
        return Err(failure(
            "ANN cache operation cancelled. / 向量缓存操作已取消。",
            "ABORT_ERR",
        ));
    }
    Ok(())
}
fn key(value: &Value) -> Result<&str, ContractError> {
    value
        .as_str()
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| {
            failure(
                "Invalid ANN cache identity. / 向量缓存身份无效。",
                "INVALID_RETRIEVAL_ANN",
            )
        })
}
fn inspect(root: &Path, path: &Path, directory: bool) -> Result<bool, ContractError> {
    let exists = managed_files::inspect(root, path, false, !directory, u64::MAX).map_err(|_| {
        failure(
            "Unsafe ANN cache path. / 向量缓存包含链接或异常结构。",
            "UNSAFE_RETRIEVAL_PATH",
        )
    })?;
    if exists && !directory {
        let file = File::open(path).map_err(io_error)?;
        let identity = crate::file_identity::handle_identity(&file).map_err(io_error)?;
        if identity.links > 1 || identity.is_link {
            return Err(failure(
                "Unsafe ANN cache path. / 向量缓存包含链接或异常结构。",
                "UNSAFE_RETRIEVAL_PATH",
            ));
        }
    }
    Ok(exists)
}
fn paths(directory: &Path, key: &str) -> (PathBuf, PathBuf) {
    (
        directory.join(format!("{key}.json")),
        directory.join(format!("{key}.usearch")),
    )
}
fn graph_hash(path: &Path) -> Result<String, ContractError> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        check_cancelled()?;
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn temporary(directory: &Path, key: &str, value: &Value) -> Result<PathBuf, ContractError> {
    let leaf = value.as_str().unwrap_or("");
    let prefix = format!("{key}.usearch.");
    let identifier = leaf
        .strip_prefix(&prefix)
        .and_then(|leaf| leaf.strip_suffix(".tmp"))
        .unwrap_or("");
    if identifier.len() != 36
        || !identifier
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(failure(
            "Invalid owned ANN temporary name. / 受管向量临时文件名无效。",
            "INVALID_RETRIEVAL_ANN",
        ));
    }
    Ok(directory.join(leaf))
}

/// SDK graph bytes remain opaque. Native storage owns verification, graph/manifest publication and pruning.
/// SDK 图字节保持不透明；原生存储拥有核验、图及清单发布，以及预算清理。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let _guard = CACHE_TRANSACTIONS.lock().map_err(|_| {
        failure(
            "ANN cache lock failed. / 向量缓存锁失效。",
            "RETRIEVAL_ANN_CACHE_WRITE_FAILED",
        )
    })?;
    check_cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let directory = root.join("ann");
    inspect(root, &directory, true)?;
    if operation == "storage.ann.prepare" {
        managed_files::inspect(root, &directory, true, false, u64::MAX)?;
        return Ok(json!({"directory": directory.to_string_lossy()}));
    }
    if operation == "storage.ann.retireChat" {
        let scope = arguments["scopeKey"].as_str().unwrap_or("");
        let id = scope.strip_prefix("chat:").ok_or_else(|| {
            failure(
                "Invalid retired chat scope. / 删除聊天范围无效。",
                "INVALID_RETRIEVAL_ANN",
            )
        })?;
        super::identity(&json!(id))?;
        if !inspect(root, &directory, true)? {
            return Ok(json!({"removed":0}));
        }
        let mut removed = 0;
        let mut skipped = 0;
        for entry in fs::read_dir(&directory).map_err(io_error)? {
            check_cancelled()?;
            let leaf = entry
                .map_err(io_error)?
                .file_name()
                .to_string_lossy()
                .into_owned();
            let Some(candidate) = leaf.strip_suffix(".json") else {
                continue;
            };
            if key(&json!(candidate)).is_err() {
                continue;
            }
            let (manifest, graph) = paths(&directory, candidate);
            let bytes = managed_files::read(root, &manifest, 65536)?
                .ok_or_else(|| io_error(std::io::Error::from(std::io::ErrorKind::NotFound)))?;
            let metadata: Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if metadata["identity"]["scopeKey"] != scope {
                continue;
            }
            // Validate both owned cache files before deleting either; no source or workspace path is consulted.
            // 两个自有缓存文件均核验后再删除；不读取或删除来源、工作目录、共享记录或Trash原文。
            let graph_exists = inspect(root, &graph, false)?;
            inspect(root, &manifest, false)?;
            if graph_exists {
                fs::remove_file(graph).map_err(io_error)?;
            }
            fs::remove_file(manifest).map_err(io_error)?;
            removed += 1;
        }
        return Ok(
            json!({"removed":removed,"skippedInvalidManifests":skipped,"complete":skipped==0}),
        );
    }
    if operation == "storage.ann.prune" {
        if !inspect(root, &directory, true)? {
            return Ok(json!({"removed": 0}));
        }
        let resident: HashSet<&str> = arguments["resident"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut entries = Vec::new();
        for entry in fs::read_dir(&directory).map_err(io_error)? {
            check_cancelled()?;
            let entry = entry.map_err(io_error)?;
            let leaf = entry.file_name().to_string_lossy().into_owned();
            let Some(candidate) = leaf.strip_suffix(".usearch") else {
                continue;
            };
            if key(&json!(candidate)).is_err() {
                continue;
            }
            let (manifest, graph) = paths(&directory, candidate);
            inspect(root, &manifest, false)?;
            inspect(root, &graph, false)?;
            let metadata = fs::metadata(&graph).map_err(io_error)?;
            entries.push((
                candidate.to_owned(),
                manifest,
                graph,
                metadata.len(),
                metadata.modified().map_err(io_error)?,
            ));
        }
        entries.sort_by(|left, right| right.4.cmp(&left.4));
        let maximum_shard = arguments["options"]["maxShardBytes"].as_u64().unwrap_or(0);
        let maximum_count = arguments["options"]["maxCachedShards"]
            .as_u64()
            .unwrap_or(0);
        let mut retained_bytes = 0u64;
        let mut retained_count = 0u64;
        let mut removed = 0;
        for (key, manifest, graph, size, _) in entries {
            if size <= maximum_shard
                && (resident.contains(key.as_str())
                    || retained_count < maximum_count * 4
                        && retained_bytes.saturating_add(size)
                            <= maximum_count.saturating_mul(maximum_shard))
            {
                retained_count += 1;
                retained_bytes += size;
                continue;
            }
            check_cancelled()?;
            if inspect(root, &manifest, false)? {
                fs::remove_file(manifest).map_err(io_error)?;
            }
            inspect(root, &graph, false)?;
            fs::remove_file(graph).map_err(io_error)?;
            removed += 1;
        }
        return Ok(json!({"removed": removed}));
    }
    let key = key(&arguments["key"])?;
    let (manifest, graph) = paths(&directory, key);
    let manifest_exists = inspect(root, &manifest, false)?;
    let graph_exists = inspect(root, &graph, false)?;
    if operation == "storage.ann.load" {
        if !manifest_exists || !graph_exists {
            return Ok(json!({"eligible": false}));
        }
        let manifest_bytes = managed_files::read(root, &manifest, 65536)
            .map_err(|_| {
                failure(
                    "Invalid ANN cache manifest. / 向量缓存清单无效。",
                    "RETRIEVAL_ANN_CACHE_READ_FAILED",
                )
            })?
            .unwrap();
        if fs::metadata(&graph).map_err(io_error)?.len()
            > arguments["maximumBytes"].as_u64().unwrap_or(0)
        {
            return Ok(json!({"eligible": false}));
        }
        let Ok(manifest_fields) = serde_json::from_slice::<
            std::collections::HashMap<String, Box<serde_json::value::RawValue>>,
        >(&manifest_bytes) else {
            return Ok(json!({"eligible": false}));
        };
        let Ok(metadata) = serde_json::from_str::<Value>(&super::raw_json::scalar_projection(
            &String::from_utf8_lossy(&manifest_bytes),
        )) else {
            return Ok(json!({"eligible": false}));
        };
        let expected_identity = arguments["identityRaw"].as_str().unwrap_or("");
        if manifest_fields
            .get("identity")
            .is_none_or(|identity| super::raw_json::compact(identity.get()) != expected_identity)
            || metadata["generation"] != arguments["generation"]
            || metadata["count"] != arguments["count"]
        {
            return Ok(json!({"eligible": false}));
        }
        Ok(
            json!({"eligible": metadata["graphHash"].as_str() == Some(graph_hash(&graph)?.as_str())}),
        )
    } else if operation == "storage.ann.publish" {
        let temporary = temporary(&directory, key, &arguments["temporary"])?;
        if !inspect(root, &temporary, false)? {
            return Err(failure(
                "ANN save produced no graph. / 向量保存没有产生图文件。",
                "RETRIEVAL_ANN_CACHE_WRITE_FAILED",
            ));
        }
        if fs::metadata(&temporary).map_err(io_error)?.len()
            > arguments["maximumBytes"].as_u64().unwrap_or(0)
        {
            return Err(failure(
                "ANN graph exceeds its disk budget. / 向量缓存超过单分片磁盘预算。",
                "RETRIEVAL_ANN_RESOURCE_LIMIT",
            ));
        }
        let identity_raw = arguments["identityRaw"].as_str().unwrap_or("");
        let identity: Value = serde_json::from_str(&super::raw_json::scalar_projection(
            identity_raw,
        ))
        .map_err(|_| {
            failure(
                "Invalid ANN identity. / 向量缓存身份无效。",
                "INVALID_RETRIEVAL_ANN",
            )
        })?;
        let raw = format!(
            "{{\"identity\":{identity_raw},\"generation\":{},\"count\":{},\"graphHash\":\"{}\"}}",
            arguments["generation"],
            arguments["count"],
            graph_hash(&temporary)?
        );
        let temporary_manifest = temporary.with_extension("manifest.tmp");
        inspect(root, &temporary_manifest, false)?;
        let mut handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_manifest)
            .map_err(io_error)?;
        let result = (|| {
            handle.write_all(raw.as_bytes()).map_err(io_error)?;
            handle.sync_all().map_err(io_error)?;
            drop(handle);
            // Bind disposable publication to the actual scope generation while preventing a writer race.
            // 可重建缓存发布绑定实际范围版本，并在文件发布期间阻止正式写入竞态。
            let index_path = root.join("retrieval.sqlite");
            let generation_guard = if managed_files::inspect(
                root,
                &index_path,
                false,
                true,
                u64::MAX,
            )? {
                let connection =
                    super::sqlite_connections::acquire_write(root, &index_path, 120_000)?;
                connection
                    .execute_batch("BEGIN IMMEDIATE")
                    .map_err(super::sqlite_store::failure)?;
                let current: Option<i64> = connection
                    .prepare_cached("SELECT generation FROM scope_snapshots WHERE scope_key=?")
                    .map_err(super::sqlite_store::failure)?
                    .query_row([identity["scopeKey"].as_str().unwrap_or("")], |row| {
                        row.get(0)
                    })
                    .ok();
                if current != arguments["generation"].as_i64() {
                    return Err(failure(
                        "ANN cache generation changed during save. / 向量缓存保存期间范围版本已变化。",
                        "RETRIEVAL_ANN_STALE_BUILD",
                    ));
                }
                Some(connection)
            } else {
                None
            };
            inspect(root, &graph, false)?;
            inspect(root, &manifest, false)?;
            check_cancelled()?;
            fs::rename(&temporary, &graph).map_err(io_error)?;
            fs::rename(&temporary_manifest, &manifest).map_err(io_error)?;
            drop(generation_guard);
            Ok(json!({"published": true}))
        })();
        if temporary_manifest.exists() {
            fs::remove_file(temporary_manifest).map_err(io_error)?;
        }
        result
    } else if operation == "storage.ann.discardTemporary" {
        let temporary = temporary(&directory, key, &arguments["temporary"])?;
        if inspect(root, &temporary, false)? {
            fs::remove_file(temporary).map_err(io_error)?;
        }
        Ok(Value::Null)
    } else {
        Err(failure(
            "Unknown ANN cache operation. / 未知向量缓存操作。",
            "INVALID_RETRIEVAL_ANN",
        ))
    }
}
