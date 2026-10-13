use super::{ContractError, ContractResult, identity, managed_files, retrieval};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

const MAXIMUM_REGISTRY_BYTES: u64 = 32 * 1024 * 1024;
fn failure(message: impl Into<String>, code: &'static str, status: u16) -> ContractError {
    ContractError::new(message, code, status)
}
fn corrupt() -> ContractError {
    failure(
        "资料登记损坏，原文件已保留。",
        "CORRUPT_RETRIEVAL_LIBRARY",
        500,
    )
}
fn capacity() -> ContractError {
    failure(
        "此资料范围超过配置容量，原文已保留。",
        "RETRIEVAL_LIBRARY_LIMIT",
        413,
    )
}
fn cancelled() -> Result<(), ContractError> {
    if super::cancellation::is_cancelled() {
        Err(failure("资料操作已取消。", "ABORT_ERR", 499))
    } else {
        Ok(())
    }
}
fn scope(entry: &Value) -> String {
    if entry["scope"] == "user" {
        "user".to_owned()
    } else {
        format!("project:{}", entry["projectId"].as_str().unwrap_or(""))
    }
}
fn safe_integer(value: &Value) -> bool {
    value.as_f64().is_some_and(|value| {
        value.is_finite() && value.fract() == 0.0 && value.abs() <= super::MAX_SAFE_INTEGER as f64
    })
}
fn limit_violation(entry: &Value, limits: &Value, bytes: f64) -> Option<Value> {
    if entry.get("extraction").is_none() {
        return None;
    }
    let output_limit = limits["maximumDocumentOutputBytes"]
        .as_f64()
        .unwrap_or(f64::INFINITY);
    if bytes > output_limit {
        return Some(
            json!({"dimension": "maximumDocumentOutputBytes", "limit": output_limit, "observed": bytes}),
        );
    }
    let extraction = &entry["extraction"];
    let selected_pages = extraction["pages"]
        .as_array()
        .map(|pages| pages.len() as f64)
        .unwrap_or_else(|| extraction["pageCount"].as_f64().unwrap_or(0.0));
    let page_limit = limits["maximumPdfPages"].as_f64().unwrap_or(f64::INFINITY);
    if selected_pages > page_limit {
        Some(
            json!({"dimension": "maximumPdfPages", "limit": page_limit, "observed": selected_pages}),
        )
    } else {
        None
    }
}

fn validate(document: &Value) -> ContractResult {
    if document["schemaVersion"] != 1
        || !safe_integer(&document["revision"])
        || !document["sources"].is_array()
    {
        return Err(failure(
            "资料登记版本无效，原文件已保留。",
            "UNSUPPORTED_RETRIEVAL_SCHEMA",
            409,
        ));
    }
    for entry in document["sources"].as_array().unwrap() {
        identity(&entry["id"])?;
        if ![json!("user"), json!("project")].contains(&entry["scope"])
            || !entry["title"].is_string()
            || ![json!("ready"), json!("deleted")].contains(&entry["status"])
            || !safe_integer(&entry["revision"])
        {
            return Err(corrupt());
        }
        if entry["scope"] == "project" {
            identity(&entry["projectId"])?;
        }
        if let Some(value) = entry.get("extraction") {
            retrieval::apply("storage.retrieval.extraction", &json!({"value": value}))?;
        }
        if let Some(value) = entry.get("fileWindow") {
            retrieval::apply("storage.retrieval.fileWindow", &json!({"value": value}))?;
        }
    }
    Ok(document.clone())
}

fn file_inspect(root: &Path, path: &Path, maximum: u64) -> Result<bool, ContractError> {
    let exists = managed_files::inspect(root, path, false, true, maximum)?;
    if exists {
        let file = File::open(path)
            .map_err(|error| failure(error.to_string(), "RETRIEVAL_LIBRARY_IO_FAILED", 500))?;
        let identity = crate::file_identity::handle_identity(&file)
            .map_err(|error| failure(error.to_string(), "RETRIEVAL_LIBRARY_IO_FAILED", 500))?;
        if identity.links > 1 || identity.is_link {
            return Err(failure(
                "资料路径包含链接，原文件已保留。",
                "UNSAFE_PATH",
                409,
            ));
        }
    }
    Ok(exists)
}

/// The imported registry owns eligibility, deduplication, scope budgets, revocation and snapshot publication.
/// 导入登记拥有可用性、去重、范围预算、撤销及快照发布，派生索引不能恢复已撤销资料。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    let document = &arguments["document"];
    match operation {
        "storage.library.target" => {
            if arguments["scope"] == "user" {
                return Ok(json!({"scope":"user","projectId":null}));
            }
            if arguments["scope"] != "project" {
                return Err(failure("资料范围无效。", "INVALID_RETRIEVAL_SCOPE", 400));
            }
            let project_id = identity(&arguments["projectId"])?;
            let relationship = &arguments["relationship"];
            if relationship["isFolderlessWorkspace"] == true || relationship["isArchived"] == true {
                return Err(failure(
                    "此工作不能添加共享资料。",
                    "RETRIEVAL_SOURCE_UNAVAILABLE",
                    409,
                ));
            }
            Ok(
                json!({"scope":"project","projectId":project_id.as_str().unwrap().to_ascii_lowercase()}),
            )
        }
        "storage.library.list" => {
            let project_id = arguments["projectId"].as_str().map(str::to_ascii_lowercase);
            let sources = document["sources"].as_array().ok_or_else(corrupt)?;
            let items=sources.iter().enumerate().filter(|(_,entry)|entry["status"]!="deleted" &&
                (entry["scope"]=="user" || project_id.as_deref().is_some_and(|id|entry["projectId"]==id)))
                .map(|(index,entry)|{
                    let mut value=json!({"id":entry["id"],"title":entry["title"],"scope":entry["scope"],"projectId":entry["projectId"],
                        "path":entry["originalPath"],"revision":entry["revision"],"status":entry["status"]});
                    if let Some(error)=arguments["errors"].get(entry["id"].as_str().unwrap_or("")) {
                        value["status"]=json!("error");value["error"]=error.clone();
                    }
                    json!({"index":index,"value":value})
                }).collect::<Vec<_>>();
            Ok(json!({"revision":document["revision"],"items":items}))
        }
        "storage.library.validate" => validate(document),
        "storage.library.limitViolation" => Ok(limit_violation(
            &arguments["entry"],
            &arguments["limits"],
            arguments["bytes"].as_f64().unwrap_or(0.0),
        )
        .unwrap_or(Value::Null)),
        "storage.library.select" => {
            let scopes = arguments["scopes"].as_array().cloned().unwrap_or_default();
            Ok(json!(
                document["sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry["status"] != "deleted"
                        && scopes.contains(&json!(scope(entry)))
                        && arguments.get("id").is_none_or(|id| *id == entry["id"])
                        && arguments
                            .get("revision")
                            .is_none_or(|revision| *revision == entry["revision"]))
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            ))
        }
        "storage.library.addition" => {
            let mut next = document.clone();
            let target = &arguments["target"];
            let entry = &arguments["entry"];
            let sources = next["sources"].as_array_mut().ok_or_else(corrupt)?;
            let bytes = entry["storedBytes"].as_f64().unwrap_or(0.0);
            if bytes
                > arguments["limits"]["maximumSourceBytes"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .min(2.0 * 1024.0 * 1024.0)
            {
                return Err(failure("资料文本过大。", "INVALID_RETRIEVAL_SOURCE", 400));
            }
            if limit_violation(entry, &arguments["limits"], bytes).is_some() {
                return Err(capacity());
            }
            let existing = sources.iter().find(|entry| {
                entry["status"] != "deleted"
                    && entry["scope"] == target["scope"]
                    && entry["projectId"] == target["projectId"]
                    && arguments["existingKeys"][entry["id"].as_str().unwrap_or("")]
                        == arguments["duplicateKey"]
            });
            if let Some(existing) = existing {
                return Ok(json!({"document": document, "entry": existing, "created": false}));
            }
            let same_scope: Vec<&Value> = sources
                .iter()
                .filter(|entry| {
                    entry["status"] != "deleted"
                        && entry["scope"] == target["scope"]
                        && entry["projectId"] == target["projectId"]
                })
                .collect();
            if same_scope.len() as f64 + 1.0
                > arguments["limits"]["maximumFiles"].as_f64().unwrap_or(0.0)
                || same_scope
                    .iter()
                    .map(|entry| entry["storedBytes"].as_f64().unwrap_or(0.0))
                    .sum::<f64>()
                    + bytes
                    > arguments["limits"]["maximumTotalBytes"]
                        .as_f64()
                        .unwrap_or(0.0)
            {
                return Err(capacity());
            }
            sources.push(entry.clone());
            Ok(json!({"document": next, "entry": entry, "created": true}))
        }
        "storage.library.remove" => {
            let mut next = document.clone();
            let source = next["sources"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["id"] == arguments["id"])
                .ok_or_else(|| failure("资料不存在。", "RETRIEVAL_SOURCE_UNAVAILABLE", 404))?;
            if arguments
                .get("expectedRevision")
                .is_some_and(|revision| *revision != source["revision"])
            {
                return Err(failure(
                    "资料已变化，请重新读取。",
                    "RETRIEVAL_CONFIG_CONFLICT",
                    409,
                ));
            }
            let changed = source["status"] != "deleted";
            if changed {
                source["status"] = json!("deleted");
                source["revision"] = json!(source["revision"].as_f64().unwrap() + 1.0);
                source["deletedAt"] = arguments["now"].clone();
            }
            let mut entry = source.clone();
            entry["scopeKey"] = json!(scope(&entry));
            if changed {
                next["revision"] = json!(next["revision"].as_f64().unwrap() + 1.0);
            }
            Ok(json!({"document": next, "entry": entry, "changed": changed}))
        }
        "storage.library.describe" => {
            let scopes = arguments["scopes"].as_array().unwrap();
            let limits = &arguments["limits"];
            let projects = &arguments["projects"];
            let mut usage: HashMap<String, (usize, f64)> = HashMap::new();
            let mut selected = Vec::new();
            let mut discovered = 0;
            let mut skipped = 0;
            let mut diagnostics = Vec::new();
            let mut diagnostic_keys = HashSet::new();
            for (index, entry) in document["sources"].as_array().unwrap().iter().enumerate() {
                cancelled()?;
                let scope = scope(entry);
                if entry["status"] == "deleted" || !scopes.contains(&json!(scope)) {
                    continue;
                }
                discovered += 1;
                if entry["scope"] == "project"
                    && projects[entry["projectId"].as_str().unwrap_or("")] != true
                {
                    continue;
                }
                let current = usage.get(&scope).copied().unwrap_or((0, 0.0));
                let bytes = entry["storedBytes"].as_f64().unwrap_or(0.0);
                let proposed = (current.0 + 1, current.1 + bytes);
                let violation = limit_violation(entry, limits, bytes).or_else(|| {
                    [("maximumSourceBytes", bytes), ("maximumFiles", proposed.0 as f64), ("maximumTotalBytes", proposed.1)].into_iter().find(|(dimension, observed)| *observed > limits[*dimension].as_f64().unwrap_or(0.0)).map(|(dimension, observed)| json!({"dimension": dimension, "limit": limits[dimension], "observed": observed}))
                });
                if let Some(mut violation) = violation {
                    if arguments["allowPartial"] != true {
                        return Err(capacity());
                    }
                    skipped += 1;
                    let dimension = violation["dimension"].as_str().unwrap().to_owned();
                    if diagnostics.len() < 8 && diagnostic_keys.insert((dimension, scope.clone())) {
                        violation["scopeKey"] = json!(scope);
                        diagnostics.push(violation);
                    }
                    continue;
                }
                usage.insert(scope, proposed);
                selected.push(index);
            }
            Ok(
                json!({"selected": selected, "coverage": {"complete": skipped == 0, "discovered": discovered, "admitted": selected.len(), "skipped": skipped, "limits": diagnostics}}),
            )
        }
        _ => apply_io(operation, arguments),
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn project_admission_and_listing_preserve_revocation_and_scope() {
        let target = apply(
            "storage.library.target",
            &json!({"scope":"project","projectId":"AbC"}),
        )
        .unwrap();
        assert_eq!(target, json!({"scope":"project","projectId":"abc"}));
        let rejected = apply(
            "storage.library.target",
            &json!({"scope":"project","projectId":"abc","relationship":{"isArchived":true}}),
        )
        .unwrap_err();
        assert_eq!(rejected.code, "RETRIEVAL_SOURCE_UNAVAILABLE");
        let row = |id: &str, scope: &str, project_id: Value, status: &str| json!({"id":id,"scope":scope,"projectId":project_id,"status":status,"title":id,"originalPath":"original.txt","revision":1});
        let result=apply("storage.library.list",&json!({"projectId":"ABC","errors":{"visible":"READ_FAILED"},"document":{"revision":5,"sources":[
            row("global","user",Value::Null,"ready"),row("visible","project",json!("abc"),"ready"),
            row("other","project",json!("other"),"ready"),row("deleted","user",Value::Null,"deleted")
        ]}})).unwrap();
        assert_eq!(result["revision"], 5);
        assert_eq!(result["items"].as_array().unwrap().len(), 2);
        assert_eq!(result["items"][1]["index"], 1);
        assert_eq!(result["items"][1]["value"]["status"], "error");
        assert_eq!(result["items"][1]["value"]["error"], "READ_FAILED");
        assert_eq!(result["items"][1]["value"]["path"], "original.txt");
    }
}

fn apply_io(operation: &str, arguments: &Value) -> ContractResult {
    cancelled()?;
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let folder = root.join("Knowledge");
    let registry = folder.join("catalog.json");
    managed_files::inspect(root, &folder, true, false, u64::MAX)?;
    if operation == "storage.library.readRegistry" {
        if file_inspect(root, &registry, u64::MAX)?
            && fs::metadata(&registry).map_err(|_| corrupt())?.len() > MAXIMUM_REGISTRY_BYTES
        {
            return Err(capacity());
        }
        return managed_files::apply(
            "storage.files.read",
            &json!({"root": root, "path": registry, "maximumBytes": MAXIMUM_REGISTRY_BYTES}),
        );
    }
    if operation == "storage.library.publish" {
        let raw = arguments["raw"].as_str().ok_or_else(corrupt)?;
        if raw.len() as u64 > MAXIMUM_REGISTRY_BYTES {
            return Err(capacity());
        }
        let projection: Value = serde_json::from_str(&super::raw_json::scalar_projection(raw))
            .map_err(|_| corrupt())?;
        validate(&projection)?;
        file_inspect(root, &registry, MAXIMUM_REGISTRY_BYTES)?;
        return managed_files::apply(
            "storage.files.commit",
            &json!({"root": root, "path": registry, "raw": raw, "expectedHash": arguments["expectedHash"], "maximumBytes": MAXIMUM_REGISTRY_BYTES}),
        );
    }
    let id = identity(&arguments["id"])?;
    let source_folder = folder.join(id.as_str().unwrap()).join("source");
    let path = source_folder.join("document.txt");
    if operation == "storage.library.stage" {
        managed_files::inspect(root, &source_folder, true, false, u64::MAX)?;
        let raw = arguments["text"].as_str().ok_or_else(corrupt)?;
        if raw.len() > 2 * 1024 * 1024 {
            return Err(capacity());
        }
        file_inspect(root, &path, 2 * 1024 * 1024)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| failure(error.to_string(), "RETRIEVAL_LIBRARY_IO_FAILED", 500))?;
        let result = (|| {
            file.write_all(raw.as_bytes())
                .map_err(|error| failure(error.to_string(), "RETRIEVAL_LIBRARY_IO_FAILED", 500))?;
            file.sync_all()
                .map_err(|error| failure(error.to_string(), "RETRIEVAL_LIBRARY_IO_FAILED", 500))?;
            cancelled()?;
            Ok(json!({"hash": managed_files::digest(raw.as_bytes())}))
        })();
        if result.is_err() {
            drop(file);
            fs::remove_file(path)
                .map_err(|error| failure(error.to_string(), "RETRIEVAL_LIBRARY_IO_FAILED", 500))?;
        }
        return result;
    }
    if operation == "storage.library.readSnapshot" {
        if file_inspect(root, &path, u64::MAX)?
            && fs::metadata(&path).map_err(|_| corrupt())?.len() > 2 * 1024 * 1024
        {
            return Err(failure("导入资料异常。", "INVALID_RETRIEVAL_SOURCE", 409));
        }
        return managed_files::apply(
            "storage.files.read",
            &json!({"root": root, "path": path, "maximumBytes": 2 * 1024 * 1024}),
        );
    }
    if operation == "storage.library.discard" {
        if file_inspect(root, &path, 2 * 1024 * 1024)? {
            let bytes = managed_files::read(root, &path, 2 * 1024 * 1024)?.unwrap();
            if arguments["hash"].as_str() != Some(managed_files::digest(&bytes).as_str()) {
                return Err(failure(
                    "待回滚快照已变化，原文已保留。",
                    "STALE_RETRIEVAL_SOURCE",
                    409,
                ));
            }
            fs::remove_file(path)
                .map_err(|error| failure(error.to_string(), "RETRIEVAL_LIBRARY_IO_FAILED", 500))?;
        }
        return Ok(Value::Null);
    }
    Err(failure(
        "未知资料服务操作。",
        "INVALID_RETRIEVAL_SOURCE",
        400,
    ))
}
