//! `hs-types` — 全 workspace 共享的公共 ID、坐标与基础枚举。
//!
//! 错误约定（G0 冻结）：
//! - 库 crate 用 `thiserror` 定义各自的域错误（如 `StateError`、`CardDbError`），
//!   不在本 crate 集中定义大一统错误。
//! - bin crate 边界用 `anyhow` 聚合。
//! - 所有域错误必须实现 `std::error::Error + Send + Sync + 'static`。
//!
//! 依赖约定：本 crate 不依赖任何 workspace 内 crate，也不依赖 GPUI/SQLite。

mod geometry;
mod ids;

pub use geometry::{LogicalRect, PhysicalRect, ScaleFactor};
pub use ids::{
    BuildId, CardId, DbfId, DeckId, EntityId, GameId, HandlerId, LogFileId, LogSessionId, PlayerId,
    PoolId,
};

use serde::{Deserialize, Serialize};

/// 对局/牌组格式（Deckstring format 字节的领域映射）。
///
/// `Unknown` 保留未识别原始值（与 [`Zone`] 同一 ADR-015 约束）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum FormatType {
    Wild,
    Standard,
    Classic,
    Twist,
    Unknown(i32),
}

impl FormatType {
    /// Deckstring/日志中的原始格式值 → 领域枚举。
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => Self::Wild,
            2 => Self::Standard,
            3 => Self::Classic,
            4 => Self::Twist,
            other => Self::Unknown(other),
        }
    }

    /// 领域枚举 → 原始格式值（编码 Deckstring 用）。
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Wild => 1,
            Self::Standard => 2,
            Self::Classic => 3,
            Self::Twist => 4,
            Self::Unknown(other) => other,
        }
    }
}

/// 实体所处区域（Power.log GameTag ZONE 的领域映射）。
///
/// `Unknown` 承载未识别的原始值：设计约束"未知信息必须显式保留"（ADR-015）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Zone {
    Play,
    Deck,
    Hand,
    Graveyard,
    RemovedFromGame,
    SetAside,
    Secret,
    Invalid,
    Unknown(i32),
}

/// 实体的卡牌类型（Power.log GameTag CARDTYPE 的领域映射）。
///
/// 与 [`Zone`] 同惯例：`Unknown` 承载未识别的原始值，未知信息显式保留
/// （ADR-015）。**本枚举只定义类型词汇，不含数值映射** —— 数值到本枚举的
/// 转换在 `hs-observed-state`（它依赖 `hs-log-core`，用解析器写入 tag 时
/// 的同一套常量，两侧自洽），避免在多处手写游戏枚举表
/// （见 spec `backend/quality-guidelines.md` 的 M1 事故）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum CardType {
    Game,
    Player,
    Hero,
    Minion,
    Spell,
    Enchantment,
    Weapon,
    HeroPower,
    Location,
    Invalid,
    Unknown(i32),
}

impl CardType {
    /// 是否为会「死亡」并进坟场的在场实体（随从/武器/地标）。
    ///
    /// 英雄不算：英雄生命归零是游戏结束，不走死亡结算（§13 死亡结算范围）。
    pub fn dies_on_zero_health(self) -> bool {
        matches!(
            self,
            CardType::Minion | CardType::Weapon | CardType::Location
        )
    }
}

/// 规则/池定义生效的 Build 区间（闭区间；`max: None` = 对后续 build 持续生效）。
/// Effect IR 与 PoolSpec 共用的版本键（§11.2/§12.2，M3 冻结）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRange {
    pub min: BuildId,
    #[serde(default)]
    pub max: Option<BuildId>,
}

impl BuildRange {
    /// build 是否落在区间内。
    pub fn contains(&self, build: BuildId) -> bool {
        build >= self.min && self.max.is_none_or(|max| build <= max)
    }
}

/// 规则可信状态（§11.3；Effect IR 与 PoolSpec 共用，M3 冻结）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    /// 已由日志、回归测试或人工机制测试验证。
    Verified,
    /// 由文本和模板生成，尚未验证。
    Inferred,
    /// 只覆盖部分分支。
    Partial,
    /// 无可执行实现。
    Unknown,
    /// 卡牌或 Build 变化后需要复核。
    Stale,
}

/// 对局模式（日志 GameType 的领域映射；PoolContext 版本键之一，M3 池域）。
///
/// `Unknown` 保留未识别原始值（ADR-015）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum GameMode {
    Ranked,
    Casual,
    Arena,
    Friendly,
    Practice,
    Battlegrounds,
    Unknown(i32),
}

impl GameMode {
    /// 日志 GameType 原始值 → 领域枚举（GT_* 常量子集）。
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => Self::Practice,
            2 => Self::Friendly,
            5 => Self::Arena,
            7 => Self::Ranked,
            16 => Self::Casual,
            23 => Self::Battlegrounds,
            other => Self::Unknown(other),
        }
    }

    /// 领域枚举 → 原始值。
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Practice => 1,
            Self::Friendly => 2,
            Self::Arena => 5,
            Self::Ranked => 7,
            Self::Casual => 16,
            Self::Battlegrounds => 23,
            Self::Unknown(other) => other,
        }
    }
}

/// 对局阶段（粗粒度状态机，细化随 M1 Reducer 演进）。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum GamePhase {
    /// 尚未观察到 CREATE_GAME
    #[default]
    NotStarted,
    Mulligan,
    Playing,
    Ended,
}
