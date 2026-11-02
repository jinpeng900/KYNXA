//! 训练样本沿用现有文件授权和解析器，按来源分组后冻结，不读取其他聊天的私有资料。
use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    storage::managed_files,
    tools::{
        gateway::{self, GatewayToolContext},
        local_files,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

const SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const DATASET_BYTES: usize = 64 * 1024 * 1024;
const MAX_SAMPLES: usize = 200_000;
const MAX_SOURCES: usize = 2_000;

#[path = "dataset_tables.rs"]
mod tables;
use tables::{delimited, table_samples};

fn invalid(message: impl Into<String>) -> GatewayError {
    GatewayError::new("TRAINING_DATA_INVALID", message, 400)
}
fn text_hash(value: &str) -> String {
    managed_files::digest(value.as_bytes())
}

fn check(context: &GatewayToolContext) -> Result<(), GatewayError> {
    if *context.cancellation.borrow() {
        Err(GatewayError::new(
            "TOOL_CANCELLED",
            "资料导入已取消，已有数据集保留。",
            499,
        ))
    } else {
        Ok(())
    }
}

/// 只有全部来源处理成功后才发布数据集；图片字节在该版本的 assets 中独立保存。
pub(crate) async fn create(
    input: &Value,
    context: &GatewayToolContext,
    config: &GatewayConfig,
    dataset_dir: &Path,
) -> Result<Value, GatewayError> {
    check(context)?;
    let normalized = normalize_input(input)?;
    let input = &normalized;
    managed_files::inspect(
        &context.data_home,
        dataset_dir,
        true,
        false,
        DATASET_BYTES as u64,
    )?;
    let supplied = input["samples"].as_array();
    let mut kind = input["kind"]
        .as_str()
        .or_else(|| input["format"].as_str())
        .filter(|kind| ["text", "tabular", "image"].contains(kind))
        .unwrap_or("text")
        .to_owned();
    if input["kind"]
        .as_str()
        .is_some_and(|kind| !["text", "tabular", "image"].contains(&kind))
    {
        return Err(invalid("数据类型只支持 text、tabular 或 image。"));
    }
    if input.get("kind").is_none() {
        if let Some(first) = supplied.and_then(|samples| samples.first()) {
            if first["features"].is_array() {
                kind = "tabular".into();
            } else if first["imagePath"].is_string() {
                kind = "image".into();
            }
        }
    }
    let mut samples = Vec::new();
    let mut sources = Vec::new();
    let mut feature_names: Vec<String> = input["featureNames"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if let Some(rows) = supplied {
        if rows.len() > MAX_SAMPLES {
            return Err(invalid("样本超过单个数据集的 200000 条容量，请分开导入。"));
        }
        for row in rows {
            if !row.is_object() {
                return Err(invalid("每条训练样本必须是对象。"));
            }
            let mut row = row.clone();
            if kind == "image" {
                let path = row["imagePath"]
                    .as_str()
                    .ok_or_else(|| invalid("图片样本缺少 imagePath。"))?;
                let source = json!({"path":path,"label":row["label"],"text":row["text"],"messages":row["messages"]});
                let (file, metadata, bytes) = source_bytes(&source, context).await?;
                row["imagePath"] = json!(freeze_image(&file, &bytes, dataset_dir, context)?);
                row["source"] = metadata.clone();
                row["groupId"] = metadata["sha256"].clone();
                sources.push(metadata);
            }
            validate_sample(&row, &kind)?;
            samples.push(row);
        }
    }
    if let Some(text) = input["text"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
    {
        if kind != "text" {
            return Err(invalid("直接输入文字请使用 text 类型数据集。"));
        }
        let digest = text_hash(text);
        append_text(
            &mut samples,
            text,
            &digest,
            &json!({"kind":"inline","sha256":digest}),
            input,
        )?;
    }
    let requested = input["sources"].as_array().cloned().unwrap_or_default();
    if requested.len() > MAX_SOURCES {
        return Err(invalid("单个数据集最多导入 2000 个来源，请分开创建。"));
    }
    let mut pending = requested;
    let mut visited = HashSet::new();
    let mut count = 0;
    while let Some(source) = pending.pop() {
        check(context)?;
        if !source.is_object() {
            return Err(invalid("资料来源必须包含 path 或 importId。"));
        }
        if source["importId"].as_str().is_none() {
            let path = source["path"]
                .as_str()
                .ok_or_else(|| invalid("资料来源缺少 path。"))?;
            let stat = authorized_stat(path, context).await?;
            if stat["type"] == "directory" {
                let absolute = stat["path"]
                    .as_str()
                    .ok_or_else(|| invalid("目录未返回真实路径。"))?;
                if !visited.insert(absolute.to_owned()) {
                    continue;
                }
                if visited.len() > 128 {
                    return Err(invalid("来源子目录超过 128 个，请缩小导入范围。"));
                }
                let list = Box::pin(gateway::execute(
                    "filesystem.list",
                    json!({"path":absolute,"limit":500}),
                    context,
                ))
                .await?;
                require_authorized(&list)?;
                if list["truncated"] == true {
                    return Err(invalid("单个目录文件过多，请选择具体资料或分批导入。"));
                }
                for entry in list["entries"].as_array().into_iter().flatten() {
                    let name = entry["name"].as_str().unwrap_or("");
                    if name.starts_with('.') || entry["type"] == "link" {
                        continue;
                    }
                    let child = Path::new(absolute).join(name);
                    if entry["type"] == "directory" || supported(&child, &kind) {
                        let mut child_source = source.clone();
                        child_source["path"] = json!(child);
                        if input["inferLabelsFromFolders"] == true && kind == "image" {
                            child_source["label"] = json!(
                                Path::new(absolute)
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("")
                            );
                        }
                        pending.push(child_source);
                    }
                }
                continue;
            }
        }
        count += 1;
        if count > MAX_SOURCES {
            return Err(invalid("导入来源超过 2000 个文件，请缩小范围。"));
        }
        let (path, metadata, bytes) = source_bytes(&source, context).await?;
        let hash = metadata["sha256"].as_str().unwrap().to_owned();
        if kind != "image" && sources.iter().any(|old| old["sha256"] == hash) {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if kind == "image" {
            let mut row = json!({"imagePath":freeze_image(&path,&bytes,dataset_dir,context)?,"source":metadata,"groupId":hash});
            for field in ["label", "text", "messages", "split"] {
                if !source[field].is_null() {
                    row[field] = source[field].clone();
                }
            }
            samples.push(row);
        } else if matches!(
            extension.as_str(),
            "xlsx" | "xls" | "xlsm" | "xltx" | "xltm"
        ) && (kind == "tabular" || input.get("kind").is_none())
        {
            kind = "tabular".into();
            let parameters = input.clone();
            let metadata_for_read = metadata.clone();
            let monitor = gateway::WorkerCancellation::new(context);
            let token = monitor.signal.clone();
            let (rows, names) = tokio::task::spawn_blocking(move || {
                tables::spreadsheet_samples(
                    &bytes,
                    &extension,
                    &parameters,
                    &metadata_for_read,
                    &token,
                )
            })
            .await
            .map_err(|_| {
                GatewayError::new("TRAINING_IMPORT_FAILED", "工作表导入未完成。", 500)
            })??;
            drop(monitor);
            check(context)?;
            if feature_names.is_empty() {
                feature_names = names;
            } else if names != feature_names {
                return Err(invalid("多个表格的特征列不一致，请统一列名和顺序。"));
            }
            samples.extend(rows);
        } else if matches!(extension.as_str(), "csv" | "tsv")
            && (kind == "tabular" || input.get("kind").is_none())
        {
            if kind != "tabular" && input.get("kind").is_none() && samples.is_empty() {
                kind = "tabular".into();
            }
            if kind != "tabular" {
                return Err(invalid("表格资料请选择 tabular 类型，或先整理为文本样本。"));
            }
            let text = decode(&bytes, input["encoding"].as_str())?;
            let (rows, names) = table_samples(
                &text,
                if extension == "tsv" { '\t' } else { ',' },
                input,
                &metadata,
            )?;
            if feature_names.is_empty() {
                feature_names = names;
            } else if names != feature_names {
                return Err(invalid("多个表格的特征列不一致，请统一列名和顺序。"));
            }
            samples.extend(rows);
        } else if matches!(extension.as_str(), "json" | "jsonl" | "ndjson")
            && input["format"] != "text"
        {
            let text = decode(&bytes, input["encoding"].as_str())?;
            let values = sample_json(&text).ok_or_else(|| {
                invalid(
                    "JSON/JSONL 需为有效的 text、messages、prompt/completion 或 features 样本。",
                )
            })?;
            if input.get("kind").is_none()
                && samples.is_empty()
                && values.first().is_some_and(|row| row["features"].is_array())
            {
                kind = "tabular".into();
            }
            for mut row in values {
                if !row.is_object() {
                    return Err(invalid("JSON/JSONL 每条样本必须为对象。"));
                }
                if row["imagePath"].is_string() {
                    return Err(invalid(
                        "JSON 中的图片请通过图片来源或直接 samples 导入，以冻结并校验真实图片。",
                    ));
                }
                validate_sample(&row, &kind)?;
                row["source"] = metadata.clone();
                samples.push(row);
            }
        } else if kind == "text" {
            let text = if crate::document_formats::detect(&path).is_some() {
                extracted_text(path.clone(), config.clone(), context, &hash).await?
            } else {
                decode(&bytes, input["encoding"].as_str())?
            };
            if text.trim().is_empty() {
                return Err(invalid(format!(
                    "{} 没有可用于文本训练的内容；图像学习请选择图片数据集。",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )));
            }
            append_text(&mut samples, &text, &hash, &metadata, input)?;
        } else {
            return Err(invalid("表格训练需要 CSV/TSV 或结构化 features 样本。"));
        }
        sources.push(metadata);
        if samples.len() > MAX_SAMPLES {
            return Err(invalid("样本超过 200000 条容量，请分开导入。"));
        }
    }
    if samples.is_empty() {
        return Err(invalid("没有可训练样本，请导入资料或填写文本。"));
    }
    let (samples, counts, duplicates) = split_samples(samples, input)?;
    let id = dataset_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("数据集标识无效。"))?;
    let mut warnings = Vec::new();
    if counts["validation"] == 0 && input["split"]["validation"].as_f64().unwrap_or(0.1) > 0.0 {
        warnings.push("当前独立来源不足或划分未产生验证样本；不会把同一文档的片段伪作独立验证集。");
    }
    if counts["test"] == 0 && input["split"]["test"].as_f64().unwrap_or(0.1) > 0.0 {
        warnings.push("当前划分没有独立测试样本，可追加独立资料后新建数据集版本。");
    }
    let mut dataset = json!({"schemaVersion":1,"id":id,"name":input["name"].as_str().filter(|s|!s.trim().is_empty()).unwrap_or("训练资料"),
        "kind":kind,"task":input["task"].as_str().unwrap_or("auto"),"createdAt":chrono::Utc::now().to_rfc3339(),
        "samples":samples,"sampleCount":samples.len(),"splitCounts":counts,"duplicatesRemoved":duplicates,
        "sources":sources,"sourceCount":sources.len(),"warnings":warnings,"splitPolicy":"stable-source-group-sha256-v1"});
    if !feature_names.is_empty() {
        dataset["featureNames"] = json!(feature_names);
    }
    let bytes = serde_json::to_vec(&dataset).map_err(|_| invalid("训练样本无法保存。"))?;
    if bytes.len() > DATASET_BYTES {
        return Err(invalid("冻结数据集超过 64 MiB，请拆分资料，原文件未修改。"));
    }
    dataset["version"] = json!(managed_files::digest(&bytes));
    Ok(dataset)
}

fn sample_json(text: &str) -> Option<Vec<Value>> {
    let value: Vec<Value> = if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(rows) = value.as_array().or_else(|| value["samples"].as_array()) {
            rows.clone()
        } else {
            vec![value]
        }
    } else {
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .ok()?
    };
    (!value.is_empty()
        && value.iter().all(|row| {
            row.is_object()
                && ["features", "text", "messages", "prompt", "imagePath"]
                    .iter()
                    .any(|key| row.get(*key).is_some())
        }))
    .then_some(value)
}
fn normalize_input(input: &Value) -> Result<Value, GatewayError> {
    if !input.is_object() {
        return Err(invalid("请提供数据集配置对象。"));
    }
    let mut value = input.clone();
    let format = input["format"].as_str().unwrap_or("auto");
    if !["auto", "text", "conversation", "table", "tabular", "image"].contains(&format) {
        return Err(invalid("资料格式只支持自动、文本、对话、表格或图片。"));
    }
    if input.get("kind").is_none() {
        match format {
            "table" | "tabular" => value["kind"] = json!("tabular"),
            "conversation" | "text" => value["kind"] = json!("text"),
            "image" => value["kind"] = json!("image"),
            _ => {}
        }
    }
    if input["featureColumns"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        value.as_object_mut().unwrap().remove("featureColumns");
    }
    if input["targetColumn"].as_str() == Some("") || input["targetColumn"].is_null() {
        value.as_object_mut().unwrap().remove("targetColumn");
    }
    let Some(text) = input["text"].as_str().filter(|s| !s.trim().is_empty()) else {
        return Ok(value);
    };
    let structured = if format == "conversation" || format == "auto" {
        let rows = sample_json(text);
        if format == "conversation" && rows.is_none() {
            return Err(invalid(
                "对话资料需为 JSON/JSONL 样本，包含 messages 或 prompt/completion。",
            ));
        }
        rows
    } else {
        None
    };
    let delimiter = if text.lines().next().is_some_and(|line| line.contains('\t')) {
        '\t'
    } else {
        ','
    };
    let looks_table = format == "auto"
        && structured.is_none()
        && delimited(text, delimiter).ok().is_some_and(|rows| {
            rows.len() > 1
                && rows[0].len() > 1
                && rows.iter().all(|row| row.len() == rows[0].len())
                && rows
                    .iter()
                    .skip(1)
                    .flatten()
                    .any(|cell| cell.trim().parse::<f64>().is_ok())
        });
    let rows = if matches!(format, "table" | "tabular") || looks_table {
        let metadata = json!({"kind":"inline","sha256":text_hash(text)});
        let (rows, names) = table_samples(text, delimiter, &value, &metadata)?;
        value["kind"] = json!("tabular");
        value["featureNames"] = json!(names);
        Some(rows)
    } else {
        structured
    };
    if let Some(mut rows) = rows {
        if value.get("kind").is_none() {
            value["kind"] = json!(if rows[0]["features"].is_array() {
                "tabular"
            } else if rows[0]["imagePath"].is_string() {
                "image"
            } else {
                "text"
            });
        }
        if let Some(existing) = input["samples"].as_array() {
            rows.extend(existing.iter().cloned());
        }
        value["samples"] = json!(rows);
        value["text"] = json!("");
    }
    Ok(value)
}

fn require_authorized(result: &Value) -> Result<(), GatewayError> {
    if result["requiresApproval"] == true {
        Err(GatewayError::new(
            "TRAINING_SOURCE_AUTHORIZATION_REQUIRED",
            "训练资料尚未获得当前文件权限，请授权该资料后重试。",
            403,
        ))
    } else {
        Ok(())
    }
}
async fn authorized_stat(path: &str, context: &GatewayToolContext) -> Result<Value, GatewayError> {
    let result = Box::pin(gateway::execute(
        "filesystem.stat",
        json!({"path":path}),
        context,
    ))
    .await?;
    require_authorized(&result)?;
    Ok(result)
}
async fn source_bytes(
    source: &Value,
    context: &GatewayToolContext,
) -> Result<(PathBuf, Value, Vec<u8>), GatewayError> {
    let (path, mut metadata) = if let Some(id) = source["importId"].as_str() {
        crate::training::imports::resolve(id, context)?
    } else {
        let path = source["path"]
            .as_str()
            .ok_or_else(|| invalid("资料来源缺少 path 或 importId。"))?;
        let metadata = authorized_stat(path, context).await?;
        if metadata["type"] != "file" {
            return Err(invalid("训练资料必须是普通文件。"));
        }
        (
            PathBuf::from(
                metadata["path"]
                    .as_str()
                    .ok_or_else(|| invalid("文件未返回真实路径。"))?,
            ),
            metadata,
        )
    };
    let read_path = path.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        local_files::read_checked(&read_path, SOURCE_BYTES, true, "TRAINING_SOURCE_CHANGED")
            .map_err(|error| GatewayError::new(error.code, error.message, error.status_code))
    })
    .await
    .map_err(|_| GatewayError::new("TRAINING_IMPORT_FAILED", "训练资料读取未完成。", 500))??;
    let digest = managed_files::digest(&bytes);
    if source["expectedSha256"]
        .as_str()
        .or_else(|| metadata["sha256"].as_str())
        .is_some_and(|expected| expected != digest)
    {
        return Err(GatewayError::new(
            "TRAINING_SOURCE_CHANGED",
            "资料在导入期间变化，请重新导入当前版本。",
            409,
        ));
    }
    if !metadata.is_object() {
        metadata = json!({});
    }
    if metadata["path"]
        .as_str()
        .is_some_and(|old| Path::new(old) != path)
    {
        metadata["originalPath"] = metadata["path"].clone();
    }
    metadata["path"] = json!(path);
    metadata["sha256"] = json!(digest);
    metadata["bytes"] = json!(bytes.len());
    Ok((path, metadata, bytes))
}
fn supported(path: &Path, kind: &str) -> bool {
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if kind == "image" {
        matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff"
        )
    } else {
        crate::document_formats::detect(path)
            .is_some_and(|format| format.family != crate::document_formats::ParserFamily::Archive)
            || matches!(
                ext.as_str(),
                "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "jsonl" | "ndjson"
            )
    }
}
fn freeze_image(
    path: &Path,
    bytes: &[u8],
    directory: &Path,
    context: &GatewayToolContext,
) -> Result<PathBuf, GatewayError> {
    let format = image::guess_format(bytes).map_err(|_| invalid("来源不是可识别图片。"))?;
    if !matches!(
        format,
        image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::Gif
            | image::ImageFormat::WebP
            | image::ImageFormat::Bmp
            | image::ImageFormat::Tiff
    ) {
        return Err(invalid("图片编码尚不支持。"));
    }
    let dimensions = image::ImageReader::with_format(std::io::Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| invalid("图片尺寸信息无效。"))?;
    if u64::from(dimensions.0) * u64::from(dimensions.1) > 64_000_000 {
        return Err(invalid("单张训练图片超过 6400 万像素，请缩小后导入。"));
    }
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .filter(|x| x.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or("image");
    let target = directory
        .join("assets")
        .join(format!("{}.{}", managed_files::digest(bytes), ext));
    managed_files::atomic_write(&context.data_home, &target, bytes, SOURCE_BYTES)?;
    Ok(target)
}
async fn extracted_text(
    path: PathBuf,
    config: GatewayConfig,
    context: &GatewayToolContext,
    hash: &str,
) -> Result<String, GatewayError> {
    let monitor = gateway::WorkerCancellation::new(context);
    let token = monitor.signal.clone();
    let expected = hash.to_owned();
    let result=tokio::task::spawn_blocking(move||{
        let limits=json!({"maximumSourceBytes":SOURCE_BYTES,"maximumDocumentOutputBytes":8*1024*1024,"maximumPdfPages":100});
        let extracted=crate::retrieval::gateway_documents::read_authorized_with_config(&config,&path,&limits,&token)?;
        let actual=extracted["rawContentHash"].as_str().or_else(||extracted["extraction"]["rawContentHash"].as_str());
        if actual.is_some_and(|actual|actual!=expected){return Err(GatewayError::new("TRAINING_SOURCE_CHANGED","文档解析版本与导入来源不一致。",409));}
        if let Some(windows)=extracted["documentWindows"].as_array(){
            if extracted["documentComplete"]==false||windows.last().is_some_and(|w|!w["extraction"]["pageWindow"]["nextPage"].is_null()){return Err(invalid("PDF 尚有未提取页面，请分段导入。"));}
            let mut text=String::new();
            for body in windows.iter().filter_map(|w|w["text"].as_str()) {
                if text.len()+body.len()>8*1024*1024{return Err(invalid("文档提取正文超过 8 MiB，请分段导入。"));}
                text.push_str(body);text.push('\n');
            }
            Ok(text)
        }else{
            if extracted["extraction"]["complete"]==false{return Err(invalid("文档提取尚不完整，请按范围拆分资料后导入。"));}
            Ok(extracted["text"].as_str().unwrap_or("").to_owned())
        }
    }).await.map_err(|_|GatewayError::new("TRAINING_IMPORT_FAILED","训练文档解析未完成。",500))?;
    drop(monitor);
    check(context)?;
    result
}
fn decode(bytes: &[u8], encoding: Option<&str>) -> Result<String, GatewayError> {
    if encoding.is_some_and(|value| matches!(value, "gbk" | "GBK" | "gb18030")) {
        let (text, _, errors) = encoding_rs::GBK.decode(bytes);
        if errors {
            return Err(invalid("资料与指定的 GBK 编码不一致。"));
        }
        return Ok(text.into_owned());
    }
    if bytes.starts_with(&[0xff, 0xfe]) {
        let (text, _, errors) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        if errors {
            return Err(invalid("UTF-16 文本编码无效。"));
        }
        return Ok(text.into_owned());
    }
    String::from_utf8(
        bytes
            .strip_prefix(&[0xef, 0xbb, 0xbf])
            .unwrap_or(bytes)
            .to_vec(),
    )
    .map_err(|_| invalid("文本不是 UTF-8，请指定 encoding=gbk 或先转换编码。"))
}
fn validate_sample(row: &Value, kind: &str) -> Result<(), GatewayError> {
    if row["split"]
        .as_str()
        .is_some_and(|s| !["train", "validation", "test"].contains(&s))
    {
        return Err(invalid("样本 split 只能为 train、validation 或 test。"));
    }
    match kind {
        "tabular" => {
            let features = row["features"]
                .as_array()
                .filter(|x| !x.is_empty())
                .ok_or_else(|| invalid("表格样本缺少非空 features 数组。"))?;
            if features
                .iter()
                .any(|x| !x.is_number() && !x.is_string() && !x.is_null())
            {
                return Err(invalid("特征值只支持数字、类别文本或空值。"));
            }
        }
        "image" => {
            if !row["imagePath"].is_string() {
                return Err(invalid("图片样本缺少 imagePath。"));
            }
        }
        _ => {
            if !row["text"].as_str().is_some_and(|s| !s.trim().is_empty())
                && !row["messages"].is_array()
                && !(row["prompt"].is_string() && row["completion"].is_string())
            {
                return Err(invalid(
                    "文本样本需有 text、messages 或 prompt/completion。",
                ));
            }
        }
    }
    Ok(())
}
fn append_text(
    rows: &mut Vec<Value>,
    text: &str,
    group: &str,
    source: &Value,
    input: &Value,
) -> Result<(), GatewayError> {
    let size = input["chunkChars"].as_u64().unwrap_or(4000);
    if !(64..=131072).contains(&size) {
        return Err(invalid("chunkChars 需在 64 至 131072 之间。"));
    }
    let chars: Vec<_> = text.chars().collect();
    for (index, chunk) in chars.chunks(size as usize).enumerate() {
        let text: String = chunk.iter().collect();
        if text.trim().is_empty() {
            continue;
        }
        rows.push(json!({"text":text,"groupId":group,"source":source,"chunkIndex":index}));
        if rows.len() > MAX_SAMPLES {
            return Err(invalid("文本分块超过数据集样本容量。"));
        }
    }
    Ok(())
}

fn fingerprint(row: &Value) -> String {
    let mut value = row.clone();
    if let Some(value) = value.as_object_mut() {
        for key in ["source", "split", "groupId", "chunkIndex", "sourceRow"] {
            value.remove(key);
        }
    }
    text_hash(&value.to_string())
}
fn split_samples(
    rows: Vec<Value>,
    input: &Value,
) -> Result<(Vec<Value>, Value, usize), GatewayError> {
    let validation = input["split"]["validation"]
        .as_f64()
        .or_else(|| input["validationSplit"].as_f64())
        .unwrap_or(0.1);
    let test = input["split"]["test"]
        .as_f64()
        .or_else(|| input["testSplit"].as_f64())
        .unwrap_or(0.1);
    if !validation.is_finite()
        || !test.is_finite()
        || validation < 0.
        || test < 0.
        || validation + test >= 1.
    {
        return Err(invalid("验证集和测试集比例需非负，合计小于 1。"));
    }
    let seed = input["seed"].as_u64().unwrap_or(42);
    let mut unique = HashSet::new();
    let mut groups: HashMap<String, String> = HashMap::new();
    let mut explicit = HashSet::new();
    let mut output = Vec::new();
    let mut duplicates = 0;
    // An explicit split binds the entire source group; conflicting declarations fail visibly.
    for row in &rows {
        if let Some(split) = row["split"].as_str() {
            if !["train", "validation", "test"].contains(&split) {
                return Err(invalid("样本 split 无效。"));
            }
            let group = row["groupId"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| fingerprint(row));
            explicit.insert(group.clone());
            if groups
                .insert(group, split.to_owned())
                .is_some_and(|old| old != split)
            {
                return Err(invalid(
                    "同一文档或重复样本不能同时放入训练集与验证/测试集。",
                ));
            }
        }
    }
    let mut counts = json!({"train":0,"validation":0,"test":0});
    for mut row in rows {
        let key = fingerprint(&row);
        if !unique.insert(key.clone()) {
            duplicates += 1;
            continue;
        }
        let group = row["groupId"].as_str().map(str::to_owned).unwrap_or(key);
        let split = groups.entry(group.clone()).or_insert_with(|| {
            let digest = text_hash(&format!("{seed}:{group}"));
            let n = u64::from_str_radix(&digest[..12], 16).unwrap() as f64
                / 0x1_0000_0000_0000u64 as f64;
            (if n < test {
                "test"
            } else if n < test + validation {
                "validation"
            } else {
                "train"
            })
            .to_owned()
        });
        row["groupId"] = json!(group);
        row["split"] = json!(split.as_str());
        counts[split.as_str()] = json!(counts[split.as_str()].as_u64().unwrap_or(0) + 1);
        output.push(row);
    }
    if counts["train"].as_u64() == Some(0) {
        if let Some(group) = output
            .iter()
            .filter_map(|row| row["groupId"].as_str())
            .filter(|group| !explicit.contains(*group))
            .min()
            .map(str::to_owned)
        {
            for row in output.iter_mut().filter(|row| row["groupId"] == group) {
                let old = row["split"].as_str().unwrap().to_owned();
                counts[old.as_str()] = json!(counts[old.as_str()].as_u64().unwrap() - 1);
                row["split"] = json!("train");
                counts["train"] = json!(counts["train"].as_u64().unwrap() + 1);
            }
        }
    }
    if counts["train"].as_u64() == Some(0) {
        return Err(invalid(
            "当前划分没有训练样本，请调整比例、指定 split 或加入更多独立来源。",
        ));
    }
    Ok((output, counts, duplicates))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_handles_quoted_multiline_and_delimiters() {
        let rows = delimited("x,y,label\n\"hello,world\",2,A\n\"two\nlines\",3,B", ',').unwrap();
        assert_eq!(rows[1][0], "hello,world");
        assert_eq!(rows[2][0], "two\nlines");
        assert!(delimited("\"unfinished", ',').is_err());
    }
    #[test]
    fn document_chunks_keep_one_split_and_duplicates_do_not_leak() {
        let rows = vec![
            json!({"text":"one","groupId":"doc-a","split":"train"}),
            json!({"text":"two","groupId":"doc-a"}),
            json!({"text":"one","groupId":"doc-a"}),
        ];
        let (rows, counts, duplicates) = split_samples(rows, &json!({})).unwrap();
        assert_eq!(duplicates, 1);
        assert_eq!(counts["train"], 2);
        assert!(rows.iter().all(|row| row["split"] == "train"));
        assert!(
            split_samples(
                vec![
                    json!({"text":"same","split":"train"}),
                    json!({"text":"same","split":"test"})
                ],
                &json!({})
            )
            .is_err()
        );
    }
    #[test]
    fn chunks_preserve_unicode_and_original_content() {
        let text = "中😀文".repeat(50);
        let mut rows = Vec::new();
        append_text(
            &mut rows,
            &text,
            "source",
            &json!({}),
            &json!({"chunkChars":64}),
        )
        .unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r["text"].as_str().unwrap())
                .collect::<String>(),
            text
        );
    }
    #[test]
    fn table_selects_named_target_without_target_leakage() {
        let (rows, names) = table_samples(
            "amount,kind,target\n2,red,yes\n3,blue,no",
            ',',
            &json!({"targetColumn":"target"}),
            &json!({}),
        )
        .unwrap();
        assert_eq!(names, vec!["amount", "kind"]);
        assert_eq!(rows[0]["features"], json!([2., "red"]));
        assert_eq!(rows[0]["label"], "yes");
    }
}
