//! Evidence-quality confidence aggregation. This module never estimates wins.

use hs_advisor_api::{
    ActionOrigin, ConfidenceBreakdown, ConfidenceFactor, ConfidencePolicy, ConfidenceReason,
    RuleCoverageReport, SearchCompleteness, UnavailableReason,
};
use hs_sim::{OutcomeSet, SimOutcome};
use hs_types::RuleStatus;

use crate::PlannerError;

/// Stable projection of the simulator evidence that affects confidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationEvidence {
    Deterministic {
        truncated: bool,
    },
    Exact {
        probability_mass_complete: bool,
        truncated: bool,
    },
    Sampled {
        truncated: bool,
    },
    /// Unsupported and lazy outcomes are not scoreable advice lines.
    Unsupported,
}

impl SimulationEvidence {
    pub fn from_outcome(outcome: &SimOutcome) -> Self {
        let meta_truncated = outcome.meta.truncation.is_some();
        match &outcome.outcome {
            OutcomeSet::Deterministic(_) => Self::Deterministic {
                truncated: meta_truncated,
            },
            OutcomeSet::Exact(branches) if branches.is_empty() => Self::Unsupported,
            OutcomeSet::Exact(branches) => {
                let probability_mass = branches
                    .iter()
                    .map(|branch| branch.probability)
                    .sum::<f64>();
                Self::Exact {
                    probability_mass_complete: probability_mass.is_finite()
                        && (probability_mass - 1.0).abs() <= 1e-9,
                    truncated: meta_truncated,
                }
            }
            OutcomeSet::Sampled(sampled) if sampled.samples.is_empty() => Self::Unsupported,
            OutcomeSet::Sampled(sampled) => Self::Sampled {
                truncated: meta_truncated
                    || sampled.total_branches.is_some_and(|total| {
                        total > u64::try_from(sampled.samples.len()).unwrap_or(u64::MAX)
                    }),
            },
            OutcomeSet::Lazy(_) | OutcomeSet::Unsupported(_) => Self::Unsupported,
        }
    }
}

/// Evidence accumulated for one candidate line.
pub struct LineEvidence<'a> {
    pub rule_coverage: &'a RuleCoverageReport,
    pub action_origins: &'a [ActionOrigin],
    pub simulations: &'a [SimulationEvidence],
}

/// Conservatively combine independent confidence caps for one line.
///
/// Empty or unsupported evidence is not a low-ranked line: it is excluded as
/// unreliable, so an engine with no remaining lines reports NoReliableActions.
pub fn line_confidence(
    policy: &ConfidencePolicy,
    evidence: &LineEvidence<'_>,
) -> Result<ConfidenceBreakdown, PlannerError> {
    if evidence.action_origins.is_empty()
        || evidence.simulations.is_empty()
        || evidence
            .simulations
            .contains(&SimulationEvidence::Unsupported)
        || evidence
            .rule_coverage
            .items()
            .iter()
            .any(|item| matches!(item.status, None | Some(RuleStatus::Unknown)))
    {
        return Err(UnavailableReason::NoReliableActions.into());
    }

    let mut factors = Vec::new();
    for item in evidence.rule_coverage.items() {
        let mut rule = policy.rule_factor(item.status, false);
        rule.confidence = rule.confidence.min(item.confidence);
        factors.push(rule);
        if item.runtime_unknown {
            let mut runtime = policy.rule_factor(item.status, true);
            runtime.confidence = runtime.confidence.min(item.confidence);
            factors.push(runtime);
        }
    }

    for origin in evidence.action_origins {
        factors.push(match origin {
            ActionOrigin::Server => ConfidenceFactor {
                reason: ConfidenceReason::ServerLegalAction,
                confidence: policy.server_legal_action,
            },
            ActionOrigin::Simulated => ConfidenceFactor {
                reason: ConfidenceReason::SimulatedLegalAction,
                confidence: policy.simulated_legal_action,
            },
        });
    }

    for simulation in evidence.simulations {
        match simulation {
            SimulationEvidence::Deterministic { truncated } => {
                factors.push(ConfidenceFactor {
                    reason: ConfidenceReason::DeterministicSimulation,
                    confidence: policy.deterministic_simulation,
                });
                add_truncation_factor(policy, &mut factors, *truncated);
            }
            SimulationEvidence::Exact {
                probability_mass_complete,
                truncated,
            } => {
                factors.push(ConfidenceFactor {
                    reason: ConfidenceReason::ExactSimulation,
                    confidence: policy.exact_simulation,
                });
                if !probability_mass_complete {
                    factors.push(ConfidenceFactor {
                        reason: ConfidenceReason::ProbabilityMassIncomplete,
                        confidence: policy.sampled_simulation,
                    });
                }
                add_truncation_factor(policy, &mut factors, *truncated);
            }
            SimulationEvidence::Sampled { truncated } => {
                factors.push(ConfidenceFactor {
                    reason: ConfidenceReason::SampledSimulation,
                    confidence: policy.sampled_simulation,
                });
                add_truncation_factor(policy, &mut factors, *truncated);
            }
            SimulationEvidence::Unsupported => unreachable!("filtered above"),
        }
    }

    Ok(ConfidenceBreakdown::new(factors))
}

/// Apply the independently measured root-action comparison completeness cap.
pub fn result_confidence(
    policy: &ConfidencePolicy,
    top_line: &ConfidenceBreakdown,
    completeness: &SearchCompleteness,
) -> ConfidenceBreakdown {
    policy.result_breakdown(top_line, completeness)
}

fn add_truncation_factor(
    policy: &ConfidencePolicy,
    factors: &mut Vec<ConfidenceFactor>,
    truncated: bool,
) {
    if truncated {
        factors.push(ConfidenceFactor {
            reason: ConfidenceReason::BudgetTruncated,
            confidence: policy.sampled_simulation,
        });
    }
}
