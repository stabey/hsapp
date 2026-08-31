//! M4 玩家攻击与 Effect IR Attack 的统一战斗语义回归。

mod common;

use common::{
    board, deathrattle_card, expect_det, hero, minion, player, program, EmptyPool, MapCards,
    MapRules,
};
use hs_effect_ir::{Operation, Selector, TriggerEvent, TriggerProgram};
use hs_sim::{
    apply_action, apply_program_with_lookups, OutcomeSet, SimAction, SimContext, SimGameState,
    SimOutcome, SimRequest, TargetBinding, UnsupportedReason,
};
use hs_types::{BuildId, CardId, CardType, EntityId, PlayerId, RuleStatus, Zone};

const FRIENDLY_HERO: EntityId = EntityId(4);
const ENEMY_HERO: EntityId = EntityId(5);
const ATTACKER: EntityId = EntityId(10);
const DEFENDER: EntityId = EntityId(20);

fn combat_state(
    attacker_attack: i32,
    attacker_health: i32,
    defender_attack: i32,
    defender_health: i32,
) -> SimGameState {
    board(
        vec![
            hero(FRIENDLY_HERO.0, "HERO_01", 1, 30),
            hero(ENEMY_HERO.0, "HERO_02", 2, 30),
            minion(ATTACKER.0, "ATTACKER", 1, attacker_attack, attacker_health),
            minion(DEFENDER.0, "DEFENDER", 2, defender_attack, defender_health),
        ],
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    )
}

fn apply_attack(
    state: &SimGameState,
    attacker: EntityId,
    defender: EntityId,
    rules: &MapRules,
) -> SimOutcome {
    let cards = MapCards::new(BuildId(1), Vec::new());
    let context = SimContext {
        build: BuildId(1),
        pools: &EmptyPool,
        rules,
        cards: &cards,
    };
    apply_action(
        state,
        &SimAction::Attack { attacker, defender },
        &context,
        &SimRequest::default(),
    )
    .expect("apply_action attack")
}

fn assert_action_unavailable(given: &SimGameState, defender: EntityId, expected_detail: &str) {
    let before = given.clone();
    let outcome = apply_attack(given, ATTACKER, defender, &MapRules::new(Vec::new()));

    match outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::ActionUnavailable { what }) => assert!(
            what.contains(expected_detail),
            "拒绝原因 `{what}` 应包含 `{expected_detail}`"
        ),
        other => panic!("期望 ActionUnavailable, 得到 {other:?}"),
    }
    assert_eq!(given, &before, "失败动作不得改动输入局面");
}

#[test]
fn minions_deal_simultaneous_damage_and_can_die_together() {
    let given = combat_state(3, 3, 3, 3);
    let rules = MapRules::new(vec![
        program("ATTACKER", Vec::new()),
        program("DEFENDER", Vec::new()),
    ]);

    let outcome = apply_attack(&given, ATTACKER, DEFENDER, &rules);
    let then = expect_det(&outcome);

    assert_eq!(then.entity(ATTACKER).unwrap().damage, 3);
    assert_eq!(then.entity(DEFENDER).unwrap().damage, 3);
    assert_eq!(then.entity(ATTACKER).unwrap().zone, Zone::Graveyard);
    assert_eq!(then.entity(DEFENDER).unwrap().zone, Zone::Graveyard);
    assert_eq!(then.entity(ATTACKER).unwrap().action.attacks_this_turn, 1);
    assert!(then.entity(ATTACKER).unwrap().action.exhausted);

    let ops = outcome
        .trace
        .entries
        .iter()
        .map(|entry| entry.op.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ops,
        ["action", "attack", "damage", "damage", "death", "death"]
    );
}

#[test]
fn attacking_a_hero_uses_armor_and_never_retaliates() {
    let mut given = combat_state(4, 5, 2, 6);
    given.entities.get_mut(&ENEMY_HERO).unwrap().attack = 7;
    given.players.get_mut(&PlayerId(2)).unwrap().armor = 2;

    let outcome = apply_attack(&given, ATTACKER, ENEMY_HERO, &MapRules::new(Vec::new()));
    let then = expect_det(&outcome);

    let attacker = then.entity(ATTACKER).unwrap();
    assert_eq!(attacker.damage, 0, "防守英雄即使有攻击力也不反伤");
    assert_eq!(attacker.current_health(), 5);
    assert_eq!(then.entity(ENEMY_HERO).unwrap().damage, 2);
    assert_eq!(then.player(PlayerId(2)).unwrap().armor, 0);
    assert_eq!(then.player(PlayerId(2)).unwrap().hero_health, 28);
    assert!(outcome
        .trace
        .entries
        .iter()
        .any(|entry| entry.op == "attack" && entry.detail.contains("retaliation=0")));
}

#[test]
fn basic_attack_legality_rejections_are_atomic() {
    struct Case {
        name: &'static str,
        defender: EntityId,
        expected_detail: &'static str,
        mutate: fn(&mut SimGameState),
    }

    let cases = [
        Case {
            name: "taunt",
            defender: ENEMY_HERO,
            expected_detail: "嘲讽",
            mutate: |state| {
                let mut guard = minion(21, "TAUNT", 2, 1, 4);
                guard.action.taunt = true;
                state.entities.insert(guard.entity_id, guard);
            },
        },
        Case {
            name: "stealth",
            defender: DEFENDER,
            expected_detail: "不可被攻击",
            mutate: |state| state.entities.get_mut(&DEFENDER).unwrap().action.stealth = true,
        },
        Case {
            name: "attacks_this_turn",
            defender: DEFENDER,
            expected_detail: "疲劳",
            mutate: |state| {
                state
                    .entities
                    .get_mut(&ATTACKER)
                    .unwrap()
                    .action
                    .attacks_this_turn = 1;
            },
        },
        Case {
            name: "exhausted",
            defender: DEFENDER,
            expected_detail: "疲劳",
            mutate: |state| state.entities.get_mut(&ATTACKER).unwrap().action.exhausted = true,
        },
        Case {
            name: "frozen",
            defender: DEFENDER,
            expected_detail: "不能攻击",
            mutate: |state| state.entities.get_mut(&ATTACKER).unwrap().action.frozen = true,
        },
        Case {
            name: "cant_attack",
            defender: DEFENDER,
            expected_detail: "不能攻击",
            mutate: |state| {
                state
                    .entities
                    .get_mut(&ATTACKER)
                    .unwrap()
                    .action
                    .cant_attack = true
            },
        },
        Case {
            name: "cannot_attack_heroes",
            defender: ENEMY_HERO,
            expected_detail: "不能攻击英雄",
            mutate: |state| {
                state
                    .entities
                    .get_mut(&ATTACKER)
                    .unwrap()
                    .action
                    .cannot_attack_heroes = true;
            },
        },
    ];

    for case in cases {
        let mut given = combat_state(3, 5, 2, 6);
        (case.mutate)(&mut given);
        assert_action_unavailable(&given, case.defender, case.expected_detail);
        assert_eq!(
            given.entity(ATTACKER).unwrap().action.attacks_this_turn,
            i32::from(case.name == "attacks_this_turn"),
            "{} 拒绝后输入攻击次数不应被额外消费",
            case.name
        );
    }
}

#[test]
fn complex_combat_keywords_are_explicitly_unsupported_and_atomic() {
    struct Case {
        keyword: &'static str,
        mutate: fn(&mut SimGameState),
    }

    let cases = [
        Case {
            keyword: "windfury",
            mutate: |state| state.entities.get_mut(&ATTACKER).unwrap().action.windfury = true,
        },
        Case {
            keyword: "mega_windfury",
            mutate: |state| {
                state
                    .entities
                    .get_mut(&ATTACKER)
                    .unwrap()
                    .action
                    .mega_windfury = true;
            },
        },
        Case {
            keyword: "divine_shield",
            mutate: |state| {
                state
                    .entities
                    .get_mut(&DEFENDER)
                    .unwrap()
                    .action
                    .divine_shield = true;
            },
        },
        Case {
            keyword: "poisonous",
            mutate: |state| state.entities.get_mut(&ATTACKER).unwrap().action.poisonous = true,
        },
        Case {
            keyword: "lifesteal",
            mutate: |state| state.entities.get_mut(&ATTACKER).unwrap().action.lifesteal = true,
        },
        Case {
            keyword: "immune",
            mutate: |state| state.entities.get_mut(&ATTACKER).unwrap().action.immune = true,
        },
        Case {
            keyword: "immune_while_attacking",
            mutate: |state| {
                state
                    .entities
                    .get_mut(&ATTACKER)
                    .unwrap()
                    .action
                    .immune_while_attacking = true;
            },
        },
    ];

    for case in cases {
        let mut given = combat_state(3, 5, 2, 6);
        (case.mutate)(&mut given);
        let before = given.clone();
        let outcome = apply_attack(&given, ATTACKER, DEFENDER, &MapRules::new(Vec::new()));

        match outcome.outcome {
            OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what }) => {
                assert_eq!(what, format!("combat keyword {}", case.keyword));
            }
            other => panic!("{} 应显式 NotImplemented, 得到 {other:?}", case.keyword),
        }
        assert_eq!(given, before, "{} 拒绝后不得改动输入局面", case.keyword);
    }
}

#[test]
fn known_aura_blocks_player_attack_instead_of_being_ignored() {
    let mut given = combat_state(3, 5, 2, 6);
    given
        .entities
        .insert(EntityId(21), minion(21, "AURA_CARD", 2, 1, 2));
    let mut aura = program("AURA_CARD", Vec::new());
    aura.status = RuleStatus::Inferred;
    aura.confidence = 0.73;
    aura.build_range.max = Some(BuildId(8));
    aura.triggers.push(TriggerProgram {
        event: TriggerEvent::AuraRecalculate,
        when: Vec::new(),
        effects: Vec::new(),
    });
    let rules = MapRules::new(vec![aura]);
    let before = given.clone();

    let outcome = apply_attack(&given, ATTACKER, DEFENDER, &rules);
    assert!(matches!(
        outcome.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { ref what })
            if what == "aura"
    ));
    assert!(outcome.trace.entries.iter().any(|entry| entry.op == "aura"));
    assert_eq!(outcome.rule_uses.len(), 1);
    assert_eq!(outcome.rule_uses[0].card_id, CardId::new("AURA_CARD"));
    assert_eq!(outcome.rule_uses[0].status, RuleStatus::Inferred);
    assert_eq!(outcome.rule_uses[0].confidence, 0.73);
    assert_eq!(outcome.rule_uses[0].rule_version, "AURA_CARD@1..8");
    assert!(outcome.unknown_rule_cards.is_empty());
    assert_eq!(given, before);
}

#[test]
fn every_current_build_aura_is_registered_in_stable_order() {
    let mut given = combat_state(3, 5, 2, 6);
    given
        .entities
        .insert(EntityId(21), minion(21, "AURA_Z", 2, 1, 2));
    given
        .entities
        .insert(EntityId(22), minion(22, "AURA_A", 2, 1, 2));
    let mut aura_z = program("AURA_Z", Vec::new());
    aura_z.triggers.push(TriggerProgram {
        event: TriggerEvent::AuraEnter,
        when: Vec::new(),
        effects: Vec::new(),
    });
    let mut aura_a = program("AURA_A", Vec::new());
    aura_a.status = RuleStatus::Partial;
    aura_a.confidence = 0.6;
    aura_a.triggers.push(TriggerProgram {
        event: TriggerEvent::AuraRecalculate,
        when: Vec::new(),
        effects: Vec::new(),
    });
    let rules = MapRules::new(vec![aura_z, aura_a]);
    let before = given.clone();

    let outcome = apply_attack(&given, ATTACKER, DEFENDER, &rules);

    assert!(matches!(
        outcome.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { ref what })
            if what == "aura"
    ));
    assert_eq!(
        outcome
            .rule_uses
            .iter()
            .map(|rule_use| rule_use.card_id.as_str())
            .collect::<Vec<_>>(),
        vec!["AURA_A", "AURA_Z"]
    );
    assert_eq!(outcome.rule_uses[0].status, RuleStatus::Partial);
    assert_eq!(outcome.rule_uses[0].confidence, 0.6);
    let aura_trace = outcome
        .trace
        .entries
        .iter()
        .find(|entry| entry.op == "aura")
        .expect("aura trace");
    assert!(aura_trace.detail.contains("AURA_A,AURA_Z"));
    assert_eq!(given, before);
}

#[test]
fn wrong_build_aura_is_an_unknown_gap_not_a_current_rule_use() {
    let mut given = combat_state(3, 5, 2, 6);
    given
        .entities
        .insert(EntityId(21), minion(21, "FUTURE_AURA", 2, 1, 2));
    let mut aura = program("FUTURE_AURA", Vec::new());
    aura.build_range.min = BuildId(2);
    aura.triggers.push(TriggerProgram {
        event: TriggerEvent::AuraRecalculate,
        when: Vec::new(),
        effects: Vec::new(),
    });
    let rules = MapRules::new(vec![aura]);
    let before = given.clone();

    let outcome = apply_attack(&given, ATTACKER, DEFENDER, &rules);

    match &outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { what }) => {
            assert!(what.contains("aura rule build mismatch"), "{what}");
            assert!(what.contains("FUTURE_AURA"), "{what}");
        }
        other => panic!("wrong-build aura 应保守退出,得到 {other:?}"),
    }
    assert!(outcome.rule_uses.is_empty());
    assert_eq!(outcome.unknown_rule_cards, vec![CardId::new("FUTURE_AURA")]);
    assert!(outcome.trace.entries.iter().any(|entry| entry.op == "aura"));
    assert_eq!(given, before);
}

#[test]
fn hero_weapon_attack_decrements_and_breaks_durability() {
    fn state_with_weapon(durability: i32) -> SimGameState {
        let mut state = combat_state(3, 30, 2, 6);
        state.entities.remove(&ATTACKER);
        state.entities.get_mut(&FRIENDLY_HERO).unwrap().attack = 3;
        let mut weapon = minion(30, "WEAPON", 1, 3, 1);
        weapon.card_type = Some(CardType::Weapon);
        weapon.action.durability = durability;
        weapon.action.durability_known = true;
        state.entities.insert(weapon.entity_id, weapon);
        state
    }

    let rules = MapRules::new(vec![program("WEAPON", Vec::new())]);

    let two_durability = state_with_weapon(2);
    let outcome = apply_attack(&two_durability, FRIENDLY_HERO, ENEMY_HERO, &rules);
    let then = expect_det(&outcome);
    let weapon = then.entity(EntityId(30)).unwrap();
    assert_eq!(weapon.action.durability, 1);
    assert_eq!(weapon.zone, Zone::Play);
    assert_eq!(then.player(PlayerId(2)).unwrap().hero_health, 27);
    assert_eq!(
        then.entity(FRIENDLY_HERO).unwrap().action.attacks_this_turn,
        1
    );

    let last_durability = state_with_weapon(1);
    let outcome = apply_attack(&last_durability, FRIENDLY_HERO, ENEMY_HERO, &rules);
    let then = expect_det(&outcome);
    let weapon = then.entity(EntityId(30)).unwrap();
    assert_eq!(weapon.action.durability, 0);
    assert_eq!(weapon.zone, Zone::Graveyard);
    assert_eq!(
        then.entity(FRIENDLY_HERO).unwrap().attack,
        0,
        "武器离场后英雄攻击力必须同步移除"
    );
    assert!(outcome
        .trace
        .entries
        .iter()
        .any(|entry| entry.op == "death" && entry.detail.contains("entity=30")));
}

#[test]
fn effect_ir_attack_and_player_action_share_combat_damage() {
    let mut given = combat_state(4, 5, 2, 6);
    given.entities.get_mut(&ENEMY_HERO).unwrap().attack = 7;
    given.players.get_mut(&PlayerId(2)).unwrap().armor = 2;

    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(BuildId(1), Vec::new());
    let context = SimContext {
        build: BuildId(1),
        pools: &EmptyPool,
        rules: &rules,
        cards: &cards,
    };
    let action_outcome = apply_action(
        &given,
        &SimAction::Attack {
            attacker: ATTACKER,
            defender: ENEMY_HERO,
        },
        &context,
        &SimRequest::default(),
    )
    .unwrap();
    let ir_outcome = apply_program_with_lookups(
        &given,
        &program(
            "IR_ATTACK",
            vec![Operation::Attack {
                attacker: Selector::Source,
                defender: Selector::Target,
            }],
        ),
        &TargetBinding {
            source: Some(ATTACKER),
            target: Some(ENEMY_HERO),
        },
        &context,
        &SimRequest::default(),
    )
    .unwrap();

    let action_state = expect_det(&action_outcome);
    let ir_state = expect_det(&ir_outcome);
    for id in [ATTACKER, ENEMY_HERO] {
        assert_eq!(
            action_state.entity(id).unwrap().damage,
            ir_state.entity(id).unwrap().damage
        );
        assert_eq!(
            action_state.entity(id).unwrap().zone,
            ir_state.entity(id).unwrap().zone
        );
    }
    assert_eq!(
        action_state.player(PlayerId(2)).unwrap().armor,
        ir_state.player(PlayerId(2)).unwrap().armor
    );
    assert_eq!(
        action_state.player(PlayerId(2)).unwrap().hero_health,
        ir_state.player(PlayerId(2)).unwrap().hero_health
    );

    let combat_trace = |outcome: &SimOutcome| {
        outcome
            .trace
            .entries
            .iter()
            .filter(|entry| entry.op == "attack" || entry.op == "damage")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(combat_trace(&action_outcome), combat_trace(&ir_outcome));
    assert_eq!(
        action_state
            .entity(ATTACKER)
            .unwrap()
            .action
            .attacks_this_turn,
        1,
        "玩家动作层负责消费攻击次数"
    );
    assert_eq!(
        ir_state.entity(ATTACKER).unwrap().action.attacks_this_turn,
        0,
        "规则 IR 只复用同时伤害与死亡底层"
    );
}

#[test]
fn combat_deathrattle_chain_records_the_rule_use() {
    let mut given = combat_state(3, 3, 3, 3);
    given.entities.get_mut(&DEFENDER).unwrap().card_id = Some(CardId::new("DR_FIRST"));
    given
        .entities
        .insert(EntityId(21), minion(21, "PLAIN_SECOND", 2, 1, 2));
    let rules = MapRules::new(vec![
        program("ATTACKER", Vec::new()),
        deathrattle_card(
            "DR_FIRST",
            vec![Operation::Damage {
                target: Selector::FriendlyMinions,
                amount: 2,
            }],
        ),
        program("PLAIN_SECOND", Vec::new()),
    ]);

    let outcome = apply_attack(&given, ATTACKER, DEFENDER, &rules);
    let then = expect_det(&outcome);

    assert_eq!(then.entity(ATTACKER).unwrap().zone, Zone::Graveyard);
    assert_eq!(then.entity(DEFENDER).unwrap().zone, Zone::Graveyard);
    assert_eq!(then.entity(EntityId(21)).unwrap().zone, Zone::Graveyard);
    assert!(outcome.unknown_rule_cards.is_empty());
    assert_eq!(outcome.rule_uses.len(), 1);
    assert_eq!(outcome.rule_uses[0].card_id, CardId::new("DR_FIRST"));
    assert_eq!(outcome.rule_uses[0].status, RuleStatus::Verified);

    let deaths = outcome
        .trace
        .entries
        .iter()
        .filter(|entry| entry.op == "death")
        .map(|entry| entry.detail.as_str())
        .collect::<Vec<_>>();
    assert_eq!(deaths.len(), 3);
    assert!(deaths[0].contains("depth=0"));
    assert!(deaths[1].contains("depth=0"));
    assert!(deaths[2].contains("depth=1"));
}
