//! 每个已支持 Operation 的 Given / When / Then + RuleTrace。

mod common;

use common::{
    apply_bound, apply_ops, board, default_state, expect_det, hero, minion, player, trace_ops,
};
use hs_effect_ir::{
    CardRef, EntityCondition, EntityRole, Operation, PlayerRef, Predicate, Selector,
};
use hs_sim::{OutcomeSet, TargetBinding, UnsupportedReason};
use hs_types::{CardId, CardType, EntityId, PlayerId, Zone};

#[test]
fn draw_decrements_deck_and_increments_hand() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::Draw {
            player: PlayerRef::SelfPlayer,
            count: 2,
        }],
    );
    let then = expect_det(&outcome);
    let p1 = then.player(PlayerId(1)).unwrap();
    assert_eq!(p1.deck_count, 3);
    assert_eq!(p1.hand_count, 2);
    assert_eq!(trace_ops(&outcome), ["draw", "draw"]);
}

#[test]
fn draw_fatigue_damages_hero_when_deck_empty() {
    let mut given = default_state();
    given.players.get_mut(&PlayerId(1)).unwrap().deck_count = 0;
    let outcome = apply_ops(
        &given,
        vec![Operation::Draw {
            player: PlayerRef::SelfPlayer,
            count: 1,
        }],
    );
    let then = expect_det(&outcome);
    let p1 = then.player(PlayerId(1)).unwrap();
    assert_eq!(p1.fatigue, 1);
    assert_eq!(p1.deck_count, 0);
    assert_eq!(p1.hero_health, 29);
    let hero = then.entity(EntityId(4)).unwrap();
    assert_eq!(hero.damage, 1);
    assert!(outcome.trace.entries[0].detail.contains("fatigue=1"));
}

#[test]
fn draw_fatigue_to_immune_hero_is_explicitly_unsupported_and_atomic() {
    let mut given = default_state();
    given.players.get_mut(&PlayerId(1)).unwrap().deck_count = 0;
    given.entities.get_mut(&EntityId(4)).unwrap().action.immune = true;
    let before = given.clone();

    let outcome = apply_ops(
        &given,
        vec![Operation::Draw {
            player: PlayerRef::SelfPlayer,
            count: 2,
        }],
    );

    assert!(matches!(
        &outcome.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what })
            if what == "fatigue target immune"
    ));
    assert!(
        outcome.trace.entries.is_empty(),
        "疲劳保护必须在增加 fatigue 或记录 draw trace 前预检"
    );
    assert_eq!(given, before, "immune 疲劳失败不能修改输入局面");
}

#[test]
fn draw_fatigue_does_not_treat_divine_shield_as_hero_protection() {
    let mut given = default_state();
    given.players.get_mut(&PlayerId(1)).unwrap().deck_count = 0;
    given
        .entities
        .get_mut(&EntityId(4))
        .unwrap()
        .action
        .divine_shield = true;

    let outcome = apply_ops(
        &given,
        vec![Operation::Draw {
            player: PlayerRef::SelfPlayer,
            count: 1,
        }],
    );
    let then = expect_det(&outcome);

    assert_eq!(then.player(PlayerId(1)).unwrap().hero_health, 29);
    assert_eq!(then.entity(EntityId(4)).unwrap().damage, 1);
    assert!(then.entity(EntityId(4)).unwrap().action.divine_shield);
}

#[test]
fn damage_applies_to_minion_without_changing_zone() {
    let given = default_state();
    let outcome = apply_bound(
        &given,
        vec![Operation::Damage {
            target: Selector::Target,
            amount: 3,
        }],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    let then = expect_det(&outcome);
    let minion = then.entity(EntityId(10)).unwrap();
    assert_eq!(minion.damage, 3);
    assert_eq!(minion.zone, Zone::Play);
    assert_eq!(minion.health, 5);
    assert_eq!(trace_ops(&outcome), ["damage"]);
}

#[test]
fn damage_lethal_moves_to_graveyard_without_cascade() {
    let mut given = default_state();
    given
        .entities
        .insert(EntityId(11), minion(11, "EX1_012", 1, 1, 1));
    let friend_count = given.entities.len();
    let outcome = apply_bound(
        &given,
        vec![Operation::Damage {
            target: Selector::Target,
            amount: 5,
        }],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    let then = expect_det(&outcome);
    assert_eq!(then.entity(EntityId(10)).unwrap().zone, Zone::Graveyard);
    assert_eq!(then.entity(EntityId(11)).unwrap().zone, Zone::Play);
    assert_eq!(then.entities.len(), friend_count);
    assert!(outcome.trace.entries[0].detail.contains("lethal=true"));
}

#[test]
fn damage_hits_armor_before_hero_health() {
    let mut given = default_state();
    given.players.get_mut(&PlayerId(1)).unwrap().armor = 5;
    let outcome = apply_ops(
        &given,
        vec![
            Operation::Damage {
                target: Selector::FriendlyHero,
                amount: 3,
            },
            Operation::Damage {
                target: Selector::FriendlyHero,
                amount: 10,
            },
        ],
    );
    let then = expect_det(&outcome);
    let p1 = then.player(PlayerId(1)).unwrap();
    assert_eq!(p1.armor, 0);
    assert_eq!(p1.hero_health, 22);
    let hero = then.entity(EntityId(4)).unwrap();
    assert_eq!(hero.damage, 8);
    assert_eq!(hero.zone, Zone::Play);
    assert!(outcome.trace.entries[0].detail.contains("armor_absorbed=3"));
    assert!(outcome.trace.entries[1].detail.contains("armor_absorbed=2"));
}

#[test]
fn damage_to_immune_hero_is_explicitly_unsupported_and_atomic() {
    let mut given = default_state();
    given.entities.get_mut(&EntityId(4)).unwrap().action.immune = true;
    let before = given.clone();

    let outcome = apply_ops(
        &given,
        vec![Operation::Damage {
            target: Selector::FriendlyHero,
            amount: 6,
        }],
    );

    assert!(matches!(
        &outcome.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what })
            if what == "damage target immune"
    ));
    assert!(outcome.trace.entries.is_empty());
    assert_eq!(given, before, "immune 失败不能修改输入局面");
}

#[test]
fn multi_target_damage_prechecks_divine_shield_before_any_damage() {
    let mut given = default_state();
    let mut shielded = minion(11, "SHIELDED", 1, 2, 3);
    shielded.action.divine_shield = true;
    given.entities.insert(EntityId(11), shielded);
    let before = given.clone();

    let outcome = apply_ops(
        &given,
        vec![Operation::Damage {
            target: Selector::FriendlyMinions,
            amount: 2,
        }],
    );

    assert!(matches!(
        &outcome.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what })
            if what == "damage target divine_shield"
    ));
    assert!(
        outcome.trace.entries.is_empty(),
        "较早的普通目标不得先留下 damage trace"
    );
    assert_eq!(given, before, "多目标预检失败不能修改输入局面");
}

#[test]
fn heal_reduces_damage_and_does_not_overheal() {
    let mut given = default_state();
    given.entities.get_mut(&EntityId(10)).unwrap().damage = 4;
    let outcome = apply_bound(
        &given,
        vec![
            Operation::Heal {
                target: Selector::Target,
                amount: 3,
            },
            Operation::Heal {
                target: Selector::Target,
                amount: 10,
            },
        ],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    let then = expect_det(&outcome);
    assert_eq!(then.entity(EntityId(10)).unwrap().damage, 0);
    assert_eq!(trace_ops(&outcome), ["heal", "heal"]);
}

#[test]
fn gain_armor_adds_to_player() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::GainArmor {
            player: PlayerRef::SelfPlayer,
            amount: 2,
        }],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.player(PlayerId(1)).unwrap().armor, 2);
    assert_eq!(trace_ops(&outcome), ["gain_armor"]);
}

#[test]
fn summon_creates_simulated_entity_with_unknown_stats() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::Summon {
            player: PlayerRef::SelfPlayer,
            card: CardRef::Id(CardId::new("CS2_231")),
            count: 1,
        }],
    );
    let then = expect_det(&outcome);
    let summoned = then
        .entities
        .values()
        .find(|e| e.simulated)
        .expect("summoned entity");
    assert_eq!(summoned.zone, Zone::Play);
    assert_eq!(summoned.attack, 0);
    assert_eq!(summoned.health, 0);
    assert_eq!(
        summoned.card_id.as_ref().map(CardId::as_str),
        Some("CS2_231")
    );
    assert_eq!(summoned.entity_id, EntityId(-1));
    assert!(outcome.trace.entries[0].detail.contains("stats unknown"));
}

#[test]
fn destroy_moves_to_graveyard() {
    let given = default_state();
    let outcome = apply_bound(
        &given,
        vec![Operation::Destroy {
            target: Selector::Target,
        }],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    let then = expect_det(&outcome);
    assert_eq!(then.entity(EntityId(10)).unwrap().zone, Zone::Graveyard);
    // Destroy 现在走死亡结算(才能触发亡语):destroy 标记待死 → death 移区 →
    // 查亡语。apply_program 用的是空规则表,故记为 unknown 覆盖缺口而非
    // 静默当作「没有亡语」。
    assert_eq!(trace_ops(&outcome), ["destroy", "death", "deathrattle"]);
    assert!(
        !outcome.unknown_rule_cards.is_empty(),
        "空规则表下死亡应记覆盖缺口"
    );
}

#[test]
fn move_zone_updates_hand_count() {
    let given = default_state();
    let outcome = apply_bound(
        &given,
        vec![Operation::MoveZone {
            target: Selector::Target,
            to: Zone::Hand,
        }],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    let then = expect_det(&outcome);
    assert_eq!(then.entity(EntityId(10)).unwrap().zone, Zone::Hand);
    assert_eq!(then.player(PlayerId(1)).unwrap().hand_count, 1);
    assert_eq!(trace_ops(&outcome), ["move_zone"]);
}

#[test]
fn create_card_puts_simulated_entity_in_zone() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::CreateCard {
            player: PlayerRef::SelfPlayer,
            card: CardRef::Id(CardId::new("CS2_029")),
            zone: Zone::Hand,
            count: 1,
        }],
    );
    let then = expect_det(&outcome);
    let created = then
        .entities
        .values()
        .find(|e| e.simulated)
        .expect("created");
    assert_eq!(created.zone, Zone::Hand);
    assert_eq!(then.player(PlayerId(1)).unwrap().hand_count, 1);
    assert!(outcome.trace.entries[0].detail.contains("stats unknown"));
}

#[test]
fn modify_tag_supports_atk_health_cost() {
    let given = default_state();
    let outcome = apply_bound(
        &given,
        vec![
            Operation::ModifyTag {
                target: Selector::Target,
                tag: "ATK".into(),
                value: 9,
            },
            Operation::ModifyTag {
                target: Selector::Target,
                tag: "HEALTH".into(),
                value: 7,
            },
            Operation::ModifyTag {
                target: Selector::Target,
                tag: "COST".into(),
                value: 4,
            },
        ],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    let then = expect_det(&outcome);
    let minion = then.entity(EntityId(10)).unwrap();
    assert_eq!(minion.attack, 9);
    assert_eq!(minion.health, 7);
    assert_eq!(minion.cost, 4);
    assert_eq!(
        trace_ops(&outcome),
        ["modify_tag", "modify_tag", "modify_tag"]
    );
}

#[test]
fn modify_tag_unknown_is_unsupported() {
    let given = default_state();
    let outcome = apply_bound(
        &given,
        vec![Operation::ModifyTag {
            target: Selector::Target,
            tag: "DIVINE_SHIELD".into(),
            value: 1,
        }],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    match outcome.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what }) => {
            assert_eq!(what, "modify_tag:DIVINE_SHIELD");
        }
        other => panic!("expected NotImplemented, got {other:?}"),
    }
}

#[test]
fn refresh_mana_restores_or_adds() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::RefreshMana {
            player: PlayerRef::SelfPlayer,
            amount: None,
        }],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.player(PlayerId(1)).unwrap().mana, 5);

    let outcome = apply_ops(
        &given,
        vec![Operation::RefreshMana {
            player: PlayerRef::SelfPlayer,
            amount: Some(1),
        }],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.player(PlayerId(1)).unwrap().mana, 4);
}

#[test]
fn overload_increments_counter() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::Overload {
            player: PlayerRef::SelfPlayer,
            amount: 2,
        }],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.player(PlayerId(1)).unwrap().overloaded, 2);
    assert_eq!(trace_ops(&outcome), ["overload"]);
}

#[test]
fn conditional_takes_then_or_otherwise() {
    let given = default_state();
    let when_true = vec![Predicate {
        subject: EntityRole::Target,
        condition: EntityCondition {
            zone: Some(Zone::Play),
            ..EntityCondition::default()
        },
    }];
    let outcome = apply_bound(
        &given,
        vec![Operation::Conditional {
            when: when_true,
            then: vec![Operation::GainArmor {
                player: PlayerRef::SelfPlayer,
                amount: 3,
            }],
            otherwise: vec![Operation::GainArmor {
                player: PlayerRef::SelfPlayer,
                amount: 1,
            }],
        }],
        TargetBinding {
            source: None,
            target: Some(EntityId(10)),
        },
    );
    assert_eq!(expect_det(&outcome).player(PlayerId(1)).unwrap().armor, 3);

    let when_missing = vec![Predicate {
        subject: EntityRole::Target,
        condition: EntityCondition::default(),
    }];
    let outcome = apply_ops(
        &given,
        vec![Operation::Conditional {
            when: when_missing,
            then: vec![Operation::GainArmor {
                player: PlayerRef::SelfPlayer,
                amount: 3,
            }],
            otherwise: vec![Operation::GainArmor {
                player: PlayerRef::SelfPlayer,
                amount: 1,
            }],
        }],
    );
    assert_eq!(expect_det(&outcome).player(PlayerId(1)).unwrap().armor, 1);
}

#[test]
fn foreach_rebinds_target_to_each_minion() {
    let given = board(
        vec![
            hero(4, "HERO_01", 1, 30),
            hero(5, "HERO_02", 2, 30),
            minion(10, "CS2_182", 1, 4, 5),
            minion(11, "CS2_200", 1, 6, 7),
        ],
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    );
    let outcome = apply_ops(
        &given,
        vec![Operation::ForEach {
            over: Selector::FriendlyMinions,
            effects: vec![Operation::Damage {
                target: Selector::Target,
                amount: 1,
            }],
        }],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.entity(EntityId(10)).unwrap().damage, 1);
    assert_eq!(then.entity(EntityId(11)).unwrap().damage, 1);
    assert_eq!(then.entity(EntityId(4)).unwrap().damage, 0);
    assert_eq!(trace_ops(&outcome), ["for_each", "damage", "damage"]);
}

#[test]
fn repeat_runs_inner_sequence_n_times() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::Repeat {
            times: 2,
            effects: vec![Operation::Draw {
                player: PlayerRef::SelfPlayer,
                count: 1,
            }],
        }],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.player(PlayerId(1)).unwrap().deck_count, 3);
    assert_eq!(then.player(PlayerId(1)).unwrap().hand_count, 2);
    assert_eq!(trace_ops(&outcome), ["repeat", "draw", "draw"]);
}

#[test]
fn union_and_filter_selectors() {
    let given = board(
        vec![
            hero(4, "HERO_01", 1, 30),
            minion(10, "CS2_182", 1, 4, 5),
            minion(11, "CS2_200", 1, 6, 7),
        ],
        vec![player(1, 30, 0, 3, 5, 5, 0)],
    );
    let outcome = apply_ops(
        &given,
        vec![Operation::Damage {
            target: Selector::Filter(
                Box::new(Selector::Union(vec![
                    Selector::FriendlyMinions,
                    Selector::FriendlyHero,
                ])),
                EntityCondition {
                    cost_at_least: Some(2),
                    ..EntityCondition::default()
                },
            ),
            amount: 1,
        }],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.entity(EntityId(10)).unwrap().damage, 1);
    assert_eq!(then.entity(EntityId(11)).unwrap().damage, 1);
    assert_eq!(then.entity(EntityId(4)).unwrap().damage, 0);
}

#[test]
fn missing_target_binding_is_error() {
    let given = default_state();
    let err = hs_sim::apply_program(
        &given,
        &common::program(
            "TEST_CARD",
            vec![Operation::Destroy {
                target: Selector::Target,
            }],
        ),
        &TargetBinding::default(),
        &common::EmptyPool,
        &hs_sim::SimRequest::default(),
    )
    .expect_err("binding");
    match err {
        hs_sim::SimError::InvalidBinding(msg) => {
            assert!(msg.contains("target"), "{msg}");
        }
        other => panic!("expected InvalidBinding, got {other:?}"),
    }
}

#[test]
fn empty_play_is_deterministic_clone() {
    let given = default_state();
    let outcome = apply_ops(&given, vec![]);
    let then = expect_det(&outcome);
    assert_eq!(then, &given);
    assert!(outcome.trace.entries.is_empty());
    assert_eq!(outcome.meta.rule_version, "TEST_CARD@1..");
    assert_eq!(outcome.meta.seed, 0);
    assert_eq!(outcome.meta.branch_budget, 64);
    assert!(outcome.meta.truncation.is_none());
}

#[test]
fn create_card_dbf_ref_uses_synthetic_id() {
    let given = default_state();
    let outcome = apply_ops(
        &given,
        vec![Operation::CreateCard {
            player: PlayerRef::SelfPlayer,
            card: CardRef::Dbf(hs_types::DbfId(123)),
            zone: Zone::Deck,
            count: 1,
        }],
    );
    let then = expect_det(&outcome);
    let created = then.entities.values().find(|e| e.simulated).unwrap();
    assert_eq!(created.card_id.as_ref().unwrap().as_str(), "dbf:123");
    assert_eq!(then.player(PlayerId(1)).unwrap().deck_count, 6);
}

#[test]
fn friendly_minions_requires_explicit_minion_type() {
    let mut given = default_state();
    let template = given.entity(EntityId(10)).expect("template minion").clone();
    for (id, card_type) in [
        (12, None),
        (13, Some(CardType::Location)),
        (14, Some(CardType::Weapon)),
        (15, Some(CardType::HeroPower)),
    ] {
        let mut entity = template.clone();
        entity.entity_id = EntityId(id);
        entity.card_id = Some(CardId::new(format!("NON_MINION_{id}")));
        entity.card_type = card_type;
        given.entities.insert(entity.entity_id, entity);
    }
    let mut enemy_minion = template.clone();
    enemy_minion.entity_id = EntityId(20);
    enemy_minion.controller = Some(PlayerId(2));
    given.entities.insert(enemy_minion.entity_id, enemy_minion);
    for (id, card_type) in [
        (22, None),
        (23, Some(CardType::Location)),
        (24, Some(CardType::Weapon)),
        (25, Some(CardType::HeroPower)),
    ] {
        let mut entity = template.clone();
        entity.entity_id = EntityId(id);
        entity.card_id = Some(CardId::new(format!("ENEMY_NON_MINION_{id}")));
        entity.card_type = card_type;
        entity.controller = Some(PlayerId(2));
        given.entities.insert(entity.entity_id, entity);
    }

    let outcome = apply_ops(
        &given,
        vec![
            Operation::Damage {
                target: Selector::FriendlyMinions,
                amount: 1,
            },
            Operation::Damage {
                target: Selector::EnemyMinions,
                amount: 1,
            },
        ],
    );
    let then = expect_det(&outcome);
    assert_eq!(then.entity(EntityId(10)).unwrap().damage, 1);
    assert_eq!(then.entity(EntityId(20)).unwrap().damage, 1);
    for id in [12, 13, 14, 15, 22, 23, 24, 25] {
        assert_eq!(
            then.entity(EntityId(id)).unwrap().damage,
            0,
            "CARDTYPE 非显式 Minion 的在场实体 {id} 不得进入 selector"
        );
    }
}
