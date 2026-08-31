use std::collections::BTreeMap;
use std::sync::Arc;

use hs_advisor_api::{Confidence, SearchConfig};
use hs_effect_ir::{EffectProgram, EntityCondition, Operation, Selector, IR_SCHEMA_VERSION};
use hs_log_core::{
    game_tag, DomainEvent, OptionAvailability, OptionSummary, OptionTargetSummary, TagKey,
};
use hs_observed_state::{reduce, EntityState, ObservedGameState};
use hs_planner::{
    prepare_request, search_beam, BeamSearchResult, BeamStopReason, PlannerResources,
    PlaybookCatalog,
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
const OPPONENT_HERO: EntityId = EntityId(5);

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
            .ok_or(SimError::NotImplemented("missing Beam budget test pool"))
    }
}

struct BudgetCase {
    state: ObservedGameState,
    options: Vec<OptionSummary>,
    rules: TestRules,
    cards: TestCards,
    pools: TestPools,
}

impl BudgetCase {
    fn new() -> Self {
        let mut state = ObservedGameState::default();
        state.build_id = Some(BUILD);
        state.game_id = Some(GameId(778));
        state.phase = GamePhase::Playing;
        state.current_player = Some(FRIENDLY);
        state.friendly_player = Some(FRIENDLY);
        state.revision = 1;
        add_player(&mut state, FRIENDLY, EntityId(1), FRIENDLY_HERO);
        add_player(&mut state, OPPONENT, EntityId(2), OPPONENT_HERO);
        state.entities.insert(
            FRIENDLY_HERO,
            hero(FRIENDLY_HERO, "HERO_FRIENDLY", FRIENDLY, 30),
        );
        state.entities.insert(
            OPPONENT_HERO,
            hero(OPPONENT_HERO, "HERO_OPPONENT", OPPONENT, 60),
        );
        Self {
            state,
            options: Vec::new(),
            rules: TestRules::default(),
            cards: TestCards::default(),
            pools: TestPools::default(),
        }
    }

    fn add_minion(&mut self, id: i32, position: u8) -> EntityId {
        let id = EntityId(id);
        let card = CardId::new(format!("BUDGET_MINION_{id:?}"));
        let mut entity = EntityState::new(id, Some(card.clone()), Zone::Play);
        entity.controller = Some(FRIENDLY);
        entity.zone_position = Some(position);
        entity
            .tags
            .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
        entity.tags.insert(game_tag::HEALTH, 5);
        entity.tags.insert(game_tag::DAMAGE, 0);
        entity.tags.insert(game_tag::ATK, 3);
        self.state.entities.insert(id, entity);
        self.rules.0.insert(card.clone(), blank_program(card));
        id
    }

    fn add_random_spell(&mut self, source: EntityId, pool: &str, cards: &[&str]) {
        let source_card = CardId::new("BUDGET_RANDOM_SPELL");
        let mut entity = EntityState::new(source, Some(source_card.clone()), Zone::Hand);
        entity.controller = Some(FRIENDLY);
        entity
            .tags
            .insert(game_tag::CARDTYPE, game_tag::card_type::SPELL);
        entity.tags.insert(game_tag::COST, 1);
        self.state.entities.insert(source, entity);
        self.cards.0.insert(
            (BUILD, source_card.clone()),
            definition(source_card.clone(), CardType::Spell, 1),
        );
        self.rules.0.insert(
            source_card.clone(),
            program(
                source_card,
                vec![Operation::RandomChoice {
                    pool: PoolId::new(pool),
                    count: 1,
                }],
            ),
        );
        for card in cards {
            let card = CardId::new(*card);
            self.cards
                .0
                .insert((BUILD, card.clone()), definition(card, CardType::Spell, 0));
        }
        self.pools.0.insert(
            PoolId::new(pool),
            PoolSnapshot {
                pool_id: PoolId::new(pool),
                build_id: BUILD,
                context_hash: format!("hash-{pool}"),
                candidates: cards
                    .iter()
                    .map(|card| WeightedCard {
                        card_id: CardId::new(*card),
                        dbf_id: None,
                        weight: 1.0,
                    })
                    .collect(),
                trace: Vec::new(),
                exact: true,
            },
        );
    }

    fn attack_option(&mut self, index: u32, attacker: EntityId) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "ATTACK".into();
        option.entity = Some(attacker);
        option.availability = OptionAvailability::Allowed;
        let mut target = OptionTargetSummary::default();
        target.index = 1;
        target.entity = Some(OPPONENT_HERO);
        target.availability = OptionAvailability::Allowed;
        option.target_entries = vec![target];
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

    fn end_turn_option(&mut self, index: u32) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "END_TURN".into();
        option.availability = OptionAvailability::Allowed;
        self.options.push(option);
    }

    fn search(mut self, config: SearchConfig) -> BeamSearchResult {
        self.state = reduce(
            &self.state,
            &DomainEvent::OptionsUpdated {
                id: 91,
                options: self.options,
            },
        )
        .expect("valid budget options");
        let resources = PlannerResources::new(
            BUILD,
            Arc::new(self.rules),
            Arc::new(self.cards),
            Arc::new(self.pools),
        );
        let request = prepare_request(self.state, resources, config).expect("valid budget request");
        let catalog = PlaybookCatalog::from_sources([(
            "generic.yaml",
            include_str!("../../../rules/playbooks/generic.yaml"),
        )])
        .expect("repository generic Playbook");
        let selected = catalog
            .select(request.identity().build_id, None)
            .expect("generic Playbook");
        search_beam(&request, &selected).expect("Beam budget search")
    }
}

fn add_player(state: &mut ObservedGameState, player: PlayerId, entity: EntityId, hero: EntityId) {
    let mut player_entity = EntityState::new(entity, None, Zone::Play);
    player_entity.controller = Some(player);
    player_entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::PLAYER);
    state.entities.insert(entity, player_entity);
    apply_tag(state, entity, TagKey::PLAYER_ID, player.0);
    apply_tag(state, entity, TagKey(game_tag::RESOURCES), 10);
    apply_tag(state, entity, TagKey(game_tag::RESOURCES_USED), 0);
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
    .expect("valid tag update");
}

fn hero(id: EntityId, card: &str, controller: PlayerId, health: i32) -> EntityState {
    let mut entity = EntityState::new(id, Some(CardId::new(card)), Zone::Play);
    entity.controller = Some(controller);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::HERO);
    entity.tags.insert(game_tag::HEALTH, health);
    entity.tags.insert(game_tag::DAMAGE, 0);
    entity.tags.insert(game_tag::ATK, 0);
    entity.tags.insert(game_tag::id_by_name("ARMOR"), 0);
    entity
}

fn blank_program(card: CardId) -> EffectProgram {
    program(card, Vec::new())
}

fn program(card_id: CardId, play: Vec<Operation>) -> EffectProgram {
    EffectProgram {
        schema: IR_SCHEMA_VERSION,
        card_id,
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

fn definition(card_id: CardId, card_type: CardType, cost: i32) -> SimCardDefinition {
    SimCardDefinition {
        card_id,
        card_type,
        cost,
        attack: 0,
        health: 0,
        durability: 0,
    }
}

fn two_attacks() -> BudgetCase {
    let mut case = BudgetCase::new();
    let first = case.add_minion(10, 1);
    let second = case.add_minion(11, 2);
    case.attack_option(1, first);
    case.attack_option(2, second);
    case
}

fn two_end_turns() -> BudgetCase {
    let mut case = BudgetCase::new();
    case.end_turn_option(1);
    case.end_turn_option(2);
    case
}

fn random_spell() -> BudgetCase {
    let mut case = BudgetCase::new();
    let source = EntityId(20);
    case.add_random_spell(
        source,
        "budget.random",
        &["RANDOM_A", "RANDOM_B", "RANDOM_C"],
    );
    case.play_option(1, source);
    case
}

fn probabilistic_two_step_lethal() -> BudgetCase {
    let mut case = BudgetCase::new();
    case.state
        .entities
        .get_mut(&OPPONENT_HERO)
        .expect("opponent hero")
        .tags
        .insert(game_tag::HEALTH, 6);
    let source = EntityId(20);
    case.add_random_spell(source, "budget.lethal", &["HIT", "MISS"]);
    case.rules.0.insert(
        CardId::new("BUDGET_RANDOM_SPELL"),
        program(
            CardId::new("BUDGET_RANDOM_SPELL"),
            vec![
                Operation::RandomChoice {
                    pool: PoolId::new("budget.lethal"),
                    count: 1,
                },
                Operation::ForEach {
                    over: Selector::Filter(
                        Box::new(Selector::EntitiesInZone(Zone::Hand)),
                        EntityCondition {
                            card_id: Some(CardId::new("HIT")),
                            ..EntityCondition::default()
                        },
                    ),
                    effects: vec![Operation::Damage {
                        target: Selector::EnemyHero,
                        amount: 3,
                    }],
                },
            ],
        ),
    );
    let attacker = case.add_minion(10, 1);
    case.play_option(1, source);
    case.attack_option(2, attacker);
    case
}

#[test]
fn max_depth_zero_spends_nothing_and_reports_the_limit() {
    let config = SearchConfig {
        max_depth: 0,
        beam_width: 4,
        node_budget: 20,
        candidate_budget: 20,
        ..SearchConfig::default()
    };
    let result = two_attacks().search(config);
    let stats = result.stats();

    assert_eq!(stats.config(), config);
    assert_eq!(stats.legal_root_total(), 2);
    assert_eq!(stats.roots_evaluated(), 0);
    assert_eq!(stats.beam_roots_evaluated(), 0);
    assert_eq!(stats.lethal_roots_evaluated(), 0);
    assert_eq!(stats.candidates_evaluated(), 0);
    assert_eq!(stats.nodes_visited(), 0);
    assert_eq!(stats.simulation_branches_examined(), 0);
    assert_eq!(stats.max_depth_reached(), 0);
    assert!(stats.layer_widths().is_empty());
    assert_eq!(result.stop_reasons(), [BeamStopReason::MaxDepth]);
    assert_eq!(result.search_completeness().total(), 2);
    assert_eq!(result.search_completeness().reliably_compared(), 0);
    assert_eq!(
        result.search_completeness().evidence_cap(),
        Confidence::ratio(3, 4)
    );
    assert_eq!(result.search_completeness().ratio(), Confidence::ZERO);
    assert!(result.candidates().is_empty());
}

#[test]
fn max_depth_one_truncates_only_the_available_follow_ups() {
    let config = SearchConfig {
        max_depth: 1,
        beam_width: 4,
        node_budget: 20,
        candidate_budget: 20,
        ..SearchConfig::default()
    };
    let result = two_attacks().search(config);
    let stats = result.stats();

    assert_eq!(stats.config().max_depth, 1);
    assert_eq!(stats.roots_evaluated(), 2);
    assert_eq!(stats.roots_reliably_compared(), 2);
    assert_eq!(stats.candidates_evaluated(), 2);
    assert_eq!(stats.nodes_visited(), 2);
    assert_eq!(stats.simulation_branches_examined(), 2);
    assert!(stats.candidates_evaluated() <= config.candidate_budget);
    assert!(stats.nodes_visited() <= config.node_budget);
    assert_eq!(stats.max_depth_reached(), 1);
    assert_eq!(stats.layer_widths(), [0]);
    assert_eq!(result.stop_reasons(), [BeamStopReason::MaxDepth]);
    assert_eq!(result.candidates().len(), 2);
    assert_eq!(
        result.search_completeness().evidence_cap(),
        Confidence::ratio(3, 4)
    );
    assert_eq!(
        result.search_completeness().ratio(),
        Confidence::ratio(3, 4)
    );
}

#[test]
fn max_depth_n_exhausts_a_two_attack_closure_without_a_false_stop() {
    let config = SearchConfig {
        max_depth: 2,
        beam_width: 4,
        node_budget: 20,
        candidate_budget: 20,
        ..SearchConfig::default()
    };
    let result = two_attacks().search(config);
    let stats = result.stats();

    assert_eq!(stats.config().max_depth, 2);
    assert_eq!(stats.roots_evaluated(), 2);
    assert_eq!(stats.roots_reliably_compared(), 2);
    assert_eq!(stats.candidates_evaluated(), 4);
    assert_eq!(stats.nodes_visited(), 4);
    assert_eq!(stats.simulation_branches_examined(), 4);
    assert!(stats.candidates_evaluated() <= config.candidate_budget);
    assert!(stats.nodes_visited() <= config.node_budget);
    assert_eq!(stats.max_depth_reached(), 2);
    assert_eq!(stats.layer_widths(), [2, 0]);
    assert_eq!(stats.deduplicated_nodes(), 1);
    assert!(result.stop_reasons().is_empty());
    assert_eq!(result.search_completeness().evidence_cap(), Confidence::ONE);
    assert_eq!(result.search_completeness().ratio(), Confidence::ONE);
}

#[test]
fn beam_width_zero_stops_before_the_general_pass_and_reports_total_prior_use() {
    let config = SearchConfig {
        max_depth: 2,
        beam_width: 0,
        node_budget: 20,
        candidate_budget: 20,
        ..SearchConfig::default()
    };
    let result = two_attacks().search(config);
    let stats = result.stats();

    assert_eq!(stats.config(), config);
    assert_eq!(stats.roots_evaluated(), 2);
    assert_eq!(stats.beam_roots_evaluated(), 0);
    assert_eq!(stats.lethal_roots_evaluated(), 2);
    assert_eq!(stats.candidates_evaluated(), 4);
    assert_eq!(stats.nodes_visited(), 4);
    assert_eq!(stats.simulation_branches_examined(), 4);
    assert_eq!(stats.max_depth_reached(), 2);
    assert!(stats.candidates_evaluated() <= config.candidate_budget);
    assert!(stats.nodes_visited() <= config.node_budget);
    assert!(stats.layer_widths().is_empty());
    assert_eq!(result.stop_reasons(), [BeamStopReason::BeamWidth]);
    assert_eq!(
        result.search_completeness().evidence_cap(),
        Confidence::ratio(3, 4)
    );
    assert_eq!(result.search_completeness().ratio(), Confidence::ZERO);
    assert!(result.candidates().is_empty());
}

#[test]
fn beam_width_one_prunes_one_route_while_width_n_keeps_both() {
    let narrow_config = SearchConfig {
        max_depth: 2,
        beam_width: 1,
        node_budget: 20,
        candidate_budget: 20,
        ..SearchConfig::default()
    };
    let narrow = two_attacks().search(narrow_config);
    let narrow_stats = narrow.stats();

    assert_eq!(narrow_stats.config().beam_width, 1);
    assert_eq!(narrow_stats.roots_evaluated(), 2);
    assert_eq!(narrow_stats.roots_reliably_compared(), 2);
    assert_eq!(narrow_stats.candidates_evaluated(), 4);
    assert_eq!(narrow_stats.nodes_visited(), 4);
    assert_eq!(narrow_stats.simulation_branches_examined(), 4);
    assert!(narrow_stats.candidates_evaluated() <= narrow_config.candidate_budget);
    assert!(narrow_stats.nodes_visited() <= narrow_config.node_budget);
    assert_eq!(narrow_stats.max_depth_reached(), 2);
    assert_eq!(narrow_stats.layer_widths(), [1, 0]);
    assert_eq!(narrow.stop_reasons(), [BeamStopReason::BeamWidth]);
    assert_eq!(narrow.candidates().len(), 1);
    assert_eq!(
        narrow.search_completeness().evidence_cap(),
        Confidence::ratio(3, 4)
    );
    assert_eq!(
        narrow.search_completeness().ratio(),
        Confidence::ratio(3, 4)
    );

    let wide_config = SearchConfig {
        beam_width: 2,
        ..narrow_config
    };
    let wide = two_attacks().search(wide_config);
    let wide_stats = wide.stats();
    assert_eq!(wide_stats.config().beam_width, 2);
    assert_eq!(wide_stats.candidates_evaluated(), 4);
    assert_eq!(wide_stats.nodes_visited(), 4);
    assert_eq!(wide_stats.simulation_branches_examined(), 4);
    assert!(wide_stats.candidates_evaluated() <= wide_config.candidate_budget);
    assert!(wide_stats.nodes_visited() <= wide_config.node_budget);
    assert_eq!(wide_stats.layer_widths(), [2, 0]);
    assert!(!wide.stop_reasons().contains(&BeamStopReason::BeamWidth));
    assert_eq!(wide.candidates().len(), 2);
    assert_eq!(wide.search_completeness().evidence_cap(), Confidence::ONE);
    assert_eq!(wide.search_completeness().ratio(), Confidence::ONE);
}

#[test]
fn node_budget_zero_one_n_separates_examined_branches_from_committed_nodes() {
    let base = SearchConfig {
        max_depth: 1,
        beam_width: 4,
        candidate_budget: 20,
        ..SearchConfig::default()
    };

    let zero_config = SearchConfig {
        node_budget: 0,
        ..base
    };
    let zero = two_end_turns().search(zero_config);
    assert_eq!(zero.stats().config().node_budget, 0);
    assert_eq!(zero.stats().roots_evaluated(), 1);
    assert_eq!(zero.stats().candidates_evaluated(), 1);
    assert_eq!(zero.stats().simulation_branches_examined(), 1);
    assert_eq!(zero.stats().nodes_visited(), 0);
    assert!(zero.stats().nodes_visited() <= zero_config.node_budget);
    assert!(zero.stats().candidates_evaluated() <= zero_config.candidate_budget);
    assert_eq!(zero.stop_reasons(), [BeamStopReason::NodeBudget]);
    assert_eq!(zero.search_completeness().reliably_compared(), 0);
    assert_eq!(
        zero.search_completeness().evidence_cap(),
        Confidence::ratio(3, 4)
    );
    assert_eq!(zero.search_completeness().ratio(), Confidence::ZERO);

    let one_config = SearchConfig {
        node_budget: 1,
        ..base
    };
    let one = two_end_turns().search(one_config);
    assert_eq!(one.stats().config().node_budget, 1);
    assert_eq!(one.stats().roots_evaluated(), 2);
    assert_eq!(one.stats().candidates_evaluated(), 2);
    assert_eq!(one.stats().simulation_branches_examined(), 2);
    assert_eq!(one.stats().nodes_visited(), 1);
    assert!(one.stats().nodes_visited() <= one_config.node_budget);
    assert!(one.stats().candidates_evaluated() <= one_config.candidate_budget);
    assert_eq!(one.stop_reasons(), [BeamStopReason::NodeBudget]);
    assert_eq!(one.search_completeness().reliably_compared(), 1);
    assert_eq!(
        one.search_completeness().evidence_cap(),
        Confidence::ratio(3, 4)
    );
    assert_eq!(one.search_completeness().ratio(), Confidence::ratio(1, 2));
    assert_eq!(one.candidates().len(), 1);

    let enough_config = SearchConfig {
        node_budget: 4,
        ..base
    };
    let enough = two_end_turns().search(enough_config);
    assert_eq!(enough.stats().config().node_budget, 4);
    assert_eq!(enough.stats().roots_evaluated(), 2);
    assert_eq!(enough.stats().candidates_evaluated(), 2);
    assert_eq!(enough.stats().simulation_branches_examined(), 2);
    assert_eq!(enough.stats().nodes_visited(), 2);
    assert!(enough.stats().nodes_visited() <= enough_config.node_budget);
    assert!(enough.stats().candidates_evaluated() <= enough_config.candidate_budget);
    assert!(!enough.stop_reasons().contains(&BeamStopReason::NodeBudget));
    assert_eq!(enough.search_completeness().reliably_compared(), 2);
    assert_eq!(enough.search_completeness().evidence_cap(), Confidence::ONE);
    assert_eq!(enough.search_completeness().ratio(), Confidence::ONE);
}

#[test]
fn simulation_branch_budget_zero_one_n_tracks_incomplete_sampled_and_exact_use() {
    let base = SearchConfig {
        max_depth: 1,
        beam_width: 4,
        node_budget: 20,
        candidate_budget: 20,
        ..SearchConfig::default()
    };

    let zero_config = SearchConfig {
        sim_branch_budget: 0,
        ..base
    };
    let zero = random_spell().search(zero_config);
    assert_eq!(zero.stats().config().sim_branch_budget, 0);
    assert_eq!(zero.stats().roots_evaluated(), 1);
    assert_eq!(zero.stats().candidates_evaluated(), 1);
    assert_eq!(zero.stats().simulation_branches_examined(), 0);
    assert_eq!(zero.stats().nodes_visited(), 0);
    assert_eq!(zero.stats().roots_reliably_compared(), 0);
    assert_eq!(
        zero.stop_reasons(),
        [BeamStopReason::IncompleteProbabilityMass]
    );
    assert_eq!(
        zero.search_completeness().evidence_cap(),
        Confidence::ratio(1, 2)
    );
    assert_eq!(zero.search_completeness().ratio(), Confidence::ZERO);

    let sampled_config = SearchConfig {
        sim_branch_budget: 1,
        ..base
    };
    let sampled = random_spell().search(sampled_config);
    assert_eq!(sampled.stats().config().sim_branch_budget, 1);
    assert_eq!(sampled.stats().roots_evaluated(), 1);
    assert_eq!(sampled.stats().candidates_evaluated(), 1);
    assert_eq!(sampled.stats().simulation_branches_examined(), 1);
    assert_eq!(sampled.stats().nodes_visited(), 1);
    assert_eq!(sampled.stats().roots_reliably_compared(), 1);
    assert_eq!(sampled.stop_reasons(), [BeamStopReason::SampledSimulation]);
    assert_eq!(sampled.candidates().len(), 1);
    assert!(!sampled.candidates()[0].evidence().exact());
    assert_eq!(sampled.candidates()[0].evidence().sample_count(), 1);
    assert_eq!(
        sampled.search_completeness().evidence_cap(),
        Confidence::ratio(7, 10)
    );
    assert_eq!(
        sampled.search_completeness().ratio(),
        Confidence::ratio(7, 10)
    );

    let exact_config = SearchConfig {
        sim_branch_budget: 3,
        ..base
    };
    let exact = random_spell().search(exact_config);
    assert_eq!(exact.stats().config().sim_branch_budget, 3);
    assert_eq!(exact.stats().roots_evaluated(), 1);
    assert_eq!(exact.stats().candidates_evaluated(), 1);
    assert_eq!(exact.stats().simulation_branches_examined(), 3);
    assert_eq!(exact.stats().nodes_visited(), 3);
    assert_eq!(exact.stats().roots_reliably_compared(), 1);
    assert!(!exact
        .stop_reasons()
        .contains(&BeamStopReason::SampledSimulation));
    assert_eq!(exact.candidates().len(), 1);
    assert!(exact.candidates()[0].evidence().exact());
    assert_eq!(exact.candidates()[0].evidence().sample_count(), 0);
    assert_eq!(exact.search_completeness().evidence_cap(), Confidence::ONE);
    assert_eq!(exact.search_completeness().ratio(), Confidence::ONE);
}

#[test]
fn oversized_exact_chance_group_is_rejected_but_a_later_cheap_root_still_runs() {
    let mut case = random_spell();
    case.end_turn_option(2);
    let config = SearchConfig {
        max_depth: 1,
        beam_width: 4,
        node_budget: 2,
        candidate_budget: 20,
        sim_branch_budget: 3,
        ..SearchConfig::default()
    };

    let result = case.search(config);
    let stats = result.stats();

    assert_eq!(stats.legal_root_total(), 2);
    assert_eq!(stats.roots_evaluated(), 2);
    assert_eq!(stats.lethal_roots_evaluated(), 2);
    assert_eq!(stats.beam_roots_evaluated(), 0);
    assert_eq!(stats.candidates_evaluated(), 2);
    assert_eq!(stats.simulation_branches_examined(), 4);
    assert_eq!(stats.nodes_visited(), 1);
    assert!(stats.nodes_visited() <= config.node_budget);
    assert_eq!(result.stop_reasons(), [BeamStopReason::NodeBudget]);
    assert_eq!(result.search_completeness().reliably_compared(), 1);
    assert_eq!(result.candidates().len(), 1);
    assert_eq!(
        result.candidates()[0].line().first_action.kind,
        hs_advisor_api::ActionKind::EndTurn
    );
}

#[test]
fn paid_depth_two_probabilistic_lethal_is_replayed_without_double_charging() {
    let config = SearchConfig {
        max_depth: 2,
        beam_width: 8,
        node_budget: 5,
        candidate_budget: 3,
        sim_branch_budget: 2,
        ..SearchConfig::default()
    };

    let result = probabilistic_two_step_lethal().search(config);
    let stats = result.stats();

    assert_eq!(stats.candidates_evaluated(), 3);
    assert_eq!(stats.lethal_candidates_evaluated(), 3);
    assert_eq!(stats.nodes_visited(), 5);
    assert_eq!(stats.lethal_nodes_visited(), 5);
    let route = result
        .candidates()
        .iter()
        .find(|candidate| !candidate.line().follow_up.is_empty())
        .expect("paid depth-two route remains publishable");
    assert_eq!(
        route.line().follow_up[0].kind,
        hs_advisor_api::ActionKind::Attack
    );
    assert!(matches!(
        route.line().lethal,
        hs_advisor_api::LethalAssessment::Probabilistic { probability }
            if probability == Confidence::ratio(1, 2)
    ));
}

#[test]
fn oversized_deep_group_does_not_hide_a_later_affordable_continuation() {
    let mut case = random_spell();
    let attacker = case.add_minion(10, 1);
    case.attack_option(2, attacker);
    case.end_turn_option(3);
    let config = SearchConfig {
        max_depth: 2,
        beam_width: 8,
        node_budget: 6,
        candidate_budget: 20,
        sim_branch_budget: 3,
        ..SearchConfig::default()
    };

    let result = case.search(config);
    let stats = result.stats();

    assert_eq!(stats.nodes_visited(), 6);
    assert!(stats.nodes_visited() <= config.node_budget);
    assert!(result.stop_reasons().contains(&BeamStopReason::NodeBudget));
    let route = result
        .candidates()
        .iter()
        .find(|candidate| {
            candidate.line().first_action.kind == hs_advisor_api::ActionKind::Attack
                && candidate.line().follow_up.len() == 1
                && candidate.line().follow_up[0].kind == hs_advisor_api::ActionKind::EndTurn
        })
        .expect("later one-node continuation still fits and remains publishable");
    assert_eq!(route.path().len(), 2);
}
