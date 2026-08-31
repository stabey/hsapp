//! from_observed:用 hs-observed-state + hs-log-core 事件归约构造小状态。

use hs_log_core::{game_tag, DomainEvent, TagKey};
use hs_observed_state::{reduce, ObservedGameState};
use hs_sim::SimGameState;
use hs_types::{BuildId, CardId, CardType, EntityId, GameId, GamePhase, PlayerId, Zone};

fn reduce_all(events: &[DomainEvent]) -> ObservedGameState {
    events
        .iter()
        .fold(ObservedGameState::default(), |state, event| {
            reduce(&state, event).expect("reduce")
        })
}

fn created(entity_id: i32, card_id: Option<&str>, zone: Zone) -> DomainEvent {
    DomainEvent::EntityCreated {
        entity_id: EntityId(entity_id),
        card_id: card_id.map(CardId::new),
        zone,
    }
}

fn tag(entity_id: i32, tag: TagKey, value: i32) -> DomainEvent {
    DomainEvent::TagChanged {
        entity_id: EntityId(entity_id),
        tag,
        value,
    }
}

fn named_tag(entity_id: i32, name: &str, value: i32) -> DomainEvent {
    tag(entity_id, TagKey(game_tag::id_by_name(name)), value)
}

#[test]
fn from_observed_projects_entities_and_player_counts() {
    let observed = reduce_all(&[
        DomainEvent::GameCreated {
            game_id: GameId(1),
            build_id: Some(BuildId(123_456)),
        },
        created(1, None, Zone::Play),
        tag(1, TagKey::TURN, 4),
        tag(1, TagKey::STEP, game_tag::step::MAIN_ACTION),
        created(2, None, Zone::Play),
        tag(2, TagKey::PLAYER_ID, 1),
        tag(2, TagKey::CONTROLLER, 1),
        tag(2, TagKey(game_tag::CURRENT_PLAYER), 1),
        tag(2, TagKey(game_tag::RESOURCES), 5),
        tag(2, TagKey(game_tag::RESOURCES_USED), 2),
        tag(2, TagKey(game_tag::HERO_ENTITY), 4),
        created(3, None, Zone::Play),
        tag(3, TagKey::PLAYER_ID, 2),
        tag(3, TagKey::CONTROLLER, 2),
        tag(3, TagKey(game_tag::HERO_ENTITY), 5),
        created(4, Some("HERO_01"), Zone::Play),
        tag(4, TagKey::HEALTH, 30),
        tag(4, TagKey::DAMAGE, 4),
        tag(4, TagKey::ATK, 0),
        tag(4, TagKey::COST, 0),
        tag(4, TagKey::CONTROLLER, 1),
        tag(4, TagKey::CARDTYPE, 3),
        named_tag(4, "ARMOR", 5),
        created(5, Some("HERO_02"), Zone::Play),
        tag(5, TagKey::HEALTH, 30),
        tag(5, TagKey::CONTROLLER, 2),
        tag(5, TagKey::CARDTYPE, 3),
        created(10, Some("CS2_182"), Zone::Play),
        tag(10, TagKey::ATK, 4),
        tag(10, TagKey::HEALTH, 5),
        tag(10, TagKey::CONTROLLER, 1),
        created(11, Some("CS2_029"), Zone::Hand),
        tag(11, TagKey::CONTROLLER, 1),
        created(12, None, Zone::Deck),
        tag(12, TagKey::CONTROLLER, 1),
        created(13, None, Zone::Hand),
        tag(13, TagKey::CONTROLLER, 2),
        DomainEvent::FriendlyPlayerDetected {
            player: PlayerId(1),
        },
    ]);

    let sim = SimGameState::from_observed(&observed);

    assert_eq!(sim.friendly_player, Some(PlayerId(1)));
    assert_eq!(sim.build_id, Some(BuildId(123_456)));
    assert_eq!(sim.current_player, Some(PlayerId(1)));
    assert_eq!(sim.phase, GamePhase::Playing);
    assert_eq!(sim.turn, 4);
    assert!(!sim.turn_ended);
    assert_eq!(sim.source_revision, observed.revision);

    let hero = sim.entity(EntityId(4)).unwrap();
    assert_eq!(hero.card_id.as_ref().unwrap().as_str(), "HERO_01");
    assert_eq!(hero.health, 30);
    assert_eq!(hero.damage, 4);
    assert!(hero.action.attack_known, "真实 ATK=0 仍是已知值");
    assert!(hero.action.cost_known, "真实 COST=0 仍是已知值");
    assert!(hero.action.health_known);
    assert!(hero.is_hero());

    let hidden = sim.entity(EntityId(13)).unwrap();
    assert_eq!(hidden.card_id, None, "未揭示手牌不得猜测 card_id");
    assert_eq!(hidden.zone, Zone::Hand);
    assert!(!hidden.action.attack_known);
    assert!(!hidden.action.health_known);
    assert!(!hidden.action.cost_known);
    assert!(!hidden.action.durability_known);

    let minion = sim.entity(EntityId(10)).unwrap();
    assert_eq!(minion.attack, 4);
    assert_eq!(minion.health, 5);

    let p1 = sim.player(PlayerId(1)).unwrap();
    assert_eq!(p1.hero_health, 26);
    assert_eq!(p1.armor, 5);
    assert_eq!(p1.mana, 3);
    assert_eq!(p1.max_mana, 5);
    assert_eq!(p1.deck_count, 1);
    assert_eq!(p1.hand_count, 1);
    assert_eq!(p1.fatigue, 0);

    let p2 = sim.player(PlayerId(2)).unwrap();
    assert_eq!(p2.hand_count, 1);
    assert_eq!(p2.deck_count, 0);
    assert_eq!(p2.hero_health, 30);
    assert_eq!(p2.armor, 0);

    assert_eq!(sim.hero_entity(PlayerId(1)), Some(EntityId(4)));
}

#[test]
fn from_observed_projects_action_facts_without_local_tag_numbers() {
    let observed = reduce_all(&[
        DomainEvent::GameCreated {
            game_id: GameId(1),
            build_id: Some(BuildId(123_456)),
        },
        created(10, Some("ACTION_ENTITY"), Zone::Play),
        tag(10, TagKey::ATK, 0),
        tag(10, TagKey::HEALTH, 5),
        tag(10, TagKey::COST, 0),
        tag(10, TagKey::ZONE_POSITION, 2),
        named_tag(10, "DURABILITY", 3),
        named_tag(10, "NUM_ATTACKS_THIS_TURN", 1),
        named_tag(10, "EXHAUSTED", 1),
        named_tag(10, "FROZEN", 1),
        named_tag(10, "TAUNT", 1),
        named_tag(10, "STEALTH", 1),
        named_tag(10, "LOCATION_ACTION_COOLDOWN", 2),
        named_tag(10, "LOCATION_ACTION_COST", 1),
        named_tag(10, "WINDFURY", 1),
        named_tag(10, "MEGA_WINDFURY", 1),
        named_tag(10, "DIVINE_SHIELD", 1),
        named_tag(10, "POISONOUS", 1),
        named_tag(10, "LIFESTEAL", 1),
        named_tag(10, "IMMUNE", 1),
        named_tag(10, "IMMUNE_WHILE_ATTACKING", 1),
        named_tag(10, "CHARGE", 1),
        named_tag(10, "RUSH", 1),
        named_tag(10, "CANT_ATTACK", 1),
        named_tag(10, "CANT_BE_ATTACKED", 1),
        named_tag(10, "CANNOT_ATTACK_HEROES", 1),
    ]);

    let sim = SimGameState::from_observed(&observed);
    let entity = sim.entity(EntityId(10)).expect("projected entity");
    let action = entity.action;

    assert_eq!(entity.attack, 0);
    assert_eq!(entity.cost, 0);
    assert!(action.attack_known, "真实 ATK=0 不能退化为未知");
    assert!(action.health_known);
    assert!(action.cost_known, "真实 COST=0 不能退化为未知");
    assert_eq!(action.zone_position, Some(2));
    assert_eq!(action.durability, 3);
    assert!(action.durability_known);
    assert_eq!(action.attacks_this_turn, 1);
    assert!(action.exhausted);
    assert!(action.frozen);
    assert!(action.taunt);
    assert!(action.stealth);
    assert_eq!(action.location_cooldown, 2);
    assert_eq!(action.location_action_cost, 1);
    assert!(action.windfury);
    assert!(action.mega_windfury);
    assert!(action.divine_shield);
    assert!(action.poisonous);
    assert!(action.lifesteal);
    assert!(action.immune);
    assert!(action.immune_while_attacking);
    assert!(action.charge);
    assert!(action.rush);
    assert!(action.cant_attack);
    assert!(action.cant_be_attacked);
    assert!(action.cannot_attack_heroes);
}

#[test]
fn legacy_sim_json_defaults_new_action_fields() {
    let observed = reduce_all(&[
        DomainEvent::GameCreated {
            game_id: GameId(1),
            build_id: Some(BuildId(123_456)),
        },
        created(10, Some("MINION_A"), Zone::Play),
        tag(10, TagKey::ATK, 0),
    ]);
    let sim = SimGameState::from_observed(&observed);
    let mut legacy = serde_json::to_value(&sim).expect("serialize current state");
    let root = legacy.as_object_mut().expect("state object");
    root.remove("build_id");
    root.remove("current_player");
    root.remove("turn_ended");
    for entity in root
        .get_mut("entities")
        .and_then(serde_json::Value::as_object_mut)
        .expect("entities object")
        .values_mut()
    {
        entity
            .as_object_mut()
            .expect("entity object")
            .remove("action");
    }

    let restored: SimGameState = serde_json::from_value(legacy).expect("read legacy state");
    assert_eq!(restored.build_id, None);
    assert_eq!(restored.current_player, None);
    assert!(!restored.turn_ended);
    assert_eq!(
        restored.entity(EntityId(10)).expect("entity").action,
        Default::default()
    );
}

/// `card_type` 从观测 `CARDTYPE` tag 投影(A6)。
///
/// 值空间必须与解析器写入 tags 时的一致 —— 这里直接用 `game_tag::card_type::*`,
/// 与 `EntityState::card_type()` 同源,不在测试里手写数字。
#[test]
fn from_observed_projects_card_type() {
    let observed = reduce_all(&[
        DomainEvent::GameCreated {
            game_id: GameId(1),
            build_id: None,
        },
        created(10, Some("HERO_01"), Zone::Play),
        tag(10, TagKey::CARDTYPE, game_tag::card_type::HERO),
        created(11, Some("MINION_A"), Zone::Play),
        tag(11, TagKey::CARDTYPE, game_tag::card_type::MINION),
        created(12, Some("WEAPON_A"), Zone::Play),
        tag(12, TagKey::CARDTYPE, game_tag::card_type::WEAPON),
        // 未观测 CARDTYPE 的实体:保持 None,不猜测。
        created(13, None, Zone::Hand),
        created(14, Some("LOCATION_A"), Zone::Play),
        // 故意使用协议原始数字,避免与待测常量同源而产生假绿。
        tag(14, TagKey::CARDTYPE, 39),
    ]);
    let sim = SimGameState::from_observed(&observed);

    assert_eq!(
        sim.entity(EntityId(10)).unwrap().card_type,
        Some(CardType::Hero)
    );
    assert_eq!(
        sim.entity(EntityId(11)).unwrap().card_type,
        Some(CardType::Minion)
    );
    assert_eq!(
        sim.entity(EntityId(12)).unwrap().card_type,
        Some(CardType::Weapon)
    );
    assert_eq!(sim.entity(EntityId(13)).unwrap().card_type, None);
    assert_eq!(
        sim.entity(EntityId(14)).unwrap().card_type,
        Some(CardType::Location)
    );

    // 英雄判定改用 card_type 后仍成立;武器不再被旧启发式误当随从。
    assert!(sim.entity(EntityId(10)).unwrap().is_hero());
    assert!(!sim.entity(EntityId(12)).unwrap().is_hero());

    // 死亡结算范围:随从/武器会死,英雄不走死亡结算。
    assert!(!sim.entity(EntityId(10)).unwrap().dies_on_zero_health());
    assert!(sim.entity(EntityId(11)).unwrap().dies_on_zero_health());
    assert!(sim.entity(EntityId(12)).unwrap().dies_on_zero_health());
    assert!(sim.entity(EntityId(14)).unwrap().dies_on_zero_health());
}

/// 未观测 CARDTYPE 时退回 M3 旧启发式,行为不比 M3 差。
#[test]
fn card_type_absent_falls_back_to_m3_heuristic() {
    let observed = reduce_all(&[
        DomainEvent::GameCreated {
            game_id: GameId(1),
            build_id: None,
        },
        created(20, Some("HERO_09"), Zone::Play),
        created(21, Some("CS2_182"), Zone::Play),
    ]);
    let sim = SimGameState::from_observed(&observed);

    assert_eq!(sim.entity(EntityId(20)).unwrap().card_type, None);
    assert!(
        sim.entity(EntityId(20)).unwrap().is_hero(),
        "HERO_* 启发式仍生效"
    );
    assert!(
        sim.entity(EntityId(21)).unwrap().dies_on_zero_health(),
        "在场已知牌面当随从"
    );
}
