use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, atomic::{AtomicBool, Ordering}};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, imageops::FilterType};
use serde_json::{Value, json};

use super::gateway::{GatewayToolContext, WorkerCancellation, check_cancelled, failure};
use super::service_error::{ServiceError, ServiceResult};
use crate::gateway_contract::GatewayError;

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 64_000_000;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
static IMAGE_WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn active(cancellation: &AtomicBool) -> ServiceResult<()> {
    if cancellation.load(Ordering::Acquire) {
        return Err(ServiceError::new("TOOL_CANCELLED", "图片读取已取消。", 409));
    }
    Ok(())
}

fn image_error(error: image::ImageError) -> ServiceError {
    match error {
        image::ImageError::Limits(_) => ServiceError::new("IMAGE_DECODE_LIMIT", "图片解码超出资源上限，请先缩小原图。", 413),
        _ => ServiceError::new("IMAGE_INVALID_CONTENT", "图片无法解码，内容可能损坏或格式不受支持。", 422),
    }
}

fn crop_region(arguments: &Value, width: u32, height: u32) -> ServiceResult<Option<[u32; 4]>> {
    if arguments.get("crop").is_none() { return Ok(None); }
    let invalid = || ServiceError::new("INVALID_TOOL_ARGUMENTS", "crop 须为原图坐标内的非空矩形，不能越界。", 400);
    let mut region = [0_u32; 4];
    for (index, key) in ["x", "y", "width", "height"].iter().enumerate() {
        region[index] = arguments["crop"][key].as_u64().and_then(|value| u32::try_from(value).ok()).ok_or_else(invalid)?;
    }
    let [x, y, crop_width, crop_height] = region;
    if crop_width == 0 || crop_height == 0
        || x.checked_add(crop_width).is_none_or(|right| right > width)
        || y.checked_add(crop_height).is_none_or(|bottom| bottom > height) {
        return Err(invalid());
    }
    Ok(Some(region))
}

/// Limits precede decoding; coordinates refer to the original image after its orientation is applied.
/// 先限制解码资源；裁剪坐标对应应用方向信息后的原图。
fn image_projection(bytes: Vec<u8>, format: ImageFormat, arguments: &Value, cancellation: &AtomicBool) -> ServiceResult<Value> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(32768);
    limits.max_image_height = Some(32768);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(image_error)?;
    let (encoded_width, encoded_height) = decoder.dimensions();
    if encoded_width == 0 || encoded_height == 0
        || u64::from(encoded_width) * u64::from(encoded_height) > MAX_IMAGE_PIXELS
        || decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err(ServiceError::new("IMAGE_DECODE_LIMIT", "原图超过 6400 万像素或解码内存上限，请先缩小原图。", 413));
    }
    let orientation = decoder.orientation().map_err(image_error)?;
    active(cancellation)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(image_error)?;
    active(cancellation)?;
    image.apply_orientation(orientation);
    let (original_width, original_height) = (image.width(), image.height());
    let crop = crop_region(arguments, original_width, original_height)?;
    if let Some([x, y, width, height]) = crop {
        image = image.crop_imm(x, y, width, height);
    }
    let maximum = arguments["maxDimension"].as_u64().unwrap_or(2048) as u32;
    let resized = image.width() > maximum || image.height() > maximum;
    if resized { image = image.resize(maximum, maximum, FilterType::Triangle); }
    active(cancellation)?;
    let (width, height) = (image.width(), image.height());
    let source_hash = super::local_files::sha256(&bytes);
    let source_bytes = bytes.len();
    let transformed = crop.is_some() || resized || !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP);
    let (encoded, output_format) = if transformed {
        let output_format = if image.color().has_alpha() { ImageFormat::Png } else { ImageFormat::Jpeg };
        let image = if image.color().has_alpha() { DynamicImage::ImageRgba8(image.into_rgba8()) } else { DynamicImage::ImageRgb8(image.into_rgb8()) };
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, output_format).map_err(image_error)?;
        (output.into_inner(), output_format)
    } else { (bytes, format) };
    if encoded.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ServiceError::new("IMAGE_OUTPUT_LIMIT", "输出图片过大，请降低 maxDimension 或读取较小的 crop。", 413));
    }
    active(cancellation)?;
    let image_hash = super::local_files::sha256(&encoded);
    let data = STANDARD.encode(&encoded);
    active(cancellation)?;
    Ok(json!({"isError":false,"content":[{"type":"image","mimeType":output_format.to_mime_type(),"data":data}],
        "structuredContent":{"sha256":source_hash,"sizeBytes":source_bytes,"sourceMimeType":format.to_mime_type(),
            "originalWidth":original_width,"originalHeight":original_height,"width":width,"height":height,
            "mimeType":output_format.to_mime_type(),"imageSha256":image_hash,"imageBytes":encoded.len(),
            "crop":arguments.get("crop"),"resized":resized,"transformed":transformed,
            "frame":if format == ImageFormat::Gif { Some(0) } else { None }}}))
}

fn source_format(path: &Path) -> ServiceResult<ImageFormat> {
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
    Ok(match extension.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::WebP,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        _ => return Err(ServiceError::new("IMAGE_FORMAT_UNSUPPORTED", "直接看图支持 PNG/JPEG/GIF/WebP/BMP/TIFF；其他格式请先转换，不能只改扩展名。", 415)),
    })
}

fn project_source(bytes: Vec<u8>, format: ImageFormat, arguments: &Value, cancellation: &AtomicBool,
    conflict_code: &str) -> ServiceResult<Value> {
    active(cancellation)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ServiceError::new("IMAGE_FILE_TOO_LARGE", "图片必须非空且不超过 32 MiB。", 400));
    }
    let sha256 = super::local_files::sha256(&bytes);
    if arguments["expectedSha256"].as_str().is_some_and(|expected| expected != sha256) {
        return Err(ServiceError::new(conflict_code, "图片已变化，请读取当前版本。", 409));
    }
    if image::guess_format(&bytes).map_err(image_error)? != format {
        return Err(ServiceError::new("IMAGE_INVALID_CONTENT", "图片实际格式与扩展名不符，请使用正确的文件。", 422));
    }
    image_projection(bytes, format, arguments, cancellation)
}

/// Read the authorized version once; model media never creates a workspace preview.
/// 只读取已授权的同一文件版本；模型图像输入不会创建右侧预览。
fn read_image(path: PathBuf, binding: Value, arguments: Value, cancellation: &AtomicBool) -> ServiceResult<Value> {
    active(cancellation)?;
    let format = source_format(&path)?;
    super::service_filesystem::revalidate(&binding)?;
    let metadata = super::local_files::inspect(&path, false, true)?
        .filter(|metadata| metadata.is_file())
        .ok_or_else(|| ServiceError::new("INVALID_TOOL_ARGUMENTS", "图片路径不是普通文件。", 400))?;
    if metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return Err(ServiceError::new("IMAGE_FILE_TOO_LARGE", "图片必须非空且不超过 32 MiB。", 400));
    }
    let bytes = super::local_files::read_checked(&path, MAX_IMAGE_BYTES, true, "TOOL_FILE_CONFLICT")?;
    super::service_filesystem::revalidate(&binding)?;
    let mut result = project_source(bytes, format, &arguments, cancellation, "TOOL_FILE_CONFLICT")?;
    result["structuredContent"]["path"] = json!(super::local_files::path_text(&path));
    Ok(result)
}

fn validate_arguments(arguments: &Value) -> Result<(), GatewayError> {
    let expected = arguments["expectedSha256"].as_str().map(str::to_owned);
    if expected.as_ref().is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))) {
        return Err(failure("INVALID_TOOL_ARGUMENTS", "expectedSha256 必须是完整的小写 SHA-256。", 400));
    }
    if arguments.get("maxDimension").is_some() && !arguments["maxDimension"].as_u64().is_some_and(|value| (1..=4096).contains(&value)) {
        return Err(failure("INVALID_TOOL_ARGUMENTS", "maxDimension 必须在 1 到 4096 之间。", 400));
    }
    Ok(())
}

async fn image_worker<F>(context: &GatewayToolContext, work: F) -> Result<Value, GatewayError>
where F: FnOnce(&AtomicBool) -> ServiceResult<Value> + Send + 'static {
    check_cancelled(context)?;
    let mut cancellation = context.cancellation.clone();
    let workers = IMAGE_WORKERS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2))).clone();
    let permit = tokio::select! {
        permit = workers.acquire_owned() => permit.map_err(|_| failure("IMAGE_READER_UNAVAILABLE", "图片读取暂不可用。", 503))?,
        _ = super::gateway::wait_for_cancellation(&mut cancellation) => return Err(failure("TOOL_CANCELLED", "图片读取已取消。", 409)),
    };
    let guard = WorkerCancellation::new(context);
    let signal = guard.signal.clone();
    // The worker owns admission until decoding ends, even when the awaiting request is cancelled.
    // 读取者持有并发名额直到解码结束，等待方取消也不会提前放开资源。
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work(&signal)
    });
    let result = tokio::select! {
        result = worker => result.map_err(|_| failure("TOOL_WORKER_FAILED", "图片读取线程结束。", 500))?
            .map_err(|error| failure(&error.code, &error.message, error.status_code))?,
        _ = super::gateway::wait_for_cancellation(&mut cancellation) => return Err(failure("TOOL_CANCELLED", "图片读取已取消。", 409)),
    };
    check_cancelled(context)?;
    Ok(result)
}

pub(super) async fn read(arguments: Value, path: PathBuf, binding: Value, context: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(context)?;
    validate_arguments(&arguments)?;
    let mut result = image_worker(context, move |signal| read_image(path, binding, arguments, signal)).await?;
    result["structuredContent"]["mountId"] = context.config["activeMountId"].clone();
    result["structuredContent"]["mountName"] = context.config["activeMount"]["name"].clone();
    Ok(result)
}

fn read_saved_attachment(root: PathBuf, conversation: String, message: String, attachment: String,
    arguments: Value, cancellation: &AtomicBool) -> ServiceResult<Value> {
    active(cancellation)?;
    let (metadata, bytes) = super::gateway_attachment_read::read_image_original(
        &root, &conversation, &message, &attachment, MAX_IMAGE_BYTES, cancellation,
    ).map_err(|error| ServiceError::new(&error.code, &error.message, error.status))?;
    let format = source_format(Path::new(metadata["name"].as_str().unwrap_or("")))?;
    let mut result = project_source(bytes, format, &arguments, cancellation, "ATTACHMENT_VERSION_CHANGED")?;
    let structured = &mut result["structuredContent"];
    structured["sourceId"] = json!(format!("attachment:{conversation}:{message}:{attachment}"));
    structured["messageId"] = json!(message);
    structured["attachmentId"] = metadata["id"].clone();
    structured["sourceVersion"] = metadata["sha256"].clone();
    structured["name"] = metadata["name"].clone();
    Ok(result)
}

/// Read pixels from this conversation's immutable attachment, without granting filesystem access.
/// 按当前会话的正式附件身份读取原图像素，不授予私有数据目录或工作区路径访问。
pub(super) async fn read_attachment(arguments: Value, context: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(context)?;
    validate_arguments(&arguments)?;
    let message = arguments["messageId"].as_str().filter(|id| !id.is_empty());
    let attachment = arguments["attachmentId"].as_str().filter(|id| !id.is_empty());
    if arguments.get("path").is_some() || arguments.get("mountId").is_some() || message.is_none() || attachment.is_none() {
        return Err(failure("INVALID_TOOL_ARGUMENTS", "请选择图片路径，或提供同一消息的 messageId 与 attachmentId。", 400));
    }
    let (message, attachment) = (message.unwrap().to_owned(), attachment.unwrap().to_owned());
    let root = context.data_home.clone();
    let conversation = context.conversation_id.clone();
    image_worker(context, move |signal| read_saved_attachment(root, conversation, message, attachment, arguments, signal)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::gateway_attachment_read::{test_save_image_attachment as saved_fixture, test_reserve_image_attachment};

    fn fixture(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, _| {
            if x < width / 2 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) }
        }));
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, format).unwrap();
        encoded.into_inner()
    }

    #[tokio::test]
    async fn saved_attachment_returns_original_pixels_and_crop_without_private_paths_or_workspace_writes() {
        let (context, _cancel) = super::super::gateway::test_context();
        let bytes = fixture(ImageFormat::Png, 80, 40);
        let hash = super::super::local_files::sha256(&bytes);
        let (message, attachment) = saved_fixture(&context, "原图.png", &bytes);
        let arguments = json!({"messageId":message,"attachmentId":attachment,"expectedSha256":hash});
        let result = read_attachment(arguments.clone(), &context).await.unwrap();
        assert_eq!(STANDARD.decode(result["content"][0]["data"].as_str().unwrap()).unwrap(), bytes);
        assert_eq!(result["structuredContent"]["sourceVersion"], hash);
        assert_eq!(result["structuredContent"]["sha256"], hash);
        assert_eq!(result["structuredContent"]["messageId"], message);
        assert_eq!(result["structuredContent"]["attachmentId"], attachment);
        assert_eq!(result["structuredContent"]["name"], "原图.png");
        assert_eq!(result["structuredContent"]["sourceId"], format!("attachment:{}:{message}:{attachment}", context.conversation_id));
        for field in ["path", "mountId", "presentation", "previewStatus", "imagePreview", "contentUrl"] {
            assert!(result.get(field).is_none());
            assert!(result["structuredContent"].get(field).is_none());
        }
        let mut crop_arguments = arguments;
        crop_arguments["crop"] = json!({"x":40,"y":0,"width":40,"height":40});
        crop_arguments["maxDimension"] = json!(20);
        let cropped = read_attachment(crop_arguments, &context).await.unwrap();
        let sent = STANDARD.decode(cropped["content"][0]["data"].as_str().unwrap()).unwrap();
        let image = image::load_from_memory(&sent).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (20, 20));
        assert!(image.get_pixel(10, 10)[2] > 240 && image.get_pixel(10, 10)[0] < 20);
        assert_eq!(cropped["structuredContent"]["sourceVersion"], hash);
        assert_eq!(super::super::gateway_attachment_read::read_image_original(&context.data_home, &context.conversation_id,
            &message, &attachment, MAX_IMAGE_BYTES, &AtomicBool::new(false)).unwrap().1, bytes);
        assert!(context.runtime.preview_grants.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&context.workdir).unwrap().count(), 0);
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn saved_attachment_checks_scope_version_format_size_arguments_and_cancellation() {
        let (context, cancel) = super::super::gateway::test_context();
        let bytes = fixture(ImageFormat::Png, 2, 1);
        let (message, attachment) = saved_fixture(&context, "photo.png", &bytes);
        for (extra, code) in [
            (json!({"expectedSha256":"0".repeat(64)}), "ATTACHMENT_VERSION_CHANGED"),
            (json!({"expectedSha256":"invalid"}), "INVALID_TOOL_ARGUMENTS"),
            (json!({"maxDimension":0}), "INVALID_TOOL_ARGUMENTS"),
            (json!({"path":"photo.png"}), "INVALID_TOOL_ARGUMENTS"),
        ] {
            let mut arguments = json!({"messageId":message,"attachmentId":attachment});
            arguments.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            assert_eq!(read_attachment(arguments, &context).await.unwrap_err().code, code);
        }
        let mut foreign = context.clone();
        foreign.conversation_id = uuid::Uuid::new_v4().to_string();
        let (foreign_message, foreign_attachment) = saved_fixture(&foreign, "foreign.png", &bytes);
        assert_eq!(read_attachment(json!({"messageId":foreign_message,"attachmentId":foreign_attachment}), &context)
            .await.unwrap_err().code, "MESSAGE_NOT_FOUND");
        for (name, source, code) in [
            ("wrong.jpg", bytes.clone(), "IMAGE_INVALID_CONTENT"),
            ("broken.png", b"not an image".to_vec(), "IMAGE_INVALID_CONTENT"),
            ("vector.svg", b"<svg/>".to_vec(), "IMAGE_FORMAT_UNSUPPORTED"),
        ] {
            let (message, attachment) = saved_fixture(&context, name, &source);
            assert_eq!(read_attachment(json!({"messageId":message,"attachmentId":attachment}), &context)
                .await.unwrap_err().code, code);
        }
        let (oversized_message, oversized_attachment) = test_reserve_image_attachment(&context, "large.png", MAX_IMAGE_BYTES + 1);
        assert_eq!(read_attachment(json!({"messageId":oversized_message,"attachmentId":oversized_attachment}), &context)
            .await.unwrap_err().code, "IMAGE_FILE_TOO_LARGE");
        cancel.send(true).unwrap();
        assert_eq!(read_attachment(json!({"messageId":message,"attachmentId":attachment}), &context)
            .await.unwrap_err().code, "TOOL_CANCELLED");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn local_image_returns_exact_pixels_without_opening_workspace() {
        let (context, _cancel) = super::super::gateway::test_context();
        let bytes = fixture(ImageFormat::Png, 2, 1);
        let hash = super::super::local_files::sha256(&bytes);
        std::fs::write(context.workdir.join("photo.png"), &bytes).unwrap();
        let result = super::super::gateway::execute("image.read", json!({"path":"photo.png","expectedSha256":hash}), &context).await.unwrap();
        assert_eq!(STANDARD.decode(result["content"][0]["data"].as_str().unwrap()).unwrap(), bytes);
        assert_eq!(result["structuredContent"]["sha256"], hash);
        assert_eq!(result["structuredContent"]["width"], 2);
        assert_eq!(result["structuredContent"]["height"], 1);
        assert_eq!(result["content"][0]["mimeType"], "image/png");
        for field in ["presentation", "previewStatus", "imagePreview", "contentUrl"] {
            assert!(result.get(field).is_none());
            assert!(result["structuredContent"].get(field).is_none());
        }
        assert!(context.runtime.preview_grants.lock().unwrap().is_empty());
        let mut scoped = context.clone();
        scoped.config["workspaceMounts"] = json!([]);
        assert_eq!(super::super::gateway::execute("image.read", json!({"path":"photo.png","mountId":"missing-mount"}), &scoped)
            .await.unwrap_err().code, "WORKSPACE_MOUNT_UNAVAILABLE");
        let conflict = super::super::gateway::execute("image.read", json!({"path":"photo.png","expectedSha256":"0".repeat(64)}), &context).await.unwrap_err();
        assert_eq!(conflict.code, "TOOL_FILE_CONFLICT");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn local_image_checks_content_limits_authorization_and_cancellation() {
        let (context, cancel) = super::super::gateway::test_context();
        let bytes = fixture(ImageFormat::Png, 2, 1);
        std::fs::write(context.workdir.join("wrong.jpg"), &bytes).unwrap();
        std::fs::write(context.workdir.join("broken.png"), b"not an image").unwrap();
        std::fs::write(context.workdir.join("unsupported.svg"), b"<svg/>").unwrap();
        let oversized = std::fs::File::create(context.workdir.join("large.png")).unwrap();
        oversized.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        drop(oversized);
        for (path, code) in [("wrong.jpg", "IMAGE_INVALID_CONTENT"), ("broken.png", "IMAGE_INVALID_CONTENT"),
            ("unsupported.svg", "IMAGE_FORMAT_UNSUPPORTED"), ("large.png", "IMAGE_FILE_TOO_LARGE")] {
            assert_eq!(super::super::gateway::execute("image.read", json!({"path":path}), &context).await.unwrap_err().code, code, "{path}");
        }
        let outside = context.workdir.parent().unwrap().join("outside.png");
        std::fs::write(&outside, &bytes).unwrap();
        let approval = super::super::gateway::execute("image.read", json!({"path":outside}), &context).await.unwrap();
        assert_eq!(approval["requiresApproval"], true);
        assert!(approval.get("content").is_none());
        let protected = context.data_home.join("private.png");
        std::fs::write(&protected, &bytes).unwrap();
        assert_eq!(super::super::gateway::execute("image.read", json!({"path":protected}), &context).await.unwrap_err().code, "PROTECTED_APP_DATA");
        cancel.send(true).unwrap();
        assert_eq!(super::super::gateway::execute("image.read", json!({"path":"wrong.jpg"}), &context).await.unwrap_err().code, "TOOL_CANCELLED");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[test]
    fn local_image_rejects_changed_path_binding_and_cancelled_worker() {
        let root = std::env::temp_dir().join(format!("kynxa-image-binding-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("first.png");
        let second = root.join("second.png");
        let bytes = fixture(ImageFormat::Png, 2, 1);
        std::fs::write(&first, &bytes).unwrap();
        std::fs::write(&second, &bytes).unwrap();
        let target = super::super::service_filesystem::canonical(&second, false).unwrap();
        let binding = json!({"requestedPath":first,"path":super::super::local_files::path_text(&target),"allowHardLinks":true});
        assert_eq!(read_image(target.clone(), binding.clone(), json!({}), &AtomicBool::new(false)).unwrap_err().code, "TOOL_PATH_CHANGED");
        assert_eq!(read_image(target, binding, json!({}), &AtomicBool::new(true)).unwrap_err().code, "TOOL_CANCELLED");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn local_image_supported_formats_crop_and_resize_keep_source_and_sent_versions() {
        let (context, _cancel) = super::super::gateway::test_context();
        for (extension, format) in [("png", ImageFormat::Png), ("jpg", ImageFormat::Jpeg), ("gif", ImageFormat::Gif), ("webp", ImageFormat::WebP), ("bmp", ImageFormat::Bmp), ("tiff", ImageFormat::Tiff)] {
            let bytes = fixture(format, 80, 40);
            let original_hash = super::super::local_files::sha256(&bytes);
            let path = context.workdir.join(format!("photo.{extension}"));
            std::fs::write(&path, &bytes).unwrap();
            let result = super::super::gateway::execute("image.read", json!({"path":path,"maxDimension":20,
                "crop":{"x":40,"y":0,"width":40,"height":40},"expectedSha256":original_hash}), &context).await.unwrap();
            let sent = STANDARD.decode(result["content"][0]["data"].as_str().unwrap()).unwrap();
            let decoded = image::load_from_memory(&sent).unwrap().to_rgb8();
            assert_eq!((decoded.width(), decoded.height()), (20, 20));
            assert!(decoded.get_pixel(10, 10)[2] > 240, "{extension}");
            assert!(decoded.get_pixel(10, 10)[0] < 20, "{extension}");
            assert_eq!(result["structuredContent"]["originalWidth"], 80);
            assert_eq!(result["structuredContent"]["originalHeight"], 40);
            assert_eq!(result["structuredContent"]["sha256"], original_hash);
            assert_eq!(result["structuredContent"]["imageSha256"], super::super::local_files::sha256(&sent));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        assert_eq!(super::super::gateway::execute("image.read", json!({"path":"photo.png","crop":{"x":79,"y":0,"width":2,"height":1}}), &context)
            .await.unwrap_err().code, "INVALID_TOOL_ARGUMENTS");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }

    #[test]
    fn local_image_rejects_oversized_decoded_dimensions_before_pixels() {
        let mut bytes = fixture(ImageFormat::Gif, 2, 1);
        bytes[6..10].copy_from_slice(&[255, 255, 255, 255]);
        let error = image_projection(bytes, ImageFormat::Gif, &json!({}), &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.code, "IMAGE_DECODE_LIMIT");
    }
}
