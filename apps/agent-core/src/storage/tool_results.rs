use super::{
    ContractError, ContractResult, identity, managed_files, raw_json, safe_integer, utf16_length,
};
use serde_json::{Value, json};
use std::path::Path;

const RESULT_BYTES: u64 = 8 * 1024 * 1024;
const DOCUMENT_BYTES: u64 = RESULT_BYTES + 8192;

fn failure(message: &str, code: &'static str, status: u16) -> ContractError {
    ContractError::new(message, code, status)
}

fn invalid() -> ContractError {
    failure("工具结果身份或结构无效。", "INVALID_TOOL_RESULT", 400)
}
fn corrupt() -> ContractError {
    failure(
        "工具结果身份、结构或完整性无效。",
        "CORRUPT_TOOL_RESULT",
        500,
    )
}

fn same_id(left: &Value, right: &Value) -> Result<bool, ContractError> {
    Ok(identity(left)?.as_str().unwrap().to_ascii_lowercase()
        == identity(right)?.as_str().unwrap().to_ascii_lowercase())
}

fn result_id(value: &Value) -> Result<String, ContractError> {
    let value = value
        .as_str()
        .ok_or_else(|| failure("工具结果引用无效。", "INVALID_TOOL_RESULT_REFERENCE", 400))?;
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || bytes[14] != b'4'
        || !matches!(bytes[19].to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b')
        || !bytes.iter().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(failure(
            "工具结果引用无效。",
            "INVALID_TOOL_RESULT_REFERENCE",
            400,
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn depth(value: &Value, level: usize) -> Result<(), ContractError> {
    if value.is_object() || value.is_array() {
        if level > 64 {
            return Err(failure("工具结果嵌套过深。", "INVALID_TOOL_RESULT", 400));
        }
        if let Some(value) = value.as_object() {
            for child in value.values() {
                depth(child, level + 1)?;
            }
        }
        if let Some(value) = value.as_array() {
            for child in value {
                depth(child, level + 1)?;
            }
        }
    }
    Ok(())
}

fn validate_archive(arguments: &Value) -> ContractResult {
    let document = &arguments["document"];
    if !matches!(document["version"].as_u64(), Some(1 | 2))
        || document["id"] != arguments["id"]
        || !document["conversationId"].is_string()
        || !same_id(&document["conversationId"], &arguments["conversationId"])?
        || !document["requestId"].is_string()
        || !document["toolCallId"].is_string()
        || !document["toolName"].is_string()
        || !safe_integer(&document["bytes"], 0, RESULT_BYTES)
        || !document["canonical"].is_object()
        || arguments["verifyDigest"] == true
            && (document["bytes"] != arguments["canonicalBytes"]
                || document["sha256"] != arguments["integrityHash"]
                || document["version"] == 2 && arguments["environmentMatches"] != true)
    {
        return Err(corrupt());
    }
    depth(&document["canonical"], 0)?;
    Ok(Value::Null)
}

fn projection(
    value: &Value,
    path: &mut Vec<String>,
    kind: &str,
    include_media: bool,
    removals: &mut Vec<Value>,
    media: &mut Vec<Value>,
    level: usize,
) -> Result<(), ContractError> {
    if let Some(items) = value.as_array() {
        for (index, item) in items.iter().enumerate() {
            path.push(index.to_string());
            projection(
                item,
                path,
                if kind == "content" { "block" } else { "value" },
                include_media,
                removals,
                media,
                level + 1,
            )?;
            path.pop();
        }
    } else if let Some(fields) = value.as_object() {
        if level > 64 {
            return Err(invalid());
        }
        let binary = kind == "block"
            && matches!(value["type"].as_str(), Some("image" | "audio"))
            && value["data"].is_string();
        let blob = kind == "resource" && value["blob"].is_string();
        for (key, child) in fields {
            path.push(key.clone());
            if key == "_meta"
                || path.len() == 1 && key == "executionEnvironment"
                || !include_media && (binary && key == "data" || blob && key == "blob")
            {
                removals.push(json!(path));
            } else {
                let next = if path.len() == 1 && key == "content" && child.is_array() {
                    "content"
                } else if kind == "block" && value["type"] == "resource" && key == "resource" {
                    "resource"
                } else {
                    "value"
                };
                projection(child, path, next, include_media, removals, media, level + 1)?;
            }
            path.pop();
        }
        if !include_media && (binary || blob) {
            let key = if binary { "data" } else { "blob" };
            let pointer = format!(
                "/{}/{}",
                path.iter()
                    .map(|part| part.replace('~', "~0").replace('/', "~1"))
                    .collect::<Vec<_>>()
                    .join("/"),
                key
            );
            media.push(json!({"path": path, "key": key, "pointer": if path.is_empty() { format!("/{key}") } else { pointer }}));
        }
    }
    Ok(())
}

fn raw_units(arguments: &Value) -> Result<Vec<u16>, ContractError> {
    raw_json::utf16_identity(arguments["sourceRaw"].as_str().unwrap_or("")).ok_or_else(invalid)
}

fn integer(value: &Value, minimum: u64, maximum: u64) -> Result<usize, ContractError> {
    if !safe_integer(value, minimum, maximum) {
        return Err(failure(
            "工具结果分页或预览参数无效。",
            "INVALID_TOOL_ARGUMENTS",
            400,
        ));
    }
    Ok(value.as_f64().unwrap() as usize)
}

fn reference(arguments: &Value) -> ContractResult {
    let receipt = &arguments["reference"];
    let evidence = arguments["evidence"] == true;
    if !safe_integer(&receipt["bytes"], 0, RESULT_BYTES)
        || !receipt["sha256"].as_str().is_some_and(|value| {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        || evidence
            && (arguments["owner"]["toolName"] != "knowledge.search"
                || !safe_integer(&arguments["number"], 1, 60))
    {
        return Err(failure(
            "工具历史或证据回执无效。",
            if evidence {
                "INVALID_EVIDENCE_REFERENCE"
            } else {
                "INVALID_TOOL_RESULT_REFERENCE"
            },
            400,
        ));
    }
    if let Some(document) = arguments.get("document") {
        let owner = &arguments["owner"];
        if document["sha256"] != receipt["sha256"]
            || document["bytes"] != receipt["bytes"]
            || !same_id(&document["requestId"], &owner["requestId"])?
            || document["toolCallId"] != owner["toolCallId"]
            || document["toolName"] != owner["toolName"]
        {
            return Err(failure(
                "工具历史回执与归档身份不匹配。",
                "TOOL_RESULT_REFERENCE_MISMATCH",
                409,
            ));
        }
        if evidence {
            let items = document["canonical"]["structuredContent"]["items"]
                .as_array()
                .ok_or_else(|| failure("证据条目不存在。", "EVIDENCE_REFERENCE_NOT_FOUND", 404))?;
            let number = arguments["number"].as_u64().unwrap() as usize;
            if items.len() > 60 || number > items.len() {
                return Err(failure(
                    "证据条目不存在。",
                    "EVIDENCE_REFERENCE_NOT_FOUND",
                    404,
                ));
            }
            return Ok(json!({"index": number - 1}));
        }
    }
    Ok(Value::Null)
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    apply_service(operation, arguments).map_err(|error| {
        if error.code == "INVALID_MEMORY_PATH" {
            failure(
                "工具结果文件包含链接或不安全结构。",
                "UNSAFE_TOOL_PATH",
                403,
            )
        } else {
            error
        }
    })
}

fn apply_service(operation: &str, arguments: &Value) -> ContractResult {
    match operation {
        "storage.toolResults.id" => return result_id(&arguments["value"]).map(Value::String),
        "storage.toolResults.depth" => {
            depth(&arguments["value"], 0)?;
            return Ok(Value::Null);
        }
        "storage.toolResults.scope" => {
            let relationship = &arguments["relationship"];
            let context = &arguments["context"];
            let root = Path::new(arguments["root"].as_str().unwrap_or(""));
            let session = Path::new(relationship["sessionDirectory"].as_str().unwrap_or(""));
            if !session.starts_with(root) {
                return Err(failure("工具结果存储路径无效。", "UNSAFE_TOOL_PATH", 403));
            }
            if arguments["checkProject"] == true
                && context.get("projectId").is_some()
                && (context["projectId"].is_null() != relationship["projectId"].is_null()
                    || !context["projectId"].is_null()
                        && !same_id(&context["projectId"], &relationship["projectId"])?)
            {
                return Err(failure("聊天工作范围已变化。", "WORKSPACE_CHANGED", 409));
            }
            if arguments["allowArchived"] != true
                && (relationship["isArchived"] == true || relationship["projectArchived"] == true)
            {
                return Err(failure(
                    "已归档聊天的工具结果仅供本机历史查看。",
                    "TOOL_RESULT_ARCHIVED",
                    409,
                ));
            }
            return Ok(Value::Null);
        }
        "storage.toolResults.prepare" => {
            let call = &arguments["identity"];
            if !call["id"].as_str().is_some_and(|value| {
                !value.is_empty()
                    && utf16_length(value) <= 200
                    && !value.contains(['\0', '\r', '\n'])
            }) || !call["name"]
                .as_str()
                .is_some_and(|value| !value.is_empty() && utf16_length(value) <= 256)
                || !arguments["canonical"].is_object()
            {
                return Err(invalid());
            }
            if arguments["bytes"].as_u64().unwrap_or(u64::MAX) > RESULT_BYTES {
                return Err(failure(
                    "完整工具结果超过 8 MiB 保存上限。",
                    "TOOL_RESULT_TOO_LARGE",
                    413,
                ));
            }
            depth(&arguments["canonical"], 0)?;
            return Ok(Value::Null);
        }
        "storage.toolResults.validate" => return validate_archive(arguments),
        "storage.toolResults.projection" => {
            depth(&arguments["value"], 0)?;
            let mut removals = Vec::new();
            let mut media = Vec::new();
            projection(
                &arguments["value"],
                &mut Vec::new(),
                "value",
                arguments["includeMedia"] == true,
                &mut removals,
                &mut media,
                0,
            )?;
            return Ok(
                json!({"removals": removals, "media": media, "wrap": !arguments["value"].is_object()}),
            );
        }
        "storage.toolResults.page" => {
            let source = raw_units(arguments)?;
            let mut offset = integer(&arguments["offset"], 0, DOCUMENT_BYTES)?;
            let limit = integer(&arguments["limit"], 1, 16000)?;
            if arguments["projection"] != "public" {
                return Err(failure(
                    "模型只能读取公开工具结果。",
                    "INVALID_TOOL_RESULT_PROJECTION",
                    403,
                ));
            }
            if offset > source.len() {
                return Err(failure(
                    "工具结果分页位置超出范围。",
                    "INVALID_TOOL_RESULT_OFFSET",
                    400,
                ));
            }
            if offset > 0
                && source
                    .get(offset)
                    .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
                && (0xd800..=0xdbff).contains(&source[offset - 1])
            {
                offset -= 1;
            }
            let mut end = source.len().min(offset + limit);
            if end < source.len() && end > 0 && (0xd800..=0xdbff).contains(&source[end - 1]) {
                end -= 1;
            }
            if end == offset && end < source.len() {
                end = source.len().min(end + 2);
            }
            return Ok(
                json!({"offset": offset, "end": end, "totalCharacters": source.len(),
                    "hasMore": end < source.len(), "truncated": end < source.len()}),
            );
        }
        "storage.toolResults.preview" => {
            let source = raw_units(arguments)?;
            let maximum = integer(&arguments["maximum"], 256, 65536)?;
            if source.len() <= maximum {
                return Ok(json!({"complete": true}));
            }
            let fixed = arguments["envelopeLength"].as_u64().ok_or_else(invalid)? as usize;
            if fixed > maximum {
                return Err(failure(
                    "工具结果引用超过预览预算。",
                    "INVALID_TOOL_RESULT_REFERENCE",
                    400,
                ));
            }
            let mut length = fixed;
            let mut end = 0;
            while end < source.len() {
                let unit = source[end];
                let pair = (0xd800..=0xdbff).contains(&unit)
                    && source
                        .get(end + 1)
                        .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit));
                let cost = if pair {
                    2
                } else {
                    match unit {
                        8 | 9 | 10 | 12 | 13 | 34 | 92 => 2,
                        0..=31 | 0xd800..=0xdfff => 6,
                        _ => 1,
                    }
                };
                if length + cost > maximum {
                    break;
                }
                length += cost;
                end += if pair { 2 } else { 1 };
            }
            return Ok(json!({"complete": false, "end": end}));
        }
        "storage.toolResults.reference" => return reference(arguments),
        "storage.toolResults.continuation" => {
            let document = &arguments["document"];
            let value = &document["canonical"]["_meta"]["modelContinuation"];
            return Ok(json!(
                document["toolName"] == "model.continuation"
                    && document["sha256"] == arguments["reference"]["sha256"]
                    && value["version"] == 1
                    && value["prefixFingerprint"] == arguments["prefixFingerprint"]
                    && arguments["storedOriginRaw"] == arguments["originRaw"]
            ));
        }
        _ => {}
    }
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let directory = Path::new(arguments["directory"].as_str().unwrap_or(""));
    let id = result_id(&arguments["id"])?;
    let path = directory.join(format!("{id}.json"));
    if directory.file_name().and_then(|name| name.to_str()) != Some("tool-results")
        || !directory.starts_with(root)
    {
        return Err(failure("工具结果存储路径无效。", "UNSAFE_TOOL_PATH", 403));
    }
    match operation {
        "storage.toolResults.save" => {
            let raw = arguments["raw"].as_str().ok_or_else(invalid)?;
            if raw.len() as u64 > DOCUMENT_BYTES {
                return Err(failure(
                    "工具结果元信息过大。",
                    "TOOL_RESULT_TOO_LARGE",
                    413,
                ));
            }
            let result = managed_files::apply(
                "storage.files.commit",
                &json!({"root": root, "path": path, "raw": raw, "expectedHash": null, "maximumBytes": DOCUMENT_BYTES}),
            );
            match result {
                Err(error) if error.code == "MEMORY_CONFLICT" => {
                    Err(failure("工具结果引用已存在。", "TOOL_RESULT_CONFLICT", 409))
                }
                result => result,
            }
        }
        "storage.toolResults.read" => {
            let maximum = arguments["maximumReadBytes"]
                .as_u64()
                .unwrap_or(DOCUMENT_BYTES)
                .min(DOCUMENT_BYTES);
            let bytes = managed_files::read(root, &path, DOCUMENT_BYTES)
                .map_err(|error| {
                    if error.code == "INVALID_MEMORY_PATH" {
                        failure(
                            "工具结果文件包含链接或不安全结构。",
                            "UNSAFE_TOOL_PATH",
                            403,
                        )
                    } else if error.code == "CORRUPT_MEMORY" || error.code == "MEMORY_CONFLICT" {
                        corrupt()
                    } else {
                        error
                    }
                })?
                .ok_or_else(|| {
                    failure("当前聊天不存在此工具结果。", "TOOL_RESULT_NOT_FOUND", 404)
                })?;
            if bytes.len() as u64 > maximum {
                return Err(failure(
                    "工具结果文件与回执大小不匹配。",
                    "TOOL_RESULT_REFERENCE_MISMATCH",
                    409,
                ));
            }
            Ok(json!({"raw": String::from_utf8_lossy(&bytes)}))
        }
        _ => Err(invalid()),
    }
}
