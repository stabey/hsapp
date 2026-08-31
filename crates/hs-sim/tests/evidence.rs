//! M4 卡牌定义与规则证据边界。

mod common;

use common::{
    board, card_definition, deathrattle_card, hero, minion, player, pool_snapshot, program,
    EmptyPool, FixedPool, MapCards, MapRules,
};
use hs_effect_ir::{CardRef, EntityCondition, Operation, PlayerRef, Selector};
use hs_sim::{
    apply_program, apply_program_with_lookups, apply_program_with_rules, OutcomeSet, SampleMethod,
    SimContext, SimRequest, TargetBinding, UnsupportedReason,
};
use hs_types::{BuildId, CardId, CardType, EntityId, RuleStatus, Zone};

fn evidence_state() -> hs_sim::SimGameState {
    board(
        vec![
            hero(4, "HERO_01", 1, 30),
            hero(5, "HERO_02", 2, 30),
            minion(20, "DR_CARD", 1, 1, 2),
            minion(21, "NO_RULE", 1, 1, 2),
        ],
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    )
}

fn random_then_kill_program() -> hs_effect_ir::EffectProgram {
    program(
        "ROOT_RANDOM",
        vec![
            Operation::RandomChoice {
                pool: hs_types::PoolId::new("random.test"),
                count: 1,
            },
            Operation::Damage {
                target: Selector::FriendlyMinions,
                amount: 2,
            },
        ],
    )
}

fn death_rules() -> MapRules {
    MapRules::new(vec![deathrattle_card(
        "DR_CARD",
        vec![Operation::GainArmor {
            player: PlayerRef::SelfPlayer,
            amount: 1,
        }],
    )])
}

#[test]
fn unknown_root_is_not_a_successful_blank_but_verified_blank_is() {
    let state = evidence_state();
    let mut unknown = program("UNKNOWN_ROOT", Vec::new());
    unknown.status = RuleStatus::Unknown;

    let out = apply_program(
        &state,
        &unknown,
        &TargetBinding::default(),
        &EmptyPool,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { .. })
    ));
    assert_eq!(out.rule_uses.len(), 1);
    assert_eq!(out.rule_uses[0].status, RuleStatus::Unknown);

    let verified = program("VERIFIED_BLANK", Vec::new());
    let out = apply_program(
        &state,
        &verified,
        &TargetBinding::default(),
        &EmptyPool,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(out.outcome, OutcomeSet::Deterministic(_)));
    assert_eq!(out.rule_uses.len(), 1);
    assert_eq!(out.rule_uses[0].status, RuleStatus::Verified);
}

#[test]
fn strict_generated_cards_use_exact_definition_and_preserve_real_zeroes() {
    let state = evidence_state();
    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(
        BuildId(1),
        vec![
            card_definition("TOKEN_ZERO", CardType::Minion, 0, 0, 3, 0),
            card_definition("HAND_SPELL", CardType::Spell, 1, 0, 0, 0),
        ],
    );
    let context = SimContext {
        build: BuildId(1),
        pools: &EmptyPool,
        rules: &rules,
        cards: &cards,
    };
    let root = program(
        "CREATE_ROOT",
        vec![
            Operation::Summon {
                player: PlayerRef::SelfPlayer,
                card: CardRef::Id(CardId::new("TOKEN_ZERO")),
                count: 1,
            },
            Operation::CreateCard {
                player: PlayerRef::SelfPlayer,
                card: CardRef::Id(CardId::new("HAND_SPELL")),
                zone: Zone::Hand,
                count: 1,
            },
        ],
    );

    let out = apply_program_with_lookups(
        &state,
        &root,
        &TargetBinding::default(),
        &context,
        &SimRequest::default(),
    )
    .unwrap();
    let OutcomeSet::Deterministic(after) = out.outcome else {
        panic!("strict definitions should execute");
    };
    let token = after
        .entities
        .values()
        .find(|entity| entity.card_id.as_ref().map(CardId::as_str) == Some("TOKEN_ZERO"))
        .unwrap();
    assert_eq!(token.card_type, Some(CardType::Minion));
    assert_eq!((token.cost, token.attack, token.health), (0, 0, 3));
    assert!(token.action.cost_known);
    assert!(token.action.attack_known);
    assert!(token.action.health_known);
    assert!(token.action.durability_known);
    assert_eq!(token.action.durability, 0);

    let spell = after
        .entities
        .values()
        .find(|entity| entity.card_id.as_ref().map(CardId::as_str) == Some("HAND_SPELL"))
        .unwrap();
    assert_eq!(spell.card_type, Some(CardType::Spell));
    assert_eq!(spell.zone, Zone::Hand);
    assert!(spell.action.health_known, "真实 0 不是缺失");
}

#[test]
fn strict_missing_definition_rejects_the_whole_program() {
    let state = evidence_state();
    let original = state.clone();
    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(
        BuildId(1),
        vec![card_definition("KNOWN_TOKEN", CardType::Minion, 1, 1, 1, 0)],
    );
    let context = SimContext {
        build: BuildId(1),
        pools: &EmptyPool,
        rules: &rules,
        cards: &cards,
    };
    let root = program(
        "CREATE_ROOT",
        vec![Operation::CreateCard {
            player: PlayerRef::SelfPlayer,
            card: CardRef::Id(CardId::new("MISSING_TOKEN")),
            zone: Zone::Hand,
            count: 1,
        }],
    );

    let out = apply_program_with_lookups(
        &state,
        &root,
        &TargetBinding::default(),
        &context,
        &SimRequest::default(),
    )
    .unwrap();
    match out.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { what }) => {
            assert!(what.contains("MISSING_TOKEN"), "{what}");
            assert!(what.contains("build=1"), "{what}");
        }
        other => panic!("expected missing definition, got {other:?}"),
    }
    assert_eq!(state, original, "失败不能修改输入状态");
}

#[test]
fn strict_entry_requires_matching_state_rule_and_pool_builds() {
    let state = evidence_state();
    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(BuildId(1), Vec::new());
    let mut wrong_pool = pool_snapshot("random.test", &["A"]);
    wrong_pool.build_id = BuildId(2);
    let pools = FixedPool {
        snapshot: wrong_pool,
    };
    let context = SimContext {
        build: BuildId(1),
        pools: &pools,
        rules: &rules,
        cards: &cards,
    };

    let mut missing_build = state.clone();
    missing_build.build_id = None;
    let out = apply_program_with_lookups(
        &missing_build,
        &program("ROOT", Vec::new()),
        &TargetBinding::default(),
        &context,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { ref what })
            if what.contains("strict simulation build mismatch")
    ));

    let mut wrong_rule = program("ROOT", Vec::new());
    wrong_rule.build_range.min = BuildId(2);
    let out = apply_program_with_lookups(
        &state,
        &wrong_rule,
        &TargetBinding::default(),
        &context,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { ref what })
            if what.contains("root rule build mismatch")
    ));

    let mut unknown_play_type = state.clone();
    unknown_play_type
        .entities
        .get_mut(&EntityId(20))
        .unwrap()
        .card_type = None;
    let out = apply_program_with_lookups(
        &unknown_play_type,
        &program("ROOT", Vec::new()),
        &TargetBinding::default(),
        &context,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { ref what })
            if what.contains("known card_type")
    ));

    let out = apply_program_with_lookups(
        &state,
        &program(
            "ROOT",
            vec![Operation::RandomChoice {
                pool: hs_types::PoolId::new("random.test"),
                count: 1,
            }],
        ),
        &TargetBinding::default(),
        &context,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { ref what })
            if what.contains("pool snapshot build mismatch")
    ));
    assert_eq!(out.meta.pool_snapshot_hashes, ["hash-random.test"]);
}

#[test]
fn strict_sampled_pool_checks_every_possible_generated_card() {
    let state = evidence_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &["KNOWN", "MISSING"]),
    };
    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(
        BuildId(1),
        vec![card_definition("KNOWN", CardType::Spell, 0, 0, 0, 0)],
    );
    let context = SimContext {
        build: BuildId(1),
        pools: &pools,
        rules: &rules,
        cards: &cards,
    };
    let root = program(
        "RANDOM_ROOT",
        vec![Operation::RandomChoice {
            pool: hs_types::PoolId::new("random.test"),
            count: 1,
        }],
    );

    let out = apply_program_with_lookups(
        &state,
        &root,
        &TargetBinding::default(),
        &context,
        &SimRequest {
            seed: 7,
            branch_budget: 1,
        },
    )
    .unwrap();
    match out.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { what }) => {
            assert!(what.contains("MISSING"), "{what}");
        }
        other => panic!("sampled must not hide a missing candidate: {other:?}"),
    }
}

#[test]
fn exact_suffix_unions_root_deathrattle_unknown_and_pool_evidence() {
    let state = evidence_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &["A", "B"]),
    };
    let root = random_then_kill_program();
    let rules = death_rules();

    let out = apply_program_with_rules(
        &state,
        &root,
        &TargetBinding::default(),
        &pools,
        &rules,
        &SimRequest {
            seed: 1,
            branch_budget: 8,
        },
    )
    .unwrap();
    assert!(matches!(out.outcome, OutcomeSet::Exact(_)));
    assert_eq!(out.unknown_rule_cards, vec![CardId::new("NO_RULE")]);
    assert_eq!(out.meta.pool_snapshot_hashes, ["hash-random.test"]);
    let used: Vec<_> = out
        .rule_uses
        .iter()
        .map(|rule| (rule.card_id.as_str(), rule.rule_version.as_str()))
        .collect();
    assert_eq!(
        used,
        [
            ("DR_CARD", "DR_CARD@1.."),
            ("ROOT_RANDOM", "ROOT_RANDOM@1..")
        ]
    );
}

#[test]
fn sampled_suffix_unions_the_same_evidence_reproducibly() {
    let state = evidence_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &["A", "B", "C"]),
    };
    let root = random_then_kill_program();
    let rules = death_rules();
    let run = || {
        apply_program_with_rules(
            &state,
            &root,
            &TargetBinding::default(),
            &pools,
            &rules,
            &SimRequest {
                seed: 11,
                branch_budget: 1,
            },
        )
        .unwrap()
    };

    let first = run();
    assert!(matches!(first.outcome, OutcomeSet::Sampled(_)));
    assert_eq!(first.unknown_rule_cards, vec![CardId::new("NO_RULE")]);
    assert_eq!(first.meta.pool_snapshot_hashes, ["hash-random.test"]);
    assert_eq!(first.rule_uses.len(), 2);

    let first_json = serde_json::to_string(&(
        first.unknown_rule_cards,
        first.rule_uses,
        first.meta.pool_snapshot_hashes,
    ))
    .unwrap();
    for _ in 0..4 {
        let again = run();
        let json = serde_json::to_string(&(
            again.unknown_rule_cards,
            again.rule_uses,
            again.meta.pool_snapshot_hashes,
        ))
        .unwrap();
        assert_eq!(json, first_json);
    }
}

#[test]
fn strict_indirect_missing_or_unknown_rule_is_insufficient_but_legacy_records_gap() {
    let state = board(
        vec![
            hero(4, "HERO_01", 1, 30),
            hero(5, "HERO_02", 2, 30),
            minion(20, "NO_RULE", 1, 1, 2),
        ],
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    );
    let root = program(
        "ROOT_DAMAGE",
        vec![Operation::Damage {
            target: Selector::Target,
            amount: 2,
        }],
    );
    let binding = TargetBinding {
        source: None,
        target: Some(EntityId(20)),
    };
    let empty_rules = MapRules::new(Vec::new());

    let legacy = apply_program_with_rules(
        &state,
        &root,
        &binding,
        &EmptyPool,
        &empty_rules,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(legacy.outcome, OutcomeSet::Deterministic(_)));
    assert_eq!(legacy.unknown_rule_cards, [CardId::new("NO_RULE")]);

    let cards = MapCards::new(BuildId(1), Vec::new());
    let context = SimContext {
        build: BuildId(1),
        pools: &EmptyPool,
        rules: &empty_rules,
        cards: &cards,
    };
    let strict =
        apply_program_with_lookups(&state, &root, &binding, &context, &SimRequest::default())
            .unwrap();
    assert!(matches!(
        strict.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { .. })
    ));
    assert_eq!(strict.unknown_rule_cards, [CardId::new("NO_RULE")]);

    let mut unknown_rule = program("NO_RULE", Vec::new());
    unknown_rule.status = RuleStatus::Unknown;
    let unknown_rules = MapRules::new(vec![unknown_rule]);
    let context = SimContext {
        build: BuildId(1),
        pools: &EmptyPool,
        rules: &unknown_rules,
        cards: &cards,
    };
    let strict =
        apply_program_with_lookups(&state, &root, &binding, &context, &SimRequest::default())
            .unwrap();
    assert!(matches!(
        strict.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { .. })
    ));
    assert_eq!(strict.unknown_rule_cards, [CardId::new("NO_RULE")]);
}

#[test]
fn unsupported_random_suffix_invalidates_the_whole_outcome() {
    let state = evidence_state();
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &["A", "B"]),
    };
    let root = program(
        "UNSUPPORTED_SUFFIX",
        vec![
            Operation::RandomChoice {
                pool: hs_types::PoolId::new("random.test"),
                count: 1,
            },
            Operation::Transform {
                target: Selector::Target,
                into: CardRef::Id(CardId::new("X")),
            },
        ],
    );
    let out = apply_program(
        &state,
        &root,
        &TargetBinding {
            source: None,
            target: Some(EntityId(20)),
        },
        &pools,
        &SimRequest::default(),
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { .. })
    ));
    assert_eq!(out.meta.pool_snapshot_hashes, ["hash-random.test"]);
}

#[test]
fn exact_failure_still_unions_evidence_from_later_branches() {
    let state = board(
        vec![hero(4, "HERO_01", 1, 30), hero(5, "HERO_02", 2, 30)],
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    );
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &["A", "B"]),
    };
    let rules = MapRules::new(vec![deathrattle_card(
        "B",
        vec![Operation::GainArmor {
            player: PlayerRef::SelfPlayer,
            amount: 0,
        }],
    )]);
    let cards = MapCards::new(
        BuildId(1),
        vec![
            card_definition("A", CardType::Minion, 0, 1, 1, 0),
            card_definition("B", CardType::Minion, 0, 1, 1, 0),
        ],
    );
    let context = SimContext {
        build: BuildId(1),
        pools: &pools,
        rules: &rules,
        cards: &cards,
    };
    let only_b = EntityCondition {
        card_id: Some(CardId::new("B")),
        ..EntityCondition::default()
    };
    let root = program(
        "EXACT_ROOT",
        vec![
            Operation::RandomChoice {
                pool: hs_types::PoolId::new("random.test"),
                count: 1,
            },
            Operation::MoveZone {
                target: Selector::Filter(
                    Box::new(Selector::EntitiesInZone(Zone::Hand)),
                    only_b.clone(),
                ),
                to: Zone::Play,
            },
            Operation::Damage {
                target: Selector::Filter(Box::new(Selector::FriendlyMinions), only_b),
                amount: 1,
            },
            Operation::Transform {
                target: Selector::FriendlyHero,
                into: CardRef::Id(CardId::new("X")),
            },
        ],
    );

    let out = apply_program_with_lookups(
        &state,
        &root,
        &TargetBinding::default(),
        &context,
        &SimRequest {
            seed: 0,
            branch_budget: 8,
        },
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { .. })
    ));
    let used = out
        .rule_uses
        .iter()
        .map(|rule| rule.card_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(used, ["B", "EXACT_ROOT"]);
}

#[test]
fn sampled_failure_unions_evidence_from_every_sample() {
    let state = board(
        vec![hero(4, "HERO_01", 1, 30), hero(5, "HERO_02", 2, 30)],
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    );
    let pools = FixedPool {
        snapshot: pool_snapshot("random.test", &["A", "B", "C"]),
    };
    let rules = MapRules::new(
        ["A", "B", "C"]
            .into_iter()
            .map(|card| {
                deathrattle_card(
                    card,
                    vec![Operation::GainArmor {
                        player: PlayerRef::SelfPlayer,
                        amount: 0,
                    }],
                )
            })
            .collect(),
    );
    let cards = MapCards::new(
        BuildId(1),
        ["A", "B", "C"]
            .into_iter()
            .map(|card| card_definition(card, CardType::Minion, 0, 1, 1, 0))
            .collect(),
    );
    let context = SimContext {
        build: BuildId(1),
        pools: &pools,
        rules: &rules,
        cards: &cards,
    };
    let root = program(
        "SAMPLED_ROOT",
        vec![
            Operation::RandomChoice {
                pool: hs_types::PoolId::new("random.test"),
                count: 1,
            },
            Operation::MoveZone {
                target: Selector::EntitiesInZone(Zone::Hand),
                to: Zone::Play,
            },
            Operation::Damage {
                target: Selector::FriendlyMinions,
                amount: 1,
            },
            Operation::Transform {
                target: Selector::FriendlyHero,
                into: CardRef::Id(CardId::new("X")),
            },
        ],
    );

    // seed=0 的两个等权样本依次选择 A、B；即使 A 分支先失败，也必须继续
    // 执行 B 分支并把二者的亡语 RuleUse 合并进最终 Unsupported 证据。
    let out = apply_program_with_lookups(
        &state,
        &root,
        &TargetBinding::default(),
        &context,
        &SimRequest {
            seed: 0,
            branch_budget: 2,
        },
    )
    .unwrap();
    assert!(matches!(
        out.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { .. })
    ));
    assert_eq!(out.meta.method, SampleMethod::MonteCarlo);
    assert!(out.meta.truncation.is_some());
    let used = out
        .rule_uses
        .iter()
        .map(|rule| rule.card_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(used, ["A", "B", "SAMPLED_ROOT"]);
}
