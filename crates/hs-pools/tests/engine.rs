//! §19.6 清单:固定池 / 格式 / 职业 / 动态 / include-exclude / 去重 / 权重 / Trace / 错误。

mod common;

use common::{friendly_state, mage_context, put_entity, MemoryCatalog};
use hs_observed_state::ObservedGameState;
use hs_pools::{
    assemble_card_filter, parse_spec, resolve, CardQuery, PoolError, PoolKind, PoolTraceEntry,
    WeightRule,
};
use hs_types::{CardId, EntityId, Zone};

fn catalog() -> MemoryCatalog {
    MemoryCatalog::hearthstone_subset()
}

#[test]
fn explicit_include_is_the_candidate_set() {
    let spec = parse_spec(
        r#"
pool_id: explicit.entourage
source_card_id: EX1_116
build_range: { min: 249896 }
kind: explicit
selection: { picks: 1 }
include: [CS2_122, NEW1_019, EX1_066]
status: verified
"#,
    )
    .unwrap();
    let snap = resolve(&spec, &mage_context("EX1_116"), &catalog(), None).unwrap();
    let ids: Vec<_> = snap.candidates.iter().map(|c| c.card_id.as_str()).collect();
    assert_eq!(ids, ["CS2_122", "EX1_066", "NEW1_019"]);
    assert!(snap.exact);
    assert!(snap.candidates.iter().all(|c| c.weight == 1.0));
}

#[test]
fn format_legal_drops_legacy_and_naxx() {
    let spec = parse_spec(
        r#"
pool_id: discover.format
source_card_id: CS2_023
build_range: { min: 249896 }
kind: discover
filters:
  - format_legal
selection: { picks: 3 }
status: inferred
"#,
    )
    .unwrap();
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), None).unwrap();
    assert!(snap.candidates.iter().all(|c| !matches!(
        c.card_id.as_str(),
        "EX1_116" | "CS2_142" | "CS2_213" | "FP1_004" | "EX1_279"
    )));
    assert!(snap
        .candidates
        .iter()
        .any(|c| c.card_id.as_str() == "CS2_029"));
    let format_step = snap
        .trace
        .iter()
        .find(|e| e.step == "format_legal")
        .expect("format_legal step");
    assert_eq!(format_step.removed, 5);
}

#[test]
fn class_matches_keeps_mage_and_optional_neutral() {
    let spec = parse_spec(
        r#"
pool_id: discover.mage
source_card_id: CS2_023
build_range: { min: 249896 }
kind: random_spell
filters:
  - !class_matches
    include_neutral: true
  - !collectible true
selection: { picks: 3 }
status: inferred
"#,
    )
    .unwrap();
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), None).unwrap();
    let ids: Vec<_> = snap.candidates.iter().map(|c| c.card_id.as_str()).collect();
    assert!(ids.contains(&"CS2_029"));
    assert!(ids.contains(&"EX1_279"));
    assert!(!ids.contains(&"EX1_246"));
    assert!(!ids.contains(&"CS2_062"));
}

#[test]
fn class_matches_without_neutral_drops_neutral() {
    let spec = parse_spec(
        r#"
pool_id: discover.mage_only
source_card_id: CS2_023
build_range: { min: 249896 }
kind: random_minion
filters:
  - !class_matches
    include_neutral: false
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), None).unwrap();
    assert!(snap.candidates.is_empty());
}

#[test]
fn dynamic_graveyard_reads_state() {
    let spec = parse_spec(
        r#"
pool_id: history.graveyard
source_card_id: CS2_023
build_range: { min: 249896 }
kind: from_graveyard
context_inputs: [graveyard]
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let mut state = friendly_state();
    put_entity(&mut state, 10, Some("CS2_029"), Zone::Graveyard, Some(1));
    put_entity(&mut state, 11, Some("CS2_122"), Zone::Graveyard, Some(2));
    put_entity(&mut state, 12, Some("CS2_023"), Zone::Hand, Some(1));
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), Some(&state)).unwrap();
    let ids: Vec<_> = snap.candidates.iter().map(|c| c.card_id.as_str()).collect();
    assert_eq!(ids, ["CS2_029", "CS2_122"]);
    assert!(snap.exact);
}

#[test]
fn dynamic_played_and_generated_history() {
    let played = parse_spec(
        r#"
pool_id: history.played
source_card_id: CS2_023
build_range: { min: 249896 }
kind: from_played_history
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let generated = parse_spec(
        r#"
pool_id: history.generated
source_card_id: CS2_023
build_range: { min: 249896 }
kind: from_generated_history
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let mut state = friendly_state();
    put_entity(&mut state, 20, Some("NEW1_019"), Zone::Play, Some(1));
    let mut created =
        hs_observed_state::EntityState::new(EntityId(21), Some(CardId::new("EX1_277")), Zone::Hand);
    created.controller = Some(hs_types::PlayerId(1));
    created.source_entity = Some(EntityId(20));
    state.entities.insert(created.entity_id, created);

    let played_snap = resolve(&played, &mage_context("CS2_023"), &catalog(), Some(&state)).unwrap();
    assert_eq!(played_snap.candidates.len(), 1);
    assert_eq!(played_snap.candidates[0].card_id.as_str(), "NEW1_019");

    let gen_snap = resolve(
        &generated,
        &mage_context("CS2_023"),
        &catalog(),
        Some(&state),
    )
    .unwrap();
    assert_eq!(gen_snap.candidates.len(), 1);
    assert_eq!(gen_snap.candidates[0].card_id.as_str(), "EX1_277");
}

#[test]
fn include_exclude_and_weights() {
    let spec = parse_spec(
        r#"
pool_id: discover.mix
source_card_id: CS2_023
build_range: { min: 249896 }
kind: random_spell
filters:
  - !collectible true
include: [CS2_122]
exclude: [EX1_279]
weight_rule: !explicit
  - [CS2_029, 2.5]
selection: { picks: 3 }
status: inferred
"#,
    )
    .unwrap();
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), None).unwrap();
    let ids: Vec<_> = snap.candidates.iter().map(|c| c.card_id.as_str()).collect();
    assert!(ids.contains(&"CS2_122"), "include 并入随从");
    assert!(!ids.contains(&"EX1_279"), "exclude 移除");
    let fireball = snap
        .candidates
        .iter()
        .find(|c| c.card_id.as_str() == "CS2_029")
        .unwrap();
    assert_eq!(fireball.weight, 2.5);
    let intellect = snap
        .candidates
        .iter()
        .find(|c| c.card_id.as_str() == "CS2_023")
        .unwrap();
    assert_eq!(intellect.weight, 1.0);
    assert!(snap.trace.iter().any(|e| e.step == "include"));
    assert!(snap.trace.iter().any(|e| e.step == "exclude"));
}

#[test]
fn dedup_same_card_id_in_graveyard() {
    let spec = parse_spec(
        r#"
pool_id: history.dedup
source_card_id: CS2_023
build_range: { min: 249896 }
kind: from_graveyard
selection: { picks: 2 }
status: inferred
"#,
    )
    .unwrap();
    let mut state = friendly_state();
    put_entity(&mut state, 10, Some("CS2_029"), Zone::Graveyard, Some(1));
    put_entity(&mut state, 11, Some("CS2_029"), Zone::Graveyard, Some(1));
    put_entity(&mut state, 12, Some("CS2_032"), Zone::Graveyard, Some(1));
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), Some(&state)).unwrap();
    let ids: Vec<_> = snap.candidates.iter().map(|c| c.card_id.as_str()).collect();
    assert_eq!(ids, ["CS2_029", "CS2_032"]);
    assert_eq!(
        snap.trace[0],
        PoolTraceEntry {
            step: "initial".into(),
            removed: 0,
            remaining: 2,
        }
    );
}

#[test]
fn trace_counts_are_accurate_per_step() {
    let spec = parse_spec(
        r#"
pool_id: discover.trace
source_card_id: CS2_023
build_range: { min: 249896 }
kind: discover
filters:
  - !card_type SPELL
  - !class_matches
    include_neutral: false
  - exclude_source
selection: { picks: 3 }
status: inferred
"#,
    )
    .unwrap();
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), None).unwrap();
    assert_eq!(snap.trace[0].step, "initial");
    assert_eq!(snap.trace[0].removed, 0);
    let catalog_len = catalog()
        .query(common::BUILD, &hs_pools::CardFilter::default())
        .unwrap()
        .len();
    assert_eq!(snap.trace[0].remaining, catalog_len);
    let type_step = &snap.trace[1];
    assert_eq!(type_step.step, "card_type=SPELL");
    assert_eq!(type_step.removed + type_step.remaining, catalog_len);
    let class_step = &snap.trace[2];
    assert_eq!(class_step.step, "class_matches");
    assert_eq!(
        class_step.removed + class_step.remaining,
        type_step.remaining
    );
    let exclude = &snap.trace[3];
    assert_eq!(exclude.step, "exclude_source");
    assert_eq!(exclude.removed, 1);
    assert_eq!(exclude.remaining + 1, class_step.remaining);
}

#[test]
fn state_required_when_dynamic_kind_has_no_state() {
    let spec = parse_spec(
        r#"
pool_id: history.need_state
source_card_id: CS2_023
build_range: { min: 249896 }
kind: from_hand
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let err = resolve(&spec, &mage_context("CS2_023"), &catalog(), None).unwrap_err();
    match err {
        PoolError::StateRequired { pool_id } => {
            assert_eq!(pool_id.as_str(), "history.need_state");
        }
        other => panic!("expected StateRequired, got {other}"),
    }
}

#[test]
fn deck_unknown_faces_mark_exact_false() {
    let spec = parse_spec(
        r#"
pool_id: deck.partial
source_card_id: CS2_023
build_range: { min: 249896 }
kind: from_deck
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let mut state = friendly_state();
    put_entity(&mut state, 30, Some("CS2_029"), Zone::Deck, Some(1));
    put_entity(&mut state, 31, None, Zone::Deck, Some(1));
    put_entity(&mut state, 32, None, Zone::Deck, Some(2));
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), Some(&state)).unwrap();
    assert!(!snap.exact);
    assert_eq!(snap.candidates.len(), 1);
    assert_eq!(snap.candidates[0].card_id.as_str(), "CS2_029");
}

#[test]
fn opponent_hand_unknown_does_not_enter_friendly_from_hand() {
    let spec = parse_spec(
        r#"
pool_id: hand.friendly
source_card_id: CS2_023
build_range: { min: 249896 }
kind: from_hand
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let mut state = friendly_state();
    put_entity(&mut state, 40, Some("CS2_032"), Zone::Hand, Some(1));
    put_entity(&mut state, 41, None, Zone::Hand, Some(2));
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), Some(&state)).unwrap();
    assert!(snap.exact);
    assert_eq!(snap.candidates.len(), 1);
    assert_eq!(snap.candidates[0].card_id.as_str(), "CS2_032");
}

#[test]
fn resolve_is_byte_stable() {
    let spec = parse_spec(
        r#"
pool_id: discover.stable
source_card_id: CS2_023
build_range: { min: 249896 }
kind: discover
filters:
  - format_legal
  - !card_type SPELL
selection: { picks: 3 }
status: inferred
"#,
    )
    .unwrap();
    let ctx = mage_context("CS2_023");
    let a = resolve(&spec, &ctx, &catalog(), None).unwrap();
    let b = resolve(&spec, &ctx, &catalog(), None).unwrap();
    let ja = serde_json::to_vec(&a).unwrap();
    let jb = serde_json::to_vec(&b).unwrap();
    assert_eq!(ja, jb);
}

#[test]
fn assemble_card_filter_pushes_kind_and_format() {
    let spec = parse_spec(
        r#"
pool_id: assemble.demo
source_card_id: CS2_023
build_range: { min: 249896 }
kind: random_minion
filters:
  - format_legal
  - !class_matches
    include_neutral: true
  - !collectible true
selection: { picks: 1 }
status: inferred
"#,
    )
    .unwrap();
    let filter = assemble_card_filter(&spec, &mage_context("CS2_023"));
    assert_eq!(filter.card_type.as_deref(), Some("MINION"));
    assert_eq!(filter.collectible, Some(true));
    assert!(filter.card_classes.contains(&"MAGE".into()));
    assert!(filter.card_classes.contains(&"NEUTRAL".into()));
    assert!(filter.card_sets.contains(&"CORE".into()));
}

#[test]
fn empty_state_is_not_used_for_discover() {
    let spec = parse_spec(
        r#"
pool_id: discover.no_state
source_card_id: CS2_023
build_range: { min: 249896 }
kind: discover
selection: { picks: 3 }
status: inferred
"#,
    )
    .unwrap();
    let snap = resolve(
        &spec,
        &mage_context("CS2_023"),
        &catalog(),
        Some(&ObservedGameState::default()),
    )
    .unwrap();
    assert!(snap.exact);
    assert!(!snap.candidates.is_empty());
}

#[test]
fn entourage_kind_uses_include() {
    let spec = parse_spec(
        r#"
pool_id: entourage.demo
source_card_id: EX1_116
build_range: { min: 249896 }
kind: entourage
selection: { picks: 1 }
include: [CS2_168]
status: verified
"#,
    )
    .unwrap();
    assert_eq!(spec.kind, PoolKind::Entourage);
    let snap = resolve(&spec, &mage_context("EX1_116"), &catalog(), None).unwrap();
    assert_eq!(snap.candidates.len(), 1);
    assert_eq!(snap.candidates[0].card_id.as_str(), "CS2_168");
}

#[test]
fn weight_rule_enum_uniform_default() {
    let spec = parse_spec(
        r#"
pool_id: weights.default
source_card_id: CS2_023
build_range: { min: 249896 }
kind: explicit
selection: { picks: 1 }
include: [CS2_029]
status: verified
"#,
    )
    .unwrap();
    assert_eq!(spec.weight_rule, WeightRule::Uniform);
}
