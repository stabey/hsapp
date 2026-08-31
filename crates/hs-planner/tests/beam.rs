mod common;

use std::collections::BTreeSet;

use hs_advisor_api::{ActionKind, Confidence, SearchConfig, ServerActionRef, UnavailableReason};
use hs_log_core::{
    game_tag, DomainEvent, OptionAvailability, OptionSummary, OptionTargetSummary, TagKey,
};
use hs_observed_state::{reduce, EntityState, ObservedGameState};
use hs_planner::{
    prepare_request, search_beam, BeamEngine, BeamSearchResult, BeamStopReason, Engine,
    PlannerError, PlannerRequest, PlaybookCatalog,
};
use hs_types::{BuildId, CardId, EntityId, GameId, GamePhase, PlayerId, Zone};

use common::{resources, BUILD, FRIENDLY};

const OPPONENT: PlayerId = PlayerId(2);
const FRIENDLY_HERO: EntityId = EntityId(4);
const OPPONENT_HERO: EntityId = EntityId(5);
const QUEST_HUNTER_BUILD: BuildId = BuildId(249_896);

struct BeamCase {
    state: ObservedGameState,
    options: Vec<OptionSummary>,
}

impl BeamCase {
    fn new(opponent_health: i32) -> Self {
        let mut state = ObservedGameState::default();
        state.build_id = Some(BUILD);
        state.game_id = Some(GameId(777));
        state.phase = GamePhase::Playing;
        state.current_player = Some(FRIENDLY);
        state.friendly_player = Some(FRIENDLY);
        state.revision = 1;
        add_player(&mut state, FRIENDLY, EntityId(1), FRIENDLY_HERO, 10);
        add_player(&mut state, OPPONENT, EntityId(2), OPPONENT_HERO, 10);
        state.entities.insert(
            FRIENDLY_HERO,
            hero(FRIENDLY_HERO, "HERO_FRIENDLY", FRIENDLY, 30),
        );
        state.entities.insert(
            OPPONENT_HERO,
            hero(OPPONENT_HERO, "HERO_OPPONENT", OPPONENT, opponent_health),
        );
        Self {
            state,
            options: Vec::new(),
        }
    }

    fn add_minion(&mut self, id: i32, attack: i32, position: u8) -> EntityId {
        let id = EntityId(id);
        let mut entity =
            EntityState::new(id, Some(CardId::new(format!("MINION_{id:?}"))), Zone::Play);
        entity.controller = Some(FRIENDLY);
        entity.zone_position = Some(position);
        entity
            .tags
            .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
        entity.tags.insert(game_tag::HEALTH, 5);
        entity.tags.insert(game_tag::DAMAGE, 0);
        entity.tags.insert(game_tag::ATK, attack);
        self.state.entities.insert(id, entity);
        id
    }

    fn add_unknown_spell(&mut self, id: i32, cost: i32) -> EntityId {
        let id = EntityId(id);
        let mut entity = EntityState::new(id, Some(CardId::new("UNKNOWN_SPELL")), Zone::Hand);
        entity.controller = Some(FRIENDLY);
        entity
            .tags
            .insert(game_tag::CARDTYPE, game_tag::card_type::SPELL);
        entity.tags.insert(game_tag::COST, cost);
        self.state.entities.insert(id, entity);
        id
    }

    fn attack_option(&mut self, index: u32, attacker: EntityId) {
        self.attack_option_targets(index, attacker, [OPPONENT_HERO]);
    }

    fn attack_option_targets(
        &mut self,
        index: u32,
        attacker: EntityId,
        defenders: impl IntoIterator<Item = EntityId>,
    ) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "ATTACK".into();
        option.entity = Some(attacker);
        option.availability = OptionAvailability::Allowed;
        option.target_entries = defenders
            .into_iter()
            .zip(1_u32..)
            .map(|(defender, target_index)| {
                let mut target = OptionTargetSummary::default();
                target.index = target_index;
                target.entity = Some(defender);
                target.availability = OptionAvailability::Allowed;
                target
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

    fn end_turn_option(&mut self, index: u32) {
        let mut option = OptionSummary::default();
        option.index = index;
        option.option_type = "END_TURN".into();
        option.availability = OptionAvailability::Allowed;
        self.options.push(option);
    }

    fn request(mut self, config: SearchConfig, reverse_options: bool) -> PlannerRequest {
        if reverse_options {
            self.options.reverse();
        }
        self.state = reduce(
            &self.state,
            &DomainEvent::OptionsUpdated {
                id: 91,
                options: self.options,
            },
        )
        .expect("valid Beam options");
        let build = self.state.build_id.expect("Beam fixture has a Build");
        prepare_request(self.state, resources(build), config).expect("valid Beam request")
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
    .expect("valid tag update");
}

fn hero(id: EntityId, card_id: &str, controller: PlayerId, health: i32) -> EntityState {
    let mut entity = EntityState::new(id, Some(CardId::new(card_id)), Zone::Play);
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

fn repo_catalog() -> PlaybookCatalog {
    PlaybookCatalog::from_sources([
        (
            "generic.yaml",
            include_str!("../../../rules/playbooks/generic.yaml"),
        ),
        (
            "standard.quest_hunter.249896.yaml",
            include_str!("../../../rules/playbooks/standard.quest_hunter.249896.yaml"),
        ),
    ])
    .expect("repository Playbook catalog")
}

fn isolated_health_catalog(opponent_health_weight: i32) -> PlaybookCatalog {
    let yaml = format!(
        r#"
schema_version: 1
id: generic
priority: 0
applies_to:
  build_range:
    min: 0
  deck_signatures: []
weights:
  friendly_effective_health: 0
  opponent_effective_health: {opponent_health_weight}
  friendly_board_attack: 0
  opponent_board_attack: 0
  friendly_board_health: 0
  opponent_board_health: 0
  friendly_board_value: 0
  opponent_board_value: 0
  friendly_hand_size: 0
  opponent_hand_size: 0
  resource_advantage: 0
  mana_efficiency: 0
  playbook_preference: 0
  risk_penalty: 0
risk_weight_milli: 0
win_points: []
explanations: {{}}
"#
    );
    PlaybookCatalog::from_sources([("generic.yaml", yaml)]).expect("isolated score Playbook")
}

fn selected_search(request: &PlannerRequest, catalog: &PlaybookCatalog) -> BeamSearchResult {
    let selected = catalog
        .select(request.identity().build_id, None)
        .expect("generic Playbook");
    search_beam(request, &selected).expect("Beam search")
}

fn two_attack_request(reverse_options: bool) -> PlannerRequest {
    let mut case = BeamCase::new(30);
    let first = case.add_minion(10, 3, 1);
    let second = case.add_minion(11, 3, 2);
    case.attack_option(2, first);
    case.attack_option(1, second);
    case.request(
        SearchConfig {
            seed: 42,
            max_depth: 1,
            beam_width: 8,
            ..SearchConfig::default()
        },
        reverse_options,
    )
}

#[test]
fn candidate_order_is_stable_and_independent_of_options_input_order() {
    let catalog = repo_catalog();
    let normal_request = two_attack_request(false);
    let reversed_request = two_attack_request(true);
    assert_eq!(
        normal_request.legal_actions().actions,
        reversed_request.legal_actions().actions
    );

    let normal = selected_search(&normal_request, &catalog);
    let reversed = selected_search(&reversed_request, &catalog);

    assert_eq!(
        serde_json::to_vec(&normal).expect("stable Beam JSON"),
        serde_json::to_vec(&reversed).expect("stable reversed Beam JSON")
    );
    assert_eq!(normal.candidates().len(), 2);
    assert_eq!(
        normal.candidates()[0]
            .line()
            .first_action
            .server_ref
            .option_index,
        1,
        "equal scores use canonical server coordinates"
    );
    assert_eq!(
        normal.candidates()[1]
            .line()
            .first_action
            .server_ref
            .option_index,
        2
    );
}

#[test]
fn candidate_budget_truncation_lowers_search_completeness() {
    fn request(candidate_budget: u64) -> PlannerRequest {
        let mut case = BeamCase::new(30);
        case.end_turn_option(1);
        case.end_turn_option(2);
        case.request(
            SearchConfig {
                candidate_budget,
                max_depth: 1,
                beam_width: 4,
                ..SearchConfig::default()
            },
            false,
        )
    }

    let catalog = repo_catalog();
    // Lethal-first root expansions seed Beam without paying twice.
    let complete = selected_search(&request(2), &catalog);
    let truncated = selected_search(&request(1), &catalog);

    assert_eq!(complete.search_completeness().ratio(), Confidence::ONE);
    assert_eq!(complete.stats().roots_reliably_compared(), 2);
    assert_eq!(truncated.stats().candidates_evaluated(), 1);
    assert_eq!(truncated.stats().roots_evaluated(), 1);
    assert_eq!(truncated.stats().beam_roots_evaluated(), 0);
    assert_eq!(truncated.stats().lethal_roots_evaluated(), 1);
    assert_eq!(truncated.stats().roots_reliably_compared(), 1);
    assert_eq!(truncated.search_completeness().total(), 2);
    assert!(truncated.search_completeness().ratio() < complete.search_completeness().ratio());
    assert!(truncated
        .stop_reasons()
        .contains(&BeamStopReason::CandidateBudget));
}

#[test]
fn unsupported_only_roots_return_no_candidates_or_fallback_end_turn() {
    fn request() -> PlannerRequest {
        let mut case = BeamCase::new(30);
        let spell = case.add_unknown_spell(20, 1);
        case.play_option(1, spell);
        case.request(SearchConfig::default(), false)
    }

    let catalog = repo_catalog();
    let search = selected_search(&request(), &catalog);
    assert!(search.candidates().is_empty());
    assert_eq!(search.stats().legal_root_total(), 1);
    assert_eq!(search.stats().roots_reliably_compared(), 0);
    assert_eq!(search.search_completeness().ratio(), Confidence::ZERO);
    assert!(search.stop_reasons().iter().any(|reason| matches!(
        reason,
        BeamStopReason::UnsupportedSimulation | BeamStopReason::MissingEvidence
    )));

    let engine = BeamEngine::new(catalog, None);
    assert_eq!(
        engine.plan(request()),
        Err(PlannerError::Unavailable(
            UnavailableReason::NoReliableActions
        ))
    );
}

#[test]
fn beam_engine_produces_a_planner_result_bound_to_the_prepared_root() {
    let mut case = BeamCase::new(30);
    case.end_turn_option(7);
    let request = case.request(SearchConfig::default(), false);
    let identity = request.identity().clone();
    let legal_root = request.legal_actions().actions[0];
    let engine = BeamEngine::new(repo_catalog(), None);

    let result = engine.plan(request).expect("contract-valid PlannerResult");

    assert_eq!(result.identity(), &identity);
    assert_eq!(result.payload().lines.len(), 1);
    assert_eq!(result.payload().lines[0].first_action, legal_root);
    assert_eq!(result.payload().search_completeness.total(), 1);
    assert_eq!(result.payload().search_completeness.reliably_compared(), 1);
    assert_eq!(
        result.payload().search_completeness.ratio(),
        Confidence::ONE
    );
    assert!(
        result.payload().confidence.confidence()
            <= result.payload().lines[0].confidence.confidence()
    );
    assert!(
        result.payload().confidence.confidence() <= result.payload().search_completeness.ratio()
    );
}

#[test]
fn playbook_weights_change_ranking_without_changing_roots_or_evidence() {
    let mut case = BeamCase::new(30);
    let attacker = case.add_minion(10, 3, 1);
    case.attack_option(1, attacker);
    case.end_turn_option(2);
    let request = case.request(
        SearchConfig {
            max_depth: 1,
            beam_width: 4,
            ..SearchConfig::default()
        },
        false,
    );
    let legal_roots = request
        .legal_actions()
        .actions
        .iter()
        .map(|action| action.server_ref)
        .collect::<BTreeSet<_>>();
    let pressure = selected_search(&request, &isolated_health_catalog(-1_000));
    let preservation = selected_search(&request, &isolated_health_catalog(1_000));

    assert_eq!(
        pressure.candidates()[0].line().first_action.kind,
        ActionKind::Attack
    );
    assert_eq!(
        preservation.candidates()[0].line().first_action.kind,
        ActionKind::EndTurn
    );

    fn roots(result: &BeamSearchResult) -> BTreeSet<ServerActionRef> {
        result
            .candidates()
            .iter()
            .map(|candidate| candidate.line().first_action.server_ref)
            .collect()
    }
    fn evidence(result: &BeamSearchResult) -> Vec<(ServerActionRef, String)> {
        let mut evidence = result
            .candidates()
            .iter()
            .map(|candidate| {
                (
                    candidate.line().first_action.server_ref,
                    serde_json::to_string(candidate.evidence()).expect("stable evidence"),
                )
            })
            .collect::<Vec<_>>();
        evidence.sort_by_key(|(server_ref, _)| *server_ref);
        evidence
    }

    assert_eq!(roots(&pressure), legal_roots);
    assert_eq!(roots(&preservation), legal_roots);
    assert_eq!(evidence(&pressure), evidence(&preservation));
    assert_eq!(pressure.rule_coverage(), preservation.rule_coverage());
    assert_eq!(request.legal_actions().actions.len(), 2);
}

#[test]
fn repository_quest_hunter_changes_order_without_changing_legal_roots() {
    let mut case = BeamCase::new(30);
    case.state.build_id = Some(QUEST_HUNTER_BUILD);
    let attacker = case.add_minion(10, 3, 1);
    let defender = case.add_minion(20, 1, 1);
    case.state
        .entities
        .get_mut(&defender)
        .expect("opponent minion fixture")
        .controller = Some(OPPONENT);
    // The same 3/5 attacker can either deal three face damage or take one
    // damage while reducing an opposing 1/5 to 1/2. The checked-in weight
    // profiles deliberately value face pressure and board damage differently,
    // without changing either server-backed root.
    case.attack_option_targets(1, attacker, [defender, OPPONENT_HERO]);
    let request = case.request(
        SearchConfig {
            max_depth: 1,
            beam_width: 4,
            ..SearchConfig::default()
        },
        false,
    );
    let legal_roots = request
        .legal_actions()
        .actions
        .iter()
        .map(|action| action.server_ref)
        .collect::<BTreeSet<_>>();

    let catalog = repo_catalog();
    let quest_signature = catalog
        .playbooks()
        .iter()
        .find(|playbook| playbook.id == "standard.quest_hunter.249896")
        .and_then(|playbook| playbook.applies_to.deck_signatures.first())
        .cloned()
        .expect("repository quest hunter signature");
    let generic = catalog
        .select(QUEST_HUNTER_BUILD, None)
        .expect("repository generic Playbook");
    let quest_hunter = catalog
        .select(QUEST_HUNTER_BUILD, Some(&quest_signature))
        .expect("repository quest hunter Playbook");
    assert_eq!(generic.id(), "generic");
    assert_eq!(quest_hunter.id(), "standard.quest_hunter.249896");

    let generic_search = search_beam(&request, &generic).expect("generic Beam search");
    let quest_search = search_beam(&request, &quest_hunter).expect("quest hunter Beam search");
    let candidate_roots = |result: &BeamSearchResult| {
        result
            .candidates()
            .iter()
            .map(|candidate| candidate.line().first_action.server_ref)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(candidate_roots(&generic_search), legal_roots);
    assert_eq!(candidate_roots(&quest_search), legal_roots);

    assert_eq!(
        generic_search.candidates()[0]
            .line()
            .first_action
            .server_ref
            .target,
        Some(defender),
        "generic values the favorable board contest above three face damage"
    );
    assert_eq!(
        quest_search.candidates()[0]
            .line()
            .first_action
            .server_ref
            .target,
        Some(OPPONENT_HERO),
        "quest hunter values three face damage above the favorable board contest"
    );
    assert_ne!(
        generic_search
            .candidates()
            .iter()
            .map(|candidate| candidate.line().first_action.server_ref)
            .collect::<Vec<_>>(),
        quest_search
            .candidates()
            .iter()
            .map(|candidate| candidate.line().first_action.server_ref)
            .collect::<Vec<_>>(),
        "the checked-in Playbooks must produce an observable candidate-order difference"
    );
}

#[test]
fn guaranteed_lethal_is_reused_when_it_spends_the_entire_shared_budget() {
    fn request() -> PlannerRequest {
        let mut case = BeamCase::new(3);
        let attacker = case.add_minion(10, 3, 1);
        case.attack_option(1, attacker);
        case.request(
            SearchConfig {
                max_depth: 1,
                beam_width: 4,
                node_budget: 1,
                candidate_budget: 1,
                ..SearchConfig::default()
            },
            false,
        )
    }

    let catalog = repo_catalog();
    let result = selected_search(&request(), &catalog);
    assert_eq!(result.stats().candidates_evaluated(), 1);
    assert_eq!(result.stats().nodes_visited(), 1);
    assert_eq!(result.stats().lethal_roots_evaluated(), 1);
    assert_eq!(result.stats().beam_roots_evaluated(), 0);
    assert_eq!(result.stats().roots_reliably_compared(), 1);
    assert_eq!(result.candidates().len(), 1);
    assert!(result.candidates()[0].lethal_first_route());
    assert_eq!(
        result.candidates()[0].line().lethal,
        hs_advisor_api::LethalAssessment::Guaranteed
    );

    let planned = BeamEngine::new(catalog, None)
        .plan(request())
        .expect("proven lethal remains publishable");
    assert_eq!(planned.payload().lines.len(), 1);
    assert_eq!(
        planned.payload().lines[0].lethal,
        hs_advisor_api::LethalAssessment::Guaranteed
    );
    assert!(planned.payload().lines[0].chance.available());
    assert_eq!(
        planned.payload().lines[0].chance.rank_milli(),
        result.candidates()[0].chance_score().rank_milli()
    );
}

#[test]
fn guaranteed_lethal_survives_a_zero_general_beam_width() {
    let mut case = BeamCase::new(3);
    let attacker = case.add_minion(10, 3, 1);
    case.attack_option(1, attacker);
    let request = case.request(
        SearchConfig {
            max_depth: 1,
            beam_width: 0,
            node_budget: 1,
            candidate_budget: 1,
            ..SearchConfig::default()
        },
        false,
    );

    let result = selected_search(&request, &repo_catalog());
    assert_eq!(result.candidates().len(), 1);
    assert!(result.candidates()[0].lethal_first_route());
    assert!(result.stop_reasons().contains(&BeamStopReason::BeamWidth));
    assert_eq!(result.stats().beam_roots_evaluated(), 0);
}

#[test]
fn lethal_pass_unknown_evidence_survives_when_the_general_budget_is_spent() {
    let mut case = BeamCase::new(3);
    let attacker = case.add_minion(10, 3, 1);
    let unknown_spell = case.add_unknown_spell(20, 1);
    case.attack_option(1, attacker);
    case.play_option(2, unknown_spell);
    let request = case.request(
        SearchConfig {
            max_depth: 1,
            beam_width: 4,
            node_budget: 2,
            candidate_budget: 2,
            ..SearchConfig::default()
        },
        false,
    );

    let result = selected_search(&request, &repo_catalog());
    assert_eq!(result.candidates().len(), 1);
    assert_eq!(
        result.candidates()[0].line().lethal,
        hs_advisor_api::LethalAssessment::Guaranteed
    );
    assert!(result.stop_reasons().iter().any(|reason| matches!(
        reason,
        BeamStopReason::MissingEvidence | BeamStopReason::UnsupportedSimulation
    )));
    assert_eq!(
        result.search_completeness().evidence_cap(),
        Confidence::ratio(1, 2)
    );
    let missing = result
        .rule_coverage()
        .items()
        .iter()
        .find(|item| item.card_id == CardId::new("UNKNOWN_SPELL"))
        .expect("unknown root remains in aggregate coverage");
    assert_eq!(missing.status, None);
    assert_eq!(missing.confidence, Confidence::ZERO);
}
