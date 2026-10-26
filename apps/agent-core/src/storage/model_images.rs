//! Expiring image payloads are separate from immutable execution receipts.
//! 可过期图像载荷独立于不可变执行回执，只清理本模块创建的旁车。

use super::{ContractError, ContractResult, managed_files};
use base64::Engine;
use serde_json::{Value, json};
use std::{collections::HashMap, path::{Path, PathBuf}, sync::Mutex};

const PAYLOAD_BYTES: u64 = 44 * 1024 * 1024;
const METADATA_BYTES: u64 = 16 * 1024;
const TEMPORARY_MS: i64 = 24 * 60 * 60 * 1000;
const MAXIMUM_LEASE_MS: i64 = 30 * 60 * 1000;
static CLEANUP_POSITIONS: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

fn invalid() -> ContractError {
    ContractError::new("Model image reference is invalid. / 模型图像引用无效。", "INVALID_MODEL_IMAGE", 409)
}

fn image_reference(value: &Value) -> Option<&Value> {
    value.get("kynxaImageRef").filter(|reference| reference["version"] == 1)
}

fn hex_hash(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn metadata(root: &Path, directory: &Path, chat: &str, reference: &Value) -> Result<Option<Value>, ContractError> {
    let id = hex_hash(&reference["id"]).ok_or_else(invalid)?;
    let path = directory.join(format!("{id}.json"));
    let Some(bytes) = managed_files::read(root, &path, METADATA_BYTES)? else { return Ok(None); };
    let metadata: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if metadata["version"] != 1 || metadata["id"] != reference["id"]
        || !metadata["conversationId"].as_str().is_some_and(|owner| owner.eq_ignore_ascii_case(chat))
        || metadata["requestId"] != reference["requestId"] || metadata["sha256"] != reference["sha256"]
        || metadata["bytes"] != reference["bytes"] || metadata["imageBytes"] != reference["imageBytes"]
        || metadata["imageSha256"] != reference["imageSha256"] {
        return Err(invalid());
    }
    Ok(Some(metadata))
}

fn write_metadata(root: &Path, directory: &Path, metadata: &Value) -> Result<(), ContractError> {
    let id = hex_hash(&metadata["id"]).ok_or_else(invalid)?;
    managed_files::atomic_write(root, &directory.join(format!("{id}.json")),
        &serde_json::to_vec(metadata).map_err(|_| invalid())?, METADATA_BYTES)
}

fn available(metadata: &Value, now_ms: i64) -> bool {
    metadata["pinnedReason"].as_str().is_some_and(|reason| !reason.trim().is_empty())
        || metadata["expiresAtMs"].as_i64().is_some_and(|expires| expires > now_ms)
        || metadata["leaseUntilMs"].as_i64().is_some_and(|expires| expires > now_ms)
}

fn opaque(value: &Value) -> bool {
    matches!(value["type"].as_str(), Some("thinking" | "redacted_thinking" | "reasoning"))
}

fn opaque_field(key: &str) -> bool {
    matches!(key, "reasoning" | "reasoning_content" | "reasoning_details" | "thinking" | "signature" | "encrypted_content" | "encryptedContent")
}

fn image_url_container(value: &Value, key: &str) -> bool {
    value["type"] == "image_url" && key == "image_url"
}

fn ollama_images(value: &Value, key: &str, child: &Value) -> bool {
    key == "images" && child.is_array() && value["role"].is_string()
}

fn encoded_image(value: &Value, key: &str, child: &Value, url_container: bool) -> Option<Vec<u8>> {
    let text = child.as_str()?;
    let native = key == "data" && (value["type"] == "image"
        || value["type"] == "base64" && value["media_type"].as_str().is_some_and(|kind| kind.starts_with("image/")));
    let url = key == "image_url" && matches!(value["type"].as_str(), Some("image_url" | "input_image"))
        || url_container && key == "url";
    let encoded = if native { text } else if url && text.starts_with("data:image/") {
        text.split_once(";base64,")?.1
    } else { return None; };
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

pub(crate) fn has_image_payload(value: &Value) -> bool {
    fn visit(value: &Value, url_container: bool) -> bool {
        if image_reference(value).is_some() || opaque(value) { return false; }
        if let Some(items) = value.as_array() { return items.iter().any(|item| visit(item, false)); }
        value.as_object().is_some_and(|fields| fields.iter().any(|(key, child)| !opaque_field(key)
            && (encoded_image(value, key, child, url_container).is_some()
                || ollama_images(value, key, child) && child.as_array().is_some_and(|items| items.iter().any(Value::is_string))
                || visit(child, image_url_container(value, key)))))
    }
    visit(value, false)
}

pub(crate) fn has_references(value: &Value) -> bool {
    image_reference(value).is_some() || !opaque(value) && (value.as_array().is_some_and(|items| items.iter().any(has_references))
        || value.as_object().is_some_and(|fields| fields.iter().any(|(key, child)| !opaque_field(key) && has_references(child))))
}

fn store_image(root: &Path, directory: &Path, chat: &str, request: &str, text: &str, image: &[u8], now_ms: i64) -> ContractResult {
    let payload = text.as_bytes();
    if payload.len() as u64 > PAYLOAD_BYTES { return Err(invalid()); }
    let sha256 = managed_files::digest(payload);
    let id = managed_files::digest(format!("{request}:{sha256}").as_bytes());
    let reference = json!({"version":1,"id":id,"sha256":sha256,"imageSha256":managed_files::digest(image),
        "bytes":payload.len(),"imageBytes":image.len(),"requestId":request});
    let existing = metadata(root, directory, chat, &reference)?;
    let mut record = existing.unwrap_or_else(|| json!({"version":1,"id":id,"conversationId":chat,
        "requestId":request,"sha256":reference["sha256"],"imageSha256":reference["imageSha256"],
        "bytes":payload.len(),"imageBytes":image.len(),"createdAtMs":now_ms,"expiresAtMs":now_ms.saturating_add(TEMPORARY_MS)}));
    let payload_path = directory.join(format!("{id}.bin"));
    if !managed_files::inspect(root, &payload_path, false, true, PAYLOAD_BYTES)? {
        managed_files::atomic_write(root, &payload_path, payload, PAYLOAD_BYTES)?;
    }
    record["expiresAtMs"] = json!(now_ms.saturating_add(TEMPORARY_MS));
    record.as_object_mut().unwrap().remove("expiredAtMs");
    write_metadata(root, directory, &record)?;
    Ok(json!({"kynxaImageRef":reference}))
}

pub(crate) fn externalize(root: &Path, directory: &Path, chat: &str, request: &str, value: &Value, now_ms: i64) -> ContractResult {
    fn visit(root: &Path, directory: &Path, chat: &str, request: &str, value: &Value, now_ms: i64, url_container: bool) -> ContractResult {
        if image_reference(value).is_some() || opaque(value) { return Ok(value.clone()); }
        if let Some(items) = value.as_array() {
            return items.iter().map(|item| visit(root, directory, chat, request, item, now_ms, false))
                .collect::<Result<Vec<_>, _>>().map(Value::Array);
        }
        let Some(fields) = value.as_object() else { return Ok(value.clone()); };
        let mut projected = serde_json::Map::new();
        for (key, child) in fields {
            if opaque_field(key) {
                projected.insert(key.clone(), child.clone());
            } else if let Some(image) = encoded_image(value, key, child, url_container) {
                projected.insert(key.clone(), store_image(root, directory, chat, request, child.as_str().unwrap(), &image, now_ms)?);
            } else if ollama_images(value, key, child) {
                let mut images = Vec::new();
                for item in child.as_array().unwrap() {
                    let decoded = item.as_str().and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok());
                    images.push(if let Some(bytes) = decoded.filter(|bytes| !bytes.is_empty()) {
                        store_image(root, directory, chat, request, item.as_str().unwrap(), &bytes, now_ms)?
                    } else { visit(root, directory, chat, request, item, now_ms, false)? });
                }
                projected.insert(key.clone(), json!(images));
            } else {
                projected.insert(key.clone(), visit(root, directory, chat, request, child, now_ms, image_url_container(value, key))?);
            }
        }
        Ok(Value::Object(projected))
    }
    visit(root, directory, chat, request, value, now_ms, false)
}

pub(crate) fn hydrate(root: &Path, directory: &Path, chat: &str, value: &Value, now_ms: i64) -> ContractResult {
    fn visit(root: &Path, directory: &Path, chat: &str, value: &Value, now_ms: i64,
        expired: &mut Vec<Value>, count: &mut usize) -> ContractResult {
        if opaque(value) { return Ok(value.clone()); }
        if let Some(reference) = image_reference(value) {
            let record = metadata(root, directory, chat, reference)?;
            if record.as_ref().is_some_and(|record| available(record, now_ms)) {
                let path = directory.join(format!("{}.bin", reference["id"].as_str().unwrap()));
                if let Some(bytes) = managed_files::read(root, &path, PAYLOAD_BYTES)? {
                    if reference["sha256"] != managed_files::digest(&bytes) || reference["bytes"].as_u64() != Some(bytes.len() as u64) {
                        return Err(invalid());
                    }
                    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
                    *count += 1;
                    return Ok(json!(text));
                }
            }
            let mut reference = reference.clone();
            reference["state"] = json!("expired");
            expired.push(reference.clone());
            return Ok(json!({"kynxaImageRef":reference}));
        }
        if let Some(items) = value.as_array() {
            return items.iter().map(|item| visit(root, directory, chat, item, now_ms, expired, count))
                .collect::<Result<Vec<_>, _>>().map(Value::Array);
        }
        let Some(fields) = value.as_object() else { return Ok(value.clone()); };
        let expired_before = expired.len();
        let mut hydrated = serde_json::Map::new();
        for (key, child) in fields {
            hydrated.insert(key.clone(), if opaque_field(key) { child.clone() }
                else { visit(root, directory, chat, child, now_ms, expired, count)? });
        }
        if expired.len() > expired_before {
            let hashes = expired[expired_before..].iter().filter_map(|reference| reference["imageSha256"].as_str())
                .collect::<Vec<_>>().join(",");
            let notice = format!("[Untrusted historical media: temporary image payload expired; sha256={hashes}. Saved observations and execution receipts remain; expiry is not permission to repeat completed operations.]");
            if matches!(value["type"].as_str(), Some("image" | "image_url" | "input_image")) {
                return Ok(json!({"type":if value["type"] == "input_image" {"input_text"} else {"text"},"text":notice}));
            }
            if value["role"].is_string() {
                if let Some(images) = hydrated.get_mut("images").and_then(Value::as_array_mut) {
                    images.retain(Value::is_string);
                    if images.is_empty() { hydrated.remove("images"); }
                    if let Some(text) = hydrated.get("content").and_then(Value::as_str) {
                        hydrated.insert("content".to_owned(), json!(format!("{text}\n{notice}")));
                    }
                }
            }
        }
        Ok(Value::Object(hydrated))
    }
    let mut expired = Vec::new();
    let mut count = 0;
    let value = visit(root, directory, chat, value, now_ms, &mut expired, &mut count)?;
    Ok(json!({"value":value,"expiredMedia":expired,"availableMedia":count}))
}

pub(crate) fn retain(root: &Path, directory: &Path, chat: &str, value: &Value, reason: Option<&str>, lease_ms: i64, now_ms: i64) -> ContractResult {
    fn references<'a>(value: &'a Value, output: &mut HashMap<&'a str, &'a Value>) {
        if let Some(reference) = image_reference(value) {
            if let Some(id) = reference["id"].as_str() { output.insert(id, reference); }
        } else if let Some(items) = value.as_array() {
            for item in items { references(item, output); }
        } else if let Some(fields) = value.as_object() {
            for child in fields.values() { references(child, output); }
        }
    }
    if reason.is_some_and(|reason| reason.trim().is_empty() || reason.chars().count() > 2000) { return Err(invalid()); }
    let mut found = HashMap::new();
    references(value, &mut found);
    let mut retained = 0;
    let mut expired = 0;
    for reference in found.values() {
        let Some(mut record) = metadata(root, directory, chat, reference)? else { expired += 1; continue; };
        let path = directory.join(format!("{}.bin", reference["id"].as_str().unwrap()));
        if !managed_files::inspect(root, &path, false, true, PAYLOAD_BYTES)? { expired += 1; continue; }
        if let Some(reason) = reason { record["pinnedReason"] = json!(reason.trim()); }
        else { record["leaseUntilMs"] = json!(record["leaseUntilMs"].as_i64().unwrap_or(0)
            .max(now_ms.saturating_add(lease_ms.clamp(0, MAXIMUM_LEASE_MS)))); }
        write_metadata(root, directory, &record)?;
        retained += 1;
    }
    Ok(json!({"retained":retained,"expired":expired}))
}

pub(crate) fn cleanup(root: &Path, directory: &Path, chat: &str, now_ms: i64, limit: usize) -> ContractResult {
    if limit == 0 || !managed_files::inspect(root, directory, false, false, 0)? {
        return Ok(json!({"scanned":0,"visited":0,"removed":0,"removedBytes":0}));
    }
    let start = CLEANUP_POSITIONS.lock().map_err(|_| invalid())?.iter()
        .find(|(path, _)| path == directory).map_or(0, |(_, position)| *position);
    let entries = std::fs::read_dir(directory).map_err(|_| invalid())?;
    let mut scanned = 0;
    let mut visited = 0;
    let mut removed = 0;
    let mut removed_bytes = 0_u64;
    for entry in entries.skip(start).take(limit) {
        visited += 1;
        let entry = entry.map_err(|_| invalid())?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") { continue; }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()).filter(|id|
            id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())) else { continue; };
        let Some(bytes) = managed_files::read(root, &path, METADATA_BYTES)? else { continue; };
        let Ok(record) = serde_json::from_slice::<Value>(&bytes) else { continue; };
        if record["version"] != 1 || record["id"] != id
            || !record["conversationId"].as_str().is_some_and(|owner| owner.eq_ignore_ascii_case(chat))
            || !record["bytes"].as_u64().is_some_and(|bytes| (1..=PAYLOAD_BYTES).contains(&bytes))
            || hex_hash(&record["sha256"]).is_none() || hex_hash(&record["imageSha256"]).is_none()
            || record["createdAtMs"].as_i64().is_none() || record["expiresAtMs"].as_i64().is_none()
            || !record["requestId"].is_string()
            || record["id"] != managed_files::digest(format!("{}:{}", record["requestId"].as_str().unwrap(), record["sha256"].as_str().unwrap()).as_bytes()) {
            continue;
        }
        scanned += 1;
        if available(&record, now_ms) { continue; }
        let payload = directory.join(format!("{id}.bin"));
        if managed_files::inspect(root, &payload, false, true, PAYLOAD_BYTES)? {
            std::fs::remove_file(&payload).map_err(|_| invalid())?;
            removed += 1;
            removed_bytes = removed_bytes.saturating_add(record["bytes"].as_u64().unwrap_or(0));
        }
    }
    let mut positions = CLEANUP_POSITIONS.lock().map_err(|_| invalid())?;
    if let Some((_, position)) = positions.iter_mut().find(|(path, _)| path == directory) {
        *position = if visited < limit { 0 } else { start.saturating_add(visited) };
    } else {
        if positions.len() >= 128 { positions.remove(0); }
        positions.push((directory.to_path_buf(), if visited < limit { 0 } else { visited }));
    }
    Ok(json!({"scanned":scanned,"visited":visited,"removed":removed,"removedBytes":removed_bytes}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("kynxa-model-images-{}", uuid::Uuid::new_v4()));
        let directory = root.join("media");
        (root, directory)
    }

    fn image(text: &str) -> Value {
        json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(text)})
    }

    #[test]
    fn temporary_model_images_roundtrip_then_expire_as_valid_text_without_changing_receipts() {
        let (root, directory) = fixture();
        let data = "data:image/png;base64,aW1hZ2U=";
        let original = json!({"messages":[{"role":"user","content":[image("image"),
            {"type":"image_url","image_url":{"url":data}},
            {"type":"input_image","image_url":data},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}}]},
            {"role":"user","content":"local image","images":["aW1hZ2U="]}],
            "caption":data,"reasoning_content":{"type":"image","data":"aW1hZ2U="},
            "opaque":{"type":"thinking","thinking":"private","signature":"signed","image_url":data}});
        let stored = externalize(&root, &directory, "chat", "request", &original, 1000).unwrap();
        assert_eq!(stored["caption"], data);
        assert_eq!(stored["reasoning_content"], original["reasoning_content"]);
        assert_eq!(stored["opaque"], original["opaque"]);
        assert!(stored["messages"][0]["content"][0]["data"]["kynxaImageRef"].is_object());
        assert_eq!(hydrate(&root, &directory, "chat", &stored, 1000).unwrap()["value"], original);
        let original_hash = managed_files::digest(stored.to_string().as_bytes());
        assert_eq!(cleanup(&root, &directory, "chat", 1000 + TEMPORARY_MS + 1, 64).unwrap()["removed"], 2);
        let expired = hydrate(&root, &directory, "chat", &stored, 1000 + TEMPORARY_MS + 1).unwrap();
        assert_eq!(expired["expiredMedia"].as_array().unwrap().len(), 5);
        assert_eq!(expired["value"]["messages"][0]["content"][0]["type"], "text");
        assert_eq!(expired["value"]["messages"][0]["content"][2]["type"], "input_text");
        assert!(expired["value"]["messages"][1].get("images").is_none());
        assert!(expired["value"]["messages"][1]["content"].as_str().unwrap().contains("expired"));
        assert_eq!(expired["value"]["opaque"], original["opaque"]);
        assert_eq!(managed_files::digest(stored.to_string().as_bytes()), original_hash);
        assert_eq!(expired["expiredMedia"][0]["state"], "expired");
        assert!(hydrate(&root, &directory, "foreign-chat", &stored, 1000).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_media_and_bounded_active_leases_survive_only_their_owned_lifetime() {
        let (root, directory) = fixture();
        let saved = externalize(&root, &directory, "chat", "request", &json!([image("pinned"), image("active"), image("paused")]), 1000).unwrap();
        assert!(retain(&root, &directory, "chat", &saved[0], Some(" "), 0, 1000).is_err());
        retain(&root, &directory, "chat", &saved[0], Some("user-requested evidence"), 0, 1000).unwrap();
        retain(&root, &directory, "chat", &saved[1], None, i64::MAX, 1000 + TEMPORARY_MS - 1).unwrap();
        assert_eq!(cleanup(&root, &directory, "chat", 1000 + TEMPORARY_MS + 1, 64).unwrap()["removed"], 1);
        let after_ttl = hydrate(&root, &directory, "chat", &saved, 1000 + TEMPORARY_MS + 1).unwrap();
        assert_eq!(after_ttl["availableMedia"], 2);
        assert_eq!(after_ttl["expiredMedia"].as_array().unwrap().len(), 1);
        assert_eq!(cleanup(&root, &directory, "chat", 1000 + TEMPORARY_MS + MAXIMUM_LEASE_MS + 1, 64).unwrap()["removed"], 1);
        assert_eq!(hydrate(&root, &directory, "chat", &saved, i64::MAX).unwrap()["availableMedia"], 1);
        let reference = &saved[0]["data"]["kynxaImageRef"];
        std::fs::write(directory.join(format!("{}.bin", reference["id"].as_str().unwrap())), b"changed").unwrap();
        assert!(hydrate(&root, &directory, "chat", &saved[0], 1000).is_err());
        let mut foreign = metadata(&root, &directory, "chat", reference).unwrap().unwrap();
        foreign["conversationId"] = json!("other-chat");
        foreign.as_object_mut().unwrap().remove("pinnedReason");
        write_metadata(&root, &directory, &foreign).unwrap();
        assert_eq!(cleanup(&root, &directory, "chat", i64::MAX, 64).unwrap()["removed"], 0);
        assert!(directory.join(format!("{}.bin", reference["id"].as_str().unwrap())).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_media_fast_paths_do_not_initialize_a_catalog_or_rewrite_opaque_text() {
        let (root, _) = fixture();
        let value = json!({"messages":[{"role":"user","content":"ordinary text"}],
            "caption":"data:image/png;base64,aW1hZ2U=","reasoning":{"type":"image","data":"aW1hZ2U="}});
        assert!(!has_image_payload(&value));
        assert_eq!(crate::storage::gateway::externalize_model_images(&root, "missing", "request", &value).unwrap(), value);
        assert_eq!(crate::storage::gateway::hydrate_model_images(&root, "missing", &value).unwrap()["value"], value);
        assert_eq!(crate::storage::gateway::protect_model_images(&root, "missing", &value, u64::MAX).unwrap()["retained"], 0);
        assert_eq!(crate::storage::gateway::pin_model_images(&root, "missing", &value, "requested").unwrap()["retained"], 0);
        assert_eq!(crate::storage::gateway::cleanup_expired_model_images(&root, 1000, 256).unwrap()["removed"], 0);
        assert!(!root.exists());
    }

    #[test]
    fn browser_value_content_images_are_hidden_from_text_but_preserved_for_model_media() {
        let canonical = json!({"sessionId":"browser-session","tabId":"tab", "value":{"content":[image("image"),
            {"type":"text","text":"literal data:image/png;base64,aW1hZ2U="}]}});
        let public = crate::storage::gateway::public_tool_result(&canonical, Some(&json!({"id":"owned-result"})), None, false).unwrap();
        assert!(public["value"]["content"][0].get("data").is_none());
        assert_eq!(public["value"]["content"][0]["media"]["bytes"], 5);
        assert_eq!(public["value"]["content"][1], canonical["value"]["content"][1]);
        assert_eq!(crate::storage::gateway::public_tool_result(&canonical, None, None, true).unwrap(), canonical);
    }

    #[test]
    fn image_archives_keep_hashed_raw_references_after_cleanup_and_pin_requires_a_reason() {
        let (root, _) = fixture();
        let chat = uuid::Uuid::new_v4().to_string();
        let request = uuid::Uuid::new_v4().to_string();
        crate::storage::gateway::ensure_conversation(&root, &chat, "Isolated image retention").unwrap();
        let canonical = json!({"content":[{"type":"text","text":"saved observation"},image(&"x".repeat(7 * 1024 * 1024))]});
        let call = json!({"id":"screenshot","name":"mcp.fixture.look","arguments":{}});
        let mut invalid_pin = call.clone();
        invalid_pin["arguments"] = json!({"retention":"permanent"});
        assert_eq!(crate::storage::gateway::save_tool_result(&root, &chat, &request, &invalid_pin, &canonical, None)
            .unwrap_err().code(), "INVALID_IMAGE_RETENTION");
        let reference = crate::storage::gateway::save_tool_result(&root, &chat, &request, &call, &canonical, None).unwrap();
        let archived = crate::storage::gateway::load_private_tool_result(&root, &chat, reference["id"].as_str().unwrap()).unwrap();
        assert!(archived["canonical"]["content"][1]["data"]["kynxaImageRef"].is_object());
        assert_eq!(reference["sha256"], managed_files::digest(archived["canonical"].to_string().as_bytes()));
        let owner = json!({"requestId":request,"toolCallId":call["id"],"toolName":call["name"]});
        assert_eq!(crate::storage::gateway::model_media(&root, &chat, &reference, &owner).unwrap(), canonical);
        crate::storage::gateway::cleanup_expired_model_images(&root, chrono::Utc::now().timestamp_millis() + TEMPORARY_MS + 1, 256).unwrap();
        assert_eq!(crate::storage::gateway::load_private_tool_result(&root, &chat, reference["id"].as_str().unwrap()).unwrap(), archived);
        let expired = crate::storage::gateway::model_media(&root, &chat, &reference, &owner).unwrap();
        assert_eq!(expired["content"][0]["text"], "saved observation");
        assert_eq!(expired["content"][1]["type"], "text");
        assert_eq!(expired["mediaRetention"]["expiredMedia"][0]["state"], "expired");
        std::fs::remove_dir_all(root).unwrap();
    }
}
