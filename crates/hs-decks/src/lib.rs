//! `hs-decks` — 牌组域与对局历史仓储(设计文档 §10.1/§17,M2 PRD 决策 4/5)。
//!
//! G-M2 冻结契约;SQLite 实现由 M2 Track B/C 补齐。
//! 依赖约定:不依赖 GPUI;公开接口不含平台特定类型(Windows 接入约束);
//! rusqlite 连接对象不得出现在 trait 签名中。

mod archive;
mod deck_match;
mod migrate;
mod store;

pub use archive::{archive_power_log, LogArchive};
pub use deck_match::match_decks;
pub use migrate::{apply_deck_migrations, DECK_MIGRATIONS};
pub use store::SqliteDeckStore;

use hs_deckstrings::DeckList;
use hs_types::{DeckId, FormatType, GameId, PlayerId};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum DecksError {
    #[error("deck not found: {0:?}")]
    NotFound(DeckId),
    #[error("deckstring error: {0}")]
    Deckstring(#[from] hs_deckstrings::DeckstringError),
    #[error("migration failed: {0}")]
    Migration(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// 本地牌组(§10.1)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Deck {
    pub id: DeckId,
    pub name: String,
    /// 职业(CardClass 名,如 "MAGE";多职业卡组存主职业)。
    pub class: String,
    pub format: FormatType,
    pub list: DeckList,
    pub archived: bool,
    /// 创建/更新时间(ISO-8601 UTC 文本,与 SQLite `datetime('now')` 一致)。
    pub created_at: String,
    pub updated_at: String,
}

/// 新建/编辑牌组的输入(id 与时间戳由仓储生成)。
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct DeckDraft {
    pub name: String,
    pub class: String,
    pub format: FormatType,
    pub list: DeckList,
}

impl DeckDraft {
    pub fn new(name: impl Into<String>, class: impl Into<String>, list: DeckList) -> Self {
        Self {
            name: name.into(),
            class: class.into(),
            format: list.format,
            list,
        }
    }
}

/// 牌组仓储契约(G-M2 冻结)。
pub trait DeckRepository {
    fn create(&self, draft: &DeckDraft) -> Result<Deck, DecksError>;
    fn update(&self, id: DeckId, draft: &DeckDraft) -> Result<Deck, DecksError>;
    fn copy(&self, id: DeckId, new_name: &str) -> Result<Deck, DecksError>;
    fn set_archived(&self, id: DeckId, archived: bool) -> Result<(), DecksError>;
    fn get(&self, id: DeckId) -> Result<Deck, DecksError>;
    /// `include_archived=false` 时只返回未归档牌组。
    fn list(&self, include_archived: bool) -> Result<Vec<Deck>, DecksError>;
    /// 活动牌组;`None` 表示取消激活。
    fn set_active(&self, id: Option<DeckId>) -> Result<(), DecksError>;
    fn active(&self) -> Result<Option<Deck>, DecksError>;
    /// 按规范化 deckstring 比对:没见过新建,见过复用。不改活动牌组。
    fn upsert_observed_deck(&self, name: &str, deckstring: &str) -> Result<DeckId, DecksError>;
    /// Decks.log 自动导入入口(PRD 决策 5):[`upsert_observed_deck`] 后激活。
    fn ingest_observed_deck(&self, name: &str, deckstring: &str) -> Result<DeckId, DecksError>;
}

/// 一局对局的落库记录(开始/结束时各写一次,§17 game_sessions 子集)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GameRecord {
    pub game_id: GameId,
    /// 使用的本地牌组(未识别时 None)。
    pub deck_id: Option<DeckId>,
    /// 我方/对方职业(CardClass 名)。
    pub friendly_class: Option<String>,
    pub opposing_class: Option<String>,
    pub friendly_player: Option<PlayerId>,
    pub winner: Option<PlayerId>,
    /// 对局起止(ISO-8601 UTC;结束前 ended_at 为 None)。
    pub started_at: String,
    pub ended_at: Option<String>,
    /// 原始日志段存档路径与 sha256(PRD 决策 4;存档失败为 None)。
    pub raw_log_path: Option<String>,
    pub raw_log_sha256: Option<String>,
}

impl GameRecord {
    /// 仅带 `game_id` 的空记录（其余字段按需赋值）。
    /// `GameRecord` 是 `#[non_exhaustive]`，跨 crate 只能这样构造。
    pub fn new(game_id: GameId) -> Self {
        Self {
            game_id,
            deck_id: None,
            friendly_class: None,
            opposing_class: None,
            friendly_player: None,
            winner: None,
            started_at: String::new(),
            ended_at: None,
            raw_log_path: None,
            raw_log_sha256: None,
        }
    }
}

/// 按牌组统计(§10.2 对局历史与按牌组统计)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DeckStats {
    pub deck_id: DeckId,
    pub deck_name: String,
    pub games: u32,
    pub wins: u32,
    pub losses: u32,
}

/// 对局历史查询过滤(全 None = 全部)。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct GameFilter {
    pub deck_id: Option<DeckId>,
    pub limit: Option<u32>,
}

/// 对局历史仓储契约(G-M2 冻结)。
pub trait GameHistoryRepository {
    /// 幂等 upsert:同一 `game_id` 重复写入以最新记录覆盖(对局结束补写终局字段)。
    fn record_game(&self, rec: &GameRecord) -> Result<(), DecksError>;
    fn list_games(&self, filter: &GameFilter) -> Result<Vec<GameRecord>, DecksError>;
    fn deck_stats(&self) -> Result<Vec<DeckStats>, DecksError>;
}
