use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use url::Url;

use super::intent::matches;
use super::local_files::{absolute_path, inspect, path_text, read_checked, within};
use super::service_error::{ServiceError, ServiceResult};
use super::service_mcp::preview_sections;
use crate::models::protocol::{array, string, truthy};

const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PIXELS: u64 = 16_000_000;

fn appended_image_receipt(image: Value, arguments: &Value) -> Value {
    let mut receipt = json!({"screenshotStatus":"available"});
    if arguments["includePreview"] == true {
        receipt["previewAppendSections"] = json!(preview_sections(std::slice::from_ref(&image)));
    }
    receipt["appendImage"] = image;
    receipt
}

fn u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
fn u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

/// Media structure and scope are verified before any bytes become a local-viewer artifact.
/// 媒体结构与读取范围在字节成为本地查看附件前完成验证。
fn dimensions(bytes: &[u8], declared: &Value) -> Option<Value> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let (mime, width, height) = if bytes.len() >= 45
        && bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10])
        && u32_be(bytes, 8) == Some(13)
        && bytes.get(12..16) == Some(b"IHDR")
    {
        let width = u32_be(bytes, 16)?;
        let height = u32_be(bytes, 20)?;
        let mut offset = 8;
        let mut data = false;
        let mut ended = false;
        while offset + 12 <= bytes.len() {
            let length = u32_be(bytes, offset)? as usize;
            let end = offset.checked_add(length)?.checked_add(12)?;
            if end > bytes.len() {
                return None;
            }
            let chunk = bytes.get(offset + 4..offset + 8)?;
            if chunk == b"IDAT" {
                data = true;
            }
            if chunk == b"IEND" {
                ended = length == 0 && end == bytes.len();
                break;
            }
            offset = end;
        }
        if !data || !ended {
            return None;
        }
        ("image/png", width, height)
    } else if bytes.len() >= 4 && bytes.starts_with(&[255, 216]) && bytes.ends_with(&[255, 217]) {
        let mut offset = 2;
        let mut dimensions = None;
        while offset + 4 <= bytes.len() {
            if *bytes.get(offset)? != 255 {
                return None;
            }
            offset += 1;
            while bytes.get(offset) == Some(&255) {
                offset += 1;
            }
            let marker = *bytes.get(offset)?;
            offset += 1;
            if [218, 217].contains(&marker) {
                break;
            }
            if marker == 1 || (208..=215).contains(&marker) {
                continue;
            }
            let length = u16_be(bytes, offset)? as usize;
            if length < 2 || offset + length > bytes.len() {
                return None;
            }
            if [192, 193, 194].contains(&marker) {
                if length < 8 {
                    return None;
                }
                dimensions = Some((
                    u16_be(bytes, offset + 5)? as u32,
                    u16_be(bytes, offset + 3)? as u32,
                ));
                break;
            }
            offset += length;
        }
        let (width, height) = dimensions?;
        ("image/jpeg", width, height)
    } else {
        return None;
    };
    if truthy(declared) && declared != mime
        || width == 0
        || height == 0
        || width as u64 * height as u64 > MAX_PIXELS
    {
        return None;
    }
    Some(json!({"mimeType":mime,"width":width,"height":height}))
}

fn decoded(encoded: &Value, mime: &Value) -> Option<Value> {
    let encoded = encoded.as_str()?;
    if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
        || !matches(r"^[A-Za-z0-9+/]+={0,2}$", encoded)
    {
        return None;
    }
    let bytes = STANDARD.decode(encoded).ok()?;
    if STANDARD.encode(&bytes) != encoded {
        return None;
    }
    let dimensions = dimensions(&bytes, mime)?;
    Some(json!({"type":"image","data":encoded,"mimeType":dimensions["mimeType"]}))
}

fn screenshot(descriptor: &Value) -> bool {
    descriptor["operation"] == "tools/call"
        && ["browser_take_screenshot", "take_screenshot"].contains(&string(&descriptor["toolName"]))
}
fn option(arguments: &[Value], names: &[&str]) -> Option<String> {
    for (index, value) in arguments.iter().enumerate() {
        let argument = string(value);
        for name in names {
            if argument == *name {
                return arguments
                    .get(index + 1)
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            if let Some(value) = argument.strip_prefix(&format!("{name}=")) {
                return Some(value.into());
            }
        }
    }
    None
}
fn remote(server: &Value) -> bool {
    if server["transport"].as_str().unwrap_or("stdio") != "stdio" {
        return true;
    }
    let variables = ["PLAYWRIGHT_MCP_CDP_ENDPOINT", "PLAYWRIGHT_MCP_ENDPOINT"];
    if variables
        .iter()
        .any(|name| server["envRefs"].get(*name).is_some())
    {
        return true;
    }
    let endpoint = option(
        array(&server["args"]),
        &[
            "--cdp-endpoint",
            "--endpoint",
            "--browser-url",
            "--browserUrl",
            "--ws-endpoint",
            "--wsEndpoint",
        ],
    )
    .or_else(|| {
        variables.iter().find_map(|name| {
            server["env"][*name]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
    });
    let Some(endpoint) = endpoint.filter(|value| !value.is_empty()) else {
        return false;
    };
    let Ok(url) = Url::parse(&endpoint) else {
        return true;
    };
    let host = url.host_str().unwrap_or("");
    !["localhost", "[::1]", "::1"].contains(&host) && !matches(r"^127(?:\.\d{1,3}){3}$", host)
}

fn safe_path(value: &Value, root: &Path) -> Option<PathBuf> {
    let value = value.as_str()?;
    if value.is_empty()
        || value.encode_utf16().count() > 4096
        || value.contains(['\0', '\r', '\n'])
        || value.starts_with(r"\\")
    {
        return None;
    }
    let scheme_test = if matches(r"(?i)^[a-z]:[\\/]", value) {
        &value[3..]
    } else {
        value
    };
    if matches(r"(?i)^[a-z][a-z0-9+.-]*:", scheme_test) {
        return None;
    }
    if cfg!(windows) {
        let tail = if matches(r"(?i)^[a-z]:", value) {
            &value[2..]
        } else {
            value
        };
        if tail.contains(':')
            || value.split(['\\', '/']).any(|part| {
                !matches!(part, "." | "..")
                    && (part.ends_with(['.', ' '])
                        || matches(r"(?i)^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)", part))
            })
        {
            return None;
        }
    }
    Some(absolute_path(&path_text(&root.join(value))))
}

fn paths(server: &Value, context: &Value, arguments: &Value, result: &Value) -> Vec<PathBuf> {
    let cwd = server["cwd"]
        .as_str()
        .or_else(|| context["workspaceRoot"].as_str())
        .unwrap_or("");
    if cwd.is_empty() || !Path::new(cwd).is_absolute() {
        return Vec::new();
    }
    let cwd = absolute_path(cwd);
    let mut roots = vec![cwd.clone()];
    let output = option(array(&server["args"]), &["--output-dir"]).or_else(|| {
        server["env"]["PLAYWRIGHT_MCP_OUTPUT_DIR"]
            .as_str()
            .map(str::to_owned)
    });
    if let Some(output) = &output {
        if let Some(path) = safe_path(&json!(output), &cwd) {
            roots.push(path);
        }
    }
    let aliases = roots
        .into_iter()
        .filter_map(|root| {
            if !inspect(&root, false, false).ok().flatten()?.is_dir() {
                return None;
            }
            let canonical = std::fs::canonicalize(&root).ok()?;
            if !inspect(&canonical, false, false).ok().flatten()?.is_dir() {
                return None;
            }
            Some((root, absolute_path(&path_text(&canonical))))
        })
        .collect::<Vec<_>>();
    let normalize = |value: &Value| -> Option<PathBuf> {
        let path = safe_path(value, &cwd)?;
        for (lexical, canonical) in &aliases {
            if within(lexical, &path) {
                let suffix = path
                    .components()
                    .skip(lexical.components().count())
                    .collect::<PathBuf>();
                return Some(canonical.join(suffix));
            }
            if within(canonical, &path) {
                return Some(path);
            }
        }
        None
    };
    let supplied = arguments
        .get("filename")
        .filter(|value| !value.is_null())
        .or_else(|| arguments.get("filePath"));
    let mut candidates = supplied.into_iter().cloned().collect::<Vec<_>>();
    if arguments.get("filePath").is_some() {
        let requested = normalize(&arguments["filePath"]);
        let mut reported = Vec::new();
        let mut has_receipt = false;
        let pattern = regex::Regex::new(r"(?m)^Saved screenshot to (.+)\.$").unwrap();
        for block in array(&result["content"]) {
            let text = string(&block["text"]);
            if block["type"] != "text" || text.encode_utf16().count() > 65536 {
                continue;
            }
            for receipt in pattern.captures_iter(text) {
                has_receipt = true;
                let value = json!(&receipt[1]);
                let candidate = normalize(&value);
                if candidate.as_ref().zip(requested.as_ref()).is_some_and(
                    |(candidate, requested)| {
                        candidate.with_extension("") == requested.with_extension("")
                    },
                ) {
                    reported.push(value);
                }
            }
        }
        if has_receipt {
            candidates = reported;
        }
    }
    if supplied.is_none() && output.as_ref().is_some_and(|value| !value.is_empty()) {
        let pattern =
            regex::Regex::new(r"\[(?:Screenshot[^\]\r\n]*|Page screenshot)\]\(([^)\r\n]+)\)")
                .unwrap();
        for block in array(&result["content"]) {
            let text = string(&block["text"]);
            if block["type"] != "text" || text.encode_utf16().count() > 65536 {
                continue;
            }
            candidates.extend(
                pattern
                    .captures_iter(text)
                    .map(|capture| json!(&capture[1])),
            );
        }
    }
    let mut result = Vec::new();
    for candidate in candidates.iter().take(4) {
        if let Some(path) = normalize(candidate) {
            if !result.contains(&path) {
                result.push(path);
            }
        }
    }
    result
}

fn cancelled(cancellation: &Option<Arc<AtomicBool>>) -> bool {
    cancellation
        .as_ref()
        .is_some_and(|token| token.load(Ordering::Acquire))
}
fn cancelled_receipt() -> Value {
    json!({"screenshotStatus":"unavailable","screenshotCode":"BROWSER_SCREENSHOT_PREVIEW_CANCELLED"})
}

pub fn apply(
    operation: &str,
    arguments: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
) -> ServiceResult<Value> {
    match operation {
        "artifact.screenshotTool" => Ok(json!(screenshot(&arguments["descriptor"]))),
        "artifact.inspectPlan" => Ok(json!(
            arguments["buffer"] == true
                && arguments["length"]
                    .as_u64()
                    .is_some_and(|length| length > 0 && length <= MAX_IMAGE_BYTES as u64)
        )),
        "artifact.inspect" => Ok(arguments["data"]
            .as_str()
            .and_then(|data| STANDARD.decode(data).ok())
            .and_then(|bytes| dimensions(&bytes, &arguments["declaredMime"]))
            .unwrap_or(Value::Null)),
        "artifact.cancelled" => Ok(cancelled_receipt()),
        "artifact.archive" => {
            let result = &arguments["result"];
            if !screenshot(&arguments["descriptor"]) || truthy(&result["isError"]) {
                return Ok(json!({}));
            }
            if array(&result["content"]).iter().any(|block| {
                block["type"] == "image" && decoded(&block["data"], &block["mimeType"]).is_some()
            }) {
                return Ok(json!({"screenshotStatus":"available"}));
            }
            if let Some(image) = array(&result["content"])
                .iter()
                .filter(|block| block["type"] == "resource")
                .take(20)
                .find_map(|block| {
                    decoded(&block["resource"]["blob"], &block["resource"]["mimeType"])
                })
            {
                return Ok(appended_image_receipt(image, arguments));
            }
            let server = &arguments["server"];
            if remote(server) {
                return Ok(
                    json!({"screenshotStatus":"unavailable","screenshotCode":"BROWSER_REMOTE_IMAGE_REQUIRED"}),
                );
            }
            if arguments["cancelled"] == true || cancelled(cancellation) {
                return Ok(cancelled_receipt());
            }
            for path in paths(server, &arguments["context"], &arguments["args"], result) {
                if cancelled(cancellation) {
                    return Ok(cancelled_receipt());
                }
                let image: ServiceResult<Option<Value>> = (|| {
                    let metadata = inspect(&path, false, false)?.ok_or_else(|| {
                        ServiceError::new("ENOENT", "Screenshot artifact was not found.", 404)
                    })?;
                    if !metadata.is_file()
                        || metadata.len() == 0
                        || metadata.len() > MAX_IMAGE_BYTES as u64
                    {
                        return Ok(None);
                    }
                    let bytes = read_checked(
                        &path,
                        MAX_IMAGE_BYTES as u64,
                        false,
                        "BROWSER_SCREENSHOT_CHANGED",
                    )?;
                    Ok(dimensions(&bytes,&Value::Null).map(|dimensions|json!({"type":"image","data":STANDARD.encode(bytes),"mimeType":dimensions["mimeType"]})))
                })();
                if let Ok(Some(image)) = image {
                    return Ok(appended_image_receipt(image, arguments));
                }
            }
            if cancelled(cancellation) {
                return Ok(cancelled_receipt());
            }
            Ok(
                json!({"screenshotStatus":"unavailable","screenshotCode":"BROWSER_SCREENSHOT_ARTIFACT_UNAVAILABLE"}),
            )
        }
        _ => Err(ServiceError::new(
            "AGENT_CORE_UNKNOWN_OPERATION",
            "Unknown browser artifact service operation.",
            400,
        )),
    }
}
