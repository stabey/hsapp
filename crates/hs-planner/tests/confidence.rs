use hs_advisor_api::{
    ActionOrigin, Confidence, ConfidencePolicy, ConfidenceReason, RuleCoverageItem,
    RuleCoverageReport, SearchCompleteness, UnavailableReason,
};
use hs_planner::{
    line_confidence, result_confidence, LineEvidence, PlannerError, SimulationEvidence,
};
use hs_sim::{
    OutcomeSet, ReproMeta, RuleTrace, SampleMethod, SampledOutcomeSet, SimGameState, SimOutcome,
    UnsupportedReason, WeightedOutcome,
};
use hs_types::{CardId, RuleStatus};

fn coverage(status: Option<RuleStatus>, runtime_unknown: bool) -> RuleCoverageReport {
    RuleCoverageReport::new([RuleCoverageItem {
        card_id: CardId::new("CARD"),
        status,
        confidence: Confidence::ONE,
        runtime_unknown,
    }])
}

#[test]
fn rule_evidence_monotonically_lowers_confidence() {
    let policy = ConfidencePolicy::default();
    let values = [
        policy
            .rule_factor(Some(RuleStatus::Verified), false)
            .confidence,
        policy
            .rule_factor(Some(RuleStatus::Inferred), false)
            .confidence,
        policy
            .rule_factor(Some(RuleStatus::Partial), false)
            .confidence,
        policy
            .rule_factor(Some(RuleStatus::Stale), false)
            .confidence,
        policy
            .rule_factor(Some(RuleStatus::Unknown), false)
            .confidence,
        policy.rule_factor(None, false).confidence,
        policy
            .rule_factor(Some(RuleStatus::Verified), true)
            .confidence,
    ];

    assert!(values.windows(2).all(|pair| pair[0] > pair[1]));
}

#[test]
fn static_unknown_or_missing_rules_are_not_reliable_lines() {
    for status in [None, Some(RuleStatus::Unknown)] {
        let report = coverage(status, false);
        assert_eq!(
            line_confidence(
                &ConfidencePolicy::default(),
                &LineEvidence {
                    rule_coverage: &report,
                    action_origins: &[ActionOrigin::Server],
                    simulations: &[SimulationEvidence::Deterministic { truncated: false }],
                },
            ),
            Err(PlannerError::Unavailable(
                UnavailableReason::NoReliableActions
            ))
        );
    }

    let runtime_report = coverage(Some(RuleStatus::Verified), true);
    let runtime = line_confidence(
        &ConfidencePolicy::default(),
        &LineEvidence {
            rule_coverage: &runtime_report,
            action_origins: &[ActionOrigin::Server],
            simulations: &[SimulationEvidence::Deterministic { truncated: false }],
        },
    )
    .expect("runtime unknown remains publishable with a conservative cap");
    assert_eq!(
        runtime.confidence(),
        ConfidencePolicy::default().runtime_unknown
    );
}

#[test]
fn server_simulated_exact_sampled_and_truncation_reasons_are_explicit_and_stable() {
    let report = coverage(Some(RuleStatus::Verified), false);
    let breakdown = line_confidence(
        &ConfidencePolicy::default(),
        &LineEvidence {
            rule_coverage: &report,
            action_origins: &[
                ActionOrigin::Simulated,
                ActionOrigin::Server,
                ActionOrigin::Simulated,
            ],
            simulations: &[
                SimulationEvidence::Exact {
                    probability_mass_complete: false,
                    truncated: false,
                },
                SimulationEvidence::Sampled { truncated: true },
            ],
        },
    )
    .expect("supported line");
    let reasons = breakdown
        .factors()
        .iter()
        .map(|factor| factor.reason.clone())
        .collect::<Vec<_>>();

    assert!(reasons.contains(&ConfidenceReason::ServerLegalAction));
    assert!(reasons.contains(&ConfidenceReason::SimulatedLegalAction));
    assert!(reasons.contains(&ConfidenceReason::ExactSimulation));
    assert!(reasons.contains(&ConfidenceReason::SampledSimulation));
    assert!(reasons.contains(&ConfidenceReason::ProbabilityMassIncomplete));
    assert!(reasons.contains(&ConfidenceReason::BudgetTruncated));
    assert!(reasons.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        reasons
            .iter()
            .filter(|reason| **reason == ConfidenceReason::SimulatedLegalAction)
            .count(),
        1
    );
}

#[test]
fn search_completeness_caps_an_otherwise_verified_top_line() {
    let report = coverage(Some(RuleStatus::Verified), false);
    let line = line_confidence(
        &ConfidencePolicy::default(),
        &LineEvidence {
            rule_coverage: &report,
            action_origins: &[ActionOrigin::Server],
            simulations: &[SimulationEvidence::Deterministic { truncated: false }],
        },
    )
    .expect("verified line");
    assert_eq!(line.confidence(), Confidence::ONE);

    let completeness = SearchCompleteness::new(4, 2).expect("valid completeness");
    let result = result_confidence(&ConfidencePolicy::default(), &line, &completeness);

    assert_eq!(result.confidence(), Confidence::new(0.5).expect("valid"));
    assert!(result
        .factors()
        .iter()
        .any(|factor| factor.reason == ConfidenceReason::SearchIncomplete));
}

#[test]
fn empty_or_unsupported_line_evidence_is_no_reliable_actions() {
    let report = RuleCoverageReport::default();
    let cases = [
        LineEvidence {
            rule_coverage: &report,
            action_origins: &[],
            simulations: &[SimulationEvidence::Deterministic { truncated: false }],
        },
        LineEvidence {
            rule_coverage: &report,
            action_origins: &[ActionOrigin::Server],
            simulations: &[],
        },
        LineEvidence {
            rule_coverage: &report,
            action_origins: &[ActionOrigin::Server],
            simulations: &[SimulationEvidence::Unsupported],
        },
    ];

    for case in cases {
        assert_eq!(
            line_confidence(&ConfidencePolicy::default(), &case),
            Err(PlannerError::Unavailable(
                UnavailableReason::NoReliableActions
            ))
        );
    }
}

fn outcome(outcome: OutcomeSet, truncation: Option<&str>) -> SimOutcome {
    SimOutcome {
        outcome,
        trace: RuleTrace::default(),
        meta: ReproMeta {
            seed: 1,
            branch_budget: 4,
            method: SampleMethod::Exact,
            truncation: truncation.map(str::to_owned),
            chance_kind: None,
            rule_version: String::new(),
            pool_snapshot_hashes: Vec::new(),
        },
        unknown_rule_cards: Vec::new(),
        rule_uses: Vec::new(),
    }
}

#[test]
fn simulation_evidence_projects_exact_mass_sampling_and_runtime_truncation() {
    let state = SimGameState::default();
    let exact = outcome(
        OutcomeSet::Exact(vec![
            WeightedOutcome {
                probability: 0.4,
                state: state.clone(),
                note: None,
            },
            WeightedOutcome {
                probability: 0.4,
                state: state.clone(),
                note: None,
            },
        ]),
        None,
    );
    assert_eq!(
        SimulationEvidence::from_outcome(&exact),
        SimulationEvidence::Exact {
            probability_mass_complete: false,
            truncated: false
        }
    );

    let sampled = outcome(
        OutcomeSet::Sampled(SampledOutcomeSet {
            samples: vec![WeightedOutcome {
                probability: 1.0,
                state,
                note: None,
            }],
            method: SampleMethod::MonteCarlo,
            total_branches: Some(3),
        }),
        None,
    );
    assert_eq!(
        SimulationEvidence::from_outcome(&sampled),
        SimulationEvidence::Sampled { truncated: true }
    );

    let unsupported = outcome(
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented {
            what: "unsupported line".into(),
        }),
        Some("budget"),
    );
    assert_eq!(
        SimulationEvidence::from_outcome(&unsupported),
        SimulationEvidence::Unsupported
    );

    let empty = outcome(OutcomeSet::Exact(Vec::new()), None);
    assert_eq!(
        SimulationEvidence::from_outcome(&empty),
        SimulationEvidence::Unsupported
    );
}
