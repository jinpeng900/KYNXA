use super::{ContractError, ContractResult, identity, raw_json, retrieval, safe_integer};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashMap;

const IDENTITY_FIELDS: &[&str] = &[
    "sourceId",
    "contentHash",
    "sourceRevision",
    "chunkId",
    "chunkHash",
    "derivationSignature",
    "indexSnapshotId",
    "scopeSnapshots",
    "modelSourceRef",
    "canonicalSourceRef",
    "evidenceArchiveId",
];
const PATH_FIELDS: &[&str] = &[
    "path",
    "absolutePath",
    "sourcePath",
    "indexPath",
    "cachePath",
    "root",
    "workspaceRoot",
    "dataRoot",
    "directory",
    "filename",
    "relativePath",
];

fn invalid() -> ContractError {
    ContractError::new(
        "Invalid evidence reference. / 证据引用无效。",
        "INVALID_EVIDENCE_REFERENCE",
        400,
    )
}
fn same_id(left: &Value, right: &Value) -> Result<bool, ContractError> {
    Ok(identity(left)?
        .as_str()
        .unwrap()
        .eq_ignore_ascii_case(identity(right)?.as_str().unwrap()))
}

fn archive(value: &Value) -> Result<[u8; 16], ContractError> {
    let value = value.as_str().ok_or_else(invalid)?;
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
        return Err(invalid());
    }
    let compact: String = value
        .chars()
        .filter(|character| *character != '-')
        .collect();
    let mut result = [0; 16];
    for (index, value) in result.iter_mut().enumerate() {
        *value =
            u8::from_str_radix(&compact[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(result)
}

fn source_ref(id: &Value, number: &Value) -> ContractResult {
    let bytes = archive(id)?;
    if !safe_integer(number, 1, 60) {
        return Err(invalid());
    }
    let number = number.as_f64().unwrap() as u8;
    let digit = |value: u8| {
        if value < 10 {
            (b'0' + value) as char
        } else {
            (b'a' + value - 10) as char
        }
    };
    Ok(json!(format!(
        "ev1:{}:{}{}",
        URL_SAFE_NO_PAD.encode(bytes),
        digit(number / 36),
        digit(number % 36)
    )))
}

fn parse(value: &Value) -> ContractResult {
    let value = value.as_str().ok_or_else(invalid)?;
    let parts: Vec<_> = value.split(':').collect();
    if parts.len() != 3 || parts[0] != "ev1" || parts[1].len() != 22 || parts[2].len() != 2 {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(parts[1]).map_err(|_| invalid())?;
    if bytes.len() != 16 || URL_SAFE_NO_PAD.encode(&bytes) != parts[1] {
        return Err(invalid());
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let id = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    let number = u32::from_str_radix(parts[2], 36).map_err(|_| invalid())?;
    if source_ref(&json!(id), &json!(number))? != value {
        return Err(invalid());
    }
    Ok(json!({"archiveId": id, "referenceNumber": number}))
}

fn validate(arguments: &Value) -> ContractResult {
    if !arguments["item"].is_object() {
        return Err(invalid());
    }
    let descriptor = &arguments["descriptor"];
    retrieval::apply("storage.retrieval.reference", &json!({"value": descriptor}))
        .map_err(|_| invalid())?;
    let encoded = arguments["item"]["sourceRef"]
        .as_str()
        .and_then(|value| value.strip_prefix("rag1:"))
        .ok_or_else(invalid)?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
    if URL_SAFE_NO_PAD.encode(bytes) != encoded
        || arguments["equalFields"]
            .as_array()
            .is_none_or(|fields| fields.iter().any(|value| value != true))
    {
        return Err(invalid());
    }
    Ok(Value::Null)
}

fn absolute(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("\\\\")
        || value
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
        || value.as_bytes().get(1) == Some(&b':')
            && value
                .as_bytes()
                .get(2)
                .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
            && value.as_bytes()[0].is_ascii_alphabetic()
}

fn key(path: &[String]) -> String {
    serde_json::to_string(path).unwrap()
}

fn units(value: &str, path: &[String], raw: &Value) -> Vec<u16> {
    raw.get(key(path))
        .and_then(Value::as_str)
        .and_then(raw_json::utf16_identity)
        .unwrap_or_else(|| value.encode_utf16().collect())
}

fn inspect(
    value: &Value,
    path: &mut Vec<String>,
    known: &mut Vec<Vec<u16>>,
    raw: &Value,
    depth: usize,
) -> Result<(), ContractError> {
    if depth > 64 {
        return Err(invalid());
    }
    if let Some(fields) = value.as_object() {
        for (name, child) in fields {
            path.push(name.clone());
            if (PATH_FIELDS.contains(&name.as_str()) || name == "title")
                && child.as_str().is_some_and(absolute)
            {
                let path_units = units(child.as_str().unwrap(), path, raw);
                if !known.contains(&path_units) {
                    known.push(path_units);
                }
            }
            inspect(child, path, known, raw, depth + 1)?;
            path.pop();
        }
    } else if let Some(items) = value.as_array() {
        for (index, child) in items.iter().enumerate() {
            path.push(index.to_string());
            inspect(child, path, known, raw, depth + 1)?;
            path.pop();
        }
    }
    Ok(())
}

fn apply_edits(units: &mut Vec<u16>, edits: &[(usize, usize, String)]) {
    for (start, end, replacement) in edits.iter().rev() {
        units.splice(*start..*end, replacement.encode_utf16());
    }
}

/// Apply redactions at original UTF-16 offsets, preserving the source encoding.
/// 按原始 UTF-16 偏移脱敏，保留来源编码。
fn redactions(mut units: Vec<u16>, known: &[Vec<u16>]) -> Vec<Value> {
    let mut stages = Vec::new();
    for path in known {
        let mut edits = Vec::new();
        let mut index = 0;
        while index + path.len() <= units.len() && !path.is_empty() {
            if &units[index..index + path.len()] == path.as_slice() {
                edits.push((index, index + path.len(), "[local-path]".into()));
                index += path.len();
            } else {
                index += 1;
            }
        }
        if !edits.is_empty() {
            apply_edits(&mut units, &edits);
            stages.push(json!(edits));
        }
    }
    for (pattern, replacement, preserve_prefix) in [
        (
            r#"(?i)'(?:[a-z]:[\\/]|\\\\|/)[^'"\r\n]*'|"(?:[a-z]:[\\/]|\\\\|/)[^'"\r\n]*""#,
            "'[local-path]'",
            false,
        ),
        (
            r"(?i)\((?:[a-z]:[\\/]|/)[^)\r\n]*\)",
            "([local-path])",
            false,
        ),
        (
            r#"(?i)(^|[\p{White_Space}\x{feff}("'=,:])(?:[a-z]:[\\/]|\\\\(?:[?.][\\/]|[^\\/\p{White_Space}\x{feff}'";]+[\\/]))[^'"\r\n;)]*"#,
            "[local-path]",
            true,
        ),
    ] {
        let text = String::from_utf16_lossy(&units);
        let expression = Regex::new(pattern).expect("constant redaction expression");
        let mut edits = Vec::new();
        for captures in expression.captures_iter(&text) {
            let matched = captures.get(0).unwrap();
            let start = if preserve_prefix {
                captures.get(1).unwrap().end()
            } else {
                matched.start()
            };
            edits.push((
                text[..start].encode_utf16().count(),
                text[..matched.end()].encode_utf16().count(),
                replacement.to_owned(),
            ));
        }
        if !edits.is_empty() {
            apply_edits(&mut units, &edits);
            stages.push(json!(edits));
        }
    }
    stages
}

fn projection(
    value: &Value,
    name: &str,
    path: &mut Vec<String>,
    known: &[Vec<u16>],
    raw: &Value,
    facts: &HashMap<String, Value>,
    error_receipt: bool,
    depth: usize,
) -> ContractResult {
    if depth > 64 {
        return Err(invalid());
    }
    if let Some(items) = value.as_array() {
        let mut children = Vec::new();
        for (index, item) in items.iter().enumerate() {
            path.push(index.to_string());
            let plan = projection(
                item,
                name,
                path,
                known,
                raw,
                facts,
                error_receipt,
                depth + 1,
            )?;
            path.pop();
            if !plan.is_null() {
                children.push(json!({"key": index.to_string(), "plan": plan}));
            }
        }
        return Ok(json!({"children": children, "remove": []}));
    }
    if let Some(fields) = value.as_object() {
        let mut children = Vec::new();
        let mut removals = Vec::new();
        for (key_name, child) in fields {
            path.push(key_name.clone());
            if PATH_FIELDS.contains(&key_name.as_str()) && child.as_str().is_some_and(absolute) {
                removals.push(key_name.clone());
            } else {
                let plan = if key_name == "fileWindow" && child["version"] == "text-file-window-v1"
                {
                    json!({"fields": ["version", "startOffset", "endOffset", "totalCharacters", "offsetUnit", "rawContentHash"]})
                } else if key_name == "text" && value["type"] == "text" && child.is_string() {
                    if let Some(fact) = facts.get(&key(path)) {
                        let parsed = &fact["parsed"];
                        let mut parsed_path = vec![format!("json:{}", key(path))];
                        let plan = if error_receipt && parsed.is_string() {
                            Ok(
                                json!({"stages": redactions(units(parsed.as_str().unwrap(), &parsed_path, raw), known)}),
                            )
                        } else {
                            projection(
                                parsed,
                                "",
                                &mut parsed_path,
                                known,
                                raw,
                                facts,
                                error_receipt,
                                depth + 1,
                            )
                        };
                        match plan {
                            Ok(plan) => json!({"json": plan, "fact": key(path)}),
                            Err(_) if error_receipt => {
                                json!({"stages": redactions(units(child.as_str().unwrap(), path, raw), known)})
                            }
                            Err(_) => Value::Null,
                        }
                    } else if error_receipt {
                        json!({"stages": redactions(units(child.as_str().unwrap(), path, raw), known)})
                    } else {
                        Value::Null
                    }
                } else {
                    projection(
                        child,
                        key_name,
                        path,
                        known,
                        raw,
                        facts,
                        error_receipt,
                        depth + 1,
                    )?
                };
                if !plan.is_null() {
                    children.push(json!({"key": key_name, "plan": plan}));
                }
            }
            path.pop();
        }
        return Ok(json!({"children": children, "remove": removals}));
    }
    if let Some(value) = value.as_str() {
        if name == "title" && absolute(value) {
            let value = units(value, path, raw);
            let mut end = value.len();
            while end > 0 && matches!(value[end - 1], 47 | 92) {
                end -= 1;
            }
            let start = value[..end]
                .iter()
                .rposition(|unit| matches!(unit, 47 | 92))
                .map_or(0, |index| index + 1);
            return Ok(json!({"slice": [start, end], "fallback": "[local-source]"}));
        }
        if [
            "message",
            "error",
            "reason",
            "diagnostic",
            "preview",
            "stack",
        ]
        .contains(&name)
        {
            return Ok(json!({"stages": redactions(units(value, path, raw), known)}));
        }
    }
    Ok(Value::Null)
}

fn scope(arguments: &Value) -> ContractResult {
    let relationship = &arguments["relationship"];
    let context = &arguments["context"];
    if context.get("projectId").is_some()
        && (context["projectId"].is_null() != relationship["projectId"].is_null()
            || !context["projectId"].is_null()
                && !same_id(&context["projectId"], &relationship["projectId"])?)
    {
        return Err(ContractError::new(
            "Workspace changed. / 聊天工作范围已变化。",
            "WORKSPACE_CHANGED",
            409,
        ));
    }
    if relationship["isArchived"] == true || relationship["projectArchived"] == true {
        return Err(ContractError::new(
            "Evidence is unavailable in archived chats. / 归档聊天不能回读证据。",
            "TOOL_RESULT_ARCHIVED",
            409,
        ));
    }
    let conversation = identity(&relationship["conversationId"])?;
    let mut allowed = vec![
        json!("user"),
        json!(format!(
            "chat:{}",
            conversation.as_str().unwrap().to_ascii_lowercase()
        )),
    ];
    if relationship["isFolderlessWorkspace"] != true
        && relationship["projectArchived"] != true
        && !relationship["projectId"].is_null()
    {
        let project = identity(&relationship["projectId"])?;
        allowed.push(json!(format!(
            "project:{}",
            project.as_str().unwrap().to_ascii_lowercase()
        )));
    }
    let requested = if arguments["scopes"].is_null() {
        json!(allowed)
    } else {
        arguments["scopes"].clone()
    };
    let scopes = retrieval::apply("storage.retrieval.scopes", &json!({"value": requested}))?;
    if scopes
        .as_array()
        .unwrap()
        .iter()
        .any(|scope| !allowed.contains(scope))
    {
        return Err(ContractError::new(
            "Evidence is outside the current scope. / 证据超出当前范围。",
            "RETRIEVAL_SCOPE_REQUIRED",
            403,
        ));
    }
    Ok(json!({"scopes": scopes, "projectId": relationship["projectId"]}))
}

pub fn apply(operation: &str, arguments: &Value) -> ContractResult {
    match operation {
        "storage.evidence.codecPolicy" => Ok(
            json!({"stringFields": PATH_FIELDS.iter().copied().chain(["title", "message", "error", "reason", "diagnostic", "preview", "stack"]).collect::<Vec<_>>(), "jsonType": "text", "jsonField": "text"}),
        ),
        "storage.evidence.reference" => source_ref(&arguments["archiveId"], &arguments["number"]),
        "storage.evidence.parse" => parse(&arguments["value"]),
        "storage.evidence.validate" => validate(arguments),
        "storage.evidence.scopes" => scope(arguments),
        "storage.evidence.bind" => {
            if arguments["receipt"]["id"] != arguments["parsed"]["archiveId"]
                || arguments["owner"]["toolName"] != "knowledge.search"
            {
                return Err(invalid());
            }
            Ok(Value::Null)
        }
        "storage.evidence.authorize" => {
            if !arguments["scopes"]
                .as_array()
                .is_some_and(|scopes| scopes.contains(&arguments["item"]["scopeKey"]))
            {
                return Err(ContractError::new(
                    "Evidence source is outside the current scope. / 证据来源超出当前范围。",
                    "RETRIEVAL_SOURCE_NOT_FOUND",
                    404,
                ));
            }
            Ok(Value::Null)
        }
        "storage.evidence.trusted" => {
            if !same_id(
                &arguments["owner"]["requestId"],
                &arguments["context"]["requestId"],
            )
            .map_err(|_| invalid())?
            {
                return Err(invalid());
            }
            Ok(Value::Null)
        }
        "storage.evidence.receipt" => {
            if let Some(messages) = arguments["messages"].as_array() {
                for (index, message) in messages.iter().enumerate() {
                    if message["Role"] != "assistant" {
                        continue;
                    }
                    if message["RetrievalResultRef"]["id"] == arguments["archiveId"] {
                        return Ok(json!({"message": index, "activity": null}));
                    }
                    if let Some(activities) = message["ToolActivities"].as_array() {
                        for (activity_index, activity) in activities.iter().enumerate() {
                            if activity["name"] == "knowledge.search"
                                && activity["resultRef"]["id"] == arguments["archiveId"]
                            {
                                return Ok(json!({"message": index, "activity": activity_index}));
                            }
                        }
                    }
                }
            }
            Err(ContractError::new(
                "No formal evidence receipt in this chat. / 当前聊天没有此证据的正式回执。",
                "EVIDENCE_REFERENCE_NOT_FOUND",
                404,
            ))
        }
        "storage.evidence.payload" => {
            let payload = &arguments["value"];
            let items = payload["items"]
                .as_array()
                .filter(|items| items.len() <= 60)
                .ok_or_else(invalid)?;
            if !payload.is_object() {
                return Err(invalid());
            }
            let mut references = Vec::new();
            for index in 0..items.len() {
                references.push(source_ref(&arguments["archiveId"], &json!(index + 1))?);
            }
            Ok(json!({"remove": IDENTITY_FIELDS, "references": references}))
        }
        "storage.evidence.search" => {
            source_ref(&arguments["archiveId"], &json!(1))?;
            let value = &arguments["value"];
            if !value.is_object() {
                return Err(invalid());
            }
            Ok(
                json!({"kind": if value["isError"] == true && !value["structuredContent"]["items"].is_array() { "error" } else if value.get("structuredContent").is_some() { "structured" } else { "payload" }}),
            )
        }
        "storage.evidence.text" => {
            let payload = &arguments["payload"];
            let parsed = &arguments["parsed"];
            Ok(json!(
                parsed["items"]
                    .as_array()
                    .zip(payload["items"].as_array())
                    .is_some_and(|(left, right)| left.len() == right.len()
                        && left
                            .iter()
                            .zip(right)
                            .all(|(left, right)| left["sourceRef"] == right["sourceRef"]))
            ))
        }
        "storage.evidence.projection" => {
            let value = &arguments["value"];
            let raw = &arguments["stringsRaw"];
            let mut known = Vec::new();
            inspect(value, &mut Vec::new(), &mut known, raw, 0)?;
            known.sort_by_key(|path| std::cmp::Reverse(path.len()));
            let facts: HashMap<String, Value> = arguments["jsonFacts"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|fact| (fact["key"].as_str().unwrap_or("").to_owned(), fact.clone()))
                .collect();
            projection(
                value,
                "",
                &mut Vec::new(),
                &known,
                raw,
                &facts,
                value["isError"] == true,
                0,
            )
        }
        _ => Err(invalid()),
    }
}
