//! Pinned GGUF downloads keep resumable siblings and publish only verified complete files.
//! 固定版本 GGUF 下载保留同目录断点，完整性核验后才发布正式模型文件。

use super::*;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static DOWNLOAD_SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DownloadSources {
    schema_version: u32,
    preferred: String,
    mirrors: BTreeMap<String, Vec<String>>,
    assets: BTreeMap<String, Vec<String>>,
}

impl Default for DownloadSources {
    fn default() -> Self {
        Self {
            schema_version: 1,
            preferred: "auto".into(),
            mirrors: BTreeMap::from([("https://huggingface.co/".into(), vec!["https://hf-mirror.com/".into()])]),
            assets: BTreeMap::new(),
        }
    }
}

struct DownloadSource {
    url: String,
    kind: &'static str,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct ResumeIdentity {
    url: String,
    sha256: String,
    bytes: u64,
    etag: Option<String>,
    last_modified: Option<String>,
}

fn valid_source(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        (url.scheme() == "https" || cfg!(test) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
            && url.username().is_empty() && url.password().is_none()
    })
}

fn sources(home: &Path, file: &ModelFile) -> Result<Vec<DownloadSource>, ModelError> {
    let bytes = managed_files::read(home, &home.join("download-sources.json"), 1024 * 1024).map_err(storage_error)?;
    let configured: DownloadSources = match bytes {
        Some(bytes) => serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))
            .map_err(|_|error("INVALID_LOCAL_MODEL_SOURCES", "模型下载来源配置无法读取，原文件已保留。", 400))?,
        None => DownloadSources::default(),
    };
    if configured.schema_version != 1 || !["auto", "official", "mirror", "github"].contains(&configured.preferred.as_str()) {
        return Err(error("INVALID_LOCAL_MODEL_SOURCES", "模型下载来源配置版本或首选来源无效。", 400));
    }
    let mut result = vec![DownloadSource { url: file.url.clone(), kind: "official" }];
    for (original, mirrors) in configured.mirrors {
        if let Some(suffix) = file.url.strip_prefix(&original) {
            if !original.ends_with('/') || !valid_source(&original) {
                return Err(error("INVALID_LOCAL_MODEL_SOURCES", "模型镜像的原站前缀必须是完整 HTTPS 目录。", 400));
            }
            for mirror in mirrors {
                if !mirror.ends_with('/') {
                    return Err(error("INVALID_LOCAL_MODEL_SOURCES", "模型镜像前缀必须以斜线结尾。", 400));
                }
                result.push(DownloadSource { url: format!("{mirror}{suffix}"), kind: "mirror" });
            }
        }
    }
    if let Some(urls) = configured.assets.get(&file.sha256.to_ascii_lowercase()) {
        for value in urls {
            let parsed = url::Url::parse(value).ok();
            if !parsed.is_some_and(|url| url.host_str() == Some("github.com") && url.path().contains("/releases/download/")) {
                return Err(error("INVALID_LOCAL_MODEL_SOURCES", "GitHub 模型来源必须配置真实 Release 资产链接。", 400));
            }
            result.push(DownloadSource { url: value.clone(), kind: "github" });
        }
    }
    if result.iter().any(|source| !valid_source(&source.url)) {
        return Err(error("INVALID_LOCAL_MODEL_SOURCE", "模型下载必须使用无内嵌凭据的 HTTPS 来源。", 400));
    }
    let mut seen = std::collections::HashSet::new();
    result.retain(|source| seen.insert(source.url.clone()));
    if configured.preferred != "auto" {
        result.sort_by_key(|source| source.kind != configured.preferred);
    }
    Ok(result)
}

fn partial_path(directory: &Path, file: &ModelFile, source: &DownloadSource) -> PathBuf {
    if source.url == file.url {
        directory.join(format!("{}.part", file.name))
    } else {
        directory.join(format!("{}.source-{:x}.part", file.name, Sha256::digest(source.url.as_bytes())))
    }
}

/// Only this file's deterministic resume siblings belong to model cancellation or deletion.
/// 取消或删除模型时只清理该文件可确定归属的断点和身份记录。
pub(super) fn owns_temporary_name(file_name: &str, candidate: &str) -> bool {
    if candidate == format!("{file_name}.part") || candidate == format!("{file_name}.part.json") { return true; }
    candidate.strip_prefix(&format!("{file_name}.source-")).is_some_and(|suffix| {
        let digest = suffix.strip_suffix(".part").or_else(||suffix.strip_suffix(".part.json"));
        digest.is_some_and(|value| value.len() == 64 && value.bytes().all(|byte|byte.is_ascii_hexdigit()))
    })
}

fn io_error(error: std::io::Error) -> ModelError {
    self::error("LOCAL_MODEL_DOWNLOAD_IO", format!("模型下载文件操作失败：{error}"), 500)
}

async fn interrupted(receiver: &mut watch::Receiver<DownloadAction>) -> DownloadAction {
    loop {
        let action = *receiver.borrow_and_update();
        if action != DownloadAction::Running { return action; }
        if receiver.changed().await.is_err() { return DownloadAction::Paused; }
    }
}

async fn verified(path: &Path, file: &ModelFile, receiver: &watch::Receiver<DownloadAction>) -> Result<bool, ModelError> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.is_file() || metadata.len() != file.bytes { return Ok(false); }
    let mut reader = tokio::fs::File::open(path).await.map_err(io_error)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if *receiver.borrow() != DownloadAction::Running { return Ok(false); }
        let count = reader.read(&mut buffer).await.map_err(io_error)?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    if !file.sha256.is_empty() && format!("{:x}", hash.finalize()) != file.sha256 {
        return Ok(false);
    }
    validate_gguf(path).await?;
    Ok(true)
}

async fn progress(home: &Path, library: &Library, id: &str, bytes: u64, save: bool) -> Result<(), ModelError> {
    let mut state = library.state.lock().await;
    if let Some(model) = state.manifest.models.get_mut(id) {
        model.downloaded_bytes = bytes;
        model.status = "downloading".into();
    }
    if save { persist(home, &state.manifest).await?; }
    Ok(())
}

async fn transfer(
    client: &reqwest::Client,
    home: &Path,
    library: &Library,
    record: &InstalledModel,
    file: &ModelFile,
    source: &DownloadSource,
    completed: u64,
    receiver: &mut watch::Receiver<DownloadAction>,
) -> Result<DownloadAction, ModelError> {
    if !safe_leaf(&file.name) { return Err(error("INVALID_LOCAL_MODEL_PATH", "下载文件名无效。", 400)); }
    let directory = resolve(home, &record.directory);
    let destination = directory.join(&file.name);
    let partial = partial_path(&directory, file, source);
    let identity_path = partial.with_extension("part.json");
    for path in [&destination, &partial, &identity_path] {
        managed_files::inspect(&directory, path, false, true, u64::MAX).map_err(storage_error)?;
    }
    if verified(&destination, file, receiver).await? {
        clear_resume_files(&directory, file).await?;
        progress(home, library, &record.spec.id, completed + file.bytes, false).await?;
        return Ok(DownloadAction::Running);
    }
    if *receiver.borrow() != DownloadAction::Running { return Ok(*receiver.borrow()); }
    let url = url::Url::parse(&source.url).map_err(|_| error("INVALID_LOCAL_MODEL_SOURCE", "模型下载来源无效。", 400))?;
    if !valid_source(&source.url) {
        return Err(error("INVALID_LOCAL_MODEL_SOURCE", "模型下载必须使用 HTTPS 来源。", 400));
    }
    let identity = managed_files::read(&directory, &identity_path, 8192).map_err(storage_error)?
        .and_then(|bytes|serde_json::from_slice::<ResumeIdentity>(&bytes).ok()).unwrap_or_default();
    let mut offset = tokio::fs::metadata(&partial).await.map(|metadata| metadata.len()).unwrap_or(0);
    if offset > file.bytes || !identity.url.is_empty() && (identity.url != source.url || identity.sha256 != file.sha256 || identity.bytes != file.bytes) {
        tokio::fs::OpenOptions::new().write(true).truncate(true).open(&partial).await.map_err(io_error)?;
        offset = 0;
    }
    if offset == file.bytes {
        if verified(&partial, file, receiver).await? {
            tokio::fs::rename(&partial, &destination).await.map_err(io_error)?;
            clear_resume_files(&directory, file).await?;
            return Ok(DownloadAction::Running);
        }
        if *receiver.borrow() != DownloadAction::Running { return Ok(*receiver.borrow()); }
        tokio::fs::OpenOptions::new().write(true).truncate(true).open(&partial).await.map_err(io_error)?;
        offset = 0;
    }
    let mut request = client.get(url).header(reqwest::header::ACCEPT_ENCODING, "identity");
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        // If-Range belongs to the same source; different mirrors keep independent prefixes and validators.
        // If-Range 只属于同一来源；不同镜像各自保留前缀和验证标识，不互相覆盖已有断点。
        if let Some(validator) = identity.etag.as_deref().filter(|etag|!etag.starts_with("W/"))
            .or(identity.last_modified.as_deref()) {
            request = request.header(reqwest::header::IF_RANGE, validator);
        }
    }
    let response = tokio::select! {
        action = interrupted(receiver) => return Ok(action),
        response = request.send() => response.map_err(|_|self::error("LOCAL_MODEL_DOWNLOAD_FAILED","模型下载连接失败，正在尝试其他已配置来源。",502))?,
    };
    if !response.status().is_success() {
        return Err(error("LOCAL_MODEL_DOWNLOAD_HTTP", format!("模型下载来源返回 HTTP {}，已有断点保留。", response.status()), 502));
    }
    let expected_response_bytes;
    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let range = response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|value|value.to_str().ok()).unwrap_or("");
        let expression = regex::Regex::new(r"^bytes (\d+)-(\d+)/(\d+)$").unwrap();
        let valid = expression.captures(range).is_some_and(|parts|
            parts[1].parse::<u64>() == Ok(offset) && parts[3].parse::<u64>() == Ok(file.bytes)
                && parts[2].parse::<u64>().is_ok_and(|end|end < file.bytes && end >= offset));
        if !valid { return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "下载来源返回了不匹配的范围，未追加文件。", 502)); }
        let end = expression.captures(range).unwrap()[2].parse::<u64>().unwrap();
        expected_response_bytes = end - offset + 1;
        let changed_identity = identity.etag.as_ref().is_some_and(|etag|
            response.headers().get(reqwest::header::ETAG).and_then(|value|value.to_str().ok()).is_some_and(|current| current != etag));
        if offset > 0 && changed_identity {
            return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "下载来源的文件标识已改变，未追加旧断点。", 502));
        }
    } else {
        // A server ignoring Range starts this file again; never append its full body to an old prefix.
        // 服务端忽略 Range 时仅重新下载当前文件，不能把完整响应追加到已有前缀后。
        offset = 0;
        expected_response_bytes = file.bytes;
    }
    if response.content_length().is_some_and(|length|length != expected_response_bytes) {
        return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "下载来源返回长度与固定文件范围不符，未修改断点。", 502));
    }
    let identity = ResumeIdentity {
        url: source.url.clone(), sha256: file.sha256.clone(), bytes: file.bytes,
        etag: response.headers().get(reqwest::header::ETAG).and_then(|value|value.to_str().ok()).map(str::to_owned),
        last_modified: response.headers().get(reqwest::header::LAST_MODIFIED).and_then(|value|value.to_str().ok()).map(str::to_owned),
    };
    let encoded = serde_json::to_vec(&identity).map_err(|_|error("LOCAL_MODEL_DOWNLOAD_IO", "下载断点身份无法编码。", 500))?;
    managed_files::atomic_write(&directory, &identity_path, &encoded, 8192).map_err(storage_error)?;
    let mut output = tokio::fs::OpenOptions::new().create(true).write(true)
        .append(offset > 0).truncate(offset == 0).open(&partial).await.map_err(io_error)?;
    let response_start = offset;
    let mut chunks = response.bytes_stream();
    let mut shown = Instant::now();
    let mut saved = Instant::now();
    loop {
        let chunk = tokio::select! {
            action = interrupted(receiver) => {
                output.flush().await.map_err(io_error)?;
                progress(home, library, &record.spec.id, completed + offset, true).await?;
                return Ok(action);
            },
            chunk = chunks.next() => chunk,
        };
        let Some(chunk) = chunk else { break; };
        let chunk = chunk.map_err(|_|self::error("LOCAL_MODEL_DOWNLOAD_INTERRUPTED","模型下载中断，断点已保留。",502))?;
        if offset.saturating_add(chunk.len() as u64) > file.bytes || offset - response_start + chunk.len() as u64 > expected_response_bytes {
            return Err(error("LOCAL_MODEL_DOWNLOAD_SIZE", "下载内容大于固定模型文件长度，断点已保留。", 502));
        }
        output.write_all(&chunk).await.map_err(io_error)?;
        offset += chunk.len() as u64;
        if shown.elapsed() >= Duration::from_millis(250) {
            let save = saved.elapsed() >= Duration::from_secs(2);
            progress(home, library, &record.spec.id, completed + offset, save).await?;
            shown = Instant::now(); if save { saved = Instant::now(); }
        }
    }
    output.flush().await.map_err(io_error)?;
    output.sync_all().await.map_err(io_error)?;
    drop(output);
    if offset - response_start != expected_response_bytes || offset < file.bytes {
        return Err(error("LOCAL_MODEL_DOWNLOAD_INTERRUPTED", "模型响应提前结束，已保留有效断点。", 502));
    }
    if !verified(&partial, file, receiver).await? {
        if *receiver.borrow() != DownloadAction::Running { return Ok(*receiver.borrow()); }
        return Err(error("LOCAL_MODEL_INTEGRITY_FAILED", "模型长度或 SHA-256 校验失败，未发布为已安装。", 502));
    }
    tokio::fs::rename(&partial, &destination).await.map_err(io_error)?;
    clear_resume_files(&directory, file).await?;
    progress(home, library, &record.spec.id, completed + file.bytes, true).await?;
    Ok(DownloadAction::Running)
}

async fn clear_resume_files(directory: &Path, file: &ModelFile) -> Result<(), ModelError> {
    let mut entries = tokio::fs::read_dir(directory).await.map_err(io_error)?;
    while let Some(entry) = entries.next_entry().await.map_err(io_error)? {
        if entry.file_name().to_str().is_some_and(|name|owns_temporary_name(&file.name, name)) {
            managed_files::inspect(directory, &entry.path(), false, true, u64::MAX).map_err(storage_error)?;
            tokio::fs::remove_file(entry.path()).await.map_err(io_error)?;
        }
    }
    Ok(())
}

pub(super) async fn run(
    home: &Path,
    library: &Library,
    record: &InstalledModel,
    mut receiver: watch::Receiver<DownloadAction>,
) -> Result<DownloadAction, ModelError> {
    let slots = DOWNLOAD_SLOTS.get_or_init(||Arc::new(tokio::sync::Semaphore::new(4))).clone();
    let _permit = tokio::select! {
        action = interrupted(&mut receiver) => return Ok(action),
        permit = slots.acquire_owned() => permit.map_err(|_|error("LOCAL_MODEL_DOWNLOAD_UNAVAILABLE","下载调度器已停止。",503))?,
    };
    own_directory(home, record).await?;
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(12))
        .read_timeout(Duration::from_secs(30)).no_gzip().no_brotli().no_deflate()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 { attempt.error("too many download redirects") }
            else if valid_source(attempt.url().as_str()) { attempt.follow() }
            else { attempt.error("download redirect requires HTTPS") }
        }))
        .user_agent("KYNXA-local-models/1").build().map_err(|_|error("LOCAL_MODEL_DOWNLOAD_UNAVAILABLE","模型下载连接无法初始化。",500))?;
    let mut completed = 0;
    for file in &record.spec.files {
        let sources = sources(home, file)?;
        let mut result = Err(error("LOCAL_MODEL_DOWNLOAD_FAILED", "模型下载未完成。", 502));
        'attempts: for attempt in 0..2 {
            for source in &sources {
                result = transfer(&client, home, library, record, file, source, completed, &mut receiver).await;
                match &result {
                    Ok(_) => break 'attempts,
                    Err(error) if !["LOCAL_MODEL_DOWNLOAD_FAILED", "LOCAL_MODEL_DOWNLOAD_INTERRUPTED", "LOCAL_MODEL_DOWNLOAD_HTTP",
                        "LOCAL_MODEL_RANGE_MISMATCH", "LOCAL_MODEL_INTEGRITY_FAILED", "LOCAL_MODEL_DOWNLOAD_SIZE"].contains(&error.code.as_str()) => break 'attempts,
                    _ => {},
                }
            }
            if attempt == 0 {
                tokio::select! {
                    action = interrupted(&mut receiver) => {result=Ok(action);break;},
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
            }
        }
        match result? {
            DownloadAction::Running => completed += file.bytes,
            action => return Ok(action),
        }
    }
    Ok(DownloadAction::Running)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::{Request, Response, StatusCode}};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Fixture {
        home: PathBuf,
        bytes: Vec<u8>,
        record: InstalledModel,
        library: Library,
    }

    fn bytes() -> Vec<u8> {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.resize(1048, 7);
        bytes
    }

    async fn fixture(source: String) -> Fixture {
        let home = std::env::temp_dir().join(format!("kynxa-download-sources-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&home).await.unwrap();
        let bytes = bytes();
        let mut spec = catalog()[0].clone();
        spec.id = "fixture-download".into();
        spec.download_bytes = bytes.len() as u64;
        spec.files = vec![ModelFile { name: "tiny.gguf".into(), url: source,
            bytes: bytes.len() as u64, sha256: format!("{:x}", Sha256::digest(&bytes)), role: "model".into() }];
        let record = new_record(&home, &home.join("Local/fixture-download"), spec);
        own_directory(&home, &record).await.unwrap();
        let mut manifest = Manifest::default();
        manifest.models.insert(record.spec.id.clone(), record.clone());
        Fixture { home, bytes, record, library: Library { state: Mutex::new(LibraryState {
            manifest, jobs: HashMap::new() }), operations: Mutex::new(()) } }
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        (address, task)
    }

    async fn transfer_fixture(fixture: &Fixture) -> Result<DownloadAction, ModelError> {
        let (_command, mut receiver) = watch::channel(DownloadAction::Running);
        let file = &fixture.record.spec.files[0];
        transfer(&reqwest::Client::new(), &fixture.home, &fixture.library, &fixture.record, file,
            &DownloadSource { url: file.url.clone(), kind: "official" }, 0, &mut receiver).await
    }

    #[tokio::test]
    async fn download_sources_preserve_defaults_and_require_explicit_github_assets() {
        let fixture = fixture("https://huggingface.co/owner/repository/resolve/fixed/model.gguf".into()).await;
        let file = &fixture.record.spec.files[0];
        let defaults = sources(&fixture.home, file).unwrap();
        assert_eq!(defaults.len(), 2);
        assert_eq!(defaults[1].url, "https://hf-mirror.com/owner/repository/resolve/fixed/model.gguf");
        let github = "https://github.com/owner/repository/releases/download/model/model.gguf";
        let digest = file.sha256.clone();
        tokio::fs::write(fixture.home.join("download-sources.json"), serde_json::to_vec(&json!({
            "schemaVersion":1, "preferred":"github", "assets":{digest:[github]}
        })).unwrap()).await.unwrap();
        let configured = sources(&fixture.home, file).unwrap();
        assert_eq!(configured[0].url, github);
        assert_eq!(configured.len(), 3);
        tokio::fs::remove_dir_all(fixture.home).await.unwrap();
    }

    #[tokio::test]
    async fn download_interruption_resumes_persisted_prefix_with_if_range() {
        let body = Arc::new(bytes());
        let resumed = Arc::new(AtomicBool::new(false));
        let app = Router::new().fallback({
            let body = body.clone(); let resumed = resumed.clone();
            move |request: Request<Body>| {
                let body = body.clone(); let resumed = resumed.clone();
                async move {
                    if request.headers().get("range").is_some() {
                        resumed.store(request.headers().get("range").unwrap() == "bytes=64-"
                            && request.headers().get("if-range").is_some_and(|value| value == "\"fixed\""), Ordering::SeqCst);
                        Response::builder().status(StatusCode::PARTIAL_CONTENT).header("etag", "\"fixed\"")
                            .header("content-range", format!("bytes 64-{}/{}", body.len()-1, body.len()))
                            .body(Body::from(body[64..].to_vec())).unwrap()
                    } else {
                        let stream = futures_util::stream::unfold(0, move |state| {
                            let body = body.clone();
                            async move { match state {
                                0 => Some((Ok::<_, std::io::Error>(body[..64].to_vec()), 1)),
                                1 => { tokio::time::sleep(Duration::from_millis(60)).await;
                                    Some((Err(std::io::Error::other("fixture disconnected")), 2)) },
                                _ => None,
                            } }
                        });
                        Response::builder().header("etag", "\"fixed\"").body(Body::from_stream(stream)).unwrap()
                    }
                }
            }
        });
        let (source, server) = serve(app).await;
        let fixture = fixture(format!("{source}tiny.gguf")).await;
        assert_eq!(transfer_fixture(&fixture).await.err().unwrap().code, "LOCAL_MODEL_DOWNLOAD_INTERRUPTED");
        let directory = resolve(&fixture.home, &fixture.record.directory);
        assert_eq!(tokio::fs::read(directory.join("tiny.gguf.part")).await.unwrap(), fixture.bytes[..64]);
        assert!(directory.join("tiny.gguf.part.json").is_file());
        assert!(matches!(transfer_fixture(&fixture).await.unwrap(), DownloadAction::Running));
        assert!(resumed.load(Ordering::SeqCst));
        assert_eq!(tokio::fs::read(directory.join("tiny.gguf")).await.unwrap(), fixture.bytes);
        assert!(!directory.join("tiny.gguf.part.json").exists());
        server.abort(); let _ = server.await;
        tokio::fs::remove_dir_all(fixture.home).await.unwrap();
    }

    #[tokio::test]
    async fn download_failed_origin_falls_back_without_overwriting_its_prefix() {
        let failed = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback({ let failed = failed.clone(); move || {
            failed.fetch_add(1, Ordering::SeqCst);
            async { (StatusCode::SERVICE_UNAVAILABLE, "fixture unavailable") }
        }});
        let (origin, original_server) = serve(app).await;
        let body = bytes();
        let (mirror, mirror_server) = serve(Router::new().fallback(move || {
            let body = body.clone(); async move { body }
        })).await;
        let fixture = fixture(format!("{origin}tiny.gguf")).await;
        let directory = resolve(&fixture.home, &fixture.record.directory);
        tokio::fs::write(directory.join("tiny.gguf.part"), &fixture.bytes[..64]).await.unwrap();
        tokio::fs::write(fixture.home.join("download-sources.json"), serde_json::to_vec(&json!({
            "preferred":"official", "mirrors":{origin:[mirror]}
        })).unwrap()).await.unwrap();
        let (_command, receiver) = watch::channel(DownloadAction::Running);
        assert!(matches!(run(&fixture.home, &fixture.library, &fixture.record, receiver).await.unwrap(), DownloadAction::Running));
        assert_eq!(failed.load(Ordering::SeqCst), 1);
        assert_eq!(tokio::fs::read(directory.join("tiny.gguf")).await.unwrap(), fixture.bytes);
        assert!(!directory.join("tiny.gguf.part").exists());
        original_server.abort(); mirror_server.abort(); let _ = original_server.await; let _ = mirror_server.await;
        tokio::fs::remove_dir_all(fixture.home).await.unwrap();
    }

    #[tokio::test]
    async fn download_ignored_range_restarts_and_invalid_range_preserves_prefix() {
        let body = bytes();
        let (origin, server) = serve(Router::new().fallback(move |request: Request<Body>| {
            let body = body.clone(); async move {
                let wrong_range = request.uri().path().contains("invalid");
                Response::builder().status(if wrong_range { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
                    .header("content-range", format!("bytes 0-{}/{}", body.len()-1, body.len()))
                    .body(Body::from(body)).unwrap()
            }
        })).await;
        for path in ["ignored", "invalid"] {
            let fixture = fixture(format!("{origin}{path}")).await;
            let directory = resolve(&fixture.home, &fixture.record.directory);
            tokio::fs::write(directory.join("tiny.gguf.part"), &fixture.bytes[..64]).await.unwrap();
            if path == "ignored" {
                assert!(matches!(transfer_fixture(&fixture).await.unwrap(), DownloadAction::Running));
                assert_eq!(tokio::fs::read(directory.join("tiny.gguf")).await.unwrap(), fixture.bytes);
            } else {
                assert_eq!(transfer_fixture(&fixture).await.err().unwrap().code, "LOCAL_MODEL_RANGE_MISMATCH");
                assert_eq!(tokio::fs::read(directory.join("tiny.gguf.part")).await.unwrap(), fixture.bytes[..64]);
                assert!(!directory.join("tiny.gguf").exists());
            }
            tokio::fs::remove_dir_all(fixture.home).await.unwrap();
        }
        server.abort(); let _ = server.await;
    }

    #[tokio::test]
    async fn download_hash_mismatch_never_publishes_or_deletes_another_sources_prefix() {
        let mut corrupted = bytes(); corrupted[128] ^= 1;
        let (origin, server) = serve(Router::new().fallback(move || {
            let body = corrupted.clone(); async move { body }
        })).await;
        let fixture = fixture(format!("{origin}tiny.gguf")).await;
        let file = &fixture.record.spec.files[0];
        let directory = resolve(&fixture.home, &fixture.record.directory);
        let other = partial_path(&directory, file, &DownloadSource { url:"https://example.com/tiny.gguf".into(), kind:"mirror" });
        tokio::fs::write(&other, &fixture.bytes[..64]).await.unwrap();
        assert_eq!(transfer_fixture(&fixture).await.err().unwrap().code, "LOCAL_MODEL_INTEGRITY_FAILED");
        assert!(!directory.join("tiny.gguf").exists());
        assert_eq!(tokio::fs::read(other).await.unwrap(), fixture.bytes[..64]);
        server.abort(); let _ = server.await;
        tokio::fs::remove_dir_all(fixture.home).await.unwrap();
    }
}
