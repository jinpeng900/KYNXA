//! Bounded parallel ranges merge into the original contiguous prefix, never a sparse model file.
//! 有界分段下载按序合入原连续断点，不把稀疏文件长度误当下载进度。
use super::*;
use status::{Counters, TransferMeter};
use std::sync::atomic::Ordering;
use tokio::{io::{AsyncWriteExt, BufReader, BufWriter}, task::JoinSet};

#[cfg(not(test))] pub(super) const RANGE_BYTES: u64 = 8 * 1024 * 1024;
#[cfg(test)] pub(super) const RANGE_BYTES: u64 = 64 * 1024;
pub(super) const MINIMUM_BYTES: u64 = RANGE_BYTES * 4;

pub(super) async fn discard_stale(partial: &Path, file: &ModelFile) -> Result<(), ModelError> {
    let directory=partial.parent().unwrap();
    let prefix=format!("{}.range-",partial.file_name().unwrap().to_string_lossy());
    let mut entries=tokio::fs::read_dir(directory).await.map_err(io_error)?;
    while let Some(entry)=entries.next_entry().await.map_err(io_error)? {
        if entry.file_name().to_str().is_some_and(|name|name.starts_with(&prefix) && owns_temporary_name(&file.name,name)) {
            managed_files::inspect(directory,&entry.path(),false,true,u64::MAX).map_err(storage_error)?;
            tokio::fs::remove_file(entry.path()).await.map_err(io_error)?;
        }
    }
    Ok(())
}

pub(super) async fn connection(receiver: &mut watch::Receiver<DownloadAction>) -> Result<Option<adaptive::Connection>, ModelError> {
    Ok(adaptive::connection(receiver).await)
}

fn range(response: &reqwest::Response, start: u64, end: u64, total: u64) -> bool {
    response.status() == reqwest::StatusCode::PARTIAL_CONTENT
        && response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v|v.to_str().ok())
            == Some(format!("bytes {start}-{end}/{total}").as_str())
        && response.content_length().is_none_or(|bytes| bytes == end - start + 1)
}

fn validator(request: reqwest::RequestBuilder, identity: &ResumeIdentity) -> reqwest::RequestBuilder {
    if let Some(value) = identity.etag.as_deref().filter(|value| !value.starts_with("W/"))
        .or(identity.last_modified.as_deref()) { request.header(reqwest::header::IF_RANGE, value) } else { request }
}

fn retry_after(value: Option<&str>) -> u64 {
    value.and_then(|value| value.parse::<u64>().ok().or_else(||
        chrono::DateTime::parse_from_rfc2822(value).ok().map(|date|
            date.timestamp().saturating_sub(chrono::Utc::now().timestamp()).max(0) as u64)))
        .unwrap_or(2).min(24 * 60 * 60)
}

struct Chunk { start: u64, end: u64, path: PathBuf }

async fn chunk(client: reqwest::Client, file: ModelFile, identity: ResumeIdentity, partial: PathBuf,
    start: u64, end: u64, counts: Arc<Counters>, mut receiver: watch::Receiver<DownloadAction>) -> Result<Option<Chunk>, ModelError> {
    let path = PathBuf::from(format!("{}.range-{start}-{end}", partial.display()));
    managed_files::inspect(path.parent().unwrap(), &path, false, true, u64::MAX).map_err(storage_error)?;
    let mut existing = tokio::fs::metadata(&path).await.map(|v|v.len()).unwrap_or(0);
    if existing > end - start + 1 { existing = 0; }
    counts.range_received(start, existing);
    if existing == end - start + 1 { return Ok(Some(Chunk { start, end, path })); }
    let Some(_permit) = connection(&mut receiver).await? else { return Ok(None); };
    counts.active.fetch_add(1, Ordering::Relaxed);
    let result = async {
        let begin = start + existing;
        let request = validator(client.get(&identity.url).header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(reqwest::header::RANGE, format!("bytes={begin}-{end}")), &identity);
        let response = tokio::select! {
            _ = interrupted(&mut receiver) => return Ok(None),
            value = request.send() => value.map_err(|_|error("LOCAL_MODEL_DOWNLOAD_FAILED", "分段连接失败，断点已保留。", 502))?,
        };
        if response.status() == reqwest::StatusCode::OK {
            return Err(error("LOCAL_MODEL_RANGE_UNAVAILABLE", "来源忽略分段请求，回退单流下载。", 502));
        }
        if matches!(response.status().as_u16(), 429 | 503) {
            let delay = retry_after(response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v|v.to_str().ok()));
            let mut failure = error("LOCAL_MODEL_DOWNLOAD_THROTTLED", "来源限流，降低并发并保留断点。", 503);
            failure.details = json!({"retryAfterSeconds":delay});
            return Err(failure);
        }
        if !response.status().is_success() { return Err(error("LOCAL_MODEL_DOWNLOAD_HTTP",format!("分段来源返回 HTTP {}，断点保留。",response.status()),502)); }
        if !range(&response, begin, end, file.bytes)
            || identity.etag.as_ref().is_some_and(|saved| response.headers().get(reqwest::header::ETAG)
                .and_then(|v|v.to_str().ok()).is_some_and(|current| current != saved)) {
            return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "分段范围或文件身份已变化，未追加数据。", 502));
        }
        let output = tokio::fs::OpenOptions::new().create(true).write(true).append(existing > 0).truncate(existing == 0)
            .open(&path).await.map_err(io_error)?;
        let mut output = BufWriter::with_capacity(256 * 1024, output);
        let mut stream = response.bytes_stream();
        let transfer = async {
            let mut received = existing;
            loop {
                let data = tokio::select! { _ = interrupted(&mut receiver) => return Ok(false), value = stream.next() => value };
                let Some(data) = data else { break; };
                let data = data.map_err(|_|error("LOCAL_MODEL_DOWNLOAD_INTERRUPTED", "分段连接中断，断点已保留。", 502))?;
                if received.saturating_add(data.len() as u64) > end - start + 1 {
                    return Err(error("LOCAL_MODEL_DOWNLOAD_SIZE", "分段响应超过范围，未发布模型。", 502));
                }
                output.write_all(&data).await.map_err(io_error)?;
                received += data.len() as u64;
                counts.range_received(start, received);
                counts.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
            if received != end - start + 1 { return Err(error("LOCAL_MODEL_DOWNLOAD_INTERRUPTED", "分段响应提前结束，断点已保留。", 502)); }
            Ok(true)
        }.await;
        // Settle buffered writes even on pause/error before another attempt touches this range.
        // 暂停或失败也先结算缓冲写入，下一次尝试不能与旧写入重叠。
        output.flush().await.map_err(io_error)?;
        if transfer.as_ref().is_ok_and(|complete| *complete) { output.get_ref().sync_all().await.map_err(io_error)?; }
        Ok(transfer?.then_some(Chunk { start, end, path }))
    }.await;
    counts.active.fetch_sub(1, Ordering::Relaxed);
    result
}

pub(super) async fn transfer(context: &TransferContext<'_>, partial: &Path, old: &ResumeIdentity, offset: u64,
    receiver: &mut watch::Receiver<DownloadAction>, switch_slow: bool) -> Result<Option<DownloadAction>, ModelError> {
    let TransferContext {client,home,library,record,file,source,completed}=*context;
    let mut probe_receiver = receiver.clone();
    let Some(_permit) = connection(&mut probe_receiver).await? else { return Ok(Some(*probe_receiver.borrow())); };
    let response = tokio::select! {
        action = interrupted(receiver) => return Ok(Some(action)),
        value = validator(client.get(&source.url).header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(reqwest::header::RANGE, format!("bytes={offset}-{offset}")), old).send() =>
            value.map_err(|_|error("LOCAL_MODEL_DOWNLOAD_FAILED", "下载来源连接失败。", 502))?,
    };
    if response.status() == reqwest::StatusCode::OK { return Ok(None); }
    if !response.status().is_success() { return Err(error("LOCAL_MODEL_DOWNLOAD_HTTP",format!("下载来源返回 HTTP {}，断点保留。",response.status()),502)); }
    if !range(&response, offset, offset, file.bytes) { return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "来源不支持有效分段范围。", 502)); }
    let identity = ResumeIdentity { url: source.url.clone(), sha256: file.sha256.clone(), bytes: file.bytes,
        etag: response.headers().get(reqwest::header::ETAG).and_then(|v|v.to_str().ok()).map(str::to_owned),
        last_modified: response.headers().get(reqwest::header::LAST_MODIFIED).and_then(|v|v.to_str().ok()).map(str::to_owned) };
    if old.etag.as_ref().zip(identity.etag.as_ref()).is_some_and(|(a,b)|a != b) {
        return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "来源身份已变化，保留原断点。", 502));
    }
    let mut probe = response.bytes_stream();
    let mut probe_bytes = 0;
    loop {
        let value = tokio::select! { action=interrupted(receiver)=>return Ok(Some(action)), value=probe.next()=>value };
        let Some(value)=value else {break;};
        probe_bytes += value.map_err(|_|error("LOCAL_MODEL_DOWNLOAD_INTERRUPTED", "分段探测中断。", 502))?.len();
        if probe_bytes>1 {return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "分段探测响应长度无效。", 502));}
    }
    if probe_bytes!=1 {return Err(error("LOCAL_MODEL_RANGE_MISMATCH", "分段探测响应不完整。", 502));}
    drop(probe); drop(_permit);
    let identity_path = partial.with_extension("part.json");
    managed_files::atomic_write(partial.parent().unwrap(), &identity_path,
        &serde_json::to_vec(&identity).map_err(|_|error("LOCAL_MODEL_DOWNLOAD_IO", "断点身份无法保存。", 500))?, 8192).map_err(storage_error)?;
    let _file_transfer = adaptive::FileTransfer::new();
    let mut meter = TransferMeter::new(completed + offset);
    let mut controller = adaptive::Controller::new(Instant::now());
    meter.counters.target.store(2, Ordering::Relaxed);
    let mut prefix = offset;
    let mut next = offset;
    let mut jobs = JoinSet::new();
    let mut workers: BTreeMap<u64, (u64, usize, watch::Sender<DownloadAction>)> = BTreeMap::new();
    let mut pending = BTreeMap::new();
    let mut ready = BTreeMap::new();
    let directory = partial.parent().unwrap().to_owned();
    let initial_directory = directory.clone();
    let mut resources = tokio::task::spawn_blocking(move ||adaptive::resource_capacity(&initial_directory)).await.unwrap_or(2);
    let mut resource_check = Instant::now();
    let mut resource_job = None;
    let mut retry_at = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    let result = async {
        while prefix < file.bytes {
            // Completed ranges still consume the window until the contiguous prefix catches up.
            // 待合并的段也占窗口，避免乱序下载堆满磁盘。
            while (jobs.len() + ready.len() < controller.target || jobs.is_empty()) && Instant::now() >= retry_at && (!pending.is_empty() || next < file.bytes) {
                let (start, end) = if let Some(value) = pending.pop_first() { value } else {
                    let start = next; let end = (next + RANGE_BYTES - 1).min(file.bytes - 1); next = end + 1; (start, end)
                };
                let (cancel, commands) = watch::channel(DownloadAction::Running);
                let future = chunk(client.clone(), file.clone(), identity.clone(), partial.to_owned(), start, end, meter.counters.clone(), commands);
                jobs.spawn(async move { (start, future.await) });
                workers.insert(start, (end, controller.target, cancel));
            }
            tokio::select! {
                action = interrupted(receiver) => return Ok(action),
                _ = ticker.tick() => {
                    let now = Instant::now();
                    if resource_job.as_ref().is_some_and(|job: &tokio::task::JoinHandle<usize>|job.is_finished()) {
                        resources = resource_job.take().unwrap().await.unwrap_or(2);
                    }
                    if resource_job.is_none() && resource_check.elapsed() >= Duration::from_secs(5) {
                        let directory = directory.clone();
                        resource_job = Some(tokio::task::spawn_blocking(move ||adaptive::resource_capacity(&directory)));
                        resource_check = now;
                    }
                    controller.observe(now, meter.counters.bytes.load(Ordering::Relaxed),
                        adaptive::file_capacity(resources).min((file.bytes-prefix).div_ceil(RANGE_BYTES) as usize),
                        super::super::super::gateway_download_network::foreground_active());
                    meter.counters.target.store(controller.target, Ordering::Relaxed);
                    *meter.counters.adjustment.lock().unwrap_or_else(|e|e.into_inner()) = controller.reason;
                    // Cancel distant work first; each writer flushes before its range may be resumed.
                    // 缩并发先暂停远段，原写入退出并落盘后才能续传。
                    for (_, (_, _, cancel)) in workers.iter().rev().take(workers.len().saturating_sub(controller.target)) {
                        cancel.send_replace(DownloadAction::Paused);
                    }
                    meter.publish(home, library, record, source, false).await?;
                    if switch_slow && now >= retry_at && controller.reason != "chat-priority" && meter.slow() { return Err(error("LOCAL_MODEL_SOURCE_SLOW", "当前来源持续偏慢，尝试其他来源。", 502)); }
                },
                result = jobs.join_next(), if !jobs.is_empty() => {
                    let (start, result) = result.unwrap().map_err(|_|error("LOCAL_MODEL_DOWNLOAD_IO", "分段下载任务退出。", 500))?;
                    let (end, dispatched_target, _) = workers.remove(&start).unwrap();
                    match result {
                        Ok(Some(chunk)) => { ready.insert(chunk.start, chunk); },
                        Ok(None) => { pending.insert(start, end); },
                        Err(failure) if matches!(failure.code.as_str(), "LOCAL_MODEL_DOWNLOAD_THROTTLED" | "LOCAL_MODEL_DOWNLOAD_FAILED" | "LOCAL_MODEL_DOWNLOAD_INTERRUPTED") => {
                            // At one connection, hand persistent throttling back to source fallback.
                            // 单连接仍持续限流时交回来源切换，不无限重试同一来源。
                            if dispatched_target == 1 {
                                return Err(if failure.code == "LOCAL_MODEL_DOWNLOAD_THROTTLED" { error("LOCAL_MODEL_DOWNLOAD_HTTP", failure.message, 503) } else { failure });
                            }
                            let delay = Duration::from_secs(failure.details["retryAfterSeconds"].as_u64().unwrap_or(2));
                            retry_at = Instant::now() + delay;
                            controller.congestion(Instant::now(), meter.counters.bytes.load(Ordering::Relaxed), delay);
                            pending.insert(start, end);
                            for (_, _, cancel) in workers.values() { cancel.send_replace(DownloadAction::Paused); }
                        },
                        Err(failure) => return Err(failure),
                    }
                }
            }
            while let Some(chunk) = ready.remove(&prefix) {
                let mut input = BufReader::with_capacity(256 * 1024, tokio::fs::File::open(&chunk.path).await.map_err(io_error)?);
                let output = tokio::fs::OpenOptions::new().create(true).append(true).open(partial).await.map_err(io_error)?;
                let mut output = BufWriter::with_capacity(256 * 1024, output);
                let copied = tokio::io::copy_buf(&mut input, &mut output).await.map_err(io_error)?;
                output.flush().await.map_err(io_error)?;
                output.get_ref().sync_all().await.map_err(io_error)?;
                if copied != chunk.end - chunk.start + 1 { return Err(error("LOCAL_MODEL_DOWNLOAD_SIZE", "本地分段长度已变化。", 502)); }
                prefix = chunk.end + 1;
                drop(input); drop(output);
                tokio::fs::remove_file(chunk.path).await.map_err(io_error)?;
            }
        }
        Ok(DownloadAction::Running)
    }.await;
    for (_, _, cancel) in workers.values() { cancel.send_replace(DownloadAction::Paused); }
    while jobs.join_next().await.is_some() {}
    meter.publish(home, library, record, source, true).await?;
    result.map(Some)
}
