//! Structured document generation uses the existing authorized file transaction, never a script runtime.
//! 结构化文档生成复用已授权文件事务，不调用脚本运行时或让模型传输完整二进制文件。

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};

use super::gateway::{GatewayToolContext, WorkerCancellation, check_cancelled, failure};
use crate::gateway_contract::GatewayError;

const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;

fn document_format(path: &Path, arguments: &Value) -> Result<String, GatewayError> {
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
    let format = arguments["format"].as_str().unwrap_or(&extension).trim().to_ascii_lowercase();
    if !["docx", "pptx", "xlsx", "pdf"].contains(&format.as_str()) || format != extension {
        return Err(failure("INVALID_DOCUMENT_FORMAT", "请使用相符的 DOCX、PPTX、XLSX 或 PDF 文件扩展名和格式。", 400));
    }
    Ok(format)
}

fn validate_xml(bytes: &[u8], root_name: &[u8]) -> Result<(), GatewayError> {
    let mut reader = Reader::from_reader(bytes);
    let mut found_root = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) if !found_root => {
                if element.local_name().as_ref() != root_name {
                    return Err(failure("DOCUMENT_INVALID_RESULT", "生成文档的 XML 主部件无效。", 500));
                }
                found_root = true;
            }
            Ok(Event::DocType(_)) | Err(_) => {
                return Err(failure("DOCUMENT_INVALID_RESULT", "生成文档的 XML 结构无效。", 500));
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }
    if !found_root {
        return Err(failure("DOCUMENT_INVALID_RESULT", "生成文档的 XML 主部件为空。", 500));
    }
    Ok(())
}

fn validate_document(bytes: &[u8], format: &str) -> Result<(), GatewayError> {
    if bytes.is_empty() {
        return Err(failure("DOCUMENT_INVALID_RESULT", "文档生成未返回文件内容。", 500));
    }
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(failure("TOOL_FILE_TOO_LARGE", "生成文档超过 64 MiB 上限。", 400));
    }
    if format == "pdf" {
        let document = lopdf::Document::load_mem(bytes)
            .map_err(|_| failure("DOCUMENT_INVALID_RESULT", "转换结果不是有效的 PDF 文档。", 500))?;
        if document.get_pages().is_empty() {
            return Err(failure("DOCUMENT_INVALID_RESULT", "生成 PDF 没有可读取的页面。", 500));
        }
        return Ok(());
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| failure("DOCUMENT_INVALID_RESULT", "生成结果不是有效的 Office 文档包。", 500))?;
    let format_parts: &[(&str, &[u8])] = match format {
        "docx" => &[("word/document.xml", b"document")],
        "pptx" => &[("ppt/presentation.xml", b"presentation"), ("ppt/slides/slide1.xml", b"sld")],
        "xlsx" => &[("xl/workbook.xml", b"workbook"), ("xl/worksheets/sheet1.xml", b"worksheet")],
        _ => return Err(failure("INVALID_DOCUMENT_FORMAT", "文档格式不受支持。", 400)),
    };
    for (name, root) in [("[Content_Types].xml", b"Types".as_slice()), ("_rels/.rels", b"Relationships".as_slice())]
        .into_iter().chain(format_parts.iter().copied()) {
        let mut part = archive.by_name(name)
            .map_err(|_| failure("DOCUMENT_INVALID_RESULT", "生成 Office 文档缺少必要部件。", 500))?;
        if part.size() > MAX_DOCUMENT_BYTES {
            return Err(failure("TOOL_FILE_TOO_LARGE", "生成文档的 XML 部件超过处理上限。", 400));
        }
        let mut xml = Vec::new();
        (&mut part).take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut xml)?;
        if xml.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(failure("TOOL_FILE_TOO_LARGE", "生成文档的 XML 部件超过处理上限。", 400));
        }
        validate_xml(&xml, root)?;
    }
    Ok(())
}

enum GeneratedContent {
    Bytes(Vec<u8>),
    ConvertedPdf(PathBuf),
}

/// The gateway supplies a canonical target/binding after its ordinary write permission gate.
/// 网关先执行普通写入权限检查，再传入真实目标及绑定；生成器不能自行扩大文件权限。
pub(super) async fn create(context: &GatewayToolContext, arguments: Value) -> Result<Value, GatewayError> {
    check_cancelled(context)?;
    let path = arguments["path"].as_str().map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| failure("TOOL_PATH_CHANGED", "文档目标尚未完成路径授权。", 409))?;
    let binding = arguments["_pathBinding"].clone();
    if !binding.is_object() || binding["path"].as_str() != Some(super::local_files::path_text(&path).as_str()) {
        return Err(failure("TOOL_PATH_CHANGED", "文档目标与已授权路径不一致。", 409));
    }
    let format = document_format(&path, &arguments)?;
    let source_format = if format == "pdf" { "docx".to_owned() } else { format.clone() };
    let worker_arguments = arguments.clone();
    let cancellation_guard = WorkerCancellation::new(context);
    let signal = cancellation_guard.signal.clone();
    let pdf = format == "pdf";
    let generated = tokio::task::spawn_blocking(move || {
        let bytes = super::document_packages::generate(&worker_arguments, &source_format)
            .map_err(|message| failure("INVALID_DOCUMENT_CONTENT", &message, 400))?;
        if pdf { validate_document(&bytes, "docx")?; }
        let version = if pdf { Some(super::local_files::sha256(&bytes)) } else { None };
        Ok::<_, GatewayError>((bytes, version))
    }).await.map_err(|_| failure("TOOL_WORKER_FAILED", "文档生成工作线程结束。", 500))??;
    check_cancelled(context)?;
    let content = if pdf {
        let source_name = path.with_extension("docx");
        let converted = super::gateway_workspace_preview::office_pdf(&source_name, generated.1.as_deref().unwrap(), &generated.0, context)
            .await?.ok_or_else(|| failure("DOCUMENT_CONVERTER_UNAVAILABLE", "当前软件包未提供 PDF 转换器；可先生成 DOCX 文档。", 503))?;
        GeneratedContent::ConvertedPdf(converted)
    } else { GeneratedContent::Bytes(generated.0) };
    check_cancelled(context)?;
    let expected = arguments["expectedHash"].clone();
    let result_format = format.clone();
    // Cancellation after rename must retain the committed-file receipt.
    // 重命名后取消仍保留文件已提交的回执。
    let receipt = tokio::task::spawn_blocking(move || {
        let bytes = match content {
            GeneratedContent::Bytes(bytes) => bytes,
            GeneratedContent::ConvertedPdf(path) => {
                let file = std::fs::File::open(path)?;
                if file.metadata()?.len() > MAX_DOCUMENT_BYTES {
                    return Err(failure("TOOL_FILE_TOO_LARGE", "生成 PDF 超过 64 MiB 上限。", 400));
                }
                let mut bytes = Vec::new();
                file.take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut bytes)?;
                bytes
            }
        };
        validate_document(&bytes, &result_format)?;
        let mut receipt = super::service_filesystem::commit_binary(&path, &bytes, &expected, &binding, &Some(signal))
            .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
        receipt["format"] = json!(result_format);
        receipt["actualFormat"] = receipt["format"].clone();
        receipt["validated"] = json!(true);
        receipt["validation"] = json!(if result_format == "pdf" { "pdf-structure" } else { "package-structure" });
        Ok::<_, GatewayError>(receipt)
    }).await.map_err(|_| failure("TOOL_WORKER_FAILED", "文档提交工作线程结束。", 500))?;
    drop(cancellation_guard);
    receipt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires KYNXA_TEST_DOCUMENT_OFFICIAL_ROOT pointing at a packaged Office runtime"]
    async fn document_packages_open_with_packaged_office_and_publish_real_pdf() {
        let (mut context, _cancellation) = super::super::gateway::test_context();
        let test_root = context.workdir.parent().unwrap().to_owned();
        let official = std::env::var("KYNXA_TEST_DOCUMENT_OFFICIAL_ROOT").expect("packaged Office root");
        context.config["officialToolsRoot"] = json!(official);
        let receipt = super::super::gateway::execute("document.create", json!({
            "path":"report.pdf","title":"中文与 English 报告","blocks":[
                {"type":"heading","level":1,"text":"验证内容"},
                {"type":"paragraph","text":"真实 DOCX 导出，保留中文内容。"},
                {"type":"table","rows":[["Item","Value"],["样本",42]]}
            ],"expectedHash":null
        }), &context).await.unwrap();
        assert_eq!(receipt["validated"], true);
        validate_document(&std::fs::read(context.workdir.join("report.pdf")).unwrap(), "pdf").unwrap();
        for (format, arguments) in [
            ("pptx", json!({"slides":[{"title":"中英 Slides","body":"真实幻灯片","bullets":["Point","要点"],"rows":[["指标","值"],["A",12]]}]})),
            ("xlsx", json!({"sheets":[{"name":"Data","rows":[["Item","Value"],["样本",42],["Formula",{"formula":"B2*2","cachedValue":84}]]}]})),
        ] {
            let bytes = super::super::document_packages::generate(&arguments, format).unwrap();
            let name = context.workdir.join(format!("fixture.{format}"));
            let version = super::super::local_files::sha256(&bytes);
            let exported = super::super::gateway_workspace_preview::office_pdf(&name, &version, &bytes, &context).await.unwrap().unwrap();
            validate_document(&std::fs::read(exported).unwrap(), "pdf").unwrap();
        }
        std::fs::remove_dir_all(test_root).unwrap();
    }

    #[tokio::test]
    async fn document_create_commits_real_office_packages_and_keeps_conflicts() {
        let (context, _cancellation) = super::super::gateway::test_context();
        for (format, content) in [
            ("docx", json!({"title":"报告","content":"中文与 English 文档正文。"})),
            ("pptx", json!({"slides":[{"title":"概览","body":"幻灯片正文","bullets":["First","第二项"]}]})),
            ("xlsx", json!({"sheets":[{"name":"Data","rows":[["名称","数量"],["First",42]]}]})),
        ] {
            let path = context.workdir.join(format!("generated.{format}"));
            let mut arguments = content;
            arguments["path"] = json!(path);
            arguments["expectedHash"] = Value::Null;
            let receipt = super::super::gateway::execute("document.create", arguments.clone(), &context).await.unwrap();
            assert_eq!(receipt["validated"], true);
            assert_eq!(receipt["actualFormat"], format);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(receipt["sha256"], super::super::local_files::sha256(&bytes));
            assert_eq!(receipt["bytes"], bytes.len());
            validate_document(&bytes, format).unwrap();
            let conflict = super::super::gateway::execute("document.create", arguments, &context).await.unwrap_err();
            assert_eq!(conflict.code, "TOOL_FILE_CONFLICT");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let denied = super::super::gateway::execute("document.create", json!({
            "path":context.data_home.join("private.docx"),"content":"private","expectedHash":null}), &context).await.unwrap_err();
        assert_eq!(denied.code, "PROTECTED_APP_DATA");
        std::fs::remove_dir_all(context.data_home.parent().unwrap()).unwrap();
    }
}
