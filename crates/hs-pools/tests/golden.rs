//! Golden Snapshot 基建:PoolSnapshot pretty JSON + 字段级比较器。
//!
//! `UPDATE_GOLDEN=1 cargo test -p hs-pools --test golden` 重写 `tests/golden/*.json`。

mod common;

use common::{friendly_state, mage_context, put_entity, MemoryCatalog};
use hs_pools::{parse_spec, resolve, PoolSnapshot};
use hs_types::{CardId, Zone};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn catalog() -> MemoryCatalog {
    MemoryCatalog::hearthstone_subset()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"))
}

fn update_golden() -> bool {
    matches!(std::env::var("UPDATE_GOLDEN"), Ok(v) if v == "1")
}

/// 反序列化 golden 与实际逐字段比对;差异用人话列出。
fn diff_snapshots(expected: &PoolSnapshot, actual: &PoolSnapshot) -> Vec<String> {
    let mut diffs = Vec::new();
    if expected.pool_id != actual.pool_id {
        diffs.push(format!(
            "pool_id: 期望 {} 实际 {}",
            expected.pool_id, actual.pool_id
        ));
    }
    if expected.build_id != actual.build_id {
        diffs.push(format!(
            "build_id: 期望 {:?} 实际 {:?}",
            expected.build_id, actual.build_id
        ));
    }
    if expected.context_hash != actual.context_hash {
        diffs.push(format!(
            "context_hash: 期望 {} 实际 {}",
            expected.context_hash, actual.context_hash
        ));
    }
    if expected.exact != actual.exact {
        diffs.push(format!(
            "exact: 期望 {} 实际 {}",
            expected.exact, actual.exact
        ));
    }

    let exp: BTreeMap<&CardId, f64> = expected
        .candidates
        .iter()
        .map(|c| (&c.card_id, c.weight))
        .collect();
    let act: BTreeMap<&CardId, f64> = actual
        .candidates
        .iter()
        .map(|c| (&c.card_id, c.weight))
        .collect();

    for id in act.keys() {
        if !exp.contains_key(id) {
            diffs.push(format!("新增候选: {id}"));
        }
    }
    for id in exp.keys() {
        if !act.contains_key(id) {
            diffs.push(format!("移除候选: {id}"));
        }
    }
    for (id, old) in &exp {
        if let Some(new) = act.get(id) {
            if (old - *new).abs() > f64::EPSILON {
                diffs.push(format!("权重变化: {id} {old} → {new}"));
            }
        }
    }

    if expected.trace != actual.trace {
        diffs.push(format!(
            "trace 变化: 期望 {:?} 实际 {:?}",
            expected.trace, actual.trace
        ));
    }
    diffs
}

fn assert_golden(name: &str, actual: &PoolSnapshot) {
    let path = golden_path(name);
    let pretty = serde_json::to_string_pretty(actual).expect("serialize snapshot");
    if update_golden() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create golden dir");
        }
        fs::write(&path, pretty + "\n").expect("write golden");
        return;
    }
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "读取 golden {}: {e}; 可设 UPDATE_GOLDEN=1 生成",
            path.display()
        )
    });
    let expected: PoolSnapshot = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("golden JSON 损坏 {}: {e}", path.display()));
    let diffs = diff_snapshots(&expected, actual);
    assert!(
        diffs.is_empty(),
        "golden `{name}` 不一致:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn golden_explicit_pool() {
    let spec = parse_spec(
        r#"
pool_id: golden.explicit
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
    assert_golden("explicit", &snap);
}

#[test]
fn golden_format_and_class_pool() {
    let spec = parse_spec(
        r#"
pool_id: golden.format_class
source_card_id: CS2_023
build_range: { min: 249896 }
kind: discover
filters:
  - format_legal
  - !card_type SPELL
  - !class_matches
    include_neutral: true
  - !collectible true
  - exclude_source
selection: { picks: 3 }
status: inferred
"#,
    )
    .unwrap();
    let snap = resolve(&spec, &mage_context("CS2_023"), &catalog(), None).unwrap();
    assert_eq!(
        snap.candidates
            .iter()
            .map(|c| c.card_id.as_str())
            .collect::<Vec<_>>(),
        ["CS2_029", "CS2_032", "EX1_277"]
    );
    assert_golden("format_class", &snap);
}

#[test]
fn golden_dynamic_graveyard_pool() {
    let spec = parse_spec(
        r#"
pool_id: golden.graveyard
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
    put_entity(&mut state, 11, Some("CS2_122"), Zone::Graveyard, Some(1));
    put_entity(&mut state, 12, Some("EX1_116"), Zone::Graveyard, Some(2));
    let mut ctx = mage_context("CS2_023");
    ctx.state_revision = state.revision;
    let snap = resolve(&spec, &ctx, &catalog(), Some(&state)).unwrap();
    assert_golden("from_graveyard", &snap);
}

#[test]
fn comparator_reports_added_removed_weight_and_trace() {
    let spec = parse_spec(
        r#"
pool_id: golden.explicit
source_card_id: EX1_116
build_range: { min: 249896 }
kind: explicit
selection: { picks: 1 }
include: [CS2_122, NEW1_019]
status: verified
"#,
    )
    .unwrap();
    let actual = resolve(&spec, &mage_context("EX1_116"), &catalog(), None).unwrap();
    let mut expected = actual.clone();
    expected
        .candidates
        .retain(|c| c.card_id.as_str() != "NEW1_019");
    expected.candidates.push(hs_pools::WeightedCard {
        card_id: CardId::new("CS2_168"),
        dbf_id: None,
        weight: 1.0,
    });
    expected.candidates[0].weight = 3.0;
    expected.trace.clear();
    let diffs = diff_snapshots(&expected, &actual);
    assert!(
        diffs.iter().any(|d| d.starts_with("新增候选: NEW1_019")),
        "{diffs:?}"
    );
    assert!(
        diffs.iter().any(|d| d.starts_with("移除候选: CS2_168")),
        "{diffs:?}"
    );
    assert!(
        diffs.iter().any(|d| d.starts_with("权重变化:")),
        "{diffs:?}"
    );
    assert!(
        diffs.iter().any(|d| d.starts_with("trace 变化:")),
        "{diffs:?}"
    );
}
