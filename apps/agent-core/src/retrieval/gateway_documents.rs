//! Offline document extraction keeps bounded decoding, exact source identity and page coverage.
//! 离线文档提取保留有界解码、精确来源身份与页面覆盖，不通过浏览器或 Node SDK。

use crate::storage::{ContractError, gateway};
use serde_json::{Value, json};
use std::{
    io::{Cursor, Read},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn failure(code: &'static str) -> ContractError {
    gateway::error("文档提取未完成，原文件已保留。", code, 422)
}
fn cancelled(token: &Arc<AtomicBool>) -> Result<(), ContractError> {
    if token.load(Ordering::Acquire) {
        Err(failure("ABORT_ERR"))
    } else {
        crate::storage::sqlite_store::cancelled()
    }
}

/// Equal OCR text cannot prove equal source bytes or decoder identity.
/// 相同识别文字不能证明原文件字节或解码身份未变化；回源与发布共用这一检查。
pub(crate) fn verify_extraction(expected: &Value, current: &Value) -> Result<(), ContractError> {
    if expected.is_object() && expected != current {
        Err(gateway::error(
            "文档原始字节或提取版本已变化，请重新检索。",
            "STALE_RETRIEVAL_SOURCE",
            409,
        ))
    } else {
        Ok(())
    }
}
pub fn read(path: &Path, limits: &Value, cancel: &Arc<AtomicBool>) -> Result<Value, ContractError> {
    read_impl(None, path, limits, cancel, true)
}
pub fn read_with_config(
    config: &crate::gateway_contract::GatewayConfig,
    path: &Path,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    read_impl(Some(config), path, limits, cancel, true)
}

/// Tool callers already verified the exact file scope, including the conversation's own sandbox.
/// 工具调用端已校验精确文件权限及会话自有沙箱；索引入口仍排除全部正式数据目录。
pub(crate) fn read_authorized_with_config(
    config: &crate::gateway_contract::GatewayConfig,
    path: &Path,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    read_impl(Some(config), path, limits, cancel, false)
}
fn read_impl(
    config: Option<&crate::gateway_contract::GatewayConfig>,
    path: &Path,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    exclude_app_data: bool,
) -> Result<Value, ContractError> {
    let mut policy = crate::tools::service_source_reading::SourceReadingState::default();
    let mut fields = limits.clone();
    fields["path"] = json!(path);
    if let Some(config) = config.filter(|_| exclude_app_data) {
        fields["excludedRoots"] =
            json!([config.data_root, config.data_home, config.extension_root]);
    }
    let policy = policy
        .apply("source.reading.filePolicy", &fields, &Some(cancel.clone()))
        .map_err(|error| {
            gateway::error(
                &error.message,
                if error.code == "UNSUPPORTED_RETRIEVAL_SOURCE" {
                    "UNSUPPORTED_RETRIEVAL_SOURCE"
                } else {
                    "PROTECTED_RETRIEVAL_SOURCE"
                },
                error.status_code,
            )
        })?;
    let format = policy["format"].as_str();
    if format.is_none() {
        let mut args = limits.clone();
        args["path"] = json!(path);
        args["decodeText"] = json!(true);
        return crate::tools::service_source_reading::execute_io(
            "ordinaryRead",
            &args,
            &Some(cancel.clone()),
        )
        .map_err(|error| {
            gateway::error(
                &error.message,
                "RETRIEVAL_SOURCE_UNAVAILABLE",
                error.status_code,
            )
        });
    }
    let maximum = policy["inputBytes"].as_u64().unwrap().min(32 * 1024 * 1024);
    let (bytes, metadata) = crate::tools::service_source_reading::read_binary_stable(
        path,
        maximum,
        &Some(cancel.clone()),
    )
    .map_err(|error| {
        failure(
            if error.code.contains("LIMIT")
                || error
                    .details
                    .get("observedBytes")
                    .and_then(Value::as_u64)
                    .is_some_and(|bytes| bytes > maximum)
            {
                "DOCUMENT_INPUT_LIMIT"
            } else if error.code == "ABORT_ERR" {
                "ABORT_ERR"
            } else {
                "DOCUMENT_READ_FAILED"
            },
        )
    })?;
    let mut decoder_limits = limits.clone();
    decoder_limits["maximumDocumentOutputBytes"] = policy["outputBytes"].clone();
    let result = parse_bytes(config, path, &bytes, &decoder_limits, cancel)?;
    let mut result = result;
    result["metadata"] = metadata.clone();
    for window in result["documentWindows"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        window["metadata"] = metadata.clone();
    }
    Ok(result)
}

/// Downloaded and archived bytes use the same decoder without inventing a temporary source path.
/// 下载和包内字节共用正式解码器；虚拟成员名称用于选解析器，不被伪装成真实磁盘来源。
pub(crate) fn parse_bytes(
    config: Option<&crate::gateway_contract::GatewayConfig>,
    name: &Path,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    let input_maximum = limits["maximumDocumentInputBytes"].as_u64().unwrap_or(32 * 1024 * 1024)
        .min(limits["maximumSourceBytes"].as_u64().unwrap_or(32 * 1024 * 1024));
    if bytes.is_empty() || bytes.len() as u64 > input_maximum {
        return Err(failure("DOCUMENT_INPUT_LIMIT"));
    }
    let digest = super::gateway_index::hash_bytes(bytes);
    let Some(format) = crate::document_formats::detect(name) else {
        if !crate::tools::service_source_reading::supported(name) {
            return Err(failure("UNSUPPORTED_RETRIEVAL_SOURCE"));
        }
        let text = if let Some(body) = bytes.strip_prefix(&[0xff, 0xfe]) {
            crate::tools::service_source_reading::decode(body, "utf-16le")
        } else {
            crate::tools::service_source_reading::decode(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes), "utf-8")
        }.map_err(|_| failure("DOCUMENT_TEXT_ENCODING_UNSUPPORTED"))?;
        let maximum = limits["maximumDocumentOutputBytes"].as_u64().unwrap_or(2 * 1024 * 1024).clamp(1024, 32 * 1024 * 1024) as usize;
        if text.len() > maximum {
            let mut windows = Vec::new(); let mut start = 0; let mut utf16_start = 0;
            while start < text.len() {
                cancelled(cancel)?;
                let mut end = start.saturating_add(maximum).min(text.len());
                while !text.is_char_boundary(end) { end -= 1; }
                let part = &text[start..end]; let length = part.encode_utf16().count();
                windows.push(json!({"text":part,"rawContentHash":digest,"textRange":{"startOffset":utf16_start,"endOffset":utf16_start+length,"unit":"utf16"},"windowIndex":windows.len(),"complete":true}));
                start = end; utf16_start += length;
            }
            return Ok(json!({"documentWindows":windows,"rawContentHash":digest,"complete":true}));
        }
        return Ok(json!({"text":text,"rawContentHash":digest}));
    };
    use crate::document_formats::ParserFamily;
    let result = match format.family {
        ParserFamily::Pdf => pdf(config, bytes, limits, cancel, &digest),
        ParserFamily::Word => docx(bytes, limits, cancel, &digest),
        ParserFamily::Image => image(config, format.name, bytes, limits, cancel, &digest),
        ParserFamily::Office => super::gateway_office_documents::decode(format.name, bytes, limits, cancel, &digest),
        ParserFamily::Archive => Err(failure("DOCUMENT_ARCHIVE_REQUIRES_EXPANSION")),
        ParserFamily::LegacyOffice => super::gateway_legacy_office::decode(format.name, bytes, limits, cancel, &digest),
    }?;
    cancelled(cancel)?;
    Ok(result)
}
/// The existing Windows OCR protocol keeps a real process owner and lease until the child exits.
/// 沿用 Windows OCR 协议，真实进程所有者与资源租约持续到子进程确认退出。
fn recognize(
    config: &crate::gateway_contract::GatewayConfig,
    bytes: &[u8],
    pages: &[u32],
    maximum: usize,
    format: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    use crate::tools::{gateway_process, service_document_extraction::DocumentExtractionState};
    use base64::Engine;
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncWriteExt, BufReader};
    let mut control = DocumentExtractionState::default();
    let apply = |control: &mut DocumentExtractionState, operation: &str, args: Value| {
        control.apply(operation, &args).map_err(|error| {
            failure(match error.code.as_str() {
                "OCR_INVALID_REQUEST" => "OCR_INVALID_REQUEST",
                "OCR_LANGUAGE_UNAVAILABLE" => "OCR_LANGUAGE_UNAVAILABLE",
                "OCR_INVALID_PAGE" => "OCR_INVALID_PAGE",
                "OCR_IMAGE_MULTIFRAME_UNSUPPORTED" => "OCR_IMAGE_MULTIFRAME_UNSUPPORTED",
                "OCR_IMAGE_PIXEL_LIMIT" => "OCR_IMAGE_PIXEL_LIMIT",
                "OCR_OUTPUT_LIMIT" => "OCR_OUTPUT_LIMIT",
                "OCR_INVALID_RESULT" => "OCR_INVALID_RESULT",
                "OCR_CANCELLED" => "ABORT_ERR",
                "OCR_UNAVAILABLE" => "OCR_UNAVAILABLE",
                "DOCUMENT_INVALID_INPUT" => "DOCUMENT_INVALID_INPUT",
                _ => "OCR_FAILED",
            })
        })
    };
    let plan = apply(
        &mut control,
        "ocrOpen",
        json!({"bytesValid":true,"byteLength":bytes.len(),"pages":pages,"maximumOutputBytes":maximum,"format":format,"timeoutMs":20000}),
    )?;
    let result = super::gateway_embedding::runtime()?.block_on(async {
        let service = crate::resources::gateway::global()
            .ok_or_else(|| failure("DOCUMENT_RESOURCE_FAILED"))?;
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let token = cancel.clone();
        let monitor = tokio::spawn(async move {
            while !token.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let _ = sender.send(true);
        });
        let mut request = plan["resource"].clone();
        request["taskId"] = json!(gateway::new_id());
        request["workspaceId"] = json!("local");
        request["gpuMemoryBytes"] = json!(0);
        request["ttlMs"] = json!(30000);
        let acquired = service.acquire(request, receiver).await;
        monitor.abort();
        let mut lease = acquired.map_err(|_| failure("DOCUMENT_RESOURCE_FAILED"))?;
        let mut command = tokio::process::Command::new(&config.toolhost_exe);
        gateway_process::hide(&mut command);
        command.arg("--document-ocr").stdin(Stdio::piped())
            .stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let process = command.spawn().map_err(|_| failure("OCR_UNAVAILABLE"));
        let result = match process {
            Ok(mut child) => {
                let mut child_owner = None;
                let output = async {
                    child_owner = Some(gateway_process::OwnedProcessJob::attach(&child)
                        .map_err(|error| gateway::error(&error.message, "OCR_UNAVAILABLE", error.status))?);
                    let registered = service.request("registerExecutor",
                        json!({"leaseId":lease.allocation["leaseId"],"processId":child.id()}), Duration::from_secs(5))
                        .await.map_err(|_| failure("DOCUMENT_RESOURCE_FAILED"))?;
                    apply(&mut control, "registered", json!({"status":registered["status"]}))?;
                    let mut stdin = child.stdin.take().ok_or_else(|| failure("OCR_FAILED"))?;
                    let stdout = child.stdout.take().ok_or_else(|| failure("OCR_FAILED"))?;
                    let mut wire = json!({"operation":"document_ocr","schemaVersion":1,
                        "bytesBase64":base64::engine::general_purpose::STANDARD.encode(bytes),
                        "pages":pages,"maximumOutputBytes":maximum,"format":format}).to_string();
                    wire.push('\n');
                    // EOF is the ToolHost's owner-loss cancellation signal; retain stdin through the receipt and exit.
                    // EOF 表示调用方丢失并触发 ToolHost 取消；收到回执并确认退出前保留标准输入。
                    stdin.write_all(wire.as_bytes()).await.map_err(|_| failure("OCR_FAILED"))?;
                    // JSON escaping and recognized UTF-8 text have separate bounded budgets.
                    // JSON 转义帧与识别文字的 UTF-8 字节数分别按额度核验。
                    let wire = gateway_process::bounded_line(&mut BufReader::new(stdout), maximum.saturating_mul(6) + 65536)
                        .await.map_err(|_| failure("DOCUMENT_OUTPUT_LIMIT"))?
                        .ok_or_else(|| failure("OCR_INVALID_RESULT"))?;
                    apply(&mut control, "ocrChunk", json!({"id":plan["id"],"bytes":wire.len()}))?;
                    let value: Value = serde_json::from_str(&wire).map_err(|_| failure("OCR_INVALID_RESULT"))?;
                    let exit = child.wait().await.map_err(|_| failure("OCR_FAILED"))?;
                    apply(&mut control, "ocrResult", json!({"id":plan["id"],"result":value,
                        "exitCode":exit.code(),"textBytes":value["pages"].as_array().map(|pages|
                            pages.iter().map(|page|page["text"].as_str().map(str::len)).collect::<Vec<_>>())}))?;
                    Ok(value)
                };
                let aborted = async {
                    while !cancel.load(Ordering::Acquire) {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                };
                let timeout = Duration::from_millis(plan["timeoutMs"].as_u64().unwrap_or(20000));
                let result = tokio::select! {
                    result = output => result,
                    _ = aborted => Err(failure("ABORT_ERR")),
                    _ = tokio::time::sleep(timeout) => Err(failure("DOCUMENT_DECODER_TIMEOUT")),
                };
                if result.is_err() { gateway_process::terminate(&mut child).await; }
                let _ = child.wait().await;
                drop(child_owner);
                result
            }
            Err(error) => Err(error),
        };
        lease.release_confirmed().await.map_err(|_| failure("DOCUMENT_RESOURCE_FAILED"))?;
        result
    });
    let _ = apply(&mut control, "ocrClose", json!({"id":plan["id"]}));
    result
}

/// Raster OCR publishes recognized text with the original byte hash, not an invented image description.
/// 栅格 OCR 发布识别文字并保留原图哈希，不把文字识别冒充图像语义理解。
fn image(
    config: Option<&crate::gateway_contract::GatewayConfig>,
    encoding: &str,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    if limits["ocrDisabled"] == true {
        return Err(failure("OCR_UNAVAILABLE"));
    }
    let maximum = limits["maximumDocumentOutputBytes"]
        .as_u64()
        .unwrap_or(2 * 1024 * 1024)
        .min(
            limits["maximumSourceBytes"]
                .as_u64()
                .unwrap_or(32 * 1024 * 1024),
        )
        .min(2 * 1024 * 1024) as usize;
    let result = recognize(
        config.ok_or_else(|| failure("OCR_UNAVAILABLE"))?,
        bytes,
        &[1],
        maximum,
        encoding,
        cancel,
    )?;
    let text = result["pages"][0]["text"].as_str().unwrap();
    if text.trim().is_empty() {
        return Err(failure("DOCUMENT_TEXT_UNAVAILABLE"));
    }
    let extraction = json!({"format":"image","version":"windows-image-ocr-text-v1",
        "rawContentHash":digest,"image":result["image"],"complete":true,
        "textRange":{"startOffset":0,"endOffset":text.encode_utf16().count()},
        "ocr":{"backend":"windows-media-ocr","version":result["version"],"language":result["language"]}});
    crate::storage::retrieval::apply(
        "storage.retrieval.extraction",
        &json!({"value":extraction,"textLength":text.encode_utf16().count()}),
    )?;
    Ok(json!({"text":text,"extraction":extraction}))
}
fn pdf(
    config: Option<&crate::gateway_contract::GatewayConfig>,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    let document =
        lopdf::Document::load_mem(bytes).map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
    if document.is_encrypted() {
        return Err(failure("DOCUMENT_ENCRYPTED"));
    }
    let pages = document.get_pages();
    let start_page = limits["startPage"].as_u64().unwrap_or(1);
    let end_page = limits["endPage"].as_u64().unwrap_or(pages.len() as u64);
    if start_page == 0 || start_page > end_page || start_page > pages.len() as u64 {
        return Err(gateway::error("PDF 页码范围无效。", "INVALID_TOOL_ARGUMENTS", 400));
    }
    let end_page = end_page.min(pages.len() as u64);
    let selected_pages = pages
        .keys()
        .copied()
        .filter(|page| (*page as u64) >= start_page && (*page as u64) <= end_page)
        .collect::<Vec<_>>();
    let per_window = limits["maximumPdfPages"]
        .as_u64()
        .unwrap_or(100)
        .clamp(1, 100) as usize;
    let output_limit = limits["maximumDocumentOutputBytes"]
        .as_u64()
        .unwrap_or(2 * 1024 * 1024) as usize;
    let mut windows = Vec::new();
    let mut text = String::new();
    let mut descriptors = Vec::new();
    let mut first = start_page as u32;
    let mut recognized = std::collections::HashMap::new();
    let mut ocr_identity = Value::Null;
    for (position, page) in selected_pages.iter().enumerate() {
        cancelled(cancel)?;
        let mut page_text = document.extract_text(&[*page]).unwrap_or_default();
        let mut method = "text";
        if page_text.trim().is_empty() {
            if limits["ocrDisabled"] == true {
                return Err(failure("OCR_UNAVAILABLE"));
            }
            if !recognized.contains_key(page) {
                let missing = selected_pages
                    .iter()
                    .skip(position)
                    .take(per_window)
                    .filter(|page| {
                        document
                            .extract_text(&[**page])
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                    })
                    .copied()
                    .collect::<Vec<_>>();
                let result = recognize(
                    config.ok_or_else(|| failure("OCR_UNAVAILABLE"))?,
                    bytes,
                    &missing,
                    output_limit,
                    "pdf",
                    cancel,
                )?;
                ocr_identity = json!({"backend":"windows-media-ocr","version":result["version"],"language":result["language"]});
                for result in result["pages"].as_array().unwrap() {
                    recognized.insert(
                        result["page"].as_u64().unwrap() as u32,
                        result["text"].as_str().unwrap().to_string(),
                    );
                }
            }
            page_text = recognized
                .remove(page)
                .ok_or_else(|| failure("OCR_INVALID_RESULT"))?;
            method = "ocr";
        }
        if page_text.len() > output_limit {
            return Err(failure("DOCUMENT_OUTPUT_LIMIT"));
        }
        if !text.is_empty()
            && (descriptors.len() >= per_window || text.len() + page_text.len() + 1 > output_limit)
        {
            windows.push(pdf_window(
                std::mem::take(&mut text),
                std::mem::take(&mut descriptors),
                first,
                *page - 1,
                pages.len(),
                digest,
            ));
            first = *page;
        }
        let offset = text.encode_utf16().count();
        text.push_str(&page_text);
        text.push('\n');
        descriptors.push(json!({"page":page,"startOffset":offset,"endOffset":text.encode_utf16().count(),"method":method}));
        if position + 1 == selected_pages.len() {
            windows.push(pdf_window(
                std::mem::take(&mut text),
                std::mem::take(&mut descriptors),
                first,
                *page,
                pages.len(),
                digest,
            ));
        }
    }
    if windows.is_empty() {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    for window in &mut windows {
        if window["extraction"]["pages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|page| page["method"] == "ocr")
        {
            window["extraction"]["ocr"] = ocr_identity.clone();
        }
    }
    let mut value = windows[0].clone();
    value["documentWindows"] = json!(windows);
    value["documentComplete"] = json!(start_page == 1 && end_page == pages.len() as u64);
    Ok(value)
}
fn pdf_window(
    text: String,
    pages: Vec<Value>,
    start: u32,
    end: u32,
    count: usize,
    digest: &str,
) -> Value {
    let mut uncovered = Vec::new();
    if start > 1 {
        uncovered.push(json!({"startPage":1,"endPage":start-1}));
    }
    if (end as usize) < count {
        uncovered.push(json!({"startPage":end+1,"endPage":count}));
    }
    json!({"text":text,"extraction":{"format":"pdf","version":"rust-lopdf-text-v1","rawContentHash":digest,"pageCount":count,"pages":pages,
        "complete":start==1&&end as usize==count,"pageWindow":{"version":"pdf-page-window-v1","startPage":start,"endPage":end,"nextPage":if end as usize==count{Value::Null}else{json!(end+1)}},
        "uncoveredRanges":uncovered}})
}
fn docx(
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
    if archive.len() > 512 {
        return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
    }
    let mut expanded = 0u64;
    for number in 0..archive.len() {
        cancelled(cancel)?;
        let entry = archive
            .by_index(number)
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
        expanded = expanded
            .checked_add(entry.size())
            .ok_or_else(|| failure("DOCUMENT_ARCHIVE_LIMIT"))?;
        if entry.enclosed_name().is_none() || expanded > 16 * 1024 * 1024 {
            return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
        }
    }
    let mut entry = archive
        .by_name("word/document.xml")
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
    if entry.size() > 16 * 1024 * 1024 {
        return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
    }
    let mut xml = Vec::new();
    entry
        .by_ref()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut xml)
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
    let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
    reader.config_mut().trim_text(false);
    let maximum = limits["maximumDocumentOutputBytes"]
        .as_u64()
        .unwrap_or(2 * 1024 * 1024) as usize;
    let mut text = String::new();
    let mut capture = false;
    loop {
        cancelled(cancel)?;
        match reader
            .read_event()
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
        {
            quick_xml::events::Event::Start(event) => {
                if event.local_name().as_ref() == b"t" {
                    capture = true;
                }
            }
            quick_xml::events::Event::Text(event) if capture => {
                let decoded = event
                    .decode()
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                text.push_str(
                    &quick_xml::escape::unescape(&decoded)
                        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?,
                );
            }
            // CDATA is legal XML content inside w:t; keep it as text without entity unescaping.
            // w:t 内允许出现合法 XML CDATA；其内容不经过实体解码，避免丢失正文中的字面量符号。
            quick_xml::events::Event::CData(event) if capture => {
                let decoded = event
                    .decode()
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                text.push_str(&decoded);
            }
            quick_xml::events::Event::End(event) => match event.local_name().as_ref() {
                b"t" => capture = false,
                b"p" | b"tr" => text.push('\n'),
                b"tc" => text.push('\t'),
                _ => {}
            },
            quick_xml::events::Event::Empty(event) => {
                if [b"br".as_slice(), b"cr".as_slice()].contains(&event.local_name().as_ref()) {
                    text.push('\n');
                } else if event.local_name().as_ref() == b"tab" {
                    text.push('\t');
                }
            }
            quick_xml::events::Event::DocType(_) => {
                return Err(failure("DOCUMENT_XML_UNSUPPORTED"));
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
        if text.len() > maximum {
            return Err(failure("DOCUMENT_OUTPUT_LIMIT"));
        }
    }
    if text.trim().is_empty() {
        return Err(failure("DOCUMENT_TEXT_UNAVAILABLE"));
    }
    Ok(
        json!({"text":text,"extraction":{"format":"docx","version":"rust-docx-text-v1","rawContentHash":digest}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/documents")
            .join(name)
    }

    #[test]
    fn document_fixture_text_offsets_and_original_hash_survive_readback() {
        let cancel = Arc::new(AtomicBool::new(false));
        for (name, expected, format) in [
            ("account-text.pdf", "Account password reset", Some("pdf")),
            ("account-guide.docx", "账户密码重置说明", Some("docx")),
            ("account-guide.md", "账户密码重置", None),
        ] {
            let path = fixture(name);
            let first = read(&path, &json!({}), &cancel).unwrap();
            assert!(
                first["text"].as_str().unwrap().contains(expected),
                "{first}"
            );
            let second = read(&path, &json!({}), &cancel).unwrap();
            assert_eq!(first, second);
            if let Some(format) = format {
                assert_eq!(first["extraction"]["format"], format);
                assert_eq!(
                    first["extraction"]["rawContentHash"],
                    super::super::gateway_index::hash_bytes(&std::fs::read(path).unwrap())
                );
                crate::storage::retrieval::apply("storage.retrieval.extraction", &json!({"value":first["extraction"],"textLength":first["text"].as_str().unwrap().encode_utf16().count()})).unwrap();
            }
        }
    }

    #[test]
    fn docx_cdata_text_is_preserved_alongside_regular_runs() {
        use std::io::Write;

        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t><![CDATA[CDATA & literal]]></w:t></w:r></w:p>
    <w:p><w:r><w:t>regular text</w:t></w:r></w:p>
  </w:body>
</w:document>"#,
            )
            .unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let cancel = Arc::new(AtomicBool::new(false));

        let result = docx(&bytes, &json!({}), &cancel, "test-hash").unwrap();

        assert_eq!(
            result["text"], "CDATA & literal\nregular text\n",
            "CDATA and regular OOXML text should both be retained"
        );
        assert_eq!(result["extraction"]["rawContentHash"], "test-hash");
    }

    #[test]
    fn original_bytes_and_extraction_identity_cannot_be_replaced_by_equal_ocr_text() {
        let original = json!({"format":"image","rawContentHash":"original","ocr":{"version":"v2","language":"zh-Hans-CN"}});
        let mut changed = original.clone();
        assert!(verify_extraction(&original, &changed).is_ok());
        changed["rawContentHash"] = json!("changed-even-with-equal-text");
        assert_eq!(
            verify_extraction(&original, &changed).unwrap_err().code(),
            "STALE_RETRIEVAL_SOURCE"
        );
        changed = original.clone();
        changed["ocr"]["language"] = json!("en-US");
        assert!(verify_extraction(&original, &changed).is_err());
        assert!(verify_extraction(&Value::Null, &Value::Null).is_ok());
    }

    #[test]
    fn document_limits_cancel_and_missing_ocr_do_not_publish_partial_text() {
        let cancel = Arc::new(AtomicBool::new(false));
        assert_eq!(
            read(
                &fixture("account-guide.docx"),
                &json!({"maximumDocumentInputBytes":100}),
                &cancel
            )
            .unwrap_err()
            .code(),
            "DOCUMENT_INPUT_LIMIT"
        );
        assert_eq!(
            read(
                &fixture("account-guide.docx"),
                &json!({"maximumSourceBytes":100}),
                &cancel
            )
            .unwrap_err()
            .code(),
            "DOCUMENT_INPUT_LIMIT"
        );
        assert_eq!(
            read(
                &fixture("account-guide.docx"),
                &json!({"maximumDocumentOutputBytes":8}),
                &cancel
            )
            .unwrap_err()
            .code(),
            "DOCUMENT_OUTPUT_LIMIT"
        );
        assert_eq!(
            read(
                &fixture("account-guide.png"),
                &json!({"ocrDisabled":true}),
                &cancel
            )
            .unwrap_err()
            .code(),
            "OCR_UNAVAILABLE"
        );
        assert_eq!(
            read(
                &fixture("account-scan.pdf"),
                &json!({"ocrDisabled":true}),
                &cancel
            )
            .unwrap_err()
            .code(),
            "OCR_UNAVAILABLE"
        );
        cancel.store(true, Ordering::Release);
        assert!(read(&fixture("account-text.pdf"), &json!({}), &cancel).is_err());
    }
}
