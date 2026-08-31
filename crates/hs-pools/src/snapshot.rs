//! PoolSnapshot:一次池求值的完整可解释结果(§12.5)。
//! Golden Snapshot 测试(§12.7/§19.6)直接序列化本结构对比。

use hs_types::{BuildId, CardId, DbfId, PoolId};
use serde::{Deserialize, Serialize};

/// 带权候选。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeightedCard {
    pub card_id: CardId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dbf_id: Option<DbfId>,
    pub weight: f64,
}

/// 过滤 Trace 一级(§12.5 示例:「类型不是法术: -1730」)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PoolTraceEntry {
    /// 本级描述(如 "format_legal" / "card_type=SPELL" / "exclude")。
    pub step: String,
    /// 本级移除数。
    pub removed: usize,
    /// 本级后剩余数。
    pub remaining: usize,
}

/// 池求值结果(§12.5,冻结)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoolSnapshot {
    pub pool_id: PoolId,
    pub build_id: BuildId,
    /// PoolContext 稳定序列化后的 sha256(可复现性锚点,§13.3 引用)。
    pub context_hash: String,
    /// 候选按 card_id 升序(golden 对比要求确定序)。
    pub candidates: Vec<WeightedCard>,
    pub trace: Vec<PoolTraceEntry>,
    /// 候选集是否精确(动态池信息不足时 false,如对手牌库未知)。
    pub exact: bool,
}
