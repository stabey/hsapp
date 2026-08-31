//! Discover / Sampled / 可复现 / Unsupported 路径。

mod common;

use common::{default_state, pool_snapshot, program, FixedPool};
use hs_effect_ir::Operation;
use hs_sim::{
    apply_program, ChanceKind, OutcomeSet, ReproMeta, SampleMethod, SimRequest, TargetBinding,
    UnsupportedReason,
};
use hs_types::{CardId, HandlerId, PlayerId, PoolId};

fn discover_op() -> Operation {
    Operation::Discover {
        pool: PoolId::new("discover.test"),
        options: 3,
        count: 1,
    }
}

fn random_choice(count: u8) -> Operation {
    Operation::RandomChoice {
        pool: PoolId::new("random.test"),
        count,
    }
}

#[test]
fn discover_exact_enumerates_three_uniform_branches() {
    let given = default_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("discover.test", &["AAA_01", "BBB_02", "CCC_03"]),
    };
    let outcome = apply_program(
        &given,
        &program("DISCOVER_CARD", vec![discover_op()]),
        &TargetBinding::default(),
        &pools,
        &SimRequest {
            seed: 7,
            branch_budget: 64,
        },
    )
    .unwrap();

    let OutcomeSet::Exact(branches) = &outcome.outcome else {
        panic!("expected Exact, got {:?}", outcome.outcome);
    };
    assert_eq!(branches.len(), 3);
    for branch in branches {
        assert!((branch.probability - 1.0 / 3.0).abs() < 1e-12);
        let p1 = branch.state.player(PlayerId(1)).unwrap();
        assert_eq!(
            p1.hand_count,
            given.player(PlayerId(1)).unwrap().hand_count + 1
        );
        let note = branch.note.as_deref().expect("note");
        assert!(
            ["AAA_01", "BBB_02", "CCC_03"].contains(&note),
            "note={note}"
        );
        let has_card = branch.state.entities.values().any(|e| {
            e.simulated
                && e.zone == hs_types::Zone::Hand
                && e.card_id.as_ref().map(CardId::as_str) == Some(note)
        });
        assert!(has_card, "branch missing card {note}");
    }
    let notes: Vec<_> = branches.iter().map(|b| b.note.clone().unwrap()).collect();
    let mut sorted = notes.clone();
    sorted.sort();
    assert_eq!(sorted, ["AAA_01", "BBB_02", "CCC_03"]);
    assert_eq!(outcome.meta.method, SampleMethod::Exact);
    assert_eq!(outcome.meta.chance_kind, Some(ChanceKind::PlayerChoice));
    assert!(outcome.meta.truncation.is_none());
    assert_eq!(outcome.meta.pool_snapshot_hashes, ["hash-discover.test"]);
    assert_eq!(outcome.trace.entries[0].op, "discover");
}

#[test]
fn random_choice_over_budget_is_sampled() {
    let given = default_state();
    let cards: Vec<String> = (0..20).map(|i| format!("C{i:02}")).collect();
    let refs: Vec<&str> = cards.iter().map(String::as_str).collect();
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &refs),
    };
    let outcome = apply_program(
        &given,
        &program("RANDOM_CARD", vec![random_choice(2)]),
        &TargetBinding::default(),
        &pools,
        &SimRequest {
            seed: 11,
            branch_budget: 4,
        },
    )
    .unwrap();

    let OutcomeSet::Sampled(sampled) = &outcome.outcome else {
        panic!("expected Sampled, got {:?}", outcome.outcome);
    };
    assert_eq!(sampled.samples.len(), 4);
    assert_eq!(sampled.method, SampleMethod::MonteCarlo);
    assert_eq!(sampled.total_branches, Some(190));
    assert_eq!(outcome.meta.method, SampleMethod::MonteCarlo);
    assert_eq!(outcome.meta.chance_kind, Some(ChanceKind::Random));
    assert!(
        outcome
            .meta
            .truncation
            .as_deref()
            .is_some_and(|t| t.contains("branch_budget exceeded")),
        "truncation={:?}",
        outcome.meta.truncation
    );
    for sample in &sampled.samples {
        assert_eq!(sample.state.player(PlayerId(1)).unwrap().hand_count, 2);
        let note = sample.note.as_ref().unwrap();
        assert!(note.contains('+'), "note should list two cards: {note}");
    }
}

#[test]
fn chance_kind_is_independent_from_exact_or_sampled_shape() {
    let random_pools = FixedPool {
        snapshot: pool_snapshot("random.test", &["AAA_01", "BBB_02", "CCC_03"]),
    };
    let random = apply_program(
        &default_state(),
        &program("RANDOM_EXACT", vec![random_choice(1)]),
        &TargetBinding::default(),
        &random_pools,
        &SimRequest {
            seed: 17,
            branch_budget: 64,
        },
    )
    .unwrap();
    assert!(matches!(random.outcome, OutcomeSet::Exact(_)));
    assert_eq!(random.meta.chance_kind, Some(ChanceKind::Random));

    let cards = (0..20)
        .map(|index| format!("D{index:02}"))
        .collect::<Vec<_>>();
    let refs = cards.iter().map(String::as_str).collect::<Vec<_>>();
    let discover_pools = FixedPool {
        snapshot: pool_snapshot("discover.test", &refs),
    };
    let discover = apply_program(
        &default_state(),
        &program("DISCOVER_SAMPLED", vec![discover_op()]),
        &TargetBinding::default(),
        &discover_pools,
        &SimRequest {
            seed: 19,
            branch_budget: 4,
        },
    )
    .unwrap();
    assert!(matches!(discover.outcome, OutcomeSet::Sampled(_)));
    assert_eq!(discover.meta.chance_kind, Some(ChanceKind::PlayerChoice));
}

#[test]
fn deterministic_program_has_no_chance_kind() {
    let outcome = apply_program(
        &default_state(),
        &program("NO_CHANCE", vec![]),
        &TargetBinding::default(),
        &common::EmptyPool,
        &SimRequest::default(),
    )
    .unwrap();

    assert!(matches!(outcome.outcome, OutcomeSet::Deterministic(_)));
    assert_eq!(outcome.meta.chance_kind, None);
}

#[test]
fn chance_kind_serde_contract_is_stable_and_legacy_meta_defaults_to_none() {
    assert_eq!(
        serde_json::to_value(ChanceKind::Random).unwrap(),
        serde_json::json!("random")
    );
    assert_eq!(
        serde_json::to_value(ChanceKind::PlayerChoice).unwrap(),
        serde_json::json!("player_choice")
    );
    let legacy: ReproMeta = serde_json::from_value(serde_json::json!({
        "seed": 7,
        "branch_budget": 64,
        "method": "exact",
        "rule_version": "LEGACY@1..",
        "pool_snapshot_hashes": []
    }))
    .unwrap();
    assert_eq!(legacy.chance_kind, None);
}

#[test]
fn same_seed_is_byte_identical_five_times() {
    let given = default_state();
    let cards: Vec<String> = (0..20).map(|i| format!("C{i:02}")).collect();
    let refs: Vec<&str> = cards.iter().map(String::as_str).collect();
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &refs),
    };
    let req = SimRequest {
        seed: 42,
        branch_budget: 4,
    };
    let prog = program("RANDOM_CARD", vec![random_choice(2)]);

    let runs: Vec<String> = (0..5)
        .map(|_| {
            let outcome =
                apply_program(&given, &prog, &TargetBinding::default(), &pools, &req).unwrap();
            let OutcomeSet::Sampled(sampled) = &outcome.outcome else {
                panic!("expected Sampled");
            };
            let states = serde_json::to_string(&sampled.samples).unwrap();
            let trace = serde_json::to_string(&outcome.trace).unwrap();
            format!("{states}\n{trace}")
        })
        .collect();
    for run in &runs[1..] {
        assert_eq!(run.as_bytes(), runs[0].as_bytes());
    }

    let other = apply_program(
        &given,
        &prog,
        &TargetBinding::default(),
        &pools,
        &SimRequest {
            seed: 99_999,
            branch_budget: 4,
        },
    )
    .unwrap();
    let OutcomeSet::Sampled(other_s) = &other.outcome else {
        panic!("expected Sampled");
    };
    let other_json = serde_json::to_string(&other_s.samples).unwrap();
    let first_json = {
        let first = apply_program(&given, &prog, &TargetBinding::default(), &pools, &req).unwrap();
        let OutcomeSet::Sampled(s) = &first.outcome else {
            panic!("sampled");
        };
        serde_json::to_string(&s.samples).unwrap()
    };
    assert_ne!(other_json, first_json);
}

#[test]
fn exact_is_also_byte_reproducible() {
    let given = default_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("discover.test", &["AAA_01", "BBB_02", "CCC_03"]),
    };
    let prog = program("DISCOVER_CARD", vec![discover_op()]);
    let req = SimRequest {
        seed: 1,
        branch_budget: 8,
    };
    let first = apply_program(&given, &prog, &TargetBinding::default(), &pools, &req).unwrap();
    let OutcomeSet::Exact(a) = &first.outcome else {
        panic!("exact");
    };
    let blob_a = format!(
        "{}\n{}",
        serde_json::to_string(a).unwrap(),
        serde_json::to_string(&first.trace).unwrap()
    );
    for _ in 0..4 {
        let again = apply_program(&given, &prog, &TargetBinding::default(), &pools, &req).unwrap();
        let OutcomeSet::Exact(b) = &again.outcome else {
            panic!("exact");
        };
        let blob_b = format!(
            "{}\n{}",
            serde_json::to_string(b).unwrap(),
            serde_json::to_string(&again.trace).unwrap()
        );
        assert_eq!(blob_a.as_bytes(), blob_b.as_bytes());
    }
}

#[test]
fn unimplemented_ops_report_reason() {
    let given = default_state();
    let cases: Vec<(&str, Operation)> = vec![
        (
            "transform",
            Operation::Transform {
                target: hs_effect_ir::Selector::FriendlyHero,
                into: hs_effect_ir::CardRef::Id(CardId::new("CS2_182")),
            },
        ),
        (
            "silence",
            Operation::Silence {
                target: hs_effect_ir::Selector::FriendlyHero,
            },
        ),
        (
            "add_enchantment",
            Operation::AddEnchantment {
                target: hs_effect_ir::Selector::FriendlyHero,
                enchantment: hs_effect_ir::CardRef::Id(CardId::new("XXX")),
            },
        ),
        (
            "remove_enchantment",
            Operation::RemoveEnchantment {
                target: hs_effect_ir::Selector::FriendlyHero,
                enchantment: None,
            },
        ),
        (
            "cast",
            Operation::Cast {
                card: hs_effect_ir::CardRef::Id(CardId::new("CS2_029")),
                target: None,
            },
        ),
        ("choose_one", Operation::ChooseOne { options: vec![] }),
        (
            "copy",
            Operation::Copy {
                source: hs_effect_ir::Selector::FriendlyHero,
                to_zone: hs_types::Zone::Hand,
                count: 1,
            },
        ),
        (
            "set_aside",
            Operation::SetAside {
                target: hs_effect_ir::Selector::FriendlyHero,
            },
        ),
        (
            "return_to_zone",
            Operation::ReturnToZone {
                target: hs_effect_ir::Selector::FriendlyHero,
                to: hs_types::Zone::Hand,
            },
        ),
    ];
    for (what, op) in cases {
        let outcome = apply_program(
            &given,
            &program("U", vec![op]),
            &TargetBinding::default(),
            &common::EmptyPool,
            &SimRequest::default(),
        )
        .unwrap();
        match outcome.outcome {
            OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what: got }) => {
                assert_eq!(got, what);
            }
            other => panic!("{what}: expected NotImplemented, got {other:?}"),
        }
    }
}

#[test]
fn custom_handler_is_unsupported() {
    let given = default_state();
    let mut prog = program("CUSTOM", vec![]);
    prog.custom_handler = Some(HandlerId::new("cards.current_set.example"));
    let outcome = apply_program(
        &given,
        &prog,
        &TargetBinding::default(),
        &common::EmptyPool,
        &SimRequest::default(),
    )
    .unwrap();
    match outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::CustomHandler { handler }) => {
            assert_eq!(handler, "cards.current_set.example");
        }
        other => panic!("expected CustomHandler, got {other:?}"),
    }
}

#[test]
fn multiple_random_ops_are_unsupported() {
    let given = default_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("discover.test", &["AAA_01", "BBB_02", "CCC_03"]),
    };
    let outcome = apply_program(
        &given,
        &program(
            "TWO",
            vec![
                discover_op(),
                Operation::RandomChoice {
                    pool: PoolId::new("discover.test"),
                    count: 1,
                },
            ],
        ),
        &TargetBinding::default(),
        &pools,
        &SimRequest::default(),
    )
    .unwrap();
    match outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what }) => {
            assert_eq!(what, "multiple random ops");
        }
        other => panic!("expected multiple random ops, got {other:?}"),
    }
}

#[test]
fn insufficient_state_on_unrevealed_hand_filter() {
    let mut given = default_state();
    given.entities.insert(
        hs_types::EntityId(20),
        hs_sim::SimEntity {
            entity_id: hs_types::EntityId(20),
            card_id: None,
            card_type: None,
            zone: hs_types::Zone::Hand,
            controller: Some(PlayerId(2)),
            attack: 0,
            health: 0,
            damage: 0,
            cost: 0,
            action: Default::default(),
            simulated: false,
        },
    );
    let outcome = apply_program(
        &given,
        &program(
            "SPY",
            vec![Operation::Destroy {
                target: hs_effect_ir::Selector::Filter(
                    Box::new(hs_effect_ir::Selector::EntitiesInZone(hs_types::Zone::Hand)),
                    hs_effect_ir::EntityCondition {
                        card_id: Some(CardId::new("CS2_029")),
                        ..hs_effect_ir::EntityCondition::default()
                    },
                ),
            }],
        ),
        &TargetBinding::default(),
        &common::EmptyPool,
        &SimRequest::default(),
    )
    .unwrap();
    match outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { what }) => {
            assert!(what.contains("unrevealed"), "{what}");
        }
        other => panic!("expected InsufficientState, got {other:?}"),
    }
}

#[test]
fn empty_pool_is_insufficient_state() {
    let given = default_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("discover.test", &[]),
    };
    let outcome = apply_program(
        &given,
        &program("EMPTY", vec![discover_op()]),
        &TargetBinding::default(),
        &pools,
        &SimRequest::default(),
    )
    .unwrap();
    match outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { what }) => {
            assert_eq!(what, "empty pool");
        }
        other => panic!("expected empty pool, got {other:?}"),
    }
}

#[test]
fn nested_random_op_is_unsupported() {
    let given = default_state();
    let outcome = apply_program(
        &given,
        &program(
            "NEST",
            vec![Operation::Repeat {
                times: 1,
                effects: vec![discover_op()],
            }],
        ),
        &TargetBinding::default(),
        &common::EmptyPool,
        &SimRequest::default(),
    )
    .unwrap();
    match outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what }) => {
            assert_eq!(what, "nested random op");
        }
        other => panic!("expected nested random op, got {other:?}"),
    }
}
