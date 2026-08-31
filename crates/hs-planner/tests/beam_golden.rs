//! Stable Beam output projections.
//!
//! Normal test runs are read-only. To intentionally refresh every fixture:
//! `UPDATE_GOLDEN=1 cargo test -p hs-planner --test beam_golden`

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use hs_advisor_api::{Confidence, LethalAssessment, SearchConfig};
use hs_effect_ir::{EffectProgram, EntityCondition, Operation, Selector, IR_SCHEMA_VERSION};
use hs_log_core::{
    game_tag, DomainEvent, OptionAvailability, OptionSummary, OptionTargetSummary, TagKey,
};
use hs_observed_state::{reduce, EntityState, ObservedGameState};
use hs_planner::{
    prepare_request, search_beam, BeamSearchResult, BeamStopReason, DeckSignature,
    PlannerResources, Playbook, PlaybookCatalog, CANONICAL_PATH_SEED_VERSION,
    SEARCH_COMPLETENESS_POLICY_VERSION,
};
use hs_pools::{PoolSnapshot, WeightedCard};
use hs_sim::{CardLookup, PoolResolver, RuleLookup, SimCardDefinition, SimError};
use hs_types::{
    BuildId, BuildRange, CardId, CardType, EntityId, GameId, GamePhase, PlayerId, PoolId,
    RuleStatus, Zone,
};
use serde_json::{json, Value};

const BUILD: BuildId = BuildId(249_896);
const FRIENDLY: PlayerId = PlayerId(1);
const OPPONENT: PlayerId = PlayerId(2);
const FRIENDLY_HERO: EntityId = EntityId(4);
const OPPONENT_HERO: EntityId = EntityId(5);

#[derive(Default)]
struct GoldenRules(BTreeMap<CardId, EffectProgram>);

impl RuleLookup for GoldenRules {
    fn program_for(&self, card_id: &CardId) -> Option<&EffectProgram> {
        self.0.get(card_id)
    }
}

#[derive(Default)]
struct GoldenCards(BTreeMap<(BuildId, CardId), SimCardDefinition>);

impl CardLookup for GoldenCards {
    fn sim_card(&self, build: BuildId, card_id: &CardId) -> Option<SimCardDefinition> {
        self.0.get(&(build, card_id.clone())).cloned()
    }
}

#[derive(Default)]
struct GoldenPools(BTreeMap<PoolId, PoolSnapshot>);

impl PoolResolver for GoldenPools {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        self.0
            .get(pool_id)
            .cloned()
            .ok_or(SimError::NotImplemented("missing Beam golden pool"))
    }
}

struct GoldenCase {
    state: ObservedGameState,
    options: Vec<OptionSummary>,
    rules: GoldenRules,
    cards: GoldenCards,
    pools: GoldenPools,
}

struct GoldenSearch {
    result: BeamSearchResult,
    playbook_id: String,
}

impl GoldenCase {
    fn new(opponent_health: i32) -> Self {
        let mut state = ObservedGameState::default();
        state.build_id = Some(BUILD);
        state.game_id = Some(GameId(9_001));
        state.phase = GamePhase::Playing;
        state.current_player = Some(FRIENDLY);
        state.friendly_player = Some(FRIENDLY);
        state.revision = 17;
        add_player(&mut state, FRIENDLY, EntityId(1), FRIENDLY_HERO);
        add_player(&mut state, OPPONENT, EntityId(2), OPPONENT_HERO);
        state.entities.insert(
            FRIENDLY_HERO,
            hero(FRIENDLY_HERO, "GOLDEN_FRIENDLY_HERO", FRIENDLY, 30),
        );
        state.entities.insert(
            OPPONENT_HERO,
            hero(
                OPPONENT_HERO,
                "GOLDEN_OPPONENT_HERO",
                OPPONENT,
                opponent_health,
            ),
        );
        Self {
            state,
            options: Vec::new(),
            rules: GoldenRules::default(),
            cards: GoldenCards::default(),
            pools: GoldenPools::default(),
        }
    }

    fn add_minion(&mut self, id: i32, attack: i32, position: u8) -> EntityId {
        let entity_id = EntityId(id);
        let card_id = CardId::new(format!("GOLDEN_MINION_{id}"));
        let mut entity = EntityState::new(entity_id, Some(card_id.clone()), Zone::Play);
        entity.controller = Some(FRIENDLY);
        entity.zone_position = Some(position);
        entity
            .tags
            .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
        entity.tags.insert(game_tag::HEALTH, 5);
        entity.tags.insert(game_tag::DAMAGE, 0);
        entity.tags.insert(game_tag::ATK, attack);
        self.state.entities.insert(entity_id, entity);
        self.rules
            .0
            .insert(card_id.clone(), program(card_id, Vec::new()));
        entity_id
    }

    fn add_random_damage_spell(
        &mut self,
        source: EntityId,
        source_card: &str,
        pool: &str,
        candidates: &[(&str, f64)],
        hit_card: &str,
        damage: i32,
    ) {
        let source_card = CardId::new(source_card);
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
                        effects: vec![Operation::Damage {
                            target: Selector::EnemyHero,
                            amount: damage,
                        }],
                    },
                ],
            ),
        );
        for (card, _) in candidates {
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
                context_hash: format!("golden-hash-{pool}"),
                candidates: candidates
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

    fn add_unknown_spell(&mut self, source: EntityId) {
        let mut entity = EntityState::new(
            source,
            Some(CardId::new("GOLDEN_UNKNOWN_SPELL")),
            Zone::Hand,
        );
        entity.controller = Some(FRIENDLY);
        entity
            .tags
            .insert(game_tag::CARDTYPE, game_tag::card_type::SPELL);
        entity.tags.insert(game_tag::COST, 1);
        self.state.entities.insert(source, entity);
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

    fn search(
        mut self,
        config: SearchConfig,
        deck_signature: Option<&DeckSignature>,
    ) -> GoldenSearch {
        self.state = reduce(
            &self.state,
            &DomainEvent::OptionsUpdated {
                id: 901,
                options: self.options,
            },
        )
        .expect("valid golden Options");
        let resources = PlannerResources::new(
            BUILD,
            Arc::new(self.rules),
            Arc::new(self.cards),
            Arc::new(self.pools),
        );
        let request = prepare_request(self.state, resources, config).expect("valid golden request");
        let catalog = repository_catalog();
        let selected = catalog
            .select(BUILD, deck_signature)
            .expect("golden Playbook selection");
        let playbook_id = selected.id().to_owned();
        let result = search_beam(&request, &selected).expect("golden Beam search");
        GoldenSearch {
            result,
            playbook_id,
        }
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

fn program(card_id: CardId, play: Vec<Operation>) -> EffectProgram {
    EffectProgram {
        schema: IR_SCHEMA_VERSION,
        card_id,
        build_range: BuildRange {
            min: BUILD,
            max: Some(BUILD),
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

fn repository_catalog() -> PlaybookCatalog {
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

fn quest_hunter_signature() -> DeckSignature {
    Playbook::from_yaml(include_str!(
        "../../../rules/playbooks/standard.quest_hunter.249896.yaml"
    ))
    .expect("repository quest hunter Playbook")
    .applies_to
    .deck_signatures
    .into_iter()
    .next()
    .expect("quest hunter exact deck signature")
}

fn two_attack_case() -> GoldenCase {
    let mut case = GoldenCase::new(30);
    let first = case.add_minion(10, 3, 1);
    let second = case.add_minion(11, 4, 2);
    case.attack_option(1, first);
    case.attack_option(2, second);
    case
}

fn stable_projection(search: &GoldenSearch) -> Value {
    let result = &search.result;
    let completeness = result.search_completeness();
    let candidates = result
        .candidates()
        .iter()
        .map(|candidate| {
            let line = candidate.line();
            let total_milli =
                i64::try_from(line.score.total_milli()).expect("golden score total fits i64");
            json!({
                "path": candidate.path(),
                "path_seed": candidate.path_seed(),
                "lethal_first_route": candidate.lethal_first_route(),
                "first_action": line.first_action,
                "follow_up": &line.follow_up,
                "score": {
                    "components": line.score.components(),
                    "total_milli": total_milli,
                },
                "chance": line.chance,
                "rank_chance": candidate.chance_score(),
                "lethal": line.lethal,
                "confidence": &line.confidence,
                "complete": line.complete,
                "win_point": &line.win_point,
                "explanation": &line.explanation,
                "risks": &line.risks,
                "evidence": candidate.evidence(),
                "rule_coverage": candidate.rule_coverage(),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "build_id": BUILD,
        "playbook_id": search.playbook_id,
        "canonical_path_seed_version": CANONICAL_PATH_SEED_VERSION,
        "search_completeness_policy_version": SEARCH_COMPLETENESS_POLICY_VERSION,
        "playbook_selection": result.playbook_selection(),
        "lethal_first": result.lethal_first(),
        "stop_reasons": result.stop_reasons(),
        "complete": result.complete(),
        "search_completeness": {
            "total": completeness.total(),
            "reliably_compared": completeness.reliably_compared(),
            "evidence_cap": completeness.evidence_cap(),
            "ratio": completeness.ratio(),
        },
        "stats": result.stats(),
        "rule_coverage": result.rule_coverage(),
        "candidates": candidates,
    })
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/beam")
        .join(format!("{name}.json"))
}

fn update_golden() -> bool {
    matches!(std::env::var("UPDATE_GOLDEN"), Ok(value) if value == "1")
}

fn assert_golden(name: &str, projection: Value) {
    let path = golden_path(name);
    let actual = json!({
        "fixture_schema_version": 1,
        "fixture_id": name,
        "projection": projection,
    });
    let pretty = serde_json::to_string_pretty(&actual).expect("serialize Beam golden projection");
    if update_golden() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create Beam golden directory");
        }
        fs::write(&path, format!("{pretty}\n")).expect("write Beam golden fixture");
        return;
    }

    let raw = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing Beam golden {}: {error}; refresh explicitly with \
             UPDATE_GOLDEN=1 cargo test -p hs-planner --test beam_golden",
            path.display()
        )
    });
    let expected: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("invalid Beam golden {}: {error}", path.display()));
    assert_eq!(
        expected, actual,
        "Beam golden `{name}` changed; inspect the projection and refresh explicitly with \
         UPDATE_GOLDEN=1 cargo test -p hs-planner --test beam_golden"
    );
}

#[test]
fn golden_generic_deterministic() {
    let config = SearchConfig {
        seed: 101,
        max_depth: 2,
        beam_width: 2,
        node_budget: 32,
        candidate_budget: 32,
        ..SearchConfig::default()
    };
    let first_search = two_attack_case().search(config, None);
    let repeated_search = two_attack_case().search(config, None);
    assert_eq!(first_search.playbook_id, "generic");
    assert!(first_search.result.stop_reasons().is_empty());
    assert!(first_search.result.complete());
    let first = stable_projection(&first_search);
    let repeated = stable_projection(&repeated_search);
    assert_eq!(
        first, repeated,
        "fixed generic search must reproduce exactly"
    );
    assert_golden("generic_deterministic", first);
}

#[test]
fn golden_quest_hunter() {
    let config = SearchConfig {
        seed: 202,
        max_depth: 2,
        beam_width: 2,
        node_budget: 32,
        candidate_budget: 32,
        ..SearchConfig::default()
    };
    let signature = quest_hunter_signature();
    let search = two_attack_case().search(config, Some(&signature));
    assert_eq!(search.playbook_id, "standard.quest_hunter.249896");
    assert!(search.result.stop_reasons().is_empty());
    assert!(search
        .result
        .candidates()
        .iter()
        .any(|candidate| candidate.line().win_point.is_some()));
    let actual = stable_projection(&search);
    assert_golden("quest_hunter", actual);
}

#[test]
fn golden_exact_chance() {
    let mut case = GoldenCase::new(30);
    let source = EntityId(20);
    case.add_random_damage_spell(
        source,
        "GOLDEN_EXACT_SPELL",
        "golden.exact",
        &[("EXACT_HIT", 1.0), ("EXACT_MISS", 3.0)],
        "EXACT_HIT",
        5,
    );
    case.play_option(1, source);
    let search = case.search(
        SearchConfig {
            seed: 303,
            sim_branch_budget: 2,
            max_depth: 1,
            beam_width: 2,
            node_budget: 16,
            candidate_budget: 16,
        },
        None,
    );
    let candidate = &search.result.candidates()[0];
    assert!(candidate.evidence().exact());
    assert!(!candidate.line().chance.estimated());
    assert!(candidate.line().chance.downside_milli() > 0);
    assert_eq!(search.result.search_completeness().ratio(), Confidence::ONE);
    let actual = stable_projection(&search);
    assert_golden("exact_chance", actual);
}

#[test]
fn golden_sampled_chance() {
    let mut case = GoldenCase::new(30);
    let source = EntityId(20);
    let cards = (0..8)
        .map(|index| (format!("SAMPLED_{index:02}"), 1.0))
        .collect::<Vec<_>>();
    let borrowed = cards
        .iter()
        .map(|(card, weight)| (card.as_str(), *weight))
        .collect::<Vec<_>>();
    case.add_random_damage_spell(
        source,
        "GOLDEN_SAMPLED_SPELL",
        "golden.sampled",
        &borrowed,
        "SAMPLED_00",
        5,
    );
    case.play_option(1, source);
    let search = case.search(
        SearchConfig {
            seed: 404,
            sim_branch_budget: 3,
            max_depth: 1,
            beam_width: 2,
            node_budget: 16,
            candidate_budget: 16,
        },
        None,
    );
    let candidate = &search.result.candidates()[0];
    assert!(!candidate.evidence().exact());
    assert_eq!(candidate.evidence().sample_count(), 3);
    assert!(candidate.line().chance.estimated());
    assert!(search
        .result
        .stop_reasons()
        .contains(&BeamStopReason::SampledSimulation));
    assert_eq!(
        search.result.search_completeness().ratio(),
        Confidence::ratio(7, 10)
    );
    let actual = stable_projection(&search);
    assert_golden("sampled_chance", actual);
}

#[test]
fn golden_budget_truncation() {
    let mut case = GoldenCase::new(30);
    case.end_turn_option(1);
    case.end_turn_option(2);
    let search = case.search(
        SearchConfig {
            seed: 505,
            max_depth: 1,
            beam_width: 2,
            node_budget: 16,
            candidate_budget: 1,
            ..SearchConfig::default()
        },
        None,
    );
    assert!(search
        .result
        .stop_reasons()
        .contains(&BeamStopReason::CandidateBudget));
    assert_eq!(
        search.result.search_completeness().ratio(),
        Confidence::ratio(1, 2)
    );
    let actual = stable_projection(&search);
    assert_golden("budget_truncation", actual);
}

#[test]
fn golden_unknown_unsupported() {
    let mut case = GoldenCase::new(30);
    let source = EntityId(20);
    case.add_unknown_spell(source);
    case.play_option(1, source);
    let search = case.search(
        SearchConfig {
            seed: 606,
            max_depth: 1,
            beam_width: 2,
            node_budget: 16,
            candidate_budget: 16,
            ..SearchConfig::default()
        },
        None,
    );
    assert!(search.result.candidates().is_empty());
    assert_eq!(search.result.lethal_first(), LethalAssessment::Unknown);
    assert!(search
        .result
        .stop_reasons()
        .contains(&BeamStopReason::UnsupportedSimulation));
    assert_eq!(
        search.result.search_completeness().ratio(),
        Confidence::ZERO
    );
    let actual = stable_projection(&search);
    assert_golden("unknown_unsupported", actual);
}
