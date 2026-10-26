use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string, trim, truthy};

pub(super) fn limits() -> Value {
    json!({"maximumInputBytes": 32 * 1024 * 1024, "maximumOutputBytes": 2 * 1024 * 1024, "maximumPages": 100, "maximumArchiveEntries": 512, "maximumExpandedBytes": 16 * 1024 * 1024, "maximumProcesses": 2, "timeoutMs": 10000, "maximumHeapMb": 192, "maximumRssBytes": 256 * 1024 * 1024})
}
fn number(value: &Value, fallback: u64) -> u64 {
    value.as_u64().unwrap_or(fallback)
}
fn valid_code(code: &str) -> bool {
    regex::Regex::new("^(?:DOCUMENT_[A-Z_]+|OCR_[A-Z_]+)$")
        .unwrap()
        .is_match(code)
}
fn failure(code: &str) -> ServiceError {
    let status = if code.ends_with("LIMIT") || code.ends_with("BUDGET") {
        413
    } else if code == "DOCUMENT_DECODER_TIMEOUT" {
        504
    } else if (code.ends_with("FAILED") || code.ends_with("BUSY") || code.ends_with("UNSUPPORTED"))
        && code != "DOCUMENT_PARSE_FAILED"
    {
        503
    } else {
        422
    };
    ServiceError::new(
        code,
        if code == "OCR_UNAVAILABLE" {
            "A PDF page has no extractable text; OCR is unavailable. / PDF 页面没有可提取文本，当前未提供 OCR。"
        } else {
            "Document text extraction failed within its bounded offline decoder. / 有界离线文档文本提取失败。"
        },
        status,
    )
}
fn validate_limits(value: Option<&Value>) -> ServiceResult<Value> {
    let defaults = limits();
    let mut result = defaults.clone();
    if let Some(value) = value {
        let object = value
            .as_object()
            .ok_or_else(|| failure("DOCUMENT_INVALID_LIMITS"))?;
        for (key, value) in object {
            if !defaults.get(key).is_some_and(|maximum| {
                value
                    .as_u64()
                    .is_some_and(|number| number >= 1 && number <= maximum.as_u64().unwrap())
            }) {
                return Err(failure("DOCUMENT_INVALID_LIMITS"));
            }
            result[key] = value.clone();
        }
    }
    Ok(result)
}
fn page_window(value: Option<&Value>, maximum: u64) -> ServiceResult<Value> {
    let Some(value) = value else {
        return Ok(Value::Null);
    };
    let object = value
        .as_object()
        .ok_or_else(|| failure("DOCUMENT_INVALID_PAGE_WINDOW"))?;
    let start = value["startPage"]
        .as_u64()
        .filter(|page| (1..=1000000).contains(page))
        .ok_or_else(|| failure("DOCUMENT_INVALID_PAGE_WINDOW"))?;
    if object
        .keys()
        .any(|key| !["startPage", "endPage", "rawContentHash"].contains(&key.as_str()))
        || value.get("endPage").is_some_and(|end| {
            !end.as_u64()
                .is_some_and(|end| end >= start && end <= 1000000 && end - start < maximum)
        })
        || value.get("rawContentHash").is_some_and(|hash| {
            !regex::Regex::new("^[a-f0-9]{64}$")
                .unwrap()
                .is_match(string(hash))
        })
    {
        return Err(failure("DOCUMENT_INVALID_PAGE_WINDOW"));
    }
    Ok(value.clone())
}
fn coverage(args: &Value) -> Value {
    let details = &args["details"];
    let count = details["pageCount"]
        .as_u64()
        .filter(|count| *count > 0 && *count <= 9_007_199_254_740_991);
    let processed = count
        .map(|count| {
            details["processedPages"]
                .as_u64()
                .filter(|value| *value <= 9_007_199_254_740_991)
                .unwrap_or(0)
                .min(count)
        })
        .unwrap_or(0);
    let start = number(&details["pageWindow"]["startPage"], 1);
    let mut unprocessed = Vec::new();
    if let Some(count) = count {
        if start > 1 {
            unprocessed.push(json!({"startPage": 1, "endPage": start - 1}));
        }
        if start.saturating_add(processed) <= count {
            unprocessed.push(json!({"startPage": start + processed, "endPage": count}));
        }
    }
    let mut readback = json!({"kind": "original-document"});
    if let Some(count) = count {
        readback["startPage"] = json!(start);
        readback["endPage"] = details["pageWindow"]
            .get("endPage")
            .cloned()
            .unwrap_or(json!(count));
    }
    let mut value = json!({"format": args["format"], "reasonCode": args["code"], "complete": false, "processedPages": processed, "publishedPages": 0,
        "uncoveredRanges": count.map(|count| json!([{"startPage": 1, "endPage": count}])).unwrap_or(json!([])), "unprocessedRanges": unprocessed,
        "effectiveLimits": {"maximumInputBytes": args["limits"]["maximumInputBytes"], "maximumOutputBytes": args["limits"]["maximumOutputBytes"], "maximumPages": args["limits"]["maximumPages"]}, "readback": readback});
    if truthy(&args["rawContentHash"]) {
        value["rawContentHash"] = args["rawContentHash"].clone();
    }
    if let Some(count) = count {
        value["pageCount"] = json!(count);
    }
    if details["pages"].is_array() {
        value["missingTextPages"] = json!(
            array(&details["pages"])
                .iter()
                .filter(|page| page
                    .as_u64()
                    .is_some_and(|page| page >= 1 && count.is_some_and(|count| page <= count)))
                .take(100)
                .collect::<Vec<_>>()
        );
    }
    if truthy(&details["limit"]) {
        value["limit"] = details["limit"].clone();
    }
    value
}
#[derive(Default)]
struct Decoder {
    limits: Value,
    format: String,
    window: Value,
    closed: bool,
    released: bool,
    lease: Value,
    ocr: HashSet<u64>,
    pending_ocr: HashSet<u64>,
    has_result: bool,
    terminated: bool,
}
#[derive(Default)]
struct Pdf {
    limits: Value,
    page_count: u64,
    start: u64,
    end: u64,
    text: String,
    pending_text: String,
    pages: Vec<Value>,
    missing: Vec<u64>,
    ocr: Value,
    ocr_pages: u64,
    current_page: u64,
    method: bool,
}
#[derive(Default)]
struct Archive {
    limits: Value,
    declared: u64,
    expanded: u64,
    entries: u64,
    names: HashSet<String>,
    document: bool,
    content_types: bool,
}

/// Retain decoder slots and leases until close/release is observed; SDK promises are not proof.
/// 确认关闭或释放前保留解码槽和租约；SDK Promise 不算执行证明。
#[derive(Default)]
pub(crate) struct DocumentExtractionState {
    sequence: u64,
    decoders: HashMap<u64, Decoder>,
    worker_started: bool,
    worker_limits: Value,
    pdf: Pdf,
    archive: Archive,
    ocr_outputs: HashMap<u64, Value>,
}
impl DocumentExtractionState {
    pub(crate) fn apply(&mut self, operation: &str, args: &Value) -> ServiceResult<Value> {
        let operation = operation
            .strip_prefix("source.document.")
            .unwrap_or(operation);
        let id = number(&args["id"], 0);
        match operation {
            "limits" => Ok(limits()),
            "validateLimits" => validate_limits(args.get("options")),
            "validateWindow" => page_window(args.get("value"), number(&args["maximumPages"], 100)),
            "failure" => { let error = failure(string(&args["code"])); Ok(json!({"statusCode": error.status_code, "message": error.message})) }
            "coverage" => Ok(coverage(args)),
            "hashPlan" => Ok(json!(args["bytesValid"] == true && number(&args["byteLength"], u64::MAX) <= number(&args["limits"]["maximumInputBytes"], 0))),
            "parentOpen" => {
                let limits = validate_limits(args.get("limits"))?; let format = string(&args["format"]); if args["bytesValid"] != true || !["pdf", "docx"].contains(&format) { return Err(failure("DOCUMENT_INVALID_INPUT")); }
                let window = page_window(args.get("pageWindow"), number(&limits["maximumPages"], 100))?;
                if !window.is_null() && format != "pdf" { return Err(failure("DOCUMENT_INVALID_PAGE_WINDOW")); } if window.get("rawContentHash").is_some_and(|hash| *hash != args["rawContentHash"]) { return Err(failure("DOCUMENT_SOURCE_CHANGED")); }
                let bytes = number(&args["byteLength"], 0); if bytes == 0 || bytes > number(&limits["maximumInputBytes"], 0) { return Err(failure("DOCUMENT_BYTES_LIMIT").with_details(json!({"details": {"documentCoverage": coverage(&json!({"format": format, "limits": limits, "code": "DOCUMENT_BYTES_LIMIT", "rawContentHash": args["rawContentHash"], "details": {"limit": {"dimension": "maximumInputBytes", "limit": limits["maximumInputBytes"], "observed": bytes}}}))}}))); }
                if self.decoders.values().filter(|decoder| !decoder.closed).count() >= 2 { return Err(failure("DOCUMENT_DECODER_BUSY").with_details(json!({"details": {"documentCoverage": coverage(&json!({"format": format, "limits": limits, "code": "DOCUMENT_DECODER_BUSY", "rawContentHash": args["rawContentHash"]}))}}))); }
                self.sequence += 1; let id = self.sequence; self.decoders.insert(id, Decoder { limits: limits.clone(), format: format.into(), window: window.clone(), ..Decoder::default() });
                Ok(json!({"id": id, "limits": limits, "pageWindow": window, "closeWaitMs": 2000, "renewalMs": 10000,
                    "request": {"taskId": args["taskId"], "kind": "background", "cpuThreads": 1, "memoryBytes": number(&limits["maximumRssBytes"], 0) + bytes * 2 + number(&limits["maximumOutputBytes"], 0) * 2, "waitMs": number(&limits["timeoutMs"], 10000).min(10000), "ttlMs": 30000}}))
            }
            "parentLease" => { let decoder = self.decoders.get_mut(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; if args["lease"]["status"] != "granted" { return Err(failure("DOCUMENT_RESOURCE_BUSY")); } decoder.lease = args["lease"].clone(); Ok(Value::Null) }
            "parentReleasePlan" => Ok(self.decoders.get(&id).filter(|decoder| !decoder.released && !decoder.lease.is_null()).map(|decoder| json!(decoder.lease["leaseId"])).unwrap_or(Value::Null)),
            "parentRelease" => { let decoder = self.decoders.get_mut(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; if args["status"] != "released" { return Err(failure("DOCUMENT_RESOURCE_FAILED")); } decoder.released = true; Ok(Value::Null) }
            "parentFailureCoverage" => { let code = string(&args["code"]); Ok(if valid_code(code) { coverage(args) } else { Value::Null }) }
            "parentClose" => { if let Some(decoder) = self.decoders.get_mut(&id) { decoder.closed = true; } Ok(Value::Null) }
            "parentFinish" => { let decoder = self.decoders.get(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; let closed = decoder.closed; if closed && (decoder.released || decoder.lease.is_null()) && decoder.pending_ocr.is_empty() { self.decoders.remove(&id); } Ok(json!({"closed": closed, "failure": if closed { Value::Null } else { json!("DOCUMENT_DECODER_FAILED") }})) }
            "parentStop" => { let Some(decoder) = self.decoders.get_mut(&id) else { return Ok(json!(false)); }; if decoder.terminated { return Ok(json!(false)); } decoder.terminated = true; Ok(json!(true)) }
            "parentMemory" => { let decoder = self.decoders.get(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; if number(&args["rssBytes"], u64::MAX) > number(&decoder.limits["maximumRssBytes"], 0) { return Err(failure("DOCUMENT_DECODER_MEMORY_LIMIT")); } Ok(Value::Null) }
            "parentOcrRequest" => {
                let decoder = self.decoders.get_mut(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; let request = &args["message"]; let page = request["requestId"].as_u64(); let start = number(&decoder.window["startPage"], 1); let end = number(&decoder.window["endPage"], start + number(&decoder.limits["maximumPages"], 100) - 1);
                if decoder.format != "pdf" || args["hasBackend"] != true || !decoder.pending_ocr.is_empty() || page.is_none_or(|page| decoder.ocr.contains(&page) || page < start || page > end) || array(&request["pages"]).len() != 1 || request["pages"][0] != request["requestId"] { return Err(failure("OCR_INVALID_REQUEST")); }
                let page = page.unwrap(); decoder.ocr.insert(page); decoder.pending_ocr.insert(page); Ok(json!({"timeoutMs": number(&decoder.limits["timeoutMs"], 10000).min(8000)}))
            }
            "parentOcrReceipt" => {
                let decoder = self.decoders.get(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; let receipt = &args["receipt"]; let index = array(&receipt["pages"]).iter().position(|page| page["page"] == args["requestId"]); let page = index.map(|index| &receipt["pages"][index]);
                if index.is_none() || page.is_none_or(|page| !page["text"].is_string()) || number(&index.map(|index| args["textBytes"][index].clone()).unwrap_or(Value::Null), u64::MAX) > number(&decoder.limits["maximumOutputBytes"], 0) || !receipt["version"].is_string() || !receipt["language"].is_string() { return Err(failure("OCR_TEXT_UNAVAILABLE")); } Ok(json!({"pageIndex": index}))
            }
            "parentOcrSettled" => { if let Some(decoder) = self.decoders.get_mut(&id) { decoder.pending_ocr.remove(&number(&args["requestId"], 0)); } Ok(Value::Null) }
            "parentError" => {
                let decoder = self.decoders.get(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; let message = &args["message"]; let code = if valid_code(string(&message["code"])) { string(&message["code"]) } else { "DOCUMENT_PARSE_FAILED" };
                let pages: Vec<Value> = array(&message["pages"]).iter().filter(|page| page.as_i64().is_some_and(|page| page >= 1 && page as u64 <= number(&message["details"]["pageCount"], 1000000))).take(number(&decoder.limits["maximumPages"], 100) as usize).cloned().collect();
                let mut details = message["details"].as_object().cloned().unwrap_or_default(); if message["pages"].is_array() { details.insert("pages".into(), json!(pages)); }
                Ok(json!({"code": code, "details": {"pages": if message["pages"].is_array() { json!(pages) } else { Value::Null }, "documentCoverage": coverage(&json!({"format": decoder.format, "limits": decoder.limits, "code": code, "rawContentHash": args["rawContentHash"], "details": details}))}}))
            }
            "parentStatus" => Ok(self.decoders.get(&id).map(|decoder| json!({"closed": decoder.closed, "terminated": decoder.terminated, "hasResult": decoder.has_result})).unwrap_or(json!({"closed": true, "terminated": true, "hasResult": false}))),
            "parentResult" => { self.decoders.get_mut(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?.has_result = true; Ok(Value::Null) }
            "parentOutput" => { let decoder = self.decoders.get(&id).ok_or_else(|| failure("DOCUMENT_DECODER_FAILED"))?; if args["textValid"] != true || trim(string(&args["text"])).is_empty() || string(&args["text"]).contains('\0') || number(&args["textBytes"], u64::MAX) > number(&decoder.limits["maximumOutputBytes"], 0) { return Err(failure("DOCUMENT_OUTPUT_LIMIT")); } Ok(Value::Null) }
            "workerOpen" => { if self.worker_started { return Ok(json!({"start": false})); } self.worker_started = true; self.worker_limits = validate_limits(args.get("limits"))?;
                if args["type"] != "decode" || !["pdf", "docx"].contains(&string(&args["format"])) || args["bytesValid"] != true { return Err(failure("DOCUMENT_INVALID_INPUT")); } if number(&args["byteLength"], 0) == 0 || number(&args["byteLength"], 0) > number(&self.worker_limits["maximumInputBytes"], 0) { return Err(failure("DOCUMENT_BYTES_LIMIT")); }
                Ok(json!({"start": true, "limits": self.worker_limits, "memoryIntervalMs": 25})) }
            "workerVersion" => { let expected = match string(&args["name"]) { "pdfjs-dist" => "6.4.299", "mammoth" => "1.13.0", "yauzl" => "3.4.0", _ => "" }; if args["version"] != expected { return Err(failure("DOCUMENT_DECODER_VERSION_UNSUPPORTED")); } Ok(Value::Null) }
            "workerMemory" => { if number(&args["rssBytes"], u64::MAX) > number(&self.worker_limits["maximumRssBytes"], 0) { return Err(failure("DOCUMENT_DECODER_MEMORY_LIMIT")); } Ok(Value::Null) }
            "workerError" => { let code = if args["name"] == "PasswordException" { "DOCUMENT_ENCRYPTED" } else if valid_code(string(&args["code"])) { string(&args["code"]) } else { "DOCUMENT_PARSE_FAILED" }; let mut details = args["details"].as_object().cloned().unwrap_or_default(); if self.pdf.page_count > 0 { details.insert("pageCount".into(), json!(self.pdf.page_count)); details.insert("processedPages".into(), json!(self.pdf.pages.len())); details.insert("pageWindow".into(), json!({"startPage": self.pdf.start, "endPage": self.pdf.end})); }
                let mut result = json!({"type": "error", "code": code, "details": details}); if !args["pages"].is_null() { result["pages"] = args["pages"].clone(); } else if !self.pdf.missing.is_empty() { result["pages"] = json!(self.pdf.missing); } Ok(result) }
            "pdfBegin" => { let limits = &self.worker_limits; let window = page_window(args.get("pageWindow"), number(&limits["maximumPages"], 100))?; let count = number(&args["pageCount"], 0); let start = number(&window["startPage"], 1); let end = number(&window["endPage"], start + number(&limits["maximumPages"], 100) - 1).min(count);
                if count < 1 || start > count || window.get("endPage").is_some_and(|end| number(end, u64::MAX) > count) { return Err(failure("DOCUMENT_INVALID_PAGE_WINDOW")); }
                self.pdf = Pdf { limits: limits.clone(), page_count: count, start, end, ..Pdf::default() }; Ok(json!({"startPage": start, "endPage": end})) }
            "pdfPieces" => { let mut pieces = String::new(); for item in array(&args["items"]) { if let Some(text) = item["str"].as_str() { pieces.push_str(text); pieces.push(if truthy(&item["hasEOL"]) { '\n' } else { ' ' }); if pieces.len() as u64 > number(&self.pdf.limits["maximumOutputBytes"], 0) { return Err(failure("DOCUMENT_OUTPUT_LIMIT").with_details(json!({"details": {"limit": {"dimension": "maximumOutputBytes", "limit": self.pdf.limits["maximumOutputBytes"], "observed": pieces.len()}}}))); } } }
                self.pdf.pending_text = regex::Regex::new("[ \\t]+\\n").unwrap().replace_all(&pieces, "\n").trim_end_matches(|character: char| trim(&character.to_string()).is_empty()).into(); self.pdf.current_page = number(&args["page"], 0); self.pdf.method = false; Ok(json!({"ocr": trim(&self.pdf.pending_text).is_empty() && args["ocrEnabled"] == true})) }
            "pdfOcr" => { let receipt = &args["receipt"]; if !self.pdf.ocr.is_null() && (self.pdf.ocr["version"] != receipt["version"] || self.pdf.ocr["language"] != receipt["language"]) { return Err(failure("DOCUMENT_OCR_IDENTITY_CHANGED")); } self.pdf.pending_text = string(&receipt["text"]).into(); self.pdf.ocr = json!({"backend": "windows-media-ocr", "version": receipt["version"], "language": receipt["language"]}); self.pdf.method = true; Ok(Value::Null) }
            "pdfAppend" => {
                if trim(&self.pdf.pending_text).is_empty() && !self.pdf.method { self.pdf.missing.push(self.pdf.current_page); } let separator = if self.pdf.pages.is_empty() { "" } else { "\n\n" }; let proposed = self.pdf.text.len() + separator.len() + self.pdf.pending_text.len();
                if !self.pdf.pages.is_empty() && proposed as u64 > number(&self.pdf.limits["maximumOutputBytes"], 0) { return Ok(json!({"stop": true})); } self.pdf.text.push_str(separator); let start = self.pdf.text.encode_utf16().count(); self.pdf.text.push_str(&self.pdf.pending_text);
                let mut page = json!({"page": self.pdf.current_page, "startOffset": start, "endOffset": self.pdf.text.encode_utf16().count()}); if self.pdf.method { page["method"] = json!("ocr"); self.pdf.ocr_pages += 1; } self.pdf.pages.push(page);
                if self.pdf.text.len() as u64 > number(&self.pdf.limits["maximumOutputBytes"], 0) { return Err(failure("DOCUMENT_OUTPUT_LIMIT")); } Ok(json!({"stop": self.pdf.ocr_pages >= 2}))
            }
            "pdfResult" => {
                if !self.pdf.missing.is_empty() { return Err(failure("OCR_UNAVAILABLE").with_details(json!({"pages": self.pdf.missing}))); } let last = self.pdf.pages.last().ok_or_else(|| failure("DOCUMENT_TEXT_UNAVAILABLE"))?["page"].clone(); let last = number(&last, 0);
                let pages = self.pdf.pages.iter().map(|page| format!("{{\"page\":{},\"startOffset\":{},\"endOffset\":{}{}}}", page["page"], page["startOffset"], page["endOffset"], if page["method"].is_string() { format!(",\"method\":{}", page["method"]) } else { String::new() })).collect::<Vec<_>>().join(",");
                let mut ranges = Vec::new(); if self.pdf.start > 1 { ranges.push(format!("{{\"startPage\":1,\"endPage\":{}}}", self.pdf.start - 1)); } if last < self.pdf.page_count { ranges.push(format!("{{\"startPage\":{},\"endPage\":{}}}", last + 1, self.pdf.page_count)); }
                let ocr = if self.pdf.ocr.is_null() { String::new() } else { format!(",\"ocr\":{{\"backend\":\"windows-media-ocr\",\"version\":{},\"language\":{}}}", self.pdf.ocr["version"], self.pdf.ocr["language"]) };
                Ok(json!({"wire": format!("{{\"text\":{},\"pageCount\":{},\"pages\":[{}],\"pageWindow\":{{\"version\":\"pdf-page-window-v1\",\"startPage\":{},\"endPage\":{},\"nextPage\":{}}},\"complete\":{},\"uncoveredRanges\":[{}]{}}}", json!(self.pdf.text), self.pdf.page_count, pages, self.pdf.start, last, if last < self.pdf.page_count { json!(last + 1) } else { Value::Null }, self.pdf.start == 1 && last == self.pdf.page_count, ranges.join(","), ocr)}))
            }
            "archiveBegin" => { self.archive = Archive { limits: self.worker_limits.clone(), ..Archive::default() }; Ok(Value::Null) }
            "archiveEntry" => { let entry = &args["entry"]; self.archive.entries += 1; let size = number(&entry["uncompressedSize"], u64::MAX); self.archive.declared = self.archive.declared.saturating_add(size);
                let observed = if self.archive.entries > number(&self.archive.limits["maximumArchiveEntries"], 0) { Some(("maximumArchiveEntries", self.archive.entries)) } else if size > number(&self.archive.limits["maximumExpandedBytes"], 0) { Some(("maximumExpandedBytes", size)) } else if self.archive.declared > number(&self.archive.limits["maximumExpandedBytes"], 0) { Some(("maximumExpandedBytes", self.archive.declared)) } else { None };
                if let Some((dimension, observed)) = observed { return Err(failure("DOCUMENT_ARCHIVE_LIMIT").with_details(json!({"details": {"limit": {"dimension": dimension, "limit": self.archive.limits[dimension], "observed": observed}}}))); }
                let name = string(&entry["fileName"]); if !self.archive.names.insert(name.into()) { return Err(failure("DOCUMENT_PARSE_FAILED")); } if number(&entry["generalPurposeBitFlag"], 0) & 1 != 0 { return Err(failure("DOCUMENT_ENCRYPTED")); }
                Ok(json!({"directory": name.ends_with('/'), "xml": regex::Regex::new("(?i)\\.(?:xml|rels)$").unwrap().is_match(name)})) }
            "archiveChunk" => { self.archive.expanded = self.archive.expanded.saturating_add(number(&args["bytes"], 0)); if self.archive.expanded > number(&self.archive.limits["maximumExpandedBytes"], 0) { return Err(failure("DOCUMENT_ARCHIVE_LIMIT").with_details(json!({"details": {"limit": {"dimension": "maximumExpandedBytes", "limit": self.archive.limits["maximumExpandedBytes"], "observed": self.archive.expanded}}}))); } Ok(Value::Null) }
            "archiveXml" => { let text = string(&args["text"]); if regex::Regex::new("(?i)<!\\s*(?:DOCTYPE|ENTITY)\\b").unwrap().is_match(text) { return Err(failure("DOCUMENT_XML_UNSUPPORTED")); } if args["name"] == "word/document.xml" { self.archive.document = true; } if args["name"] == "[Content_Types].xml" { self.archive.content_types = text.contains("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"); } Ok(Value::Null) }
            "archiveFinish" => { if !self.archive.document || !self.archive.content_types { return Err(failure("DOCUMENT_PARSE_FAILED")); } self.archive.names.clear(); Ok(Value::Null) }
            "docxOutput" => { if number(&args["messageCount"], 0) > 0 { return Err(failure("DOCUMENT_PARSE_FAILED")); } if number(&args["textBytes"], u64::MAX) > number(&self.worker_limits["maximumOutputBytes"], 0) { return Err(failure("DOCUMENT_OUTPUT_LIMIT").with_details(json!({"details": {"limit": {"dimension": "maximumOutputBytes", "limit": self.worker_limits["maximumOutputBytes"], "observed": args["textBytes"]}}}))); } if trim(string(&args["text"])).is_empty() { return Err(failure("DOCUMENT_TEXT_UNAVAILABLE")); } Ok(Value::Null) }
            "ocrOpen" => { let pages = array(&args["pages"]); let mut unique = HashSet::new(); if args["bytesValid"] != true || number(&args["byteLength"], 0) == 0 || number(&args["byteLength"], 0) > 32 * 1024 * 1024 || pages.is_empty() || pages.len() > 100 || pages.iter().any(|page| page.as_u64().is_none_or(|page| !(1..=1000000).contains(&page) || !unique.insert(page))) || args["maximumOutputBytes"].as_u64().is_none_or(|bytes| !(1..=2 * 1024 * 1024).contains(&bytes)) { return Err(failure("DOCUMENT_INVALID_INPUT")); }
                let format = args["format"].as_str().unwrap_or("pdf");
                if !["pdf", "png", "jpeg", "bmp", "gif", "tiff", "webp"].contains(&format) || format != "pdf" && pages != &[json!(1)] { return Err(failure("DOCUMENT_INVALID_INPUT")); }
                self.sequence += 1; let id = self.sequence; self.ocr_outputs.insert(id, json!({"pages": pages, "maximum": args["maximumOutputBytes"], "bytes": 0, "format":format}));
                // Raster leases cover the bounded original decode, scaled bitmap and helper overhead.
                // 栅格租约覆盖有界原图解码、缩放位图及辅助进程开销，不能只按压缩字节预约。
                let decode_memory = if format == "pdf" {384} else {512} * 1024 * 1024;
                Ok(json!({"id": id, "timeoutMs": number(&args["timeoutMs"], 20000).clamp(1000, 30000), "killWaitMs": 2000, "resource": {"kind": "background", "cpuThreads": 1, "memoryBytes": decode_memory + number(&args["byteLength"], 0) * 4 + number(&args["maximumOutputBytes"], 0) * 6, "waitMs": args["timeoutMs"]}})) }
            "ocrChunk" => { let output = self.ocr_outputs.get_mut(&id).ok_or_else(|| failure("OCR_INVALID_RESULT"))?; let bytes = number(&output["bytes"], 0).saturating_add(number(&args["bytes"], 0)); output["bytes"] = json!(bytes); Ok(json!(bytes <= number(&output["maximum"], 0).saturating_mul(6) + 64 * 1024)) }
            "ocrResult" => { let output = self.ocr_outputs.get(&id).ok_or_else(|| failure("OCR_INVALID_RESULT"))?; let result = &args["result"]; let returned_code = result.get("errorCode").or_else(|| result.get("code"));
                if returned_code.is_some_and(truthy) { let code = string(returned_code.unwrap()); return Err(failure(if regex::Regex::new("^OCR_[A-Z_]+$").unwrap().is_match(code) { code } else { "OCR_FAILED" })); }
                let clean_identity = |value: &Value| value.as_str().is_some_and(|value| !value.is_empty() && value.encode_utf16().count() <= 128 && !value.chars().any(|character| character <= '\u{001f}'));
                let known_version = ["windows-media-ocr-v1|", "windows-media-ocr-v2|"].iter().any(|prefix| string(&result["version"]).starts_with(prefix));
                let format_matches = result["format"] == output["format"] || output["format"] == "pdf" && result.get("format").is_none();
                if args["exitCode"] != 0 || result["protocolVersion"] != 1 || result["schemaVersion"] != 1 || result["boundary"] != "document-ocr" || result["completed"] != true || !clean_identity(&result["version"]) || !known_version || !format_matches || !clean_identity(&result["language"]) || !result["pages"].is_array() || array(&result["pages"]).len() != array(&output["pages"]).len() || array(&result["pages"]).iter().enumerate().any(|(index, page)| page["page"] != output["pages"][index] || !page["text"].is_string()) || array(&args["textBytes"]).iter().map(|bytes| number(bytes, u64::MAX)).try_fold(0u64, u64::checked_add).is_none_or(|bytes| bytes > number(&output["maximum"], 0)) { return Err(failure("OCR_INVALID_RESULT")); } Ok(Value::Null) }
            "ocrClose" => { self.ocr_outputs.remove(&id); Ok(Value::Null) }
            "ocrError" => Ok(json!(if valid_code(string(&args["code"])) { string(&args["code"]) } else { "OCR_FAILED" })),
            "registered" => { if args["status"] != "registered" { return Err(failure("DOCUMENT_RESOURCE_FAILED")); } Ok(Value::Null) }
            _ => Err(failure("AGENT_CORE_UNKNOWN_OPERATION")),
        }
    }
}
