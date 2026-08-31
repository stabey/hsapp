//! Shared chance, evidence, and fixed-point aggregation primitives.

use std::collections::BTreeMap;

use hs_advisor_api::ActionKind;
use hs_sim::{ChanceKind, OutcomeSet, RuleUse, SimGameState, SimOutcome, UnsupportedReason};
use hs_types::{CardId, EntityId, PlayerId, RuleStatus};
use serde::Serialize;

use crate::legal_actions::ActionTemplate;

const PROBABILITY_ULPS_PER_TERM: f64 = 32.0;

/// Fixed denominator used when chance probabilities contribute to ranking.
/// Input probabilities remain simulator evidence; only score aggregation is
/// quantized, with the rounding rule centralized here.
pub const PROBABILITY_SCALE: i128 = 1_000_000_000_000_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct WeightedState {
    pub(crate) state: SimGameState,
    pub(crate) probability: f64,
}

/// Stable proof/evidence accumulator shared by lethal and Beam paths.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PathEvidence {
    exact: bool,
    had_exact_chance: bool,
    truncated: bool,
    sample_count: u64,
    rule_uses: Vec<RuleUse>,
    unknown_rule_cards: Vec<CardId>,
    pool_snapshot_hashes: Vec<String>,
}

impl PathEvidence {
    pub(crate) fn root() -> Self {
        Self {
            exact: true,
            had_exact_chance: false,
            truncated: false,
            sample_count: 0,
            rule_uses: Vec::new(),
            unknown_rule_cards: Vec::new(),
            pool_snapshot_hashes: Vec::new(),
        }
    }

    pub const fn exact(&self) -> bool {
        self.exact
    }

    /// Whether an exact random outcome occurred anywhere on this path, even
    /// when all of its branches later converged to one canonical state.
    pub const fn had_exact_chance(&self) -> bool {
        self.had_exact_chance
    }

    pub fn trusted_rules(&self) -> bool {
        self.rule_uses.iter().all(|rule_use| {
            rule_use.status == RuleStatus::Verified
                && rule_use.confidence.is_finite()
                && rule_use.confidence == 1.0
        })
    }

    pub fn runtime_unknown(&self) -> bool {
        !self.unknown_rule_cards.is_empty()
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    pub const fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn rule_uses(&self) -> &[RuleUse] {
        &self.rule_uses
    }

    pub fn unknown_rule_cards(&self) -> &[CardId] {
        &self.unknown_rule_cards
    }

    pub fn pool_snapshot_hashes(&self) -> &[String] {
        &self.pool_snapshot_hashes
    }

    pub(crate) fn observe(&mut self, outcome: &SimOutcome) {
        self.truncated |= outcome.meta.truncation.is_some();
        merge_rule_uses(&mut self.rule_uses, &outcome.rule_uses);
        self.unknown_rule_cards
            .extend(outcome.unknown_rule_cards.iter().cloned());
        self.unknown_rule_cards.sort();
        self.unknown_rule_cards.dedup();
        self.pool_snapshot_hashes
            .extend(outcome.meta.pool_snapshot_hashes.iter().cloned());
        self.pool_snapshot_hashes.sort();
        self.pool_snapshot_hashes.dedup();
    }

    pub(crate) fn mark_sampled(&mut self, sample_count: usize) {
        self.exact = false;
        self.truncated = true;
        self.sample_count = self
            .sample_count
            .saturating_add(u64::try_from(sample_count).unwrap_or(u64::MAX));
    }

    pub(crate) fn mark_exact_chance(&mut self) {
        self.had_exact_chance = true;
    }

    pub(crate) fn merge_from(&mut self, other: &Self) {
        self.exact &= other.exact;
        self.had_exact_chance |= other.had_exact_chance;
        self.truncated |= other.truncated;
        self.sample_count = self.sample_count.saturating_add(other.sample_count);
        merge_rule_uses(&mut self.rule_uses, &other.rule_uses);
        self.unknown_rule_cards
            .extend(other.unknown_rule_cards.iter().cloned());
        self.unknown_rule_cards.sort();
        self.unknown_rule_cards.dedup();
        self.pool_snapshot_hashes
            .extend(other.pool_snapshot_hashes.iter().cloned());
        self.pool_snapshot_hashes.sort();
        self.pool_snapshot_hashes.dedup();
    }

    pub(crate) fn proves_exact(&self) -> bool {
        self.exact && self.trusted_rules() && !self.runtime_unknown() && !self.truncated
    }

    pub(crate) fn supports_sampled_estimate(&self) -> bool {
        !self.exact && self.sample_count > 0 && self.trusted_rules() && !self.runtime_unknown()
    }
}

fn merge_rule_uses(current: &mut Vec<RuleUse>, incoming: &[RuleUse]) {
    let mut by_key = BTreeMap::<(CardId, String), RuleUse>::new();
    for rule_use in current.drain(..).chain(incoming.iter().cloned()) {
        let key = (rule_use.card_id.clone(), rule_use.rule_version.clone());
        let normalized = RuleUse {
            confidence: conservative_rule_confidence(rule_use.confidence),
            ..rule_use
        };
        by_key
            .entry(key)
            .and_modify(|existing| merge_rule_use(existing, &normalized))
            .or_insert(normalized);
    }
    *current = by_key.into_values().collect();
}

fn merge_rule_use(current: &mut RuleUse, incoming: &RuleUse) {
    if rule_status_strength(incoming.status) < rule_status_strength(current.status) {
        current.status = incoming.status;
    }
    current.confidence = current.confidence.min(incoming.confidence);
}

fn conservative_rule_confidence(confidence: f32) -> f32 {
    if confidence.is_finite() && (0.0..=1.0).contains(&confidence) {
        confidence
    } else {
        0.0
    }
}

const fn rule_status_strength(status: RuleStatus) -> u8 {
    match status {
        RuleStatus::Unknown => 1,
        RuleStatus::Stale => 2,
        RuleStatus::Partial => 3,
        RuleStatus::Inferred => 4,
        RuleStatus::Verified => 5,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OutcomeExpansion {
    Expanded {
        states: Vec<WeightedState>,
        node_count: u64,
        sampled: bool,
    },
    Boundary(SearchBoundary),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SearchBoundary {
    PlayerChoice,
    MissingEvidence,
    IncompleteProbabilityMass,
    Lazy,
    Unsupported(UnsupportedReason),
}

/// Validate chance provenance and preserve an entire outcome group.  The
/// caller decides whether its fixed node budget can atomically accept the
/// returned `node_count`; it must never retain only a prefix of `states`.
pub(crate) fn expand_outcome(
    outcome: SimOutcome,
    parent_probability: f64,
    evidence: &mut PathEvidence,
) -> OutcomeExpansion {
    evidence.observe(&outcome);
    let chance_kind = outcome.meta.chance_kind;
    if chance_kind == Some(ChanceKind::PlayerChoice) {
        return OutcomeExpansion::Boundary(SearchBoundary::PlayerChoice);
    }

    match outcome.outcome {
        OutcomeSet::Deterministic(state) => {
            if chance_kind.is_some() {
                return OutcomeExpansion::Boundary(SearchBoundary::MissingEvidence);
            }
            OutcomeExpansion::Expanded {
                states: vec![WeightedState {
                    state,
                    probability: parent_probability,
                }],
                node_count: 1,
                sampled: false,
            }
        }
        OutcomeSet::Exact(branches) => {
            if chance_kind != Some(ChanceKind::Random) {
                return OutcomeExpansion::Boundary(SearchBoundary::MissingEvidence);
            }
            if !exact_mass_complete(&branches) {
                return OutcomeExpansion::Boundary(SearchBoundary::IncompleteProbabilityMass);
            }
            evidence.mark_exact_chance();
            let node_count = u64::try_from(branches.len()).unwrap_or(u64::MAX);
            OutcomeExpansion::Expanded {
                states: branches
                    .into_iter()
                    .map(|branch| WeightedState {
                        state: branch.state,
                        probability: parent_probability * branch.probability,
                    })
                    .collect(),
                node_count,
                sampled: false,
            }
        }
        OutcomeSet::Sampled(sampled) => {
            if chance_kind != Some(ChanceKind::Random) {
                return OutcomeExpansion::Boundary(SearchBoundary::MissingEvidence);
            }
            if sampled.samples.is_empty() || !exact_mass_complete(&sampled.samples) {
                return OutcomeExpansion::Boundary(SearchBoundary::IncompleteProbabilityMass);
            }
            evidence.mark_sampled(sampled.samples.len());
            let node_count = u64::try_from(sampled.samples.len()).unwrap_or(u64::MAX);
            OutcomeExpansion::Expanded {
                states: sampled
                    .samples
                    .into_iter()
                    .map(|sample| WeightedState {
                        state: sample.state,
                        probability: parent_probability * sample.probability,
                    })
                    .collect(),
                node_count,
                sampled: true,
            }
        }
        OutcomeSet::Lazy(_) => OutcomeExpansion::Boundary(SearchBoundary::Lazy),
        OutcomeSet::Unsupported(reason) => {
            OutcomeExpansion::Boundary(SearchBoundary::Unsupported(reason))
        }
    }
}

pub(crate) fn weighted_probability_mass_complete(states: &[WeightedState]) -> bool {
    !states.is_empty()
        && states.iter().all(|state| {
            state.probability.is_finite() && state.probability > 0.0 && state.probability <= 1.0
        })
        && probability_sum_complete(
            states.iter().map(|state| state.probability).sum::<f64>(),
            states.len(),
        )
}

pub(crate) fn templates_after_action(
    templates: &[ActionTemplate],
    kind: ActionKind,
    source: Option<EntityId>,
) -> Vec<ActionTemplate> {
    if kind == ActionKind::EndTurn {
        return Vec::new();
    }
    templates
        .iter()
        .filter(|template| template.kind() != kind || template.source() != source)
        .cloned()
        .collect()
}

pub(crate) fn template_entities_present(
    template: &ActionTemplate,
    states: &[WeightedState],
    opponent: PlayerId,
) -> bool {
    states.iter().all(|weighted| {
        if effective_life(&weighted.state, opponent).is_some_and(|life| life <= 0) {
            return true;
        }
        template
            .source()
            .is_none_or(|source| weighted.state.entity(source).is_some())
            && template
                .target()
                .is_none_or(|target| weighted.state.entity(target).is_some())
    })
}

pub(crate) fn template_relevant(
    template: &ActionTemplate,
    states: &[WeightedState],
    opponent: PlayerId,
) -> bool {
    template_entities_present(template, states, opponent)
        && !common_positions(template, states, opponent).is_empty()
}

pub(crate) fn common_positions(
    template: &ActionTemplate,
    states: &[WeightedState],
    opponent: PlayerId,
) -> Vec<Option<u8>> {
    let mut live = states.iter().filter(|weighted| {
        effective_life(&weighted.state, opponent).is_some_and(|life| life > 0)
            && !weighted.state.turn_ended
    });
    let Some(first) = live.next() else {
        return Vec::new();
    };
    let mut positions = template.positions_for(&first.state);
    for weighted in live {
        let available = template.positions_for(&weighted.state);
        positions.retain(|position| available.contains(position));
    }
    positions
}

pub(crate) fn opponent_player(state: &SimGameState) -> Option<PlayerId> {
    let friendly = state.friendly_player?;
    let mut opponents = state
        .players
        .keys()
        .copied()
        .filter(|player| *player != friendly);
    let opponent = opponents.next()?;
    if opponents.next().is_some() {
        return None;
    }
    Some(opponent)
}

pub(crate) fn effective_life(state: &SimGameState, player: PlayerId) -> Option<i32> {
    let hero = state.hero_entity(player)?;
    if !state.entity(hero)?.action.health_known {
        return None;
    }
    state
        .player(player)
        .map(|facts| facts.hero_health.saturating_add(facts.armor))
}

pub(crate) fn exact_mass_complete(branches: &[hs_sim::WeightedOutcome]) -> bool {
    !branches.is_empty()
        && branches.iter().all(|branch| {
            branch.probability.is_finite() && branch.probability > 0.0 && branch.probability <= 1.0
        })
        && probability_sum_complete(
            branches
                .iter()
                .map(|branch| branch.probability)
                .sum::<f64>(),
            branches.len(),
        )
}

fn probability_sum_complete(sum: f64, term_count: usize) -> bool {
    let tolerance = f64::EPSILON * PROBABILITY_ULPS_PER_TERM * (term_count.max(1) as f64);
    sum.is_finite() && (sum - 1.0).abs() <= tolerance
}

/// Convert a finite probability to the fixed ranking scale using round-half
/// away from zero (all valid probabilities are non-negative).
pub(crate) fn probability_units(probability: f64) -> Option<i128> {
    if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
        return None;
    }
    let scaled = probability * (PROBABILITY_SCALE as f64);
    Some(scaled.round() as i128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_sim::{ReproMeta, RuleTrace, SampleMethod, SampledOutcomeSet, WeightedOutcome};

    fn outcome(outcome: OutcomeSet, chance_kind: Option<ChanceKind>) -> SimOutcome {
        SimOutcome {
            outcome,
            trace: RuleTrace::default(),
            meta: ReproMeta {
                seed: 7,
                branch_budget: 4,
                method: SampleMethod::Exact,
                truncation: None,
                chance_kind,
                rule_version: "test".into(),
                pool_snapshot_hashes: Vec::new(),
            },
            unknown_rule_cards: Vec::new(),
            rule_uses: Vec::new(),
        }
    }

    fn weighted(probability: f64) -> WeightedOutcome {
        WeightedOutcome {
            probability,
            state: SimGameState::default(),
            note: None,
        }
    }

    #[test]
    fn chance_provenance_is_explicit_and_player_choice_stops() {
        let mut evidence = PathEvidence::root();
        assert!(matches!(
            expand_outcome(
                outcome(OutcomeSet::Deterministic(SimGameState::default()), None,),
                1.0,
                &mut evidence,
            ),
            OutcomeExpansion::Expanded {
                node_count: 1,
                sampled: false,
                ..
            }
        ));

        assert!(matches!(
            expand_outcome(
                outcome(
                    OutcomeSet::Exact(vec![weighted(1.0)]),
                    Some(ChanceKind::PlayerChoice),
                ),
                1.0,
                &mut evidence,
            ),
            OutcomeExpansion::Boundary(SearchBoundary::PlayerChoice)
        ));
        assert!(matches!(
            expand_outcome(
                outcome(OutcomeSet::Exact(vec![weighted(1.0)]), None),
                1.0,
                &mut evidence,
            ),
            OutcomeExpansion::Boundary(SearchBoundary::MissingEvidence)
        ));

        let mut exact_evidence = PathEvidence::root();
        assert!(matches!(
            expand_outcome(
                outcome(
                    OutcomeSet::Exact(vec![weighted(0.5), weighted(0.5)]),
                    Some(ChanceKind::Random),
                ),
                1.0,
                &mut exact_evidence,
            ),
            OutcomeExpansion::Expanded { .. }
        ));
        assert!(exact_evidence.had_exact_chance());
    }

    #[test]
    fn sampled_group_remains_atomic_and_marks_estimation() {
        let mut evidence = PathEvidence::root();
        let expansion = expand_outcome(
            outcome(
                OutcomeSet::Sampled(SampledOutcomeSet {
                    samples: vec![weighted(0.5), weighted(0.5)],
                    method: SampleMethod::MonteCarlo,
                    total_branches: Some(20),
                }),
                Some(ChanceKind::Random),
            ),
            1.0,
            &mut evidence,
        );
        assert!(matches!(
            expansion,
            OutcomeExpansion::Expanded {
                node_count: 2,
                sampled: true,
                ..
            }
        ));
        assert!(!evidence.exact());
        assert!(evidence.truncated());
        assert_eq!(evidence.sample_count(), 2);
    }

    #[test]
    fn probability_mass_tolerance_rejects_material_gaps() {
        assert!(exact_mass_complete(&[weighted(0.5), weighted(0.5)]));
        assert!(exact_mass_complete(&[
            weighted(0.5),
            weighted(0.5 - f64::EPSILON)
        ]));
        assert!(!exact_mass_complete(&[
            weighted(0.5),
            weighted(0.5 - 5e-10)
        ]));
        assert_eq!(probability_units(0.5), Some(PROBABILITY_SCALE / 2));
        assert_eq!(probability_units(f64::NAN), None);
    }

    #[test]
    fn evidence_union_keeps_versions_and_weakest_facts_in_stable_order() {
        let verified = RuleUse {
            card_id: CardId::new("CARD"),
            status: RuleStatus::Verified,
            confidence: 1.0,
            rule_version: "v1".into(),
        };
        let partial = RuleUse {
            status: RuleStatus::Partial,
            confidence: 0.6,
            ..verified.clone()
        };
        let other_version = RuleUse {
            rule_version: "v2".into(),
            ..verified.clone()
        };
        let mut evidence = PathEvidence::root();
        let mut first = outcome(OutcomeSet::Deterministic(SimGameState::default()), None);
        first.rule_uses = vec![verified];
        first.unknown_rule_cards = vec![CardId::new("Z"), CardId::new("A")];
        first.meta.pool_snapshot_hashes = vec!["z".into(), "a".into()];
        evidence.observe(&first);

        let mut second = outcome(OutcomeSet::Deterministic(SimGameState::default()), None);
        second.rule_uses = vec![other_version, partial];
        second.unknown_rule_cards = vec![CardId::new("A")];
        second.meta.pool_snapshot_hashes = vec!["a".into()];
        evidence.observe(&second);

        assert_eq!(evidence.rule_uses().len(), 2);
        assert_eq!(evidence.rule_uses()[0].rule_version, "v1");
        assert_eq!(evidence.rule_uses()[0].status, RuleStatus::Partial);
        assert_eq!(evidence.rule_uses()[0].confidence, 0.6);
        assert_eq!(evidence.rule_uses()[1].rule_version, "v2");
        assert_eq!(
            evidence.unknown_rule_cards(),
            &[CardId::new("A"), CardId::new("Z")]
        );
        assert_eq!(evidence.pool_snapshot_hashes(), ["a", "z"]);
    }
}
