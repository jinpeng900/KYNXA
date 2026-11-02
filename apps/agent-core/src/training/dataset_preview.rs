//! 工作台只预览当前归属的冻结副本，不修改训练样本。
use super::{invalid, store};
use crate::{
    gateway_contract::GatewayError,
    storage::managed_files,
    tools::gateway::{self, GatewayToolContext},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
};

const IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const DECODE_BYTES: u64 = 256 * 1024 * 1024;
const PAGE_BYTES: usize = 512 * 1024;
static PREVIEW_WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

async fn run<T: Send + 'static>(
    ctx: &GatewayToolContext,
    work: impl FnOnce(GatewayToolContext) -> Result<T, GatewayError> + Send + 'static,
) -> Result<T, GatewayError> {
    gateway::check_cancelled(ctx)?;
    let permit = PREVIEW_WORKERS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| invalid("资料预览暂不可用。"))?;
    let context = ctx.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        gateway::check_cancelled(&context)?;
        work(context)
    })
    .await
    .map_err(|_| invalid("资料预览未完成，请重试。"))?
}

fn dataset(input: &Value, ctx: &GatewayToolContext) -> Result<(PathBuf, Value), GatewayError> {
    let id = store::id(input, "datasetId")?;
    let directory = store::scope_dir(ctx).join("datasets").join(id);
    let data = store::read(ctx, &directory.join("dataset.json"), store::RECORD_LIMIT)?;
    if !data["samples"].is_array() {
        return Err(invalid("数据集没有可预览的样本。"));
    }
    Ok((directory, data))
}

fn safe_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn image_source(
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<(Vec<u8>, String), GatewayError> {
    if input.get("path").is_some()
        || input.get("importId").is_some() == input.get("datasetId").is_some()
    {
        return Err(invalid("请选择一个导入文件，或指定数据集中的图片样本。"));
    }
    let (path, expected, name) = if input.get("importId").is_some() {
        let directory = store::scope_dir(ctx)
            .join("imports")
            .join(store::id(input, "importId")?);
        let source = store::read(ctx, &directory.join("source.json"), 65536)?;
        let name = source["name"]
            .as_str()
            .filter(|name| safe_name(name))
            .ok_or_else(|| invalid("资料名称无效。"))?;
        (
            directory.join(name),
            source["sha256"].as_str().unwrap_or("").to_owned(),
            name.to_owned(),
        )
    } else {
        let (directory, data) = dataset(input, ctx)?;
        let index = input["sampleIndex"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| invalid("请选择有效的图片样本。"))?;
        let sample = data["samples"]
            .as_array()
            .and_then(|rows| rows.get(index))
            .ok_or_else(|| invalid("图片样本不存在。"))?;
        let path = PathBuf::from(
            sample["imagePath"]
                .as_str()
                .ok_or_else(|| invalid("这个样本不是图片。"))?,
        );
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|name| safe_name(name))
            .ok_or_else(|| invalid("图片记录路径无效。"))?;
        if path != directory.join("assets").join(name) {
            return Err(invalid("图片不属于这个数据集版本。"));
        }
        let expected = sample["source"]["sha256"]
            .as_str()
            .ok_or_else(|| invalid("图片记录缺少完整性信息。"))?
            .to_owned();
        let title = sample["source"]["name"].as_str().unwrap_or(name).to_owned();
        (path, expected, title)
    };
    let bytes = managed_files::read(&ctx.data_home, &path, IMAGE_BYTES)?
        .ok_or_else(|| invalid("图片副本已丢失。"))?;
    if expected.is_empty() || managed_files::digest(&bytes) != expected {
        return Err(GatewayError::new(
            "TRAINING_SOURCE_CHANGED",
            "图片副本已变化，请重新导入。",
            409,
        ));
    }
    Ok((bytes, name))
}

fn thumbnail(
    bytes: Vec<u8>,
    name: String,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let format = image::guess_format(&bytes).map_err(|_| invalid("图片格式无法识别。"))?;
    if !matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Tiff
    ) {
        return Err(invalid("这类图片暂不支持预览。"));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(32768);
    limits.max_image_height = Some(32768);
    limits.max_alloc = Some(DECODE_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| invalid("图片无法解码或超过预览内存上限。"))?;
    let (width, height) = decoder.dimensions();
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > 64_000_000
        || decoder.total_bytes() > DECODE_BYTES
    {
        return Err(invalid("图片超过预览大小上限，请缩小后导入。"));
    }
    let orientation = decoder
        .orientation()
        .map_err(|_| invalid("图片方向信息无效。"))?;
    gateway::check_cancelled(ctx)?;
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|_| invalid("图片内容损坏或无法解码。"))?;
    image.apply_orientation(orientation);
    let (original_width, original_height) = (image.width(), image.height());
    let image = image.thumbnail(384, 384).into_rgba8();
    gateway::check_cancelled(ctx)?;
    let (width, height) = image.dimensions();
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|_| invalid("缩略图生成失败。"))?;
    if output.get_ref().len() > 2 * 1024 * 1024 {
        return Err(invalid("缩略图超过输出上限。"));
    }
    gateway::check_cancelled(ctx)?;
    Ok(
        json!({"name":name,"dataUrl":format!("data:image/png;base64,{}",STANDARD.encode(output.into_inner())),
        "mimeType":"image/png","width":width,"height":height,"originalWidth":original_width,"originalHeight":original_height,
        "sha256":managed_files::digest(&bytes)}),
    )
}

pub(super) async fn source_preview(
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let input = input.clone();
    run(ctx, move |ctx| {
        let (bytes, name) = image_source(&input, &ctx)?;
        thumbnail(bytes, name, &ctx)
    })
    .await
}

fn text(value: &Value, maximum: usize) -> Value {
    value
        .as_str()
        .map(|s| json!(s.chars().take(maximum).collect::<String>()))
        .unwrap_or(Value::Null)
}
fn cell(value: &Value) -> Value {
    if value.is_string() {
        text(value, 1024)
    } else if value.is_number() || value.is_boolean() {
        value.clone()
    } else {
        Value::Null
    }
}
fn nonempty(value: &Value) -> bool {
    !value.is_null() && !value.as_str().is_some_and(|s| s.trim().is_empty())
}

fn source_location(source: &Value, ctx: &GatewayToolContext) -> Value {
    let mut output = json!({});
    for key in ["name", "importId", "sha256", "sheetName", "kind"] {
        if let Some(value) = source.get(key) {
            output[key] = text(value, 1024);
        }
    }
    let path = source["originalPath"]
        .as_str()
        .or_else(|| source["path"].as_str());
    if let Some(path) = path {
        if !gateway::protected_path(ctx, Path::new(path)) {
            output["path"] = json!(path.chars().take(4096).collect::<String>());
        }
        if output["name"].is_null() {
            output["name"] = json!(
                Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
        }
    }
    output
}

fn preview_row(row: &Value, index: usize, ctx: &GatewayToolContext) -> Value {
    let mut output = json!({"index":index,"source":source_location(&row["source"],ctx),"hasImage":row["imagePath"].is_string()});
    for key in ["split", "label", "targetName", "sourceRow", "chunkIndex"] {
        if let Some(value) = row.get(key) {
            output[key] = cell(value);
        }
    }
    for key in ["text", "prompt", "completion"] {
        if let Some(value) = row.get(key) {
            output[key] = text(value, 10000);
            if value.as_str().is_some_and(|s| s.chars().count() > 10000) {
                output["textTruncated"] = json!(true);
            }
        }
    }
    if let Some(features) = row["features"].as_array() {
        output["features"] = json!(features.iter().take(64).map(cell).collect::<Vec<_>>());
        output["featureCount"] = json!(features.len());
        output["featuresTruncated"] = json!(features.len() > 64);
    }
    if let Some(messages) = row["messages"].as_array() {
        output["messages"] = json!(
            messages
                .iter()
                .take(12)
                .map(|m| json!({"role":text(&m["role"],30),"content":text(&m["content"],2000)}))
                .collect::<Vec<_>>()
        );
        output["messagesTruncated"] = json!(
            messages.len() > 12
                || messages.iter().any(|m| m["content"]
                    .as_str()
                    .is_some_and(|s| s.chars().count() > 2000))
        );
    }
    output
}

fn statistics(
    data: &Value,
    rows: &[Value],
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let (
        mut missing_labels,
        mut missing_descriptions,
        mut empty_content,
        mut missing_feature_cells,
    ) = (0, 0, 0, 0);
    for (index, row) in rows.iter().enumerate() {
        if index % 1024 == 0 {
            gateway::check_cancelled(ctx)?;
        }
        if (data["kind"] == "image" || data["kind"] == "tabular") && !nonempty(&row["label"]) {
            missing_labels += 1;
        }
        let has_text = ["text", "prompt", "completion"]
            .iter()
            .any(|key| row[*key].as_str().is_some_and(|s| !s.trim().is_empty()))
            || row["messages"].as_array().is_some_and(|messages| {
                messages
                    .iter()
                    .any(|m| m["content"].as_str().is_some_and(|s| !s.trim().is_empty()))
            });
        if data["kind"] == "image" && !has_text {
            missing_descriptions += 1;
        }
        if let Some(features) = row["features"].as_array() {
            missing_feature_cells += features.iter().filter(|v| !nonempty(v)).count();
            if features.iter().all(|v| !nonempty(v)) {
                empty_content += 1;
            }
        } else if !row["imagePath"].is_string() && !has_text {
            empty_content += 1;
        }
    }
    Ok(
        json!({"sampleCount":rows.len(),"missingLabels":missing_labels,"missingDescriptions":missing_descriptions,
        "emptyContent":empty_content,"missingFeatureCells":missing_feature_cells,"duplicatesRemoved":data["duplicatesRemoved"],
        "emptyRemoved":Value::Null,"splitCounts":data["splitCounts"]}),
    )
}

pub(super) async fn read(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let input = input.clone();
    run(ctx, move |ctx| {
        let (_, data) = dataset(&input, &ctx)?;
        let samples = data["samples"].as_array().ok_or_else(||invalid("数据集记录无效。"))?;
        let offset = input["offset"].as_u64().unwrap_or(0).min(samples.len() as u64) as usize;
        let limit = input["limit"].as_u64().unwrap_or(20).clamp(1,50) as usize;
        let mut rows = Vec::new(); let mut size=0;
        for (index,row) in samples.iter().enumerate().skip(offset).take(limit) {
            gateway::check_cancelled(&ctx)?;
            let item=preview_row(row,index,&ctx); let bytes=serde_json::to_vec(&item).map_err(|_|invalid("样本预览无法编码。"))?.len();
            if size+bytes>PAGE_BYTES && !rows.is_empty() {break;}
            size+=bytes;rows.push(item);
        }
        let next=offset+rows.len();
        let mut summary=json!({});
        for key in ["id","version","name","kind","task","createdAt","sampleCount","sourceCount","splitCounts","duplicatesRemoved","warnings"] {summary[key]=data[key].clone();}
        let names=data["featureNames"].as_array().map(|names|names.iter().take(64).map(|name|text(name,256)).collect::<Vec<_>>()).unwrap_or_default();
        let target=samples.iter().find_map(|row|row["targetName"].as_str());
        Ok(json!({"dataset":summary,"rows":rows,"offset":offset,"limit":limit,"total":samples.len(),
            "nextOffset":if next<samples.len(){Some(next)}else{None},"statistics":statistics(&data,samples,&ctx)?,
            "featureNames":names,"targetName":target,"featureNamesTruncated":data["featureNames"].as_array().is_some_and(|names|names.len()>64)}))
    }).await
}
