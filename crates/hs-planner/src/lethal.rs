//! Deterministic, conservative one-turn lethal search.
//!
//! Current server actions are the only roots. Continuations are projections
//! of the same Options packet and are always published as simulated actions;
//! the simulator must prove each continuation usable in the derived state.

use std::cmp::Ordering;
use std::collections::{BTreeSet, VecDeque};

use hs_advisor_api::{Confidence, LegalAction, LethalAssessment, PlannedAction};
use hs_sim::{apply_action, SimAction, SimGameState, SimRequest, UnsupportedReason};
use hs_types::{PlayerId, RuleStatus};

use crate::legal_actions::ActionTemplate;
use crate::node::{CanonicalActionKey, CanonicalActionPath};
use crate::search::{
    common_positions, effective_life, expand_outcome, opponent_player, template_entities_present,
    template_relevant, templates_after_action, weighted_probability_mass_complete,
    OutcomeExpansion, PathEvidence, SearchBoundary, WeightedState,
};
use crate::sim_action::template_action_to_sim;
use crate::{legal_action_to_sim, PlannerError, PlannerRequest};

/// Why the fixed search could not prove an exhaustive exact result.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LethalStopReason {
    MissingOpponent,
    CandidateBudget,
    NodeBudget,
    MaxDepth,
    PlayerChoice,
    SampledSimulation,
    LazySimulation,
    UnsupportedSimulation,
    MissingEvidence,
    IncompleteProbabilityMass,
}

/// Stable counters for budget accounting and regression tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LethalSearchStats {
    roots_evaluated: u64,
    candidates_evaluated: u64,
    nodes_visited: u64,
    simulation_branches_examined: u64,
    max_depth_reached: u16,
}

impl LethalSearchStats {
    pub const fn roots_evaluated(self) -> u64 {
        self.roots_evaluated
    }

    pub const fn candidates_evaluated(self) -> u64 {
        self.candidates_evaluated
    }

    pub const fn nodes_visited(self) -> u64 {
        self.nodes_visited
    }

    pub const fn simulation_branches_examined(self) -> u64 {
        self.simulation_branches_examined
    }

    pub const fn max_depth_reached(self) -> u16 {
        self.max_depth_reached
    }
}

/// One stable route. Only `first_action` is executable against current
/// Options; every entry in `follow_up` is a simulator continuation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LethalRoute {
    first_action: LegalAction,
    follow_up: Vec<PlannedAction>,
    path_seed: u64,
}

impl LethalRoute {
    pub const fn first_action(&self) -> LegalAction {
        self.first_action
    }

    pub fn follow_up(&self) -> &[PlannedAction] {
        &self.follow_up
    }

    pub const fn path_seed(&self) -> u64 {
        self.path_seed
    }

    pub fn action_count(&self) -> usize {
        self.follow_up.len().saturating_add(1)
    }
}

/// Result of one fixed-budget lethal-first pass.
#[derive(Clone, Debug, PartialEq)]
pub struct LethalSearchResult {
    assessment: LethalAssessment,
    route: Option<LethalRoute>,
    evidence: Option<LethalEvidence>,
    selected_path: Option<CanonicalActionPath>,
    selected_states: Option<Vec<WeightedState>>,
    aggregate_evidence: PathEvidence,
    evaluated_roots: Vec<LegalAction>,
    root_evaluations: Vec<LethalRootEvaluation>,
    evaluated_paths: Vec<CanonicalActionPath>,
    path_expansions: Vec<LethalPathExpansion>,
    complete: bool,
    stats: LethalSearchStats,
    stop_reasons: Vec<LethalStopReason>,
}

impl LethalSearchResult {
    pub const fn assessment(&self) -> LethalAssessment {
        self.assessment
    }

    pub fn route(&self) -> Option<&LethalRoute> {
        self.route.as_ref()
    }

    pub fn evidence(&self) -> Option<&LethalEvidence> {
        self.evidence.as_ref()
    }

    pub(crate) fn selected_path(&self) -> Option<&CanonicalActionPath> {
        self.selected_path.as_ref()
    }

    pub(crate) fn selected_states(&self) -> Option<&[WeightedState]> {
        self.selected_states.as_deref()
    }

    pub(crate) fn aggregate_evidence(&self) -> &PathEvidence {
        &self.aggregate_evidence
    }

    pub(crate) fn evaluated_roots(&self) -> &[LegalAction] {
        &self.evaluated_roots
    }

    pub(crate) fn root_evaluations(&self) -> &[LethalRootEvaluation] {
        &self.root_evaluations
    }

    pub(crate) fn evaluated_paths(&self) -> &[CanonicalActionPath] {
        &self.evaluated_paths
    }

    pub(crate) fn path_expansions(&self) -> &[LethalPathExpansion] {
        &self.path_expansions
    }

    /// Whether the bounded search exhausted every conservatively projectable
    /// route. A route-local Guaranteed proof remains valid when this is false;
    /// incompleteness prevents `None` and means a better route may be unseen.
    pub const fn complete(&self) -> bool {
        self.complete
    }

    pub const fn stats(&self) -> LethalSearchStats {
        self.stats
    }

    /// Highest-priority reason, for compact UI/Beam diagnostics.
    pub fn stop_reason(&self) -> Option<LethalStopReason> {
        self.stop_reasons.first().copied()
    }

    pub fn stop_reasons(&self) -> &[LethalStopReason] {
        &self.stop_reasons
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LethalRootEvaluation {
    Expansion {
        root: LegalAction,
        path: CanonicalActionPath,
        states: Vec<WeightedState>,
        evidence: PathEvidence,
    },
    Boundary {
        root: LegalAction,
        reason: LethalStopReason,
    },
}

/// Every successfully committed chance group already paid for by the
/// lethal-first pass. Beam replays these nodes through its own layer pruning
/// without simulating or charging the shared ledger a second time.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LethalPathExpansion {
    pub(crate) first_action: LegalAction,
    pub(crate) path: CanonicalActionPath,
    pub(crate) states: Vec<WeightedState>,
    pub(crate) evidence: PathEvidence,
}

type Scenario = WeightedState;

/// Conservative proof summary for the selected lethal route.
pub type LethalEvidence = PathEvidence;

#[derive(Clone)]
struct SearchNode {
    scenarios: Vec<Scenario>,
    remaining: Vec<ActionTemplate>,
    first_action: LegalAction,
    path: CanonicalActionPath,
    evidence: LethalEvidence,
}

/// A search state is only equivalent when its chance distribution, remaining
/// same-Options actions, and proof quality are all equivalent. The chosen path
/// is deliberately excluded: stable breadth-first insertion keeps the shortest
/// lexicographically first representative of converging action orders.
#[derive(Clone, PartialEq)]
struct SearchNodeKey {
    scenarios: Vec<Scenario>,
    remaining: Vec<ActionTemplate>,
    evidence: LethalEvidence,
}

impl From<&SearchNode> for SearchNodeKey {
    fn from(node: &SearchNode) -> Self {
        Self {
            scenarios: node.scenarios.clone(),
            remaining: node.remaining.clone(),
            evidence: node.evidence.clone(),
        }
    }
}

#[derive(Default)]
struct SearchFrontier {
    queue: VecDeque<SearchNode>,
    seen: Vec<SearchNodeKey>,
}

impl SearchFrontier {
    fn push(&mut self, node: SearchNode) {
        let key = SearchNodeKey::from(&node);
        if self.seen.contains(&key) {
            return;
        }
        self.seen.push(key);
        self.queue.push_back(node);
    }

    fn pop_front(&mut self) -> Option<SearchNode> {
        self.queue.pop_front()
    }
}

#[derive(Clone)]
struct RankedCandidate {
    assessment: LethalAssessment,
    route: LethalRoute,
    evidence: LethalEvidence,
    probability: f64,
    stable_path: CanonicalActionPath,
    states: Vec<WeightedState>,
}

impl RankedCandidate {
    fn is_better_than(&self, other: &Self) -> bool {
        match assessment_rank(self.assessment).cmp(&assessment_rank(other.assessment)) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => match self.route.action_count().cmp(&other.route.action_count()) {
                Ordering::Less => true,
                Ordering::Greater => false,
                Ordering::Equal => match self
                    .probability
                    .partial_cmp(&other.probability)
                    .unwrap_or(Ordering::Equal)
                {
                    Ordering::Greater => true,
                    Ordering::Less => false,
                    Ordering::Equal => self.stable_path < other.stable_path,
                },
            },
        }
    }
}

fn assessment_rank(assessment: LethalAssessment) -> u8 {
    match assessment {
        LethalAssessment::Guaranteed => 4,
        LethalAssessment::Probabilistic { .. } => 3,
        LethalAssessment::Estimated { .. } => 2,
        LethalAssessment::Unknown => 1,
        LethalAssessment::None => 0,
    }
}

/// Search the current request for a conservative one-turn lethal route.
///
/// The implementation intentionally lives in `hs-planner`: no log IO, wall
/// clock, platform state, or reconstructed server action can enter this pass.
pub fn search_lethal(request: &PlannerRequest) -> Result<LethalSearchResult, PlannerError> {
    search_lethal_impl(request)
}

fn search_lethal_impl(request: &PlannerRequest) -> Result<LethalSearchResult, PlannerError> {
    let config = request.search_config();
    let initial = SimGameState::from_observed(request.observed_state());
    let Some(opponent) = opponent_player(&initial) else {
        return Ok(LethalSearchResult {
            assessment: LethalAssessment::Unknown,
            route: None,
            evidence: None,
            selected_path: None,
            selected_states: None,
            aggregate_evidence: PathEvidence::root(),
            evaluated_roots: Vec::new(),
            root_evaluations: Vec::new(),
            evaluated_paths: Vec::new(),
            path_expansions: Vec::new(),
            complete: false,
            stats: LethalSearchStats::default(),
            stop_reasons: vec![LethalStopReason::MissingOpponent],
        });
    };
    if effective_life(&initial, opponent).is_none() {
        return Ok(LethalSearchResult {
            assessment: LethalAssessment::Unknown,
            route: None,
            evidence: None,
            selected_path: None,
            selected_states: None,
            aggregate_evidence: PathEvidence::root(),
            evaluated_roots: Vec::new(),
            root_evaluations: Vec::new(),
            evaluated_paths: Vec::new(),
            path_expansions: Vec::new(),
            complete: false,
            stats: LethalSearchStats::default(),
            stop_reasons: vec![LethalStopReason::MissingOpponent],
        });
    }

    let roots = &request.legal_actions().actions;
    if config.max_depth == 0 {
        return Ok(LethalSearchResult {
            assessment: LethalAssessment::Unknown,
            route: None,
            evidence: None,
            selected_path: None,
            selected_states: None,
            aggregate_evidence: PathEvidence::root(),
            evaluated_roots: Vec::new(),
            root_evaluations: Vec::new(),
            evaluated_paths: Vec::new(),
            path_expansions: Vec::new(),
            complete: false,
            stats: LethalSearchStats::default(),
            stop_reasons: vec![LethalStopReason::MaxDepth],
        });
    }

    let templates = request.action_templates().templates().to_vec();
    let mut frontier = SearchFrontier::default();
    let mut stops = BTreeSet::<LethalStopReason>::new();
    if request.action_templates().has_projection_gaps() {
        stops.insert(LethalStopReason::MissingEvidence);
    }
    let mut stats = LethalSearchStats::default();
    let mut lethal_candidates = Vec::<RankedCandidate>::new();
    let mut aggregate_evidence = PathEvidence::root();
    let mut evaluated_roots = Vec::new();
    let mut root_evaluations = Vec::new();
    let mut evaluated_paths = Vec::new();
    let mut path_expansions = Vec::new();
    let mut exhausted_budget = false;

    // Evaluate every server-backed root before any depth-2 continuation. This
    // gives stable breadth-first behavior even when budgets stop the search.
    for root in roots.iter().copied() {
        if stats.candidates_evaluated >= config.candidate_budget {
            stops.insert(LethalStopReason::CandidateBudget);
            exhausted_budget = true;
            break;
        }
        stats.candidates_evaluated = stats.candidates_evaluated.saturating_add(1);
        stats.roots_evaluated = stats.roots_evaluated.saturating_add(1);
        evaluated_roots.push(root);
        let path = CanonicalActionPath::from_root(root);
        evaluated_paths.push(path.clone());
        let Some(sim_action) = legal_action_to_sim(&root).ok() else {
            stops.insert(LethalStopReason::UnsupportedSimulation);
            root_evaluations.push(LethalRootEvaluation::Boundary {
                root,
                reason: LethalStopReason::UnsupportedSimulation,
            });
            continue;
        };
        let parent = SearchNode {
            scenarios: vec![Scenario {
                state: initial.clone(),
                probability: 1.0,
            }],
            remaining: templates_after_action(&templates, root.kind, root.source),
            first_action: root,
            path: CanonicalActionPath::default(),
            evidence: LethalEvidence::root(),
        };
        let evaluation = evaluate_candidate(
            request,
            &parent,
            sim_action,
            &path,
            opponent,
            true,
            &mut stats,
            &mut stops,
            &mut aggregate_evidence,
        )?;
        match evaluation {
            CandidateEvaluation::Expansion(expansion) => {
                root_evaluations.push(LethalRootEvaluation::Expansion {
                    root,
                    path: path.clone(),
                    states: expansion.scenarios.clone(),
                    evidence: expansion.evidence.clone(),
                });
                path_expansions.push(LethalPathExpansion {
                    first_action: root,
                    path: path.clone(),
                    states: expansion.scenarios.clone(),
                    evidence: expansion.evidence.clone(),
                });
                accept_expansion(
                    request,
                    expansion,
                    root,
                    path,
                    opponent,
                    &mut frontier,
                    &mut lethal_candidates,
                    &mut stops,
                );
            }
            CandidateEvaluation::Skipped(reason) => {
                root_evaluations.push(LethalRootEvaluation::Boundary { root, reason });
            }
            CandidateEvaluation::BudgetExhausted => {
                root_evaluations.push(LethalRootEvaluation::Boundary {
                    root,
                    reason: LethalStopReason::NodeBudget,
                });
                // An exact chance group is atomic: if it does not fit, reject
                // that group without spending nodes. Cheaper later roots may
                // still fit the remaining global node budget.
                if stats.nodes_visited >= config.node_budget {
                    exhausted_budget = true;
                    break;
                }
            }
        }
    }

    while !exhausted_budget {
        let Some(node) = frontier.pop_front() else {
            break;
        };
        let depth = u16::try_from(node.path.len()).unwrap_or(u16::MAX);
        if depth >= config.max_depth {
            if node.remaining.iter().any(|template| {
                template_relevant(template, &node.scenarios, opponent)
                    && template.has_only_recheckable_gates()
            }) {
                stops.insert(LethalStopReason::MaxDepth);
            }
            if node.remaining.iter().any(|template| {
                template_relevant(template, &node.scenarios, opponent)
                    && !template.has_only_recheckable_gates()
            }) {
                stops.insert(LethalStopReason::MissingEvidence);
            }
            continue;
        }

        for template in &node.remaining {
            if !template_entities_present(template, &node.scenarios, opponent) {
                continue;
            }
            if !template.has_only_recheckable_gates() {
                stops.insert(LethalStopReason::MissingEvidence);
                continue;
            }

            let positions = common_positions(template, &node.scenarios, opponent);
            for position in positions {
                if stats.candidates_evaluated >= config.candidate_budget {
                    stops.insert(LethalStopReason::CandidateBudget);
                    exhausted_budget = true;
                    break;
                }
                stats.candidates_evaluated = stats.candidates_evaluated.saturating_add(1);
                let action = CanonicalActionKey::from_template(template, position);
                let mut path = node.path.clone();
                path.push(action);
                evaluated_paths.push(path.clone());
                let Ok(sim_action) = template_action_to_sim(template, position) else {
                    stops.insert(LethalStopReason::UnsupportedSimulation);
                    continue;
                };

                let evaluation = evaluate_candidate(
                    request,
                    &node,
                    sim_action,
                    &path,
                    opponent,
                    false,
                    &mut stats,
                    &mut stops,
                    &mut aggregate_evidence,
                )?;
                match evaluation {
                    CandidateEvaluation::Expansion(expansion) => {
                        path_expansions.push(LethalPathExpansion {
                            first_action: node.first_action,
                            path: path.clone(),
                            states: expansion.scenarios.clone(),
                            evidence: expansion.evidence.clone(),
                        });
                        let remaining = templates_after_action(
                            &node.remaining,
                            template.kind(),
                            template.source(),
                        );
                        accept_expansion_with_remaining(
                            request,
                            expansion,
                            node.first_action,
                            path,
                            remaining,
                            opponent,
                            &mut frontier,
                            &mut lethal_candidates,
                            &mut stops,
                        );
                    }
                    CandidateEvaluation::Skipped(_) => {}
                    CandidateEvaluation::BudgetExhausted => {
                        // Keep enumerating stable later actions while there is
                        // still node capacity; only the oversized chance group
                        // is rejected atomically.
                        if stats.nodes_visited >= config.node_budget {
                            exhausted_budget = true;
                            break;
                        }
                    }
                }
            }
            if exhausted_budget {
                break;
            }
        }
    }

    let stop_reasons = stops.into_iter().collect::<Vec<_>>();
    let complete = stop_reasons.is_empty();
    let best = lethal_candidates.into_iter().reduce(|current, candidate| {
        if candidate.is_better_than(&current) {
            candidate
        } else {
            current
        }
    });
    let (assessment, route, evidence, selected_path, selected_states) = match best {
        Some(candidate) => (
            candidate.assessment,
            Some(candidate.route),
            Some(candidate.evidence),
            Some(candidate.stable_path),
            Some(candidate.states),
        ),
        None if complete => (LethalAssessment::None, None, None, None, None),
        None => (LethalAssessment::Unknown, None, None, None, None),
    };
    Ok(LethalSearchResult {
        assessment,
        route,
        evidence,
        selected_path,
        selected_states,
        aggregate_evidence,
        evaluated_roots,
        root_evaluations,
        evaluated_paths,
        path_expansions,
        complete,
        stats,
        stop_reasons,
    })
}

struct CandidateExpansion {
    scenarios: Vec<Scenario>,
    evidence: LethalEvidence,
}

enum CandidateEvaluation {
    Expansion(CandidateExpansion),
    Skipped(LethalStopReason),
    BudgetExhausted,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_candidate(
    request: &PlannerRequest,
    node: &SearchNode,
    action: SimAction,
    path: &CanonicalActionPath,
    opponent: PlayerId,
    is_root: bool,
    stats: &mut LethalSearchStats,
    stops: &mut BTreeSet<LethalStopReason>,
    aggregate_evidence: &mut PathEvidence,
) -> Result<CandidateEvaluation, PlannerError> {
    let config = request.search_config();
    stats.max_depth_reached = stats
        .max_depth_reached
        .max(u16::try_from(path.len()).unwrap_or(u16::MAX));

    let seed = path.seed(config.seed);
    let sim_request = simulation_request(seed, config.sim_branch_budget);
    let context = request.resources().sim_context();
    let mut scenarios = Vec::new();
    let mut evidence = node.evidence.clone();
    let mut newly_visited = 0_u64;
    let mut first_boundary = None;

    for scenario in &node.scenarios {
        if effective_life(&scenario.state, opponent).is_some_and(|life| life <= 0) {
            scenarios.push(scenario.clone());
            continue;
        }

        let outcome =
            apply_action(&scenario.state, &action, &context, &sim_request).map_err(|error| {
                PlannerError::Failed {
                    message: format!("lethal simulation failed: {error}"),
                }
            })?;
        let has_untrusted_evidence = !outcome.unknown_rule_cards.is_empty()
            || outcome.rule_uses.iter().any(|rule_use| {
                rule_use.status != RuleStatus::Verified
                    || !rule_use.confidence.is_finite()
                    || rule_use.confidence != 1.0
            });
        if has_untrusted_evidence {
            stops.insert(LethalStopReason::MissingEvidence);
        }
        let expansion = expand_outcome(outcome, scenario.probability, &mut evidence);
        aggregate_evidence.merge_from(&evidence);
        match expansion {
            OutcomeExpansion::Expanded {
                states,
                node_count,
                sampled,
            } => {
                stats.simulation_branches_examined = stats
                    .simulation_branches_examined
                    .saturating_add(node_count);
                if sampled {
                    stops.insert(LethalStopReason::SampledSimulation);
                }
                newly_visited = newly_visited.saturating_add(node_count);
                scenarios.extend(states);
            }
            OutcomeExpansion::Boundary(SearchBoundary::PlayerChoice) => {
                stops.insert(LethalStopReason::PlayerChoice);
                first_boundary.get_or_insert(LethalStopReason::PlayerChoice);
            }
            OutcomeExpansion::Boundary(SearchBoundary::MissingEvidence) => {
                stops.insert(LethalStopReason::MissingEvidence);
                first_boundary.get_or_insert(LethalStopReason::MissingEvidence);
            }
            OutcomeExpansion::Boundary(SearchBoundary::IncompleteProbabilityMass) => {
                stops.insert(LethalStopReason::IncompleteProbabilityMass);
                first_boundary.get_or_insert(LethalStopReason::IncompleteProbabilityMass);
            }
            OutcomeExpansion::Boundary(SearchBoundary::Lazy) => {
                stops.insert(LethalStopReason::LazySimulation);
                first_boundary.get_or_insert(LethalStopReason::LazySimulation);
            }
            OutcomeExpansion::Boundary(SearchBoundary::Unsupported(reason)) => {
                if is_root || !matches!(reason, UnsupportedReason::ActionUnavailable { .. }) {
                    stops.insert(LethalStopReason::UnsupportedSimulation);
                }
                first_boundary.get_or_insert(LethalStopReason::UnsupportedSimulation);
            }
        }
    }

    let exceeds_node_budget =
        expansion_exceeds_node_budget(stats, 0, newly_visited, config.node_budget);
    if exceeds_node_budget {
        stops.insert(LethalStopReason::NodeBudget);
    }
    if let Some(reason) = first_boundary {
        return Ok(CandidateEvaluation::Skipped(reason));
    }
    if exceeds_node_budget {
        return Ok(CandidateEvaluation::BudgetExhausted);
    }
    stats.nodes_visited = stats.nodes_visited.saturating_add(newly_visited);
    Ok(CandidateEvaluation::Expansion(CandidateExpansion {
        scenarios,
        evidence,
    }))
}

fn expansion_exceeds_node_budget(
    stats: &LethalSearchStats,
    newly_visited: u64,
    additional: u64,
    node_budget: u64,
) -> bool {
    stats
        .nodes_visited
        .checked_add(newly_visited)
        .and_then(|visited| visited.checked_add(additional))
        .is_none_or(|visited| visited > node_budget)
}

#[allow(clippy::too_many_arguments)]
fn accept_expansion(
    request: &PlannerRequest,
    expansion: CandidateExpansion,
    first_action: LegalAction,
    path: CanonicalActionPath,
    opponent: PlayerId,
    frontier: &mut SearchFrontier,
    lethal_candidates: &mut Vec<RankedCandidate>,
    stops: &mut BTreeSet<LethalStopReason>,
) {
    let remaining = templates_after_action(
        request.action_templates().templates(),
        first_action.kind,
        first_action.source,
    );
    accept_expansion_with_remaining(
        request,
        expansion,
        first_action,
        path,
        remaining,
        opponent,
        frontier,
        lethal_candidates,
        stops,
    );
}

#[allow(clippy::too_many_arguments)]
fn accept_expansion_with_remaining(
    request: &PlannerRequest,
    expansion: CandidateExpansion,
    first_action: LegalAction,
    path: CanonicalActionPath,
    remaining: Vec<ActionTemplate>,
    opponent: PlayerId,
    frontier: &mut SearchFrontier,
    lethal_candidates: &mut Vec<RankedCandidate>,
    stops: &mut BTreeSet<LethalStopReason>,
) {
    if !weighted_probability_mass_complete(&expansion.scenarios) {
        stops.insert(LethalStopReason::IncompleteProbabilityMass);
        return;
    }

    let lethal_probability = expansion
        .scenarios
        .iter()
        .filter(|scenario| effective_life(&scenario.state, opponent).is_some_and(|life| life <= 0))
        .map(|scenario| scenario.probability)
        .sum::<f64>();
    let has_lethal_scenario = expansion
        .scenarios
        .iter()
        .any(|scenario| effective_life(&scenario.state, opponent).is_some_and(|life| life <= 0));
    if has_lethal_scenario {
        let assessment =
            classify_scenario_lethal(&expansion.evidence, &expansion.scenarios, opponent);
        let candidate = RankedCandidate {
            assessment,
            route: LethalRoute {
                first_action,
                follow_up: path
                    .iter()
                    .skip(1)
                    .copied()
                    .map(|action| {
                        action
                            .planned_action()
                            .expect("canonical follow-up actions are simulated")
                    })
                    .collect(),
                path_seed: path.seed(request.search_config().seed),
            },
            evidence: expansion.evidence.clone(),
            probability: lethal_probability,
            stable_path: path.clone(),
            states: expansion.scenarios.clone(),
        };
        lethal_candidates.push(candidate);
    }

    let has_live_scenario = expansion.scenarios.iter().any(|scenario| {
        effective_life(&scenario.state, opponent).is_some_and(|life| life > 0)
            && !scenario.state.turn_ended
    });
    if !has_live_scenario || remaining.is_empty() {
        return;
    }
    let depth = u16::try_from(path.len()).unwrap_or(u16::MAX);
    if depth >= request.search_config().max_depth {
        if remaining.iter().any(|template| {
            template_relevant(template, &expansion.scenarios, opponent)
                && template.has_only_recheckable_gates()
        }) {
            stops.insert(LethalStopReason::MaxDepth);
        }
        if remaining.iter().any(|template| {
            template_relevant(template, &expansion.scenarios, opponent)
                && !template.has_only_recheckable_gates()
        }) {
            stops.insert(LethalStopReason::MissingEvidence);
        }
        return;
    }
    frontier.push(SearchNode {
        scenarios: expansion.scenarios,
        remaining,
        first_action,
        path,
        evidence: expansion.evidence,
    });
}

fn classify_lethal_candidate(
    evidence: &LethalEvidence,
    all_scenarios_lethal: bool,
    lethal_probability: f64,
) -> LethalAssessment {
    if evidence.proves_exact() {
        if all_scenarios_lethal {
            LethalAssessment::Guaranteed
        } else {
            LethalAssessment::Probabilistic {
                probability: bounded_probability(lethal_probability),
            }
        }
    } else if evidence.supports_sampled_estimate() {
        LethalAssessment::Estimated {
            probability: bounded_probability(lethal_probability),
            sample_count: evidence.sample_count(),
        }
    } else {
        LethalAssessment::Unknown
    }
}

pub(crate) fn classify_scenario_lethal(
    evidence: &PathEvidence,
    scenarios: &[WeightedState],
    opponent: PlayerId,
) -> LethalAssessment {
    let lethal_probability = scenarios
        .iter()
        .filter(|scenario| effective_life(&scenario.state, opponent).is_some_and(|life| life <= 0))
        .map(|scenario| scenario.probability)
        .sum::<f64>();
    let any_lethal = scenarios
        .iter()
        .any(|scenario| effective_life(&scenario.state, opponent).is_some_and(|life| life <= 0));
    if !any_lethal {
        return if evidence.proves_exact() {
            LethalAssessment::None
        } else {
            LethalAssessment::Unknown
        };
    }
    let all_lethal = scenarios
        .iter()
        .all(|scenario| effective_life(&scenario.state, opponent).is_some_and(|life| life <= 0));
    classify_lethal_candidate(evidence, all_lethal, lethal_probability)
}

fn bounded_probability(probability: f64) -> Confidence {
    let bounded = if probability.is_finite() {
        probability.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut narrowed = bounded as f32;
    if bounded > 0.0 && narrowed == 0.0 {
        narrowed = f32::from_bits(1);
    } else if bounded < 1.0 && narrowed >= 1.0 {
        narrowed = f32::from_bits(1.0_f32.to_bits() - 1);
    }
    Confidence::new(narrowed).expect("clamped probability is a confidence")
}

fn simulation_request(seed: u64, branch_budget: u32) -> SimRequest {
    SimRequest {
        seed,
        branch_budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_sim::{OutcomeSet, ReproMeta, RuleTrace, RuleUse, SampleMethod, SimOutcome};
    use hs_types::CardId;

    fn evidence_outcome(rule_uses: Vec<RuleUse>, unknown_rule_cards: Vec<CardId>) -> SimOutcome {
        SimOutcome {
            outcome: OutcomeSet::Deterministic(SimGameState::default()),
            trace: RuleTrace::default(),
            meta: ReproMeta {
                seed: 1,
                branch_budget: 2,
                method: SampleMethod::Exact,
                truncation: None,
                chance_kind: None,
                rule_version: "root".into(),
                pool_snapshot_hashes: Vec::new(),
            },
            unknown_rule_cards,
            rule_uses,
        }
    }

    #[test]
    fn sampled_candidate_requires_trusted_rules_and_no_unknown_cards() {
        let mut evidence = LethalEvidence::root();
        evidence.mark_sampled(4);
        assert!(matches!(
            classify_lethal_candidate(&evidence, true, 1.0),
            LethalAssessment::Estimated {
                sample_count: 4,
                ..
            }
        ));

        let mut inferred = evidence.clone();
        inferred.observe(&evidence_outcome(
            vec![RuleUse {
                card_id: CardId::new("INFERRED"),
                status: RuleStatus::Inferred,
                confidence: 0.9,
                rule_version: "v1".into(),
            }],
            Vec::new(),
        ));
        assert_eq!(
            classify_lethal_candidate(&inferred, true, 1.0),
            LethalAssessment::Unknown
        );

        let mut low_confidence = evidence.clone();
        low_confidence.observe(&evidence_outcome(
            vec![RuleUse {
                card_id: CardId::new("VERIFIED"),
                status: RuleStatus::Verified,
                confidence: 0.9,
                rule_version: "v1".into(),
            }],
            Vec::new(),
        ));
        assert_eq!(
            classify_lethal_candidate(&low_confidence, true, 1.0),
            LethalAssessment::Unknown,
            "Verified with confidence below one is still untrusted"
        );

        evidence.observe(&evidence_outcome(Vec::new(), vec![CardId::new("MISSING")]));
        assert_eq!(
            classify_lethal_candidate(&evidence, true, 1.0),
            LethalAssessment::Unknown
        );
    }
}
