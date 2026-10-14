use std::collections::HashSet;

use serde_json::{Value, json};

use super::{
    ContractError, ContractResult, MAX_SAFE_INTEGER, hash, keys, member, object, safe_integer, trim,
    utf16_length,
};

const MAX_SOURCE_CHARACTERS: u64 = 2 * 1024 * 1024;

fn failure(message: impl Into<String>) -> ContractError {
    ContractError::new(message, "INVALID_RETRIEVAL_INPUT", 400)
}
fn extraction_failure(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_RETRIEVAL_EXTRACTION", 400)
}
fn structure_failure() -> ContractError {
    ContractError::new(
        "Invalid retrieval structure. / 检索结构元信息无效。",
        "INVALID_RETRIEVAL_STRUCTURE",
        400,
    )
}
fn require_keys(value: &Value, allowed: &[&str], label: &str) -> Result<(), ContractError> {
    let value = object(
        value,
        &format!("{label} must be an object. / 检索配置必须是对象。"),
        "INVALID_RETRIEVAL_INPUT",
        400,
    )?;
    for name in value.keys() {
        if !allowed.contains(&name.as_str()) {
            return Err(failure(format!(
                "Unknown {label} field: {name}. / 存在不支持的配置字段。"
            )));
        }
    }
    Ok(())
}
fn valid_text(value: &Value, maximum: usize) -> bool {
    value.as_str().is_some_and(|value| {
        !value.is_empty()
            && utf16_length(value) <= maximum
            && !value.chars().any(|character| character <= '\u{1f}')
    })
}
fn source_id(value: &Value) -> ContractResult {
    if !valid_text(value, 256) || trim(value.as_str().unwrap()).is_empty() {
        return Err(failure("Invalid source identity. / 资料身份无效。"));
    }
    Ok(value.clone())
}
fn scopes(value: &Value) -> ContractResult {
    let array = value
        .as_array()
        .filter(|array| !array.is_empty() && array.len() <= 128)
        .ok_or_else(|| {
            ContractError::new(
                "Authorized scopes are required. / 必须提供已授权的检索范围。",
                "RETRIEVAL_SCOPE_REQUIRED",
                403,
            )
        })?;
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for value in array {
        let valid = value.as_str().is_some_and(|value| {
            value == "user"
                || ["project:", "chat:"].iter().any(|prefix| {
                    value.strip_prefix(prefix).is_some_and(|id| {
                        !id.is_empty()
                            && id.len() <= 128
                            && id.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                            })
                    })
                })
        });
        if !valid {
            return Err(failure("Invalid retrieval scope. / 检索范围格式无效。"));
        }
        if seen.insert(value.as_str().unwrap()) {
            result.push(value.clone());
        }
    }
    Ok(json!(result))
}
fn source_extraction(value: &Value, text_length: Option<&Value>) -> ContractResult {
    require_keys(
        value,
        &[
            "format",
            "version",
            "rawContentHash",
            "pageCount",
            "pages",
            "pageWindow",
            "complete",
            "uncoveredRanges",
            "ocr",
            "image",
            "textRange",
            "parts",
            "capabilities",
            "omittedContent",
        ],
        "Source extraction",
    )?;
    if !crate::document_formats::extracted_format(value["format"].as_str().unwrap_or(""))
        || !valid_text(&value["version"], 256)
        || !hash(&value["rawContentHash"])
    {
        return Err(extraction_failure(
            "Invalid document extraction identity. / 文档提取身份无效。",
        ));
    }
    let mut result = json!({"format": value["format"], "version": value["version"], "rawContentHash": value["rawContentHash"]});
    if let Some(parts) = value.get("parts") {
        let maximum_offset = text_length.and_then(Value::as_u64).unwrap_or(MAX_SOURCE_CHARACTERS);
        if parts.as_array().is_none_or(|parts| parts.len() > 4096 || parts.iter().any(|part| {
            !keys(part, &["kind", "name", "startOffset", "endOffset"])
                || !valid_text(&part["kind"], 64) || !valid_text(&part["name"], 2048)
                || !safe_integer(&part["startOffset"], 0, maximum_offset)
                || !safe_integer(&part["endOffset"], 0, maximum_offset)
                || part["endOffset"].as_u64() < part["startOffset"].as_u64()
        })) { return Err(extraction_failure("Document structure positions are invalid. / 文档结构位置无效。")); }
        result["parts"] = parts.clone();
    }
    for field in ["capabilities", "omittedContent"] {
        if let Some(values) = value.get(field) {
            if values.as_array().is_none_or(|values| values.len() > 64 || values.iter().any(|item| !valid_text(item, 512))) {
                return Err(extraction_failure("Document capability metadata is invalid. / 文档能力元信息无效。"));
            }
            result[field] = values.clone();
        }
    }
    if value["format"] == "image" {
        if ["pageCount", "pages", "pageWindow", "uncoveredRanges"]
            .iter()
            .any(|field| value.get(field).is_some())
        {
            return Err(extraction_failure(
                "Image frames cannot claim PDF page positions. / 图像帧不能声称 PDF 页码。",
            ));
        }
        let image = &value["image"];
        require_keys(
            image,
            &[
                "encoding",
                "width",
                "height",
                "decodedWidth",
                "decodedHeight",
                "frameCount",
                "processedFrames",
                "resampled",
                "orientation",
            ],
            "Image dimensions",
        )?;
        let range = &value["textRange"];
        require_keys(range, &["startOffset", "endOffset"], "Image text range")?;
        let ocr = &value["ocr"];
        require_keys(
            ocr,
            &["backend", "version", "language"],
            "Image OCR identity",
        )?;
        if !member(
            &image["encoding"],
            &["png", "jpeg", "bmp", "gif", "tiff", "webp"],
        ) || !safe_integer(&image["width"], 1, 65536)
            || !safe_integer(&image["height"], 1, 65536)
            || image["width"]
                .as_u64()
                .unwrap_or(u64::MAX)
                .saturating_mul(image["height"].as_u64().unwrap_or(u64::MAX))
                > 64 * 1024 * 1024
            || !safe_integer(&image["decodedWidth"], 1, 4000)
            || !safe_integer(&image["decodedHeight"], 1, 4000)
            || image["frameCount"] != 1
            || image["processedFrames"] != json!([1])
            || value["complete"] != true
            || !image["resampled"].is_boolean()
            || image["orientation"] != "exif-respected"
            || range["startOffset"] != 0
            || !safe_integer(&range["endOffset"], 1, MAX_SOURCE_CHARACTERS)
            || text_length.is_some_and(|length| length != &range["endOffset"])
            || ocr["backend"] != "windows-media-ocr"
            || !valid_text(&ocr["version"], 128)
            || !valid_text(&ocr["language"], 128)
        {
            return Err(extraction_failure(
                "Invalid image text, dimensions, coverage or OCR identity. / 图像文字、尺寸、覆盖或 OCR 身份无效。",
            ));
        }
        for field in ["image", "textRange", "complete", "ocr"] {
            result[field] = value[field].clone();
        }
        return Ok(result);
    }
    if value.get("image").is_some() || value.get("textRange").is_some() {
        return Err(extraction_failure(
            "Raster image fields require image format. / 栅格字段仅属于图像格式。",
        ));
    }
    if value["format"] != "pdf" {
        if [
            "pageCount",
            "pages",
            "pageWindow",
            "complete",
            "uncoveredRanges",
            "ocr",
        ]
        .iter()
        .any(|key| value.get(key).is_some())
        {
            return Err(extraction_failure(
                "DOCX text extraction cannot claim rendered page positions. / DOCX 文本提取不能声称已得到渲染页码。",
            ));
        }
        return Ok(result);
    }
    let window = value.get("pageWindow").filter(|window| {
        !window.is_null()
            && window != &&json!(false)
            && window != &&json!(0)
            && window != &&json!("")
    });
    if let Some(window) = window {
        require_keys(
            window,
            &["version", "startPage", "endPage", "nextPage"],
            "PDF page window",
        )?;
        if window["version"] != "pdf-page-window-v1"
            || !safe_integer(&window["startPage"], 1, MAX_SAFE_INTEGER)
            || !safe_integer(
                &window["endPage"],
                window["startPage"].as_u64().unwrap_or(MAX_SAFE_INTEGER),
                value["pageCount"].as_u64().unwrap_or(0),
            )
            || window["endPage"]
                .as_u64()
                .unwrap_or(0)
                .saturating_sub(window["startPage"].as_u64().unwrap_or(0))
                >= 100
            || window.get("nextPage")
                != Some(&if window["endPage"] != value["pageCount"] {
                    json!(window["endPage"].as_u64().unwrap_or(0) + 1)
                } else {
                    Value::Null
                })
        {
            return Err(extraction_failure(
                "Invalid PDF page window. / PDF 页窗口无效。",
            ));
        }
    }
    let expected_count = window
        .map(|window| {
            window["endPage"].as_u64().unwrap_or(0) - window["startPage"].as_u64().unwrap_or(0) + 1
        })
        .unwrap_or(value["pageCount"].as_u64().unwrap_or(0));
    if !safe_integer(&value["pageCount"], 1, 1_000_000)
        || !value["pages"].as_array().is_some_and(|pages| {
            !pages.is_empty() && pages.len() <= 100 && pages.len() as u64 == expected_count
        })
    {
        return Err(extraction_failure(
            "Invalid PDF page ranges. / PDF 页范围无效。",
        ));
    }
    let mut previous_end = 0;
    let mut pages = Vec::new();
    for (index, page) in value["pages"].as_array().unwrap().iter().enumerate() {
        require_keys(
            page,
            &["page", "startOffset", "endOffset", "method"],
            "PDF page range",
        )?;
        let start_page = window
            .map(|window| window["startPage"].as_u64().unwrap_or(1))
            .unwrap_or(1);
        if page["page"] != json!(start_page + index as u64)
            || page
                .get("method")
                .is_some_and(|method| !member(method, &["text", "ocr"]))
            || !safe_integer(&page["startOffset"], previous_end, MAX_SAFE_INTEGER)
            || !safe_integer(
                &page["endOffset"],
                page["startOffset"].as_u64().unwrap_or(MAX_SAFE_INTEGER),
                MAX_SOURCE_CHARACTERS,
            )
            || text_length.is_some_and(|length| {
                page["endOffset"].as_f64().unwrap_or(0.0) > length.as_f64().unwrap_or(0.0)
            })
        {
            return Err(extraction_failure(
                "PDF page range differs from extracted text. / PDF 页范围与提取正文不匹配。",
            ));
        }
        previous_end = page["endOffset"].as_u64().unwrap_or(0);
        let mut result = json!({"page": page["page"], "startOffset": page["startOffset"], "endOffset": page["endOffset"]});
        if let Some(method) = page.get("method") {
            result["method"] = method.clone();
        }
        pages.push(result);
    }
    if let Some(window) = window {
        let complete = window["startPage"] == 1 && window["endPage"] == value["pageCount"];
        let mut uncovered = Vec::new();
        if window["startPage"].as_u64().unwrap_or(1) > 1 {
            uncovered.push(
                json!({"startPage": 1, "endPage": window["startPage"].as_u64().unwrap() - 1}),
            );
        }
        if window["endPage"] != value["pageCount"] {
            uncovered.push(json!({"startPage": window["endPage"].as_u64().unwrap() + 1, "endPage": value["pageCount"]}));
        }
        if value["complete"] != json!(complete) || value["uncoveredRanges"] != json!(uncovered) {
            return Err(extraction_failure(
                "PDF coverage differs from its page window. / PDF 覆盖说明与页窗口不一致。",
            ));
        }
        result["pageWindow"] = window.clone();
        result["complete"] = json!(complete);
        result["uncoveredRanges"] = json!(uncovered);
    } else if value.get("complete").is_some() || value.get("uncoveredRanges").is_some() {
        return Err(extraction_failure(
            "PDF coverage requires its explicit window. / PDF 覆盖说明必须带显式窗口。",
        ));
    }
    let has_ocr = pages.iter().any(|page| page["method"] == "ocr");
    if let Some(ocr) = value.get("ocr") {
        require_keys(ocr, &["backend", "version", "language"], "PDF OCR identity")?;
        if ocr["backend"] != "windows-media-ocr"
            || !valid_text(&ocr["version"], 128)
            || !valid_text(&ocr["language"], 128)
            || !has_ocr
        {
            return Err(extraction_failure(
                "Invalid OCR extraction identity. / OCR 提取身份无效。",
            ));
        }
        result["ocr"] = ocr.clone();
    } else if has_ocr {
        return Err(extraction_failure(
            "OCR pages require their backend identity. / OCR 页面必须带识别后端身份。",
        ));
    }
    result["pageCount"] = value["pageCount"].clone();
    result["pages"] = json!(pages);
    Ok(result)
}
fn file_window(value: &Value, text_length: Option<&Value>) -> ContractResult {
    require_keys(
        value,
        &[
            "version",
            "encoding",
            "offsetUnit",
            "startOffset",
            "endOffset",
            "startLine",
            "endLine",
            "startByte",
            "endByte",
            "contentHash",
            "textBytes",
            "rawContentHash",
            "textContentHash",
            "totalCharacters",
            "metadata",
        ],
        "source file window",
    )?;
    let numbers = [
        "startOffset",
        "endOffset",
        "startByte",
        "endByte",
        "startLine",
        "endLine",
        "textBytes",
        "totalCharacters",
    ];
    let number = |key: &str| value[key].as_u64().unwrap_or(0);
    let metadata = &value["metadata"];
    if value["version"] != "text-file-window-v1"
        || !member(&value["encoding"], &["utf-8", "utf-16le"])
        || value["offsetUnit"] != "utf16-code-units"
        || ["contentHash", "rawContentHash", "textContentHash"]
            .iter()
            .any(|key| !hash(&value[key]))
        || numbers
            .iter()
            .any(|key| !safe_integer(&value[key], 0, MAX_SAFE_INTEGER))
        || number("endOffset") <= number("startOffset")
        || number("endByte") <= number("startByte")
        || number("endOffset").saturating_sub(number("startOffset")) > 128 * 1024
        || number("endByte").saturating_sub(number("startByte")) > 512 * 1024
        || number("textBytes") > 512 * 1024
        || number("startLine") < 1
        || number("endLine") < number("startLine")
        || number("endOffset") > number("totalCharacters")
        || text_length.is_some_and(|length| {
            length.as_u64() != Some(number("endOffset") - number("startOffset"))
        })
        || ["sizeBytes", "mtimeMs", "ctimeMs", "device", "inode"]
            .iter()
            .any(|key| !metadata[key].as_f64().is_some_and(f64::is_finite))
        || !safe_integer(&metadata["sizeBytes"], 0, MAX_SAFE_INTEGER)
        || ["device", "inode"]
            .iter()
            .any(|key| metadata[key].as_f64().unwrap_or(-1.0) < 0.0)
        || number("endByte") as f64 > metadata["sizeBytes"].as_f64().unwrap_or(0.0)
    {
        return Err(ContractError::new(
            "Invalid source file window. / 来源文件窗口无效。",
            "INVALID_RETRIEVAL_WINDOW",
            400,
        ));
    }
    Ok(value.clone())
}
fn structure_common(value: &Value) -> Result<(), ContractError> {
    let language_valid = value.get("language") == Some(&Value::Null)
        && value["parseStatus"] == "unavailable"
        || value["language"].as_str().is_some_and(|language| {
            !language.is_empty()
                && language.len() <= 64
                && language.as_bytes()[0].is_ascii_lowercase()
                && language.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'+' | b'#' | b'.' | b'-')
                })
        });
    if !member(&value["domain"], &["knowledge", "code"])
        || !member(&value["parseStatus"], &["parsed", "partial", "unavailable"])
        || !language_valid
    {
        return Err(structure_failure());
    }
    Ok(())
}
fn structure(value: &Value) -> ContractResult {
    require_keys(
        value,
        &[
            "domain",
            "language",
            "parserVersion",
            "parseStatus",
            "diagnosticCodes",
        ],
        "source structure",
    )?;
    structure_common(value)?;
    let diagnostics = value
        .get("diagnosticCodes")
        .cloned()
        .unwrap_or_else(|| json!([]));
    if !valid_text(&value["parserVersion"], 512)
        || !diagnostics.as_array().is_some_and(|codes| {
            codes.len() <= 32
                && codes.iter().all(|code| {
                    code.as_str().is_some_and(|code| {
                        !code.is_empty()
                            && code.len() <= 128
                            && code.bytes().all(|byte| {
                                byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                            })
                    })
                })
        })
    {
        return Err(structure_failure());
    }
    Ok(
        json!({"domain": value["domain"], "language": value["language"], "parserVersion": value["parserVersion"], "parseStatus": value["parseStatus"], "diagnosticCodes": diagnostics}),
    )
}
fn chunk_structure(value: &Value, args: &Value) -> ContractResult {
    require_keys(
        value,
        &[
            "domain",
            "language",
            "kind",
            "symbolName",
            "qualifiedName",
            "parentSymbol",
            "sectionTitle",
            "sectionPath",
            "unitStartOffset",
            "unitEndOffset",
            "parseStatus",
        ],
        "chunk structure",
    )?;
    structure_common(value)?;
    if !valid_text(&value["kind"], 64)
        || !value["kind"].as_str().is_some_and(|kind| {
            kind.as_bytes()[0].is_ascii_lowercase()
                && kind
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(structure_failure());
    }
    if let Some(source) = args.get("sourceStructure").filter(|value| !value.is_null()) {
        if value["domain"] != source["domain"]
            || value["language"] != source["language"]
            || source["parseStatus"] == "unavailable" && value["parseStatus"] != "unavailable"
        {
            return Err(structure_failure());
        }
    }
    let mut result = json!({"domain": value["domain"], "language": value["language"], "kind": value["kind"], "parseStatus": value["parseStatus"]});
    for field in [
        "symbolName",
        "qualifiedName",
        "parentSymbol",
        "sectionTitle",
    ] {
        if let Some(value) = value.get(field) {
            if !valid_text(value, if field == "sectionTitle" { 256 } else { 1024 }) {
                return Err(structure_failure());
            }
            result[field] = value.clone();
        }
    }
    if let Some(path) = value.get("sectionPath") {
        if !path
            .as_array()
            .is_some_and(|path| path.len() <= 16 && path.iter().all(|title| valid_text(title, 256)))
        {
            return Err(structure_failure());
        }
        result["sectionPath"] = path.clone();
    }
    if !safe_integer(&value["unitStartOffset"], 0, MAX_SAFE_INTEGER)
        || !safe_integer(
            &value["unitEndOffset"],
            0,
            args["textLength"].as_u64().unwrap_or(0),
        )
        || value["unitEndOffset"].as_u64().unwrap_or(0)
            <= value["unitStartOffset"].as_u64().unwrap_or(0)
        || value["unitStartOffset"].as_f64().unwrap_or(0.0)
            > args["chunk"]["startOffset"].as_f64().unwrap_or(0.0)
        || value["unitEndOffset"].as_f64().unwrap_or(0.0)
            < args["chunk"]["endOffset"].as_f64().unwrap_or(0.0)
        || args["splitsStart"].as_bool().unwrap_or(false)
        || args["splitsEnd"].as_bool().unwrap_or(false)
    {
        return Err(structure_failure());
    }
    result["unitStartOffset"] = value["unitStartOffset"].clone();
    result["unitEndOffset"] = value["unitEndOffset"].clone();
    Ok(result)
}
pub fn normalize_path(text: &str) -> String {
    let text = text.replace('\\', "/");
    let mut text = text.as_str();
    while let Some(rest) = text.strip_prefix("./") {
        text = rest;
    }
    let mut result = String::new();
    let mut previous_slash = false;
    for character in text.chars() {
        if character != '/' || !previous_slash {
            result.push(character);
        }
        previous_slash = character == '/';
    }
    result.to_lowercase()
}
fn intent(value: &Value) -> ContractResult {
    require_keys(
        value,
        &["domain", "symbol", "path", "preferredDomain"],
        "retrieval intent",
    )?;
    let domain = value
        .get("domain")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| json!("mixed"));
    if !member(&domain, &["knowledge", "code", "mixed"]) {
        return Err(failure("Invalid retrieval domain. / 检索领域无效。"));
    }
    if value
        .get("preferredDomain")
        .is_some_and(|value| !member(value, &["knowledge", "code", "mixed"]))
    {
        return Err(failure(
            "Invalid preferred retrieval domain. / 检索偏好领域无效。",
        ));
    }
    if value
        .get("symbol")
        .is_some_and(|value| !valid_text(value, 1024))
        || value
            .get("path")
            .is_some_and(|value| !valid_text(value, 4096))
    {
        return Err(failure(
            "Invalid exact retrieval target. / 精确检索目标无效。",
        ));
    }
    let mut result = json!({"domain": domain});
    for field in ["symbol", "preferredDomain"] {
        if let Some(value) = value.get(field) {
            result[field] = value.clone();
        }
    }
    if let Some(path) = value.get("path") {
        result["path"] = json!(normalize_path(path.as_str().unwrap()));
    }
    Ok(result)
}
fn reference(value: &Value) -> ContractResult {
    require_keys(
        value,
        &[
            "sourceId",
            "scopeKey",
            "sourceRevision",
            "contentHash",
            "chunkId",
            "chunkHash",
            "derivationSignature",
        ],
        "source reference",
    )?;
    source_id(&value["sourceId"])?;
    scopes(&json!([value["scopeKey"]]))?;
    if !hash(&value["contentHash"]) {
        return Err(failure("Invalid source hash. / 资料哈希无效。"));
    }
    if !safe_integer(&value["sourceRevision"], 0, MAX_SAFE_INTEGER)
        && !value["sourceRevision"]
            .as_str()
            .is_some_and(|value| !value.is_empty() && utf16_length(value) <= 256)
    {
        return Err(failure(
            "Invalid source reference revision. / 资料引用版本无效。",
        ));
    }
    if value.get("chunkId").is_some() != value.get("chunkHash").is_some()
        || value
            .get("chunkId")
            .is_some_and(|value| !valid_text(value, 256))
        || value.get("chunkHash").is_some_and(|value| !hash(value))
    {
        return Err(failure("Invalid chunk reference. / 分块引用无效。"));
    }
    if value
        .get("derivationSignature")
        .is_some_and(|value| !hash(value))
    {
        return Err(failure(
            "Invalid derivation signature. / 派生索引签名无效。",
        ));
    }
    Ok(Value::Null)
}

/// Source metadata never substitutes for the original bytes, permission scope or current file version.
/// 来源元信息不能替代原始字节、权限范围或当前文件版本。
pub fn apply(operation: &str, args: &Value) -> ContractResult {
    let value = &args["value"];
    match operation {
        "storage.retrieval.extraction" => source_extraction(value, args.get("textLength")),
        "storage.retrieval.fileWindow" => file_window(value, args.get("textLength")),
        "storage.retrieval.scopes" => scopes(value),
        "storage.retrieval.sourceId" => source_id(value),
        "storage.retrieval.structure" => structure(value),
        "storage.retrieval.chunkStructure" => chunk_structure(value, args),
        "storage.retrieval.intent" => intent(value),
        "storage.retrieval.normalizePath" => {
            Ok(json!(normalize_path(value.as_str().unwrap_or(""))))
        }
        "storage.retrieval.reference" => reference(value),
        _ => Err(ContractError::new(
            "Unknown retrieval contract.",
            "AGENT_CORE_UNKNOWN_OPERATION",
            400,
        )),
    }
}
