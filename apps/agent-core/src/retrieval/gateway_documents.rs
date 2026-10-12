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
pub fn read(path: &Path, limits: &Value, cancel: &Arc<AtomicBool>) -> Result<Value, ContractError> {
    read_impl(None, path, limits, cancel)
}
pub fn read_with_config(
    config: &crate::gateway_contract::GatewayConfig,
    path: &Path,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    read_impl(Some(config), path, limits, cancel)
}
fn read_impl(
    config: Option<&crate::gateway_contract::GatewayConfig>,
    path: &Path,
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    let mut policy = crate::tools::service_source_reading::SourceReadingState::default();
    let mut fields = limits.clone();
    fields["path"] = json!(path);
    if let Some(config) = config {
        fields["excludedRoots"] =
            json!([config.data_root, config.data_home, config.extension_root]);
    }
    policy
        .apply("source.reading.filePolicy", &fields, &Some(cancel.clone()))
        .map_err(|error| {
            gateway::error(
                &error.message,
                "PROTECTED_RETRIEVAL_SOURCE",
                error.status_code,
            )
        })?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !["pdf", "docx"].contains(&extension.as_str()) {
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
    let maximum = limits["maximumDocumentInputBytes"]
        .as_u64()
        .unwrap_or(32 * 1024 * 1024);
    let (bytes, metadata) = crate::tools::service_source_reading::read_binary_stable(
        path,
        maximum,
        &Some(cancel.clone()),
    )
    .map_err(|_| failure("DOCUMENT_READ_FAILED"))?;
    let digest = super::gateway_index::hash_bytes(&bytes);
    let result = if extension == "pdf" {
        pdf(config, &bytes, limits, cancel, &digest)?
    } else {
        docx(&bytes, limits, cancel, &digest)?
    };
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
/// The existing Windows OCR protocol keeps a real process owner and lease until the child exits.
/// 沿用 Windows OCR 协议，真实进程所有者与资源租约持续到子进程确认退出。
fn recognize(
    config: &crate::gateway_contract::GatewayConfig,
    bytes: &[u8],
    pages: &[u32],
    maximum: usize,
    cancel: &Arc<AtomicBool>,
) -> Result<Value, ContractError> {
    use crate::tools::{gateway_process, service_document_extraction::DocumentExtractionState};
    use base64::Engine;
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncWriteExt, BufReader};
    let mut control = DocumentExtractionState::default();
    let apply = |control: &mut DocumentExtractionState, operation: &str, args: Value| {
        control
            .apply(operation, &args)
            .map_err(|error| gateway::error(&error.message, "OCR_FAILED", error.status_code))
    };
    let plan = apply(
        &mut control,
        "ocrOpen",
        json!({"bytesValid":true,"byteLength":bytes.len(),"pages":pages,"maximumOutputBytes":maximum,"timeoutMs":8000}),
    )?;
    let result=super::gateway_embedding::runtime()?.block_on(async{
        let service=crate::resources::gateway::global().ok_or_else(||failure("DOCUMENT_RESOURCE_FAILED"))?;
        let(sender,receiver)=tokio::sync::watch::channel(false);let token=cancel.clone();let monitor=tokio::spawn(async move{while !token.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(25)).await;}let _=sender.send(true);});
        let mut request=plan["resource"].clone();request["taskId"]=json!(gateway::new_id());request["workspaceId"]=json!("local");request["gpuMemoryBytes"]=json!(0);request["ttlMs"]=json!(30000);
        let acquired=service.acquire(request,receiver).await;monitor.abort();let mut lease=acquired.map_err(|_|failure("DOCUMENT_RESOURCE_FAILED"))?;
        let process=(||{
            let mut command=tokio::process::Command::new(&config.toolhost_exe);gateway_process::hide(&mut command);command.arg("--document-ocr").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
            command.spawn().map_err(|_|failure("OCR_UNAVAILABLE"))
        })();
        let result=match process {Ok(mut child)=>{
            let mut child_owner=None;
            let output=async{
                child_owner=Some(gateway_process::OwnedProcessJob::attach(&child).map_err(|error|gateway::error(&error.message,"OCR_UNAVAILABLE",error.status))?);
                let registered=service.request("registerExecutor",json!({"leaseId":lease.allocation["leaseId"],"processId":child.id()}),Duration::from_secs(5)).await.map_err(|_|failure("DOCUMENT_RESOURCE_FAILED"))?;
                apply(&mut control,"registered",json!({"status":registered["status"]}))?;
                let mut stdin=child.stdin.take().ok_or_else(||failure("OCR_FAILED"))?;let stdout=child.stdout.take().ok_or_else(||failure("OCR_FAILED"))?;
                let mut wire=json!({"operation":"document_ocr","schemaVersion":1,"bytesBase64":base64::engine::general_purpose::STANDARD.encode(bytes),"pages":pages,"maximumOutputBytes":maximum}).to_string();wire.push('\n');
                stdin.write_all(wire.as_bytes()).await.map_err(|_|failure("OCR_FAILED"))?;drop(stdin);
                let wire=gateway_process::bounded_line(&mut BufReader::new(stdout),maximum+65536).await.map_err(|_|failure("DOCUMENT_OUTPUT_LIMIT"))?.ok_or_else(||failure("OCR_INVALID_RESULT"))?;
                apply(&mut control,"ocrChunk",json!({"id":plan["id"],"bytes":wire.len()}))?;
                let value:Value=serde_json::from_str(&wire).map_err(|_|failure("OCR_INVALID_RESULT"))?;
                let exit=child.wait().await.map_err(|_|failure("OCR_FAILED"))?;
                apply(&mut control,"ocrResult",json!({"id":plan["id"],"result":value,"exitCode":exit.code(),"textBytes":value["pages"].as_array().map(|pages|pages.iter().map(|page|page["text"].as_str().map(str::len)).collect::<Vec<_>>())}))?;Ok(value)
            };
            let aborted=async{while !cancel.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(25)).await;}};
            let result=tokio::select!{result=output=>result,_=aborted=>Err(failure("ABORT_ERR")),_=tokio::time::sleep(Duration::from_millis(plan["timeoutMs"].as_u64().unwrap_or(8000)))=>Err(failure("DOCUMENT_DECODER_TIMEOUT"))};
            if result.is_err(){gateway_process::terminate(&mut child).await;}let _=child.wait().await;drop(child_owner);result
        },Err(error)=>Err(error)};
        lease.release_confirmed().await.map_err(|_|failure("DOCUMENT_RESOURCE_FAILED"))?;result
    });
    let _ = apply(&mut control, "ocrClose", json!({"id":plan["id"]}));
    result
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
    let mut first = 1u32;
    let mut recognized = std::collections::HashMap::new();
    let mut ocr_identity = Value::Null;
    for (position, page) in pages.keys().enumerate() {
        cancelled(cancel)?;
        let mut page_text = document.extract_text(&[*page]).unwrap_or_default();
        let mut method = "text";
        if page_text.trim().is_empty() {
            if !recognized.contains_key(page) {
                let missing = pages
                    .keys()
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
        if position + 1 == pages.len() {
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
    value["documentComplete"] = json!(true);
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
    Ok(
        json!({"text":text,"extraction":{"format":"docx","version":"rust-docx-text-v1","rawContentHash":digest}}),
    )
}
