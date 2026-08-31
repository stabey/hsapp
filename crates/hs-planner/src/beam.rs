//! Deterministic, fixed-budget current-turn Beam search.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use hs_advisor_api::{
    AdviceLine, AdvicePayload, AdviceRisk, AdviceRiskKind, AdviceWarning, AdviceWarningKind,
    ChanceSummary, Confidence, ConfidencePolicy, LegalAction, LethalAssessment, PlannedAction,
    RuleCoverageReport, ScoreBreakdown, ScoreComponent, ScoreFeature, SearchCompleteness,
    SearchConfig, UnavailableReason,
};
use hs_sim::{apply_action, SimAction, SimGameState, SimRequest, UnsupportedReason};
use hs_types::{PlayerId, RuleStatus};
use serde::Serialize;

use crate::confidence::{line_confidence, result_confidence, LineEvidence, SimulationEvidence};
use crate::coverage::CoverageCollector;
use crate::legal_actions::ActionTemplate;
use crate::lethal::{
    classify_scenario_lethal, search_lethal, LethalRootEvaluation, LethalStopReason,
};
use crate::node::{CanonicalActionKey, CanonicalActionPath};
use crate::playbook::{DeckSignature, PlaybookCatalog, PlaybookSelectionReason, SelectedPlaybook};
use crate::score::{explain_score, score_state};
use crate::search::{
    common_positions, effective_life, expand_outcome, opponent_player, probability_units,
    template_entities_present, template_relevant, templates_after_action,
    weighted_probability_mass_complete, OutcomeExpansion, PathEvidence, SearchBoundary,
    WeightedState, PROBABILITY_SCALE,
};
use crate::sim_action::template_action_to_sim;
use crate::{legal_action_to_sim, Engine, PlannerError, PlannerRequest, PlannerResult};

/// Versioned policy mapping structural search gaps to an evidence cap.
pub const SEARCH_COMPLETENESS_POLICY_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeamStopReason {
    MissingOpponent,
    MissingState,
    CandidateBudget,
    NodeBudget,
    MaxDepth,
    BeamWidth,
    PlayerChoice,
    SampledSimulation,
    LazySimulation,
    UnsupportedSimulation,
    MissingEvidence,
    IncompleteProbabilityMass,
    ProjectionGap,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BeamSearchStats {
    config: SearchConfig,
    legal_root_total: u64,
    roots_evaluated: u64,
    roots_reliably_compared: u64,
    unsupported_roots: u64,
    unknown_roots: u64,
    candidates_evaluated: u64,
    nodes_visited: u64,
    simulation_branches_examined: u64,
    max_depth_reached: u16,
    layer_widths: Vec<u32>,
    deduplicated_nodes: u64,
    beam_roots_evaluated: u64,
    lethal_roots_evaluated: u64,
    lethal_candidates_evaluated: u64,
    lethal_nodes_visited: u64,
    lethal_simulation_branches_examined: u64,
    lethal_max_depth_reached: u16,
}

impl BeamSearchStats {
    fn new(config: SearchConfig, legal_root_total: u64) -> Self {
        Self {
            config,
            legal_root_total,
            roots_evaluated: 0,
            roots_reliably_compared: 0,
            unsupported_roots: 0,
            unknown_roots: 0,
            candidates_evaluated: 0,
            nodes_visited: 0,
            simulation_branches_examined: 0,
            max_depth_reached: 0,
            layer_widths: Vec::new(),
            deduplicated_nodes: 0,
            beam_roots_evaluated: 0,
            lethal_roots_evaluated: 0,
            lethal_candidates_evaluated: 0,
            lethal_nodes_visited: 0,
            lethal_simulation_branches_examined: 0,
            lethal_max_depth_reached: 0,
        }
    }

    pub const fn config(&self) -> SearchConfig {
        self.config
    }

    pub const fn legal_root_total(&self) -> u64 {
        self.legal_root_total
    }

    pub const fn roots_evaluated(&self) -> u64 {
        self.roots_evaluated
    }

    pub const fn roots_reliably_compared(&self) -> u64 {
        self.roots_reliably_compared
    }

    pub const fn unsupported_roots(&self) -> u64 {
        self.unsupported_roots
    }

    pub const fn unknown_roots(&self) -> u64 {
        self.unknown_roots
    }

    pub const fn candidates_evaluated(&self) -> u64 {
        self.candidates_evaluated
    }

    pub const fn nodes_visited(&self) -> u64 {
        self.nodes_visited
    }

    pub const fn simulation_branches_examined(&self) -> u64 {
        self.simulation_branches_examined
    }

    pub const fn max_depth_reached(&self) -> u16 {
        self.max_depth_reached
    }

    pub fn layer_widths(&self) -> &[u32] {
        &self.layer_widths
    }

    pub const fn deduplicated_nodes(&self) -> u64 {
        self.deduplicated_nodes
    }

    pub const fn lethal_candidates_evaluated(&self) -> u64 {
        self.lethal_candidates_evaluated
    }

    pub const fn beam_roots_evaluated(&self) -> u64 {
        self.beam_roots_evaluated
    }

    pub const fn lethal_roots_evaluated(&self) -> u64 {
        self.lethal_roots_evaluated
    }

    pub const fn lethal_nodes_visited(&self) -> u64 {
        self.lethal_nodes_visited
    }

    pub const fn lethal_simulation_branches_examined(&self) -> u64 {
        self.lethal_simulation_branches_examined
    }

    pub const fn lethal_max_depth_reached(&self) -> u16 {
        self.lethal_max_depth_reached
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ChanceScore {
    expected_milli: i64,
    downside_milli: i64,
    rank_milli: i64,
    min_milli: i64,
    max_milli: i64,
    probability_mass_units: i64,
}

impl ChanceScore {
    pub const fn expected_milli(&self) -> i64 {
        self.expected_milli
    }

    pub const fn downside_milli(&self) -> i64 {
        self.downside_milli
    }

    pub const fn rank_milli(&self) -> i64 {
        self.rank_milli
    }

    pub const fn min_milli(&self) -> i64 {
        self.min_milli
    }

    pub const fn max_milli(&self) -> i64 {
        self.max_milli
    }

    pub const fn probability_mass_units(&self) -> i64 {
        self.probability_mass_units
    }

    fn summary(&self, evidence: &PathEvidence) -> ChanceSummary {
        ChanceSummary::new(
            self.expected_milli,
            self.downside_milli,
            self.rank_milli,
            self.min_milli,
            self.max_milli,
            self.probability_mass_units,
            i64::try_from(PROBABILITY_SCALE)
                .expect("fixed probability scale fits the public contract"),
            !evidence.exact(),
            evidence.sample_count(),
        )
        .expect("Beam chance scoring always produces a valid public summary")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BeamCandidate {
    line: AdviceLine,
    path: CanonicalActionPath,
    path_seed: u64,
    chance_score: ChanceScore,
    evidence: PathEvidence,
    rule_coverage: RuleCoverageReport,
    lethal_first_route: bool,
}

impl BeamCandidate {
    pub fn line(&self) -> &AdviceLine {
        &self.line
    }

    pub fn path(&self) -> &CanonicalActionPath {
        &self.path
    }

    pub const fn path_seed(&self) -> u64 {
        self.path_seed
    }

    pub fn chance_score(&self) -> &ChanceScore {
        &self.chance_score
    }

    pub fn evidence(&self) -> &PathEvidence {
        &self.evidence
    }

    pub fn rule_coverage(&self) -> &RuleCoverageReport {
        &self.rule_coverage
    }

    /// Whether this is the exact stable route selected by the guaranteed
    /// lethal-first search. Probabilistic or estimated routes never set it.
    pub const fn lethal_first_route(&self) -> bool {
        self.lethal_first_route
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BeamSearchResult {
    candidates: Vec<BeamCandidate>,
    rule_coverage: RuleCoverageReport,
    search_completeness: SearchCompleteness,
    stats: BeamSearchStats,
    stop_reasons: Vec<BeamStopReason>,
    playbook_selection: PlaybookSelectionReason,
    lethal_first: LethalAssessment,
}

impl BeamSearchResult {
    pub fn candidates(&self) -> &[BeamCandidate] {
        &self.candidates
    }

    pub fn rule_coverage(&self) -> &RuleCoverageReport {
        &self.rule_coverage
    }

    pub const fn search_completeness(&self) -> SearchCompleteness {
        self.search_completeness
    }

    pub fn stats(&self) -> &BeamSearchStats {
        &self.stats
    }

    pub fn stop_reasons(&self) -> &[BeamStopReason] {
        &self.stop_reasons
    }

    pub fn playbook_selection(&self) -> &PlaybookSelectionReason {
        &self.playbook_selection
    }

    pub const fn lethal_first(&self) -> LethalAssessment {
        self.lethal_first
    }

    pub fn complete(&self) -> bool {
        self.search_completeness.ratio() == Confidence::ONE
    }
}

/// Production synchronous engine for one selected deck signature.
#[derive(Clone, Debug)]
pub struct BeamEngine {
    catalog: PlaybookCatalog,
    deck_signature: Option<DeckSignature>,
    confidence_policy: ConfidencePolicy,
}

impl BeamEngine {
    pub fn new(catalog: PlaybookCatalog, deck_signature: Option<DeckSignature>) -> Self {
        Self {
            catalog,
            deck_signature,
            confidence_policy: ConfidencePolicy::default(),
        }
    }

    pub fn with_confidence_policy(mut self, policy: ConfidencePolicy) -> Self {
        self.confidence_policy = policy;
        self
    }
}

impl Engine for BeamEngine {
    fn plan(&self, request: PlannerRequest) -> Result<PlannerResult, PlannerError> {
        let selected = self
            .catalog
            .select(request.identity().build_id, self.deck_signature.as_ref())
            .map_err(|error| PlannerError::Failed {
                message: format!("Playbook selection failed: {error}"),
            })?;
        let result = search_beam_with_policy(&request, &selected, &self.confidence_policy)?;
        if result.candidates.is_empty() {
            return Err(UnavailableReason::NoReliableActions.into());
        }

        let lines = result
            .candidates
            .iter()
            .map(|candidate| candidate.line.clone())
            .collect::<Vec<_>>();
        let confidence = result_confidence(
            &self.confidence_policy,
            &lines[0].confidence,
            &result.search_completeness,
        );
        let mut warnings = warnings_for_result(&result);
        let belief = request.belief().summary();
        if belief.insufficient_data {
            warnings.push(AdviceWarning {
                kind: AdviceWarningKind::InsufficientBeliefData,
                code: "belief.insufficient_data".into(),
                message:
                    "Opponent archetype data is not available; no hidden-card odds were invented."
                        .into(),
            });
        }
        let explanation = lines[0].explanation.clone();
        let payload = AdvicePayload {
            confidence,
            rule_coverage: result.rule_coverage,
            opponent_belief_summary: belief,
            search_completeness: result.search_completeness,
            lines,
            explanation,
            warnings,
        };
        PlannerResult::for_request(&request, payload)
    }
}

pub fn search_beam(
    request: &PlannerRequest,
    playbook: &SelectedPlaybook<'_>,
) -> Result<BeamSearchResult, PlannerError> {
    search_beam_with_policy(request, playbook, &ConfidencePolicy::default())
}

fn search_beam_with_policy(
    request: &PlannerRequest,
    playbook: &SelectedPlaybook<'_>,
    confidence_policy: &ConfidencePolicy,
) -> Result<BeamSearchResult, PlannerError> {
    let config = request.search_config();
    let mut roots = request.legal_actions().actions.clone();
    roots.sort_by_key(|action| action.server_ref);
    let root_total = u64::try_from(roots.len()).unwrap_or(u64::MAX);
    let mut stats = BeamSearchStats::new(config, root_total);
    let lethal_first = search_lethal(request)?;
    let protect_lethal_route = lethal_first.assessment() == LethalAssessment::Guaranteed;
    let lethal_stats = lethal_first.stats();
    stats.lethal_roots_evaluated = lethal_stats.roots_evaluated();
    stats.lethal_candidates_evaluated = lethal_stats.candidates_evaluated();
    stats.lethal_nodes_visited = lethal_stats.nodes_visited();
    stats.lethal_simulation_branches_examined = lethal_stats.simulation_branches_examined();
    stats.lethal_max_depth_reached = lethal_stats.max_depth_reached();
    // Candidate and node budgets are shared by the lethal-first pass and the
    // general Beam pass. The phase counters remain as an auditable subset.
    stats.candidates_evaluated = stats.lethal_candidates_evaluated;
    stats.roots_evaluated = stats.lethal_roots_evaluated;
    stats.nodes_visited = stats.lethal_nodes_visited;
    stats.simulation_branches_examined = stats.lethal_simulation_branches_examined;
    stats.max_depth_reached = stats.lethal_max_depth_reached;

    let mut global_stops = lethal_first
        .stop_reasons()
        .iter()
        .copied()
        .map(beam_stop_for_lethal)
        .collect::<BTreeSet<_>>();
    let mut global_coverage = CoverageCollector::for_request(request);
    for root in lethal_first.evaluated_roots() {
        global_coverage
            .record_root_action(root)
            .map_err(|error| PlannerError::Failed {
                message: format!("lethal coverage binding failed: {error}"),
            })?;
    }
    global_coverage.record_path_evidence(lethal_first.aggregate_evidence());

    let initial = SimGameState::from_observed(request.observed_state());
    let Some(opponent) = opponent_player(&initial) else {
        global_stops.insert(BeamStopReason::MissingOpponent);
        return empty_result(
            playbook,
            stats,
            global_stops,
            lethal_first.assessment(),
            global_coverage.finish(),
        );
    };
    if effective_life(&initial, opponent).is_none() {
        global_stops.insert(BeamStopReason::MissingOpponent);
        return empty_result(
            playbook,
            stats,
            global_stops,
            lethal_first.assessment(),
            global_coverage.finish(),
        );
    }
    if config.max_depth == 0 {
        global_stops.insert(BeamStopReason::MaxDepth);
        return empty_result(
            playbook,
            stats,
            global_stops,
            lethal_first.assessment(),
            global_coverage.finish(),
        );
    }
    if config.beam_width == 0 {
        global_stops.insert(BeamStopReason::BeamWidth);
        if !protect_lethal_route {
            return empty_result(
                playbook,
                stats,
                global_stops,
                lethal_first.assessment(),
                global_coverage.finish(),
            );
        }
    }

    let projection_gap = request.action_templates().has_projection_gaps();
    if projection_gap {
        global_stops.insert(BeamStopReason::ProjectionGap);
    }
    let templates = request.action_templates().templates().to_vec();
    let mut root_layer = Vec::new();
    let mut candidates = Vec::new();
    let mut reliable_roots = BTreeSet::new();
    let evaluated_by_lethal = lethal_first
        .evaluated_roots()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let evaluated_paths = lethal_first
        .evaluated_paths()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let cached_expansions = lethal_first
        .path_expansions()
        .iter()
        .map(|expansion| (expansion.path.clone(), expansion))
        .collect::<BTreeMap<_, _>>();
    let mut exhausted = config.beam_width == 0
        || stats.candidates_evaluated >= config.candidate_budget
        || stats.nodes_visited >= config.node_budget;

    // Successful lethal root simulations are also valid depth-1 Beam nodes.
    // Seed the general frontier from them instead of charging the shared
    // candidate/node budget twice for the same canonical action path.
    for evaluation in lethal_first.root_evaluations() {
        match evaluation {
            LethalRootEvaluation::Expansion {
                root,
                path,
                states,
                evidence,
            } => {
                let mut node = BeamNode {
                    states: states.clone(),
                    remaining: templates_after_action(&templates, root.kind, root.source),
                    first_action: *root,
                    path: path.clone(),
                    evidence: evidence.clone(),
                    root_coverage: coverage_for_root(request, root)?,
                    local_stops: BTreeSet::new(),
                };
                finalize_node_stops(&mut node, config, opponent, &mut global_stops);
                if let Some(mut candidate) = candidate_for_node(
                    request,
                    &initial,
                    &node,
                    playbook,
                    confidence_policy,
                    opponent,
                )? {
                    candidate.lethal_first_route = protect_lethal_route
                        && lethal_first
                            .selected_path()
                            .is_some_and(|selected| selected == &node.path);
                    reliable_roots.insert(*root);
                    root_layer.push(FrontierEntry {
                        protected: protect_lethal_route
                            && is_lethal_prefix(&node.path, lethal_first.selected_path()),
                        continuable: node_can_continue(&node, opponent),
                        node,
                        candidate,
                    });
                } else {
                    stats.unknown_roots = stats.unknown_roots.saturating_add(1);
                }
            }
            LethalRootEvaluation::Boundary { reason, .. } => match reason {
                LethalStopReason::LazySimulation | LethalStopReason::UnsupportedSimulation => {
                    stats.unsupported_roots = stats.unsupported_roots.saturating_add(1);
                }
                _ => {
                    stats.unknown_roots = stats.unknown_roots.saturating_add(1);
                }
            },
        }
    }

    // A guaranteed lethal proof already spent the shared budget and owns the
    // exact terminal chance group. Reuse it directly so Beam never has to pay
    // again merely to publish the proven route.
    if protect_lethal_route {
        if let (Some(route), Some(path), Some(states), Some(evidence)) = (
            lethal_first.route(),
            lethal_first.selected_path(),
            lethal_first.selected_states(),
            lethal_first.evidence(),
        ) {
            let root = route.first_action();
            global_coverage
                .record_root_action(&root)
                .map_err(|error| PlannerError::Failed {
                    message: format!("lethal root coverage binding failed: {error}"),
                })?;
            global_coverage.record_path_evidence(evidence);
            let mut node = BeamNode {
                states: states.to_vec(),
                remaining: remaining_after_path(&templates, path),
                first_action: root,
                path: path.clone(),
                evidence: evidence.clone(),
                root_coverage: coverage_for_root(request, &root)?,
                local_stops: BTreeSet::new(),
            };
            finalize_node_stops(&mut node, config, opponent, &mut global_stops);
            if let Some(mut candidate) = candidate_for_node(
                request,
                &initial,
                &node,
                playbook,
                confidence_policy,
                opponent,
            )? {
                candidate.lethal_first_route = true;
                reliable_roots.insert(root);
                candidates.push(candidate);
            }
        }
    }

    for root in roots.iter().copied() {
        if exhausted {
            break;
        }
        if evaluated_by_lethal.contains(&root) {
            continue;
        }
        if !reserve_candidate(&mut stats, config.candidate_budget) {
            global_stops.insert(BeamStopReason::CandidateBudget);
            exhausted = true;
            break;
        }
        global_coverage
            .record_root_action(&root)
            .map_err(|error| PlannerError::Failed {
                message: format!("root coverage binding failed: {error}"),
            })?;
        stats.beam_roots_evaluated = stats.beam_roots_evaluated.saturating_add(1);
        stats.roots_evaluated = stats
            .lethal_roots_evaluated
            .saturating_add(stats.beam_roots_evaluated)
            .min(stats.legal_root_total);
        let Ok(sim_action) = legal_action_to_sim(&root) else {
            stats.unsupported_roots = stats.unsupported_roots.saturating_add(1);
            global_stops.insert(BeamStopReason::UnsupportedSimulation);
            continue;
        };
        let path = CanonicalActionPath::from_root(root);
        let root_coverage = coverage_for_root(request, &root)?;
        let parent = BeamNode {
            states: vec![WeightedState {
                state: initial.clone(),
                probability: 1.0,
            }],
            remaining: templates_after_action(&templates, root.kind, root.source),
            first_action: root,
            path: CanonicalActionPath::default(),
            evidence: PathEvidence::root(),
            root_coverage,
            local_stops: BTreeSet::new(),
        };
        match evaluate_action(request, &parent, sim_action, &path, opponent, &mut stats)? {
            ActionEvaluation::Expansion { states, evidence } => {
                global_coverage.record_path_evidence(&evidence);
                let mut node = BeamNode {
                    states,
                    remaining: templates_after_action(&templates, root.kind, root.source),
                    first_action: root,
                    path,
                    evidence,
                    root_coverage: parent.root_coverage,
                    local_stops: parent.local_stops,
                };
                finalize_node_stops(&mut node, config, opponent, &mut global_stops);
                match candidate_for_node(
                    request,
                    &initial,
                    &node,
                    playbook,
                    confidence_policy,
                    opponent,
                )? {
                    Some(mut candidate) => {
                        candidate.lethal_first_route = protect_lethal_route
                            && lethal_first
                                .selected_path()
                                .is_some_and(|selected| selected == &node.path);
                        reliable_roots.insert(root);
                        root_layer.push(FrontierEntry {
                            protected: protect_lethal_route
                                && is_lethal_prefix(&node.path, lethal_first.selected_path()),
                            continuable: node_can_continue(&node, opponent),
                            node,
                            candidate,
                        });
                    }
                    None => {
                        stats.unknown_roots = stats.unknown_roots.saturating_add(1);
                        global_stops.insert(BeamStopReason::MissingEvidence);
                    }
                }
            }
            ActionEvaluation::Boundary { reason, evidence } => {
                global_coverage.record_path_evidence(&evidence);
                record_root_boundary(reason, &mut stats, &mut global_stops);
            }
            ActionEvaluation::BudgetExhausted { evidence } => {
                global_coverage.record_path_evidence(&evidence);
                global_stops.insert(BeamStopReason::NodeBudget);
                if stats.nodes_visited >= config.node_budget {
                    exhausted = true;
                    break;
                }
            }
        }
    }
    stats.roots_reliably_compared = u64::try_from(reliable_roots.len()).unwrap_or(u64::MAX);

    let kept = prune_layer(root_layer, config.beam_width, &mut stats, &mut global_stops)?;
    candidates.extend(kept.iter().map(|entry| entry.candidate.clone()));
    let mut frontier = kept
        .into_iter()
        .filter(|entry| entry.continuable)
        .collect::<Vec<_>>();
    stats
        .layer_widths
        .push(u32::try_from(frontier.len()).unwrap_or(u32::MAX));

    // Even when the shared ledger is full, walk the already-paid lethal graph
    // through Beam pruning so deep probabilistic/estimated routes are not
    // discarded. Only genuinely new simulations are blocked by `exhausted`.
    while !frontier.is_empty() {
        let mut next_layer = Vec::new();
        for entry in frontier {
            let node = entry.node;
            for template in &node.remaining {
                if !template_entities_present(template, &node.states, opponent) {
                    continue;
                }
                if !template.has_only_recheckable_gates() {
                    if template_relevant(template, &node.states, opponent) {
                        global_stops.insert(BeamStopReason::MissingEvidence);
                    }
                    continue;
                }
                for position in common_positions(template, &node.states, opponent) {
                    let mut path = node.path.clone();
                    path.push(CanonicalActionKey::from_template(template, position));

                    if let Some(expansion) = cached_expansions.get(&path) {
                        global_coverage.record_path_evidence(&expansion.evidence);
                        let mut child = BeamNode {
                            states: expansion.states.clone(),
                            remaining: templates_after_action(
                                &node.remaining,
                                template.kind(),
                                template.source(),
                            ),
                            first_action: expansion.first_action,
                            path: expansion.path.clone(),
                            evidence: expansion.evidence.clone(),
                            root_coverage: node.root_coverage.clone(),
                            local_stops: node.local_stops.clone(),
                        };
                        finalize_node_stops(&mut child, config, opponent, &mut global_stops);
                        if let Some(mut candidate) = candidate_for_node(
                            request,
                            &initial,
                            &child,
                            playbook,
                            confidence_policy,
                            opponent,
                        )? {
                            candidate.lethal_first_route = protect_lethal_route
                                && is_selected_lethal_route(
                                    &child.path,
                                    lethal_first.selected_path(),
                                );
                            next_layer.push(FrontierEntry {
                                protected: protect_lethal_route
                                    && is_lethal_prefix(&child.path, lethal_first.selected_path()),
                                continuable: node_can_continue(&child, opponent),
                                node: child,
                                candidate,
                            });
                        } else {
                            global_stops.insert(BeamStopReason::MissingEvidence);
                        }
                        continue;
                    }
                    // Lethal already charged this path but found a structured
                    // boundary. Its global stop/evidence was imported above;
                    // do not pay for the same failed attempt again.
                    if evaluated_paths.contains(&path) {
                        continue;
                    }
                    if exhausted {
                        continue;
                    }
                    if !reserve_candidate(&mut stats, config.candidate_budget) {
                        global_stops.insert(BeamStopReason::CandidateBudget);
                        exhausted = true;
                        continue;
                    }
                    let Ok(sim_action) = template_action_to_sim(template, position) else {
                        global_stops.insert(BeamStopReason::UnsupportedSimulation);
                        continue;
                    };
                    match evaluate_action(request, &node, sim_action, &path, opponent, &mut stats)?
                    {
                        ActionEvaluation::Expansion { states, evidence } => {
                            global_coverage.record_path_evidence(&evidence);
                            let mut child = BeamNode {
                                states,
                                remaining: templates_after_action(
                                    &node.remaining,
                                    template.kind(),
                                    template.source(),
                                ),
                                first_action: node.first_action,
                                path,
                                evidence,
                                root_coverage: node.root_coverage.clone(),
                                local_stops: node.local_stops.clone(),
                            };
                            finalize_node_stops(&mut child, config, opponent, &mut global_stops);
                            if let Some(mut candidate) = candidate_for_node(
                                request,
                                &initial,
                                &child,
                                playbook,
                                confidence_policy,
                                opponent,
                            )? {
                                candidate.lethal_first_route = protect_lethal_route
                                    && is_selected_lethal_route(
                                        &child.path,
                                        lethal_first.selected_path(),
                                    );
                                next_layer.push(FrontierEntry {
                                    protected: protect_lethal_route
                                        && is_lethal_prefix(
                                            &child.path,
                                            lethal_first.selected_path(),
                                        ),
                                    continuable: node_can_continue(&child, opponent),
                                    node: child,
                                    candidate,
                                });
                            } else {
                                global_stops.insert(BeamStopReason::MissingEvidence);
                            }
                        }
                        ActionEvaluation::Boundary { reason, evidence } => {
                            global_coverage.record_path_evidence(&evidence);
                            if !matches!(
                                reason,
                                SearchBoundary::Unsupported(
                                    UnsupportedReason::ActionUnavailable { .. }
                                )
                            ) {
                                global_stops.insert(stop_for_boundary(&reason));
                            }
                        }
                        ActionEvaluation::BudgetExhausted { evidence } => {
                            global_coverage.record_path_evidence(&evidence);
                            global_stops.insert(BeamStopReason::NodeBudget);
                            if stats.nodes_visited >= config.node_budget {
                                exhausted = true;
                            }
                        }
                    }
                }
            }
        }
        let kept = prune_layer(next_layer, config.beam_width, &mut stats, &mut global_stops)?;
        candidates.extend(kept.iter().map(|entry| entry.candidate.clone()));
        frontier = kept.into_iter().filter(|entry| entry.continuable).collect();
        stats
            .layer_widths
            .push(u32::try_from(frontier.len()).unwrap_or(u32::MAX));
    }

    candidates.sort_by(candidate_order);
    candidates.dedup_by(|left, right| left.path == right.path);
    let result_width = if protect_lethal_route && config.beam_width == 0 {
        1
    } else {
        usize::try_from(config.beam_width).unwrap_or(usize::MAX)
    };
    candidates.truncate(result_width);

    let evidence_cap = completeness_cap(&global_stops);
    let search_completeness = SearchCompleteness::with_evidence_cap(
        root_total,
        stats.roots_reliably_compared,
        evidence_cap,
    )
    .map_err(|error| PlannerError::Failed {
        message: format!("invalid Beam completeness: {error}"),
    })?;

    Ok(BeamSearchResult {
        candidates,
        rule_coverage: global_coverage.finish(),
        search_completeness,
        stats,
        stop_reasons: global_stops.into_iter().collect(),
        playbook_selection: playbook.selection_reason().clone(),
        lethal_first: lethal_first.assessment(),
    })
}

#[derive(Clone)]
struct BeamNode {
    states: Vec<WeightedState>,
    remaining: Vec<ActionTemplate>,
    first_action: LegalAction,
    path: CanonicalActionPath,
    evidence: PathEvidence,
    root_coverage: RuleCoverageReport,
    local_stops: BTreeSet<BeamStopReason>,
}

#[derive(Clone)]
struct FrontierEntry {
    node: BeamNode,
    candidate: BeamCandidate,
    protected: bool,
    continuable: bool,
}

enum ActionEvaluation {
    Expansion {
        states: Vec<WeightedState>,
        evidence: PathEvidence,
    },
    Boundary {
        reason: SearchBoundary,
        evidence: PathEvidence,
    },
    BudgetExhausted {
        evidence: PathEvidence,
    },
}

fn reserve_candidate(stats: &mut BeamSearchStats, limit: u64) -> bool {
    if stats.candidates_evaluated >= limit {
        return false;
    }
    stats.candidates_evaluated = stats.candidates_evaluated.saturating_add(1);
    true
}

fn coverage_for_root(
    request: &PlannerRequest,
    root: &LegalAction,
) -> Result<RuleCoverageReport, PlannerError> {
    let mut coverage = CoverageCollector::for_request(request);
    coverage
        .record_root_action(root)
        .map_err(|error| PlannerError::Failed {
            message: format!("root coverage binding failed: {error}"),
        })?;
    Ok(coverage.finish())
}

fn remaining_after_path(
    templates: &[ActionTemplate],
    path: &CanonicalActionPath,
) -> Vec<ActionTemplate> {
    path.iter().fold(templates.to_vec(), |remaining, action| {
        templates_after_action(&remaining, action.kind(), action.source())
    })
}

fn evaluate_action(
    request: &PlannerRequest,
    node: &BeamNode,
    action: SimAction,
    path: &CanonicalActionPath,
    opponent: PlayerId,
    stats: &mut BeamSearchStats,
) -> Result<ActionEvaluation, PlannerError> {
    stats.max_depth_reached = stats
        .max_depth_reached
        .max(u16::try_from(path.len()).unwrap_or(u16::MAX));
    let config = request.search_config();
    let sim_request = SimRequest {
        seed: path.seed(config.seed),
        branch_budget: config.sim_branch_budget,
    };
    let context = request.resources().sim_context();
    let mut evidence = node.evidence.clone();
    let mut states = Vec::new();
    let mut required_nodes = 0_u64;
    let mut first_boundary = None;

    for weighted in &node.states {
        if weighted.state.turn_ended
            || effective_life(&weighted.state, opponent).is_some_and(|life| life <= 0)
        {
            states.push(weighted.clone());
            continue;
        }
        let outcome =
            apply_action(&weighted.state, &action, &context, &sim_request).map_err(|error| {
                PlannerError::Failed {
                    message: format!("Beam simulation failed: {error}"),
                }
            })?;
        match expand_outcome(outcome, weighted.probability, &mut evidence) {
            OutcomeExpansion::Expanded {
                states: expanded,
                node_count,
                ..
            } => {
                required_nodes = required_nodes.saturating_add(node_count);
                states.extend(expanded);
            }
            OutcomeExpansion::Boundary(reason) => {
                if first_boundary.is_none() {
                    first_boundary = Some(reason);
                }
            }
        }
    }

    stats.simulation_branches_examined = stats
        .simulation_branches_examined
        .saturating_add(required_nodes);
    if let Some(reason) = first_boundary {
        return Ok(ActionEvaluation::Boundary { reason, evidence });
    }
    if stats
        .nodes_visited
        .checked_add(required_nodes)
        .is_none_or(|used| used > config.node_budget)
    {
        return Ok(ActionEvaluation::BudgetExhausted { evidence });
    }
    stats.nodes_visited = stats.nodes_visited.saturating_add(required_nodes);
    let states = normalize_states(states)?;
    if !weighted_probability_mass_complete(&states) {
        return Ok(ActionEvaluation::Boundary {
            reason: SearchBoundary::IncompleteProbabilityMass,
            evidence,
        });
    }
    Ok(ActionEvaluation::Expansion { states, evidence })
}

fn normalize_states(states: Vec<WeightedState>) -> Result<Vec<WeightedState>, PlannerError> {
    let mut keyed = states
        .into_iter()
        .map(|weighted| {
            let key =
                serde_json::to_vec(&weighted.state).map_err(|error| PlannerError::Failed {
                    message: format!("failed to canonicalize simulated state: {error}"),
                })?;
            Ok((key, weighted))
        })
        .collect::<Result<Vec<_>, PlannerError>>()?;
    keyed.sort_by(|left, right| {
        left.0.cmp(&right.0).then_with(|| {
            left.1
                .probability
                .to_bits()
                .cmp(&right.1.probability.to_bits())
        })
    });

    let mut normalized: Vec<(Vec<u8>, WeightedState)> = Vec::new();
    for (key, weighted) in keyed {
        if let Some((previous_key, previous)) = normalized.last_mut() {
            if *previous_key == key {
                previous.probability += weighted.probability;
                continue;
            }
        }
        normalized.push((key, weighted));
    }
    Ok(normalized
        .into_iter()
        .map(|(_, weighted)| weighted)
        .collect())
}

fn finalize_node_stops(
    node: &mut BeamNode,
    config: SearchConfig,
    opponent: PlayerId,
    global: &mut BTreeSet<BeamStopReason>,
) {
    if !node.evidence.exact() {
        node.local_stops.insert(BeamStopReason::SampledSimulation);
    }
    if node.evidence.runtime_unknown()
        || node
            .evidence
            .rule_uses()
            .iter()
            .any(|rule_use| rule_use.status == RuleStatus::Unknown)
    {
        node.local_stops.insert(BeamStopReason::MissingEvidence);
    }
    let relevant = node
        .remaining
        .iter()
        .filter(|template| template_relevant(template, &node.states, opponent))
        .collect::<Vec<_>>();
    if relevant
        .iter()
        .any(|template| !template.has_only_recheckable_gates())
    {
        node.local_stops.insert(BeamStopReason::MissingEvidence);
    }
    if u16::try_from(node.path.len()).unwrap_or(u16::MAX) >= config.max_depth
        && relevant
            .iter()
            .any(|template| template.has_only_recheckable_gates())
    {
        node.local_stops.insert(BeamStopReason::MaxDepth);
    }
    global.extend(node.local_stops.iter().copied());
}

fn node_can_continue(node: &BeamNode, opponent: PlayerId) -> bool {
    if node.local_stops.contains(&BeamStopReason::MaxDepth) {
        return false;
    }
    let has_live = node.states.iter().any(|weighted| {
        !weighted.state.turn_ended
            && effective_life(&weighted.state, opponent).is_some_and(|life| life > 0)
    });
    has_live
        && node.remaining.iter().any(|template| {
            template.has_only_recheckable_gates()
                && template_relevant(template, &node.states, opponent)
        })
}

fn candidate_for_node(
    request: &PlannerRequest,
    initial: &SimGameState,
    node: &BeamNode,
    playbook: &SelectedPlaybook<'_>,
    confidence_policy: &ConfidencePolicy,
    opponent: PlayerId,
) -> Result<Option<BeamCandidate>, PlannerError> {
    // Evidence quality belongs to confidence/completeness, never to the
    // strategic state score. Chance downside is handled separately below.
    let (score, chance_score) = score_group(initial, &node.states, playbook)?;
    let explanation = explain_score(&score, playbook);
    let lethal = classify_scenario_lethal(&node.evidence, &node.states, opponent);
    let mut coverage = CoverageCollector::for_request(request);
    coverage
        .record_root_action(&node.first_action)
        .map_err(|error| PlannerError::Failed {
            message: format!("candidate root coverage binding failed: {error}"),
        })?;
    coverage.record_path_evidence(&node.evidence);
    let coverage = coverage.finish();
    let origins = node
        .path
        .iter()
        .map(|action| action.origin())
        .collect::<Vec<_>>();
    let simulation = if !node.evidence.exact() {
        SimulationEvidence::Sampled {
            truncated: node.evidence.truncated(),
        }
    } else if node.evidence.had_exact_chance() {
        SimulationEvidence::Exact {
            probability_mass_complete: true,
            truncated: node.evidence.truncated(),
        }
    } else {
        SimulationEvidence::Deterministic {
            truncated: node.evidence.truncated(),
        }
    };
    let simulations = vec![simulation; node.path.len()];
    let confidence = match line_confidence(
        confidence_policy,
        &LineEvidence {
            rule_coverage: &coverage,
            action_origins: &origins,
            simulations: &simulations,
        },
    ) {
        Ok(confidence) => confidence,
        Err(PlannerError::Unavailable(UnavailableReason::NoReliableActions)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let follow_up = node
        .path
        .iter()
        .skip(1)
        .copied()
        .map(|action| {
            action
                .planned_action()
                .expect("Beam follow-up actions are simulator-only")
        })
        .collect::<Vec<PlannedAction>>();
    let complete =
        node.local_stops.is_empty() && node.evidence.exact() && !node.evidence.truncated();
    let line = AdviceLine {
        first_action: node.first_action,
        follow_up,
        score,
        chance: chance_score.summary(&node.evidence),
        lethal,
        confidence,
        complete,
        win_point: explanation.win_point,
        explanation: explanation.explanation,
        risks: risks_for_node(node, &chance_score),
    };
    Ok(Some(BeamCandidate {
        line,
        path: node.path.clone(),
        path_seed: node.path.seed(request.search_config().seed),
        chance_score,
        evidence: node.evidence.clone(),
        rule_coverage: coverage,
        lethal_first_route: false,
    }))
}

fn score_group(
    initial: &SimGameState,
    states: &[WeightedState],
    playbook: &SelectedPlaybook<'_>,
) -> Result<(ScoreBreakdown, ChanceScore), PlannerError> {
    let masses = fixed_probability_units(states).ok_or_else(|| PlannerError::Failed {
        message: "chance probabilities cannot be represented by the fixed score scale".into(),
    })?;
    let scores = states
        .iter()
        .map(|weighted| score_state(initial, &weighted.state, playbook, 0, 0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| PlannerError::Failed {
            message: format!("state scoring failed: {error}"),
        })?;
    let mut components = Vec::with_capacity(ScoreFeature::ALL.len());
    for feature in ScoreFeature::ALL {
        let mut numerator = 0_i128;
        let mut weight = None;
        for (score, mass) in scores.iter().zip(&masses) {
            let component = score
                .components()
                .iter()
                .find(|component| component.feature == feature)
                .expect("scorer emits every registered feature");
            numerator = numerator
                .checked_add(i128::from(component.raw) * *mass)
                .ok_or_else(|| PlannerError::Failed {
                    message: "fixed score aggregation overflowed".into(),
                })?;
            weight = Some(component.weight_milli);
        }
        let raw = div_round_nearest(numerator, PROBABILITY_SCALE);
        components.push(ScoreComponent {
            feature,
            raw: i32::try_from(raw).map_err(|_| PlannerError::Failed {
                message: "expected score feature exceeds i32".into(),
            })?,
            weight_milli: weight.unwrap_or_else(|| playbook.weight(feature)),
        });
    }
    let aggregate = ScoreBreakdown::new(components).map_err(|error| PlannerError::Failed {
        message: format!("invalid expected score breakdown: {error}"),
    })?;
    let totals = scores
        .iter()
        .map(ScoreBreakdown::total_milli)
        .collect::<Vec<_>>();
    // Rank the complete chance group from branch totals and round only once.
    // The per-feature breakdown above remains an explanatory projection and
    // cannot introduce a second rounding into the actual ordering formula.
    let expected_numerator = totals
        .iter()
        .zip(&masses)
        .try_fold(0_i128, |sum, (total, mass)| {
            total
                .checked_mul(*mass)
                .and_then(|weighted| sum.checked_add(weighted))
        })
        .ok_or_else(|| PlannerError::Failed {
            message: "expected score aggregation overflowed".into(),
        })?;
    let expected = div_round_nearest(expected_numerator, PROBABILITY_SCALE);
    let mut downside_numerator = 0_i128;
    for (total, mass) in totals.iter().zip(&masses) {
        let downside = expected.saturating_sub(*total).max(0);
        downside_numerator = downside_numerator
            .checked_add(
                downside
                    .checked_mul(*mass)
                    .ok_or_else(|| PlannerError::Failed {
                        message: "downside score multiplication overflowed".into(),
                    })?,
            )
            .ok_or_else(|| PlannerError::Failed {
                message: "downside score aggregation overflowed".into(),
            })?;
    }
    let downside = div_round_nearest(downside_numerator, PROBABILITY_SCALE);
    let risk_adjustment = div_round_nearest(
        downside
            .checked_mul(i128::from(playbook.risk_weight_milli()))
            .ok_or_else(|| PlannerError::Failed {
                message: "risk adjustment overflowed".into(),
            })?,
        1_000,
    );
    let rank = expected
        .checked_sub(risk_adjustment)
        .ok_or_else(|| PlannerError::Failed {
            message: "risk-adjusted score overflowed".into(),
        })?;
    let min = totals.iter().copied().min().unwrap_or(expected);
    let max = totals.iter().copied().max().unwrap_or(expected);
    Ok((
        aggregate,
        ChanceScore {
            expected_milli: chance_value_i64(expected, "expected")?,
            downside_milli: chance_value_i64(downside, "downside")?,
            rank_milli: chance_value_i64(rank, "risk-adjusted")?,
            min_milli: chance_value_i64(min, "minimum")?,
            max_milli: chance_value_i64(max, "maximum")?,
            probability_mass_units: chance_value_i64(masses.iter().sum(), "probability mass")?,
        },
    ))
}

fn chance_value_i64(value: i128, label: &str) -> Result<i64, PlannerError> {
    i64::try_from(value).map_err(|_| PlannerError::Failed {
        message: format!("{label} chance score exceeds the public i64 contract"),
    })
}

fn fixed_probability_units(states: &[WeightedState]) -> Option<Vec<i128>> {
    if !weighted_probability_mass_complete(states) {
        return None;
    }
    let mut masses = states
        .iter()
        .map(|weighted| probability_units(weighted.probability))
        .collect::<Option<Vec<_>>>()?;
    for mass in &mut masses {
        // Preserve every strictly positive simulator branch. Tiny exact
        // probabilities receive the smallest representable unit and the
        // deterministic largest-branch adjustment below keeps total mass 1.
        *mass = (*mass).max(1);
    }
    let total = masses
        .iter()
        .try_fold(0_i128, |sum, mass| sum.checked_add(*mass))?;
    let delta = PROBABILITY_SCALE.checked_sub(total)?;
    let adjust_index = masses
        .iter()
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            left.cmp(right).then_with(|| right_index.cmp(left_index))
        })?
        .0;
    masses[adjust_index] = masses[adjust_index].checked_add(delta)?;
    (masses[adjust_index] > 0 && masses.iter().sum::<i128>() == PROBABILITY_SCALE).then_some(masses)
}

fn div_round_nearest(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0);
    if numerator >= 0 {
        numerator.saturating_add(denominator / 2) / denominator
    } else {
        numerator.saturating_sub(denominator / 2) / denominator
    }
}

fn risks_for_node(node: &BeamNode, chance: &ChanceScore) -> Vec<AdviceRisk> {
    let mut risks = Vec::new();
    if !node.evidence.exact() {
        risks.push(AdviceRisk {
            kind: AdviceRiskKind::Randomness,
            code: "search.sampled".into(),
            detail: format!(
                "Ranked from all {} deterministic samples; this is an estimate.",
                node.evidence.sample_count()
            ),
        });
    } else if node.evidence.had_exact_chance() && chance.downside_milli() > 0 {
        risks.push(AdviceRisk {
            kind: AdviceRiskKind::Randomness,
            code: "search.exact_chance_downside".into(),
            detail: format!(
                "Exact chance group: expected {}, downside {}, range {}..{} milli-points.",
                chance.expected_milli(),
                chance.downside_milli(),
                chance.min_milli(),
                chance.max_milli()
            ),
        });
    }
    if node.evidence.runtime_unknown() {
        risks.push(AdviceRisk {
            kind: AdviceRiskKind::RuntimeUnknown,
            code: "rules.runtime_unknown".into(),
            detail: "The simulator reported a structured unknown rule.".into(),
        });
    }
    if node.path.len() > 1 {
        risks.push(AdviceRisk {
            kind: AdviceRiskKind::DerivedLegality,
            code: "actions.simulated_follow_up".into(),
            detail: "Only the first action is server-confirmed; later actions are conservative previews."
                .into(),
        });
    }
    if node.local_stops.iter().any(|reason| {
        matches!(
            reason,
            BeamStopReason::CandidateBudget
                | BeamStopReason::NodeBudget
                | BeamStopReason::MaxDepth
                | BeamStopReason::BeamWidth
        )
    }) {
        risks.push(AdviceRisk {
            kind: AdviceRiskKind::SearchTruncated,
            code: "search.truncated".into(),
            detail: "A fixed search budget stopped this route from being exhaustive.".into(),
        });
    }
    if node.local_stops.contains(&BeamStopReason::MissingEvidence)
        || node.local_stops.contains(&BeamStopReason::ProjectionGap)
    {
        risks.push(AdviceRisk {
            kind: AdviceRiskKind::RuleCoverage,
            code: "search.missing_evidence".into(),
            detail: "A known rule or future legal-action coordinate could not be proved.".into(),
        });
    }
    risks
}

fn prune_layer(
    mut entries: Vec<FrontierEntry>,
    beam_width: u32,
    stats: &mut BeamSearchStats,
    stops: &mut BTreeSet<BeamStopReason>,
) -> Result<Vec<FrontierEntry>, PlannerError> {
    entries.sort_by(frontier_order);
    let mut seen = BTreeSet::<Vec<u8>>::new();
    let mut deduplicated = Vec::new();
    for entry in entries {
        let key = canonical_node_key(&entry.node)?;
        if seen.insert(key) {
            deduplicated.push(entry);
        } else {
            stats.deduplicated_nodes = stats.deduplicated_nodes.saturating_add(1);
        }
    }
    let width = usize::try_from(beam_width).unwrap_or(usize::MAX);
    if deduplicated.len() > width {
        stops.insert(BeamStopReason::BeamWidth);
        deduplicated.truncate(width);
    }
    Ok(deduplicated)
}

fn canonical_node_key(node: &BeamNode) -> Result<Vec<u8>, PlannerError> {
    serde_json::to_vec(&(
        node.path.len(),
        &node.states,
        &node.remaining,
        &node.evidence,
        &node.root_coverage,
        &node.local_stops,
    ))
    .map_err(|error| PlannerError::Failed {
        message: format!("failed to encode canonical Beam dedup key: {error}"),
    })
}

fn frontier_order(left: &FrontierEntry, right: &FrontierEntry) -> Ordering {
    right
        .protected
        .cmp(&left.protected)
        .then_with(|| candidate_order(&left.candidate, &right.candidate))
}

fn candidate_order(left: &BeamCandidate, right: &BeamCandidate) -> Ordering {
    right
        .lethal_first_route
        .cmp(&left.lethal_first_route)
        .then_with(|| guaranteed_rank(right.line.lethal).cmp(&guaranteed_rank(left.line.lethal)))
        .then_with(|| {
            chance_path_order(
                &left.chance_score,
                &left.path,
                &right.chance_score,
                &right.path,
            )
        })
}

fn chance_path_order(
    left_score: &ChanceScore,
    left_path: &CanonicalActionPath,
    right_score: &ChanceScore,
    right_path: &CanonicalActionPath,
) -> Ordering {
    right_score
        .rank_milli
        .cmp(&left_score.rank_milli)
        .then_with(|| left_path.cmp(right_path))
}

const fn guaranteed_rank(assessment: LethalAssessment) -> u8 {
    match assessment {
        LethalAssessment::Guaranteed => 1,
        LethalAssessment::Probabilistic { .. }
        | LethalAssessment::Estimated { .. }
        | LethalAssessment::Unknown
        | LethalAssessment::None => 0,
    }
}

fn is_selected_lethal_route(
    path: &CanonicalActionPath,
    selected_path: Option<&CanonicalActionPath>,
) -> bool {
    selected_path.is_some_and(|selected| path == selected)
}

fn is_lethal_prefix(
    path: &CanonicalActionPath,
    selected_path: Option<&CanonicalActionPath>,
) -> bool {
    let Some(selected) = selected_path else {
        return false;
    };
    if path.is_empty() || path.len() > selected.len() {
        return false;
    }
    selected.as_slice().starts_with(path.as_slice())
}

fn record_root_boundary(
    reason: SearchBoundary,
    stats: &mut BeamSearchStats,
    stops: &mut BTreeSet<BeamStopReason>,
) {
    let stop = stop_for_boundary(&reason);
    match reason {
        SearchBoundary::Unsupported(_) | SearchBoundary::Lazy => {
            stats.unsupported_roots = stats.unsupported_roots.saturating_add(1);
        }
        SearchBoundary::PlayerChoice
        | SearchBoundary::MissingEvidence
        | SearchBoundary::IncompleteProbabilityMass => {
            stats.unknown_roots = stats.unknown_roots.saturating_add(1);
        }
    }
    stops.insert(stop);
}

const fn stop_for_boundary(reason: &SearchBoundary) -> BeamStopReason {
    match reason {
        SearchBoundary::PlayerChoice => BeamStopReason::PlayerChoice,
        SearchBoundary::MissingEvidence => BeamStopReason::MissingEvidence,
        SearchBoundary::IncompleteProbabilityMass => BeamStopReason::IncompleteProbabilityMass,
        SearchBoundary::Lazy => BeamStopReason::LazySimulation,
        SearchBoundary::Unsupported(_) => BeamStopReason::UnsupportedSimulation,
    }
}

const fn beam_stop_for_lethal(reason: LethalStopReason) -> BeamStopReason {
    match reason {
        LethalStopReason::MissingOpponent => BeamStopReason::MissingOpponent,
        LethalStopReason::CandidateBudget => BeamStopReason::CandidateBudget,
        LethalStopReason::NodeBudget => BeamStopReason::NodeBudget,
        LethalStopReason::MaxDepth => BeamStopReason::MaxDepth,
        LethalStopReason::PlayerChoice => BeamStopReason::PlayerChoice,
        LethalStopReason::SampledSimulation => BeamStopReason::SampledSimulation,
        LethalStopReason::LazySimulation => BeamStopReason::LazySimulation,
        LethalStopReason::UnsupportedSimulation => BeamStopReason::UnsupportedSimulation,
        LethalStopReason::MissingEvidence => BeamStopReason::MissingEvidence,
        LethalStopReason::IncompleteProbabilityMass => BeamStopReason::IncompleteProbabilityMass,
    }
}

fn completeness_cap(stops: &BTreeSet<BeamStopReason>) -> Confidence {
    let hard_gap = stops.iter().any(|reason| {
        matches!(
            reason,
            BeamStopReason::MissingOpponent
                | BeamStopReason::MissingState
                | BeamStopReason::PlayerChoice
                | BeamStopReason::LazySimulation
                | BeamStopReason::UnsupportedSimulation
                | BeamStopReason::MissingEvidence
                | BeamStopReason::IncompleteProbabilityMass
                | BeamStopReason::ProjectionGap
        )
    });
    if hard_gap {
        return Confidence::ratio(1, 2);
    }
    if stops.contains(&BeamStopReason::SampledSimulation) {
        return Confidence::ratio(7, 10);
    }
    if stops.iter().any(|reason| {
        matches!(
            reason,
            BeamStopReason::CandidateBudget
                | BeamStopReason::NodeBudget
                | BeamStopReason::MaxDepth
                | BeamStopReason::BeamWidth
        )
    }) {
        return Confidence::ratio(3, 4);
    }
    Confidence::ONE
}

fn empty_result(
    playbook: &SelectedPlaybook<'_>,
    stats: BeamSearchStats,
    stops: BTreeSet<BeamStopReason>,
    lethal_first: LethalAssessment,
    rule_coverage: RuleCoverageReport,
) -> Result<BeamSearchResult, PlannerError> {
    let completeness = SearchCompleteness::with_evidence_cap(
        stats.legal_root_total,
        stats.roots_reliably_compared,
        completeness_cap(&stops),
    )
    .map_err(|error| PlannerError::Failed {
        message: format!("invalid empty Beam completeness: {error}"),
    })?;
    Ok(BeamSearchResult {
        candidates: Vec::new(),
        rule_coverage,
        search_completeness: completeness,
        stats,
        stop_reasons: stops.into_iter().collect(),
        playbook_selection: playbook.selection_reason().clone(),
        lethal_first,
    })
}

fn warnings_for_result(result: &BeamSearchResult) -> Vec<AdviceWarning> {
    let mut warnings = Vec::new();
    if result.stop_reasons.iter().any(|reason| {
        matches!(
            reason,
            BeamStopReason::CandidateBudget
                | BeamStopReason::NodeBudget
                | BeamStopReason::MaxDepth
                | BeamStopReason::BeamWidth
        )
    }) {
        warnings.push(AdviceWarning {
            kind: AdviceWarningKind::BudgetExhausted,
            code: "search.fixed_budget_exhausted".into(),
            message: "The fixed search budget stopped before every continuation was compared."
                .into(),
        });
    }
    if result.search_completeness.ratio() < Confidence::ONE {
        warnings.push(AdviceWarning {
            kind: AdviceWarningKind::SearchIncomplete,
            code: "search.incomplete".into(),
            message: "Some legal roots, chance mass, or future coordinates could not be compared reliably."
                .into(),
        });
    }
    warnings
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hs_advisor_api::{ActionKind, ServerActionRef};
    use hs_sim::{SimEntity, SimEntityActionState, SimPlayer};
    use hs_types::{BuildId, BuildRange, CardId, CardType, EntityId, GamePhase, PlayerId, Zone};

    use super::*;
    use crate::{Playbook, PlaybookApplicability, PlaybookCatalog};

    const FRIENDLY: PlayerId = PlayerId(1);
    const OPPONENT: PlayerId = PlayerId(2);

    fn score_state_fixture(friendly_health: i32, opponent_health: i32) -> SimGameState {
        let mut state = SimGameState {
            build_id: Some(BuildId(1)),
            friendly_player: Some(FRIENDLY),
            current_player: Some(FRIENDLY),
            phase: GamePhase::Playing,
            ..SimGameState::default()
        };
        state.players.insert(
            FRIENDLY,
            SimPlayer {
                player_id: FRIENDLY,
                hero_health: friendly_health,
                armor: 0,
                mana: 0,
                max_mana: 0,
                overloaded: 0,
                deck_count: 0,
                hand_count: 0,
                fatigue: 0,
            },
        );
        state.players.insert(
            OPPONENT,
            SimPlayer {
                player_id: OPPONENT,
                hero_health: opponent_health,
                armor: 0,
                mana: 0,
                max_mana: 0,
                overloaded: 0,
                deck_count: 0,
                hand_count: 0,
                fatigue: 0,
            },
        );
        for (entity_id, controller, card_id, health) in [
            (EntityId(10), FRIENDLY, "HERO_FRIENDLY", friendly_health),
            (EntityId(11), OPPONENT, "HERO_OPPONENT", opponent_health),
        ] {
            state.entities.insert(
                entity_id,
                SimEntity {
                    entity_id,
                    card_id: Some(CardId::new(card_id)),
                    card_type: Some(CardType::Hero),
                    zone: Zone::Play,
                    controller: Some(controller),
                    attack: 0,
                    health,
                    damage: 0,
                    cost: 0,
                    action: SimEntityActionState {
                        health_known: true,
                        ..SimEntityActionState::default()
                    },
                    simulated: false,
                },
            );
        }
        state
    }

    fn selected_test_playbook() -> (PlaybookCatalog, BuildId) {
        let mut weights = ScoreFeature::ALL
            .into_iter()
            .map(|feature| (feature, 0))
            .collect::<BTreeMap<_, _>>();
        weights.insert(ScoreFeature::FriendlyEffectiveHealth, 1);
        weights.insert(ScoreFeature::OpponentEffectiveHealth, 1);
        let build = BuildId(1);
        let catalog = PlaybookCatalog::new([Playbook {
            schema_version: crate::PLAYBOOK_SCHEMA_VERSION,
            id: "generic".into(),
            priority: 0,
            applies_to: PlaybookApplicability {
                build_range: BuildRange {
                    min: BuildId(0),
                    max: None,
                },
                deck_signatures: Vec::new(),
            },
            weights,
            risk_weight_milli: 1_000,
            win_points: Vec::new(),
            explanations: BTreeMap::new(),
        }])
        .expect("valid test Playbook");
        (catalog, build)
    }

    #[test]
    fn chance_rank_uses_branch_totals_and_rounds_expected_only_once() {
        let (catalog, build) = selected_test_playbook();
        let playbook = catalog.select(build, None).expect("generic Playbook");
        let initial = score_state_fixture(1, 1);
        let states = vec![
            WeightedState {
                state: score_state_fixture(1, 0),
                probability: 0.5,
            },
            WeightedState {
                state: score_state_fixture(0, 1),
                probability: 0.5,
            },
        ];

        let (breakdown, chance) =
            score_group(&initial, &states, &playbook).expect("score complete chance group");

        // Per-feature explanatory projection rounds both 0.5 raw values to 1,
        // but both actual branch totals are exactly 1. Ranking must stay 1.
        assert_eq!(breakdown.total_milli(), 2);
        assert_eq!(chance.expected_milli(), 1);
        assert_eq!(chance.downside_milli(), 0);
        assert_eq!(chance.rank_milli(), 1);
    }

    #[test]
    fn fixed_mass_keeps_a_tiny_positive_exact_branch() {
        let states = vec![
            WeightedState {
                state: SimGameState::default(),
                probability: 1e-12,
            },
            WeightedState {
                state: SimGameState::default(),
                probability: 1.0 - 1e-12,
            },
        ];

        let units = fixed_probability_units(&states).expect("representable exact mass");
        assert!(units[0] > 0);
        assert!(units[1] > units[0]);
        assert_eq!(units.iter().sum::<i128>(), PROBABILITY_SCALE);
    }

    #[test]
    fn only_guaranteed_lethal_has_a_sorting_rank() {
        assert_eq!(guaranteed_rank(LethalAssessment::Guaranteed), 1);
        assert_eq!(
            guaranteed_rank(LethalAssessment::Probabilistic {
                probability: Confidence::ratio(1, 100),
            }),
            0
        );
        assert_eq!(
            guaranteed_rank(LethalAssessment::Estimated {
                probability: Confidence::ratio(1, 2),
                sample_count: 2,
            }),
            0
        );
        assert_eq!(guaranteed_rank(LethalAssessment::Unknown), 0);
    }

    #[test]
    fn equal_risk_adjusted_scores_tie_break_only_by_canonical_path() {
        fn score(expected_milli: i64) -> ChanceScore {
            ChanceScore {
                expected_milli,
                downside_milli: expected_milli.saturating_sub(10),
                rank_milli: 10,
                min_milli: 0,
                max_milli: expected_milli,
                probability_mass_units: i64::try_from(PROBABILITY_SCALE).unwrap(),
            }
        }
        fn path(option_index: u32) -> CanonicalActionPath {
            CanonicalActionPath::from_root(LegalAction {
                server_ref: ServerActionRef {
                    options_id: 1,
                    option_index,
                    sub_option_index: None,
                    target: None,
                    position: None,
                },
                kind: ActionKind::EndTurn,
                source: None,
            })
        }

        let earlier = path(1);
        let later = path(2);
        assert_eq!(
            chance_path_order(&score(20), &earlier, &score(1_000), &later),
            Ordering::Less,
            "expected/downside do not become an undocumented tie-break"
        );
    }

    #[test]
    fn exact_chance_with_downside_emits_a_stable_randomness_risk() {
        let first_action = LegalAction {
            server_ref: ServerActionRef {
                options_id: 1,
                option_index: 2,
                sub_option_index: None,
                target: None,
                position: None,
            },
            kind: ActionKind::EndTurn,
            source: None,
        };
        let mut evidence = PathEvidence::root();
        evidence.mark_exact_chance();
        let node = BeamNode {
            states: Vec::new(),
            remaining: Vec::new(),
            first_action,
            path: CanonicalActionPath::from_root(first_action),
            evidence,
            root_coverage: RuleCoverageReport::default(),
            local_stops: BTreeSet::new(),
        };
        let chance = ChanceScore {
            expected_milli: 1_000,
            downside_milli: 250,
            rank_milli: 750,
            min_milli: 0,
            max_milli: 2_000,
            probability_mass_units: i64::try_from(PROBABILITY_SCALE).unwrap(),
        };

        let risks = risks_for_node(&node, &chance);

        assert_eq!(risks.len(), 1);
        assert_eq!(risks[0].kind, AdviceRiskKind::Randomness);
        assert_eq!(risks[0].code, "search.exact_chance_downside");
        assert!(risks[0].detail.contains("downside 250"));
    }
}
