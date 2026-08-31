//! `hs-card-sync` — 本地卡牌 JSON 导入（TB-02）。
//!
//! 数据获取层是 [`CardSource`]；本轮只有 [`LocalFileSource`]。
//! CLI 参数用手写解析，不引入 clap。

mod fetch;

pub use fetch::{
    run_fetch, run_fetch_with, BuildSpec, FetchArgs, FetchReport, HttpError, HttpGetResponse,
    HttpTransport, UreqTransport, FETCH_ATTEMPTS, FETCH_RETRY_BACKOFF_SECS, FETCH_TIMEOUT,
};

use anyhow::{anyhow, Context, Result};
use hs_card_db::{
    CardLocalization, CardRecord, CardRepository, GameBuildRecord, ImportStats,
    SqliteCardRepository,
};
use hs_types::{BuildId, CardId};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// M0 的旧默认路径（相对进程 cwd）；M1 起仅用于迁移提示。
pub const LEGACY_DB_PATH: &str = "data/cards.sqlite";

/// CLI 默认 SQLite 路径：统一应用数据目录（M1 S1，`HS_DATA_DIR` 可覆盖）。
pub fn default_db_path() -> PathBuf {
    hs_platform::default_card_db_path()
}

/// 未显式传 `--db` 且旧默认路径仍有库文件时，返回迁移提示。
pub fn legacy_db_hint(args: &ImportArgs) -> Option<String> {
    if args.db != default_db_path() {
        return None;
    }
    let legacy = Path::new(LEGACY_DB_PATH);
    legacy.is_file().then(|| {
        format!(
            "hint: 检测到旧默认库 {LEGACY_DB_PATH}（M0）；M1 起默认库为 {}。\
             旧库不会自动迁移，可重新导入或用 --db 显式指定。",
            args.db.display()
        )
    })
}

/// `fetch` 未传 `--locale` 时的默认值。
pub const DEFAULT_FETCH_LOCALE: &str = "zhCN";

pub const USAGE: &str = "\
Usage:
  hs-card-sync import --input <cards.json> --build <id> --locale <loc> [--db <path>]
  hs-card-sync fetch --build <id|latest> [--locale <loc>] [--db <path>]
  hs-card-sync --help

Options:
  --input   卡牌 JSON（fixture 或 HearthstoneJSON 数组）
  --build   Hearthstone 客户端 build 号；fetch 可传 latest
  --locale  主 locale（import 必填；fetch 默认 zhCN）
  --db      SQLite 路径（默认 <应用数据目录>/cards.sqlite，HS_DATA_DIR 可覆盖）
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Help,
    Import(ImportArgs),
    Fetch(FetchArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportArgs {
    pub input: PathBuf,
    pub build: BuildId,
    pub locale: String,
    pub db: PathBuf,
}

/// 可插拔数据源（D-04）。M0 仅本地文件。
pub trait CardSource {
    fn fetch(&self) -> Result<CardSourcePayload>;
}

#[derive(Debug, Clone)]
pub struct CardSourcePayload {
    pub bytes: Vec<u8>,
    pub source_hash: String,
    pub source_manifest: String,
}

#[derive(Debug, Clone)]
pub struct LocalFileSource {
    path: PathBuf,
}

impl LocalFileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl CardSource for LocalFileSource {
    fn fetch(&self) -> Result<CardSourcePayload> {
        let bytes = fs::read(&self.path)
            .with_context(|| format!("read card source {}", self.path.display()))?;
        Ok(CardSourcePayload {
            source_hash: sha256_hex(&bytes),
            source_manifest: self.path.to_string_lossy().into_owned(),
            bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub build_id: BuildId,
    pub stats: ImportStats,
    pub source_hash: String,
    pub sample_name: Option<String>,
}

pub fn parse_args<I, S>(args: I) -> Result<CliAction>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let _argv0 = args.next();

    let mut positional = Vec::new();
    let mut input = None;
    let mut build = None;
    let mut locale = None;
    let mut db = None;

    while let Some(raw) = args.next() {
        let arg = raw.as_ref();
        match arg {
            "-h" | "--help" => return Ok(CliAction::Help),
            "--input" => input = Some(require_value("--input", args.next())?),
            "--build" => build = Some(require_value("--build", args.next())?),
            "--locale" => locale = Some(require_value("--locale", args.next())?),
            "--db" => db = Some(require_value("--db", args.next())?),
            flag if flag.starts_with("--input=") => {
                input = Some(flag[8..].to_string());
            }
            flag if flag.starts_with("--build=") => {
                build = Some(flag[8..].to_string());
            }
            flag if flag.starts_with("--locale=") => {
                locale = Some(flag[9..].to_string());
            }
            flag if flag.starts_with("--db=") => {
                db = Some(flag[5..].to_string());
            }
            flag if flag.starts_with('-') => {
                return Err(anyhow!("unknown flag: {flag}\n{USAGE}"));
            }
            other => positional.push(other.to_string()),
        }
    }

    match positional.as_slice() {
        [] => Err(anyhow!("missing command\n{USAGE}")),
        [cmd] if cmd == "import" => {
            let input = input.ok_or_else(|| anyhow!("--input is required\n{USAGE}"))?;
            let build_raw = build.ok_or_else(|| anyhow!("--build is required\n{USAGE}"))?;
            let locale = locale.ok_or_else(|| anyhow!("--locale is required\n{USAGE}"))?;
            let build_num: u32 = build_raw
                .parse()
                .map_err(|_| anyhow!("--build must be a u32, got {build_raw}"))?;
            Ok(CliAction::Import(ImportArgs {
                input: PathBuf::from(input),
                build: BuildId(build_num),
                locale,
                db: db.map(PathBuf::from).unwrap_or_else(default_db_path),
            }))
        }
        [cmd] if cmd == "fetch" => {
            let build_raw = build.ok_or_else(|| anyhow!("--build is required\n{USAGE}"))?;
            let spec = parse_build_spec(&build_raw)?;
            Ok(CliAction::Fetch(FetchArgs {
                build: spec,
                locale: locale.unwrap_or_else(|| DEFAULT_FETCH_LOCALE.to_string()),
                db: db.map(PathBuf::from).unwrap_or_else(default_db_path),
            }))
        }
        [cmd, ..] => Err(anyhow!("unknown command: {cmd}\n{USAGE}")),
    }
}

fn parse_build_spec(raw: &str) -> Result<BuildSpec> {
    if raw == "latest" {
        return Ok(BuildSpec::Latest);
    }
    let build_num: u32 = raw
        .parse()
        .map_err(|_| anyhow!("--build must be a u32 or latest, got {raw}"))?;
    Ok(BuildSpec::Id(BuildId(build_num)))
}

fn require_value(flag: &str, value: Option<impl AsRef<str>>) -> Result<String> {
    value
        .map(|v| v.as_ref().to_string())
        .ok_or_else(|| anyhow!("{flag} requires a value\n{USAGE}"))
}

pub fn sha256_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn run_import(args: &ImportArgs) -> Result<ImportReport> {
    if let Some(parent) = args.db.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create db directory {}", parent.display()))?;
        }
    }
    let source = LocalFileSource::new(&args.input);
    let mut repo = SqliteCardRepository::open(&args.db)
        .with_context(|| format!("open card db {}", args.db.display()))?;
    import_from_source(&mut repo, &source, args.build, &args.locale)
}

pub fn import_from_source<S: CardSource>(
    repo: &mut SqliteCardRepository,
    source: &S,
    build: BuildId,
    locale: &str,
) -> Result<ImportReport> {
    let payload = source.fetch()?;
    let cards = parse_card_records(&payload.bytes, build, locale)?;
    let stats = repo.import_build(
        &GameBuildRecord {
            build_id: build,
            version_text: None,
            source_manifest: payload.source_manifest,
            source_hash: payload.source_hash.clone(),
        },
        &cards,
    )?;
    let sample_name = repo.card_name(build, &CardId::new("CS2_029"), locale)?;
    Ok(ImportReport {
        build_id: build,
        stats,
        source_hash: payload.source_hash,
        sample_name,
    })
}

pub fn parse_card_records(
    bytes: &[u8],
    build: BuildId,
    fallback_locale: &str,
) -> Result<Vec<CardRecord>> {
    let value: Value = serde_json::from_slice(bytes).context("parse cards json")?;
    let cards = match &value {
        Value::Array(items) => items.as_slice(),
        Value::Object(map) => map
            .get("cards")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .ok_or_else(|| anyhow!("object payload must contain a 'cards' array"))?,
        _ => return Err(anyhow!("cards json must be an array or {{cards: [...]}}")),
    };

    cards
        .iter()
        .map(|card| project_card(card, build, fallback_locale))
        .collect()
}

fn project_card(card: &Value, build: BuildId, fallback_locale: &str) -> Result<CardRecord> {
    let id = card
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("card object missing string 'id'"))?;

    let mut locales: BTreeMap<String, CardLocalization> = BTreeMap::new();
    merge_locale_field(
        &mut locales,
        card.get("name"),
        fallback_locale,
        |loc, text| {
            loc.name = Some(text);
        },
    );
    merge_locale_field(
        &mut locales,
        card.get("text"),
        fallback_locale,
        |loc, text| {
            loc.text = Some(text);
        },
    );
    merge_locale_field(
        &mut locales,
        card.get("flavor"),
        fallback_locale,
        |loc, text| {
            loc.flavor = Some(text);
        },
    );

    Ok(CardRecord {
        build_id: build,
        card_id: CardId::new(id),
        dbf_id: int_field(card, "dbfId"),
        card_type: string_field(card, "type"),
        card_set: string_field(card, "set"),
        rarity: string_field(card, "rarity"),
        card_class_json: card.get("cardClass").map(Value::to_string),
        cost: int_field(card, "cost").and_then(|v| i32::try_from(v).ok()),
        attack: int_field(card, "attack").and_then(|v| i32::try_from(v).ok()),
        health: int_field(card, "health").and_then(|v| i32::try_from(v).ok()),
        collectible: card
            .get("collectible")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        raw_json: serde_json::to_string(card).context("serialize raw_json")?,
        localizations: locales.into_values().collect(),
    })
}

fn string_field(card: &Value, key: &str) -> Option<String> {
    card.get(key).and_then(Value::as_str).map(str::to_string)
}

fn int_field(card: &Value, key: &str) -> Option<i64> {
    card.get(key).and_then(Value::as_i64)
}

fn merge_locale_field(
    locales: &mut BTreeMap<String, CardLocalization>,
    field: Option<&Value>,
    fallback_locale: &str,
    set: impl Fn(&mut CardLocalization, String),
) {
    let Some(field) = field else {
        return;
    };
    match field {
        Value::Object(map) => {
            for (locale, value) in map {
                if let Some(text) = value.as_str() {
                    let entry = locales
                        .entry(locale.clone())
                        .or_insert_with(|| CardLocalization {
                            locale: locale.clone(),
                            name: None,
                            text: None,
                            flavor: None,
                        });
                    set(entry, text.to_string());
                }
            }
        }
        Value::String(text) => {
            let entry = locales
                .entry(fallback_locale.to_string())
                .or_insert_with(|| CardLocalization {
                    locale: fallback_locale.to_string(),
                    name: None,
                    text: None,
                    flavor: None,
                });
            set(entry, text.clone());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/fixtures/cards/999999-minimal.json")
    }

    #[test]
    fn sha256_empty_is_stable() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn parse_import_flags() {
        let action = parse_args([
            "hs-card-sync",
            "import",
            "--input",
            "data/fixtures/cards/999999-minimal.json",
            "--build",
            "999999",
            "--locale",
            "zhCN",
        ])
        .unwrap();
        match action {
            CliAction::Import(args) => {
                assert_eq!(
                    args.input,
                    PathBuf::from("data/fixtures/cards/999999-minimal.json")
                );
                assert_eq!(args.build, BuildId(999999));
                assert_eq!(args.locale, "zhCN");
                assert_eq!(args.db, default_db_path());
                assert!(args.db.ends_with("cards.sqlite"), "{:?}", args.db);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parse_help() {
        assert!(matches!(
            parse_args(["hs-card-sync", "--help"]).unwrap(),
            CliAction::Help
        ));
    }

    #[test]
    fn parse_rejects_unknown_command() {
        assert!(parse_args(["hs-card-sync", "export"]).is_err());
    }

    #[test]
    fn parse_fetch_latest_defaults_locale() {
        let action = parse_args(["hs-card-sync", "fetch", "--build", "latest"]).unwrap();
        match action {
            CliAction::Fetch(args) => {
                assert_eq!(args.build, BuildSpec::Latest);
                assert_eq!(args.locale, "zhCN");
                assert_eq!(args.db, default_db_path());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parse_fetch_numeric_build_and_locale() {
        let action = parse_args([
            "hs-card-sync",
            "fetch",
            "--build=249896",
            "--locale=enUS",
            "--db=/tmp/x.sqlite",
        ])
        .unwrap();
        match action {
            CliAction::Fetch(args) => {
                assert_eq!(args.build, BuildSpec::Id(BuildId(249896)));
                assert_eq!(args.locale, "enUS");
                assert_eq!(args.db, PathBuf::from("/tmp/x.sqlite"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parse_fetch_rejects_bad_build() {
        let err = parse_args(["hs-card-sync", "fetch", "--build", "abc"]).unwrap_err();
        assert!(err.to_string().contains("latest"), "{err}");
    }

    #[test]
    fn fixture_projects_fireball_and_all_locales() {
        let bytes = fs::read(fixture_path()).unwrap();
        let cards = parse_card_records(&bytes, BuildId(999999), "zhCN").unwrap();
        assert_eq!(cards.len(), 12);

        let fireball = cards
            .iter()
            .find(|c| c.card_id.as_str() == "CS2_029")
            .unwrap();
        assert_eq!(fireball.cost, Some(4));
        assert!(fireball.raw_json.contains("CS2_029"));
        let zh = fireball
            .localizations
            .iter()
            .find(|l| l.locale == "zhCN")
            .unwrap();
        assert_eq!(zh.name.as_deref(), Some("火球术"));
        let en = fireball
            .localizations
            .iter()
            .find(|l| l.locale == "enUS")
            .unwrap();
        assert_eq!(en.name.as_deref(), Some("Fireball"));
    }

    #[test]
    fn import_fixture_twice_is_idempotent() {
        let mut repo = SqliteCardRepository::open_in_memory().unwrap();
        let source = LocalFileSource::new(fixture_path());
        let first = import_from_source(&mut repo, &source, BuildId(999999), "zhCN").unwrap();
        let second = import_from_source(&mut repo, &source, BuildId(999999), "zhCN").unwrap();

        assert_eq!(first.stats.cards_upserted, 12);
        assert_eq!(second.stats.cards_upserted, 12);
        assert_eq!(first.source_hash, second.source_hash);
        assert_eq!(first.sample_name.as_deref(), Some("火球术"));
        assert_eq!(second.sample_name.as_deref(), Some("火球术"));
        assert_eq!(repo.list_builds().unwrap(), vec![BuildId(999999)]);

        let card = repo.card(BuildId(999999), &CardId::new("CS2_029")).unwrap();
        assert_eq!(card.card_type.as_deref(), Some("SPELL"));
        assert_eq!(card.cost, Some(4));
    }

    #[test]
    fn string_locale_fields_use_fallback() {
        let json = br#"{"cards":[{"id":"X","name":"Solo","text":"T"}]}"#;
        let cards = parse_card_records(json, BuildId(1), "zhCN").unwrap();
        assert_eq!(cards[0].localizations.len(), 1);
        assert_eq!(cards[0].localizations[0].locale, "zhCN");
        assert_eq!(cards[0].localizations[0].name.as_deref(), Some("Solo"));
        assert_eq!(cards[0].localizations[0].text.as_deref(), Some("T"));
    }
}
