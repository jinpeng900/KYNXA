#![cfg(test)]

use super::{cancellation, checkpoints, managed_files};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[test]
fn a_request_keeps_cancellation_between_native_transactions() {
    let id = format!(
        "native-cancel:{}",
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    cancellation::retain(&id).unwrap();
    {
        let first = cancellation::CancellationLease::acquire(&id).unwrap();
        cancellation::cancel(&id).unwrap();
        let _context = first.enter();
        assert!(cancellation::is_cancelled());
    }
    let second = cancellation::CancellationLease::acquire(&id).unwrap();
    let context = second.enter();
    assert!(cancellation::is_cancelled());
    cancellation::release(&id).unwrap();
    assert!(cancellation::is_cancelled());
    drop(context);
    drop(second);
    let independent = cancellation::CancellationLease::acquire(&id).unwrap();
    let _context = independent.enter();
    assert!(!cancellation::is_cancelled());
}
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "kynxa-native-storage-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }
    fn path(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.root.starts_with(std::env::temp_dir()));
        super::sqlite_connections::close(&self.root, &self.root.join("Index/retrieval.sqlite"))
            .unwrap();
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn atomic_commit_preserves_original_json_and_rejects_stale_receipts() {
    let fixture = Fixture::new();
    let path = fixture.root.join("Memory").join("entries.json");
    let original = "{\"revision\":1,\"content\":\"before\\ud800after\"}";
    let receipt = managed_files::apply(
        "storage.files.commit",
        &json!({"root": fixture.path(), "path": path, "raw": original, "expectedHash": null}),
    )
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
    let rejected = managed_files::apply(
        "storage.files.commit",
        &json!({"root": fixture.path(), "path": path, "raw": "{}", "expectedHash": "0".repeat(64)}),
    )
    .err()
    .unwrap();
    assert_eq!(rejected.code, "MEMORY_CONFLICT");
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
    managed_files::apply("storage.files.commit", &json!({"root": fixture.path(), "path": path, "raw": "{\"revision\":2}", "expectedHash": receipt["hash"]})).unwrap();
}

#[test]
fn cancellation_before_commit_preserves_the_existing_file() {
    let fixture = Fixture::new();
    let path = fixture.root.join("scope.json");
    fs::write(&path, "old").unwrap();
    let id = format!("fixture:{}", fixture.path().len());
    let lease = cancellation::CancellationLease::acquire(&id).unwrap();
    let context = lease.enter();
    cancellation::cancel(&id).unwrap();
    let error = managed_files::apply(
        "storage.files.commit",
        &json!({"root": fixture.path(), "path": path, "raw": "new"}),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "ABORT_ERR");
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    drop(context);
    drop(lease);
}

fn source_raw(identity: &str) -> String {
    format!(
        "{{\"sourceId\":\"{identity}\",\"inputSignature\":\"{}\",\"fingerprint\":\"{}\",\"semantic\":false,\"preparationVersion\":\"one\",\"derivationSignature\":\"{}\",\"embeddingInputSignature\":\"{}\",\"chunkCount\":1,\"vectorChunks\":0,\"embeddingProfileId\":null,\"embeddingModelVersion\":null,\"embeddingSpaceId\":null,\"vectorDimensions\":null}}",
        "a".repeat(64),
        "b".repeat(64),
        "c".repeat(64),
        "d".repeat(64)
    )
}

#[test]
fn checkpoint_recovers_original_property_order_and_only_repairs_a_torn_tail() {
    let fixture = Fixture::new();
    let checkpoint_id = "e".repeat(64);
    let first = source_raw("scalar\\ud800");
    let second = source_raw("scalar�");
    let payload = format!(
        "{{\"jobId\":\"fixture-job\",\"checkpointId\":\"{checkpoint_id}\",\"sources\":[{first},{second}]}}"
    );
    checkpoints::apply(
        "storage.checkpoints.append",
        &json!({"root": fixture.path(), "jobId": "fixture-job", "payload": payload}),
    )
    .unwrap();
    let path = fixture
        .root
        .join("Retrieval")
        .join("checkpoints")
        .join("fixture-job.jsonl");
    let committed = fs::read(&path).unwrap();
    let mut torn = committed.clone();
    torn.extend_from_slice(b"{\"uncommitted\":");
    fs::write(&path, torn).unwrap();
    let timestamp = "2026-10-11T00:00:00.000Z";
    let arguments = json!({"root": fixture.path(), "jobId": "fixture-job", "checkpoint": {"version": 1, "kind": "source-index", "checkpointId": checkpoint_id, "settingsSignature": "f".repeat(64), "root": null, "bindingRevision": 0, "preparationVersion": null, "updatedAt": timestamp}, "timestamps": {timestamp: true}});
    let sources = checkpoints::apply("storage.checkpoints.read", &arguments).unwrap();
    assert_eq!(sources, json!([first, second]));
    assert_eq!(fs::read(&path).unwrap(), committed);
    let mut corrupted = fs::read_to_string(&path).unwrap();
    corrupted = corrupted.replace("\"chunkCount\":1", "\"chunkCount\":2");
    fs::write(&path, &corrupted).unwrap();
    assert_eq!(
        checkpoints::apply("storage.checkpoints.read", &arguments)
            .err()
            .unwrap()
            .code,
        "INVALID_RETRIEVAL_CHECKPOINT"
    );
    assert_eq!(fs::read_to_string(path).unwrap(), corrupted);
}

#[test]
fn scalar_projection_does_not_rewrite_literal_backslashes() {
    let raw = r#"{"quoted":"\\ud800","unpaired":"\ud800","pair":"\ud83d\ude00"}"#;
    let value: Value = serde_json::from_str(&super::raw_json::scalar_projection(raw)).unwrap();
    assert_eq!(value["quoted"], "\\ud800");
    assert_eq!(value["unpaired"], "�");
    assert_eq!(value["pair"], "😀");
}

#[test]
fn conversation_tail_recovery_and_role_guards_preserve_authoritative_bytes() {
    let fixture = Fixture::new();
    let location = json!({"Id": "chat-fixture", "ProjectId": null});
    let raw = r#"{"Id":"message-fixture","Role":"user","Content":"raw\ud800"}"#;
    let receipt = super::conversation_logs::apply("storage.logs.append", &json!({"root": fixture.path(), "location": location, "expectedHash": null, "rawMessage": raw})).unwrap();
    let path = fixture.root.join("Chats/chat-fixture/events.jsonl");
    let original = fs::read(&path).unwrap();
    let role_error = super::conversation_logs::apply("storage.logs.append", &json!({"root": fixture.path(), "location": location, "expectedHash": receipt["hash"], "rawMessage": r#"{"Id":"message-fixture","Role":"assistant"}"#})).unwrap_err();
    assert_eq!(role_error.code, "INVALID_CONVERSATION_DATA");
    assert_eq!(fs::read(&path).unwrap(), original);
    let mut torn = original.clone();
    torn.extend_from_slice(b"{\"crashed\":");
    fs::write(&path, &torn).unwrap();
    let result = super::conversation_logs::apply("storage.logs.finalize", &json!({"root": fixture.path(), "location": location, "expectedHash": managed_files::digest(&torn), "exists": true, "invalidLine": 1})).unwrap();
    assert_eq!(result["selected"], json!([0]));
    assert_eq!(fs::read(&path).unwrap(), original);
    let recovered = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains("recovered-tail")
        })
        .unwrap();
    assert_eq!(fs::read(recovered.path()).unwrap(), b"{\"crashed\":");
}

#[test]
fn cached_conversation_reads_verify_bytes_without_returning_unchanged_bodies() {
    let fixture = Fixture::new();
    let location = json!({"Id": "chat-cache", "ProjectId": null});
    let folder = fixture.root.join("Chats/chat-cache");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("events.jsonl");
    let original = b"{\"version\":1,\"type\":\"message.upsert\",\"message\":{\"Id\":\"message-one\",\"Role\":\"user\",\"Content\":\"old\"}}\n";
    fs::write(&path, original).unwrap();
    let arguments = json!({"root": fixture.path(), "location": location});
    let first = super::conversation_logs::apply("storage.logs.read", &arguments).unwrap();
    assert_eq!(first["lines"].as_array().unwrap().len(), 1);
    let mut cached_arguments = arguments.clone();
    cached_arguments["knownHash"] = first["hash"].clone();
    let cached = super::conversation_logs::apply("storage.logs.read", &cached_arguments).unwrap();
    assert_eq!(cached["unchanged"], true);
    assert_eq!(cached["hash"], first["hash"]);
    assert!(cached.get("lines").is_none());
    assert_eq!(fs::read(&path).unwrap(), original);

    // Same-sized edits must invalidate the byte receipt; metadata equality cannot authorize reuse.
    // 相同长度的修改仍使字节回执失效，元信息相同不能授权复用旧正文。
    let changed = String::from_utf8(original.to_vec())
        .unwrap()
        .replace("old", "new");
    fs::write(&path, &changed).unwrap();
    let refreshed =
        super::conversation_logs::apply("storage.logs.read", &cached_arguments).unwrap();
    assert_ne!(refreshed["hash"], first["hash"]);
    assert_ne!(refreshed["unchanged"], true);
    assert_eq!(refreshed["lines"][0], changed.trim_end());
    fs::remove_file(&path).unwrap();
    let missing = super::conversation_logs::apply("storage.logs.read", &cached_arguments).unwrap();
    assert_eq!(missing["exists"], false);
    assert_eq!(missing["hash"], Value::Null);
    assert_eq!(missing["lines"], json!([]));
}

#[test]
fn conversation_ordering_borrows_large_utf16_bodies_and_preserves_damage_guards() {
    let fixture = Fixture::new();
    let location = json!({"Id": "chat-large", "ProjectId": null});
    let body = "正文😀".repeat(65_536);
    let raw = format!(
        "{{\"Id\":\"MESSAGE-one\",\"Role\":\"user\",\"Content\":\"{body}\\ud800\\\\ud800\",\"Extra\":{{\"ignored\":\"\\udfff\"}}}}"
    );
    let first = super::conversation_logs::apply(
        "storage.logs.append",
        &json!({"root": fixture.path(), "location": location, "expectedHash": null, "rawMessage": raw}),
    ).unwrap();
    let second = super::conversation_logs::apply(
        "storage.logs.append",
        &json!({"root": fixture.path(), "location": location, "expectedHash": first["hash"], "rawMessage": r#"{"Id":"message-one","Role":"user","Content":"latest","\ud800":"legacy-key"}"#}),
    ).unwrap();
    let path = fixture.root.join("Chats/chat-large/events.jsonl");
    let original = fs::read(&path).unwrap();
    assert!(String::from_utf8_lossy(&original).contains("\\ud800\\\\ud800"));
    let result = super::conversation_logs::apply(
        "storage.logs.finalize",
        &json!({"root": fixture.path(), "location": location, "expectedHash": second["hash"], "exists": true, "invalidLine": null}),
    ).unwrap();
    assert_eq!(result["selected"], json!([1]));
    assert_eq!(fs::read(&path).unwrap(), original);

    let damaged = original
        .iter()
        .copied()
        .chain(b"{\"broken\":\n{}\n".iter().copied())
        .collect::<Vec<_>>();
    fs::write(&path, &damaged).unwrap();
    let error = super::conversation_logs::apply(
        "storage.logs.finalize",
        &json!({"root": fixture.path(), "location": location, "expectedHash": managed_files::digest(&damaged), "exists": true, "invalidLine": 2}),
    ).unwrap_err();
    assert_eq!(error.code, "CORRUPT_CONVERSATION");
    assert_eq!(fs::read(&path).unwrap(), damaged);
}

#[test]
fn cached_conversation_reads_do_not_bypass_cancellation_or_snapshot_conflicts() {
    let fixture = Fixture::new();
    let location = json!({"Id": "chat-cancel", "ProjectId": null});
    let raw = r#"{"Id":"message-one","Role":"user","Content":"kept"}"#;
    let receipt = super::conversation_logs::apply(
        "storage.logs.append",
        &json!({"root": fixture.path(), "location": location, "expectedHash": null, "rawMessage": raw}),
    ).unwrap();
    let conflict = super::conversation_logs::apply(
        "storage.logs.finalize",
        &json!({"root": fixture.path(), "location": location, "expectedHash": "0".repeat(64), "exists": true}),
    ).unwrap_err();
    assert_eq!(conflict.code, "CONVERSATION_LOG_CHANGED");
    let request_id = format!(
        "cached-read-cancel:{}:{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let lease = cancellation::CancellationLease::acquire(&request_id).unwrap();
    let _context = lease.enter();
    cancellation::cancel(&request_id).unwrap();
    let cancelled = super::conversation_logs::apply(
        "storage.logs.read",
        &json!({"root": fixture.path(), "location": location, "knownHash": receipt["hash"]}),
    )
    .unwrap_err();
    assert_eq!(cancelled.code, "ABORT_ERR");
    assert!(
        String::from_utf8_lossy(
            &fs::read(fixture.root.join("Chats/chat-cancel/events.jsonl")).unwrap()
        )
        .contains("kept")
    );
}

#[test]
fn catalog_publication_moves_whole_session_and_rejects_stale_catalog_receipts() {
    let fixture = Fixture::new();
    let location = json!({"Id": "chat-fixture", "ProjectId": null});
    super::conversation_logs::apply("storage.logs.append", &json!({"root": fixture.path(), "location": location, "expectedHash": null, "rawMessage": r#"{"Id":"message-fixture","Role":"user"}"#})).unwrap();
    let catalog_path = fixture.root.join("catalog.json");
    let original = r#"{"Version":1,"Revision":0,"Projects":[],"Chats":[],"Tombstones":[]}"#;
    fs::write(&catalog_path, original).unwrap();
    let next = json!({"Version": 1, "Revision": 1, "Projects": [], "Chats": [], "Tombstones": []});
    let transaction = json!({"Version": 1, "NextCatalog": next, "Moves": [{"From": location, "To": {"Id": "chat-fixture", "Trash": true}}], "Writes": []}).to_string();
    let stale = super::catalog_transactions::apply("storage.conversations.commit", &json!({"root": fixture.path(), "expectedHash": "0".repeat(64), "rawTransaction": transaction})).unwrap_err();
    assert_eq!(stale.code, "CATALOG_CONFLICT");
    assert!(
        fixture
            .root
            .join("Chats/chat-fixture/events.jsonl")
            .is_file()
    );
    let receipt = super::catalog_transactions::apply("storage.conversations.commit", &json!({"root": fixture.path(), "expectedHash": managed_files::digest(original.as_bytes()), "rawTransaction": transaction, "rawCatalog": next.to_string()})).unwrap();
    assert!(
        fixture
            .root
            .join("Trash/chat-fixture/events.jsonl")
            .is_file()
    );
    assert!(!fixture.root.join("Chats/chat-fixture").exists());
    assert_eq!(
        receipt["hash"],
        managed_files::digest(&fs::read(catalog_path).unwrap())
    );
    assert!(!fixture.root.join(".catalog-transaction.json").exists());
}

#[test]
fn ann_cache_checks_real_graph_hash_before_reuse() {
    let fixture = Fixture::new();
    super::ann_cache::apply("storage.ann.prepare", &json!({"root": fixture.path()})).unwrap();
    let key = "a".repeat(64);
    let leaf = format!("{key}.usearch.00000000-0000-0000-0000-000000000000.tmp");
    fs::write(fixture.root.join("ann").join(&leaf), "opaque-sdk-graph").unwrap();
    let arguments = json!({"root": fixture.path(), "key": key, "temporary": leaf, "identityRaw": r#"{"schemaVersion":1,"scopeKey":"project:fixture"}"#, "generation": 3, "count": 2, "maximumBytes": 1024});
    super::ann_cache::apply("storage.ann.publish", &arguments).unwrap();
    assert_eq!(
        super::ann_cache::apply("storage.ann.load", &arguments).unwrap()["eligible"],
        true
    );
    fs::write(
        fixture.root.join("ann").join(format!("{key}.usearch")),
        "tampered-sdk-graph",
    )
    .unwrap();
    assert_eq!(
        super::ann_cache::apply("storage.ann.load", &arguments).unwrap()["eligible"],
        false
    );
}

#[test]
fn library_rollback_requires_its_exact_owned_snapshot_receipt() {
    let fixture = Fixture::new();
    let arguments =
        json!({"root": fixture.path(), "id": "source-fixture", "text": "imported-source"});
    let receipt = super::library::apply("storage.library.stage", &arguments).unwrap();
    let wrong = super::library::apply(
        "storage.library.discard",
        &json!({"root": fixture.path(), "id": "source-fixture", "hash": "0".repeat(64)}),
    )
    .unwrap_err();
    assert_eq!(wrong.code, "STALE_RETRIEVAL_SOURCE");
    assert_eq!(
        super::library::apply("storage.library.readSnapshot", &arguments).unwrap()["raw"],
        "imported-source"
    );
    super::library::apply(
        "storage.library.discard",
        &json!({"root": fixture.path(), "id": "source-fixture", "hash": receipt["hash"]}),
    )
    .unwrap();
    assert_eq!(
        super::library::apply("storage.library.readSnapshot", &arguments).unwrap()["exists"],
        false
    );
}

#[test]
fn managed_reads_reject_hard_links_without_modifying_either_name() {
    let fixture = Fixture::new();
    let original = fixture.root.join("original.json");
    let alias = fixture.root.join("alias.json");
    fs::write(&original, "protected").unwrap();
    fs::hard_link(&original, &alias).unwrap();
    let error = managed_files::read(&fixture.root, &alias, 1024).unwrap_err();
    assert_eq!(error.code, "INVALID_MEMORY_PATH");
    assert_eq!(fs::read_to_string(original).unwrap(), "protected");
}

#[test]
fn extension_layout_checks_future_versions_and_duplicate_keys_before_effects() {
    let fixture = Fixture::new();
    let root = fixture.root.join("Extensions");
    fs::create_dir(&root).unwrap();
    let metadata = root.join("extension-layout.json");
    for (document, expected) in [
        (r#"{"version":2}"#, "UNSUPPORTED_EXTENSION_LAYOUT"),
        (r#"{"version":0,"version":1}"#, "INVALID_EXTENSION_LAYOUT"),
    ] {
        fs::write(&metadata, document).unwrap();
        let error = super::extensions::apply("storage.extensions.prepare", &json!({"root": root}))
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert!(!root.join("Agent").exists());
        assert_eq!(fs::read_to_string(&metadata).unwrap(), document);
    }
    fs::remove_file(&metadata).unwrap();
    let plan =
        super::extensions::apply("storage.extensions.prepare", &json!({"root": root})).unwrap();
    for path in plan.as_array().unwrap() {
        super::extensions::apply(
            "storage.extensions.directory",
            &json!({"root": root, "path": path}),
        )
        .unwrap();
    }
    let receipt =
        super::extensions::apply("storage.extensions.stage", &json!({"root": root})).unwrap();
    assert!(!metadata.exists());
    assert_eq!(
        super::extensions::apply(
            "storage.extensions.publish",
            &json!({"root": root,"receipt":receipt})
        )
        .unwrap(),
        json!({"version":1})
    );
    super::extensions::apply(
        "storage.extensions.discard",
        &json!({"root":root,"receipt":receipt}),
    )
    .unwrap();
    fs::remove_file(&metadata).unwrap();
    let receipt =
        super::extensions::apply("storage.extensions.stage", &json!({"root":root})).unwrap();
    fs::write(&metadata, r#"{"version":99,"owner":"concurrent"}"#).unwrap();
    let error = super::extensions::apply(
        "storage.extensions.publish",
        &json!({"root":root,"receipt":receipt}),
    )
    .unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_EXTENSION_LAYOUT");
    super::extensions::apply(
        "storage.extensions.discard",
        &json!({"root":root,"receipt":receipt}),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&metadata).unwrap(),
        r#"{"version":99,"owner":"concurrent"}"#
    );
    assert!(!root.join(receipt["name"].as_str().unwrap()).exists());
}

#[test]
fn restarting_jobs_clears_optional_failure_fields_without_erasing_identity_or_progress() {
    let job = json!({"jobId":"fixture-job","projectId":null,"status":"paused",
        "completedSources":2,"totalSources":5,"createdAt":"2026-10-11T00:00:00Z",
        "error":"INDEX_JOB_INTERRUPTED","finishedAt":"2026-10-11T00:01:00Z"});
    let document = json!({"schemaVersion":1,"jobs":[job]});
    let arguments = json!({"document":document,"jobId":"fixture-job","patch":{"status":"running"},
        "undefinedFields":["error","finishedAt"],"timestamps":{}});
    let plan = super::jobs::apply("storage.jobs.update", &arguments).unwrap();
    let updated = &plan["document"]["jobs"][0];
    assert_eq!(updated["status"], "running");
    assert_eq!(updated["completedSources"], 2);
    assert!(updated.get("error").is_none());
    assert!(updated.get("finishedAt").is_none());
    for field in [
        "jobId",
        "projectId",
        "status",
        "completedSources",
        "automaticRebuildBlocked",
    ] {
        let mut invalid = arguments.clone();
        invalid["undefinedFields"] = json!([field]);
        assert_eq!(
            super::jobs::apply("storage.jobs.update", &invalid)
                .unwrap_err()
                .code,
            "INVALID_RETRIEVAL_JOB_UPDATE"
        );
    }
}

#[test]
fn concurrent_sqlite_readers_tolerate_retired_sidecars_and_still_reject_hard_links() {
    let fixture = Fixture::new();
    let path = fixture.root.join("Index/retrieval.sqlite");
    super::sqlite_bootstrap::apply(
        "storage.index.bootstrap",
        &json!({"root":fixture.root,
        "path":path,"epoch":"fixture-epoch"}),
    )
    .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut readers = Vec::new();
    for _ in 0..2 {
        let root = fixture.root.clone();
        let path = path.clone();
        let barrier = barrier.clone();
        readers.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..32 {
                super::sqlite_queries::apply(
                    "storage.index.scopeGeneration",
                    &json!({"root":root,"path":path,"scopeKey":"user"}),
                )
                .unwrap();
                std::thread::yield_now();
            }
        }));
    }
    barrier.wait();
    for reader in readers {
        reader.join().unwrap();
    }
    // Retire owned pooled connections before injecting a hostile sidecar; WAL may legitimately still exist.
    // 注入恶意侧文件之前先退休自有连接池；读取结束后 WAL 仍存在属于正常 SQLite 生命周期。
    super::sqlite_connections::close(&fixture.root, &path).unwrap();
    let sidecar = path.with_file_name("retrieval.sqlite-wal");
    if sidecar.exists() {
        super::managed_files::inspect(&fixture.root, &sidecar, false, true, u64::MAX).unwrap();
        fs::remove_file(&sidecar).unwrap();
    }
    let original = fixture.root.join("external-sidecar");
    fs::write(&original, "protected original").unwrap();
    fs::hard_link(&original, &sidecar).unwrap();
    let error = match super::sqlite_store::open(&fixture.root, &path, false) {
        Ok(_) => panic!("linked sidecar must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "UNSAFE_RETRIEVAL_PATH");
    assert_eq!(fs::read_to_string(&original).unwrap(), "protected original");
}

#[test]
fn archive_files_are_exclusive_and_keep_original_utf16_json_bytes() {
    let fixture = Fixture::new();
    let directory = fixture.root.join("Chats/chat-fixture/tool-results");
    let arguments = json!({"root": fixture.path(), "directory": directory,
        "id": "a3c6f6ed-17ee-4d48-84f1-bdd55b0002ea", "raw": r#"{"unpaired":"before\ud800after"}"#});
    super::tool_results::apply("storage.toolResults.save", &arguments).unwrap();
    let error = super::tool_results::apply("storage.toolResults.save", &arguments).unwrap_err();
    assert_eq!(error.code, "TOOL_RESULT_CONFLICT");
    let read = super::tool_results::apply("storage.toolResults.read", &arguments).unwrap();
    assert_eq!(read["raw"], arguments["raw"]);
    let page = super::tool_results::apply(
        "storage.toolResults.page",
        &json!({"sourceRaw":"\"a😀b\"", "offset":2, "limit":1, "projection":"public"}),
    )
    .unwrap();
    assert_eq!(page["offset"], 1);
    assert_eq!(page["end"], 3);
    assert_eq!(page["hasMore"],true);
    let last = super::tool_results::apply("storage.toolResults.page",
        &json!({"sourceRaw":"\"a😀b\"","offset":page["end"],"limit":1,"projection":"public"})).unwrap();
    assert_eq!(last["end"],4); assert_eq!(last["hasMore"],false);
    let eof = super::tool_results::apply("storage.toolResults.page",
        &json!({"sourceRaw":"\"a😀b\"","offset":last["end"],"limit":1,"projection":"public"})).unwrap();
    assert_eq!(eof["offset"],eof["end"]); assert_eq!(eof["hasMore"],false);

    let chat = uuid::Uuid::new_v4().to_string();
    super::gateway::ensure_conversation(&fixture.root,&chat,"archive paging fixture").unwrap();
    let original = json!({"text":"a😀b","hasMore":true,"totalCharacters":100});
    let reference = super::gateway::save_tool_result(&fixture.root,&chat,&uuid::Uuid::new_v4().to_string(),
        &json!({"id":"original-read","name":"filesystem.read","arguments":{}}),&original,None).unwrap();
    let id = reference["id"].as_str().unwrap();
    let first = super::gateway::read_tool_result(&fixture.root,&chat,id,0,1).unwrap();
    assert_eq!(first["hasMore"],true); assert_eq!(first["resultRef"],reference);
    let last = super::gateway::read_tool_result(&fixture.root,&chat,id,first["nextOffset"].as_u64().unwrap(),16000).unwrap();
    assert_eq!(last["hasMore"],false); assert_eq!(last["resultRef"],reference);
    let reconstructed: Value = serde_json::from_str(&format!("{}{}",first["text"].as_str().unwrap(),last["text"].as_str().unwrap())).unwrap();
    // EOF covers this saved projection only; its original source can still have unread content.
    // 归档页结束只覆盖该结果的公开投影，原始来源仍可能有未读内容。
    assert_eq!(reconstructed,original);
}

#[test]
fn evidence_reference_namespace_and_projection_preserve_utf16_offsets() {
    let id = "a3c6f6ed-17ee-4d48-84f1-bdd55b0002ea";
    let reference = super::evidence::apply(
        "storage.evidence.reference",
        &json!({"archiveId":id,"number":60}),
    )
    .unwrap();
    let parsed =
        super::evidence::apply("storage.evidence.parse", &json!({"value":reference})).unwrap();
    assert_eq!(parsed, json!({"archiveId":id,"referenceNumber":60}));
    let plan = super::evidence::apply("storage.evidence.projection", &json!({
        "value":{"isError":true,"message":"x� C:\\private\\secret.txt; retained","path":"C:\\private\\secret.txt"},
        "stringsRaw":{"[\"message\"]":"\"x\\ud800 C:\\\\private\\\\secret.txt; retained\""},"jsonFacts":[]
    })).unwrap();
    assert!(plan["remove"].as_array().unwrap().contains(&json!("path")));
    let message = plan["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["key"] == "message")
        .unwrap();
    assert_eq!(message["plan"]["stages"][0][0][0], 3);
    assert_eq!(message["plan"]["stages"][0][0][2], "[local-path]");
}

#[test]
fn sqlite_bootstrap_rejects_a_future_database_before_schema_changes() {
    let fixture = Fixture::new();
    let directory = fixture.root.join("Index");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("retrieval.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE future_owner(value TEXT);INSERT INTO future_owner VALUES ('preserve');PRAGMA user_version=7;").unwrap();
    drop(connection);
    let original = fs::read(&path).unwrap();
    let error = super::sqlite_bootstrap::apply(
        "storage.index.bootstrap",
        &json!({"root":fixture.path(),"path":path,"epoch":"fixture"}),
    )
    .unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_RETRIEVAL_INDEX_VERSION");
    assert_eq!(fs::read(&path).unwrap(), original);
}

fn legacy_retrieval_fixture(duplicate_chunk: bool) -> (Fixture, PathBuf) {
    let fixture = Fixture::new();
    let directory = fixture.root.join("Index");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("retrieval.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    let schema = super::sqlite_schema::RETRIEVAL_SCHEMA.replace("\r\n", "\n")
        .replace("TEXT NOT NULL PRIMARY KEY", "TEXT PRIMARY KEY")
        .replace(",\n    UNIQUE(source_id,chunk_index)", "");
    connection.execute_batch(&schema).unwrap();
    connection.execute_batch("INSERT INTO sources(source_id,scope_key,source_type,title,locator,text,content_hash,source_revision,binding_revision,active_generation,updated_at) VALUES ('source','project:fixture','knowledge','Legacy title','{}','Golden cathedral','hash','v1',1,1,'fixture');
        INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version) VALUES ('chunk','source',0,'Golden cathedral','golden cathedral','hash',0,16,1,1,'fixture','fixture');PRAGMA user_version=5;").unwrap();
    if duplicate_chunk {
        connection.execute_batch("INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version) SELECT 'duplicate',source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version FROM chunks;").unwrap();
    }
    drop(connection);
    (fixture, path)
}

#[test]
fn retrieval_integrity_migration_preserves_fts_rows_and_rejects_duplicate_source_ordinals() {
    let (fixture,path) = legacy_retrieval_fixture(false);
    super::sqlite_bootstrap::apply("storage.index.bootstrap", &json!({"root":fixture.path(),"path":path,"epoch":"fixture"})).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(connection.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),6);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM chunk_fts WHERE chunk_fts MATCH 'golden'",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(connection.query_row("SELECT text FROM chunks WHERE chunk_id='chunk'",[],|row|row.get::<_,String>(0)).unwrap(),"Golden cathedral");
    assert!(connection.execute_batch("INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version) SELECT 'duplicate',source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version FROM chunks;").is_err());
    assert!(connection.execute("INSERT INTO retrieval_metadata(key,value) VALUES (NULL,'invalid')",[]).is_err());
}

#[test]
fn duplicate_legacy_chunks_prevent_migration_and_preserve_every_original_row() {
    let (fixture,path) = legacy_retrieval_fixture(true);
    let error = super::sqlite_bootstrap::apply("storage.index.bootstrap", &json!({"root":fixture.path(),"path":path,"epoch":"fixture"})).unwrap_err();
    assert_eq!(error.code,"STORAGE_INTEGRITY_MIGRATION_REQUIRED");
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(connection.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),5);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM chunks",[],|row|row.get::<_,i64>(0)).unwrap(),2);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '__kynxa_integrity_%'",[],|row|row.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn conversation_integrity_migration_keeps_catalog_rows_and_nonnullable_keys() {
    let fixture = Fixture::new();
    let directory=fixture.root.join("Index");
    fs::create_dir(&directory).unwrap();
    let path=directory.join("search.sqlite");
    let connection=rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE projects(id TEXT PRIMARY KEY COLLATE NOCASE,name TEXT NOT NULL,workspace_path TEXT,sort_order INTEGER NOT NULL,pinned INTEGER NOT NULL,archived INTEGER NOT NULL);
        CREATE TABLE sessions(id TEXT PRIMARY KEY COLLATE NOCASE,project_id TEXT REFERENCES projects(id),title TEXT NOT NULL,sort_order INTEGER NOT NULL,pinned INTEGER NOT NULL,archived INTEGER NOT NULL,workspace_path TEXT);
        INSERT INTO projects VALUES ('project','preserved','workspace',0,0,0);INSERT INTO sessions VALUES ('session','project','preserved chat',0,0,0,'workspace');
        INSERT INTO metadata VALUES ('fingerprint','fixture');INSERT INTO metadata VALUES ('catalog_revision','1');PRAGMA user_version=1;").unwrap();
    drop(connection);
    let data=json!({"fingerprint":"fixture","revision":1,"projects":[{"id":"project"}],"sessions":[{"id":"session"}]});
    let result=super::conversation_index::apply("storage.catalogIndex.rebuild",&json!({"root":fixture.path(),"data":data})).unwrap();
    assert_eq!(result["rebuilt"],false,"a constraint migration must not rebuild or replace valid catalog rows");
    let connection=rusqlite::Connection::open(&path).unwrap();
    assert_eq!(connection.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),2);
    assert_eq!(connection.query_row("SELECT title FROM sessions",[],|row|row.get::<_,String>(0)).unwrap(),"preserved chat");
    assert!(connection.execute("INSERT INTO metadata VALUES (NULL,'invalid')",[]).is_err());
}

#[test]
fn integrity_migration_rolls_back_earlier_copies_when_a_later_constraint_fails() {
    let connection=rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE first(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
        CREATE TABLE second(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
        INSERT INTO first VALUES ('original-first',1);INSERT INTO second VALUES ('original-second',2);PRAGMA user_version=1;").unwrap();
    let error=super::sqlite_store::migrate_integrity(&connection,2,&[("first",""),("second","CHECK(value<0)")],&[]).unwrap_err();
    assert_eq!(error.code,"STORAGE_INTEGRITY_MIGRATION_REQUIRED");
    assert_eq!(connection.query_row("SELECT key FROM first",[],|row|row.get::<_,String>(0)).unwrap(),"original-first");
    assert_eq!(connection.query_row("SELECT key FROM second",[],|row|row.get::<_,String>(0)).unwrap(),"original-second");
    assert_eq!(connection.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(connection.query_row("PRAGMA foreign_keys",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '__kynxa_integrity_%'",[],|row|row.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn index_admission_checks_real_line_and_unicode_boundaries_in_a_batch() {
    let arguments = json!({"sourceRaw":"\"a😀\\nb\"","hasText":true,
        "chunks":[{"chunkId":"one","chunkIndex":0,"startOffset":0,"endOffset":4,"startLine":1,"endLine":2,"matchesBytes":true},
            {"chunkId":"two","chunkIndex":1,"startOffset":4,"endOffset":5,"startLine":2,"endLine":2,"matchesBytes":true}],
        "vectors":[],"vectorsArray":true,"profileProvided":false,"modelProvided":false,"spaceProvided":false});
    assert_eq!(
        super::index_validation::validate(&arguments).unwrap()["dimensions"],
        Value::Null
    );
    let mut changed = arguments;
    changed["chunks"][0]["endOffset"] = json!(2);
    changed["chunks"][0]["endLine"] = json!(1);
    assert_eq!(
        super::index_validation::validate(&changed)
            .unwrap_err()
            .code,
        "RETRIEVAL_SOURCE_CHANGED"
    );
}

#[test]
fn native_sql_channels_and_vector_pages_preserve_scope_generation_and_bytes() {
    let fixture = Fixture::new();
    let path = fixture.root.join("Index/retrieval.sqlite");
    let arguments = json!({"root":fixture.path(),"path":path,"epoch":"fixture",
        "scopes":["project:one"],"vectorTable":"chunks","profile":"fixture","dimensions":2,
        "model":"v1","modelProvided":true,"space":"space-a","spaceProvided":true,
        "cacheId":format!("native-vectors-{}",fixture.path())});
    super::sqlite_bootstrap::apply("storage.index.bootstrap", &arguments).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    for (id, scope) in [("authorized", "project:one"), ("private", "project:other")] {
        connection.execute("INSERT INTO sources(source_id,scope_key,source_type,title,locator,text,content_hash,source_revision,binding_revision,active_generation,updated_at,relative_path) VALUES (?,?, 'work-file','fixture','{}','Thing body','hash','1',1,1,'fixture','src/fixture.rs')",[id,scope]).unwrap();
        connection
            .execute(
                "INSERT INTO scope_snapshots(scope_key,generation) VALUES (?,1)",
                [scope],
            )
            .unwrap();
    }
    let vector: Vec<u8> = [1.0f32, 0.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    for index in 0..261 {
        let source = if index == 260 {
            "private"
        } else {
            "authorized"
        };
        connection.execute("INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,chunker_version,tokenizer_version,embedding_profile_id,dimensions,vector,embedding_model_version,embedding_space_id,structure_domain,symbol_name,qualified_name,parse_status,unit_start_offset,unit_end_offset) VALUES (?,?,?,'Thing body','Thing body','hash',0,10,1,1,'fixture','fixture','fixture',2,?,'v1','space-a','code','Thing','Namespace.Thing','parsed',0,10)",rusqlite::params![format!("chunk-{index:03}"),source,index,vector]).unwrap();
    }
    let channels = super::sqlite_queries::apply("storage.index.channels", &json!({"root":fixture.path(),"path":path,"scopes":["project:one"],"symbol":"Thing","pathFilter":"fixture.rs","literalQuery":"Thing","domain":"code","limit":40})).unwrap();
    assert_eq!(channels["symbols"].as_array().unwrap().len(), 1);
    assert_eq!(channels["symbols"][0]["exact_target_match"], true);
    assert_eq!(channels["paths"].as_array().unwrap().len(), 1);
    for row in channels["literal"].as_array().unwrap() {
        assert_eq!(row["scope_key"], "project:one");
    }
    let descriptors =
        super::sqlite_queries::apply("storage.index.vectorDescriptors", &arguments).unwrap();
    assert_eq!(descriptors.as_array().unwrap().len(), 1);
    assert_eq!(descriptors[0]["count"], 260);
    let mut page_arguments = arguments.clone();
    page_arguments["descriptor"] = descriptors[0].clone();
    page_arguments["lastId"] = json!(0);
    let first = super::sqlite_queries::apply("storage.index.vectorRows", &page_arguments).unwrap();
    assert_eq!(first.as_array().unwrap().len(), 256);
    assert_eq!(first[0]["vector"], "AACAPwAAAAA=");
    page_arguments["lastId"] = first[255]["id"].clone();
    let second = super::sqlite_queries::apply("storage.index.vectorRows", &page_arguments).unwrap();
    assert_eq!(second.as_array().unwrap().len(), 4);
    let mut partition_arguments = page_arguments.clone();
    partition_arguments["segments"] = json!(3);
    let parts = super::sqlite_queries::apply("storage.index.vectorPartition", &partition_arguments)
        .unwrap();
    assert_eq!(
        parts
            .as_array()
            .unwrap()
            .iter()
            .map(|part| part["count"].as_u64().unwrap())
            .sum::<u64>(),
        260
    );
    super::sqlite_queries::apply("storage.index.vectorDescriptors", &arguments).unwrap();
    let stats = super::sqlite_queries::apply("storage.index.vectorCacheStats", &arguments).unwrap();
    assert_eq!(stats["descriptorCache"]["hits"], 1);
    assert_eq!(stats["descriptorCache"]["misses"], 1);
    let invalidated =
        super::sqlite_queries::apply("storage.index.vectorCacheInvalidate", &arguments).unwrap();
    assert_eq!(invalidated["descriptorCache"]["entries"], 0);
    assert_eq!(invalidated["partitionCache"]["entries"], 0);
    let sources = super::sqlite_queries::apply("storage.index.listSources", &arguments).unwrap();
    assert_eq!(sources["rows"].as_array().unwrap().len(), 1);
    assert_eq!(sources["rows"][0]["chunkCount"], 260);
    connection
        .execute(
            "UPDATE scope_snapshots SET generation=2 WHERE scope_key='project:one'",
            [],
        )
        .unwrap();
    let error =
        super::sqlite_queries::apply("storage.index.vectorRows", &page_arguments).unwrap_err();
    assert_eq!(error.code, "RETRIEVAL_ANN_STALE_BUILD");
    let mut verify_arguments = arguments.clone();
    verify_arguments["matches"] =
        json!([{"id":first[0]["id"],"identity":descriptors[0],"distance":0.0}]);
    let stale =
        super::sqlite_queries::apply("storage.index.verifyVectorMatches", &verify_arguments)
            .unwrap();
    assert_eq!(stale["mismatch"], true);
    assert_eq!(stale["items"], json!([]));
    super::sqlite_queries::apply("storage.index.vectorCacheClose", &arguments).unwrap();
}
