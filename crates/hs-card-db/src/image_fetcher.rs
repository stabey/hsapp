//! 卡图按需下载（设计文档 §9.4）：
//! 查盘 → 查退避 → 后台下载到临时文件 → 校验 → 原子替换 → 更新 `card_assets`。
//!
//! `ensure` 本身不做网络 IO；失败写退避，调用方立刻拿到 `Miss`/`Backoff`。

use crate::endpoints::{
    full_image_url, is_supported_asset_kind, tile_image_url, ASSET_KIND_FULL, ASSET_KIND_TILE,
};
use crate::image_cache::image_cache_path;
use crate::repository::SqliteCardRepository;
use crate::{AssetKey, CardDbError, ImageCacheStatus, ImageFetcher};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 后台工作线程数（设计：1–2）。
const WORKER_COUNT: usize = 2;

/// 单次卡图下载超时。
const IMAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// 退避：第 1 次失败 30s，第 2 次 2min，第 3 次 10min，之后封顶 30min。
const BACKOFF_SECS: [u64; 4] = [30, 120, 600, 1800];

/// `card_assets.status` 取值。未列在这里的旧值原样保留。
pub const ASSET_STATUS_READY: &str = "ready";
pub const ASSET_STATUS_FAILED: &str = "failed";
pub const ASSET_STATUS_UNSUPPORTED: &str = "unsupported";

/// 可注入的下载传输（测试用本地假实现，禁止单测真联网）。
pub trait ImageTransport: Send + Sync + 'static {
    fn get(&self, url: &str) -> Result<ImageDownload, ImageTransportError>;
}

#[derive(Clone, Debug)]
pub struct ImageDownload {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ImageTransportError {
    pub url: String,
    pub message: String,
    pub status: Option<u16>,
}

impl std::fmt::Display for ImageTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(code) => write!(f, "GET {} -> HTTP {code}: {}", self.url, self.message),
            None => write!(f, "GET {}: {}", self.url, self.message),
        }
    }
}

impl std::error::Error for ImageTransportError {}

/// 生产环境：阻塞式 ureq，在工作线程里调用。
#[derive(Debug, Default)]
pub struct UreqImageTransport;

impl ImageTransport for UreqImageTransport {
    fn get(&self, url: &str) -> Result<ImageDownload, ImageTransportError> {
        match ureq::get(url).timeout(IMAGE_TIMEOUT).call() {
            Ok(resp) => read_image_response(url, resp),
            Err(ureq::Error::Status(code, resp)) => Err(ImageTransportError {
                url: url.to_string(),
                message: format!("HTTP {code}"),
                status: Some(code),
            }
            .with_body_hint(resp)),
            Err(ureq::Error::Transport(err)) => Err(ImageTransportError {
                url: url.to_string(),
                message: err.to_string(),
                status: None,
            }),
        }
    }
}

fn read_image_response(
    url: &str,
    resp: ureq::Response,
) -> Result<ImageDownload, ImageTransportError> {
    let status = resp.status();
    if status != 200 {
        return Err(ImageTransportError {
            url: url.to_string(),
            message: format!("unexpected status {status}"),
            status: Some(status),
        });
    }
    let etag = resp.header("etag").map(str::to_string);
    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| ImageTransportError {
            url: url.to_string(),
            message: format!("read body: {e}"),
            status: Some(status),
        })?;
    Ok(ImageDownload { bytes, etag })
}

impl ImageTransportError {
    fn with_body_hint(mut self, resp: ureq::Response) -> Self {
        let mut buf = [0u8; 128];
        if let Ok(n) = resp.into_reader().read(&mut buf) {
            if n > 0 {
                let hint = String::from_utf8_lossy(&buf[..n]);
                self.message = format!("{}; body-prefix={hint:?}", self.message);
            }
        }
        self
    }
}

struct BackoffEntry {
    until: Instant,
    attempts: u32,
    last_error: String,
}

struct Shared {
    cache_root: PathBuf,
    db_path: PathBuf,
    transport: Arc<dyn ImageTransport>,
    jobs: Mutex<VecDeque<AssetKey>>,
    job_cv: Condvar,
    inflight: Mutex<HashSet<AssetKey>>,
    pending: Mutex<u32>,
    idle_cv: Condvar,
    backoff: Mutex<HashMap<AssetKey, BackoffEntry>>,
    shutdown: AtomicBool,
}

/// G-M2 `ImageFetcher` 的实装：后台 2 线程，失败不阻塞调用方。
pub struct HttpImageFetcher {
    shared: Arc<Shared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl HttpImageFetcher {
    pub fn new(db_path: impl Into<PathBuf>, cache_root: impl Into<PathBuf>) -> Self {
        Self::with_transport(db_path, cache_root, Arc::new(UreqImageTransport))
    }

    pub fn with_transport(
        db_path: impl Into<PathBuf>,
        cache_root: impl Into<PathBuf>,
        transport: Arc<dyn ImageTransport>,
    ) -> Self {
        let shared = Arc::new(Shared {
            cache_root: cache_root.into(),
            db_path: db_path.into(),
            transport,
            jobs: Mutex::new(VecDeque::new()),
            job_cv: Condvar::new(),
            inflight: Mutex::new(HashSet::new()),
            pending: Mutex::new(0),
            idle_cv: Condvar::new(),
            backoff: Mutex::new(HashMap::new()),
            shutdown: AtomicBool::new(false),
        });
        let mut workers = Vec::with_capacity(WORKER_COUNT);
        for i in 0..WORKER_COUNT {
            let shared = Arc::clone(&shared);
            workers.push(
                thread::Builder::new()
                    .name(format!("hs-image-fetch-{i}"))
                    .spawn(move || worker_loop(shared))
                    .expect("spawn image fetch worker"),
            );
        }
        Self {
            shared,
            workers: Mutex::new(workers),
        }
    }

    /// 等到后台队列排空（测试/排障用）。超时返回 false。
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut pending = lock(&self.shared.pending);
        while *pending > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, wait) = self
                .shared
                .idle_cv
                .wait_timeout(pending, deadline - now)
                .expect("idle condvar");
            pending = guard;
            if wait.timed_out() && *pending > 0 {
                return false;
            }
        }
        true
    }

    #[cfg(test)]
    fn last_error(&self, key: &AssetKey) -> Option<String> {
        lock(&self.shared.backoff)
            .get(key)
            .map(|e| e.last_error.clone())
    }
}

impl Drop for HttpImageFetcher {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.job_cv.notify_all();
        let workers = std::mem::take(&mut *lock(&self.workers));
        for handle in workers {
            let _ = handle.join();
        }
    }
}

impl ImageFetcher for HttpImageFetcher {
    fn ensure(&self, key: &AssetKey) -> Result<ImageCacheStatus, CardDbError> {
        if !is_supported_asset_kind(&key.asset_kind) {
            tracing::warn!(
                asset_kind = %key.asset_kind,
                card = %key.card_id.as_str(),
                "未支持的 asset_kind，跳过下载（首版仅 tile/full）"
            );
            record_asset(
                &self.shared,
                key,
                None,
                None,
                None,
                ASSET_STATUS_UNSUPPORTED,
            );
            return Ok(ImageCacheStatus::Miss);
        }

        let path = image_cache_path(
            &self.shared.cache_root,
            key.build,
            &key.card_id,
            &key.locale,
            &key.asset_kind,
            &key.resolution,
        );
        if path.is_file() {
            return Ok(ImageCacheStatus::Hit(path));
        }

        if let Some(entry) = lock(&self.shared.backoff).get(key) {
            if Instant::now() < entry.until {
                tracing::debug!(
                    card = %key.card_id.as_str(),
                    last_error = %entry.last_error,
                    "卡图下载处于退避窗口"
                );
                return Ok(ImageCacheStatus::Backoff);
            }
        }

        enqueue(&self.shared, key);
        Ok(ImageCacheStatus::Miss)
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let key = {
            let mut jobs = lock(&shared.jobs);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(key) = jobs.pop_front() {
                    break key;
                }
                jobs = shared.job_cv.wait(jobs).expect("job condvar");
            }
        };
        let _guard = PendingGuard {
            shared: Arc::clone(&shared),
            key: key.clone(),
        };
        download_one(&shared, &key);
    }
}

struct PendingGuard {
    shared: Arc<Shared>,
    key: AssetKey,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        lock(&self.shared.inflight).remove(&self.key);
        let mut pending = lock(&self.shared.pending);
        *pending = pending.saturating_sub(1);
        self.shared.idle_cv.notify_all();
    }
}

fn enqueue(shared: &Shared, key: &AssetKey) {
    let mut inflight = lock(&shared.inflight);
    if !inflight.insert(key.clone()) {
        return;
    }
    drop(inflight);
    *lock(&shared.pending) += 1;
    lock(&shared.jobs).push_back(key.clone());
    shared.job_cv.notify_one();
}

fn download_one(shared: &Shared, key: &AssetKey) {
    let url = match image_url(key) {
        Ok(url) => url,
        Err(msg) => {
            set_backoff(shared, key, &msg);
            record_asset(shared, key, None, None, None, ASSET_STATUS_FAILED);
            return;
        }
    };

    match shared.transport.get(&url) {
        Ok(download) => match install_image(shared, key, &download) {
            Ok(()) => {
                lock(&shared.backoff).remove(key);
            }
            Err(msg) => {
                set_backoff(shared, key, &msg);
                record_asset(shared, key, None, None, None, ASSET_STATUS_FAILED);
            }
        },
        Err(err) => {
            let msg = err.to_string();
            tracing::warn!(url = %url, error = %msg, "卡图下载失败");
            set_backoff(shared, key, &msg);
            record_asset(shared, key, None, None, None, ASSET_STATUS_FAILED);
        }
    }
}

fn image_url(key: &AssetKey) -> Result<String, String> {
    match key.asset_kind.as_str() {
        ASSET_KIND_TILE => Ok(tile_image_url(&key.card_id)),
        ASSET_KIND_FULL => Ok(full_image_url(&key.card_id, &key.locale, &key.resolution)),
        other => Err(format!("unsupported asset_kind: {other}")),
    }
}

fn install_image(shared: &Shared, key: &AssetKey, download: &ImageDownload) -> Result<(), String> {
    if !looks_like_image(&download.bytes) {
        return Err(format!(
            "payload is not png/jpeg ({} bytes, prefix={:?})",
            download.bytes.len(),
            download.bytes.iter().take(8).cloned().collect::<Vec<_>>()
        ));
    }

    let dest = image_cache_path(
        &shared.cache_root,
        key.build,
        &key.card_id,
        &key.locale,
        &key.asset_kind,
        &key.resolution,
    );
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {e}"))?;
    }

    let tmp = part_path(&dest);
    fs::write(&tmp, &download.bytes).map_err(|e| format!("write temp: {e}"))?;
    fs::rename(&tmp, &dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("atomic replace: {e}")
    })?;

    let sha = sha256_hex(&download.bytes);
    record_asset(
        shared,
        key,
        Some(&dest.to_string_lossy()),
        download.etag.as_deref(),
        Some(&sha),
        ASSET_STATUS_READY,
    );
    Ok(())
}

fn part_path(dest: &Path) -> PathBuf {
    let mut raw = dest.as_os_str().to_os_string();
    raw.push(".part");
    PathBuf::from(raw)
}

fn looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G']) || bytes.starts_with(&[0xFF, 0xD8])
}

fn sha256_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn set_backoff(shared: &Shared, key: &AssetKey, error: &str) {
    let mut map = lock(&shared.backoff);
    let attempts = map
        .get(key)
        .map(|e| e.attempts.saturating_add(1))
        .unwrap_or(1);
    let delay_idx = (attempts.saturating_sub(1) as usize).min(BACKOFF_SECS.len() - 1);
    let delay = Duration::from_secs(BACKOFF_SECS[delay_idx]);
    tracing::warn!(
        card = %key.card_id.as_str(),
        attempts,
        delay_secs = delay.as_secs(),
        error,
        "卡图下载失败，进入退避"
    );
    map.insert(
        key.clone(),
        BackoffEntry {
            until: Instant::now() + delay,
            attempts,
            last_error: error.to_string(),
        },
    );
}

fn record_asset(
    shared: &Shared,
    key: &AssetKey,
    local_path: Option<&str>,
    etag: Option<&str>,
    sha256: Option<&str>,
    status: &str,
) {
    match SqliteCardRepository::open(&shared.db_path) {
        Ok(repo) => {
            if let Err(e) = repo.upsert_card_asset(key, local_path, etag, sha256, status) {
                tracing::warn!(error = %e, "card_assets 更新失败");
            }
        }
        Err(e) => tracing::warn!(error = %e, "打开卡牌库失败，跳过 card_assets 更新"),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CardRepository;
    use hs_types::{BuildId, CardId};
    use std::sync::atomic::AtomicU32;

    /// 1×1 透明 PNG。
    const MINI_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    struct ScriptedTransport {
        hits: AtomicU32,
        body: Mutex<Result<ImageDownload, ImageTransportError>>,
    }

    impl ScriptedTransport {
        fn ok(bytes: &[u8]) -> Arc<Self> {
            Arc::new(Self {
                hits: AtomicU32::new(0),
                body: Mutex::new(Ok(ImageDownload {
                    bytes: bytes.to_vec(),
                    etag: Some("\"abc\"".into()),
                })),
            })
        }

        fn err(status: Option<u16>, message: &str) -> Arc<Self> {
            Arc::new(Self {
                hits: AtomicU32::new(0),
                body: Mutex::new(Err(ImageTransportError {
                    url: "scripted".into(),
                    message: message.into(),
                    status,
                })),
            })
        }
    }

    impl ImageTransport for ScriptedTransport {
        fn get(&self, url: &str) -> Result<ImageDownload, ImageTransportError> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            match lock(&self.body).clone() {
                Ok(ok) => Ok(ok),
                Err(mut err) => {
                    err.url = url.to_string();
                    Err(err)
                }
            }
        }
    }

    fn temp_pair() -> (PathBuf, PathBuf, PathBuf) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("hs-card-db-fetch-{}-{}", std::process::id(), stamp));
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("cards.sqlite");
        let cache = dir.join("cache");
        let _ = SqliteCardRepository::open(&db).unwrap();
        (dir, db, cache)
    }

    fn fireball_tile() -> AssetKey {
        AssetKey {
            build: BuildId(9),
            card_id: CardId::new("CS2_029"),
            locale: "zhCN".into(),
            asset_kind: ASSET_KIND_TILE.into(),
            resolution: "256".into(),
        }
    }

    #[test]
    fn ensure_downloads_then_hits() {
        let (_dir, db, cache) = temp_pair();
        let transport = ScriptedTransport::ok(MINI_PNG);
        let fetcher = HttpImageFetcher::with_transport(&db, &cache, transport);
        let key = fireball_tile();

        let first = fetcher.ensure(&key).unwrap();
        assert!(matches!(first, ImageCacheStatus::Miss));
        assert!(fetcher.wait_idle(Duration::from_secs(2)));

        let second = fetcher.ensure(&key).unwrap();
        match second {
            ImageCacheStatus::Hit(path) => {
                assert!(path.is_file());
                assert!(!part_path(&path).exists());
            }
            other => panic!("expected Hit, got {other:?}"),
        }

        let repo = SqliteCardRepository::open(&db).unwrap();
        let row = repo.card_asset(&key).unwrap().expect("asset row");
        assert_eq!(row.status, ASSET_STATUS_READY);
        assert_eq!(row.sha256, Some(sha256_hex(MINI_PNG)));
        assert_eq!(row.remote_etag.as_deref(), Some("\"abc\""));
        // 打开库只为写资产，不应误导入卡牌。
        assert!(repo.list_builds().unwrap().is_empty());
    }

    #[test]
    fn failed_download_backs_off_and_keeps_error() {
        let (_dir, db, cache) = temp_pair();
        let transport = ScriptedTransport::err(Some(404), "not found");
        let fetcher = HttpImageFetcher::with_transport(&db, &cache, transport);
        let key = fireball_tile();

        assert!(matches!(
            fetcher.ensure(&key).unwrap(),
            ImageCacheStatus::Miss
        ));
        assert!(fetcher.wait_idle(Duration::from_secs(2)));
        assert!(matches!(
            fetcher.ensure(&key).unwrap(),
            ImageCacheStatus::Backoff
        ));
        let err = fetcher.last_error(&key).expect("last_error");
        assert!(err.contains("404"), "{err}");
        assert!(err.contains("tiles/CS2_029.png"), "{err}");

        let repo = SqliteCardRepository::open(&db).unwrap();
        let row = repo.card_asset(&key).unwrap().expect("asset row");
        assert_eq!(row.status, ASSET_STATUS_FAILED);
        assert!(row.local_path.is_none());
    }

    #[test]
    fn garbage_bytes_do_not_land_on_disk() {
        let (_dir, db, cache) = temp_pair();
        let transport = ScriptedTransport::ok(b"<html>nope</html>");
        let fetcher = HttpImageFetcher::with_transport(&db, &cache, transport);
        let key = fireball_tile();

        assert!(matches!(
            fetcher.ensure(&key).unwrap(),
            ImageCacheStatus::Miss
        ));
        assert!(fetcher.wait_idle(Duration::from_secs(2)));
        assert!(matches!(
            fetcher.ensure(&key).unwrap(),
            ImageCacheStatus::Backoff
        ));

        let dest = image_cache_path(
            &cache,
            key.build,
            &key.card_id,
            &key.locale,
            &key.asset_kind,
            &key.resolution,
        );
        assert!(
            !dest.is_file(),
            "invalid payload must not become cache file"
        );
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn unsupported_kind_is_miss_and_recorded() {
        let (_dir, db, cache) = temp_pair();
        let transport = ScriptedTransport::ok(MINI_PNG);
        let fetcher = HttpImageFetcher::with_transport(&db, &cache, transport.clone());
        let key = AssetKey {
            build: BuildId(1),
            card_id: CardId::new("CS2_029"),
            locale: "zhCN".into(),
            asset_kind: "art".into(),
            resolution: "orig".into(),
        };
        assert!(matches!(
            fetcher.ensure(&key).unwrap(),
            ImageCacheStatus::Miss
        ));
        let repo = SqliteCardRepository::open(&db).unwrap();
        let row = repo.card_asset(&key).unwrap().expect("asset row");
        assert_eq!(row.status, ASSET_STATUS_UNSUPPORTED);
        assert_eq!(transport.hits.load(Ordering::SeqCst), 0);
    }
}
