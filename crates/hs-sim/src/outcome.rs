//! OutcomeSet 与可复现性元数据(§13.1–13.3)。
//!
//! M3 实现 Deterministic/Exact/Sampled/Unsupported 四态;
//! Lazy 只冻结类型([`OutcomeGenerator`])不实现生成器。

use crate::state::SimGameState;
use hs_types::{CardId, RuleStatus};
use serde::{Deserialize, Serialize};

/// 一次模拟实际选择并执行过的卡牌规则。
///
/// `SimOutcome` 按 `(card_id, rule_version)` 去重并排序，调用方可直接把该
/// 列表作为覆盖率证据，不能再从 trace 文本反推。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuleUse {
    pub card_id: CardId,
    pub status: RuleStatus,
    pub confidence: f32,
    pub rule_version: String,
}

/// 惰性展开生成器(§13.1;M3 冻结接口,不提供实现)。
pub trait OutcomeGenerator {
    /// 产出下一个结果;None = 枚举完毕。
    fn next_outcome(&mut self) -> Option<WeightedOutcome>;
}

/// 带概率的单一结果分支。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeightedOutcome {
    /// 概率 ∈ (0,1],同一 OutcomeSet 内 Exact 分支概率和 = 1。
    pub probability: f64,
    pub state: SimGameState,
    /// 该分支的成因(如选中的候选 card_id),Inspector 展示用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 抽样方法(§13.2)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleMethod {
    /// 精确枚举(未截断)。
    Exact,
    /// 蒙特卡洛均匀抽样。
    MonteCarlo,
    /// 分层抽样。
    Stratified,
}

/// 机会节点的来源，供搜索方区分系统随机与需要玩家决策的边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChanceKind {
    /// 游戏规则自动产生的随机结果。
    Random,
    /// 游戏向玩家展示候选项并等待选择（如 Discover）。
    PlayerChoice,
}

/// 抽样结果集(分支数超预算时的截断产物)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampledOutcomeSet {
    pub samples: Vec<WeightedOutcome>,
    pub method: SampleMethod,
    /// 截断前的理论分支数(可估计时)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_branches: Option<u64>,
}

/// 未支持原因（所有兼容与严格入口都禁止静默降级）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedReason {
    /// Operation 变体在 M3 执行子集之外(design §4)。
    NotImplemented { what: String },
    /// 观测状态不足以执行(如需要对手手牌具体牌)。
    InsufficientState { what: String },
    /// 所需事实完整，但动作在当前局面明确不可用（如法力不足或已疲劳）。
    ActionUnavailable { what: String },
    /// 自定义 Handler(M3 只冻结接口)。
    CustomHandler { handler: String },
    /// 递归/嵌套深度超限。
    RecursionLimit,
}

/// 随机状态转移结果(§13.1,冻结)。
///
/// 含 trait object 的 Lazy 变体无法派生 Clone/PartialEq/Serialize,
/// 调用方对 Lazy 只能迭代消费——这是有意设计(不预存全部最终局面)。
pub enum OutcomeSet {
    /// 无随机性,唯一结果。
    Deterministic(SimGameState),
    /// 少量互斥结果,精确概率。
    Exact(Vec<WeightedOutcome>),
    /// 惰性展开(M3 不实现生成器)。
    Lazy(Box<dyn OutcomeGenerator>),
    /// 超预算抽样。
    Sampled(SampledOutcomeSet),
    /// 显式不支持。
    Unsupported(UnsupportedReason),
}

impl std::fmt::Debug for OutcomeSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deterministic(_) => f.write_str("OutcomeSet::Deterministic(..)"),
            Self::Exact(v) => write!(f, "OutcomeSet::Exact({} branches)", v.len()),
            Self::Lazy(_) => f.write_str("OutcomeSet::Lazy(..)"),
            Self::Sampled(s) => write!(f, "OutcomeSet::Sampled({} samples)", s.samples.len()),
            Self::Unsupported(r) => write!(f, "OutcomeSet::Unsupported({r:?})"),
        }
    }
}

/// 执行轨迹一条(§11.8 Rule Trace;Inspector 与 Rule Tests 断言用)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TraceEntry {
    /// 执行的操作名(如 "draw" / "damage" / "discover")。
    pub op: String,
    /// 人读细节(目标实体、数值、池 ID 等)。
    pub detail: String,
}

/// 一次 apply 的完整执行轨迹。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleTrace {
    pub entries: Vec<TraceEntry>,
}

/// 可复现性元数据(§13.3 六要素,冻结)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReproMeta {
    pub seed: u64,
    pub branch_budget: u32,
    pub method: SampleMethod,
    /// 截断原因(未截断为 None)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    /// 本次 apply 遇到的机会节点来源；普通确定性动作没有该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chance_kind: Option<ChanceKind>,
    /// 规则版本标识(EffectProgram 的 card_id + build_range 序列化)。
    pub rule_version: String,
    /// 本次 apply 消费的全部 PoolSnapshot context_hash。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pool_snapshot_hashes: Vec<String>,
}
