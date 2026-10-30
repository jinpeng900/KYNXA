//! Binary media and 3D previews reuse exact file bindings and preview grants.
use super::gateway::{GatewayToolContext, check_cancelled, failure};
use super::gateway_workspace_preview::{PreviewGrant, WorkspaceContent, checked_bytes, content_url, extension};
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::{collections::HashSet, path::{Path, PathBuf}, process::Stdio, sync::OnceLock, time::Duration};

pub(crate) fn format(path: &Path) -> Option<(&'static str, &'static str)> {
    Some(match extension(path).as_str() {
        "glb" => ("model3d", "model/gltf-binary"),
        "gltf" => ("model3d", "model/gltf+json"),
        "obj" => ("model3d", "model/obj"),
        "stl" => ("model3d", "model/stl"),
        "blend" => ("model3d", "application/x-blender"),
        "mp3" => ("audio", "audio/mpeg"),
        "wav" | "wave" => ("audio", "audio/wav"),
        "ogg" | "oga" => ("audio", "audio/ogg"),
        "opus" => ("audio", "audio/ogg; codecs=opus"),
        "m4a" | "m4b" => ("audio", "audio/mp4"),
        "aac" => ("audio", "audio/aac"),
        "flac" => ("audio", "audio/flac"),
        "aif" | "aiff" => ("audio", "audio/aiff"),
        "mp4" | "m4v" => ("video", "video/mp4"),
        "webm" => ("video", "video/webm"),
        "ogv" => ("video", "video/ogg"),
        "mov" => ("video", "video/quicktime"),
        "mkv" => ("video", "video/x-matroska"),
        "avi" => ("video", "video/x-msvideo"),
        _ => return None,
    })
}

pub(super) fn dependency_mime(path: &Path) -> &'static str {
    match extension(path).as_str() {
        "bin" => "application/octet-stream",
        "mtl" => "text/plain; charset=utf-8",
        "ktx2" => "image/ktx2",
        "dds" => "image/vnd-ms.dds",
        "tga" => "image/x-tga",
        _ => super::gateway_workspace_preview::format(path).1,
    }
}

fn manifest_uris(path: &Path, bytes: &[u8]) -> Result<Vec<String>, GatewayError> {
    let invalid = || failure("MODEL_MANIFEST_INVALID", "3D 文件依赖清单无法解析。", 422);
    match extension(path).as_str() {
        "gltf" | "glb" => {
            let json_bytes = if extension(path) == "glb" {
                if bytes.len() < 20 || &bytes[..4] != b"glTF" || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != 2
                    || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != bytes.len()
                    || &bytes[16..20] != b"JSON" { return Err(invalid()); }
                let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
                bytes.get(20..20usize.checked_add(length).ok_or_else(invalid)?).ok_or_else(invalid)?
            } else { bytes };
            if json_bytes.len() > 16 * 1024 * 1024 { return Err(invalid()); }
            let document: Value = serde_json::from_slice(json_bytes).map_err(|_| invalid())?;
            let mut uris = Vec::new();
            for group in ["buffers", "images"] {
                for entry in document[group].as_array().into_iter().flatten() {
                    if let Some(uri) = entry["uri"].as_str() {
                        if !uri.starts_with("data:") { uris.push(uri.to_owned()); }
                    }
                }
            }
            // Unknown vendor extensions may contain arbitrary resources. Loaders must only use this allowlist.
            Ok(uris)
        }
        "obj" | "mtl" => {
            let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
            let mut uris = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                let Some((keyword, remainder)) = line.split_once(char::is_whitespace) else { continue; };
                if keyword == "mtllib" {
                    // OBJ permits multiple libraries; quoted names are accepted as one path.
                    let remainder = remainder.trim();
                    if remainder.starts_with('"') && remainder.ends_with('"') { uris.push(remainder.trim_matches('"').to_owned()); }
                    else { uris.extend(remainder.split_whitespace().map(str::to_owned)); }
                } else if extension(path) == "mtl" && (keyword.starts_with("map_") || matches!(keyword, "bump" | "disp" | "decal" | "norm" | "refl")) {
                    let remainder = remainder.trim();
                    // Avoid guessing option grammar and accidentally authorizing a different asset.
                    if remainder.starts_with('-') { return Err(failure("MODEL_MTL_OPTIONS_UNSUPPORTED", "材质贴图含选项，请导出为 GLB 或使用无选项的贴图路径。", 422)); }
                    uris.push(remainder.trim_matches('"').to_owned());
                }
            }
            Ok(uris)
        }
        _ => Ok(Vec::new()),
    }
}

/// Immutable uploads have attachment ownership, never neighboring filesystem authority.
pub(crate) fn attachment_metadata(path: &Path, bytes: &[u8]) -> Result<Option<Value>, GatewayError> {
    let Some((kind, mime)) = format(path) else { return Ok(None); };
    let mut result = json!({"kind":kind,"mimeType":mime,"sourceFormat":extension(path),"renderFormat":extension(path),"renderable":true});
    if kind == "model3d" {
        let uris = manifest_uris(path, bytes)?;
        let dependencies = uris.into_iter().map(|uri|json!({"uri":uri,"available":false,"reason":"附件未授予邻接文件读取权限；请将模型及依赖放入已授权工作目录。"})).collect::<Vec<_>>();
        let ready = dependencies.is_empty() && extension(path) != "blend";
        result["dependencies"] = json!(dependencies); result["renderable"] = json!(ready);
        result["dependencyStatus"] = json!(if ready {"ready"}else{"blocked"});
        if !ready { result["notice"] = json!("此模型包含外部依赖或需要 Blender 转换，请在已授权工作目录打开，或上传打包后的 GLB。"); }
    }
    Ok(Some(result))
}

fn dependency_path(root: &Path, parent: &Path, uri: &str) -> Result<PathBuf, GatewayError> {
    let denied = || failure("MODEL_DEPENDENCY_FORBIDDEN", "3D 依赖必须是模型目录内的本地相对文件，不能使用远程地址或目录逃逸。", 403);
    if uri.is_empty() || uri.len() > 4096 || uri.contains(['\\', ':', '?', '#', '\0', '\r', '\n']) || uri.starts_with('/') { return Err(denied()); }
    let base = url::Url::from_directory_path(parent).map_err(|_| denied())?;
    let target = base.join(uri).map_err(|_| denied())?;
    let target = target.to_file_path().map_err(|_| denied())?;
    let canonical = super::service_filesystem::canonical(&target, false)
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    if !super::local_files::within(root, &canonical) { return Err(denied()); }
    Ok(canonical)
}

pub(super) async fn model_preview(mut result: Value, path: &Path, bytes: &[u8], context: &GatewayToolContext) -> Result<Value, GatewayError> {
    let source_format = extension(path);
    result["sourceFormat"] = json!(source_format);
    result["renderFormat"] = json!(source_format);
    result["dependencies"] = json!([]);
    result["renderable"] = json!(true);
    result["dependencyStatus"] = json!("ready");
    if source_format == "blend" {
        match blend_glb(path, result["version"].as_str().unwrap_or(""), bytes, context).await {
            Ok(Some(_)) => {
                result["renderFormat"] = json!("glb");
                result["mimeType"] = json!("model/gltf-binary");
                result["contentUrl"] = json!(format!("{}&representation=glb", content_url(context, path, result["version"].as_str().unwrap_or(""), false)));
            }
            Err(error) if error.code == "TOOL_CANCELLED" => return Err(error),
            other => {
                result["renderable"] = json!(false);
                result["dependencyStatus"] = json!("blocked");
                result["notice"] = json!(match other { Err(error) => error.message, _ => "未找到本机 Blender，安装后可生成派生 GLB 预览；源 .blend 不会改写。".into() });
            }
        }
        return Ok(result);
    }
    let root = path.parent().ok_or_else(|| failure("MODEL_PATH_INVALID", "模型路径缺少目录。", 400))?;
    let mut pending = match manifest_uris(path, bytes) {
        Ok(uris) => uris.into_iter().map(|uri| (path.to_path_buf(), uri)).collect::<Vec<_>>(),
        Err(error) => {
            result["renderable"] = json!(false); result["dependencyStatus"] = json!("blocked"); result["notice"] = json!(error.message);
            return Ok(result);
        }
    };
    let mut dependencies = Vec::new();
    let mut seen = HashSet::new();
    let mut consumed = bytes.len();
    while let Some((parent, uri)) = pending.pop() {
        check_cancelled(context)?;
        if dependencies.len() >= 128 { return Err(failure("MODEL_DEPENDENCY_LIMIT", "模型依赖超过 128 项，请导出为 GLB。", 413)); }
        let mut entry = json!({"uri":uri,"parentUri":parent.strip_prefix(root).unwrap_or(&parent).to_string_lossy().replace('\\', "/"),"available":false});
        let resolved = dependency_path(root, parent.parent().unwrap_or(root), &uri);
        let attempt = async {
            let dependency = resolved?;
            if !seen.insert(dependency.clone()) { return Ok::<_, GatewayError>(None); }
            let mut scoped = context.clone(); scoped.approved = false;
            let receipt = Box::pin(super::gateway::filesystem("filesystem.stat", json!({"path":dependency}), &scoped)).await?;
            if receipt["requiresApproval"] == true { return Err(failure("MODEL_DEPENDENCY_APPROVAL_REQUIRED", "此依赖尚未单独授权；请将模型与依赖移入已授权工作目录。", 403)); }
            let binding = json!({"requestedPath":dependency,"path":dependency,"allowMissing":false,"allowHardLinks":true});
            let source = dependency.clone();
            let asset = tokio::task::spawn_blocking(move || checked_bytes(&source, &binding)).await
                .map_err(|_| failure("PREVIEW_WORKER_FAILED", "模型依赖读取未完成。", 500))??;
            consumed = consumed.saturating_add(asset.len());
            if consumed > 256 * 1024 * 1024 { return Err(failure("MODEL_DEPENDENCY_LIMIT", "模型与依赖合计超过预览容量。", 413)); }
            let version = super::local_files::sha256(&asset);
            let nested = manifest_uris(&dependency, &asset)?;
            for nested_uri in nested { pending.push((dependency.clone(), nested_uri)); }
            let token = uuid::Uuid::new_v4().to_string();
            let mut grants = context.runtime.preview_grants.lock().map_err(|_| failure("PREVIEW_STATE_FAILED", "预览许可状态不可用。", 500))?;
            if grants.len() >= 4096 { return Err(failure("MODEL_DEPENDENCY_LIMIT", "预览许可已达容量，请关闭旧预览后重试。", 413)); }
            grants.insert(token.clone(), PreviewGrant { conversation:context.conversation_id.clone(), path:dependency.clone(), version:version.clone(), dependency:true });
            Ok(Some((dependency, version, token)))
        }.await;
        match attempt {
            Ok(Some((dependency, version, token))) => {
                entry["path"] = json!(super::local_files::path_text(&dependency));
                entry["resolvedUri"] = json!(dependency.strip_prefix(root).unwrap_or(&dependency).to_string_lossy().replace('\\', "/"));
                entry["mimeType"] = json!(dependency_mime(&dependency)); entry["version"] = json!(version);
                entry["contentUrl"] = json!(format!("{}&grant={token}", content_url(context, &dependency, &version, false)));
                entry["available"] = json!(true);
            }
            Ok(None) => continue,
            Err(error) if error.code == "TOOL_CANCELLED" => return Err(error),
            Err(error) => { entry["code"] = json!(error.code); entry["reason"] = json!(error.message); result["dependencyStatus"] = json!("blocked"); result["renderable"] = json!(false); }
        }
        dependencies.push(entry);
    }
    result["dependencies"] = json!(dependencies);
    if result["renderable"] == true { result["contentUrl"] = json!(content_url(context, path, result["version"].as_str().unwrap_or(""), false)); }
    else { result["notice"] = json!("模型依赖缺失或未获授权；请修复依赖清单后重新预览。"); }
    Ok(result)
}

fn blender() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KYNXA_BLENDER_EXE").map(PathBuf::from).filter(|path| path.is_file()) {
        return Some(path);
    }
    let mut candidates = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") { candidates.extend(std::env::split_paths(&paths).map(|root| root.join(if cfg!(windows) { "blender.exe" } else { "blender" }))); }
    if let Some(path) = candidates.iter().find(|path| path.is_file()) { return Some(path.clone()); }
    #[cfg(windows)]
    candidates.extend(blender_registered_paths());
    if let Some(root) = std::env::var_os("ProgramFiles") {
        let foundation = PathBuf::from(root).join("Blender Foundation");
        if let Ok(entries) = std::fs::read_dir(foundation) { for entry in entries.flatten().take(32) { candidates.push(entry.path().join("blender.exe")); } }
    }
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(windows)]
fn blender_registered_paths() -> Vec<PathBuf> {
    use std::{ffi::c_void, ptr};
    type Hkey = *mut c_void;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(key: Hkey, subkey: *const u16, options: u32, access: u32, result: *mut Hkey) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
        fn RegQueryValueExW(key: Hkey, name: *const u16, reserved: *mut u32, kind: *mut u32, data: *mut u8, length: *mut u32) -> i32;
        fn RegEnumKeyExW(key: Hkey, index: u32, name: *mut u16, length: *mut u32, reserved: *mut u32, class: *mut u16, class_length: *mut u32, time: *mut c_void) -> i32;
    }
    struct Key(Hkey);
    impl Drop for Key { fn drop(&mut self) { unsafe { RegCloseKey(self.0); } } }
    fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
    fn open(root: Hkey, path: &str, view: u32) -> Option<Key> {
        let mut key = ptr::null_mut();
        if unsafe { RegOpenKeyExW(root, wide(path).as_ptr(), 0, 0x20019 | view, &mut key) } != 0 { return None; }
        Some(Key(key))
    }
    fn value(key: &Key, name: &str) -> Option<String> {
        let mut data = vec![0u16; 16_384]; let mut length = (data.len() * 2) as u32; let mut kind = 0;
        if unsafe { RegQueryValueExW(key.0, wide(name).as_ptr(), ptr::null_mut(), &mut kind, data.as_mut_ptr().cast(), &mut length) } != 0
            || !matches!(kind, 1 | 2) || length % 2 != 0 || length as usize > data.len() * 2 { return None; }
        data.truncate(length as usize / 2);
        if let Some(end) = data.iter().position(|value| *value == 0) { data.truncate(end); }
        let mut text = String::from_utf16(&data).ok()?;
        if kind == 2 {
            // REG_EXPAND_SZ normally uses %ProgramFiles% or %LOCALAPPDATA%.
            let mut expanded = String::new();
            while let Some(start) = text.find('%') {
                expanded.push_str(&text[..start]);
                let tail = &text[start + 1..];
                let Some(end) = tail.find('%') else { expanded.push_str(&text[start..]); return Some(expanded); };
                expanded.push_str(&std::env::var(&tail[..end]).unwrap_or_else(|_| format!("%{}%", &tail[..end])));
                text = tail[end + 1..].to_owned();
            }
            expanded.push_str(&text); text = expanded;
        }
        Some(text)
    }
    let mut paths = Vec::new();
    for root in [0x80000001u32, 0x80000002u32] {
        let root = root as i32 as isize as Hkey;
        for view in [0x100, 0x200] {
            if let Some(key) = open(root, r"Software\Microsoft\Windows\CurrentVersion\App Paths\blender.exe", view) {
                if let Some(path) = value(&key, "") { paths.push(PathBuf::from(path.trim_matches('"'))); }
            }
            let Some(uninstall) = open(root, r"Software\Microsoft\Windows\CurrentVersion\Uninstall", view) else { continue; };
            for index in 0..4096 {
                let mut name = [0u16; 512]; let mut length = name.len() as u32;
                let status = unsafe { RegEnumKeyExW(uninstall.0, index, name.as_mut_ptr(), &mut length, ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
                if status == 259 { break; }
                if status != 0 { continue; }
                let Ok(name) = String::from_utf16(&name[..length as usize]) else { continue; };
                let Some(key) = open(uninstall.0, &name, view) else { continue; };
                if !value(&key, "DisplayName").is_some_and(|name| name.to_ascii_lowercase().starts_with("blender")) { continue; }
                if let Some(path) = value(&key, "InstallLocation") { paths.push(PathBuf::from(path.trim_matches('"')).join("blender.exe")); }
                if let Some(icon) = value(&key, "DisplayIcon") {
                    let path = match icon.rsplit_once(',') { Some((path, index)) if index.trim().parse::<i32>().is_ok() => path, _ => &icon };
                    let path = PathBuf::from(path.trim().trim_matches('"'));
                    if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.eq_ignore_ascii_case("blender.exe")) { paths.push(path); }
                }
            }
        }
    }
    paths
}

fn blend_cache(context: &GatewayToolContext, version: &str) -> PathBuf { context.data_home.join("Cache/Previews/blender-glb-v1").join(format!("{version}.glb")) }

#[path = "gateway_workspace_blend.rs"]
mod blend_source;

async fn blend_glb(_path: &Path, version: &str, bytes: &[u8], context: &GatewayToolContext) -> Result<Option<PathBuf>, GatewayError> {
    let Some(program) = blender() else { return Ok(None); };
    // A .blend can embed absolute linked-library/image paths which Blender resolves while opening.
    // Inspect bounded decompressed SDNA before opening; only known self-contained sources enter.
    // Render output and saved navigation fields are not external reads.
    if blend_source::self_contained(bytes).is_err() {
        return Err(failure("BLEND_EXTERNAL_DEPENDENCIES", "此 .blend 可能包含外部链接，不能让 Blender 读取未逐项授权的文件。请先打包全部资源或导出 GLB。", 422));
    }
    let cached = blend_cache(context, version);
    if cached.is_file() { return Ok(Some(cached)); }
    static CONVERSIONS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    let _permit = CONVERSIONS.get_or_init(|| tokio::sync::Semaphore::new(1)).acquire().await
        .map_err(|_| failure("PREVIEW_CONVERTER_UNAVAILABLE", "Blender 转换器不可用。", 503))?;
    check_cancelled(context)?;
    let temporary = cached.parent().unwrap().join(uuid::Uuid::new_v4().to_string());
    tokio::fs::create_dir_all(&temporary).await?;
    tokio::fs::write(temporary.join("source.blend"), bytes).await?;
    let script = r#"import bpy, os
root = os.path.dirname(__file__)
bpy.ops.wm.open_mainfile(filepath=os.path.join(root, 'source.blend'), load_ui=False, use_scripts=False)
if bpy.data.libraries or any(not image.packed_file and image.source == 'FILE' and image.filepath for image in bpy.data.images):
    raise RuntimeError('External dependencies must be packed before preview')
bpy.ops.export_scene.gltf(filepath=os.path.join(root, 'preview.glb'), export_format='GLB')
"#;
    tokio::fs::write(temporary.join("convert.py"), script).await?;
    let mut command = tokio::process::Command::new(program);
    command.args(["--background", "--factory-startup", "--disable-autoexec", "--python-exit-code", "1", "--python"])
        .arg(temporary.join("convert.py")).current_dir(&temporary)
        .env("BLENDER_USER_CONFIG", temporary.join("config")).env("BLENDER_USER_SCRIPTS", temporary.join("scripts"))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    super::gateway_process::hide(&mut command);
    let mut child = command.spawn().map_err(|_| failure("PREVIEW_CONVERTER_UNAVAILABLE", "Blender 无法启动。", 503))?;
    let owner = match super::gateway_process::OwnedProcessJob::attach(&child) {
        Ok(owner) => owner,
        Err(error) => {
            super::gateway_process::terminate(&mut child).await;
            let _ = tokio::fs::remove_dir_all(&temporary).await;
            return Err(error);
        }
    };
    let mut cancellation = context.cancellation.clone();
    let status = tokio::select! {
        status = child.wait() => status.map_err(GatewayError::from),
        _ = super::gateway::wait_for_cancellation(&mut cancellation) => Err(failure("TOOL_CANCELLED", "3D 转换已取消。", 409)),
        _ = tokio::time::sleep(Duration::from_secs(90)) => Err(failure("PREVIEW_CONVERTER_TIMEOUT", "Blender 预览转换超时。", 504)),
    };
    drop(owner);
    let result = match status {
        Ok(status) if status.success() => {
            let output = temporary.join("preview.glb");
            let generated = tokio::fs::read(&output).await?;
            if generated.len() > 256 * 1024 * 1024 || !generated.starts_with(b"glTF") { Err(failure("PREVIEW_CONVERTER_FAILED", "Blender 未产出有效 GLB。", 422)) }
            else { tokio::fs::rename(output, &cached).await.map(|_| Some(cached)).map_err(GatewayError::from) }
        }
        Ok(_) => Err(failure("PREVIEW_CONVERTER_FAILED", "Blender 转换失败，请打包全部依赖后重试。", 422)),
        Err(error) => Err(error),
    };
    let _ = tokio::fs::remove_dir_all(&temporary).await;
    result
}

pub(super) async fn blend_content(path: &Path, version: &str, bytes: &[u8], context: &GatewayToolContext) -> Result<WorkspaceContent, GatewayError> {
    let cached = blend_glb(path, version, bytes, context).await?.ok_or_else(|| failure("PREVIEW_CONVERTER_UNAVAILABLE", "未找到本机 Blender。", 503))?;
    let bytes = super::local_files::read_checked(&cached, 256 * 1024 * 1024, false, "PREVIEW_CACHE_CHANGED")
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    Ok(WorkspaceContent { bytes, mime_type:"model/gltf-binary", version:version.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_mimes_and_manifests_are_explicit() {
        assert_eq!(format(Path::new("SOUND.FLAC")), Some(("audio", "audio/flac")));
        assert_eq!(format(Path::new("clip.webm")), Some(("video", "video/webm")));
        assert_eq!(manifest_uris(Path::new("x.gltf"), br#"{"buffers":[{"uri":"mesh.bin"}],"images":[{"uri":"data:image/png;base64,AAAA"},{"uri":"texture.png"}]}"#).unwrap(), vec!["mesh.bin", "texture.png"]);
        assert_eq!(manifest_uris(Path::new("x.obj"), b"mtllib one.mtl two.mtl\nv 0 1 2").unwrap(), vec!["one.mtl", "two.mtl"]);
        assert!(manifest_uris(Path::new("x.mtl"), b"map_Kd -s 1 1 1 texture.png").is_err());
    }
    #[tokio::test]
    async fn blender_robot_sample_exports_through_preview_gate() {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Samples/robot.blend");
        if !sample.is_file() || blender().is_none() { return; }
        let original = std::fs::read(&sample).unwrap();
        let version = super::super::local_files::sha256(&original);
        let (context, _cancel) = super::super::gateway::test_context();
        let result = blend_glb(&sample, &version, &original, &context).await.unwrap().unwrap();
        let generated = std::fs::read(&result).unwrap();
        assert!(generated.len() > 1000);
        assert!(manifest_uris(Path::new("preview.glb"), &generated).unwrap().is_empty());
        assert_eq!(std::fs::read(&sample).unwrap(), original);
        eprintln!("Blender preview gate exported robot.blend: {} GLB bytes", generated.len());
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }
    #[test]
    fn dependency_paths_reject_remote_and_encoded_escape() {
        let root = std::env::temp_dir().join(format!("kynxa-model-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/t.png"), b"x").unwrap();
        std::fs::write(root.join("outside.png"), b"x").unwrap();
        let assets = std::fs::canonicalize(root.join("assets")).unwrap();
        assert!(dependency_path(&assets, &assets, "https://example.com/t.png").is_err());
        assert!(dependency_path(&assets, &assets, "%2e%2e/outside.png").is_err());
        assert!(dependency_path(&assets, &assets, "t.png").is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn model_dependencies_have_independent_scope_and_versions() {
        let (context, _cancel) = super::super::gateway::test_context();
        let root = context.workdir.parent().unwrap().to_owned();
        std::fs::write(context.workdir.join("scene.gltf"), br#"{"asset":{"version":"2.0"},"buffers":[{"uri":"mesh.bin","byteLength":4}]}"#).unwrap();
        std::fs::write(context.workdir.join("mesh.bin"), [1, 2, 3, 4]).unwrap();
        let result = super::super::gateway_workspace_preview::preview(json!({"path":"scene.gltf"}), &context).await.unwrap();
        assert_eq!(result["kind"], "model3d"); assert_eq!(result["renderable"], true);
        let address = url::Url::parse(&format!("http://localhost{}", result["dependencies"][0]["contentUrl"].as_str().unwrap())).unwrap();
        let mut input = json!({}); for (key,value) in address.query_pairs() { input[key.as_ref()]=json!(value); }
        assert_eq!(super::super::gateway_workspace_preview::content(&input, &context).await.unwrap().bytes, [1,2,3,4]);
        std::fs::write(context.workdir.join("mesh.bin"), [4,3,2,1]).unwrap();
        assert_eq!(super::super::gateway_workspace_preview::content(&input,&context).await.err().unwrap().code,"TOOL_FILE_CONFLICT");
        let external = root.join("external"); std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("scene.gltf"), br#"{"buffers":[{"uri":"mesh.bin"}]}"#).unwrap();
        std::fs::write(external.join("mesh.bin"), [1,2,3,4]).unwrap();
        let selected = super::super::gateway_workspace_preview::preview(json!({"path":external.join("scene.gltf"),"userSelected":true}),&context).await.unwrap();
        assert_eq!(selected["renderable"],false); assert_eq!(selected["dependencies"][0]["available"],false);
        assert_eq!(selected["dependencies"][0]["code"],"MODEL_DEPENDENCY_APPROVAL_REQUIRED");
        std::fs::remove_dir_all(root).unwrap();
    }
}
