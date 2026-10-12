use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json, value::RawValue};

use super::source_collections::SourceCollections;

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}
fn key(value: &Value) -> String {
    value.to_string()
}
fn object_key(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}
fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}
fn ignored_git(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|part| [".git", ".gitignore"].contains(&part))
}
fn under(path: &str, directory: &str) -> bool {
    path == directory
        || path.starts_with(&format!("{directory}/"))
        || path.starts_with(&format!("{directory}\\"))
}
fn raw_fields(wire: &str) -> HashMap<String, Box<RawValue>> {
    serde_json::from_str(wire).unwrap_or_default()
}
fn wire_with_fields(value: &Value, fields: &HashMap<String, String>) -> String {
    format!(
        "{{{}}}",
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| format!(
                "{}:{}",
                object_key(key),
                fields
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| value.to_string())
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Source caches, inventory generations and notifications are native facts; fs.watch/decoder I/O stay external.
/// 来源缓存、目录代次与通知由原生持有；fs.watch/解码 I/O 留在外部，失败清单不能证明文件删除。
#[derive(Default)]
pub struct SourceSyncState {
    closed: bool,
    sequence: u64,
    caches: HashMap<String, HashMap<String, Value>>,
    state_keys: HashMap<String, u64>,
    states: HashMap<u64, Value>,
    collections: SourceCollections,
    watchers: HashMap<String, Value>,
    recent: HashMap<String, VecDeque<String>>,
    scans: HashMap<u64, Value>,
    discovery_error: Value,
    released: Vec<u64>,
    busy: HashMap<u64, u64>,
}
impl SourceSyncState {
    fn state_id(&self, project: &Value) -> Option<u64> {
        self.state_keys.get(&key(project)).copied()
    }
    fn next_id(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
    fn cache_remove(&mut self, domain: &str, cache_key: &str) -> Option<u64> {
        let id = self.caches.get_mut(domain)?.remove(cache_key)?["handleId"].as_u64()?;
        self.released.push(id);
        Some(id)
    }
    fn state_map(&self, id: u64, field: &str) -> u64 {
        self.states
            .get(&id)
            .and_then(|state| state[field].as_u64())
            .unwrap_or(0)
    }
    fn inventory_memory(&self, id: u64) -> u64 {
        self.collections
            .keys(id)
            .iter()
            .filter_map(|key| self.collections.wire(id, key))
            .map(|wire| wire.encode_utf16().count() as u64 * 2 + 256)
            .sum()
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        self.apply_inner(operation, args)
    }
    fn collect(&mut self) {
        let retained_states: HashSet<u64> = self
            .state_keys
            .values()
            .copied()
            .chain(
                self.busy
                    .iter()
                    .filter(|(_, count)| **count > 0)
                    .map(|(id, _)| *id),
            )
            .collect();
        self.states.retain(|id, _| retained_states.contains(id));
        let mut collections: HashSet<u64> = self
            .states
            .values()
            .flat_map(|state| {
                [
                    "files",
                    "failureCache",
                    "dirtyPaths",
                    "discoveryFiles",
                    "scanDirty",
                ]
                .iter()
                .filter_map(|field| state[*field].as_u64())
            })
            .collect();
        collections.extend(
            self.caches
                .values()
                .flat_map(HashMap::values)
                .filter_map(|cache| cache["metadataFiles"].as_u64()),
        );
        self.collections.collect(&collections);
    }
    fn apply_inner(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation.starts_with("orchestration.sourceManagement.sync.collection.") {
            return self.collections.apply(operation, args);
        }
        let project = &args["projectId"];
        let project_key = key(project);
        let state_id = args["stateId"].as_u64().or_else(|| self.state_id(project));
        match operation {
            "orchestration.sourceManagement.sync.closed" => Ok(json!(self.closed)),
            "orchestration.sourceManagement.sync.cacheReleased" => Ok(json!(std::mem::take(&mut self.released))),
            "orchestration.sourceManagement.sync.collect" => { self.collect(); Ok(Value::Null) }
            "orchestration.sourceManagement.sync.limits" => {
                let indexing = &args["indexing"]; Ok(json!({"maximumFiles": indexing["maximumFiles"], "maximumSourceBytes": indexing["maximumSourceBytes"],
                    "maximumBytes": indexing["maximumTotalBytes"], "maximumEntries": indexing["maximumEntries"],
                    "maximumDocumentInputBytes": number(&indexing["maximumSourceBytes"]).min(number(&indexing["maximumDocumentInputBytes"])),
                    "maximumDocumentOutputBytes": number(&indexing["maximumSourceBytes"]).min(number(&indexing["maximumDocumentOutputBytes"])), "maximumPdfPages": indexing["maximumPdfPages"]}))
            }
            "orchestration.sourceManagement.sync.cacheGet" => {
                let domain = string(&args["domain"]); let cache_key = key(&args["key"]); let cached = self.caches.get(domain).and_then(|cache| cache.get(&cache_key));
                if args["checkRevision"] == true && cached.is_none_or(|cached| cached["revision"] != args["revision"] || number(&args["now"]) - number(&cached["capturedAt"]) >= 2000.0) { return Ok(Value::Null); }
                Ok(cached.cloned().unwrap_or(Value::Null))
            }
            "orchestration.sourceManagement.sync.cacheSet" => {
                if self.closed { return Ok(Value::Null); } let handle = self.next_id(); let mut value = args["metadata"].clone(); value["handleId"] = json!(handle);
                if value["memoryBytes"].is_null() { value["memoryBytes"] = json!(array(&args["sourceMeasures"]).iter().map(|source| source["textCharacters"].as_u64().unwrap_or(0) * 2
                    + if args["domain"] == "conversation" { 0 } else { source["locatorCharacters"].as_u64().unwrap_or(0) * 2 + 512 }).sum::<u64>()); }
                let domain = string(&args["domain"]).to_owned(); let cache_key = key(&args["key"]); let released = self.cache_remove(&domain, &cache_key);
                if domain == "mounted" && value.get("memoryLimitBytes").is_some_and(|limit| number(&value["memoryBytes"]) > number(limit)) { return Ok(Value::Null); }
                self.caches.entry(domain).or_default().insert(cache_key, value); Ok(json!({"handleId": handle, "released": released}))
            }
            "orchestration.sourceManagement.sync.cacheDelete" => Ok(json!(self.cache_remove(string(&args["domain"]), &key(&args["key"])))),
            "orchestration.sourceManagement.sync.cacheClear" => { let cache = self.caches.remove(string(&args["domain"])).unwrap_or_default(); let handles: Vec<u64> = cache.values().filter_map(|value| value["handleId"].as_u64()).collect(); self.released.extend(&handles); Ok(json!(handles)) }
            "orchestration.sourceManagement.sync.cacheSize" => Ok(json!(self.caches.get(string(&args["domain"])).map(HashMap::len).unwrap_or(0))),
            "orchestration.sourceManagement.sync.cacheEntries" => Ok(json!(self.caches.get(string(&args["domain"])).map(|cache| cache.iter().map(|(key, metadata)| json!({"key": serde_json::from_str::<Value>(key).unwrap_or(Value::Null), "metadata": metadata})).collect::<Vec<_>>()).unwrap_or_default())),
            "orchestration.sourceManagement.sync.trim" => {
                let mut released = Vec::new(); loop {
                    let retained: f64 = self.caches.values().flat_map(HashMap::values).map(|value| number(&value["memoryBytes"])).chain(self.state_keys.values().filter_map(|id| self.states.get(id)).map(|state| number(&state["memoryBytes"]))).sum();
                    let over_memory = retained > number(&args["memoryLimitBytes"]); let over_count = self.caches.values().any(|cache| cache.len() > 32) || self.state_keys.len() > 32;
                    if !over_memory && !over_count { break; } let mut oldest: Option<(f64, String, String)> = None;
                    for (domain, cache) in &self.caches { if !over_memory && cache.len() <= 32 { continue; } for (cache_key, entry) in cache { let captured = number(&entry["capturedAt"]);
                        if oldest.as_ref().is_none_or(|oldest| captured < oldest.0) { oldest = Some((captured, domain.clone(), cache_key.clone())); } } }
                    if over_memory || self.state_keys.len() > 32 { for (project_key, id) in &self.state_keys { let captured = number(&self.states[id]["capturedAt"]);
                        if oldest.as_ref().is_none_or(|oldest| captured < oldest.0) { oldest = Some((captured, "states".into(), project_key.clone())); } } }
                    let Some((_, domain, cache_key)) = oldest else { break; };
                    if domain == "states" { self.state_keys.remove(&cache_key); if let Some(handle) = self.cache_remove("mounted", &cache_key) { released.push(handle); } }
                    else if let Some(handle) = self.cache_remove(&domain, &cache_key) { released.push(handle); }
                } Ok(json!(released))
            }
            "orchestration.sourceManagement.sync.stateGet" => Ok(json!(self.state_id(project))),
            "orchestration.sourceManagement.sync.stateDelete" => Ok(json!(self.state_keys.remove(&project_key).is_some())),
            "orchestration.sourceManagement.sync.stateClear" => { self.state_keys.clear(); Ok(Value::Null) }
            "orchestration.sourceManagement.sync.stateEntries" => Ok(json!(self.state_keys.iter().map(|(key, id)| json!({"key": serde_json::from_str::<Value>(key).unwrap_or(Value::Null), "id": id})).collect::<Vec<_>>())),
            "orchestration.sourceManagement.sync.stateField" => Ok(state_id.and_then(|id| self.states.get(&id)).map(|state| state[string(&args["property"])].clone()).unwrap_or(Value::Null)),
            "orchestration.sourceManagement.sync.discoveryError" => { if args["set"] == true { self.discovery_error = args["value"].clone(); } Ok(self.discovery_error.clone()) }
            "orchestration.sourceManagement.sync.foreground" => {
                if self.closed { return Err("RETRIEVAL_CLOSED".into()); } let valid = state_id.and_then(|id| self.states.get(&id)).filter(|state| state["root"] == args["root"] && state["bindingRevision"] == args["bindingRevision"]);
                let metadata = valid.and_then(|state| state.get("discoveryFiles").filter(|value| !value.is_null()).or_else(|| state.get("files"))).and_then(Value::as_u64);
                let files = metadata.map(|id| self.collections.values(id)).unwrap_or_default(); let bytes: f64 = files.iter().map(|file| number(&file["textBytes"])).sum();
                Ok(json!({"metadataId": metadata, "withinBudget": files.len() as f64 <= number(&args["limits"]["maximumFiles"]) && bytes <= number(&args["limits"]["maximumBytes"]),
                    "scan": {"backgroundPending": true, "fileReads": 0, "reusedFiles": 0, "scannedFiles": 0,
                        "coverage": {"discovered": files.len(), "readable": files.len(), "complete": false}}, "diagnosticCode": self.discovery_error}))
            }
            "orchestration.sourceManagement.sync.capturePlan" => {
                if self.closed { return Err("RETRIEVAL_CLOSED".into()); }
                let state = state_id.and_then(|id| self.states.get(&id)); let cached = self.caches.get("mounted").and_then(|cache| cache.get(&project_key));
                let same_binding = state.is_some_and(|state| state["root"] == args["root"] && state["bindingRevision"] == args["bindingRevision"]);
                let unchanged = state.zip(cached).is_some_and(|(state, cache)| same_binding && state["limitsKey"] == args["limitsKey"] && state["generation"] == cache["generation"] && state["fullScan"] != true
                    && self.collections.size(state["dirtyPaths"].as_u64().unwrap_or(0)) == 0 && cache["root"] == args["root"] && cache["bindingRevision"] == args["bindingRevision"] && cache["limitsKey"] == args["limitsKey"]);
                if same_binding && state.is_some_and(|state| state["limitsKey"] != args["limitsKey"]) { let id = state_id.unwrap(); let failure_cache = self.state_map(id, "failureCache"); self.collections.clear(failure_cache); self.states.get_mut(&id).unwrap()["fullScan"] = json!(true); }
                Ok(json!({"cached": unchanged, "stateId": if same_binding { state_id } else { None }, "readManifest": !same_binding}))
            }
            "orchestration.sourceManagement.sync.initState" => {
                let id = self.next_id(); let files = self.collections.create();
                for file in array(&args["filesWire"]) { self.collections.set(files, string(&file["key"]).into(), string(&file["wire"]).into())?; }
                let failures = self.collections.create(); let dirty = self.collections.create(); let memory = self.inventory_memory(files);
                self.states.insert(id, json!({"root": args["root"], "bindingRevision": args["bindingRevision"], "files": files, "failureCache": failures,
                    "dirtyPaths": dirty, "generation": 0, "fullScan": true, "limitsKey": args["limitsKey"], "capturedAt": args["now"], "memoryBytes": memory}));
                self.state_keys.insert(project_key, id); Ok(json!(id))
            }
            "orchestration.sourceManagement.sync.scanStart" => {
                let id = state_id.ok_or("SOURCE_SYNC_STATE_NOT_FOUND")?; let state = &self.states[&id]; let files = self.state_map(id, "files"); let dirty = self.state_map(id, "dirtyPaths");
                let full = state["fullScan"] == true || self.collections.size(dirty) == 0; let generation = state["generation"].clone(); let paths = self.collections.clone_collection(dirty);
                let next = if full { self.collections.create() } else { self.collections.clone_collection(files) }; self.states.get_mut(&id).unwrap()["discoveryFiles"] = json!(next);
                self.states.get_mut(&id).unwrap()["scanDirty"] = json!(paths); *self.busy.entry(id).or_default() += 1;
                Ok(json!({"full": full, "generation": generation, "filesId": files, "dirtyId": paths, "nextId": next, "failureId": self.state_map(id, "failureCache")}))
            }
            "orchestration.sourceManagement.sync.captureSettled" => { if let Some(id) = state_id { if let Some(count) = self.busy.get_mut(&id) { *count = count.saturating_sub(1); } } self.collect(); Ok(Value::Null) }
            "orchestration.sourceManagement.sync.scanObserved" => {
                let id = state_id.ok_or("SOURCE_SYNC_STATE_NOT_FOUND")?; let next = self.state_map(id, "discoveryFiles"); let path = string(&args["relativePath"]); let map_key = object_key(path); let file = &args["file"];
                if file["missing"] == true { self.collections.delete(next, &map_key); return Ok(Value::Null); } if file["failed"] == true { return Ok(Value::Null); }
                let mut value = json!({"relativePath": path, "contentHash": file["contentHash"], "textBytes": file["textBytes"], "metadata": file["metadata"]});
                let original_fields = raw_fields(string(&args["fileWire"])); let mut wire_fields = HashMap::new();
                for field in ["extraction", "windows"] { if let Some(raw) = original_fields.get(field) { wire_fields.insert(field.into(), raw.get().into()); } }
                if !file["extraction"]["pageWindow"].is_null() {
                    let previous = self.collections.get(next, &map_key).unwrap_or(Value::Null); let start = file["extraction"]["pageWindow"]["startPage"].as_u64().unwrap_or(0);
                    let compatible = previous["extraction"]["rawContentHash"] == file["extraction"]["rawContentHash"] && start > 1;
                    let mut windows: Vec<Value> = if compatible { array(&previous["documentWindows"]).iter().filter(|window| window["extraction"]["pageWindow"]["startPage"].as_u64().unwrap_or(0) < start).cloned().collect() } else { vec![] };
                    windows.push(json!({"contentHash": file["contentHash"], "textBytes": file["textBytes"], "extraction": file["extraction"]})); let first = windows.first().unwrap();
                    value["contentHash"] = first["contentHash"].clone(); value["extraction"] = first["extraction"].clone();
                    value["documentComplete"] = json!(first["extraction"]["pageWindow"]["startPage"] == 1 && windows.last().unwrap()["extraction"]["pageWindow"]["nextPage"].is_null());
                    let previous_fields = raw_fields(&self.collections.wire(next, &map_key).unwrap_or_default());
                    let previous_windows: Vec<Box<RawValue>> = previous_fields.get("documentWindows").and_then(|raw| serde_json::from_str(raw.get()).ok()).unwrap_or_default();
                    let mut window_wires: Vec<String> = if compatible { previous_windows.iter().filter(|wire| serde_json::from_str::<Value>(wire.get()).ok().is_some_and(|window| window["extraction"]["pageWindow"]["startPage"].as_u64().unwrap_or(0) < start)).map(|wire| wire.get().into()).collect() } else { vec![] };
                    window_wires.push(format!("{{\"contentHash\":{},\"textBytes\":{},\"extraction\":{}}}", file["contentHash"], file["textBytes"], original_fields.get("extraction").map(|raw| raw.get()).unwrap_or("null")));
                    if let Some(first_wire) = window_wires.first() { if let Some(extraction) = raw_fields(first_wire).get("extraction") { wire_fields.insert("extraction".into(), extraction.get().into()); } }
                    wire_fields.insert("documentWindows".into(), format!("[{}]", window_wires.join(",")));
                    value["textBytes"] = json!(windows.iter().map(|window| number(&window["textBytes"])).sum::<f64>()); value["documentWindows"] = json!(windows);
                } else { for field in ["extraction", "windows"] { if let Some(field_value) = file.get(field) { value[field] = field_value.clone(); } } }
                self.collections.set(next, map_key, wire_with_fields(&value, &wire_fields))?; Ok(Value::Null)
            }
            "orchestration.sourceManagement.sync.scanCheck" => {
                let id = state_id.ok_or("SOURCE_SYNC_STATE_NOT_FOUND")?; let files = self.state_map(id, "files"); let next = self.state_map(id, "discoveryFiles");
                for path_key in self.collections.keys(files) { if self.collections.wire(next, &path_key).is_some() { continue; } let path: String = serde_json::from_str(&path_key).unwrap_or_default();
                    if array(&args["stats"]["failures"]).iter().any(|failure| under(&path, string(&failure["relativePath"]))) { if let Some(wire) = self.collections.wire(files, &path_key) { self.collections.set(next, path_key, wire)?; } } }
                let values = self.collections.values(next); let count: usize = values.iter().map(|file| if file["documentWindows"].is_array() { array(&file["documentWindows"]).len() } else { 1 }).sum();
                let bytes: f64 = values.iter().map(|file| number(&file["textBytes"])).sum();
                if count as f64 > number(&args["limits"]["maximumFiles"]) || bytes > number(&args["limits"]["maximumBytes"]) { return Err("RETRIEVAL_SCAN_LIMIT".into()); } Ok(Value::Null)
            }
            "orchestration.sourceManagement.sync.scanCommitted" => {
                let id = state_id.ok_or("SOURCE_SYNC_STATE_NOT_FOUND")?; let next = self.state_map(id, "discoveryFiles"); let memory = self.inventory_memory(next);
                let state = self.states.get_mut(&id).unwrap(); state["files"] = json!(next); state["discoveryFiles"] = Value::Null; state["memoryBytes"] = json!(memory); Ok(Value::Null)
            }
            "orchestration.sourceManagement.sync.captured" => {
                let id = state_id.ok_or("SOURCE_SYNC_STATE_NOT_FOUND")?; let dirty = self.state_map(id, "dirtyPaths"); let state = self.states.get_mut(&id).unwrap();
                if state["generation"] == args["generation"] { self.collections.clear(dirty); state["fullScan"] = json!(false); }
                state["limitsKey"] = args["limitsKey"].clone(); state["capturedAt"] = args["now"].clone();
                let stats = &args["stats"]; let mut coverage = json!({"discovered": stats["discoveredFiles"].as_u64().or_else(|| stats["scannedFiles"].as_u64()).unwrap_or(args["fileCount"].as_u64().unwrap_or(0)),
                    "readable": stats["scannedFiles"].as_u64().unwrap_or(args["fileCount"].as_u64().unwrap_or(0)), "failed": stats["failedFiles"].as_u64().unwrap_or(0),
                    "complete": !truthy(&stats["failedFiles"]) && !truthy(&stats["incomplete"]), "effectiveLimits": args["limits"]});
                if truthy(&stats["limit"]) { coverage["limit"] = stats["limit"].clone(); } Ok(json!({"coverage": coverage, "generation": state["generation"], "cache": !self.closed}))
            }
            "orchestration.sourceManagement.sync.describe" => {
                let files = array(&args["files"]); let failures = array(&args["failures"]); let mut plans = Vec::new();
                for (file_index, original) in files.iter().enumerate() {
                    let windows: Vec<Option<usize>> = if original["documentWindows"].is_array() { (0..array(&original["documentWindows"]).len()).map(Some).collect() } else { vec![None] };
                    for document_index in windows {
                        let mut file = original.clone(); if let Some(index) = document_index { file.as_object_mut().unwrap().extend(array(&original["documentWindows"])[index].as_object().unwrap().clone()); file.as_object_mut().unwrap().remove("documentWindows"); }
                        let relative = string(&file["relativePath"]); let failure = failures.iter().rev().find(|failure| failure["relativePath"] == relative).or_else(|| failures.iter().find(|failure| truthy(&failure["directory"]) && (string(&failure["relativePath"]).is_empty() || under(relative, string(&failure["relativePath"])))));
                        let page = &file["extraction"]["pageWindow"]; let failed_window = failure.map(|failure| &failure["documentCoverage"]["readback"]);
                        let affects = page.is_null() || failed_window.is_none_or(|window| !truthy(&window["startPage"]) || number(&page["startPage"]) <= number(&window["endPage"]) && number(&page["endPage"]) >= number(&window["startPage"]));
                        let mut common = json!({"scopeKey": format!("project:{}", string(project)), "sourceType": "work-file", "title": file.get("title").filter(|title| !title.is_null()).unwrap_or(&file["relativePath"]), "bindingRevision": args["bindingRevision"]});
                        if affects && failure.is_some_and(|failure| truthy(&failure["errorCode"])) { let failure = failure.unwrap(); common["unavailable"] = json!(true); common["errorCode"] = failure["errorCode"].clone(); if let Some(coverage) = failure.get("documentCoverage") { common["documentCoverage"] = coverage.clone(); } }
                        let file_windows: Vec<Option<usize>> = if file["windows"].is_array() { (0..array(&file["windows"]).len()).map(Some).collect() } else { vec![None] };
                        for window in file_windows { plans.push(json!({"fileIndex": file_index, "documentIndex": document_index, "windowIndex": window, "common": common,
                            "identity": if window.is_some() { "file-window" } else if !page.is_null() { "pdf-page" } else { "file" }, "pageStart": page["startPage"]})); }
                    }
                } Ok(json!(plans))
            }
            "orchestration.sourceManagement.sync.snapshot" => { let id = self.next_id(); self.scans.insert(id, args["scan"].clone()); Ok(json!(id)) }
            "orchestration.sourceManagement.sync.scanField" => Ok(self.scans.get(&args["scanId"].as_u64().unwrap_or(0)).map(|scan| if args["property"].is_null() { scan.clone() } else { scan[string(&args["property"])].clone() }).unwrap_or(Value::Null)),
            "orchestration.sourceManagement.sync.loaded" => { let scan = self.scans.get_mut(&args["scanId"].as_u64().unwrap_or(0)).ok_or("SOURCE_SNAPSHOT_NOT_FOUND")?; scan["loadedFiles"] = json!(number(&scan["loadedFiles"]) + 1.0); if !args["size"].is_null() { scan["loadedBytes"] = json!(number(&scan["loadedBytes"]) + number(&args["size"])); } Ok(Value::Null) }
            "orchestration.sourceManagement.sync.snapshotReleased" => { self.scans.remove(&args["scanId"].as_u64().unwrap_or(0)); Ok(Value::Null) }
            "orchestration.sourceManagement.sync.currentAllowed" => Ok(json!(!self.closed && args["source"]["unavailable"] != true)),
            "orchestration.sourceManagement.sync.currentPlan" => {
                if truthy(&args["source"]["unavailable"]) || self.closed || args["rootMatches"] != true || args["source"]["bindingRevision"] != args["bindingRevision"] { return Ok(json!({"current": false})); }
                let source = &args["source"]; let expected = &args["expected"]; let window = &source["locator"]["fileWindow"];
                let document_matches = array(&args["documentRevisions"]).iter().any(|window| window["contentHash"] == source["contentHash"] && window["revision"] == source["sourceRevision"]);
                let metadata = if !window.is_null() && array(&expected["windows"]).iter().any(|entry| entry["contentHash"] == source["contentHash"] && entry["startOffset"] == window["startOffset"]) { window["metadata"].clone() }
                    else if document_matches || !expected.is_null() && expected["contentHash"] == source["contentHash"] && args["expectedRevision"] == source["sourceRevision"] && (expected["extraction"].is_null() || expected["extraction"]["version"] == args["extractionVersion"]) { expected["metadata"].clone() } else { Value::Null };
                Ok(json!({"current": if metadata.is_null() || args.get("metadataMatches").is_none() { Value::Null } else { json!(args["isFile"] == true && args["metadataMatches"] == true) }, "expectedMetadata": metadata}))
            }
            "orchestration.sourceManagement.sync.sourceBody" => { if truthy(&args["source"]["unavailable"]) { return Err(args["source"]["errorCode"].as_str().unwrap_or("RETRIEVAL_SOURCE_FAILED").into()); } Ok(json!(args["custom"] == true && args["hasBody"] == true)) }
            "orchestration.sourceManagement.sync.failureIdentity" => Ok(if args["failure"]["documentCoverage"]["format"] == "pdf" { json!(["pdf-page", args["failure"]["documentCoverage"]["readback"]["startPage"].as_u64().unwrap_or(1)]) } else { json!([]) }),
            "orchestration.sourceManagement.sync.loadPlan" => {
                let source = &args["source"]; if truthy(&source["unavailable"]) { return Err(source["errorCode"].as_str().unwrap_or("RETRIEVAL_SOURCE_FAILED").into()); }
                if args["customBody"] == true { return Ok(json!({"existingBody": true})); }
                if args["rootMatches"] != true || source["bindingRevision"] != args["bindingRevision"] { return Err("STALE_RETRIEVAL_SOURCE".into()); }
                let page = &source["locator"]["extraction"]["pageWindow"]; Ok(json!({"fileWindow": !source["locator"]["fileWindow"].is_null(), "pdfOptions": if page.is_null() { json!({}) }
                    else { json!({"pdfPageWindow": {"startPage": page["startPage"], "endPage": page["endPage"], "rawContentHash": source["locator"]["extraction"]["rawContentHash"]}})} }))
            }
            "orchestration.sourceManagement.sync.versionMatches" => Ok(json!(args["contentHash"] == args["expectedHash"] && args["revision"] == args["expectedRevision"])),
            "orchestration.sourceManagement.sync.markChanged" => {
                if array(&args["excluded"]).iter().any(truthy) { return Ok(json!(false)); }
                self.cache_remove("mounted", &project_key); let Some(id) = state_id else { return Ok(json!(true)); }; let dirty = self.state_map(id, "dirtyPaths"); let files = self.state_map(id, "files"); let path = string(&args["changedPath"]); let name = string(&args["name"]);
                let state = self.states.get_mut(&id).unwrap(); state["generation"] = json!(state["generation"].as_u64().unwrap_or(0) + 1);
                let deleted_directory = state["fullScan"] != true && !path.is_empty() && self.collections.wire(files, &object_key(path)).is_none() && self.collections.keys(files).iter().filter_map(|key| serde_json::from_str::<String>(key).ok()).any(|file| under(&file, path));
                if name.is_empty() || ignored_git(name) || args["withinRoot"] != true || deleted_directory || self.collections.size(dirty) >= 2048 { state["fullScan"] = json!(true); self.collections.clear(dirty); }
                else { self.collections.set(dirty, object_key(path), object_key(path))?; } Ok(json!(true))
            }
            "orchestration.sourceManagement.sync.watchPlan" => {
                let unchanged = self.watchers.get(&project_key).is_some_and(|watcher| watcher["root"] == args["root"]);
                Ok(json!({"bind": !self.closed && !unchanged, "debounceMs": 800, "reconciliationMs": 30000}))
            }
            "orchestration.sourceManagement.sync.watchBound" => { self.watchers.insert(project_key, json!({"root": args["root"], "error": args["error"]})); Ok(Value::Null) }
            "orchestration.sourceManagement.sync.watchError" => { if let Some(watcher) = self.watchers.get_mut(&project_key) { watcher["error"] = args["error"].clone(); } Ok(Value::Null) }
            "orchestration.sourceManagement.sync.watchGet" => Ok(self.watchers.get(&project_key).cloned().unwrap_or(Value::Null)),
            "orchestration.sourceManagement.sync.watchDelete" => { self.watchers.remove(&project_key); self.recent.remove(&project_key); Ok(Value::Null) }
            "orchestration.sourceManagement.sync.notify" => {
                if array(&args["excluded"]).iter().any(truthy) { return Ok(json!(false)); }
                let name = string(&args["name"]); if !name.is_empty() && args["withinRoot"] == true && !ignored_git(name) {
                    let paths = self.recent.entry(project_key).or_default(); let path = string(&args["changedPath"]); paths.retain(|candidate| candidate != path); paths.push_back(path.into()); while paths.len() > 16 { paths.pop_front(); } } Ok(json!(true))
            }
            "orchestration.sourceManagement.sync.notifyFlush" => { let paths = self.recent.remove(&project_key).unwrap_or_default(); Ok(json!({"deliver": !self.closed, "changedPaths": paths})) }
            "orchestration.sourceManagement.sync.status" => Ok(json!({"watchers": self.watchers.values().map(|watcher| if truthy(&watcher["error"]) { json!({"state": "polling", "diagnosticCode": watcher["error"]}) } else { json!({"state": "watching"}) }).collect::<Vec<_>>(),
                "reconciliationIntervalMs": 30000, "manifest": if args["hasManifest"] == true { "durable-metadata" } else { "memory-only" }})),
            "orchestration.sourceManagement.sync.close" => { self.closed = true; self.released.extend(self.caches.values().flat_map(HashMap::values).filter_map(|value| value["handleId"].as_u64())); self.caches.clear(); self.state_keys.clear(); Ok(Value::Null) }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
