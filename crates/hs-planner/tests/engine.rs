mod common;

use std::sync::Arc;

use hs_advisor_api::{
    AdviceCore, AdviceLine, AdvicePayload, Confidence, ConfidenceBreakdown, ConfidenceReason,
    LethalAssessment, RuleCoverageReport, ScoreBreakdown, SearchCompleteness, SearchConfig,
    ServerActionRef, UnavailableReason,
};
use hs_log_core::DomainEvent;
use hs_observed_state::{reduce, state_hash, EntityState};
use hs_planner::{prepare_request, Engine, PlannerError, PlannerRequest, PlannerResult};
use hs_types::{BuildId, CardId, EntityId, PlayerId, Zone};

use common::{
    end_turn_option, resources, valid_state, with_friendly_choice, with_open_block, BUILD, FRIENDLY,
};

fn assert_unavailable(state: hs_observed_state::ObservedGameState, reason: UnavailableReason) {
    let error = prepare_request(state, resources(BUILD), SearchConfig::default())
        .expect_err("request must be unavailable");
    assert_eq!(error, PlannerError::Unavailable(reason));
}

#[test]
fn request_identity_actions_hash_and_build_come_from_one_snapshot() {
    let state = valid_state();
    let expected_hash = state_hash(&state);
    let expected_revision = state.revision;
    let expected_issued_revision = state.options.as_ref().expect("options").issued_revision;

    let request = prepare_request(state, resources(BUILD), SearchConfig::default())
        .expect("valid planner request");

    assert_eq!(request.identity().game_id, request.legal_actions().game_id);
    assert_eq!(
        request.identity().options_id,
        request.legal_actions().options_id
    );
    assert_eq!(request.identity().state_revision, expected_revision);
    assert_eq!(request.identity().state_hash, expected_hash);
    assert_eq!(request.identity().build_id, BUILD);
    assert_eq!(request.resources().build_id(), BUILD);
    assert_eq!(
        request.legal_actions().issued_revision,
        expected_issued_revision
    );
    assert_eq!(request.legal_actions().actions.len(), 1);
    assert!(request.legal_actions().diagnostics.is_empty());
}

#[test]
fn every_startup_gate_maps_to_shared_unavailable_reason() {
    let mut state = valid_state();
    state.build_id = None;
    assert_unavailable(state, UnavailableReason::MissingBuild);

    let error = prepare_request(
        valid_state(),
        resources(BuildId(BUILD.0 + 1)),
        SearchConfig::default(),
    )
    .expect_err("wrong resource build");
    assert_eq!(
        error,
        PlannerError::Unavailable(UnavailableReason::MissingBuildData)
    );

    let mut state = valid_state();
    state.friendly_player = None;
    assert_unavailable(state, UnavailableReason::MissingFriendlyPlayer);

    let mut state = valid_state();
    state.current_player = Some(PlayerId(FRIENDLY.0 + 1));
    assert_unavailable(state, UnavailableReason::NotFriendlyTurn);

    assert_unavailable(
        with_open_block(&valid_state()),
        UnavailableReason::BlocksOpen,
    );

    let mut state = valid_state();
    state.game_id = None;
    assert_unavailable(state, UnavailableReason::MissingGame);

    let mut state = valid_state();
    state.options = None;
    assert_unavailable(state, UnavailableReason::NoOptions);

    assert_unavailable(
        with_friendly_choice(&valid_state()),
        UnavailableReason::PendingChoice,
    );

    let mut state = valid_state();
    state.options.as_mut().expect("options").options.clear();
    assert_unavailable(state, UnavailableReason::NoReliableActions);
}

#[test]
fn belief_is_projected_from_public_opponent_facts_without_mutating_observed_state() {
    let mut state = valid_state();
    let mut source = EntityState::new(EntityId(10), Some(CardId::new("SOURCE")), Zone::Play);
    source.controller = Some(PlayerId(2));
    let mut generated = EntityState::new(EntityId(11), Some(CardId::new("TOKEN")), Zone::Hand);
    generated.controller = Some(PlayerId(2));
    generated.source_entity = Some(EntityId(10));
    let mut hidden = EntityState::new(EntityId(12), None, Zone::Hand);
    hidden.controller = Some(PlayerId(2));
    let mut friendly = EntityState::new(EntityId(13), Some(CardId::new("FRIENDLY")), Zone::Play);
    friendly.controller = Some(FRIENDLY);
    state.entities.insert(source.entity_id, source);
    state.entities.insert(generated.entity_id, generated);
    state.entities.insert(hidden.entity_id, hidden);
    state.entities.insert(friendly.entity_id, friendly);
    let untouched = state.clone();

    let request =
        prepare_request(state, resources(BUILD), SearchConfig::default()).expect("valid request");
    let summary = request.belief().summary();

    assert_eq!(request.observed_state(), &untouched);
    assert_eq!(
        summary.opponent_seen_cards,
        vec![CardId::new("SOURCE"), CardId::new("TOKEN")]
    );
    assert_eq!(summary.opponent_unknown_hand_count, 1);
    assert_eq!(summary.known_generation_sources.len(), 1);
    assert_eq!(
        summary.known_generation_sources[0].source_card,
        CardId::new("SOURCE")
    );
    assert!(summary.insufficient_data);
    assert!(summary.archetype_probabilities.is_empty());
}

#[test]
fn planner_boundary_is_owned_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<PlannerRequest>();
    assert_send_sync::<hs_planner::PlannerResources>();
    assert_send_sync::<Arc<dyn Engine>>();
}

#[test]
fn unavailable_and_failed_errors_are_structurally_distinct() {
    assert_ne!(
        PlannerError::Unavailable(UnavailableReason::NoReliableActions),
        PlannerError::Failed {
            message: "search worker panicked".into()
        }
    );
}

#[test]
fn empty_planner_payload_is_no_reliable_actions() {
    let request = prepare_request(valid_state(), resources(BUILD), SearchConfig::default())
        .expect("valid request");
    let payload = AdvicePayload {
        confidence: ConfidenceBreakdown::default(),
        rule_coverage: RuleCoverageReport::default(),
        opponent_belief_summary: hs_belief::BeliefState::from_public(Default::default()).summary(),
        search_completeness: SearchCompleteness::new(1, 0).expect("valid completeness"),
        lines: Vec::new(),
        explanation: String::new(),
        warnings: Vec::new(),
    };
    let error =
        PlannerResult::for_request(&request, payload).expect_err("empty results are unreliable");
    assert_eq!(
        error,
        PlannerError::Unavailable(UnavailableReason::NoReliableActions)
    );
}

fn payload_with_confidence(
    request: &PlannerRequest,
    line_confidence: ConfidenceBreakdown,
    result_confidence: ConfidenceBreakdown,
    search_completeness: SearchCompleteness,
) -> AdvicePayload {
    AdvicePayload {
        confidence: result_confidence,
        rule_coverage: RuleCoverageReport::default(),
        opponent_belief_summary: request.belief().summary(),
        search_completeness,
        lines: vec![AdviceLine {
            first_action: request.legal_actions().actions[0],
            follow_up: Vec::new(),
            score: ScoreBreakdown::default(),
            chance: Default::default(),
            lethal: LethalAssessment::None,
            confidence: line_confidence,
            complete: true,
            win_point: None,
            explanation: String::new(),
            risks: Vec::new(),
        }],
        explanation: String::new(),
        warnings: Vec::new(),
    }
}

#[test]
fn planner_result_rejects_confidence_above_line_or_search_evidence() {
    let mut state = valid_state();
    state
        .options
        .as_mut()
        .expect("options")
        .options
        .push(end_turn_option(4));
    let request =
        prepare_request(state, resources(BUILD), SearchConfig::default()).expect("valid request");
    let partial = ConfidenceBreakdown::from_factor(
        ConfidenceReason::RulePartial,
        Confidence::new(0.65).expect("valid confidence"),
    );
    let over_top = payload_with_confidence(
        &request,
        partial,
        ConfidenceBreakdown::default(),
        SearchCompleteness::new(2, 2).expect("complete search"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, over_top),
        Err(PlannerError::Failed { .. })
    ));

    let over_search = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::default(),
        SearchCompleteness::new(2, 1).expect("partial search"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, over_search),
        Err(PlannerError::Failed { .. })
    ));

    let unexplained = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::from_factor(
            ConfidenceReason::RulePartial,
            Confidence::new(0.4).expect("valid confidence"),
        ),
        SearchCompleteness::new(2, 1).expect("partial search"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, unexplained),
        Err(PlannerError::Failed { .. })
    ));

    let inaccurate = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::from_factor(
            ConfidenceReason::SearchIncomplete,
            Confidence::new(0.4).expect("valid confidence"),
        ),
        SearchCompleteness::new(2, 1).expect("partial search"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, inaccurate),
        Err(PlannerError::Failed { .. })
    ));

    let exact = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::from_factor(
            ConfidenceReason::SearchIncomplete,
            Confidence::new(0.5).expect("valid confidence"),
        ),
        SearchCompleteness::new(2, 1).expect("partial search"),
    );
    assert!(PlannerResult::for_request(&request, exact).is_ok());

    let misleading_complete = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::from_factor(ConfidenceReason::SearchIncomplete, Confidence::ONE),
        SearchCompleteness::new(2, 2).expect("complete search"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, misleading_complete),
        Err(PlannerError::Failed { .. })
    ));

    let line_level_search = payload_with_confidence(
        &request,
        ConfidenceBreakdown::from_factor(ConfidenceReason::SearchIncomplete, Confidence::ONE),
        ConfidenceBreakdown::default(),
        SearchCompleteness::new(2, 2).expect("complete search"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, line_level_search),
        Err(PlannerError::Failed { .. })
    ));
}

#[test]
fn planner_result_is_bound_to_prepared_roots_and_complete_root_count() {
    let request = prepare_request(valid_state(), resources(BUILD), SearchConfig::default())
        .expect("valid request");

    let wrong_total = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::default(),
        SearchCompleteness::new(2, 2).expect("internally valid completeness"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, wrong_total),
        Err(PlannerError::Failed { .. })
    ));

    let mut foreign_root = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::default(),
        SearchCompleteness::new(1, 1).expect("complete search"),
    );
    foreign_root.lines[0].first_action.server_ref = ServerActionRef {
        options_id: request.identity().options_id,
        option_index: 999,
        sub_option_index: None,
        target: None,
        position: None,
    };
    assert!(matches!(
        PlannerResult::for_request(&request, foreign_root),
        Err(PlannerError::Failed { .. })
    ));

    let zero_compared = payload_with_confidence(
        &request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::from_factor(ConfidenceReason::SearchIncomplete, Confidence::ZERO),
        SearchCompleteness::new(1, 0).expect("no reliable roots"),
    );
    assert!(matches!(
        PlannerResult::for_request(&request, zero_compared),
        Err(PlannerError::Failed { .. })
    ));

    let mut two_root_state = valid_state();
    two_root_state
        .options
        .as_mut()
        .expect("options")
        .options
        .push(end_turn_option(4));
    let two_root_request =
        prepare_request(two_root_state, resources(BUILD), SearchConfig::default())
            .expect("two-root request");
    let mut under_reported = payload_with_confidence(
        &two_root_request,
        ConfidenceBreakdown::default(),
        ConfidenceBreakdown::from_factor(
            ConfidenceReason::SearchIncomplete,
            Confidence::new(0.5).expect("half complete"),
        ),
        SearchCompleteness::new(2, 1).expect("one compared root"),
    );
    let mut second_line = under_reported.lines[0].clone();
    second_line.first_action = two_root_request.legal_actions().actions[1];
    under_reported.lines.push(second_line);
    assert!(matches!(
        PlannerResult::for_request(&two_root_request, under_reported),
        Err(PlannerError::Failed { .. })
    ));
}

#[test]
fn open_block_gate_observes_the_current_snapshot_revision() {
    let base = valid_state();
    let blocked = reduce(
        &base,
        &DomainEvent::BlockOpened {
            block_type: "TRIGGER".into(),
            source: None,
            target: None,
            effect_card_id: None,
        },
    )
    .expect("open block");
    assert!(blocked.revision > base.revision);
    assert_unavailable(blocked, UnavailableReason::BlocksOpen);
}

#[test]
fn planner_result_is_deterministic_until_publication_time_is_added() {
    let request = prepare_request(valid_state(), resources(BUILD), SearchConfig::default())
        .expect("valid request");
    let identity = request.identity().clone();
    let payload = AdvicePayload {
        confidence: ConfidenceBreakdown::default(),
        rule_coverage: RuleCoverageReport::default(),
        opponent_belief_summary: request.belief().summary(),
        search_completeness: SearchCompleteness::new(1, 1).expect("valid completeness"),
        lines: vec![AdviceLine {
            first_action: request.legal_actions().actions[0],
            follow_up: Vec::new(),
            score: ScoreBreakdown::default(),
            chance: Default::default(),
            lethal: LethalAssessment::None,
            confidence: ConfidenceBreakdown::default(),
            complete: true,
            win_point: None,
            explanation: "stable".into(),
            risks: Vec::new(),
        }],
        explanation: "stable payload".into(),
        warnings: Vec::new(),
    };
    let result = PlannerResult::for_request(&request, payload).expect("non-empty result");

    let first = serde_json::to_vec(&result).expect("serialize result");
    let second = serde_json::to_vec(&result).expect("serialize result again");
    assert_eq!(first, second);
    let decoded: AdviceCore = serde_json::from_slice(&first).expect("deserialize public core");
    assert_eq!(decoded.identity, *result.identity());
    assert_eq!(decoded.payload, *result.payload());
    assert!(!String::from_utf8(first)
        .expect("json utf8")
        .contains("generated_at"));

    let generated_at = chrono::DateTime::parse_from_rfc3339("2026-08-30T12:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    let published = result.into_advice_result(generated_at);
    assert_eq!(published.identity(), &identity);
    assert_eq!(published.generated_at(), generated_at);
}
