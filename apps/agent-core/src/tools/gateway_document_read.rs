//! Direct document reads reuse the native decoder after the regular file authorization gate.
//! 按路径读取文档复用原生解析器，先通过现有文件权限门，不要求入库或向量准备完成。

use super::gateway::{GatewayToolContext, WorkerCancellation, check_cancelled, failure};
use crate::gateway_contract::{GatewayConfig, GatewayError};
use serde_json::{Value, json};
use std::path::PathBuf;

pub(super) async fn read(
    arguments: Value,
    path: PathBuf,
    binding: Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let application_root = context.config["officialToolsRoot"]
        .as_str()
        .and_then(|root| std::path::Path::new(root).parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            context.toolhost_exe.parent().unwrap_or(&context.workdir).to_owned()
        });
    let config = GatewayConfig {
        data_root: context.data_home.clone(),
        data_home: context.data_home.join("Models"),
        extension_root: context.extension_root.clone(),
        toolhost_exe: context.toolhost_exe.clone(),
        vector_extension: application_root.join("runtime/native/vec0.dll"),
        inference_library: application_root.join("runtime/native/onnxruntime.dll"),
        application_root,
        port: 0,
    };
    let mut limits = context.config["retrieval"]["local"]["indexing"].clone();
    if !limits.is_object() {
        limits = json!({});
    }
    let deferred_settings = context.config["deferredDocumentSettings"] == true;
    let project_id = context.project_id.clone();
    if crate::document_formats::detect(&path).is_some_and(|format| format.name == "pdf") {
        let start_page = arguments["startPage"].as_u64().unwrap_or(1);
        limits["startPage"] = json!(start_page);
        limits["endPage"] = json!(
            arguments["endPage"].as_u64().unwrap_or_else(|| start_page.saturating_add(99))
        );
    } else if arguments.get("startPage").is_some() || arguments.get("endPage").is_some() {
        return Err(failure("INVALID_TOOL_ARGUMENTS", "页码范围只用于 PDF。", 400));
    }
    let cancellation_guard = WorkerCancellation::new(context);
    let cancellation = cancellation_guard.signal.clone();
    let result = tokio::task::spawn_blocking(move || {
        super::service_filesystem::revalidate(&binding)
            .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
        if deferred_settings {
            // Load decoder limits only for Office text extraction, not ordinary previews.
            // 仅提取 Office 正文时加载解析限额，普通预览无需准备检索配置。
            let saved = crate::retrieval::gateway::effective(&config, project_id.as_deref())?;
            let mut configured = saved["local"]["indexing"].clone();
            for (name, value) in limits.as_object().unwrap() {
                configured[name.as_str()] = value.clone();
            }
            limits = configured;
        }
        let extracted = crate::retrieval::gateway_documents::read_authorized_with_config(
            &config,
            &path,
            &limits,
            &cancellation,
        )?;
        super::service_filesystem::revalidate(&binding)
            .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
        page_result(&path, &arguments, extracted)
    })
    .await
    .map_err(|_| failure("TOOL_WORKER_FAILED", "文档解析工作线程结束。", 500));
    drop(cancellation_guard);
    check_cancelled(context)?;
    result?
}

/// Character offsets belong to the selected page window; nextPage advances to other PDF pages.
/// 字符偏移属于当前选定页窗口；PDF 的其他页面通过 nextPage 续读，不能伪装为整文件已读。
fn page_result(
    path: &std::path::Path,
    arguments: &Value,
    extracted: Value,
) -> Result<Value, GatewayError> {
    let mut extraction = extracted["extraction"].clone();
    let mut text = String::new();
    if let Some(windows) = extracted["documentWindows"].as_array() {
        let mut pages = Vec::new();
        let mut offset = 0;
        for window in windows {
            let body = window["text"].as_str().unwrap_or("");
            for page in window["extraction"]["pages"].as_array().into_iter().flatten() {
                let mut page = page.clone();
                page["startOffset"] = json!(offset + page["startOffset"].as_u64().unwrap_or(0));
                page["endOffset"] = json!(offset + page["endOffset"].as_u64().unwrap_or(0));
                pages.push(page);
            }
            offset += body.encode_utf16().count() as u64;
            text.push_str(body);
        }
        if extraction["format"] == "pdf" {
            extraction["pages"] = json!(pages);
            let first = &windows[0]["extraction"]["pageWindow"];
            let last = &windows[windows.len() - 1]["extraction"]["pageWindow"];
            extraction["pageWindow"] = json!({"version":"pdf-page-window-v1",
                "startPage":first["startPage"], "endPage":last["endPage"], "nextPage":last["nextPage"]});
            extraction["complete"] = extracted["documentComplete"].clone();
            let mut uncovered = Vec::new();
            if first["startPage"].as_u64().unwrap_or(1) > 1 {
                uncovered.push(json!({"startPage":1,"endPage":first["startPage"].as_u64().unwrap()-1}));
            }
            if !last["nextPage"].is_null() {
                uncovered.push(json!({"startPage":last["nextPage"],"endPage":extraction["pageCount"]}));
            }
            extraction["uncoveredRanges"] = json!(uncovered);
        }
    } else {
        text.push_str(extracted["text"].as_str().unwrap_or(""));
    }
    let hash = extraction["rawContentHash"]
        .as_str()
        .or_else(|| extracted["rawContentHash"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| failure("DOCUMENT_INVALID_RESULT", "文档解析未返回原文件版本。", 500))?;
    if arguments["expectedSha256"].as_str().is_some_and(|expected| expected != hash) {
        return Err(failure("TOOL_FILE_CONFLICT", "文档已变化，请重新从当前版本读取。", 409));
    }
    let mut page = super::service_filesystem::text_page(
        &text,
        arguments["offset"].as_u64().unwrap_or(0) as usize,
        arguments["maxChars"].as_u64().unwrap_or(12000) as usize,
    )
    .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    page["path"] = json!(super::local_files::path_text(path));
    page["sha256"] = json!(hash);
    page["text"] = page["content"].clone();
    page.as_object_mut().unwrap().remove("content");
    page["offsetUnit"] = json!("utf16-code-units");
    page["metadata"] = extracted["metadata"].clone();
    page["extraction"] = extraction.clone();
    page["nextPage"] = extraction["pageWindow"]["nextPage"].clone();
    page["extractionComplete"] = json!(extraction["complete"] != false);
    page["documentComplete"] = json!(
        extraction["complete"] != false && page["hasMore"] != true && page["offset"] == 0
    );
    if !extraction.is_object() {
        extraction = json!({"format":"text","rawContentHash":hash,"complete":true});
    }
    if let Some(fields) = extraction.as_object_mut() {
        fields.remove("pages");
    }
    page["locator"] = json!({"path":page["path"], "extraction":extraction});
    page["indexRequired"] = json!(false);
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_page_pdf(path: &std::path::Path) {
        use lopdf::{Document, Object, Stream, dictionary};
        let mut pdf = Document::with_version("1.5");
        let pages = pdf.new_object_id();
        let font = pdf.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
        let resources = pdf.add_object(dictionary! {"Font"=>dictionary! {"F1"=>font}});
        let mut children = Vec::<Object>::new();
        for body in ["First page source.", "Second page source."] {
            let content = pdf.add_object(Stream::new(dictionary!{},
                format!("BT /F1 12 Tf 20 100 Td ({body}) Tj ET").into_bytes()));
            let page = pdf.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,
                "MediaBox"=>vec![0.into(),0.into(),200.into(),200.into()],
                "Resources"=>resources,"Contents"=>content});
            children.push(page.into());
        }
        pdf.objects.insert(pages, Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>children,"Count"=>2}));
        let catalog = pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
        pdf.trailer.set("Root", catalog);
        pdf.save(path).unwrap();
    }

    #[tokio::test]
    async fn authorized_documents_read_real_word_and_pdf_without_an_index() {
        let (context, _cancellation) = super::super::gateway::test_context();
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/documents");
        for name in ["account-guide.docx", "account-text.pdf"] {
            std::fs::copy(fixtures.join(name), context.workdir.join(name)).unwrap();
            let first = super::super::gateway::execute("document.read",
                json!({"path":name,"maxChars":40}), &context).await.unwrap();
            assert!(!first["text"].as_str().unwrap().is_empty());
            assert_eq!(first["sha256"].as_str().unwrap().len(), 64);
            assert_eq!(first["hasMore"], true);
            assert_eq!(first["documentComplete"], false);
            let next = super::super::gateway::execute("document.read", json!({"path":name,
                "offset":first["nextOffset"],"expectedSha256":first["sha256"]}), &context).await.unwrap();
            assert_eq!(next["offset"], first["nextOffset"]);
            assert_eq!(next["sha256"], first["sha256"]);
            assert_eq!(next["executionEnvironment"]["operationLocation"], "gateway-host");
        }
        two_page_pdf(&context.workdir.join("two-pages.pdf"));
        let first_page = super::super::gateway::execute("document.read",
            json!({"path":"two-pages.pdf","endPage":1}), &context).await.unwrap();
        assert!(first_page["text"].as_str().unwrap().contains("First page source."));
        assert!(!first_page["text"].as_str().unwrap().contains("Second page source."));
        assert_eq!(first_page["nextPage"], 2);
        assert_eq!(first_page["documentComplete"], false);
        assert_eq!(first_page["locator"]["extraction"]["uncoveredRanges"], json!([{"startPage":2,"endPage":2}]));
        let second_page = super::super::gateway::execute("document.read", json!({"path":"two-pages.pdf",
            "startPage":first_page["nextPage"],"expectedSha256":first_page["sha256"]}), &context).await.unwrap();
        assert!(second_page["text"].as_str().unwrap().contains("Second page source."));
        assert!(second_page["nextPage"].is_null());
        assert_eq!(second_page["documentComplete"], false);
        let protected = context.data_home.join("private.docx");
        std::fs::copy(fixtures.join("account-guide.docx"), &protected).unwrap();
        let denied = super::super::gateway::execute("document.read",
            json!({"path":protected,"reason":"Read outside work."}), &context).await.unwrap_err();
        assert_eq!(denied.code, "PROTECTED_APP_DATA");
        let mut sandbox_context = context.clone();
        sandbox_context.workdir = context.data_home.join("Sandboxes").join(&context.conversation_id);
        std::fs::create_dir_all(&sandbox_context.workdir).unwrap();
        sandbox_context.authorized_roots = vec![sandbox_context.workdir.clone()];
        std::fs::copy(fixtures.join("account-guide.docx"), sandbox_context.workdir.join("guide.docx")).unwrap();
        let sandbox_read = super::super::gateway::execute("document.read",
            json!({"path":"guide.docx"}), &sandbox_context).await.unwrap();
        assert!(sandbox_read["text"].as_str().unwrap().contains("How to reset an account password"));
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }
}
