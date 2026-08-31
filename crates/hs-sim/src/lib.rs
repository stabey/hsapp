//! `hs-sim` — 无 IO 的局面沙盘、玩家动作与规则效果执行器。
//!
//! 边界(M4 action-sim):
//! - 严格动作入口按精确 Build 注入卡牌、规则和池快照，不自行读取数据库；
//! - 支持普通出牌、vanilla 战斗、英雄技能、Location 与当前回合终止；
//! - 执行根规则、打出期触发和单亡语级联，并结构化保留规则证据；
//! - 光环、复杂战斗关键词、多亡语真实顺序等尚未覆盖的机制整体退出；
//! - 未实现的 Operation 显式 `Unsupported(NotImplemented)`,
//!   状态不足显式 `Unsupported(InsufficientState)`——不猜测。
//!
//! 依赖约定:无 IO(规则与池快照由调用方加载传入);公开接口无平台特定类型。
//! 既有规则入口保持兼容，M4 搜索使用严格上下文与 [`apply_action`]。

mod action;
mod apply;
mod executor;
mod outcome;
mod rng;
mod select;
mod state;

pub use action::{apply_action, SimAction};
pub use apply::{
    apply_program, apply_program_with_lookups, apply_program_with_rules, CardLookup, NoRules,
    PoolResolver, RuleLookup, SimCardDefinition, SimContext, SimOutcome, SimRequest, TargetBinding,
};
pub use outcome::{
    ChanceKind, OutcomeGenerator, OutcomeSet, ReproMeta, RuleTrace, RuleUse, SampleMethod,
    SampledOutcomeSet, TraceEntry, UnsupportedReason, WeightedOutcome,
};
pub use state::{SimEntity, SimEntityActionState, SimGameState, SimPlayer};

use thiserror::Error;

/// 模拟域错误(执行器内可预期的「未支持」不走 Error,走
/// [`OutcomeSet::Unsupported`];Error 留给调用方使用错误)。
#[derive(Debug, Error)]
pub enum SimError {
    #[error("池求值失败: {0}")]
    Pool(#[from] hs_pools::PoolError),
    #[error("目标绑定无效: {0}")]
    InvalidBinding(String),
    #[error("尚未实现: {0}")]
    NotImplemented(&'static str),
}
