//! 直接效果执行器入口(冻结签名;签名对齐 §16.1 AdvisorApi::simulate)。

use crate::outcome::{OutcomeSet, ReproMeta, RuleTrace, RuleUse};
use crate::state::SimGameState;
use crate::SimError;
use hs_effect_ir::EffectProgram;
use hs_pools::PoolSnapshot;
use hs_types::{BuildId, CardId, CardType, EntityId, PoolId};
use serde::{Deserialize, Serialize};

/// 目标绑定:调用方(M4 搜索/测试)指定的源与目标实体。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TargetBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<EntityId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<EntityId>,
}

/// 池求值回调:执行器遇到 Discover/RandomChoice 时向调用方要快照。
/// 调用方预绑定 PoolContext 与卡牌库(hs-sim 保持无 IO)。
pub trait PoolResolver {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError>;
}

/// 规则查表回调:死亡结算要拿**场上其他实体**的亡语,须按 card_id 查规则。
///
/// 与 [`PoolResolver`] 同惯例 —— `BuildId` 由适配器预绑定,trait 本身不带,
/// hs-sim 保持无 IO、不依赖卡牌库。
///
/// 打出卡**自身**的触发(战吼等)就在 `program.triggers` 里,不需要本 trait。
pub trait RuleLookup {
    fn program_for(&self, card_id: &CardId) -> Option<&EffectProgram>;
}

/// 模拟所需的最小卡牌静态定义。
///
/// 数值字段即使为 `0` 也表示调用方在指定 Build 下查到了真实值；整份定义
/// 缺失则由 [`CardLookup::sim_card`] 返回 `None`，二者不得混为一谈。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimCardDefinition {
    pub card_id: CardId,
    pub card_type: CardType,
    pub cost: i32,
    pub attack: i32,
    pub health: i32,
    pub durability: i32,
}

/// 无 IO 卡牌查表回调。调用方必须用本局精确 Build 查询并返回 owned 投影。
pub trait CardLookup {
    fn sim_card(&self, build: BuildId, card_id: &CardId) -> Option<SimCardDefinition>;
}

/// M4 严格模拟依赖的只读上下文。
///
/// 所有适配器均由调用方按同一局、同一 Build 绑定；`hs-sim` 只消费投影，
/// 不持有数据库或平台对象。
pub struct SimContext<'a> {
    pub build: BuildId,
    pub pools: &'a dyn PoolResolver,
    pub rules: &'a dyn RuleLookup,
    pub cards: &'a dyn CardLookup,
}

/// 空规则表:`apply_program`(冻结签名)用它委托,保持行为不回退。
///
/// 此时任何死亡都查不到亡语 —— 死亡实体会记入
/// [`SimOutcome::unknown_rule_cards`],而不是被静默当作「没有亡语」。
pub struct NoRules;

impl RuleLookup for NoRules {
    fn program_for(&self, _card_id: &CardId) -> Option<&EffectProgram> {
        None
    }
}

/// 一次模拟请求参数(§13.3 复现要素的输入侧)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimRequest {
    pub seed: u64,
    /// 分支预算:Exact 枚举上限,超出转 Sampled。
    pub branch_budget: u32,
}

impl Default for SimRequest {
    fn default() -> Self {
        Self {
            seed: 0,
            branch_budget: 64,
        }
    }
}

/// 一次 apply 的完整产物。
#[derive(Debug)]
pub struct SimOutcome {
    pub outcome: OutcomeSet,
    pub trace: RuleTrace,
    pub meta: ReproMeta,
    /// 本次执行中**规则查不到、因而未结算亡语**的死亡卡(去重,CardId 升序)。
    ///
    /// 「查不到」不等于「没有亡语」:这是覆盖率缺口,不是确定事实。M4 用它
    /// 下调建议置信度(§21 M4 验收第 4 条「未支持规则导致置信度下降」)。
    pub unknown_rule_cards: Vec<CardId>,
    /// 本次实际走过的根/触发规则，按 `(card_id, rule_version)` 稳定排序去重。
    pub rule_uses: Vec<RuleUse>,
}

/// 对沙盘执行一个 EffectProgram 的 play 序列(冻结签名)。
///
/// - design §4 支持子集内的 Operation 逐个执行并记 Trace;
/// - 未支持变体 → `OutcomeSet::Unsupported(NotImplemented)`(非 Err);
/// - 状态不足 → `OutcomeSet::Unsupported(InsufficientState)`;
/// - 同 (state, program, binding, req.seed) 必须产出逐字节一致的结果(§13.3)。
///
/// **本入口用空规则表**,因此:场上其他实体的亡语不会结算(死亡卡记入
/// [`SimOutcome::unknown_rule_cards`]),光环检测也无从进行。需要这两者的
/// 调用方(M4 搜索)用 [`apply_program_with_rules`]。
pub fn apply_program(
    state: &SimGameState,
    program: &EffectProgram,
    binding: &TargetBinding,
    pools: &dyn PoolResolver,
    req: &SimRequest,
) -> Result<SimOutcome, SimError> {
    crate::executor::run(state, program, binding, pools, &NoRules, None, req)
}

/// 同 [`apply_program`],但额外提供规则查表,使死亡结算能取到**其他实体**的亡语。
///
/// 冻结签名 `apply_program` 没有查表位(它对齐 §16.1 `AdvisorApi::simulate`),
/// 故按「只增不改」新开本入口;M4 搜索侧用这个。
pub fn apply_program_with_rules(
    state: &SimGameState,
    program: &EffectProgram,
    binding: &TargetBinding,
    pools: &dyn PoolResolver,
    rules: &dyn RuleLookup,
    req: &SimRequest,
) -> Result<SimOutcome, SimError> {
    crate::executor::run(state, program, binding, pools, rules, None, req)
}

/// 严格 M4 规则入口：除规则表外还注入本局 Build 对应的卡牌定义。
///
/// 与两个兼容入口不同，本入口遇到以下信息缺口时整体返回
/// `Unsupported(InsufficientState)`：
///
/// - `Summon` / `CreateCard` / 随机分支生成卡查不到精确定义；
/// - 死亡实体的规则缺失或状态为 `Unknown`。
///
/// 这样旧 M3 调用仍可保留生成 0/0 的历史行为，而玩家动作搜索不会把未知
/// 卡或未知亡语当作可评分的确定局面。
pub fn apply_program_with_lookups(
    state: &SimGameState,
    program: &EffectProgram,
    binding: &TargetBinding,
    context: &SimContext<'_>,
    req: &SimRequest,
) -> Result<SimOutcome, SimError> {
    crate::executor::run(
        state,
        program,
        binding,
        context.pools,
        context.rules,
        Some((context.cards, context.build)),
        req,
    )
}
