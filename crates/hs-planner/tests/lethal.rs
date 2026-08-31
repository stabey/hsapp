use std::collections::BTreeMap;
use std::sync::Arc;

use hs_advisor_api::{ActionKind, ActionOrigin, LethalAssessment, SearchConfig};
use hs_effect_ir::{
    EffectProgram, EntityCondition, Operation, PlayerRef, Selector, TriggerEvent, TriggerProgram,
    IR_SCHEMA_VERSION,
};
use hs_log_core::{
    game_tag, DomainEvent, OptionAvailability, OptionSummary, OptionTargetSummary, TagKey,
};
use hs_observed_state::{reduce, EntityState, ObservedGameState};
use hs_planner::{
    prepare_request, search_lethal, LethalStopReason, PlannerRequest, PlannerResources,
};
use hs_pools::{PoolSnapshot, WeightedCard};
use hs_sim::{CardLookup, PoolResolver, RuleLookup, SimCardDefinition, SimError};
use hs_types::{
    BuildId, BuildRange, CardId, CardType, EntityId, GameId, GamePhase, PlayerId, PoolId,
    RuleStatus, Zone,
};

const BUILD: BuildId = BuildId(123_456);
const FRIENDLY: PlayerId = PlayerId(1);
const OPPONENT: PlayerId = PlayerId(2);
const FRIENDLY_HERO: EntityId = EntityId(4);
const ENEMY_HERO: EntityId = EntityId(5);

#[derive(Default)]
struct TestRules(BTreeMap<CardId, EffectProgram>);

impl RuleLookup for TestRules {
    fn program_for(&self, card_id: &CardId) -> Option<&EffectProgram> {
        self.0.get(card_id)
    }
}

#[derive(Default)]
struct TestCards(BTreeMap<(BuildId, CardId), SimCardDefinition>);

impl CardLookup for TestCards {
    fn sim_card(&self, build: BuildId, card_id: &CardId) -> Option<SimCardDefinition> {
        self.0.get(&(build, card_id.clone())).cloned()
    }
}

#[derive(Default)]
struct TestPools(BTreeMap<PoolId, PoolSnapshot>);

impl PoolResolver for TestPools {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        self.0
            .get(pool_id)
            .cloned()
            .ok_or(SimError::NotImplemented("missing lethal test pool"))
    }
}

struct Case {
    state: ObservedGameState,
    options: Vec<OptionSummary>,
    rules: TestRules,
    cards: TestCards,
    pools: TestPools,
}

impl Case {
    fn new(enemy_health: i32, enemy_armor: i32, friendly_mana: i32) -> Self {
        let mut state = ObservedGameState::default();
        state.build_id = Some(BUILD);
        state.game_id = Some(GameId(77));
        state.phase = GamePhase::Playing;
        state.current_player = Some(FRIENDLY);
        state.friendly_player = Some(FRIENDLY);
        state.revision = 1;
        add_player(
            &mut state,
            FRIENDLY,
            EntityId(1),
            FRIENDLY_HERO,
            friendly_mana,
        );
        add_player(&mut state, OPPONENT, EntityId(2), ENEMY_HERO, 10);
        state.entities.insert(
            FRIENDLY_HERO,
            hero(FRIENDLY_HERO, "HERO_FRIENDLY", FRIENDLY, 30, 0),
        );
        state.entities.insert(
            ENEMY_HERO,
            hero(
                ENEMY_HERO,
                "HERO_OPPONENT",
                OPPONENT,
                enemy_health,
                enemy_armor,
            ),
        );
        Self {
            state,
            options: Vec::new(),
            rules: TestRules::default(),
            cards: TestCards::default(),
            pools: TestPools::default(),
        }
    }

    fn add_minion(
        &mut self,
        id: i32,
        card: &str,
        controller: PlayerId,
        attack: i32,
        health: i32,
        taunt: bool,
    ) -> EntityId {
        let id = EntityId(id);
        self.state
            .entities
            .insert(id, minion(id, card, controller, attack, health, taunt));
        self.rules
            .0
            .insert(CardId::new(card), program(card, Vec::new()));
        id
    }

    fn add_spell(
        &mut self,
        id: i32,
        card: &str,
        cost: i32,
        operations: Vec<Operation>,
    ) -> EntityId {
        self.add_spell_with_evidence(id, card, cost, RuleStatus::Verified, 1.0, operations)
    }

    fn add_spell_with_evidence(
        &mut self,
        id: i32,
        card: &str,
        cost: i32,
        status: RuleStatus,
        confidence: f32,
        operations: Vec<Operation>,
    ) -> EntityId {
        let id = EntityId(id);
        self.state
            .entities
            .insert(id, spell(id, card, cost, FRIENDLY));
        self.cards.0.insert(
            (BUILD, CardId::new(card)),
            definition(card, CardType::Spell, cost, 0, 0, 0),
        );
        let mut rule = program(card, operations);
        rule.status = status;
        rule.confidence = confidence;
        self.rules.0.insert(CardId::new(card), rule);
        id
    }

    fn add_generated_definition(&mut self, card: &str) {
        self.cards.0.insert(
            (BUILD, CardId::new(card)),
            definition(card, CardType::Spell, 0, 0, 0, 0),
        );
    }

    fn add_pool(&mut self, pool: &str, cards: &[(&str, f64)]) {
        self.pools.0.insert(
            PoolId::new(pool),
            PoolSnapshot {
                pool_id: PoolId::new(pool),
                build_id: BUILD,
                context_hash: format!("hash-{pool}"),
                candidates: cards
                    .iter()
                    .map(|(card, weight)| WeightedCard {
                        card_id: CardId::new(*card),
                        dbf_id: None,
                        weight: *weight,
                    })
                    .collect(),
                trace: Vec::new(),
                exact: true,
            },
        );
    }

    fn attack_option(&mut self, index: u32, attacker: EntityId, target: EntityId) {
        self.attack_option_with_targets(
            index,
            attacker,
            &[(target, OptionAvailability::Allowed, None)],
        );
    }

    fn attack_option_with_targets(
        &mut self,
        index: u32,
        attacker: EntityId,
        targets: &[(EntityId, OptionAvailability, Option<&str>)],
    ) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "ATTACK".into();
        option.entity = Some(attacker);
        option.availability = OptionAvailability::Allowed;
        option.target_entries = targets
            .iter()
            .enumerate()
            .map(|(target_index, (target, availability, raw_error))| {
                let mut target_entry = OptionTargetSummary::default();
                target_entry.index = u32::try_from(target_index + 1).unwrap_or(u32::MAX);
                target_entry.entity = Some(*target);
                target_entry.availability = *availability;
                target_entry.raw_error = raw_error.map(str::to_owned);
                target_entry
            })
            .collect();
        self.options.push(option);
    }

    fn play_option(&mut self, index: u32, source: EntityId) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "POWER".into();
        option.entity = Some(source);
        option.availability = OptionAvailability::Allowed;
        self.options.push(option);
    }

    fn blocked_mana_play_option(&mut self, index: u32, source: EntityId) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "POWER".into();
        option.entity = Some(source);
        option.availability = OptionAvailability::Blocked;
        option.raw_error = Some("REQ_ENOUGH_MANA".into());
        self.options.push(option);
    }

    fn end_turn_option(&mut self, index: u32) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "END_TURN".into();
        option.availability = OptionAvailability::Allowed;
        self.options.push(option);
    }

    fn request(mut self, config: SearchConfig) -> PlannerRequest {
        self.state = reduce(
            &self.state,
            &DomainEvent::OptionsUpdated {
                id: 91,
                options: self.options,
            },
        )
        .expect("options update");
        let resources = PlannerResources::new(
            BUILD,
            Arc::new(self.rules),
            Arc::new(self.cards),
            Arc::new(self.pools),
        );
        prepare_request(self.state, resources, config).expect("valid planner request")
    }
}

fn add_player(
    state: &mut ObservedGameState,
    player: PlayerId,
    entity: EntityId,
    hero: EntityId,
    mana: i32,
) {
    let mut player_entity = EntityState::new(entity, None, Zone::Play);
    player_entity.controller = Some(player);
    player_entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::PLAYER);
    state.entities.insert(entity, player_entity);
    apply_tag(state, entity, TagKey::PLAYER_ID, player.0);
    apply_tag(state, entity, TagKey(game_tag::RESOURCES), 10);
    apply_tag(state, entity, TagKey(game_tag::RESOURCES_USED), 10 - mana);
    apply_tag(state, entity, TagKey(game_tag::HERO_ENTITY), hero.0);
}

fn apply_tag(state: &mut ObservedGameState, entity_id: EntityId, tag: TagKey, value: i32) {
    *state = reduce(
        state,
        &DomainEvent::TagChanged {
            entity_id,
            tag,
            value,
        },
    )
    .expect("tag update");
}

fn hero(id: EntityId, card: &str, controller: PlayerId, health: i32, armor: i32) -> EntityState {
    let mut entity = EntityState::new(id, Some(CardId::new(card)), Zone::Play);
    entity.controller = Some(controller);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::HERO);
    entity.tags.insert(game_tag::HEALTH, health);
    entity.tags.insert(game_tag::DAMAGE, 0);
    entity.tags.insert(game_tag::ATK, 0);
    entity.tags.insert(game_tag::id_by_name("ARMOR"), armor);
    entity
}

fn minion(
    id: EntityId,
    card: &str,
    controller: PlayerId,
    attack: i32,
    health: i32,
    taunt: bool,
) -> EntityState {
    let mut entity = EntityState::new(id, Some(CardId::new(card)), Zone::Play);
    entity.controller = Some(controller);
    entity.zone_position = Some(1);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
    entity.tags.insert(game_tag::HEALTH, health);
    entity.tags.insert(game_tag::DAMAGE, 0);
    entity.tags.insert(game_tag::ATK, attack);
    if taunt {
        entity.tags.insert(game_tag::id_by_name("TAUNT"), 1);
    }
    entity
}

fn spell(id: EntityId, card: &str, cost: i32, controller: PlayerId) -> EntityState {
    let mut entity = EntityState::new(id, Some(CardId::new(card)), Zone::Hand);
    entity.controller = Some(controller);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::SPELL);
    entity.tags.insert(game_tag::COST, cost);
    entity
}

fn program(card: &str, play: Vec<Operation>) -> EffectProgram {
    EffectProgram {
        schema: IR_SCHEMA_VERSION,
        card_id: CardId::new(card),
        build_range: BuildRange {
            min: BUILD,
            max: None,
        },
        status: RuleStatus::Verified,
        confidence: 1.0,
        triggers: Vec::new(),
        play,
        custom_handler: None,
        evidence: Vec::new(),
    }
}

fn deathrattle_program(card: &str, effects: Vec<Operation>) -> EffectProgram {
    let mut rule = program(card, Vec::new());
    rule.triggers.push(TriggerProgram {
        event: TriggerEvent::Deathrattle,
        when: Vec::new(),
        effects,
    });
    rule
}

fn definition(
    card: &str,
    card_type: CardType,
    cost: i32,
    attack: i32,
    health: i32,
    durability: i32,
) -> SimCardDefinition {
    SimCardDefinition {
        card_id: CardId::new(card),
        card_type,
        cost,
        attack,
        health,
        durability,
    }
}

fn damage_enemy(amount: i32) -> Operation {
    Operation::Damage {
        target: Selector::EnemyHero,
        amount,
    }
}

fn exact_random_damage(pool: &str, hit_card: &str, amount: i32) -> Vec<Operation> {
    vec![
        Operation::RandomChoice {
            pool: PoolId::new(pool),
            count: 1,
        },
        Operation::ForEach {
            over: Selector::Filter(
                Box::new(Selector::EntitiesInZone(Zone::Hand)),
                EntityCondition {
                    card_id: Some(CardId::new(hit_card)),
                    ..EntityCondition::default()
                },
            ),
            effects: vec![damage_enemy(amount)],
        },
    ]
}

#[test]
fn exhaustive_no_lethal_returns_none() {
    let mut case = Case::new(30, 0, 10);
    case.end_turn_option(1);
    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");

    assert_eq!(result.assessment(), LethalAssessment::None);
    assert!(result.complete());
    assert!(result.route().is_none());
    assert_eq!(result.stats().candidates_evaluated(), 1);
    assert_eq!(result.stats().nodes_visited(), 1);
}

#[test]
fn missing_opponent_hero_or_health_stops_before_search() {
    fn assert_missing_opponent(result: &hs_planner::LethalSearchResult) {
        assert_eq!(result.assessment(), LethalAssessment::Unknown);
        assert!(result.route().is_none());
        assert!(!result.complete());
        assert_eq!(result.stop_reasons(), [LethalStopReason::MissingOpponent]);
        assert_eq!(result.stats().candidates_evaluated(), 0);
        assert_eq!(result.stats().nodes_visited(), 0);
    }

    let mut missing_hero = Case::new(30, 0, 10);
    missing_hero.state.entities.remove(&ENEMY_HERO);
    missing_hero.end_turn_option(1);
    let result = search_lethal(&missing_hero.request(SearchConfig::default())).expect("search");
    assert_missing_opponent(&result);

    let mut missing_health = Case::new(30, 0, 10);
    missing_health
        .state
        .entities
        .get_mut(&ENEMY_HERO)
        .expect("enemy hero")
        .tags
        .remove(&game_tag::HEALTH);
    missing_health.end_turn_option(1);
    let result = search_lethal(&missing_health.request(SearchConfig::default())).expect("search");
    assert_missing_opponent(&result);
}

#[test]
fn incomplete_board_positions_prevent_false_exhaustive_none() {
    let mut case = Case::new(30, 0, 10);
    let source = EntityId(20);
    let mut hand_minion =
        EntityState::new(source, Some(CardId::new("PLAYABLE_MINION")), Zone::Hand);
    hand_minion.controller = Some(FRIENDLY);
    hand_minion
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
    hand_minion.tags.insert(game_tag::COST, 1);
    case.state.entities.insert(source, hand_minion);

    let board_minion = case.add_minion(30, "POSITION_UNKNOWN", FRIENDLY, 1, 1, false);
    case.state
        .entities
        .get_mut(&board_minion)
        .expect("board minion")
        .zone_position = None;
    case.play_option(1, source);
    case.end_turn_option(2);

    let request = case.request(SearchConfig::default());
    assert_eq!(request.legal_actions().actions.len(), 1);
    assert_eq!(request.legal_actions().actions[0].kind, ActionKind::EndTurn);

    let result = search_lethal(&request).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(!result.complete());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::MissingEvidence));
}

#[test]
fn pure_attack_uses_effective_life_including_armor() {
    let mut lethal = Case::new(1, 4, 10);
    let attacker = lethal.add_minion(10, "ATTACKER", FRIENDLY, 5, 5, false);
    lethal.attack_option(1, attacker, ENEMY_HERO);
    let result = search_lethal(&lethal.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Guaranteed);
    let evidence = result.evidence().expect("route evidence");
    assert!(evidence.exact());
    assert!(evidence.trusted_rules());
    assert!(!evidence.runtime_unknown());
    assert!(!evidence.truncated());
    assert_eq!(evidence.sample_count(), 0);
    assert_eq!(
        result.route().expect("route").first_action().source,
        Some(attacker)
    );

    let mut short = Case::new(1, 4, 10);
    let attacker = short.add_minion(10, "ATTACKER", FRIENDLY, 4, 5, false);
    short.attack_option(1, attacker, ENEMY_HERO);
    short.end_turn_option(2);
    let result = search_lethal(&short.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::None);
    assert!(result.complete());
}

#[test]
fn verified_damage_to_immune_enemy_hero_is_not_a_lethal() {
    let mut case = Case::new(3, 0, 10);
    case.state
        .entities
        .get_mut(&ENEMY_HERO)
        .expect("enemy hero")
        .tags
        .insert(game_tag::id_by_name("IMMUNE"), 1);
    let spell = case.add_spell(20, "IMMUNE_BOLT", 1, vec![damage_enemy(3)]);
    case.play_option(1, spell);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::UnsupportedSimulation));
}

#[test]
fn opponent_draw_cannot_fatigue_an_immune_hero_into_a_lethal() {
    let mut case = Case::new(1, 0, 10);
    case.state
        .entities
        .get_mut(&ENEMY_HERO)
        .expect("enemy hero")
        .tags
        .insert(game_tag::id_by_name("IMMUNE"), 1);
    let spell = case.add_spell(
        20,
        "FORCED_DRAW",
        1,
        vec![Operation::Draw {
            player: PlayerRef::Opponent,
            count: 1,
        }],
    );
    case.play_option(1, spell);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::UnsupportedSimulation));
}

#[test]
fn spell_then_attack_is_a_simulated_follow_up_and_keeps_root_identity() {
    let mut case = Case::new(6, 0, 10);
    let spell = case.add_spell(20, "BOLT", 2, vec![damage_enemy(2)]);
    let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 4, 5, false);
    case.play_option(1, spell);
    case.attack_option(2, attacker, ENEMY_HERO);
    case.end_turn_option(3);
    let request = case.request(SearchConfig::default());
    let legal_roots = request.legal_actions().actions.clone();

    let result = search_lethal(&request).expect("search");
    let route = result.route().expect("lethal route");
    assert_eq!(result.assessment(), LethalAssessment::Guaranteed);
    assert_eq!(route.action_count(), 2);
    assert!(legal_roots.contains(&route.first_action()));
    assert_eq!(route.first_action().source, Some(spell));
    assert_eq!(route.follow_up().len(), 1);
    assert_eq!(route.follow_up()[0].kind, ActionKind::Attack);
    assert_eq!(route.follow_up()[0].source, Some(attacker));
    assert_eq!(route.follow_up()[0].origin(), ActionOrigin::Simulated);
    assert!(
        !result.complete(),
        "unrecheckable alternate spell continuations do not invalidate the proven route"
    );
}

#[test]
fn multi_step_route_unions_rule_evidence_in_stable_order() {
    let mut case = Case::new(3, 0, 10);
    let spell = case.add_spell(
        20,
        "A_SWEEP",
        1,
        vec![Operation::Damage {
            target: Selector::EnemyMinions,
            amount: 1,
        }],
    );
    let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 4, 3, false);
    let taunt = case.add_minion(30, "B_DEATHRATTLE", OPPONENT, 0, 1, true);
    case.rules.0.insert(
        CardId::new("B_DEATHRATTLE"),
        deathrattle_program(
            "B_DEATHRATTLE",
            vec![Operation::GainArmor {
                player: PlayerRef::SelfPlayer,
                amount: 1,
            }],
        ),
    );
    case.play_option(1, spell);
    case.attack_option_with_targets(
        2,
        attacker,
        &[
            (taunt, OptionAvailability::Allowed, None),
            (
                ENEMY_HERO,
                OptionAvailability::Blocked,
                Some("REQ_TARGET_TAUNTER"),
            ),
        ],
    );

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(
        result.assessment(),
        LethalAssessment::Guaranteed,
        "{result:?}"
    );
    assert_eq!(result.route().expect("route").action_count(), 2);
    let evidence = result.evidence().expect("route evidence");
    assert_eq!(
        evidence
            .rule_uses()
            .iter()
            .map(|rule_use| rule_use.card_id.as_str())
            .collect::<Vec<_>>(),
        ["A_SWEEP", "B_DEATHRATTLE"]
    );
    assert!(evidence.unknown_rule_cards().is_empty());
}

#[test]
fn taunt_blocks_face_lethal_and_taunt_route_is_not_lethal() {
    let mut case = Case::new(4, 0, 10);
    let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 4, 5, false);
    let taunt = case.add_minion(30, "TAUNT", OPPONENT, 1, 1, true);
    case.attack_option(1, attacker, taunt);
    case.end_turn_option(2);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::None);
    assert!(result.complete());
}

#[test]
fn direct_damage_cannot_remove_divine_shield_taunt_and_unlock_face_attack() {
    let mut case = Case::new(4, 0, 10);
    let spell = case.add_spell(
        20,
        "SHIELD_PING",
        1,
        vec![Operation::Damage {
            target: Selector::EnemyMinions,
            amount: 1,
        }],
    );
    let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 4, 5, false);
    let taunt = case.add_minion(30, "SHIELDED_TAUNT", OPPONENT, 0, 1, true);
    case.state
        .entities
        .get_mut(&taunt)
        .expect("taunt")
        .tags
        .insert(game_tag::id_by_name("DIVINE_SHIELD"), 1);
    case.play_option(1, spell);
    case.attack_option_with_targets(
        2,
        attacker,
        &[
            (taunt, OptionAvailability::Allowed, None),
            (
                ENEMY_HERO,
                OptionAvailability::Blocked,
                Some("REQ_TARGET_TAUNTER"),
            ),
        ],
    );

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::UnsupportedSimulation));
}

#[test]
fn removing_taunt_can_unlock_only_the_server_retained_attack_target() {
    let mut case = Case::new(4, 0, 10);
    let remover = case.add_minion(10, "REMOVER", FRIENDLY, 1, 5, false);
    let finisher = case.add_minion(11, "FINISHER", FRIENDLY, 4, 5, false);
    let taunt = case.add_minion(30, "TAUNT", OPPONENT, 0, 1, true);
    let targets = [
        (taunt, OptionAvailability::Allowed, None),
        (
            ENEMY_HERO,
            OptionAvailability::Blocked,
            Some("REQ_TARGET_TAUNTER"),
        ),
    ];
    case.attack_option_with_targets(1, remover, &targets);
    case.attack_option_with_targets(2, finisher, &targets);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    let route = result.route().expect("lethal route");
    assert_eq!(result.assessment(), LethalAssessment::Guaranteed);
    assert_eq!(route.first_action().source, Some(remover));
    assert_eq!(route.first_action().server_ref.target, Some(taunt));
    assert_eq!(route.follow_up().len(), 1);
    assert_eq!(route.follow_up()[0].source, Some(finisher));
    assert_eq!(route.follow_up()[0].target, Some(ENEMY_HERO));
}

#[test]
fn mana_refresh_does_not_promote_a_blocked_spell_without_full_legality_proof() {
    let mut case = Case::new(5, 0, 0);
    let refresh = case.add_spell(
        20,
        "REFRESH",
        0,
        vec![Operation::RefreshMana {
            player: PlayerRef::SelfPlayer,
            amount: None,
        }],
    );
    let burn = case.add_spell(21, "BURN", 5, vec![damage_enemy(5)]);
    case.play_option(1, refresh);
    case.blocked_mana_play_option(2, burn);
    case.end_turn_option(3);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::MissingEvidence));
}

#[test]
fn exact_random_outcomes_aggregate_full_probability_mass() {
    let mut case = Case::new(3, 0, 10);
    let random = case.add_spell(20, "RANDOM_BOLT", 1, exact_random_damage("coin", "HIT", 3));
    case.add_generated_definition("HIT");
    case.add_generated_definition("MISS");
    case.add_pool("coin", &[("HIT", 1.0), ("MISS", 1.0)]);
    case.play_option(1, random);
    case.end_turn_option(2);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    match result.assessment() {
        LethalAssessment::Probabilistic { probability } => {
            assert!((probability.value() - 0.5).abs() < f32::EPSILON)
        }
        other => panic!("expected exact probability, got {other:?}"),
    }
    assert!(result.complete());
    assert_eq!(
        result
            .evidence()
            .expect("exact route evidence")
            .pool_snapshot_hashes(),
        ["hash-coin"]
    );
}

#[test]
fn tiny_exact_probabilities_are_neither_discarded_nor_rounded_to_guaranteed() {
    let mut tiny_hit = Case::new(3, 0, 10);
    let random = tiny_hit.add_spell(20, "TINY_HIT", 1, exact_random_damage("tiny-hit", "HIT", 3));
    tiny_hit.add_generated_definition("HIT");
    tiny_hit.add_generated_definition("MISS");
    tiny_hit.add_pool("tiny-hit", &[("HIT", 1e-12), ("MISS", 1.0)]);
    tiny_hit.play_option(1, random);
    let result = search_lethal(&tiny_hit.request(SearchConfig::default())).expect("search");
    match result.assessment() {
        LethalAssessment::Probabilistic { probability } => {
            assert!(probability.value() > 0.0);
            assert!(probability.value() < 1.0);
        }
        other => panic!("tiny positive lethal mass must remain probabilistic, got {other:?}"),
    }
    assert!(result.route().is_some());

    let mut tiny_miss = Case::new(3, 0, 10);
    let random = tiny_miss.add_spell(
        20,
        "TINY_MISS",
        1,
        exact_random_damage("tiny-miss", "HIT", 3),
    );
    tiny_miss.add_generated_definition("HIT");
    tiny_miss.add_generated_definition("MISS");
    tiny_miss.add_pool("tiny-miss", &[("HIT", 1.0), ("MISS", 5e-10)]);
    tiny_miss.play_option(1, random);
    let result = search_lethal(&tiny_miss.request(SearchConfig::default())).expect("search");
    match result.assessment() {
        LethalAssessment::Probabilistic { probability } => {
            assert!(probability.value() > 0.0);
            assert!(probability.value() < 1.0);
        }
        other => panic!("tiny nonlethal mass must prevent guaranteed, got {other:?}"),
    }
}

#[test]
fn sampled_randomness_is_only_estimated() {
    let mut case = Case::new(3, 0, 10);
    let random = case.add_spell(
        20,
        "SAMPLED_BOLT",
        1,
        exact_random_damage("large", "HIT", 3),
    );
    let mut candidates = vec![("HIT".to_string(), 1.0)];
    candidates.extend((0..19).map(|index| (format!("MISS_{index:02}"), 0.0)));
    for (card, _) in &candidates {
        case.add_generated_definition(card);
    }
    let borrowed = candidates
        .iter()
        .map(|(card, weight)| (card.as_str(), *weight))
        .collect::<Vec<_>>();
    case.add_pool("large", &borrowed);
    case.play_option(1, random);

    let result = search_lethal(&case.request(SearchConfig {
        sim_branch_budget: 4,
        ..SearchConfig::default()
    }))
    .expect("search");
    match result.assessment() {
        LethalAssessment::Estimated {
            probability,
            sample_count,
        } => {
            assert_eq!(probability.value(), 1.0);
            assert_eq!(sample_count, 4);
        }
        other => panic!("expected sampled estimate, got {other:?}"),
    }
    assert!(!result.complete());
    let evidence = result.evidence().expect("sampled route evidence");
    assert!(!evidence.exact());
    assert!(evidence.trusted_rules());
    assert!(evidence.truncated());
    assert_eq!(evidence.sample_count(), 4);
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::SampledSimulation));
}

#[test]
fn sampled_randomness_with_unverified_rules_is_unknown() {
    let mut case = Case::new(3, 0, 10);
    let random = case.add_spell_with_evidence(
        20,
        "SAMPLED_INFERRED_BOLT",
        1,
        RuleStatus::Inferred,
        0.9,
        exact_random_damage("large-unverified", "HIT", 3),
    );
    let mut candidates = vec![("HIT".to_string(), 1.0)];
    candidates.extend((0..19).map(|index| (format!("MISS_{index:02}"), 0.0)));
    for (card, _) in &candidates {
        case.add_generated_definition(card);
    }
    let borrowed = candidates
        .iter()
        .map(|(card, weight)| (card.as_str(), *weight))
        .collect::<Vec<_>>();
    case.add_pool("large-unverified", &borrowed);
    case.play_option(1, random);

    let result = search_lethal(&case.request(SearchConfig {
        sim_branch_budget: 4,
        ..SearchConfig::default()
    }))
    .expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_some(), "candidate remains explainable");
    let evidence = result.evidence().expect("sampled route evidence");
    assert!(!evidence.exact());
    assert!(!evidence.trusted_rules());
    assert_eq!(evidence.sample_count(), 4);
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::MissingEvidence));
}

#[test]
fn discover_is_a_player_choice_boundary_not_a_lucky_lethal() {
    let mut case = Case::new(3, 0, 10);
    let discover = case.add_spell(
        20,
        "DISCOVER_BOLT",
        1,
        vec![
            Operation::Discover {
                pool: PoolId::new("discover"),
                options: 2,
                count: 1,
            },
            damage_enemy(3),
        ],
    );
    case.add_generated_definition("A");
    case.add_generated_definition("B");
    case.add_pool("discover", &[("A", 1.0), ("B", 1.0)]);
    case.play_option(1, discover);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::PlayerChoice));
}

#[test]
fn unverified_rule_and_unsupported_effect_never_claim_lethal() {
    let mut unverified = Case::new(3, 0, 10);
    let spell = unverified.add_spell_with_evidence(
        20,
        "INFERRED_BOLT",
        1,
        RuleStatus::Inferred,
        0.9,
        vec![damage_enemy(3)],
    );
    unverified.play_option(1, spell);
    let result = search_lethal(&unverified.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(
        result.route().is_some(),
        "unknown evidence keeps the candidate explainable"
    );
    let evidence = result.evidence().expect("unverified route evidence");
    assert!(evidence.exact());
    assert!(!evidence.trusted_rules());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::MissingEvidence));

    let mut unsupported = Case::new(3, 0, 10);
    let spell = unsupported.add_spell(
        20,
        "UNSUPPORTED",
        1,
        vec![Operation::Silence {
            target: Selector::EnemyHero,
        }],
    );
    unsupported.play_option(1, spell);
    let result = search_lethal(&unsupported.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::UnsupportedSimulation));
}

#[test]
fn shortest_route_and_stable_option_order_win_ties() {
    let mut case = Case::new(4, 0, 10);
    let later = case.add_minion(11, "LATER", FRIENDLY, 4, 5, false);
    let first = case.add_minion(10, "FIRST", FRIENDLY, 4, 5, false);
    case.attack_option(2, later, ENEMY_HERO);
    case.attack_option(1, first, ENEMY_HERO);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    let route = result.route().expect("route");
    assert_eq!(result.assessment(), LethalAssessment::Guaranteed);
    assert_eq!(route.action_count(), 1);
    assert_eq!(route.first_action().source, Some(first));
    assert_eq!(route.first_action().server_ref.option_index, 1);
}

#[test]
fn shorter_exact_probability_route_beats_a_stronger_longer_route() {
    let mut case = Case::new(3, 0, 10);
    let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 2, 5, false);
    let short = case.add_spell(
        20,
        "SHORT_RANDOM",
        1,
        exact_random_damage("short", "SHORT_HIT", 3),
    );
    let long = case.add_spell(
        21,
        "LONG_RANDOM",
        1,
        exact_random_damage("long", "LONG_HIT", 1),
    );
    for card in ["SHORT_HIT", "SHORT_MISS", "LONG_HIT", "LONG_MISS"] {
        case.add_generated_definition(card);
    }
    case.add_pool("short", &[("SHORT_HIT", 1.0), ("SHORT_MISS", 1.0)]);
    case.add_pool("long", &[("LONG_HIT", 9.0), ("LONG_MISS", 1.0)]);
    case.play_option(1, short);
    case.play_option(2, long);
    case.attack_option(3, attacker, ENEMY_HERO);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    let route = result.route().expect("probabilistic route");
    assert_eq!(route.action_count(), 1);
    assert_eq!(route.first_action().source, Some(short));
    match result.assessment() {
        LethalAssessment::Probabilistic { probability } => {
            assert!((probability.value() - 0.5).abs() < f32::EPSILON);
        }
        other => panic!("expected shorter exact probability route, got {other:?}"),
    }
}

#[test]
fn seed_and_result_are_reproducible() {
    fn run(seed: u64) -> hs_planner::LethalSearchResult {
        let mut case = Case::new(4, 0, 10);
        let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 4, 5, false);
        case.attack_option(1, attacker, ENEMY_HERO);
        search_lethal(&case.request(SearchConfig {
            seed,
            ..SearchConfig::default()
        }))
        .expect("search")
    }

    let first = run(42);
    let second = run(42);
    assert_eq!(first, second);
    assert_eq!(
        first.route().expect("route").path_seed(),
        second.route().expect("route").path_seed()
    );
    assert_ne!(
        first.route().expect("route").path_seed(),
        run(43).route().expect("route").path_seed()
    );
}

#[test]
fn exact_branch_and_candidate_budgets_stop_at_precise_counts() {
    let mut exact = Case::new(30, 0, 10);
    let random = exact.add_spell(20, "RANDOM", 1, exact_random_damage("coin", "HIT", 1));
    exact.add_generated_definition("HIT");
    exact.add_generated_definition("MISS");
    exact.add_pool("coin", &[("HIT", 1.0), ("MISS", 1.0)]);
    exact.play_option(1, random);
    exact.end_turn_option(2);
    let result = search_lethal(&exact.request(SearchConfig {
        node_budget: 1,
        ..SearchConfig::default()
    }))
    .expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert_eq!(result.stats().roots_evaluated(), 2);
    assert_eq!(result.stats().candidates_evaluated(), 2);
    assert_eq!(result.stats().nodes_visited(), 1);
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::NodeBudget));

    let mut candidates = Case::new(30, 0, 10);
    candidates.end_turn_option(1);
    candidates.end_turn_option(2);
    let result = search_lethal(&candidates.request(SearchConfig {
        candidate_budget: 1,
        ..SearchConfig::default()
    }))
    .expect("search");
    assert_eq!(result.stats().candidates_evaluated(), 1);
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::CandidateBudget));
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
}

#[test]
fn converging_action_orders_are_not_expanded_twice() {
    let mut case = Case::new(30, 0, 10);
    for (option, entity, card) in [(1, 10, "A"), (2, 11, "B"), (3, 12, "C")] {
        let attacker = case.add_minion(entity, card, FRIENDLY, 1, 5, false);
        case.attack_option(option, attacker, ENEMY_HERO);
    }

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::None);
    assert!(result.complete());
    assert_eq!(
        result.stats().candidates_evaluated(),
        12,
        "A→B and B→A share state, remaining actions, and evidence"
    );
    assert_eq!(result.stats().nodes_visited(), 12);
}

#[test]
fn depth_budget_does_not_turn_a_partial_route_into_no_lethal() {
    let mut case = Case::new(6, 0, 10);
    let spell = case.add_spell(20, "BOLT", 1, vec![damage_enemy(2)]);
    let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 4, 5, false);
    case.play_option(1, spell);
    case.attack_option(2, attacker, ENEMY_HERO);

    let result = search_lethal(&case.request(SearchConfig {
        max_depth: 1,
        ..SearchConfig::default()
    }))
    .expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(result.route().is_none());
    assert!(result.stop_reasons().contains(&LethalStopReason::MaxDepth));
    assert_eq!(result.stats().max_depth_reached(), 1);
}

#[test]
fn unprojectable_option_coordinates_prevent_a_false_exhaustive_none() {
    let mut case = Case::new(30, 0, 10);
    let attacker = case.add_minion(10, "ATTACKER", FRIENDLY, 4, 5, false);
    let mut malformed = OptionSummary::default();
    malformed.index = 1;
    malformed.option_type = "ATTACK".into();
    malformed.entity = Some(attacker);
    malformed.availability = OptionAvailability::Blocked;
    malformed.raw_error = Some("REQ_TARGET_TO_PLAY".into());
    case.options.push(malformed);
    case.end_turn_option(2);

    let result = search_lethal(&case.request(SearchConfig::default())).expect("search");
    assert_eq!(result.assessment(), LethalAssessment::Unknown);
    assert!(!result.complete());
    assert!(result
        .stop_reasons()
        .contains(&LethalStopReason::MissingEvidence));
}
