use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use hs_advisor_api::{ActionKind, Confidence, SearchConfig};
use hs_effect_ir::{EffectProgram, IR_SCHEMA_VERSION};
use hs_log_core::{game_tag, DomainEvent};
use hs_observed_state::{
    reduce, EntityState, ObservedGameState, OptionAvailability, OptionSummary, OptionTargetSummary,
};
use hs_planner::{
    prepare_request, CoverageCollector, CoverageError, PlannerRequest, PlannerResources,
};
use hs_pools::PoolSnapshot;
use hs_sim::{
    CardLookup, OutcomeSet, PoolResolver, ReproMeta, RuleLookup, RuleTrace, RuleUse, SampleMethod,
    SimCardDefinition, SimError, SimGameState, SimOutcome,
};
use hs_types::{
    BuildId, BuildRange, CardId, EntityId, GameId, GamePhase, PlayerId, PoolId, RuleStatus, Zone,
};

const BUILD: BuildId = BuildId(500);
const FRIENDLY: PlayerId = PlayerId(1);

struct EmptyCards;

impl CardLookup for EmptyCards {
    fn sim_card(&self, _build: BuildId, _card_id: &CardId) -> Option<SimCardDefinition> {
        None
    }
}

struct EmptyPools;

impl PoolResolver for EmptyPools {
    fn snapshot(&self, _pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        Err(SimError::NotImplemented("empty coverage test pool"))
    }
}

fn valid_state() -> ObservedGameState {
    let mut state = ObservedGameState::default();
    state.build_id = Some(BUILD);
    state.game_id = Some(GameId(77));
    state.phase = GamePhase::Playing;
    state.friendly_player = Some(FRIENDLY);
    state.current_player = Some(FRIENDLY);
    state.revision = 40;
    reduce(
        &state,
        &DomainEvent::OptionsUpdated {
            id: 91,
            options: Vec::new(),
        },
    )
    .expect("valid options update")
}

struct MapRules {
    programs: BTreeMap<CardId, EffectProgram>,
}

impl RuleLookup for MapRules {
    fn program_for(&self, card_id: &CardId) -> Option<&EffectProgram> {
        self.programs.get(card_id)
    }
}

fn card(id: &str) -> CardId {
    CardId::new(id)
}

fn program(id: &str, status: RuleStatus, confidence: f32, min_build: u32) -> EffectProgram {
    EffectProgram {
        schema: IR_SCHEMA_VERSION,
        card_id: card(id),
        build_range: BuildRange {
            min: BuildId(min_build),
            max: None,
        },
        status,
        confidence,
        triggers: Vec::new(),
        play: Vec::new(),
        custom_handler: None,
        evidence: Vec::new(),
    }
}

fn rules(programs: Vec<EffectProgram>) -> MapRules {
    MapRules {
        programs: programs
            .into_iter()
            .map(|program| (program.card_id.clone(), program))
            .collect(),
    }
}

fn request_with_rules<R>(root_card_id: &str, rules: Arc<R>) -> PlannerRequest
where
    R: RuleLookup + Send + Sync + 'static,
{
    let mut state = valid_state();
    state.build_id = Some(BUILD);

    let mut power = EntityState::new(EntityId(1), Some(card(root_card_id)), Zone::Play);
    power.controller = Some(FRIENDLY);
    power
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::HERO_POWER);
    state.entities.insert(power.entity_id, power);

    let mut attacker = EntityState::new(EntityId(2), Some(card("ATTACKER")), Zone::Play);
    attacker.controller = Some(FRIENDLY);
    attacker
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
    state.entities.insert(attacker.entity_id, attacker);

    let mut defender = EntityState::new(EntityId(99), Some(card("DEFENDER")), Zone::Play);
    defender.controller = Some(PlayerId(FRIENDLY.0 + 1));
    defender
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
    state.entities.insert(defender.entity_id, defender);

    let mut power_option = OptionSummary::default();
    power_option.index = 1;
    power_option.option_type = "POWER".into();
    power_option.entity = Some(EntityId(1));
    power_option.availability = OptionAvailability::Allowed;

    let mut target = OptionTargetSummary::default();
    target.index = 1;
    target.entity = Some(EntityId(99));
    target.availability = OptionAvailability::Allowed;
    let mut attack_option = OptionSummary::default();
    attack_option.index = 2;
    attack_option.option_type = "ATTACK".into();
    attack_option.entity = Some(EntityId(2));
    attack_option.availability = OptionAvailability::Allowed;
    attack_option.target_entries = vec![target];

    state.options.as_mut().expect("options").options = vec![power_option, attack_option];

    prepare_request(
        state,
        PlannerResources::new(BUILD, rules, Arc::new(EmptyCards), Arc::new(EmptyPools)),
        SearchConfig::default(),
    )
    .expect("coverage request")
}

fn outcome(rule_uses: Vec<RuleUse>, unknown_rule_cards: Vec<CardId>) -> SimOutcome {
    SimOutcome {
        outcome: OutcomeSet::Deterministic(SimGameState::default()),
        trace: RuleTrace::default(),
        meta: ReproMeta {
            seed: 1,
            branch_budget: 2,
            method: SampleMethod::Exact,
            truncation: None,
            chance_kind: None,
            rule_version: String::new(),
            pool_snapshot_hashes: Vec::new(),
        },
        unknown_rule_cards,
        rule_uses,
    }
}

fn root(request: &PlannerRequest, kind: ActionKind) -> hs_advisor_api::LegalAction {
    *request
        .legal_actions()
        .actions
        .iter()
        .find(|action| action.kind == kind)
        .expect("prepared root kind")
}

#[test]
fn missing_unknown_runtime_and_wrong_build_stay_distinct_and_sorted() {
    let request = request_with_rules(
        "WRONG_BUILD",
        Arc::new(rules(vec![
            program("UNKNOWN", RuleStatus::Unknown, 0.8, BUILD.0),
            program("WRONG_BUILD", RuleStatus::Verified, 1.0, BUILD.0 + 1),
        ])),
    );

    let mut collector = CoverageCollector::for_request(&request);
    collector.record_outcome(&outcome(Vec::new(), vec![card("UNKNOWN"), card("MISSING")]));
    collector
        .record_root_action(&root(&request, ActionKind::UseHeroPower))
        .expect("prepared power root");
    let report = collector.finish();

    assert_eq!(
        report
            .items()
            .iter()
            .map(|item| item.card_id.as_str())
            .collect::<Vec<_>>(),
        vec!["MISSING", "UNKNOWN", "WRONG_BUILD"]
    );
    assert_eq!(report.items()[0].status, None);
    assert!(report.items()[0].runtime_unknown);
    assert_eq!(report.items()[1].status, Some(RuleStatus::Unknown));
    assert!(report.items()[1].runtime_unknown);
    assert_eq!(report.items()[2].status, None);
    assert!(!report.items()[2].runtime_unknown);
    assert_eq!(report.missing_count(), 2);
    assert_eq!(report.runtime_unknown_count(), 2);
}

#[test]
fn duplicate_path_evidence_merges_to_weakest_status_and_confidence() {
    let request = request_with_rules(
        "CARD",
        Arc::new(rules(vec![program(
            "CARD",
            RuleStatus::Verified,
            0.9,
            BUILD.0,
        )])),
    );
    let sim_outcome = outcome(
        vec![RuleUse {
            card_id: card("CARD"),
            status: RuleStatus::Partial,
            confidence: 0.4,
            rule_version: "CARD@500".into(),
        }],
        vec![card("CARD")],
    );

    let mut collector = CoverageCollector::for_request(&request);
    collector
        .record_root_action(&root(&request, ActionKind::UseHeroPower))
        .expect("prepared power root");
    collector.record_outcome(&sim_outcome);
    let report = collector.finish();

    assert_eq!(report.items().len(), 1);
    let item = &report.items()[0];
    assert_eq!(item.status, Some(RuleStatus::Partial));
    assert_eq!(item.confidence, Confidence::new(0.4).expect("valid"));
    assert!(item.runtime_unknown);
}

struct CountingRules {
    program: EffectProgram,
    calls: AtomicUsize,
}

impl RuleLookup for CountingRules {
    fn program_for(&self, card_id: &CardId) -> Option<&EffectProgram> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (card_id == &self.program.card_id).then_some(&self.program)
    }
}

#[test]
fn collector_is_request_bound_and_queries_only_relevant_evidence() {
    let rules = Arc::new(CountingRules {
        program: program("ROOT", RuleStatus::Verified, 1.0, BUILD.0),
        calls: AtomicUsize::new(0),
    });
    let request = request_with_rules("ROOT", rules.clone());
    let attack = root(&request, ActionKind::Attack);
    let power = root(&request, ActionKind::UseHeroPower);

    let mut collector = CoverageCollector::for_request(&request);
    collector
        .record_root_action(&attack)
        .expect("prepared attack root");
    assert_eq!(
        rules.calls.load(Ordering::SeqCst),
        0,
        "generic combat must not look up the attacker's card program"
    );

    let mut foreign = power;
    foreign.server_ref.option_index = 999;
    assert_eq!(
        collector.record_root_action(&foreign),
        Err(CoverageError::ForeignRoot)
    );
    assert_eq!(rules.calls.load(Ordering::SeqCst), 0);

    collector
        .record_root_action(&power)
        .expect("prepared power root");
    let report = collector.finish();

    assert_eq!(rules.calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.items().len(), 1);
    assert_eq!(report.items()[0].card_id, card("ROOT"));
}
