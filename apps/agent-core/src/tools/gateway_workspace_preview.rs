//! File-backed workspace previews share the regular filesystem authorization and decoder.
//! 文件工作区预览复用正式文件授权与解析器；展示引用不授予整盘读取权限。

use super::gateway::{GatewayToolContext, check_cancelled, failure};
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::{io::{Cursor, Read}, path::{Path, PathBuf}, process::Stdio, sync::OnceLock, time::Duration};

const MAX_PREVIEW_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DIRECT_DOCX_BYTES: usize = 64 * 1024 * 1024;
const MAX_DOCX_XML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OOXML_PART_XML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OOXML_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DOCX_MEDIA_BYTES: u64 = 32 * 1024 * 1024;
const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const PPTX_MIME: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

pub(crate) struct PreviewGrant {
    conversation: String,
    path: PathBuf,
    version: String,
}

pub(crate) struct WorkspaceContent {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub version: String,
}

fn extension(path: &Path) -> String {
    path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn format(path: &Path) -> (&'static str, &'static str) {
    match extension(path).as_str() {
        "md" | "markdown" | "mdown" => ("markdown", "text/markdown; charset=utf-8"),
        "mmd" | "mermaid" => ("mermaid", "text/plain; charset=utf-8"),
        "pdf" => ("pdf", "application/pdf"),
        "docx" => ("docx", DOCX_MIME),
        "pptx" => ("pptx", PPTX_MIME),
        "xlsx" => ("xlsx", XLSX_MIME),
        "png" => ("image", "image/png"),
        "jpg" | "jpeg" => ("image", "image/jpeg"),
        "gif" => ("image", "image/gif"),
        "webp" => ("image", "image/webp"),
        "bmp" => ("image", "image/bmp"),
        "svg" => ("image", "image/svg+xml"),
        "tif" | "tiff" => ("image", "image/tiff"),
        "html" | "htm" | "xhtml" => ("html", "text/html; charset=utf-8"),
        "zip" | "tar" | "gz" | "tgz" | "bz2" => ("archive", "text/plain; charset=utf-8"),
        _ => ("text", "text/plain; charset=utf-8"),
    }
}

fn is_office(path: &Path) -> bool {
    matches!(extension(path).as_str(), "doc" | "docx" | "ppt" | "pptx" | "odt" | "odp" | "rtf" | "xls" | "xlsx" | "ods")
}

fn complex_field(instruction: &str) -> bool {
    matches!(instruction.split_whitespace().next().unwrap_or("").to_ascii_uppercase().as_str(),
        "TOC" | "TOA" | "INDEX" | "REF" | "PAGEREF" | "NOTEREF" | "INCLUDETEXT" | "INCLUDEPICTURE"
        | "LINK" | "DDE" | "DDEAUTO" | "IF" | "MERGEFIELD" | "NEXT" | "NEXTIF" | "SKIPIF" | "SEQ"
        | "STYLEREF" | "DOCVARIABLE" | "AUTOTEXT" | "AUTOTEXTLIST" | "FORMTEXT" | "FORMCHECKBOX"
        | "FORMDROPDOWN" | "EQ" | "=")
}

fn docx_xml_fallback(xml: &[u8]) -> Result<Option<&'static str>, GatewayError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut field = String::new();
    let mut capture_field = false;
    loop {
        match reader.read_event().map_err(|_| failure("DOCUMENT_PARSE_FAILED", "DOCX 页面结构无法读取。", 422))? {
            quick_xml::events::Event::Start(event) | quick_xml::events::Event::Empty(event) => {
                match event.local_name().as_ref() {
                    b"object" | b"OLEObject" | b"anchor" | b"shape" | b"wsp" | b"altChunk" => return Ok(Some("embedded-or-floating-content")),
                    b"instrText" => { capture_field = true; field.clear(); }
                    b"fldSimple" => {
                        for attribute in event.attributes() {
                            let attribute = attribute.map_err(|_| failure("DOCUMENT_PARSE_FAILED", "DOCX 域结构无法读取。", 422))?;
                            if attribute.key.local_name().as_ref() == b"instr" {
                                let instruction = attribute.decode_and_unescape_value(reader.decoder())
                                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "DOCX 域内容无法读取。", 422))?;
                                if complex_field(&instruction) { return Ok(Some("complex-field")); }
                            }
                        }
                    }
                    _ => {}
                }
            }
            quick_xml::events::Event::Text(event) if capture_field => {
                let text = event.decode().map_err(|_| failure("DOCUMENT_PARSE_FAILED", "DOCX 域编码无法读取。", 422))?;
                field.push_str(&text);
                if field.len() > 4096 { return Ok(Some("complex-field")); }
            }
            quick_xml::events::Event::End(event) if event.local_name().as_ref() == b"instrText" => {
                capture_field = false;
                if complex_field(&field) { return Ok(Some("complex-field")); }
            }
            quick_xml::events::Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED", "DOCX 预览不接受外部文档类型声明。", 422)),
            quick_xml::events::Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn presentation_xml_fallback(xml: &[u8]) -> Result<Option<&'static str>, GatewayError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    loop {
        match reader.read_event().map_err(|_| failure("DOCUMENT_PARSE_FAILED", "PPTX 页面结构无法读取。", 422))? {
            quick_xml::events::Event::Start(event) | quick_xml::events::Event::Empty(event)
                if matches!(event.local_name().as_ref(), b"oleObj" | b"videoFile" | b"audioFile" | b"media" | b"contentPart") =>
                    return Ok(Some("embedded-office-content")),
            quick_xml::events::Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED", "Office 预览不接受外部文档类型声明。", 422)),
            quick_xml::events::Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn spreadsheet_xml_fallback(xml: &[u8]) -> Result<Option<&'static str>, GatewayError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    loop {
        match reader.read_event().map_err(|_| failure("DOCUMENT_PARSE_FAILED", "XLSX 表格结构无法读取。", 422))? {
            quick_xml::events::Event::Start(event) | quick_xml::events::Event::Empty(event)
                if matches!(event.local_name().as_ref(), b"oleObject" | b"control") =>
                    return Ok(Some("embedded-office-content")),
            quick_xml::events::Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED", "Office 预览不接受外部文档类型声明。", 422)),
            quick_xml::events::Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

/// Check bounded package metadata/XML without unpacking media; ordinary Office layouts stay direct.
/// 有界检查包目录和 XML，不解压媒体或创建成员路径；普通 Office 布局直接展示。
fn office_package_fallback(bytes: &[u8], kind: &str) -> Result<Option<&'static str>, GatewayError> {
    let (directory, main, unsupported): (&str, &str, &[&str]) = match kind {
        "docx" => ("word/", "word/document.xml", &["word/embeddings/", "word/charts/", "word/diagrams/", "word/activeX/"]),
        "pptx" => ("ppt/", "ppt/presentation.xml", &["ppt/embeddings/", "ppt/charts/", "ppt/diagrams/", "ppt/activeX/"]),
        "xlsx" => ("xl/", "xl/workbook.xml", &["xl/embeddings/", "xl/charts/", "xl/pivotTables/", "xl/pivotCache/", "xl/activeX/"]),
        _ => return Ok(Some("unsupported-office-format")),
    };
    if kind == "docx" && bytes.len() > MAX_DIRECT_DOCX_BYTES { return Ok(Some("direct-preview-size")); }
    let xml_budget = if kind == "docx" { MAX_DOCX_XML_BYTES } else { MAX_OOXML_EXPANDED_BYTES };
    let xml_part_limit = if kind == "docx" { MAX_DOCX_XML_BYTES } else { MAX_OOXML_PART_XML_BYTES };
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Office 文件包无法读取。", 422))?;
    let entry_limit = if kind == "docx" { 4096 } else { 20000 };
    if archive.len() > entry_limit { return Ok(Some("large-package")); }
    let mut xml_parts = Vec::new();
    let mut expanded_xml = 0u64;
    let mut expanded_package = 0u64;
    let mut fallback = None;
    let mut has_document = false;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Office 包目录无法读取。", 422))?;
        if entry.enclosed_name().is_none() || entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(failure("DOCUMENT_ARCHIVE_LIMIT", "Office 包含不支持的成员路径或链接。", 422));
        }
        let name = entry.name();
        expanded_package = expanded_package.saturating_add(entry.size());
        if expanded_package > MAX_OOXML_EXPANDED_BYTES {
            fallback = Some("expanded-package-size");
        }
        if kind == "docx" && name.starts_with("word/media/") && entry.size() > MAX_DOCX_MEDIA_BYTES {
            fallback = Some("large-media");
        }
        has_document |= name == main;
        if !entry.is_dir() && (unsupported.iter().any(|prefix| name.starts_with(prefix))
            || name.starts_with(directory) && name.ends_with("vbaProject.bin")) {
            fallback = Some("embedded-office-content");
        }
        if name.starts_with(directory) && name.ends_with(".xml") && !entry.is_dir() {
            expanded_xml = expanded_xml.saturating_add(entry.size());
            if entry.size() > xml_part_limit || expanded_xml > xml_budget { fallback = Some("large-xml"); }
            // Check total capacity here; slide/sheet renderers validate parts on demand before creating DOM.
            // 此处检查总容量；幻灯片和表格按需核验部件后生成 DOM，不预解压后续页。
            if kind == "docx" || name == main { xml_parts.push(index); }
        }
    }
    if !has_document { return Err(failure("DOCUMENT_PARSE_FAILED", "Office 文件缺少主文档部件。", 422)); }
    if fallback.is_some() { return Ok(fallback); }
    let mut read_xml = 0u64;
    for index in xml_parts {
        let mut entry = archive.by_index(index)
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Office XML 无法读取。", 422))?;
        let mut xml = Vec::new();
        entry.by_ref().take(xml_part_limit.min(xml_budget.saturating_sub(read_xml)) + 1).read_to_end(&mut xml)
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Office XML 解压未完成。", 422))?;
        read_xml = read_xml.saturating_add(xml.len() as u64);
        if xml.len() as u64 > xml_part_limit || read_xml > xml_budget { return Ok(Some("large-xml")); }
        let reason = match kind {
            "docx" => docx_xml_fallback(&xml)?,
            "pptx" => presentation_xml_fallback(&xml)?,
            _ => spreadsheet_xml_fallback(&xml)?,
        };
        if reason.is_some() { return Ok(reason); }
    }
    Ok(None)
}

fn docx_fallback(bytes: &[u8]) -> Result<Option<&'static str>, GatewayError> {
    office_package_fallback(bytes, "docx")
}

/// The caller owns file or attachment authorization; this decision never opens the supplied path.
/// 调用方负责文件或附件授权；此判断仅读取传入字节，路径仅用于识别格式，不读取其指向的文件。
pub(crate) fn docx_direct_preview(path: &Path, bytes: &[u8]) -> Result<bool, GatewayError> {
    if extension(path) != "docx" { return Ok(false); }
    Ok(docx_fallback(bytes)?.is_none())
}

/// File and immutable attachment callers keep their existing authorization before this byte-only check.
/// 文件与不可变附件调用方先完成各自授权；此检查只使用已读取字节，不扩展路径访问权限。
pub(crate) fn office_direct_preview(path: &Path, bytes: &[u8]) -> Result<bool, GatewayError> {
    match extension(path).as_str() {
        "docx" => docx_direct_preview(path, bytes),
        kind @ ("pptx" | "xlsx") => Ok(office_package_fallback(bytes, kind)?.is_none()),
        _ => Ok(false),
    }
}

pub(crate) fn web_target(arguments: &Value) -> Result<Value, GatewayError> {
    if arguments["path"].as_str().is_some_and(|path| !path.is_empty()) {
        return Err(failure("INVALID_TOOL_ARGUMENTS", "文件路径与网页地址只能选择一个。", 400));
    }
    let url = url::Url::parse(arguments["url"].as_str().unwrap_or(""))
        .map_err(|_| failure("INVALID_TOOL_ARGUMENTS", "请输入完整网页地址。", 400))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some()
    {
        return Err(failure("WEB_URL_BLOCKED", "网页预览只接受不含凭据的 HTTP 或 HTTPS 地址。", 400));
    }
    let title = arguments["title"].as_str().filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| url.host_str().unwrap_or("Web"));
    Ok(json!({"kind":"browser","title":title,"path":url.as_str(),"url":url.as_str(),"version":"","mimeType":"text/html"}))
}

fn checked_bytes(path: &Path, binding: &Value) -> Result<Vec<u8>, GatewayError> {
    super::service_filesystem::revalidate(binding)
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    let bytes = super::local_files::read_checked(path, MAX_PREVIEW_BYTES, true, "TOOL_FILE_CONFLICT")
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    super::service_filesystem::revalidate(binding)
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    Ok(bytes)
}

fn content_url(context: &GatewayToolContext, path: &Path, version: &str, converted: bool) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("conversationId", &context.conversation_id)
        .append_pair("path", &super::local_files::path_text(path))
        .append_pair("version", version);
    if let Some(mount_id) = context.config["activeMountId"].as_str() { query.append_pair("mountId", mount_id); }
    if converted { query.append_pair("representation", "pdf"); }
    format!("/agent/workspace/content?{}", query.finish())
}

fn page_text(result: &mut Value, text: &str, arguments: &Value) -> Result<(), GatewayError> {
    let page = super::service_filesystem::text_page(text,
        arguments["offset"].as_u64().unwrap_or(0) as usize,
        arguments["maxChars"].as_u64().unwrap_or(160000).clamp(1, 1024 * 1024) as usize)
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    result["text"] = page["content"].clone();
    for name in ["offset", "nextOffset", "hasMore", "totalCharacters"] {
        result[name] = page[name].clone();
    }
    result["offsetUnit"] = json!("utf16-code-units");
    Ok(())
}

fn archive_listing(path: &Path, bytes: &[u8]) -> Result<String, GatewayError> {
    let mut entries = Vec::new();
    if extension(path) == "zip" {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "ZIP 目录无法读取。", 422))?;
        for index in 0..archive.len().min(20000) {
            let entry = archive.by_index(index)
                .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "ZIP 目录项无法读取。", 422))?;
            entries.push(format!("{}\t{} bytes", entry.name(), entry.size()));
        }
        if archive.len() > entries.len() { entries.push("… 目录项超过本次显示范围。".into()); }
    } else {
        let reader: Box<dyn Read + '_> = match extension(path).as_str() {
            "gz" | "tgz" => Box::new(flate2::read::GzDecoder::new(bytes)),
            "bz2" => Box::new(bzip2::read::BzDecoder::new(bytes)),
            _ => Box::new(Cursor::new(bytes)),
        };
        let mut archive = tar::Archive::new(reader);
        for entry in archive.entries().map_err(|_| failure("DOCUMENT_PARSE_FAILED", "压缩包目录无法读取。", 422))?.take(20000) {
            let entry = entry.map_err(|_| failure("DOCUMENT_PARSE_FAILED", "压缩包目录项无法读取。", 422))?;
            let name = entry.path().map_err(|_| failure("DOCUMENT_PARSE_FAILED", "压缩包名称无法读取。", 422))?;
            entries.push(format!("{}\t{} bytes", name.display(), entry.size()));
        }
    }
    Ok(entries.join("\n"))
}

fn office_converter(context: &GatewayToolContext) -> Option<PathBuf> {
    let root = context.config["officialToolsRoot"].as_str().and_then(|path| Path::new(path).parent());
    let mut paths = root.map(|root| vec![
        root.join("runtime/office/program/soffice.exe"),
        root.join("runtime/libreoffice/program/soffice.exe"),
        root.join("runtime/libreoffice/program/soffice"),
    ]).unwrap_or_default();
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable) {
            paths.push(PathBuf::from(root).join("LibreOffice/program/soffice.exe"));
        }
    }
    paths.into_iter().find(|path| path.is_file())
}

fn converted_path(context: &GatewayToolContext, version: &str) -> PathBuf {
    context.data_home.join("Cache/Previews/office-pdf-v1").join(format!("{version}.pdf"))
}

/// Conversion has a real process owner, isolated profile and bounded concurrent work.
/// 转换使用自有进程树、独立用户配置与有界并发，取消时不会遗留 Office 后台进程。
pub(crate) async fn office_pdf(path: &Path, version: &str, source_bytes: &[u8], context: &GatewayToolContext) -> Result<Option<PathBuf>, GatewayError> {
    let cached = converted_path(context, version);
    if cached.is_file() { return Ok(Some(cached)); }
    let Some(converter) = office_converter(context) else { return Ok(None); };
    static CONVERSIONS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    let _permit = CONVERSIONS.get_or_init(|| {
        let parallelism = std::thread::available_parallelism().map(usize::from).unwrap_or(8);
        tokio::sync::Semaphore::new((parallelism / 4).clamp(2, 6))
    }).acquire().await
        .map_err(|_| failure("PREVIEW_CONVERTER_UNAVAILABLE", "文档预览转换器不可用。", 503))?;
    check_cancelled(context)?;
    if cached.is_file() { return Ok(Some(cached)); }
    let temporary = cached.parent().unwrap().join(uuid::Uuid::new_v4().to_string());
    let input_directory = temporary.join("input");
    tokio::fs::create_dir_all(&input_directory).await?;
    let input = input_directory.join(path.file_name().unwrap_or_default());
    // Convert the already verified bytes, so an external edit cannot change this cached version.
    // 转换已核验的字节副本，避免外部修改让缓存版本与实际转换内容错配。
    tokio::fs::write(&input, source_bytes).await?;
    let profile = url::Url::from_directory_path(temporary.join("profile"))
        .map_err(|_| failure("PREVIEW_CONVERTER_FAILED", "转换缓存路径不可用。", 500))?;
    let mut command = tokio::process::Command::new(converter);
    command.arg(format!("-env:UserInstallation={profile}"))
        .args(["--headless", "--nologo", "--nodefault", "--norestore", "--convert-to", "pdf", "--outdir"])
        .arg(&temporary).arg(input).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    super::gateway_process::hide(&mut command);
    let mut child = command.spawn().map_err(|_| failure("PREVIEW_CONVERTER_FAILED", "本地文档转换程序无法启动。", 503))?;
    let owner = match super::gateway_process::OwnedProcessJob::attach(&child) {
        Ok(owner) => owner,
        Err(error) => { super::gateway_process::terminate(&mut child).await; return Err(error); }
    };
    let mut cancellation = context.cancellation.clone();
    let status = tokio::select! {
        status = child.wait() => status.map_err(GatewayError::from),
        _ = super::gateway::wait_for_cancellation(&mut cancellation) => Err(failure("TOOL_CANCELLED", "文档预览已取消。", 409)),
        _ = tokio::time::sleep(Duration::from_secs(120)) => Err(failure("PREVIEW_CONVERTER_TIMEOUT", "本地文档预览转换超时，可继续读取提取正文。", 504)),
    };
    drop(owner);
    let converted = temporary.join(path.file_stem().unwrap_or_default()).with_extension("pdf");
    let result = match status {
        Ok(status) if status.success() && converted.is_file() => {
            tokio::fs::rename(&converted, &cached).await.map(|_| Some(cached)).map_err(GatewayError::from)
        }
        Ok(_) => Err(failure("PREVIEW_CONVERTER_FAILED", "本地转换未产出 PDF，可继续读取提取正文。", 422)),
        Err(error) => Err(error),
    };
    let _ = tokio::fs::remove_dir_all(&temporary).await;
    result
}

pub(super) async fn open(arguments: Value, path: PathBuf, binding: Value, context: &GatewayToolContext) -> Result<Value, GatewayError> {
    let source = path.clone();
    let read_binding = binding.clone();
    let source_format = extension(&path);
    let inspect_office = matches!(source_format.as_str(), "docx" | "pptx" | "xlsx")
        && arguments["_metadataOnly"] != true && arguments["representation"] != "pdf";
    let (bytes, version, direct_fallback) = tokio::task::spawn_blocking(move || {
        let bytes = checked_bytes(&source, &read_binding)?;
        let version = super::local_files::sha256(&bytes);
        let fallback = if inspect_office { office_package_fallback(&bytes, &source_format)? } else { None };
        Ok::<_, GatewayError>((bytes, version, fallback))
    }).await
        .map_err(|_| failure("PREVIEW_WORKER_FAILED", "文件预览读取未完成。", 500))??;
    check_cancelled(context)?;
    if arguments["version"].as_str().is_some_and(|expected| expected != version) {
        return Err(failure("TOOL_FILE_CONFLICT", "文件已更新，请重新打开当前版本。", 409));
    }
    let (kind, mime_type) = format(&path);
    let title = arguments["title"].as_str().filter(|title| !title.trim().is_empty())
        .map(str::to_owned).unwrap_or_else(|| path.file_name().unwrap_or_default().to_string_lossy().into_owned());
    let mut result = json!({"kind":kind,"title":title,"path":super::local_files::path_text(&path),"version":version,"mimeType":mime_type,
        "mountId":context.config["activeMountId"],"mountName":context.config["activeMount"]["name"]});
    if arguments["_metadataOnly"] == true {
        // Display actions return a file reference; only the active UI preview loads its body.
        // 展示动作只返回文件引用，正文由当前活动预览读取，不重复塞入模型上下文。
        result["previewAvailable"] = json!(true);
        result["previewStatus"] = json!("registered");
        result["rendered"] = json!(false);
        result["sourceFormat"] = json!(extension(&path));
        result["notice"] = json!("文件已登记到工作区，尚未加载或完成渲染；最终显示格式由活动预览决定。");
        return Ok(result);
    }
    if matches!(kind, "pdf" | "image" | "html") || matches!(kind, "docx" | "pptx" | "xlsx")
        && direct_fallback.is_none() && arguments["representation"] != "pdf" {
        result["contentUrl"] = json!(content_url(context, &path, &version, false));
    } else if is_office(&path) {
        if let Some(reason) = direct_fallback { result["directPreviewFallback"] = json!(reason); }
        match office_pdf(&path, &version, &bytes, context).await {
            Ok(Some(_)) => {
                result["kind"] = json!("pdf");
                result["mimeType"] = json!("application/pdf");
                result["contentUrl"] = json!(content_url(context, &path, &version, true));
                return Ok(result);
            }
            Err(error) if error.code == "TOOL_CANCELLED" => return Err(error),
            other => {
                result["notice"] = json!(match other {
                    Err(error) => error.message,
                    _ => "本机未提供 Office 页面转换器，当前显示提取正文；原文件保持原格式。".to_owned(),
                });
            }
        }
        result["kind"] = json!("text");
        result["mimeType"] = json!("text/plain; charset=utf-8");
        let mut read_arguments = arguments.clone();
        read_arguments["maxChars"] = json!(arguments["maxChars"].as_u64().unwrap_or(64000).min(64000));
        read_arguments["expectedSha256"] = json!(version);
        let extracted = super::gateway_document_read::read(read_arguments, path, binding, context).await?;
        result["text"] = extracted["text"].clone();
        for key in ["offset", "nextOffset", "hasMore", "totalCharacters", "offsetUnit"] {
            result[key] = extracted[key].clone();
        }
    } else if kind == "archive" {
        let text = tokio::task::spawn_blocking(move || archive_listing(&path, &bytes)).await
            .map_err(|_| failure("PREVIEW_WORKER_FAILED", "压缩包目录读取未完成。", 500))??;
        result = tokio::task::spawn_blocking(move || {
            page_text(&mut result, &text, &arguments)?;
            Ok::<_, GatewayError>(result)
        }).await.map_err(|_| failure("PREVIEW_WORKER_FAILED", "目录预览分页未完成。", 500))??;
    } else {
        result = tokio::task::spawn_blocking(move || {
            let text = if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
                crate::tools::service_source_reading::decode(bytes, "utf-16le")
            } else {
                crate::tools::service_source_reading::decode(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes), "utf-8")
            }.map_err(|_| failure("PREVIEW_FORMAT_UNSUPPORTED", "此文件不是可直接预览的文本或已支持文档。", 422))?;
            page_text(&mut result, &text, &arguments)?;
            Ok::<_, GatewayError>(result)
        }).await.map_err(|_| failure("PREVIEW_WORKER_FAILED", "文本预览解码未完成。", 500))??;
    }
    Ok(result)
}

/// User file-picker access is exact-file and conversation-bound, never inherited by model calls.
/// 文件选择器授权仅绑定本会话、此文件和版本；模型工具不会继承这一临时读取许可。
pub(crate) async fn preview(arguments: Value, context: &GatewayToolContext) -> Result<Value, GatewayError> {
    let selected = arguments["userSelected"] == true;
    let mut selected_context = context.clone();
    selected_context.approved = selected;
    let mut input = arguments.clone();
    input.as_object_mut().ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "文件预览参数无效。", 400))?.remove("userSelected");
    let mut result = if input["url"].is_string() { web_target(&input)? } else {
        super::gateway::filesystem("artifact.open", input, &selected_context).await?
    };
    if result["requiresApproval"] == true {
        return Err(failure("WORKSPACE_PREVIEW_FORBIDDEN", "此文件不在当前工作范围，请通过文件选择器打开。", 403));
    }
    if selected && result["contentUrl"].is_string() {
        let token = uuid::Uuid::new_v4().to_string();
        let grant = PreviewGrant { conversation:context.conversation_id.clone(),
            path:PathBuf::from(result["path"].as_str().unwrap_or("")), version:result["version"].as_str().unwrap_or("").into() };
        let mut grants = context.runtime.preview_grants.lock()
            .map_err(|_| failure("PREVIEW_STATE_FAILED", "预览许可状态不可用。", 500))?;
        if grants.len() >= 4096 {
            if let Some(oldest) = grants.keys().next().cloned() { grants.remove(&oldest); }
        }
        grants.insert(token.clone(), grant);
        result["contentUrl"] = json!(format!("{}&grant={token}", result["contentUrl"].as_str().unwrap()));
    }
    Ok(result)
}

pub(crate) async fn content(arguments: &Value, context: &GatewayToolContext) -> Result<WorkspaceContent, GatewayError> {
    let requested = arguments["path"].as_str().ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "缺少预览文件路径。", 400))?;
    let expected = arguments["version"].as_str().filter(|value| !value.is_empty())
        .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "缺少预览文件版本。", 400))?;
    let (path, _) = super::gateway::canonical_authorized(context, requested, false)?;
    let approved = arguments["grant"].as_str().is_some_and(|token| {
        context.runtime.preview_grants.lock().ok().and_then(|grants| grants.get(token).map(|grant|
            grant.conversation == context.conversation_id && grant.path == path && grant.version == expected)).unwrap_or(false)
    });
    let mut authorized = context.clone();
    authorized.approved = approved;
    let receipt = super::gateway::filesystem("filesystem.stat", json!({"path":requested}), &authorized).await?;
    if receipt["requiresApproval"] == true {
        return Err(failure("WORKSPACE_PREVIEW_FORBIDDEN", "文件不在本次预览许可内。", 403));
    }
    let binding = json!({"requestedPath":path,"path":path,"allowMissing":false,"allowHardLinks":true});
    let source = path.clone();
    let (bytes, version) = tokio::task::spawn_blocking(move || {
        let bytes = checked_bytes(&source, &binding)?;
        let version = super::local_files::sha256(&bytes);
        Ok::<_, GatewayError>((bytes, version))
    }).await
        .map_err(|_| failure("PREVIEW_WORKER_FAILED", "预览内容读取未完成。", 500))??;
    if version != expected { return Err(failure("TOOL_FILE_CONFLICT", "文件已变化，请刷新预览。", 409)); }
    let (kind, mime_type) = format(&path);
    if arguments["representation"] == "pdf" && is_office(&path) {
        let cached = converted_path(context, &version);
        let bytes = tokio::fs::read(cached).await
            .map_err(|_| failure("PREVIEW_CACHE_MISSING", "文档页面缓存已清理，请重新打开文件。", 404))?;
        return Ok(WorkspaceContent { bytes, mime_type:"application/pdf", version });
    }
    if kind == "docx" && bytes.len() > MAX_DIRECT_DOCX_BYTES {
        return Err(failure("PREVIEW_DIRECT_LIMIT", "此文档超过直接预览大小，请使用 PDF 页面预览。", 413));
    }
    if !matches!(kind, "pdf" | "image" | "html" | "docx" | "pptx" | "xlsx") {
        return Err(failure("PREVIEW_FORMAT_UNSUPPORTED", "此文件通过文本预览读取。", 422));
    }
    Ok(WorkspaceContent { bytes, mime_type, version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn docx_package(body: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        office_package("word/document.xml", body, extra)
    }

    fn office_package(main: &str, body: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file(main, options).unwrap();
        archive.write_all(body.as_bytes()).unwrap();
        for (name, bytes) in extra {
            archive.start_file(*name, options).unwrap(); archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    #[test]
    fn office_feature_detection_keeps_presentation_layouts_and_spreadsheet_formulas_direct() {
        let presentation = "<p:presentation xmlns:p='urn:presentation'><p:sldMasterIdLst/><p:sldIdLst/></p:presentation>";
        let slide = b"<p:sld xmlns:p='urn:presentation' xmlns:a='urn:drawing'><p:cSld><p:spTree><p:sp/><p:cxnSp/><p:pic/><p:graphicFrame><a:graphic><a:graphicData><a:tbl/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>";
        let ordinary = office_package("ppt/presentation.xml", presentation, &[
            ("ppt/slides/slide1.xml", slide), ("ppt/slideMasters/slideMaster1.xml", b"<master/>"),
            ("ppt/slideLayouts/slideLayout1.xml", b"<layout/>"), ("ppt/theme/theme1.xml", b"<theme/>"),
            ("ppt/media/image1.png", b"fixture"),
        ]);
        assert!(office_direct_preview(Path::new("slides.pptx"), &ordinary).unwrap());
        assert!(!office_direct_preview(Path::new("slides.ppt"), &ordinary).unwrap());
        let deferred_slide = office_package("ppt/presentation.xml", presentation,
            &[("ppt/slides/slide99.xml", b"<later-slide-not-yet-parsed")]);
        assert!(office_direct_preview(Path::new("slides.pptx"), &deferred_slide).unwrap());
        for part in ["ppt/embeddings/object1.bin", "ppt/charts/chart1.xml", "ppt/diagrams/data1.xml", "ppt/vbaProject.bin"] {
            assert_eq!(office_package_fallback(&office_package("ppt/presentation.xml", presentation, &[(part, b"fixture")]), "pptx").unwrap(), Some("embedded-office-content"));
        }
        let workbook = "<workbook><sheets><sheet name='Data'/><sheet name='Second'/></sheets></workbook>";
        let sheet = b"<worksheet><sheetData><row><c r='A1'><f>SUM(A2:A3)</f><v>42</v></c></row></sheetData><mergeCells><mergeCell ref='A2:B2'/></mergeCells></worksheet>";
        let ordinary = office_package("xl/workbook.xml", workbook, &[
            ("xl/worksheets/sheet1.xml", sheet), ("xl/worksheets/sheet2.xml", b"<worksheet/>"),
            ("xl/styles.xml", b"<styleSheet/>"), ("xl/sharedStrings.xml", b"<sst/>"), ("xl/tables/table1.xml", b"<table/>"),
        ]);
        assert!(office_direct_preview(Path::new("data.xlsx"), &ordinary).unwrap());
        let deferred_sheet = office_package("xl/workbook.xml", workbook,
            &[("xl/worksheets/sheet99.xml", b"<later-sheet-not-yet-parsed")]);
        assert!(office_direct_preview(Path::new("data.xlsx"), &deferred_sheet).unwrap());
        for part in ["xl/embeddings/object1.bin", "xl/charts/chart1.xml", "xl/pivotTables/pivotTable1.xml", "xl/pivotCache/pivotCacheDefinition1.xml"] {
            assert_eq!(office_package_fallback(&office_package("xl/workbook.xml", workbook, &[(part, b"fixture")]), "xlsx").unwrap(), Some("embedded-office-content"));
        }
        assert_eq!(office_package_fallback(&office_package("xl/workbook.xml", workbook, &[("../unsafe.xml", b"fixture")]), "xlsx").unwrap_err().code, "DOCUMENT_ARCHIVE_LIMIT");
    }

    #[tokio::test]
    async fn office_previews_keep_pptx_xlsx_source_bytes_versions_and_pdf_fallback() {
        let (context, _cancel) = super::super::gateway::test_context();
        for (kind, arguments, mime_type) in [
            ("pptx", json!({"slides":[{"title":"概览","body":"原始幻灯片"}]}), PPTX_MIME),
            ("xlsx", json!({"sheets":[{"name":"Data","rows":[["名称","数量"],["First",42]]}]}), XLSX_MIME),
        ] {
            let bytes = super::super::document_packages::generate(&arguments, kind).unwrap();
            let name = format!("generated.{kind}");
            std::fs::write(context.workdir.join(&name), &bytes).unwrap();
            let registered = super::super::gateway::execute("artifact.open", json!({"path":name}), &context).await.unwrap();
            assert_eq!(registered["kind"], kind); assert_eq!(registered["mimeType"], mime_type);
            assert_eq!(registered["rendered"], false); assert_eq!(registered["previewStatus"], "registered");
            let direct = preview(json!({"path":name}), &context).await.unwrap();
            assert_eq!(direct["kind"], kind); assert_eq!(direct["mimeType"], mime_type);
            assert!(!direct["contentUrl"].as_str().unwrap().contains("representation="));
            let raw = content(&json!({"path":direct["path"],"version":direct["version"]}), &context).await.unwrap();
            assert_eq!(raw.bytes, bytes); assert_eq!(raw.mime_type, mime_type);
            let pdf = b"%PDF-1.7\nfixture cached page";
            let cached = converted_path(&context, direct["version"].as_str().unwrap());
            std::fs::create_dir_all(cached.parent().unwrap()).unwrap(); std::fs::write(cached, pdf).unwrap();
            let fallback = preview(json!({"path":name,"representation":"pdf"}), &context).await.unwrap();
            assert_eq!(fallback["kind"], "pdf");
            assert!(fallback["contentUrl"].as_str().unwrap().contains("representation=pdf"));
            let rendered = content(&json!({"path":direct["path"],"version":direct["version"],"representation":"pdf"}), &context).await.unwrap();
            assert_eq!(rendered.bytes, pdf); assert_eq!(rendered.mime_type, "application/pdf");
            let external = context.workdir.parent().unwrap().join(format!("outside.{kind}"));
            std::fs::write(&external, &bytes).unwrap();
            assert_eq!(preview(json!({"path":external}), &context).await.unwrap_err().code, "WORKSPACE_PREVIEW_FORBIDDEN");
            let selected = preview(json!({"path":external,"userSelected":true}), &context).await.unwrap();
            let url = url::Url::parse(&format!("http://localhost{}", selected["contentUrl"].as_str().unwrap())).unwrap();
            let mut request = json!({});
            for (key, value) in url.query_pairs() { request[key.as_ref()] = json!(value); }
            assert_eq!(content(&request, &context).await.unwrap().bytes, bytes);
            let mut other = context.clone(); other.conversation_id = uuid::Uuid::new_v4().to_string();
            assert_eq!(content(&request, &other).await.err().unwrap().code, "WORKSPACE_PREVIEW_FORBIDDEN");
            std::fs::write(&external, b"changed").unwrap();
            assert_eq!(content(&request, &context).await.err().unwrap().code, "TOOL_FILE_CONFLICT");
            assert_eq!(preview(json!({"path":name,"version":"stale"}), &context).await.unwrap_err().code, "TOOL_FILE_CONFLICT");
        }
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[test]
    fn docx_feature_detection_keeps_ordinary_layout_and_falls_back_for_complex_parts() {
        let normal = "<w:document xmlns:w='urn:word' xmlns:wp='urn:drawing'><w:body><w:p><w:pPr><w:pStyle w:val='Heading1'/></w:pPr><w:r><w:t>标题</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl><wp:inline/></w:body></w:document>";
        let header = b"<w:hdr xmlns:w='urn:word'><w:p><w:fldSimple w:instr='PAGE'/><w:r><w:instrText> NUMPAGES </w:instrText></w:r></w:p></w:hdr>";
        let ordinary = docx_package(normal, &[("word/header1.xml", header), ("word/media/image1.png", b"fixture")]);
        assert_eq!(docx_fallback(&ordinary).unwrap(), None);
        assert!(docx_direct_preview(Path::new("report.docx"), &ordinary).unwrap());
        assert!(!docx_direct_preview(Path::new("legacy.doc"), &ordinary).unwrap());
        for part in ["word/embeddings/object1.bin", "word/charts/chart1.xml", "word/diagrams/data1.xml"] {
            assert_eq!(docx_fallback(&docx_package(normal, &[(part, b"fixture")])).unwrap(), Some("embedded-office-content"));
        }
        assert_eq!(docx_fallback(&docx_package("<w:document xmlns:w='urn:word' xmlns:wp='urn:drawing'><wp:anchor/></w:document>", &[])).unwrap(), Some("embedded-or-floating-content"));
        assert_eq!(docx_fallback(&docx_package("<w:document xmlns:w='urn:word'><w:instrText> TOC \\o &quot;1-3&quot; </w:instrText></w:document>", &[])).unwrap(), Some("complex-field"));
        assert_eq!(docx_fallback(&docx_package(normal, &[("../unsafe.xml", b"fixture")])).unwrap_err().code, "DOCUMENT_ARCHIVE_LIMIT");
    }

    #[tokio::test]
    async fn docx_previews_preserve_source_bytes_grants_and_explicit_pdf_fallback() {
        let (context, _cancel) = super::super::gateway::test_context();
        let bytes = docx_package("<w:document xmlns:w='urn:word'><w:body><w:p><w:r><w:t>正文</w:t></w:r></w:p></w:body></w:document>", &[]);
        std::fs::write(context.workdir.join("report.docx"), &bytes).unwrap();
        let registered = super::super::gateway::execute("artifact.open", json!({"path":"report.docx"}), &context).await.unwrap();
        assert_eq!(registered["kind"], "docx"); assert_eq!(registered["previewStatus"], "registered");
        assert_eq!(registered["rendered"], false); assert!(registered["contentUrl"].is_null());
        let direct = preview(json!({"path":"report.docx"}), &context).await.unwrap();
        assert_eq!(direct["kind"], "docx"); assert_eq!(direct["mimeType"], DOCX_MIME);
        assert!(!direct["contentUrl"].as_str().unwrap().contains("representation="));
        let raw = content(&json!({"path":direct["path"],"version":direct["version"]}), &context).await.unwrap();
        assert_eq!(raw.bytes, bytes); assert_eq!(raw.mime_type, DOCX_MIME);
        let pdf = b"%PDF-1.7\nfixture cached page";
        let cached = converted_path(&context, direct["version"].as_str().unwrap());
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap(); std::fs::write(cached, pdf).unwrap();
        let fallback = preview(json!({"path":"report.docx","representation":"pdf"}), &context).await.unwrap();
        assert_eq!(fallback["kind"], "pdf");
        assert!(fallback["contentUrl"].as_str().unwrap().contains("representation=pdf"));
        let rendered = content(&json!({"path":direct["path"],"version":direct["version"],"representation":"pdf"}), &context).await.unwrap();
        assert_eq!(rendered.bytes, pdf); assert_eq!(rendered.mime_type, "application/pdf");
        let external = context.workdir.parent().unwrap().join("outside.docx");
        std::fs::write(&external, &bytes).unwrap();
        assert_eq!(preview(json!({"path":external}), &context).await.unwrap_err().code, "WORKSPACE_PREVIEW_FORBIDDEN");
        let selected = preview(json!({"path":external,"userSelected":true}), &context).await.unwrap();
        let url = url::Url::parse(&format!("http://localhost{}", selected["contentUrl"].as_str().unwrap())).unwrap();
        let mut arguments = json!({});
        for (key, value) in url.query_pairs() { arguments[key.as_ref()] = json!(value); }
        assert_eq!(content(&arguments, &context).await.unwrap().bytes, bytes);
        let mut other = context.clone(); other.conversation_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(content(&arguments, &other).await.err().unwrap().code, "WORKSPACE_PREVIEW_FORBIDDEN");
        std::fs::write(&external, b"changed").unwrap();
        assert_eq!(content(&arguments, &context).await.err().unwrap().code, "TOOL_FILE_CONFLICT");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn workspace_previews_keep_file_versions_and_explicit_picker_scope() {
        let (context, _cancel) = super::super::gateway::test_context();
        std::fs::write(context.workdir.join("report.md"), "# Report\n\n正文 😀").unwrap();
        let opened = super::super::gateway::execute("artifact.open", json!({"path":"report.md","title":"报告","maxChars":10}), &context).await.unwrap();
        assert_eq!(opened["kind"], "markdown");
        assert_eq!(opened["title"], "报告");
        assert!(opened["text"].is_null());
        assert_eq!(opened["version"].as_str().unwrap().len(), 64);
        let page = preview(json!({"path":"report.md","maxChars":10}), &context).await.unwrap();
        assert_eq!(page["hasMore"], true);
        let html = "<!doctype html><style>h1{color:green}</style><h1>样式预览</h1>";
        std::fs::write(context.workdir.join("page.xhtml"), html).unwrap();
        let rendered = preview(json!({"path":"page.xhtml"}), &context).await.unwrap();
        assert_eq!(rendered["kind"], "html");
        let markup = content(&json!({"path":rendered["path"],"version":rendered["version"]}), &context).await.unwrap();
        assert_eq!(markup.mime_type, "text/html; charset=utf-8");
        assert_eq!(markup.bytes, html.as_bytes());
        let external = context.workdir.parent().unwrap().join("outside.svg");
        std::fs::write(&external, "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
        let denied = preview(json!({"path":external}), &context).await.unwrap_err();
        assert_eq!(denied.code, "WORKSPACE_PREVIEW_FORBIDDEN");
        let selected = preview(json!({"path":external,"userSelected":true}), &context).await.unwrap();
        let url = url::Url::parse(&format!("http://localhost{}", selected["contentUrl"].as_str().unwrap())).unwrap();
        let mut arguments = json!({});
        for (key, value) in url.query_pairs() { arguments[key.as_ref()] = json!(value); }
        assert_eq!(content(&arguments, &context).await.unwrap().mime_type, "image/svg+xml");
        let mut other = context.clone();
        other.conversation_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(content(&arguments, &other).await.err().unwrap().code, "WORKSPACE_PREVIEW_FORBIDDEN");
        std::fs::write(&external, "<svg>changed</svg>").unwrap();
        assert_eq!(content(&arguments, &context).await.err().unwrap().code, "TOOL_FILE_CONFLICT");
        let protected = context.data_home.join("private.txt");
        std::fs::write(&protected, "private").unwrap();
        assert_eq!(preview(json!({"path":protected,"userSelected":true}), &context).await.unwrap_err().code, "PROTECTED_APP_DATA");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[test]
    fn browser_targets_only_accept_web_addresses() {
        assert_eq!(web_target(&json!({"url":"https://example.com","title":"官网"})).unwrap()["kind"], "browser");
        assert!(web_target(&json!({"url":"file:///C:/private.txt"})).is_err());
        assert!(web_target(&json!({"url":"https://user:password@example.com"})).is_err());
        for (name, kind) in [("report.pdf", "pdf"), ("photo.png", "image"), ("drawing.svg", "image"),
            ("page.html", "html"), ("slides.pptx", "pptx"), ("data.xlsx", "xlsx"), ("notes.md", "markdown"), ("notes.txt", "text")] {
            assert_eq!(format(Path::new(name)).0, kind);
        }
        for name in ["legacy.doc", "slides.ppt", "slides.pptx", "sheet.xlsx"] {
            assert!(is_office(Path::new(name)));
        }
    }
}
