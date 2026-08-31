//! PoolSpec:池定义与选择规则分离(§12.2/12.6)——
//! 「候选是谁」(kind/filters/include/exclude)和「选几个、是否重复」
//! (SelectionRule)不混在一起,这个边界是冻结项。

use crate::context::ContextKey;
use crate::PoolError;
use hs_types::{BuildRange, CardId, PoolId, RuleStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 池类型全集(§12.3,冻结)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolKind {
    /// 显式卡表(entourage/固定伙伴,候选 = include 列表)。
    Explicit,
    Discover,
    RandomCard,
    RandomMinion,
    RandomSpell,
    RandomSummon,
    FromDeck,
    FromHand,
    FromGraveyard,
    FromPlayedHistory,
    FromGeneratedHistory,
    Entourage,
    /// 其他运行时数据库/状态组合查询。
    DynamicQuery,
}

/// 候选过滤器(逐级应用并记录 Trace;§12.5 Trace 每级对应一条)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolFilter {
    /// 当前格式合法(标准轮换;依据卡牌库 card_set)。
    FormatLegal,
    /// 卡牌类型码等于(如 "MINION"/"SPELL")。
    CardType(String),
    /// 职业匹配 PoolContext.player_class(含中立与否由参数定)。
    ClassMatches { include_neutral: bool },
    /// 指定职业码(如 "MAGE";发现池的"你的职业"之外场景)。
    CardClass(String),
    /// 稀有度码等于(如 "LEGENDARY")。
    Rarity(String),
    /// 是否仅 collectible。
    Collectible(bool),
    /// 种族/类型标签(如 "MECH";依据卡牌库 raw_json races)。
    Tribe(String),
    /// 法力费用上下界(闭区间;None = 不限)。
    CostBetween { min: Option<i32>, max: Option<i32> },
    /// 排除来源卡自身。
    ExcludeSource,
}

/// 选择规则:从候选中怎么取(§12.6「选几个、是否重复」)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionRule {
    /// 一次取几张。
    pub picks: u8,
    /// 是否要求彼此不同(发现三选一 = distinct)。
    #[serde(default = "default_true")]
    pub distinct: bool,
}

fn default_true() -> bool {
    true
}

/// 权重规则。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeightRule {
    /// 均匀分布。
    Uniform,
    /// 显式逐卡权重(未列出的卡权重 1.0)。
    Explicit(Vec<(CardId, f64)>),
}

/// 池定义(§12.2,冻结)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSpec {
    pub pool_id: PoolId,
    pub source_card_id: CardId,
    pub build_range: BuildRange,
    pub kind: PoolKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<PoolFilter>,
    /// 该池依赖的上下文键(动态池声明式依赖;resolve 前置校验)。
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub context_inputs: BTreeSet<ContextKey>,
    pub selection: SelectionRule,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<CardId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<CardId>,
    #[serde(default = "uniform")]
    pub weight_rule: WeightRule,
    pub status: RuleStatus,
}

fn uniform() -> WeightRule {
    WeightRule::Uniform
}

impl PoolSpec {
    /// 解析后校验:picks ≥ 1;Explicit/Entourage 的 include 非空。
    pub(crate) fn validate(&self) -> Result<(), PoolError> {
        if self.selection.picks < 1 {
            return Err(PoolError::Invalid(format!(
                "selection.picks 必须 >= 1,实际为 {}",
                self.selection.picks
            )));
        }
        if matches!(self.kind, PoolKind::Explicit | PoolKind::Entourage) && self.include.is_empty()
        {
            return Err(PoolError::Invalid(
                "explicit / entourage 池的 include 不能为空".into(),
            ));
        }
        Ok(())
    }
}
