//! M4 非战斗玩家动作的 Given / When / Then 回归。

mod common;

use common::{
    board, card_definition, deathrattle_card, hero, minion, player, program, EmptyPool, MapCards,
    MapRules,
};
use hs_effect_ir::{Operation, PlayerRef, RuleSet, Selector, TriggerEvent, TriggerProgram};
use hs_sim::{
    apply_action, OutcomeSet, SimAction, SimCardDefinition, SimContext, SimEntity,
    SimEntityActionState, SimGameState, SimOutcome, SimRequest, UnsupportedReason,
};
use hs_types::{BuildId, CardId, CardType, EntityId, PlayerId, Zone};
use std::path::PathBuf;

const BUILD: BuildId = BuildId(1);
const JAIL_BUILD: BuildId = BuildId(249896);

fn action_entity(id: i32, zone: Zone, definition: SimCardDefinition) -> SimEntity {
    let SimCardDefinition {
        card_id,
        card_type,
        cost,
        attack,
        health,
        durability,
    } = definition;
    SimEntity {
        entity_id: EntityId(id),
        card_id: Some(card_id),
        card_type: Some(card_type),
        zone,
        controller: Some(PlayerId(1)),
        attack,
        health,
        damage: 0,
        cost,
        action: SimEntityActionState {
            cost_known: true,
            attack_known: true,
            health_known: true,
            durability,
            durability_known: true,
            ..SimEntityActionState::default()
        },
        simulated: false,
    }
}

fn action_board(extra: Vec<SimEntity>, mana: i32, hand_count: usize) -> SimGameState {
    let mut entities = vec![hero(4, "HERO_01", 1, 30), hero(5, "HERO_02", 2, 30)];
    entities.extend(extra);
    board(
        entities,
        vec![
            player(1, 30, 0, mana, 10, 20, hand_count),
            player(2, 30, 0, 10, 10, 20, 0),
        ],
    )
}

fn run_action(
    state: &SimGameState,
    action: SimAction,
    build: BuildId,
    rules: &MapRules,
    cards: &MapCards,
) -> SimOutcome {
    run_action_with_request(state, action, build, rules, cards, &SimRequest::default())
}

fn run_action_with_request(
    state: &SimGameState,
    action: SimAction,
    build: BuildId,
    rules: &MapRules,
    cards: &MapCards,
    request: &SimRequest,
) -> SimOutcome {
    apply_action(
        state,
        &action,
        &SimContext {
            build,
            pools: &EmptyPool,
            rules,
            cards,
        },
        request,
    )
    .expect("玩家动作不应返回执行器错误")
}

fn deterministic(outcome: SimOutcome) -> SimGameState {
    match outcome.outcome {
        OutcomeSet::Deterministic(state) => state,
        other => panic!("expected deterministic action, got {other:?}"),
    }
}

#[derive(Clone, Copy)]
enum FailureKind {
    ActionUnavailable,
    InsufficientState,
}

fn assert_atomic_failure(
    state: &SimGameState,
    action: SimAction,
    rules: &MapRules,
    cards: &MapCards,
    kind: FailureKind,
    needle: &str,
) {
    let before = state.clone();
    let outcome = run_action(state, action, BUILD, rules, cards);
    let what = match (kind, outcome.outcome) {
        (
            FailureKind::ActionUnavailable,
            OutcomeSet::Unsupported(UnsupportedReason::ActionUnavailable { what }),
        )
        | (
            FailureKind::InsufficientState,
            OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { what }),
        ) => what,
        (_, other) => panic!("expected {needle:?} failure, got {other:?}"),
    };
    assert!(what.contains(needle), "unexpected failure reason: {what}");
    assert_eq!(state, &before, "失败动作不能修改输入状态");
}

#[test]
fn spell_spends_mana_moves_hand_to_graveyard_and_executes_rule() {
    let spell = action_entity(
        20,
        Zone::Hand,
        card_definition("TEST_SPELL", CardType::Spell, 3, 0, 0, 0),
    );
    let given = action_board(vec![spell], 10, 1);
    let before = given.clone();
    let rules = MapRules::new(vec![program(
        "TEST_SPELL",
        vec![Operation::GainArmor {
            player: PlayerRef::SelfPlayer,
            amount: 2,
        }],
    )]);
    let cards = MapCards::new(
        BUILD,
        vec![card_definition("TEST_SPELL", CardType::Spell, 3, 0, 0, 0)],
    );

    let outcome = run_action(
        &given,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: None,
        },
        BUILD,
        &rules,
        &cards,
    );
    assert_eq!(outcome.rule_uses.len(), 1);
    assert_eq!(outcome.rule_uses[0].card_id.as_str(), "TEST_SPELL");
    assert_eq!(
        outcome
            .trace
            .entries
            .iter()
            .map(|entry| entry.op.as_str())
            .collect::<Vec<_>>(),
        ["action", "gain_armor"]
    );
    let then = deterministic(outcome);

    assert_eq!(then.entity(EntityId(20)).unwrap().zone, Zone::Graveyard);
    assert_eq!(then.player(PlayerId(1)).unwrap().mana, 7);
    assert_eq!(then.player(PlayerId(1)).unwrap().hand_count, 0);
    assert_eq!(then.player(PlayerId(1)).unwrap().armor, 2);
    assert_eq!(given, before, "成功动作也只应修改副本");
}

#[test]
fn missing_source_rule_is_preserved_as_structured_unknown_evidence() {
    let spell = action_entity(
        20,
        Zone::Hand,
        card_definition("MISSING_RULE", CardType::Spell, 1, 0, 0, 0),
    );
    let given = action_board(vec![spell], 10, 1);
    let before = given.clone();
    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(
        BUILD,
        vec![card_definition("MISSING_RULE", CardType::Spell, 1, 0, 0, 0)],
    );

    let outcome = run_action(
        &given,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: None,
        },
        BUILD,
        &rules,
        &cards,
    );

    assert!(matches!(
        outcome.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { .. })
    ));
    assert_eq!(outcome.unknown_rule_cards, [CardId::new("MISSING_RULE")]);
    assert!(outcome.rule_uses.is_empty());
    assert_eq!(given, before, "缺规则失败不能发布已扣费的工作副本");
}

#[test]
fn minion_enters_requested_position_and_dispatches_battlecry() {
    let mut left = minion(10, "LEFT", 1, 1, 2);
    left.action.zone_position = Some(1);
    let mut right = minion(11, "RIGHT", 1, 1, 2);
    right.action.zone_position = Some(2);
    let played = action_entity(
        20,
        Zone::Hand,
        card_definition("BATTLECRY", CardType::Minion, 3, 3, 4, 0),
    );
    let given = action_board(vec![left, right, played], 10, 1);

    let mut battlecry = program("BATTLECRY", Vec::new());
    battlecry.triggers.push(TriggerProgram {
        event: TriggerEvent::Battlecry,
        when: Vec::new(),
        effects: vec![Operation::GainArmor {
            player: PlayerRef::SelfPlayer,
            amount: 3,
        }],
    });
    let rules = MapRules::new(vec![battlecry]);
    let cards = MapCards::new(
        BUILD,
        vec![card_definition("BATTLECRY", CardType::Minion, 3, 3, 4, 0)],
    );

    let outcome = run_action(
        &given,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: Some(2),
        },
        BUILD,
        &rules,
        &cards,
    );
    assert!(outcome
        .trace
        .entries
        .iter()
        .any(|entry| entry.op == "trigger" && entry.detail.contains("Battlecry dispatched")));
    let then = deterministic(outcome);

    assert_eq!(
        then.entity(EntityId(10)).unwrap().action.zone_position,
        Some(1)
    );
    assert_eq!(
        then.entity(EntityId(20)).unwrap().action.zone_position,
        Some(2)
    );
    assert_eq!(
        then.entity(EntityId(11)).unwrap().action.zone_position,
        Some(3)
    );
    assert_eq!(then.entity(EntityId(20)).unwrap().zone, Zone::Play);
    assert!(
        then.entity(EntityId(20)).unwrap().action.exhausted,
        "普通随从入场后应保留召唤疲劳，不能立刻攻击"
    );
    assert_eq!(then.player(PlayerId(1)).unwrap().mana, 7);
    assert_eq!(then.player(PlayerId(1)).unwrap().hand_count, 0);
    assert_eq!(then.player(PlayerId(1)).unwrap().armor, 3);
}

#[test]
fn playing_a_weapon_updates_the_hero_for_the_followup_attack() {
    let weapon_definition = card_definition("TEST_WEAPON", CardType::Weapon, 3, 3, 0, 2);
    let weapon = action_entity(20, Zone::Hand, weapon_definition.clone());
    let mut given = action_board(vec![weapon], 10, 1);
    given
        .entities
        .get_mut(&EntityId(4))
        .unwrap()
        .action
        .attack_known = false;
    let rules = MapRules::new(vec![program("TEST_WEAPON", Vec::new())]);
    let cards = MapCards::new(
        BUILD,
        vec![
            weapon_definition,
            card_definition("HERO_01", CardType::Hero, 0, 0, 30, 0),
        ],
    );

    let equipped = deterministic(run_action(
        &given,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: None,
        },
        BUILD,
        &rules,
        &cards,
    ));
    assert_eq!(equipped.entity(EntityId(4)).unwrap().attack, 3);
    assert!(equipped.entity(EntityId(4)).unwrap().action.attack_known);
    assert_eq!(equipped.entity(EntityId(20)).unwrap().zone, Zone::Play);

    let attacked = deterministic(run_action(
        &equipped,
        SimAction::Attack {
            attacker: EntityId(4),
            defender: EntityId(5),
        },
        BUILD,
        &rules,
        &cards,
    ));
    assert_eq!(attacked.player(PlayerId(2)).unwrap().hero_health, 27);
    assert_eq!(attacked.entity(EntityId(20)).unwrap().action.durability, 1);
}

#[test]
fn playing_location_only_places_it_and_does_not_run_activation_program() {
    let location = action_entity(
        30,
        Zone::Hand,
        card_definition("TEST_LOCATION", CardType::Location, 2, 0, 0, 2),
    );
    let given = action_board(vec![location], 10, 1);
    let rules = MapRules::new(vec![program(
        "TEST_LOCATION",
        vec![Operation::GainArmor {
            player: PlayerRef::SelfPlayer,
            amount: 99,
        }],
    )]);
    let cards = MapCards::new(
        BUILD,
        vec![card_definition(
            "TEST_LOCATION",
            CardType::Location,
            2,
            0,
            0,
            2,
        )],
    );

    let outcome = run_action(
        &given,
        SimAction::PlayCard {
            entity: EntityId(30),
            target: None,
            sub_option: None,
            position: Some(1),
        },
        BUILD,
        &rules,
        &cards,
    );
    assert!(outcome.rule_uses.is_empty(), "入场没有消费 activation 规则");
    assert!(!outcome
        .trace
        .entries
        .iter()
        .any(|entry| entry.op == "gain_armor"));
    let then = deterministic(outcome);

    let placed = then.entity(EntityId(30)).unwrap();
    assert_eq!(placed.zone, Zone::Play);
    assert_eq!(placed.action.zone_position, Some(1));
    assert_eq!(placed.action.durability, 2);
    assert_eq!(then.player(PlayerId(1)).unwrap().mana, 8);
    assert_eq!(then.player(PlayerId(1)).unwrap().hand_count, 0);
    assert_eq!(then.player(PlayerId(1)).unwrap().armor, 0);

    let activated = deterministic(run_action(
        &then,
        SimAction::ActivateLocation {
            entity: EntityId(30),
            target: None,
            sub_option: None,
        },
        BUILD,
        &rules,
        &cards,
    ));
    assert_eq!(
        activated.player(PlayerId(1)).unwrap().armor,
        99,
        "Location 可以在从手牌打出的同一回合立即使用"
    );
}

#[test]
fn location_missing_activation_rule_preserves_card_id_without_publishing_consumption() {
    let mut location = action_entity(
        30,
        Zone::Play,
        card_definition("MISSING_LOCATION_RULE", CardType::Location, 2, 0, 0, 1),
    );
    location.action.zone_position = Some(1);
    let given = action_board(vec![location], 10, 0);
    let before = given.clone();
    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(
        BUILD,
        vec![card_definition(
            "MISSING_LOCATION_RULE",
            CardType::Location,
            2,
            0,
            0,
            1,
        )],
    );

    let outcome = run_action(
        &given,
        SimAction::ActivateLocation {
            entity: EntityId(30),
            target: None,
            sub_option: None,
        },
        BUILD,
        &rules,
        &cards,
    );

    assert!(matches!(
        outcome.outcome,
        OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { .. })
    ));
    assert_eq!(
        outcome.unknown_rule_cards,
        [CardId::new("MISSING_LOCATION_RULE")]
    );
    assert!(outcome.rule_uses.is_empty());
    assert_eq!(
        given, before,
        "缺规则不能发布 Location 的耐久、冷却或离场 mutation"
    );
}

#[test]
fn hero_power_spends_mana_marks_exhausted_and_cannot_be_used_twice() {
    let power = action_entity(
        30,
        Zone::Play,
        card_definition("TEST_POWER", CardType::HeroPower, 2, 0, 0, 0),
    );
    let given = action_board(vec![power], 10, 0);
    let rules = MapRules::new(vec![program(
        "TEST_POWER",
        vec![Operation::GainArmor {
            player: PlayerRef::SelfPlayer,
            amount: 1,
        }],
    )]);
    let cards = MapCards::new(
        BUILD,
        vec![card_definition(
            "TEST_POWER",
            CardType::HeroPower,
            2,
            0,
            0,
            0,
        )],
    );
    let action = SimAction::UseHeroPower {
        entity: EntityId(30),
        target: None,
        sub_option: None,
    };

    let after_first = deterministic(run_action(&given, action, BUILD, &rules, &cards));
    assert_eq!(after_first.player(PlayerId(1)).unwrap().mana, 8);
    assert_eq!(after_first.player(PlayerId(1)).unwrap().armor, 1);
    assert!(after_first.entity(EntityId(30)).unwrap().action.exhausted);

    assert_atomic_failure(
        &after_first,
        action,
        &rules,
        &cards,
        FailureKind::ActionUnavailable,
        "已使用",
    );
}

#[test]
fn jail_location_activation_uses_exact_token_metadata_and_consumes_use() {
    let rules_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/cards");
    let set = RuleSet::load_dir(&rules_dir).expect("load repository card rules");
    let jail_program = set
        .select(JAIL_BUILD, &CardId::new("JAIL_877"))
        .expect("JAIL_877 rule for test build")
        .clone();
    let rules = MapRules::new(vec![jail_program]);
    let cards = MapCards::new(
        JAIL_BUILD,
        vec![
            card_definition("JAIL_877", CardType::Location, 3, 0, 0, 2),
            card_definition("JAIL_877t", CardType::Minion, 2, 2, 1, 0),
        ],
    );
    let mut location = action_entity(
        30,
        Zone::Play,
        card_definition("JAIL_877", CardType::Location, 3, 0, 0, 2),
    );
    location.action.zone_position = Some(1);
    location.action.location_action_cost = 0;
    let mut given = action_board(vec![location], 10, 0);
    given.build_id = Some(JAIL_BUILD);

    let outcome = run_action(
        &given,
        SimAction::ActivateLocation {
            entity: EntityId(30),
            target: None,
            sub_option: None,
        },
        JAIL_BUILD,
        &rules,
        &cards,
    );
    assert_eq!(outcome.rule_uses.len(), 1);
    assert_eq!(outcome.rule_uses[0].card_id.as_str(), "JAIL_877");
    assert!(
        outcome.unknown_rule_cards.is_empty(),
        "活着的 token 不应凭空产生亡语缺口；其日后死亡时再报告未知规则"
    );
    let then = deterministic(outcome);

    let jail = then.entity(EntityId(30)).unwrap();
    assert_eq!(jail.action.durability, 1);
    assert_eq!(jail.action.location_cooldown, 1);
    assert!(jail.action.exhausted);
    assert_eq!(then.player(PlayerId(1)).unwrap().mana, 10);

    let token = then
        .entities
        .values()
        .find(|entity| entity.card_id.as_ref().map(CardId::as_str) == Some("JAIL_877t"))
        .expect("location activation summons JAIL_877t");
    assert_eq!(token.card_type, Some(CardType::Minion));
    assert_eq!((token.attack, token.health), (2, 1));
    assert!(token.action.attack_known);
    assert!(token.action.health_known);
    assert!(token.action.exhausted, "刚召唤的 token 不得立刻攻击");
    assert_eq!(token.action.zone_position, Some(2));
    assert!(token.simulated);
    assert_eq!(token.damage, 0);
}

#[test]
fn one_use_location_leaves_first_and_frees_a_full_board_slot() {
    let mut location = action_entity(
        30,
        Zone::Play,
        card_definition("ONE_USE_LOCATION", CardType::Location, 2, 0, 0, 1),
    );
    location.action.zone_position = Some(1);
    let mut entities = vec![location];
    for position in 2..=7 {
        let mut occupant = minion(
            30 + i32::from(position),
            &format!("OCCUPANT_{position}"),
            1,
            1,
            2,
        );
        occupant.action.zone_position = Some(position);
        entities.push(occupant);
    }
    let given = action_board(entities, 10, 0);
    let rules = MapRules::new(vec![program(
        "ONE_USE_LOCATION",
        vec![Operation::Summon {
            player: PlayerRef::SelfPlayer,
            card: hs_effect_ir::CardRef::Id(CardId::new("LOCATION_TOKEN")),
            count: 1,
        }],
    )]);
    let cards = MapCards::new(
        BUILD,
        vec![
            card_definition("ONE_USE_LOCATION", CardType::Location, 2, 0, 0, 1),
            card_definition("LOCATION_TOKEN", CardType::Minion, 0, 2, 1, 0),
        ],
    );

    let outcome = run_action(
        &given,
        SimAction::ActivateLocation {
            entity: EntityId(30),
            target: None,
            sub_option: None,
        },
        BUILD,
        &rules,
        &cards,
    );
    let then = deterministic(outcome);

    let removed = then.entity(EntityId(30)).unwrap();
    assert_eq!(removed.zone, Zone::Graveyard);
    assert_eq!(removed.action.zone_position, None);
    assert_eq!(removed.action.durability, 0);
    assert_eq!(removed.action.location_cooldown, 1);
    assert!(removed.action.exhausted);
    assert_eq!(then.player(PlayerId(1)).unwrap().mana, 10);
    let token = then
        .entities
        .values()
        .find(|entity| entity.card_id.as_ref().map(CardId::as_str) == Some("LOCATION_TOKEN"))
        .expect("最后一格由 Location 离场后召唤的 token 占据");
    assert_eq!(token.action.zone_position, Some(7));
    assert_eq!((token.attack, token.health), (2, 1));
}

#[test]
fn action_replays_identically_including_trace_meta_and_rule_use_order() {
    let spell = action_entity(
        20,
        Zone::Hand,
        card_definition("Z_REPRO_SPELL", CardType::Spell, 1, 0, 0, 0),
    );
    let victim = minion(30, "A_DEATHRATTLE", 2, 1, 1);
    let given = action_board(vec![spell, victim], 10, 1);
    let rules = MapRules::new(vec![
        program(
            "Z_REPRO_SPELL",
            vec![Operation::Damage {
                target: Selector::Target,
                amount: 1,
            }],
        ),
        deathrattle_card(
            "A_DEATHRATTLE",
            vec![Operation::GainArmor {
                player: PlayerRef::SelfPlayer,
                amount: 2,
            }],
        ),
    ]);
    let cards = MapCards::new(
        BUILD,
        vec![card_definition(
            "Z_REPRO_SPELL",
            CardType::Spell,
            1,
            0,
            0,
            0,
        )],
    );
    let action = SimAction::PlayCard {
        entity: EntityId(20),
        target: Some(EntityId(30)),
        sub_option: None,
        position: None,
    };
    let request = SimRequest {
        seed: 0x1234_5678_9abc_def0,
        branch_budget: 7,
    };

    let first = run_action_with_request(&given, action, BUILD, &rules, &cards, &request);
    let second = run_action_with_request(&given, action, BUILD, &rules, &cards, &request);

    match (&first.outcome, &second.outcome) {
        (OutcomeSet::Deterministic(left), OutcomeSet::Deterministic(right)) => {
            assert_eq!(
                serde_json::to_string(left).unwrap(),
                serde_json::to_string(right).unwrap(),
                "最终状态序列化必须逐字节可复现"
            );
        }
        (left, right) => panic!("expected two deterministic outcomes, got {left:?} / {right:?}"),
    }
    assert_eq!(first.trace, second.trace);
    assert_eq!(first.meta, second.meta);
    assert_eq!(first.meta.seed, request.seed);
    assert_eq!(first.meta.branch_budget, request.branch_budget);
    assert_eq!(first.rule_uses, second.rule_uses);
    assert_eq!(first.unknown_rule_cards, second.unknown_rule_cards);
    assert_eq!(
        first
            .rule_uses
            .iter()
            .map(|rule| rule.card_id.as_str())
            .collect::<Vec<_>>(),
        ["A_DEATHRATTLE", "Z_REPRO_SPELL"]
    );
}

#[test]
fn end_turn_sets_terminal_marker_and_blocks_further_actions() {
    let given = action_board(Vec::new(), 10, 0);
    let rules = MapRules::new(Vec::new());
    let cards = MapCards::new(BUILD, Vec::new());

    let ended = deterministic(run_action(
        &given,
        SimAction::EndTurn,
        BUILD,
        &rules,
        &cards,
    ));
    assert!(ended.turn_ended);
    assert_eq!(ended.turn, given.turn);
    assert_atomic_failure(
        &ended,
        SimAction::EndTurn,
        &rules,
        &cards,
        FailureKind::ActionUnavailable,
        "已经结束",
    );
}

#[test]
fn unavailable_and_incomplete_actions_are_atomic() {
    let blank_rule = program("FAIL_SPELL", Vec::new());
    let rules = MapRules::new(vec![blank_rule]);
    let cards = MapCards::new(
        BUILD,
        vec![card_definition("FAIL_SPELL", CardType::Spell, 5, 0, 0, 0)],
    );
    let low_mana = action_board(
        vec![action_entity(
            20,
            Zone::Hand,
            card_definition("FAIL_SPELL", CardType::Spell, 5, 0, 0, 0),
        )],
        1,
        1,
    );
    assert_atomic_failure(
        &low_mana,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: None,
        },
        &rules,
        &cards,
        FailureKind::ActionUnavailable,
        "法力不足",
    );

    let mut full_board_entities = Vec::new();
    for (offset, position) in (0..7).zip(1..=7) {
        let mut entity = minion(10 + offset, &format!("BOARD_{offset}"), 1, 1, 1);
        entity.action.zone_position = Some(position);
        full_board_entities.push(entity);
    }
    full_board_entities.push(action_entity(
        30,
        Zone::Hand,
        card_definition("FAIL_MINION", CardType::Minion, 1, 1, 1, 0),
    ));
    let full_board = action_board(full_board_entities, 10, 1);
    let board_rules = MapRules::new(vec![program("FAIL_MINION", Vec::new())]);
    let board_cards = MapCards::new(
        BUILD,
        vec![card_definition("FAIL_MINION", CardType::Minion, 1, 1, 1, 0)],
    );
    assert_atomic_failure(
        &full_board,
        SimAction::PlayCard {
            entity: EntityId(30),
            target: None,
            sub_option: None,
            position: Some(1),
        },
        &board_rules,
        &board_cards,
        FailureKind::ActionUnavailable,
        "棋盘已满",
    );

    let empty = action_board(Vec::new(), 10, 0);
    let empty_rules = MapRules::new(Vec::new());
    let empty_cards = MapCards::new(BUILD, Vec::new());
    assert_atomic_failure(
        &empty,
        SimAction::PlayCard {
            entity: EntityId(999),
            target: None,
            sub_option: None,
            position: None,
        },
        &empty_rules,
        &empty_cards,
        FailureKind::InsufficientState,
        "源实体 999 不存在",
    );

    let target_missing = action_board(
        vec![action_entity(
            20,
            Zone::Hand,
            card_definition("FAIL_SPELL", CardType::Spell, 5, 0, 0, 0),
        )],
        10,
        1,
    );
    assert_atomic_failure(
        &target_missing,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: Some(EntityId(999)),
            sub_option: None,
            position: None,
        },
        &rules,
        &cards,
        FailureKind::InsufficientState,
        "目标实体 999 不存在",
    );

    let missing_definition = action_board(
        vec![action_entity(
            20,
            Zone::Hand,
            card_definition("NO_DEFINITION", CardType::Spell, 1, 0, 0, 0),
        )],
        10,
        1,
    );
    assert_atomic_failure(
        &missing_definition,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: None,
        },
        &empty_rules,
        &empty_cards,
        FailureKind::InsufficientState,
        "缺卡牌定义 NO_DEFINITION",
    );

    let exact_build = action_board(
        vec![action_entity(
            20,
            Zone::Hand,
            card_definition("FAIL_SPELL", CardType::Spell, 5, 0, 0, 0),
        )],
        10,
        1,
    );
    let mut missing_build = exact_build.clone();
    missing_build.build_id = None;
    assert_atomic_failure(
        &missing_build,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: None,
        },
        &rules,
        &cards,
        FailureKind::InsufficientState,
        "Build 不精确",
    );

    let mut mismatched_build = exact_build;
    mismatched_build.build_id = Some(BuildId(2));
    assert_atomic_failure(
        &mismatched_build,
        SimAction::PlayCard {
            entity: EntityId(20),
            target: None,
            sub_option: None,
            position: None,
        },
        &rules,
        &cards,
        FailureKind::InsufficientState,
        "Build 不精确",
    );
}
