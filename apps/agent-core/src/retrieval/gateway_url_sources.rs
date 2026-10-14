//! Explicit URL imports reuse the public-web transport and preserve a frozen, source-bound snapshot.
//! 明确导入链接复用公共网页传输，保留带来源身份的固定快照，不自动抓取整个网站。

use crate::{
    gateway_contract::GatewayConfig,
    storage::{ContractError, gateway},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn failure(code: &'static str) -> ContractError {
    gateway::error("链接资料导入未完成，已读取资料保留。", code, 422)
}
fn mapped(error: crate::gateway_contract::GatewayError) -> ContractError {
    let code = match error.code.as_str() {
        "WEB_URL_BLOCKED" => "WEB_URL_BLOCKED",
        "WEB_TIMEOUT" => "WEB_TIMEOUT",
        "WEB_REDIRECT_LIMIT" => "WEB_REDIRECT_LIMIT",
        "WEB_RESPONSE_TOO_LARGE" => "WEB_RESPONSE_TOO_LARGE",
        "WEB_UNSUPPORTED_ENCODING" => "WEB_UNSUPPORTED_ENCODING",
        "WEB_HTTP_STATUS" => "WEB_HTTP_STATUS",
        "WEB_PROXY_UNSUPPORTED" => "WEB_PROXY_UNSUPPORTED",
        "TOOL_CANCELLED" => "ABORT_ERR",
        _ => "WEB_NETWORK_FAILED",
    };
    gateway::error(&error.message, code, error.status)
}

/// MIME and magic select a parser; URL suffixes alone never turn binary content into plain text.
/// MIME 与魔数选择解析器，不能仅靠 URL 后缀把二进制内容当成普通文本。
fn filename(download: &crate::tools::gateway_web::PublicDownload) -> PathBuf {
    let declared = download
        .final_url
        .path_segments()
        .and_then(|segments| segments.filter(|name| !name.is_empty()).next_back())
        .unwrap_or("webpage");
    let extension = match download.media_type.as_str() {
        "application/pdf" => Some("pdf"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/msword" => Some("doc"),
        "application/vnd.ms-powerpoint" => Some("ppt"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.ms-excel.sheet.binary.macroenabled.12" => Some("xlsb"),
        "application/vnd.oasis.opendocument.text" => Some("odt"),
        "application/vnd.oasis.opendocument.presentation" => Some("odp"),
        "application/vnd.oasis.opendocument.spreadsheet" => Some("ods"),
        "application/epub+zip" => Some("epub"),
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpeg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/tiff" => Some("tiff"),
        "image/bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        "text/html" | "application/xhtml+xml" => Some("html"),
        "text/markdown" => Some("md"),
        "application/rtf" | "text/rtf" => Some("rtf"),
        "message/rfc822" => Some("eml"),
        "application/zip" => Some("zip"),
        "application/x-7z-compressed" => Some("7z"),
        "application/vnd.rar" | "application/x-rar-compressed" => Some("rar"),
        "application/gzip" => Some("gz"),
        "application/x-tar" => Some("tar"),
        "text/csv" => Some("csv"),
        "application/json" => Some("json"),
        "application/xml" | "text/xml" => Some("xml"),
        "text/plain" => Some("txt"),
        _ => None,
    };
    let bytes = &download.bytes;
    let magic = if bytes.starts_with(b"%PDF-") {
        Some("pdf")
    } else if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        // Recognize Office/ebook ZIP containers before the general archive decoder, without expanding their bodies.
        // 只检查目录识别 Office 或电子书 ZIP 容器，再决定使用文档还是通用归档解析器。
        if let Ok(archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
            let names = archive
                .file_names()
                .take(20000)
                .collect::<std::collections::HashSet<_>>();
            if names.contains("word/document.xml") {
                Some("docx")
            } else if names.contains("ppt/presentation.xml") {
                Some("pptx")
            } else if names.contains("xl/workbook.xml") {
                Some("xlsx")
            } else if names.contains("META-INF/container.xml") {
                Some("epub")
            } else if crate::document_formats::detect(Path::new(declared)).is_some_and(|format| {
                format.family == crate::document_formats::ParserFamily::Office
            }) {
                None
            } else {
                Some("zip")
            }
        } else {
            Some("zip")
        }
    } else if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        // Compound magic is shared by DOC, PPT and XLS; only the actual root stream identifies the family.
        // DOC、PPT、XLS 共用 CFB 魔数；依据真实根流选择解析器，不把一种 Office 文档误当另一种。
        cfb::CompoundFile::open(std::io::Cursor::new(bytes))
            .ok()
            .and_then(|document| {
                if document.exists("/WordDocument") {
                    Some("doc")
                } else if document.exists("/PowerPoint Document") {
                    Some("ppt")
                } else if document.exists("/Workbook") || document.exists("/Book") {
                    Some("xls")
                } else {
                    None
                }
            })
    } else if bytes.starts_with(b"\x1f\x8b") {
        Some("gz")
    } else if bytes.starts_with(b"Rar!\x1a\x07") {
        Some("rar")
    } else if bytes.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Some("7z")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpeg")
    } else {
        None
    };
    match magic.or(extension) {
        Some(extension) => PathBuf::from(format!("download.{extension}")),
        None => PathBuf::from(declared),
    }
}

pub(crate) fn scan(
    config: &GatewayConfig,
    input: &Value,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    consume: &mut impl FnMut(Value) -> Result<(), ContractError>,
) -> Result<Value, ContractError> {
    let urls = if let Some(url) = input["url"].as_str() {
        vec![json!(url)]
    } else {
        input["urls"].as_array().cloned().unwrap_or_default()
    };
    if urls.is_empty() || urls.len() > 64 || urls.iter().any(|url| !url.is_string()) {
        return Err(gateway::error(
            "链接导入需要 1 至 64 个明确 URL。",
            "INVALID_RETRIEVAL_SOURCE",
            400,
        ));
    }
    let maximum = limits["maximumDocumentInputBytes"]
        .as_u64()
        .unwrap_or(32 * 1024 * 1024)
        .min(
            limits["maximumSourceBytes"]
                .as_u64()
                .unwrap_or(32 * 1024 * 1024),
        )
        .min(32 * 1024 * 1024) as usize;
    let maximum_total = limits["maximumTotalBytes"]
        .as_u64()
        .unwrap_or(512 * 1024 * 1024);
    let maximum_files = limits["maximumFiles"].as_u64().unwrap_or(20000);
    let timeout = input["timeoutMs"]
        .as_u64()
        .unwrap_or(60000)
        .clamp(100, 120000);
    let runtime = super::gateway_embedding::runtime()?;
    let mut failures = Vec::new();
    let mut sources = 0u64;
    let mut bytes = 0u64;
    let mut text_bytes = 0u64;
    let mut seen = std::collections::HashSet::new();
    let mut requested_urls = std::collections::HashSet::new();
    let mut publication_failed = false;
    for requested in &urls {
        if cancel.load(Ordering::Acquire) {
            return Err(failure("ABORT_ERR"));
        }
        crate::storage::sqlite_store::cancelled()?;
        let requested = requested.as_str().unwrap();
        // Validate each exact user-selected URL through the same policy as web.fetch; no secondary client or scope expansion.
        // 每个用户选择的 URL 走 web.fetch 同一策略，不增加第二个 HTTP 客户端或扩展抓取范围。
        let result = (|| {
            let normalized =
                crate::tools::gateway_web::normalize_public_url(requested).map_err(mapped)?;
            if !requested_urls.insert(normalized.as_str().to_owned()) {
                return Ok(());
            }
            let download = runtime
                .block_on(crate::tools::gateway_web::download_public(
                    requested, maximum, timeout, cancel,
                ))
                .map_err(mapped)?;
            bytes = bytes.saturating_add(download.bytes.len() as u64);
            if bytes > maximum_total {
                return Err(failure("RETRIEVAL_SCAN_LIMIT"));
            }
            let origin = json!({"type":"url","requestedUrl":download.requested_url,"canonicalUrl":download.final_url.as_str(),
                "fetchedAt":download.fetched_at,"responseHash":download.response_hash,"contentType":download.content_type,
                "status":download.status,"redirects":download.redirects,"network":{"route":download.route,"addressVerification":download.resolution},
                "coverage":{"crawl":false,"requestedUrls":1,"complete":true}});
            let name = filename(&download);
            if super::gateway_archives::is_archive(&name) {
                let virtual_path = PathBuf::from(name.file_name().unwrap());
                let mut stage = |mut source: Value| {
                    if sources >= maximum_files {
                        return Err(failure("RETRIEVAL_SCAN_LIMIT"));
                    }
                    let member = source["locator"]["relativePath"]
                        .as_str()
                        .unwrap_or("archive")
                        .to_owned();
                    source["locator"]["path"] = json!(format!("{}#{member}", download.final_url));
                    source["locator"]["origin"] = origin.clone();
                    let size = source["text"].as_str().unwrap_or("").len() as u64;
                    if text_bytes.saturating_add(size) > maximum_total {
                        return Err(failure("RETRIEVAL_SCAN_LIMIT"));
                    }
                    if let Err(error) = consume(source) {
                        publication_failed = true;
                        return Err(error);
                    }
                    sources += 1;
                    text_bytes += size;
                    Ok(())
                };
                let report = super::gateway_archives::scan_bytes(
                    config,
                    &virtual_path,
                    &download.bytes,
                    limits,
                    cancel,
                    &mut stage,
                )?;
                for failure in report["failures"].as_array().into_iter().flatten() {
                    let mut failure = failure.clone();
                    failure["url"] = json!(requested);
                    failures.push(failure);
                }
                return Ok(());
            }
            let mut decoder_limits = limits.clone();
            decoder_limits["documentContentType"] = json!(download.content_type);
            let parsed = super::gateway_documents::parse_bytes(
                Some(config),
                &name,
                &download.bytes,
                &decoder_limits,
                cancel,
            )?;
            let windows = parsed["documentWindows"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![parsed]);
            for (index, value) in windows.into_iter().enumerate() {
                let text = value["text"]
                    .as_str()
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| failure("DOCUMENT_TEXT_UNAVAILABLE"))?;
                if sources >= maximum_files
                    || text_bytes.saturating_add(text.len() as u64) > maximum_total
                {
                    return Err(failure("RETRIEVAL_SCAN_LIMIT"));
                }
                let hash = super::gateway_index::hash(text);
                let key = json!([
                    download.final_url.as_str(),
                    download.response_hash,
                    index,
                    hash
                ])
                .to_string();
                if !seen.insert(key) {
                    continue;
                }
                let title = input["title"]
                    .as_str()
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or(download.final_url.as_str());
                let mut locator = json!({"path":download.final_url.as_str(),"relativePath":format!("{}#{index}",download.final_url),"origin":origin,"urlWindow":{"index":index,"textRange":value["textRange"]}});
                if let Some(extraction) = value.get("extraction") {
                    locator["extraction"] = extraction.clone();
                }
                let revision = super::gateway_index::hash(
                    &json!([hash, download.response_hash, value["extraction"], index]).to_string(),
                );
                if let Err(error) = consume(
                    json!({"title":title,"text":text,"contentHash":hash,"sourceRevision":revision,"locator":locator}),
                ) {
                    publication_failed = true;
                    return Err(error);
                }
                sources += 1;
                text_bytes += text.len() as u64;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if error.code() == "ABORT_ERR" || publication_failed {
                return Err(error);
            }
            failures.push(json!({"url":requested,"errorCode":error.code()}));
        }
    }
    Ok(
        json!({"scannedFiles":urls.len(),"publishedFiles":sources,"failedFiles":failures.len(),"failures":failures,
        "incomplete":!failures.is_empty(),"sourceBytes":bytes,"textBytes":text_bytes,"networkScope":{"crawl":false,"requestedUrls":urls}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(
        bytes: Vec<u8>,
        url: &str,
        mime: &str,
    ) -> crate::tools::gateway_web::PublicDownload {
        crate::tools::gateway_web::PublicDownload {
            bytes,
            requested_url: url.to_owned(),
            final_url: url::Url::parse(url).unwrap(),
            content_type: mime.to_owned(),
            media_type: mime.to_owned(),
            status: 200,
            redirects: vec![],
            fetched_at: "2026-10-13T00:00:00Z".to_owned(),
            response_hash: "a".repeat(64),
            route: "direct",
            resolution: "public-dns-pinned",
        }
    }
    #[test]
    fn legacy_compound_streams_override_an_incorrect_mime_without_url_suffix() {
        for (bytes, mime, name) in [
            (
                &include_bytes!("../../tests/fixtures/legacy-word/bilingual-stories.doc")[..],
                "application/vnd.ms-powerpoint",
                "download.doc",
            ),
            (
                &include_bytes!("../../tests/fixtures/legacy-powerpoint/basic_test_ppt_file.ppt")[..],
                "application/msword",
                "download.ppt",
            ),
        ] {
            assert_eq!(
                filename(&response(
                    bytes.to_vec(),
                    "https://example.com/download?id=1",
                    mime
                )),
                Path::new(name)
            );
        }
    }

    #[test]
    fn mime_and_signature_select_document_parser_without_url_suffix_dependency() {
        assert_eq!(
            filename(&response(
                b"%PDF-test".to_vec(),
                "https://example.com/download?id=1",
                "application/octet-stream"
            )),
            Path::new("download.pdf")
        );
        assert_eq!(
            filename(&response(
                "# 中文 markdown".as_bytes().to_vec(),
                "https://example.com/path",
                "text/markdown"
            )),
            Path::new("download.md")
        );
        assert_eq!(
            filename(&response(
                b"anything".to_vec(),
                "https://example.com/path",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            )),
            Path::new("download.pptx")
        );
    }
}
