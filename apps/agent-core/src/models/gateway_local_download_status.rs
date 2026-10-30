//! Download observations are transient; the existing manifest owns resumable byte counts.
//! 下载观测仅保存在内存，断点字节数仍由原清单持有。
use super::*;
use std::{collections::VecDeque, sync::atomic::{AtomicU64, AtomicUsize, Ordering}};

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DownloadStatus {
    pub phase: String,
    pub source: String,
    pub source_kind: String,
    pub bytes_per_second: Option<u64>,
    pub remaining_seconds: Option<u64>,
    pub connections: usize,
    pub target_connections: usize,
    pub adjustment: String,
}

pub(super) async fn phase(library: &Library, id: &str, value: &str, source: Option<&DownloadSource>) {
    let mut state = library.state.lock().await;
    state.downloads.insert(id.to_owned(), DownloadStatus {
        phase: value.into(), source: source.and_then(|s| url::Url::parse(&s.url).ok())
            .and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default(),
        source_kind: source.map_or("", |s| s.kind).into(), ..Default::default()
    });
}

#[derive(Default)]
pub(super) struct Counters {
    pub bytes: AtomicU64,
    pub active: AtomicUsize,
    pub target: AtomicUsize,
    pub adjustment: std::sync::Mutex<&'static str>,
    pub available: AtomicU64,
    pub ranges: std::sync::Mutex<BTreeMap<u64, u64>>,
}

impl Counters {
    pub fn range_received(&self, start: u64, total: u64) {
        let mut ranges = self.ranges.lock().unwrap_or_else(|e| e.into_inner());
        let previous = ranges.insert(start, total).unwrap_or(0);
        if total >= previous { self.available.fetch_add(total - previous, Ordering::Relaxed); }
        else { self.available.fetch_sub(previous - total, Ordering::Relaxed); }
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn download_resumed_ranges_count_each_byte_once() {
        let counts=Counters::default();
        counts.range_received(0,4096); counts.range_received(8192,1024);
        counts.range_received(0,4096); counts.range_received(0,8192);
        assert_eq!(counts.available.load(Ordering::Relaxed),9216);
        counts.range_received(8192,0);
        assert_eq!(counts.available.load(Ordering::Relaxed),8192);
    }
}

pub(super) struct TransferMeter {
    pub counters: Arc<Counters>,
    base: u64,
    started: Option<Instant>,
    samples: VecDeque<(Instant, u64)>,
    last_publish: Instant,
    last_save: Instant,
    pub speed: Option<u64>,
}

impl TransferMeter {
    pub fn new(base: u64) -> Self {
        Self { counters: Arc::new(Counters::default()), base, started: None, samples: VecDeque::new(),
            last_publish: Instant::now(), last_save: Instant::now(), speed: None }
    }
    pub fn received(&self, bytes: u64) { self.counters.bytes.fetch_add(bytes, Ordering::Relaxed); }
    pub fn slow(&self) -> bool {
        #[cfg(not(test))] let grace = Duration::from_secs(20);
        #[cfg(test)] let grace = Duration::from_millis(300);
        self.started.is_some_and(|start| start.elapsed() >= grace)
            && self.counters.active.load(Ordering::Relaxed) > 0
            && self.speed.is_some_and(|speed| speed < 256 * 1024)
    }
    pub async fn publish(&mut self, home: &Path, library: &Library, record: &InstalledModel, source: &DownloadSource, force: bool) -> Result<(), ModelError> {
        if !force && self.last_publish.elapsed() < Duration::from_millis(250) { return Ok(()); }
        let now = Instant::now();
        let bytes = self.counters.bytes.load(Ordering::Relaxed);
        if bytes > 0 {
            self.started.get_or_insert(now);
            self.samples.push_back((now, bytes));
            while self.samples.len() > 2 && now.duration_since(self.samples[1].0) >= Duration::from_secs(10) { self.samples.pop_front(); }
            if let Some((start, before)) = self.samples.front() {
                let elapsed = now.duration_since(*start).as_secs_f64();
                if elapsed >= 0.25 { self.speed = Some(((bytes.saturating_sub(*before)) as f64 / elapsed) as u64); }
            }
        }
        let transferred = if self.counters.target.load(Ordering::Relaxed) > 0 {
            self.counters.available.load(Ordering::Relaxed)
        } else { bytes };
        let downloaded = self.base.saturating_add(transferred).min(record.spec.download_bytes);
        let save = force || self.last_save.elapsed() >= Duration::from_secs(2);
        progress(home, library, &record.spec.id, downloaded, save).await?;
        let mut state = library.state.lock().await;
        state.downloads.insert(record.spec.id.clone(), DownloadStatus {
            phase: "downloading".into(), source: url::Url::parse(&source.url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default(),
            source_kind: source.kind.into(), bytes_per_second: self.speed,
            remaining_seconds: self.speed.filter(|speed| *speed > 0).map(|speed| record.spec.download_bytes.saturating_sub(downloaded).div_ceil(speed)),
            connections: self.counters.active.load(Ordering::Relaxed),
            target_connections: self.counters.target.load(Ordering::Relaxed),
            adjustment: self.counters.adjustment.lock().unwrap_or_else(|e|e.into_inner()).to_string(),
        });
        self.last_publish = now; if save { self.last_save = now; }
        Ok(())
    }
}
