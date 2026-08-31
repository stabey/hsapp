//! `hs-pools` — 发现池与随机卡牌池引擎(设计文档 §12,M3 Track B)。
//!
//! 核心模型:`PoolSpec + CardDatabase(Build) + GameContext + SourceEntity +
//! Observed/Sim State = PoolSnapshot`(§12.1)。
//!
//! 依赖约定:不直接依赖 SQLite——数据库查询走 [`CardQuery`] trait,
//! 由 `hs-card-db` 提供适配器实现;动态池只读 `ObservedGameState`。
//! 公开接口无平台特定类型。

mod context;
mod resolve;
mod sets;
mod snapshot;
mod spec;

pub use context::{context_hash, ContextKey, PoolContext};
pub use resolve::{
    assemble_card_filter, card_matches_filter, resolve, CardFilter, CardMeta, CardQuery,
};
pub use sets::{legal_card_sets, CLASSIC_SETS, STANDARD_ROTATION_SETS};
pub use snapshot::{PoolSnapshot, PoolTraceEntry, WeightedCard};
pub use spec::{PoolFilter, PoolKind, PoolSpec, SelectionRule, WeightRule};

use thiserror::Error;

/// 池域错误。
#[derive(Debug, Error)]
pub enum PoolError {
    #[error("池定义解析失败: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("字段校验失败: {0}")]
    Invalid(String),
    #[error("池 {pool_id} 需要状态快照(动态池)但未提供")]
    StateRequired { pool_id: hs_types::PoolId },
    #[error("卡牌库查询失败: {0}")]
    CardQuery(String),
    #[error("尚未实现: {0}")]
    NotImplemented(&'static str),
}

/// 解析单个 PoolSpec 的 YAML 文本(`rules/pools/<pool_id>.yaml`)。
///
/// 反序列化后校验:`selection.picks >= 1`;
/// `Explicit` / `Entourage` 要求 `include` 非空。
pub fn parse_spec(yaml: &str) -> Result<PoolSpec, PoolError> {
    let spec: PoolSpec = serde_yaml::from_str(yaml)?;
    spec.validate()?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_types::{BuildId, CardId, PoolId, RuleStatus};

    const EXPLICIT_YAML: &str = r#"
pool_id: explicit.entourage
source_card_id: EX1_116
build_range:
  min: 249896
kind: explicit
selection:
  picks: 1
include:
  - CS2_122
  - NEW1_019
weight_rule: uniform
status: verified
"#;

    #[test]
    fn parse_spec_explicit_round_trip() {
        let spec = parse_spec(EXPLICIT_YAML).expect("valid yaml");
        assert_eq!(spec.pool_id, PoolId::new("explicit.entourage"));
        assert_eq!(spec.source_card_id, CardId::new("EX1_116"));
        assert_eq!(spec.build_range.min, BuildId(249896));
        assert_eq!(spec.kind, PoolKind::Explicit);
        assert_eq!(spec.selection.picks, 1);
        assert!(spec.selection.distinct);
        assert_eq!(spec.include.len(), 2);
        assert_eq!(spec.status, RuleStatus::Verified);
        assert_eq!(spec.weight_rule, WeightRule::Uniform);

        let yaml = serde_yaml::to_string(&spec).expect("serialize");
        let again = parse_spec(&yaml).expect("round-trip");
        assert_eq!(spec, again);
    }

    #[test]
    fn parse_spec_rejects_zero_picks() {
        let yaml = r#"
pool_id: bad.picks
source_card_id: CS2_029
build_range:
  min: 1
kind: discover
selection:
  picks: 0
status: inferred
"#;
        let err = parse_spec(yaml).expect_err("picks=0");
        match err {
            PoolError::Invalid(msg) => assert!(msg.contains("picks"), "{msg}"),
            other => panic!("expected Invalid, got {other}"),
        }
    }

    #[test]
    fn parse_spec_rejects_empty_include_for_explicit() {
        let yaml = r#"
pool_id: bad.explicit
source_card_id: CS2_029
build_range:
  min: 1
kind: explicit
selection:
  picks: 1
status: inferred
"#;
        let err = parse_spec(yaml).expect_err("empty include");
        match err {
            PoolError::Invalid(msg) => assert!(msg.contains("include"), "{msg}"),
            other => panic!("expected Invalid, got {other}"),
        }
    }

    #[test]
    fn parse_spec_rejects_unknown_field() {
        let yaml = r#"
pool_id: bad.unknown
source_card_id: CS2_029
build_range:
  min: 1
kind: discover
selection:
  picks: 3
status: inferred
mystery: true
"#;
        let err = parse_spec(yaml).expect_err("unknown field");
        assert!(matches!(err, PoolError::Yaml(_)), "{err}");
    }

    #[test]
    fn parse_spec_filter_yaml_uses_tags() {
        let spec = parse_spec(
            r#"
pool_id: tagged.filters
source_card_id: CS2_023
build_range:
  min: 1
kind: discover
filters:
  - format_legal
  - !card_type SPELL
  - !class_matches
    include_neutral: true
  - !collectible true
  - exclude_source
selection:
  picks: 3
status: inferred
"#,
        )
        .expect("tagged filters");
        assert_eq!(spec.filters.len(), 5);
        assert_eq!(spec.filters[1], PoolFilter::CardType("SPELL".into()));
        assert_eq!(
            spec.filters[2],
            PoolFilter::ClassMatches {
                include_neutral: true
            }
        );
    }

    #[test]
    fn parse_spec_weight_rule_explicit() {
        let yaml = r#"
pool_id: weighted
source_card_id: CS2_023
build_range:
  min: 1
kind: discover
selection:
  picks: 3
weight_rule: !explicit
  - [CS2_029, 2.0]
  - [CS2_032, 0.5]
status: verified
"#;
        let spec = parse_spec(yaml).expect("weighted yaml");
        match spec.weight_rule {
            WeightRule::Explicit(table) => {
                assert_eq!(table.len(), 2);
                assert_eq!(table[0].0, CardId::new("CS2_029"));
                assert_eq!(table[0].1, 2.0);
            }
            other => panic!("expected Explicit, got {other:?}"),
        }
    }
}
