//! 格式合法集合码(M3 工程快照)。
//!
//! 不把清单内联进 [`crate::resolve`] 主流程:resolve 只通过
//! [`legal_card_sets`] 取表,后续随标准轮换更新只改本文件。

use hs_types::FormatType;

/// 标准模式视为合法的 HearthstoneJSON `set` 码。
///
/// 来源:HearthstoneJSON 卡牌 `set` 字段
/// (<https://hearthstonejson.com/>)与官方标准轮换公开列表的工程摘录,
/// 按 2026-08-23 M3 开工时的集合码固化,不随客户端自动更新。
/// `CORE` 为核心集;其余为近年标准年常见集合码,Track D 接真实卡牌库后
/// 按实际 build 再校正。
pub const STANDARD_ROTATION_SETS: &[&str] = &[
    "CORE",
    "EVENT",
    "WHIZBANGS_WORKSHOP",
    "PERILS_IN_PARADISE",
    "THE_GREAT_DARK_BEYOND",
    "INTO_THE_EMERALD_DREAM",
    "THE_LOST_CITY_OF_UNGORO",
];

/// 经典模式集合码(M3 简化;仅 LEGACY / 经典专家级常见码)。
pub const CLASSIC_SETS: &[&str] = &["LEGACY", "EXPERT1", "VANILLA", "CLASSIC"];

/// 当前格式下用于 [`crate::PoolFilter::FormatLegal`] 的集合码表。
///
/// `None` 表示该格式不按集合码裁剪(狂野 / 幻变 / 未知 = 全部通过)。
pub fn legal_card_sets(format: FormatType) -> Option<&'static [&'static str]> {
    match format {
        FormatType::Standard => Some(STANDARD_ROTATION_SETS),
        FormatType::Classic => Some(CLASSIC_SETS),
        FormatType::Wild | FormatType::Twist | FormatType::Unknown(_) => None,
    }
}
