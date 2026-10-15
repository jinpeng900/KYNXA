//! File-backed workspace previews share the regular filesystem authorization and decoder.
//! 文件工作区预览复用正式文件授权与解析器；展示引用不授予整盘读取权限。

use super::gateway::{GatewayToolContext, check_cancelled, failure};
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::{io::{Cursor, Read}, path::{Path, PathBuf}, process::Stdio, sync::OnceLock, time::Duration};

const MAX_PREVIEW_BYTES: u64 = 256 * 1024 * 1024;

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
async fn office_pdf(path: &Path, version: &str, source_bytes: &[u8], context: &GatewayToolContext) -> Result<Option<PathBuf>, GatewayError> {
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
    let (bytes, version) = tokio::task::spawn_blocking(move || {
        let bytes = checked_bytes(&source, &read_binding)?;
        let version = super::local_files::sha256(&bytes);
        Ok::<_, GatewayError>((bytes, version))
    }).await
        .map_err(|_| failure("PREVIEW_WORKER_FAILED", "文件预览读取未完成。", 500))??;
    check_cancelled(context)?;
    if arguments["version"].as_str().is_some_and(|expected| expected != version) {
        return Err(failure("TOOL_FILE_CONFLICT", "文件已更新，请重新打开当前版本。", 409));
    }
    let (kind, mime_type) = format(&path);
    let title = arguments["title"].as_str().filter(|title| !title.trim().is_empty())
        .map(str::to_owned).unwrap_or_else(|| path.file_name().unwrap_or_default().to_string_lossy().into_owned());
    let mut result = json!({"kind":kind,"title":title,"path":super::local_files::path_text(&path),"version":version,"mimeType":mime_type});
    if arguments["_metadataOnly"] == true {
        // Display actions return a file reference; only the active UI preview loads its body.
        // 展示动作只返回文件引用，正文由当前活动预览读取，不重复塞入模型上下文。
        result["previewAvailable"] = json!(true);
        return Ok(result);
    }
    if matches!(kind, "pdf" | "image" | "html") {
        result["contentUrl"] = json!(content_url(context, &path, &version, false));
    } else if is_office(&path) {
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
    if !matches!(kind, "pdf" | "image" | "html") {
        return Err(failure("PREVIEW_FORMAT_UNSUPPORTED", "此文件通过文本预览读取。", 422));
    }
    Ok(WorkspaceContent { bytes, mime_type, version })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
