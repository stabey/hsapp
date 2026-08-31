//! `fetch` 子命令：从 HearthstoneJSON 下载全卡 JSON，落临时文件后走导入管道。

use crate::{import_from_source, sha256_hex, ImportReport};
use anyhow::{anyhow, Context, Result};
use hs_card_db::endpoints::{
    cards_json_url, parse_latest_build_listing, HSJSON_LATEST_LISTING_URL,
};
use hs_card_db::SqliteCardRepository;
use hs_types::BuildId;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// `fetch` 下载 cards.json 超时（约 10MB 的 locale 文件）。
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// 瞬态失败重试次数（含首次）。
pub const FETCH_ATTEMPTS: u32 = 3;

/// 各次重试前等待（秒）：2 / 4 / 8。
pub const FETCH_RETRY_BACKOFF_SECS: [u64; 3] = [2, 4, 8];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildSpec {
    Latest,
    Id(BuildId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchArgs {
    pub build: BuildSpec,
    pub locale: String,
    pub db: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchReport {
    pub resolved_build: BuildId,
    pub source_url: String,
    pub import: ImportReport,
}

/// 可注入 HTTP（单测用假实现，禁止真联网）。
pub trait HttpTransport {
    fn get(&self, url: &str) -> Result<HttpGetResponse, HttpError>;
}

#[derive(Debug, Clone)]
pub struct HttpGetResponse {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HttpError {
    pub url: String,
    pub message: String,
    pub status: Option<u16>,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(code) => write!(f, "GET {} -> HTTP {code}: {}", self.url, self.message),
            None => write!(f, "GET {}: {}", self.url, self.message),
        }
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug, Default)]
pub struct UreqTransport;

impl HttpTransport for UreqTransport {
    fn get(&self, url: &str) -> Result<HttpGetResponse, HttpError> {
        match ureq::get(url).timeout(FETCH_TIMEOUT).call() {
            Ok(resp) => read_ok(url, resp),
            Err(ureq::Error::Status(code, resp)) => Err(http_status_error(url, code, resp)),
            Err(ureq::Error::Transport(err)) => Err(HttpError {
                url: url.to_string(),
                message: err.to_string(),
                status: None,
            }),
        }
    }
}

fn read_ok(url: &str, resp: ureq::Response) -> Result<HttpGetResponse, HttpError> {
    let status = resp.status();
    if status != 200 {
        return Err(HttpError {
            url: url.to_string(),
            message: format!("unexpected status {status}"),
            status: Some(status),
        });
    }
    let etag = resp.header("etag").map(str::to_string);
    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| HttpError {
            url: url.to_string(),
            message: format!("read body: {e}"),
            status: Some(status),
        })?;
    Ok(HttpGetResponse {
        status,
        bytes,
        etag,
    })
}

fn http_status_error(url: &str, code: u16, resp: ureq::Response) -> HttpError {
    let mut buf = [0u8; 160];
    let hint = match resp.into_reader().read(&mut buf) {
        Ok(n) if n > 0 => format!(" body-prefix={:?}", String::from_utf8_lossy(&buf[..n])),
        _ => String::new(),
    };
    HttpError {
        url: url.to_string(),
        message: format!("HTTP {code}{hint}"),
        status: Some(code),
    }
}

pub fn run_fetch(args: &FetchArgs) -> Result<FetchReport> {
    run_fetch_with(args, &UreqTransport)
}

pub fn run_fetch_with(args: &FetchArgs, http: &dyn HttpTransport) -> Result<FetchReport> {
    let resolved_build = resolve_build(http, &args.build)?;
    let source_url = cards_json_url(resolved_build, &args.locale);
    let tmp = download_cards_json(http, &source_url, resolved_build, &args.locale)?;

    if let Some(parent) = args.db.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create db directory {}", parent.display()))?;
        }
    }
    let mut repo = SqliteCardRepository::open(&args.db)
        .with_context(|| format!("open card db {}", args.db.display()))?;
    let source = FetchedFileSource {
        path: tmp.clone(),
        source_manifest: source_url.clone(),
    };
    let import = import_from_source(&mut repo, &source, resolved_build, &args.locale)?;
    let _ = fs::remove_file(&tmp);
    Ok(FetchReport {
        resolved_build,
        source_url,
        import,
    })
}

fn resolve_build(http: &dyn HttpTransport, spec: &BuildSpec) -> Result<BuildId> {
    match spec {
        BuildSpec::Id(id) => Ok(*id),
        BuildSpec::Latest => {
            let listing = get_with_retry(http, HSJSON_LATEST_LISTING_URL)?;
            let html = String::from_utf8_lossy(&listing.bytes);
            parse_latest_build_listing(&html).map_err(|e| anyhow!("{e}"))
        }
    }
}

fn download_cards_json(
    http: &dyn HttpTransport,
    url: &str,
    build: BuildId,
    locale: &str,
) -> Result<PathBuf> {
    let resp = get_with_retry(http, url)?;
    validate_cards_payload(&resp.bytes)
        .with_context(|| format!("validate downloaded cards json from {url}"))?;

    let dir = std::env::temp_dir().join("hs-card-sync");
    fs::create_dir_all(&dir).context("create temp download dir")?;
    let path = dir.join(format!(
        "cards-{}-{}-{}.json",
        build.0,
        locale,
        std::process::id()
    ));
    let tmp = path.with_extension("json.part");
    fs::write(&tmp, &resp.bytes)
        .with_context(|| format!("write temp cards json {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("atomic replace {}", path.display()))?;
    // sha256 由 FetchedFileSource / import 再算一遍；这里只确认文件非空。
    if path.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(anyhow!("downloaded cards json is empty: {url}"));
    }
    Ok(path)
}

fn get_with_retry(http: &dyn HttpTransport, url: &str) -> Result<HttpGetResponse> {
    let mut last_err: Option<HttpError> = None;
    for attempt in 0..FETCH_ATTEMPTS {
        match http.get(url) {
            Ok(resp) => return Ok(resp),
            Err(err) => {
                let retryable = err.status.map(|s| s >= 500).unwrap_or(true);
                last_err = Some(err);
                if !retryable || attempt + 1 == FETCH_ATTEMPTS {
                    break;
                }
                let delay = FETCH_RETRY_BACKOFF_SECS[attempt as usize];
                tracing::warn!(
                    url,
                    attempt = attempt + 1,
                    delay_secs = delay,
                    error = %last_err.as_ref().unwrap(),
                    "下载失败，准备重试"
                );
                thread::sleep(Duration::from_secs(delay));
            }
        }
    }
    match last_err {
        Some(err) => Err(anyhow!("{err}")),
        None => Err(anyhow!("GET {url} failed")),
    }
}

fn validate_cards_payload(bytes: &[u8]) -> Result<()> {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|i| bytes[i])
        .unwrap_or(0);
    if start == b'[' || start == b'{' {
        return Ok(());
    }
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(80)]);
    Err(anyhow!(
        "payload is not JSON (first non-space byte = {start:?}, prefix={prefix:?})"
    ))
}

struct FetchedFileSource {
    path: PathBuf,
    source_manifest: String,
}

impl crate::CardSource for FetchedFileSource {
    fn fetch(&self) -> Result<crate::CardSourcePayload> {
        let bytes = fs::read(&self.path)
            .with_context(|| format!("read fetched cards {}", self.path.display()))?;
        Ok(crate::CardSourcePayload {
            source_hash: sha256_hex(&bytes),
            source_manifest: self.source_manifest.clone(),
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CardSource;
    use hs_card_db::CardRepository;
    use hs_types::CardId;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MapHttp {
        routes: HashMap<String, Result<HttpGetResponse, HttpError>>,
        hits: Mutex<HashMap<String, u32>>,
    }

    impl MapHttp {
        fn new() -> Self {
            Self {
                routes: HashMap::new(),
                hits: Mutex::new(HashMap::new()),
            }
        }

        fn ok(mut self, url: &str, bytes: &[u8]) -> Self {
            self.routes.insert(
                url.to_string(),
                Ok(HttpGetResponse {
                    status: 200,
                    bytes: bytes.to_vec(),
                    etag: None,
                }),
            );
            self
        }

        fn err(mut self, url: &str, status: Option<u16>, message: &str) -> Self {
            self.routes.insert(
                url.to_string(),
                Err(HttpError {
                    url: url.to_string(),
                    message: message.into(),
                    status,
                }),
            );
            self
        }

        fn hit_count(&self, url: &str) -> u32 {
            self.hits.lock().unwrap().get(url).copied().unwrap_or(0)
        }
    }

    impl HttpTransport for MapHttp {
        fn get(&self, url: &str) -> Result<HttpGetResponse, HttpError> {
            *self
                .hits
                .lock()
                .unwrap()
                .entry(url.to_string())
                .or_insert(0) += 1;
            match self.routes.get(url) {
                Some(Ok(resp)) => Ok(resp.clone()),
                Some(Err(err)) => Err(err.clone()),
                None => Err(HttpError {
                    url: url.to_string(),
                    message: "no mock route (test must not hit network)".into(),
                    status: Some(404),
                }),
            }
        }
    }

    fn fixture_bytes() -> Vec<u8> {
        fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../data/fixtures/cards/999999-minimal.json"),
        )
        .unwrap()
    }

    fn temp_db() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hs-card-sync-fetch-{}-{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("cards.sqlite")
    }

    #[test]
    fn fetch_imports_fixture_via_mock_http() {
        let db = temp_db();
        let url = cards_json_url(BuildId(999999), "zhCN");
        let http = MapHttp::new().ok(&url, &fixture_bytes());
        let args = FetchArgs {
            build: BuildSpec::Id(BuildId(999999)),
            locale: "zhCN".into(),
            db: db.clone(),
        };
        let report = run_fetch_with(&args, &http).unwrap();
        assert_eq!(report.resolved_build, BuildId(999999));
        assert_eq!(report.source_url, url);
        assert_eq!(report.import.stats.cards_upserted, 12);
        assert_eq!(report.import.sample_name.as_deref(), Some("火球术"));

        let repo = SqliteCardRepository::open(&db).unwrap();
        assert_eq!(
            repo.card_name(BuildId(999999), &CardId::new("CS2_029"), "zhCN")
                .unwrap()
                .as_deref(),
            Some("火球术")
        );
    }

    #[test]
    fn fetch_latest_parses_listing_then_imports() {
        let db = temp_db();
        let listing = br#"<a href="/v1/249896">/v1/249896</a>"#;
        let cards_url = cards_json_url(BuildId(249896), "zhCN");
        let http = MapHttp::new()
            .ok(HSJSON_LATEST_LISTING_URL, listing)
            .ok(&cards_url, &fixture_bytes());
        let args = FetchArgs {
            build: BuildSpec::Latest,
            locale: "zhCN".into(),
            db,
        };
        let report = run_fetch_with(&args, &http).unwrap();
        assert_eq!(report.resolved_build, BuildId(249896));
        assert_eq!(report.source_url, cards_url);
        assert_eq!(report.import.stats.cards_upserted, 12);
    }

    #[test]
    fn http_404_is_not_retried() {
        let url = cards_json_url(BuildId(1), "zhCN");
        let http = MapHttp::new().err(&url, Some(404), "missing");
        let args = FetchArgs {
            build: BuildSpec::Id(BuildId(1)),
            locale: "zhCN".into(),
            db: temp_db(),
        };
        let err = run_fetch_with(&args, &http).unwrap_err();
        assert!(err.to_string().contains("404"), "{err}");
        assert_eq!(http.hit_count(&url), 1);
    }

    #[test]
    fn html_payload_is_rejected() {
        assert!(validate_cards_payload(b"<html>nope</html>").is_err());
        assert!(validate_cards_payload(b"  [{\"id\":\"X\"}]").is_ok());
    }

    #[test]
    fn fetched_source_keeps_url_as_manifest() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/fixtures/cards/999999-minimal.json");
        let source = FetchedFileSource {
            path,
            source_manifest: "https://example.test/cards.json".into(),
        };
        let payload = source.fetch().unwrap();
        assert_eq!(payload.source_manifest, "https://example.test/cards.json");
        assert_eq!(payload.source_hash, sha256_hex(&payload.bytes));
    }
}
