use chrono::{TimeZone, Utc};
use hs_advisor_api::{
    ActionKind, ActionOrigin, AdviceCore, AdviceIdentity, AdviceLine, AdvicePayload, AdviceResult,
    AdviceRisk, AdviceRiskKind, AdviceWarning, AdviceWarningKind, ChanceSummary, Confidence,
    ConfidenceBreakdown, ConfidenceFactor, ConfidencePolicy, ConfidenceReason, LegalAction,
    LethalAssessment, PlannedAction, RuleCoverageItem, RuleCoverageReport, ScoreBreakdown,
    ScoreComponent, ScoreFeature, SearchCompleteness, SearchConfig, ServerActionRef,
    UnavailableReason,
};
use hs_belief::{BeliefSummary, KnownGenerationSource};
use hs_types::{BuildId, CardId, EntityId, GameId, RuleStatus};
use serde::de::DeserializeOwned;
use serde::Serialize;

fn card(id: &str) -> CardId {
    CardId::new(id)
}

fn confidence(value: f32) -> Confidence {
    Confidence::new(value).expect("fixture confidence")
}

fn legal_action() -> LegalAction {
    LegalAction {
        server_ref: ServerActionRef {
            options_id: 17,
            option_index: 3,
            sub_option_index: Some(1),
            target: Some(EntityId(99)),
            position: Some(2),
        },
        kind: ActionKind::PlayCard,
        source: Some(EntityId(42)),
    }
}

fn coverage(items: Vec<RuleCoverageItem>) -> RuleCoverageReport {
    RuleCoverageReport::new(items)
}

fn score(components: Vec<ScoreComponent>) -> ScoreBreakdown {
    ScoreBreakdown::new(components).expect("unique fixture features")
}

fn core_with_coverage(rule_coverage: RuleCoverageReport) -> AdviceCore {
    let line_confidence = ConfidenceBreakdown::new([
        ConfidenceFactor {
            reason: ConfidenceReason::ExactSimulation,
            confidence: confidence(0.95),
        },
        ConfidenceFactor {
            reason: ConfidenceReason::RuleInferred,
            confidence: confidence(0.85),
        },
    ]);
    let completeness = SearchCompleteness::new(3, 2).expect("valid completeness");
    let result_confidence =
        ConfidencePolicy::default().result_breakdown(&line_confidence, &completeness);

    AdviceCore::new(
        AdviceIdentity {
            game_id: GameId(88),
            state_revision: 144,
            state_hash: "state-sha256".into(),
            options_id: 17,
            build_id: BuildId(234_567),
        },
        AdvicePayload {
            confidence: result_confidence,
            rule_coverage,
            opponent_belief_summary: BeliefSummary {
                opponent_seen_cards: vec![card("KNOWN_CARD")],
                opponent_unknown_hand_count: 3,
                known_generation_sources: vec![KnownGenerationSource {
                    generated_card: card("TOKEN"),
                    source_card: card("GENERATOR"),
                }],
                archetype_probabilities: Vec::new(),
                insufficient_data: true,
            },
            search_completeness: completeness,
            lines: vec![AdviceLine {
                first_action: legal_action(),
                follow_up: vec![PlannedAction::simulated(
                    ActionKind::Attack,
                    Some(EntityId(42)),
                    Some(EntityId(7)),
                    None,
                    None,
                )],
                score: score(vec![
                    ScoreComponent {
                        feature: ScoreFeature::RiskPenalty,
                        raw: 2,
                        weight_milli: -300,
                    },
                    ScoreComponent {
                        feature: ScoreFeature::OpponentEffectiveHealth,
                        raw: 4,
                        weight_milli: -1_000,
                    },
                ]),
                chance: ChanceSummary::new(
                    -4_600, 700, -4_775, -6_000, -4_000, 1_000_000, 1_000_000, false, 0,
                )
                .expect("valid chance summary fixture"),
                lethal: LethalAssessment::Probabilistic {
                    probability: confidence(0.5),
                },
                confidence: line_confidence,
                complete: false,
                win_point: Some("set up lethal".into()),
                explanation: "Reduce the opponent's effective health.".into(),
                risks: vec![AdviceRisk {
                    kind: AdviceRiskKind::Randomness,
                    code: "exact_split".into(),
                    detail: "Only one exact branch is lethal.".into(),
                }],
            }],
            explanation: "Best reliably compared line.".into(),
            warnings: vec![AdviceWarning {
                kind: AdviceWarningKind::SearchIncomplete,
                code: "root_not_compared".into(),
                message: "One legal root action was not reliably compared.".into(),
            }],
        },
    )
}

fn assert_round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("serialize fixture");
    let decoded: T = serde_json::from_str(&json).expect("deserialize fixture");
    assert_eq!(&decoded, value);
}

#[test]
fn legacy_legal_action_wire_shape_is_unchanged() {
    let value = legal_action();
    let json = serde_json::to_string(&value).expect("serialize legacy action");

    assert_eq!(
        json,
        r#"{"server_ref":{"options_id":17,"option_index":3,"sub_option_index":1,"target":99,"position":2},"kind":"play_card","source":42}"#
    );
    assert_round_trip(&value);
}

#[test]
fn all_public_advice_dtos_round_trip() {
    let report = coverage(vec![RuleCoverageItem {
        card_id: card("CARD_A"),
        status: Some(RuleStatus::Verified),
        confidence: Confidence::ONE,
        runtime_unknown: false,
    }]);
    let core = core_with_coverage(report);
    let generated_at = Utc
        .with_ymd_and_hms(2026, 8, 30, 9, 10, 11)
        .single()
        .expect("fixture timestamp");
    let result = AdviceResult::from_core(core.clone(), generated_at);

    assert_round_trip(&core);
    assert_round_trip(&result);
    assert_round_trip(&ConfidencePolicy::default());
    assert_round_trip(&SearchConfig {
        seed: 123,
        sim_branch_budget: 64,
        max_depth: 7,
        beam_width: 12,
        node_budget: 1_000,
        candidate_budget: 4_000,
    });
    assert_round_trip(&UnavailableReason::MissingBuildData);
    assert_eq!(result.identity(), &core.identity);
    assert_eq!(result.payload(), &core.payload);
    assert_eq!(result.generated_at(), generated_at);
    let wire = serde_json::to_value(&result).expect("serialize result shape");
    assert!(wire.get("identity").is_some());
    assert!(wire.get("payload").is_some());
    assert!(wire.get("generated_at").is_some());
    assert!(wire.get("core").is_none());
}

#[test]
fn generated_at_is_outside_deterministic_core() {
    let core = core_with_coverage(RuleCoverageReport::default());
    let early = AdviceResult::from_core(
        core.clone(),
        Utc.with_ymd_and_hms(2026, 8, 30, 1, 0, 0)
            .single()
            .expect("early timestamp"),
    );
    let late = AdviceResult::from_core(
        core,
        Utc.with_ymd_and_hms(2026, 8, 30, 2, 0, 0)
            .single()
            .expect("late timestamp"),
    );

    assert_ne!(early, late);
    assert_eq!(early.core(), late.core());
    assert_eq!(
        serde_json::to_vec(early.core()).expect("serialize early core"),
        serde_json::to_vec(late.core()).expect("serialize late core")
    );
}

#[test]
fn coverage_sorts_deduplicates_and_keeps_weakest_evidence() {
    let report = coverage(vec![
        RuleCoverageItem {
            card_id: card("CARD_B"),
            status: Some(RuleStatus::Verified),
            confidence: confidence(0.9),
            runtime_unknown: false,
        },
        RuleCoverageItem {
            card_id: card("CARD_A"),
            status: Some(RuleStatus::Inferred),
            confidence: confidence(0.8),
            runtime_unknown: false,
        },
        RuleCoverageItem {
            card_id: card("CARD_B"),
            status: Some(RuleStatus::Partial),
            confidence: confidence(0.6),
            runtime_unknown: true,
        },
        RuleCoverageItem {
            card_id: card("CARD_A"),
            status: None,
            confidence: confidence(0.3),
            runtime_unknown: true,
        },
    ]);

    assert_eq!(
        report
            .items()
            .iter()
            .map(|item| item.card_id.as_str())
            .collect::<Vec<_>>(),
        ["CARD_A", "CARD_B"]
    );
    assert_eq!(report.items()[0].status, None);
    assert_eq!(report.items()[0].confidence, confidence(0.3));
    assert!(report.items()[0].runtime_unknown);
    assert_eq!(report.items()[1].status, Some(RuleStatus::Partial));
    assert_eq!(report.items()[1].confidence, confidence(0.6));
    assert!(report.items()[1].runtime_unknown);

    assert_eq!(report.total_count(), 2);
    assert_eq!(report.covered_count(), 1);
    assert_eq!(report.missing_count(), 1);
    assert_eq!(report.status_count(RuleStatus::Partial), 1);
    assert_eq!(report.runtime_unknown_count(), 2);
    assert_eq!(report.ratio(), confidence(0.5));
    assert_eq!(RuleCoverageReport::default().ratio(), Confidence::ZERO);
}

#[test]
fn coverage_deserialization_also_normalizes_items() {
    let json = r#"{
        "items": [
            {"card_id":"Z","status":"verified","confidence":0.9,"runtime_unknown":false},
            {"card_id":"A","status":"inferred","confidence":0.8,"runtime_unknown":false},
            {"card_id":"Z","status":"stale","confidence":0.4,"runtime_unknown":true}
        ]
    }"#;

    let report: RuleCoverageReport = serde_json::from_str(json).expect("deserialize coverage");

    assert_eq!(report.items().len(), 2);
    assert_eq!(report.items()[0].card_id, card("A"));
    assert_eq!(report.items()[1].card_id, card("Z"));
    assert_eq!(report.items()[1].status, Some(RuleStatus::Stale));
    assert_eq!(report.items()[1].confidence, confidence(0.4));
    assert!(report.items()[1].runtime_unknown);
}

#[test]
fn invalid_confidence_is_rejected_at_both_boundaries() {
    assert!(Confidence::new(f32::NAN).is_err());
    assert!(Confidence::new(f32::INFINITY).is_err());
    assert!(Confidence::new(-0.01).is_err());
    assert!(Confidence::new(1.01).is_err());
    assert_eq!(
        Confidence::new(-0.0).expect("canonical zero"),
        Confidence::ZERO
    );

    for json in ["-0.01", "1.01", "1e999"] {
        assert!(
            serde_json::from_str::<Confidence>(json).is_err(),
            "invalid JSON confidence accepted: {json}"
        );
    }
}

#[test]
fn confidence_policy_is_monotonic_and_search_caps_the_result() {
    let policy = ConfidencePolicy::default();
    let factors = [
        policy.rule_factor(Some(RuleStatus::Verified), false),
        policy.rule_factor(Some(RuleStatus::Inferred), false),
        policy.rule_factor(Some(RuleStatus::Partial), false),
        policy.rule_factor(Some(RuleStatus::Stale), false),
        policy.rule_factor(None, false),
        policy.rule_factor(Some(RuleStatus::Verified), true),
    ];

    for pair in factors.windows(2) {
        assert!(pair[0].confidence > pair[1].confidence, "{pair:?}");
    }

    let verified_line = ConfidenceBreakdown::new([factors[0].clone()]);
    let half_searched = SearchCompleteness::new(4, 2).expect("valid completeness");
    let result = policy.result_breakdown(&verified_line, &half_searched);
    assert_eq!(result.confidence(), confidence(0.5));
    assert!(result
        .factors()
        .iter()
        .any(|factor| factor.reason == ConfidenceReason::SearchIncomplete));

    let complete = policy.result_breakdown(
        &verified_line,
        &SearchCompleteness::new(4, 4).expect("complete search"),
    );
    assert_eq!(complete, verified_line);
    assert!(complete
        .factors()
        .iter()
        .all(|factor| factor.reason != ConfidenceReason::SearchIncomplete));
}

#[test]
fn score_total_and_search_ratio_are_derived_only() {
    let score = score(vec![
        ScoreComponent {
            feature: ScoreFeature::RiskPenalty,
            raw: 2,
            weight_milli: -250,
        },
        ScoreComponent {
            feature: ScoreFeature::FriendlyBoardAttack,
            raw: 3,
            weight_milli: 700,
        },
    ]);

    assert_eq!(score.total(), 1_600);
    assert!(!serde_json::to_string(&score)
        .expect("serialize score")
        .contains("total"));

    let search = SearchCompleteness::new(5, 2).expect("valid completeness");
    assert_eq!(search.total(), 5);
    assert_eq!(search.reliably_compared(), 2);
    assert_eq!(search.ratio(), confidence(0.4));
    assert_eq!(search.evidence_cap(), Confidence::ONE);
    assert!(!serde_json::to_string(&search)
        .expect("serialize legacy-compatible completeness")
        .contains("evidence_cap"));

    let capped =
        SearchCompleteness::with_evidence_cap(5, 5, confidence(0.7)).expect("valid evidence cap");
    assert_eq!(capped.ratio(), confidence(0.7));
    assert!(serde_json::to_string(&capped)
        .expect("serialize capped completeness")
        .contains("evidence_cap"));
    let legacy: SearchCompleteness = serde_json::from_str(r#"{"total":5,"reliably_compared":5}"#)
        .expect("legacy completeness defaults to an uncapped value");
    assert_eq!(legacy.evidence_cap(), Confidence::ONE);
    assert!(
        serde_json::from_str::<SearchCompleteness>(r#"{"total":2,"reliably_compared":3}"#).is_err()
    );
}

#[test]
fn normalized_inputs_produce_identical_json_bytes() {
    let first = coverage(vec![
        RuleCoverageItem {
            card_id: card("B"),
            status: Some(RuleStatus::Verified),
            confidence: confidence(0.9),
            runtime_unknown: false,
        },
        RuleCoverageItem {
            card_id: card("A"),
            status: Some(RuleStatus::Partial),
            confidence: confidence(0.6),
            runtime_unknown: false,
        },
    ]);
    let second = coverage(vec![
        RuleCoverageItem {
            card_id: card("A"),
            status: Some(RuleStatus::Partial),
            confidence: confidence(0.8),
            runtime_unknown: false,
        },
        RuleCoverageItem {
            card_id: card("A"),
            status: Some(RuleStatus::Verified),
            confidence: confidence(0.6),
            runtime_unknown: false,
        },
        RuleCoverageItem {
            card_id: card("B"),
            status: Some(RuleStatus::Verified),
            confidence: confidence(0.9),
            runtime_unknown: false,
        },
    ]);

    let first_core = core_with_coverage(first);
    let second_core = core_with_coverage(second);
    assert_eq!(first_core, second_core);
    assert_eq!(
        serde_json::to_vec(&first_core).expect("serialize first"),
        serde_json::to_vec(&second_core).expect("serialize second")
    );
}

#[test]
fn legacy_advice_line_defaults_the_additive_chance_summary() {
    let mut legacy = serde_json::to_value(core_with_coverage(RuleCoverageReport::default()))
        .expect("serialize advice core");
    legacy["payload"]["lines"][0]
        .as_object_mut()
        .expect("line object")
        .remove("chance");

    let decoded: AdviceCore = serde_json::from_value(legacy).expect("legacy AdviceLine");
    assert_eq!(decoded.payload.lines[0].chance, ChanceSummary::default());
}

#[test]
fn chance_summary_rejects_inconsistent_public_values() {
    assert!(ChanceSummary::new(0, 0, 0, 0, 0, 0, 1_000, false, 0).is_err());
    assert!(ChanceSummary::new(5, 0, 5, 6, 7, 1_000, 1_000, false, 0).is_err());
    assert!(ChanceSummary::new(5, 0, 6, 0, 10, 1_000, 1_000, false, 0).is_err());
    assert!(ChanceSummary::new(5, 0, 5, 0, 10, 1_000, 1_000, true, 0).is_err());

    assert!(
        serde_json::from_str::<ChanceSummary>(r#"{"available":false,"expected_milli":1}"#).is_err()
    );
    assert!(serde_json::from_str::<ChanceSummary>(
        r#"{"available":true,"expected_milli":5,"rank_milli":5,"min_milli":0,"max_milli":10,"probability_mass_units":999,"probability_scale":1000}"#
    )
    .is_err());
}

#[test]
fn planned_simulated_action_cannot_carry_a_server_reference() {
    let action = PlannedAction::simulated(ActionKind::EndTurn, None, None, None, None);
    let json = serde_json::to_string(&action).expect("serialize planned action");
    assert!(!json.contains("server_ref"));
    assert!(json.contains(r#""origin":"simulated""#));
    assert_eq!(action.origin(), ActionOrigin::Simulated);

    let forged = json.replace(r#""origin":"simulated""#, r#""origin":"server""#);
    let error = serde_json::from_str::<PlannedAction>(&forged)
        .expect_err("server-backed continuations must be rejected");
    assert!(error
        .to_string()
        .contains("planned follow-up action origin must be simulated"));
}
