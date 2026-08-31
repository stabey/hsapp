//! `hs-effect-ir` — 卡牌效果的可执行语义表达（设计文档 §11，M3 Track A）。
//!
//! 三层结构中本 crate 承载 `EffectProgram`（可执行语义）；
//! `CardDefinition`（静态元数据）在 `hs-card-db`，
//! `RuleHandler`（特殊卡 Rust 代码）M3 只冻结 [`hs_types::HandlerId`] 不实现。
//!
//! 依赖约定：不依赖 SQLite/GPUI/平台类型；执行语义在 `hs-sim`，本 crate 只管
//! 「表达、解析、校验、按 build 选版本」。

mod program;
mod ruleset;
mod selector;

pub use program::{
    EffectProgram, EvidenceKind, Operation, RuleEvidence, TriggerEvent, TriggerProgram,
    IR_SCHEMA_VERSION,
};
// 版本键与可信状态在 hs-types(IR 与池域共用),这里 re-export 方便调用方。
pub use hs_types::{BuildRange, RuleStatus};
pub use ruleset::RuleSet;
pub use selector::{CardRef, EntityCondition, EntityRole, PlayerRef, Predicate, Selector};

use thiserror::Error;

/// Effect IR 域错误。
#[derive(Debug, Error)]
pub enum IrError {
    #[error("YAML 解析失败: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("不支持的 schema 版本 {found}(当前支持 {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("字段校验失败: {0}")]
    Invalid(String),
    #[error("重复规则定义: card {card_id} 的 build_range 与已有定义重叠")]
    DuplicateRule { card_id: hs_types::CardId },
    #[error("规则目录读取失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("尚未实现: {0}")]
    NotImplemented(&'static str),
}

/// 解析并校验单个 EffectProgram 的 YAML 文本(§11.7 语义,`schema: 1`)。
///
/// 校验项:schema 版本、`confidence ∈ [0,1]`、build_range `min <= max`、
/// 未知字段报错(deny_unknown_fields)。
///
/// `Operation` / `CardRef` 在 serde_yaml 0.9 下写成 YAML tag(`!draw`、`!id`),
/// 不是设计文档 §11.7 的 `{op: draw}` 也不是 `draw: {..}`。
pub fn parse_program(yaml: &str) -> Result<EffectProgram, IrError> {
    let program: EffectProgram = serde_yaml::from_str(yaml)?;
    validate_program(&program)?;
    Ok(program)
}

/// 解析后的字段校验(`parse_program` 与 `RuleSet::load_dir` 共用)。
pub(crate) fn validate_program(program: &EffectProgram) -> Result<(), IrError> {
    if program.schema != IR_SCHEMA_VERSION {
        return Err(IrError::UnsupportedSchema {
            found: program.schema,
            supported: IR_SCHEMA_VERSION,
        });
    }
    if !(0.0..=1.0).contains(&program.confidence) {
        return Err(IrError::Invalid(format!(
            "confidence 必须属于 [0, 1],实际为 {}",
            program.confidence
        )));
    }
    if let Some(max) = program.build_range.max {
        if program.build_range.min > max {
            return Err(IrError::Invalid(format!(
                "build_range.min 不能大于 max(min={}, max={})",
                program.build_range.min.0, max.0
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod parse_tests;
