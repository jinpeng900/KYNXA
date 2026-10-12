use super::{ContractError, ContractResult, managed_files, retrieval};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

const MAXIMUM_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;
const META_FIELDS: &[&str] = &["sizeBytes", "mtimeMs", "ctimeMs", "device", "inode"];
fn invalid(status: u16) -> ContractError {
    ContractError::new(
        "索引文件清单损坏，原记录已保留。",
        "INVALID_RETRIEVAL_MANIFEST",
        status,
    )
}
fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|value| value.is_finite())
}
fn integral(value: &Value, maximum: u64) -> bool {
    super::safe_integer(value, 0, maximum)
}

fn binding(value: &Value, arguments: &Value) -> ContractResult {
    if !value["projectId"]
        .as_str()
        .is_some_and(|value| !value.is_empty() && value.encode_utf16().count() <= 256)
        || !value["root"]
            .as_str()
            .is_some_and(|value| Path::new(value).is_absolute())
        || !integral(&value["bindingRevision"], super::MAX_SAFE_INTEGER)
    {
        return Err(invalid(400));
    }
    Ok(
        json!({"projectId": arguments.get("projectIdLower").filter(|value| value.is_string()).cloned().unwrap_or_else(|| json!(value["projectId"].as_str().unwrap().to_lowercase())), "root": value["root"], "bindingRevision": value["bindingRevision"]}),
    )
}

fn windows(file: &Value) -> Result<Option<Value>, ContractError> {
    let Some(windows) = file.get("windows") else {
        return Ok(None);
    };
    let windows = windows
        .as_array()
        .filter(|windows| !windows.is_empty() && windows.len() <= 4096)
        .ok_or_else(|| invalid(409))?;
    for window in windows {
        retrieval::apply("storage.retrieval.fileWindow", &json!({"value": window}))?;
    }
    let first = &windows[0];
    let mut offset = json!(0);
    let mut byte = first["startByte"].clone();
    let mut line = json!(1);
    let mut text_bytes = 0u64;
    if if first["encoding"] == "utf-8" {
        ![json!(0), json!(3)].contains(&byte)
    } else {
        byte != 2
    } {
        return Err(invalid(409));
    }
    for window in windows {
        if window["startOffset"] != offset
            || window["startByte"] != byte
            || window["startLine"] != line
            || window["textContentHash"] != file["contentHash"]
            || window["encoding"] != first["encoding"]
            || window["rawContentHash"] != first["rawContentHash"]
            || window["totalCharacters"] != first["totalCharacters"]
            || META_FIELDS
                .iter()
                .any(|field| window["metadata"][field] != file["metadata"][field])
        {
            return Err(invalid(409));
        }
        offset = window["endOffset"].clone();
        byte = window["endByte"].clone();
        line = window["endLine"].clone();
        text_bytes += window["textBytes"].as_u64().unwrap_or(0);
    }
    if offset != first["totalCharacters"]
        || byte != file["metadata"]["sizeBytes"]
        || json!(text_bytes) != file["textBytes"]
    {
        return Err(invalid(409));
    }
    Ok(Some(json!(windows)))
}

fn document_windows(file: &Value, first_matches: &Value) -> Result<Option<Value>, ContractError> {
    let Some(windows) = file.get("documentWindows") else {
        return Ok(None);
    };
    let windows = windows
        .as_array()
        .filter(|windows| !windows.is_empty() && windows.len() <= 100_000)
        .ok_or_else(|| invalid(409))?;
    let mut next_page = 1u64;
    let mut total_bytes = 0u64;
    let mut raw_hash = Value::Null;
    let mut page_count = Value::Null;
    let mut normalized = Vec::new();
    for window in windows {
        let extraction = retrieval::apply(
            "storage.retrieval.extraction",
            &json!({"value": window["extraction"]}),
        )?;
        if extraction["pageWindow"].is_null()
            || extraction["pageWindow"]["startPage"].as_u64() != Some(next_page)
            || !raw_hash.is_null() && extraction["rawContentHash"] != raw_hash
            || !page_count.is_null() && extraction["pageCount"] != page_count
            || !super::hash(&window["contentHash"])
            || !integral(&window["textBytes"], 2 * 1024 * 1024)
        {
            return Err(invalid(409));
        }
        if raw_hash.is_null() {
            raw_hash = extraction["rawContentHash"].clone();
        }
        if page_count.is_null() {
            page_count = extraction["pageCount"].clone();
        }
        next_page = extraction["pageWindow"]["endPage"].as_u64().unwrap() + 1;
        total_bytes += window["textBytes"].as_u64().unwrap();
        normalized.push(json!({"contentHash": window["contentHash"], "textBytes": window["textBytes"], "extraction": extraction}));
    }
    if file["textBytes"] != total_bytes
        || file["contentHash"] != normalized[0]["contentHash"]
        || first_matches != true
        || file["documentComplete"] != json!(next_page == page_count.as_u64().unwrap() + 1)
    {
        return Err(invalid(409));
    }
    Ok(Some(json!(normalized)))
}

fn files(arguments: &Value) -> ContractResult {
    let input = arguments["files"]
        .as_array()
        .filter(|files| files.len() <= 100_000)
        .ok_or_else(|| invalid(400))?;
    let mut names = HashSet::new();
    let mut normalized = Vec::new();
    for (index, file) in input.iter().enumerate() {
        let path = file["relativePath"]
            .as_str()
            .filter(|path| {
                !path.is_empty()
                    && path.encode_utf16().count() <= 4096
                    && !Path::new(path).has_root()
                    && !path.chars().any(|character| character <= '\u{1f}')
                    && path
                        .split(['\\', '/'])
                        .all(|part| ![".", "..", ""].contains(&part))
            })
            .ok_or_else(|| invalid(409))?;
        if !names.insert(path)
            || !super::hash(&file["contentHash"])
            || !integral(&file["textBytes"], 1024 * 1024 * 1024)
            || !file["metadata"].is_object()
            || META_FIELDS
                .iter()
                .any(|field| number(&file["metadata"][field]).is_none())
            || ["sizeBytes", "device", "inode"]
                .iter()
                .any(|field| number(&file["metadata"][field]).unwrap_or(-1.0) < 0.0)
        {
            return Err(invalid(409));
        }
        let mut result = json!({"relativePath": path, "contentHash": file["contentHash"], "textBytes": file["textBytes"], "metadata": {}});
        for field in META_FIELDS {
            result["metadata"][field] = file["metadata"][field].clone();
        }
        if let Some(extraction) = file.get("extraction") {
            result["extraction"] = retrieval::apply(
                "storage.retrieval.extraction",
                &json!({"value": extraction}),
            )?;
        }
        if let Some(windows) = windows(file)? {
            result["windows"] = windows;
        }
        if let Some(windows) = document_windows(file, &arguments["firstExtractionMatches"][index])?
        {
            result["documentWindows"] = windows;
            result["documentComplete"] = file["documentComplete"].clone();
        }
        normalized.push(result);
    }
    Ok(json!(normalized))
}

/// Native storage verifies bounded manifests and their path identity; no scanned metadata grants source authority.
/// 原生存储核验有界清单及路径身份，任何扫描元信息都不能授予来源权威。
pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    if operation == "storage.manifest.binding" {
        return binding(&arguments["value"], arguments);
    }
    if operation == "storage.manifest.files" {
        return files(arguments);
    }
    if operation == "storage.manifest.document" {
        if arguments["schemaVersion"] != 1
            || arguments["actualBindingRaw"] != arguments["expectedBindingRaw"]
        {
            return Err(invalid(409));
        }
        return Ok(Value::Null);
    }
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let key = arguments["key"]
        .as_str()
        .filter(|_| super::hash(&arguments["key"]))
        .ok_or_else(|| invalid(400))?;
    let folder = root.join("Retrieval/SourceManifests");
    let path = folder.join(format!("{key}.json"));
    let exists = managed_files::inspect(root, &path, false, true, u64::MAX)
        .map_err(|_| ContractError::new("索引清单路径包含链接。", "UNSAFE_TOOL_PATH", 403))?;
    if exists {
        let file = File::open(&path).map_err(|_| invalid(409))?;
        let identity = crate::file_identity::handle_identity(&file).map_err(|_| invalid(409))?;
        if identity.links != 1 || identity.is_link {
            return Err(ContractError::new(
                "索引清单路径已经变化。",
                "UNSAFE_TOOL_PATH",
                403,
            ));
        }
        if file.metadata().map_err(|_| invalid(409))?.len() > MAXIMUM_MANIFEST_BYTES {
            return Err(invalid(409));
        }
    }
    if operation == "storage.manifest.read" {
        return managed_files::apply(
            "storage.files.read",
            &json!({"root": root, "path": path, "maximumBytes": MAXIMUM_MANIFEST_BYTES}),
        );
    }
    if operation != "storage.manifest.write" {
        return Err(invalid(400));
    }
    let raw = arguments["raw"].as_str().ok_or_else(|| invalid(400))?;
    if raw.len() as u64 + 1 > MAXIMUM_MANIFEST_BYTES {
        return Err(invalid(413));
    }
    managed_files::inspect(root, &folder, true, false, u64::MAX)?;
    managed_files::apply(
        "storage.files.commit",
        &json!({"root": root, "path": path, "raw": raw, "maximumBytes": MAXIMUM_MANIFEST_BYTES}),
    )?;
    Ok(json!({"committed": true, "fileCount": arguments["fileCount"]}))
}
