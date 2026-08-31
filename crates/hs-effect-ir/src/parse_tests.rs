//! Track A:Effect IR 解析、校验与按 build 选版本。
//!
//! 冻结的 `Operation`/`CardRef` 是外部标签枚举;serde_yaml 0.9 要求
//! `!draw` / `!id` 这种 YAML tag,§11.7 的 `op: draw` 写法无法直接反序列化。

use crate::{
    parse_program, CardRef, EffectProgram, EntityCondition, EntityRole, EvidenceKind, IrError,
    Operation, PlayerRef, Predicate, RuleSet, RuleStatus, Selector, TriggerEvent,
};
use hs_types::{BuildId, BuildRange, CardId, PoolId};

const EXAMPLE_DRAW_AFTER_HERO_ATTACK: &str = r#"
schema: 1
card_id: EXAMPLE_DRAW_AFTER_HERO_ATTACK
build_range:
  min: 123456
status: verified
confidence: 0.9
triggers:
  - event: after_attack
    when:
      - subject: attacker
        condition:
          controller: self_player
          card_type: HERO
    effects:
      - !draw
        player: self_player
        count: 1
evidence:
  - kind: manual
    reference: design §11.7
    date: "2026-08-23"
"#;

const DISCOVER_SPELL: &str = r#"
schema: 1
card_id: CS2_029
build_range:
  min: 1
status: inferred
play:
  - !discover
    pool: discover.mage_spells
    options: 3
    count: 1
"#;

fn fixture_rules_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rules")
}

fn minimal_program(card: &str, min: u32, max: Option<u32>) -> EffectProgram {
    EffectProgram {
        schema: 1,
        card_id: CardId::new(card),
        build_range: BuildRange {
            min: BuildId(min),
            max: max.map(BuildId),
        },
        status: RuleStatus::Verified,
        confidence: 1.0,
        triggers: Vec::new(),
        play: Vec::new(),
        custom_handler: None,
        evidence: Vec::new(),
    }
}

#[test]
fn parse_section_11_7_style_example() {
    let program = parse_program(EXAMPLE_DRAW_AFTER_HERO_ATTACK).expect("合法示例应解析成功");

    assert_eq!(program.schema, 1);
    assert_eq!(
        program.card_id,
        CardId::new("EXAMPLE_DRAW_AFTER_HERO_ATTACK")
    );
    assert_eq!(program.build_range.min, BuildId(123456));
    assert_eq!(program.build_range.max, None);
    assert_eq!(program.status, RuleStatus::Verified);
    assert_eq!(program.confidence, 0.9);
    assert!(program.play.is_empty());
    assert!(program.custom_handler.is_none());

    assert_eq!(program.triggers.len(), 1);
    let trigger = &program.triggers[0];
    assert_eq!(trigger.event, TriggerEvent::AfterAttack);
    assert_eq!(trigger.when.len(), 1);
    assert_eq!(
        trigger.when[0],
        Predicate {
            subject: EntityRole::Attacker,
            condition: EntityCondition {
                controller: Some(PlayerRef::SelfPlayer),
                card_type: Some("HERO".into()),
                ..EntityCondition::default()
            },
        }
    );
    assert_eq!(
        trigger.effects,
        vec![Operation::Draw {
            player: PlayerRef::SelfPlayer,
            count: 1,
        }]
    );

    assert_eq!(program.evidence.len(), 1);
    assert_eq!(program.evidence[0].kind, EvidenceKind::Manual);
    assert_eq!(program.evidence[0].reference, "design §11.7");
    assert_eq!(program.evidence[0].date.as_deref(), Some("2026-08-23"));
}

#[test]
fn parse_discover_yaml_round_trip() {
    let first = parse_program(DISCOVER_SPELL).expect("Discover 法术应解析成功");
    assert_eq!(
        first.play,
        vec![Operation::Discover {
            pool: PoolId::new("discover.mage_spells"),
            options: 3,
            count: 1,
        }]
    );

    let yaml = serde_yaml::to_string(&first).expect("序列化不应失败");
    let second = parse_program(&yaml).expect("往返后再解析应成功");
    assert_eq!(first, second);
}

#[test]
fn parse_rejects_unknown_field() {
    let err = parse_program(
        r#"
schema: 1
card_id: CS2_029
build_range:
  min: 1
status: verified
unknown_field: true
"#,
    )
    .expect_err("未知字段应失败");
    assert!(
        matches!(err, IrError::Yaml(_)),
        "未知字段应是 Yaml 变体,实际为 {err:?}"
    );
}

#[test]
fn parse_rejects_unsupported_schema() {
    let err = parse_program(
        r#"
schema: 2
card_id: CS2_029
build_range:
  min: 1
status: verified
"#,
    )
    .expect_err("schema=2 应失败");
    match err {
        IrError::UnsupportedSchema {
            found: 2,
            supported: 1,
        } => {}
        other => panic!("期望 UnsupportedSchema{{found:2, supported:1}},实际 {other:?}"),
    }
}

#[test]
fn parse_rejects_confidence_out_of_range() {
    let err = parse_program(
        r#"
schema: 1
card_id: CS2_029
build_range:
  min: 1
status: verified
confidence: 1.5
"#,
    )
    .expect_err("confidence=1.5 应失败");
    match err {
        IrError::Invalid(message) => {
            assert!(
                message.contains("confidence"),
                "错误消息应点名 confidence: {message}"
            );
        }
        other => panic!("期望 Invalid,实际 {other:?}"),
    }
}

#[test]
fn parse_rejects_inverted_build_range() {
    let err = parse_program(
        r#"
schema: 1
card_id: CS2_029
build_range:
  min: 200
  max: 100
status: verified
"#,
    )
    .expect_err("min>max 应失败");
    match err {
        IrError::Invalid(message) => {
            assert!(
                message.contains("build_range"),
                "错误消息应点名 build_range: {message}"
            );
        }
        other => panic!("期望 Invalid,实际 {other:?}"),
    }
}

#[test]
fn ruleset_selects_latest_min_among_matching_ranges() {
    let mut set = RuleSet::default();
    set.insert(minimal_program("CS2_029", 100, Some(200)))
        .expect("第一段应插入");
    set.insert(minimal_program("CS2_029", 201, Some(300)))
        .expect("相邻第二段应插入");

    let card = CardId::new("CS2_029");
    let early = set.select(BuildId(150), &card).expect("150 应命中第一段");
    assert_eq!(early.build_range.min, BuildId(100));

    let late = set.select(BuildId(201), &card).expect("201 应命中第二段");
    assert_eq!(late.build_range.min, BuildId(201));

    assert!(set.select(BuildId(50), &card).is_none());
    assert!(set.select(BuildId(300), &card).is_some());
    assert_eq!(set.len(), 1);
}

#[test]
fn ruleset_insert_rejects_overlapping_ranges() {
    let mut set = RuleSet::default();
    set.insert(minimal_program("CS2_029", 100, Some(200)))
        .expect("第一段应插入");
    let err = set
        .insert(minimal_program("CS2_029", 150, Some(250)))
        .expect_err("重叠区间应报 DuplicateRule");
    match err {
        IrError::DuplicateRule { card_id } => {
            assert_eq!(card_id, CardId::new("CS2_029"));
        }
        other => panic!("期望 DuplicateRule,实际 {other:?}"),
    }
}

#[test]
fn load_dir_reads_two_files_including_multi_document() {
    let set = RuleSet::load_dir(&fixture_rules_dir()).expect("fixtures 应加载成功");
    assert_eq!(set.len(), 2, "应加载 CS2_029 与 CS2_023 两张卡");

    let fireball = set
        .select(BuildId(249896), &CardId::new("CS2_029"))
        .expect("火球术应命中");
    assert_eq!(fireball.status, RuleStatus::Verified);
    assert_eq!(
        fireball.play,
        vec![Operation::Damage {
            target: Selector::Target,
            amount: 6,
        }]
    );

    let intellect_old = set
        .select(BuildId(100), &CardId::new("CS2_023"))
        .expect("奥术智慧旧区间应命中");
    assert_eq!(intellect_old.status, RuleStatus::Verified);
    assert_eq!(intellect_old.build_range.max, Some(BuildId(200000)));

    let intellect_new = set
        .select(BuildId(200001), &CardId::new("CS2_023"))
        .expect("奥术智慧新区间应命中");
    assert_eq!(intellect_new.status, RuleStatus::Inferred);
    assert_eq!(intellect_new.confidence, 0.7);
}

#[test]
fn parse_summon_with_card_ref_id() {
    let program = parse_program(
        r#"
schema: 1
card_id: CS2_033
build_range:
  min: 1
status: partial
play:
  - !summon
    player: self_player
    card: !id CS2_033
    count: 2
"#,
    )
    .expect("Summon + CardRef::Id 应解析成功");
    assert_eq!(program.status, RuleStatus::Partial);
    assert_eq!(
        program.play,
        vec![Operation::Summon {
            player: PlayerRef::SelfPlayer,
            card: CardRef::Id(CardId::new("CS2_033")),
            count: 2,
        }]
    );
}
