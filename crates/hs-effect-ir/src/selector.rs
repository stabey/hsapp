//! Selector 只筛选实体、Predicate 只判定条件——都不修改状态(§11.6)。
//! 状态修改全部走 [`crate::Operation`],这个边界是冻结项。

use hs_types::{CardId, DbfId, Zone};
use serde::{Deserialize, Serialize};

/// 玩家指代(规则文件视角,"self" = 效果来源方)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerRef {
    SelfPlayer,
    Opponent,
    Both,
}

/// 卡牌指代:按字符串 ID 或 dbf_id 引用(两种写法等价,规则文件二选一)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardRef {
    Id(CardId),
    Dbf(DbfId),
}

/// 条件里被判定的实体角色(§11.7 `when: attacker: {...}`)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityRole {
    SelfEntity,
    Source,
    Target,
    Attacker,
    Defender,
}

/// 实体条件(全部字段可选,None = 不约束;多个字段同时给出为 AND)。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<PlayerRef>,
    /// 卡牌类型码(如 "MINION"/"SPELL"/"HERO",与卡牌库 card_type 列对齐)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_type: Option<String>,
    /// 职业码(如 "MAGE",与卡牌库 card_class 列对齐)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<Zone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_id: Option<CardId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_at_most: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_at_least: Option<i32>,
}

/// 触发条件/分支条件:对某个角色实体的一组约束。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    pub subject: EntityRole,
    #[serde(default)]
    pub condition: EntityCondition,
}

/// 实体筛选器(§11.6 冻结集,只筛选不修改)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    SelfEntity,
    Source,
    Target,
    FriendlyHero,
    EnemyHero,
    FriendlyMinions,
    EnemyMinions,
    EntitiesInZone(Zone),
    Filter(Box<Selector>, EntityCondition),
    Union(Vec<Selector>),
}
