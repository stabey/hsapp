//! Owned planner request boundary and synchronous engine contract.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use hs_advisor_api::{
    AdviceIdentity, AdvicePayload, AdviceResult, Confidence, ConfidenceReason, SearchConfig,
    UnavailableReason,
};
use hs_belief::{BeliefState, KnownGenerationSource, PublicBeliefInput};
use hs_observed_state::{state_hash, ObservedGameState};
use hs_sim::{CardLookup, PoolResolver, RuleLookup, SimContext};
use hs_types::{BuildId, Zone};
use serde::Serialize;
use thiserror::Error;

use crate::legal_actions::{legal_actions_with_templates, ActionTemplateSet};
use crate::{LegalActionSet, LegalActionsUnavailable};

/// Exact-Build resources owned by one planner job.
///
/// The extra auto-trait bounds make the bundle safe to move to a desktop
/// worker without introducing an async runtime into this crate.
#[derive(Clone)]
pub struct PlannerResources {
    build_id: BuildId,
    rules: Arc<dyn RuleLookup + Send + Sync>,
    cards: Arc<dyn CardLookup + Send + Sync>,
    pools: Arc<dyn PoolResolver + Send + Sync>,
}

impl PlannerResources {
    pub fn new(
        build_id: BuildId,
        rules: Arc<dyn RuleLookup + Send + Sync>,
        cards: Arc<dyn CardLookup + Send + Sync>,
        pools: Arc<dyn PoolResolver + Send + Sync>,
    ) -> Self {
        Self {
            build_id,
            rules,
            cards,
            pools,
        }
    }

    pub fn build_id(&self) -> BuildId {
        self.build_id
    }

    pub fn rules(&self) -> &(dyn RuleLookup + Send + Sync) {
        self.rules.as_ref()
    }

    pub fn cards(&self) -> &(dyn CardLookup + Send + Sync) {
        self.cards.as_ref()
    }

    pub fn pools(&self) -> &(dyn PoolResolver + Send + Sync) {
        self.pools.as_ref()
    }

    /// Borrow these owned resources in the shape required by `hs-sim`.
    pub fn sim_context(&self) -> SimContext<'_> {
        SimContext {
            build: self.build_id,
            pools: self.pools.as_ref(),
            rules: self.rules.as_ref(),
            cards: self.cards.as_ref(),
        }
    }
}

impl fmt::Debug for PlannerResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlannerResources")
            .field("build_id", &self.build_id)
            .finish_non_exhaustive()
    }
}

/// Fully owned and freshness-bound input to one synchronous planning job.
pub struct PlannerRequest {
    identity: AdviceIdentity,
    observed_state: ObservedGameState,
    /// Generated exactly once by the sole Options adapter during preparation.
    legal_actions: LegalActionSet,
    /// Full same-packet coordinates retained only for conservative simulated
    /// continuations. These are never current server-backed roots.
    action_templates: ActionTemplateSet,
    resources: PlannerResources,
    search_config: SearchConfig,
    belief: BeliefState,
}

impl PlannerRequest {
    pub fn identity(&self) -> &AdviceIdentity {
        &self.identity
    }

    pub fn observed_state(&self) -> &ObservedGameState {
        &self.observed_state
    }

    pub fn legal_actions(&self) -> &LegalActionSet {
        &self.legal_actions
    }

    pub(crate) fn action_templates(&self) -> &ActionTemplateSet {
        &self.action_templates
    }

    pub fn resources(&self) -> &PlannerResources {
        &self.resources
    }

    pub fn search_config(&self) -> SearchConfig {
        self.search_config
    }

    pub fn belief(&self) -> &BeliefState {
        &self.belief
    }
}

impl fmt::Debug for PlannerRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlannerRequest")
            .field("identity", &self.identity)
            .field("legal_actions", &self.legal_actions)
            .field(
                "action_template_count",
                &self.action_templates.templates().len(),
            )
            .field(
                "action_template_projection_gaps",
                &self.action_templates.has_projection_gaps(),
            )
            .field("resources", &self.resources)
            .field("search_config", &self.search_config)
            .finish_non_exhaustive()
    }
}

/// Deterministic planner output. Wall-clock publication time is added only at
/// the boundary where the result is published.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlannerResult {
    identity: AdviceIdentity,
    payload: AdvicePayload,
}

impl PlannerResult {
    /// Construct a reliable result. An engine that filtered every candidate as
    /// unsupported must report `NoReliableActions`, never synthesize EndTurn.
    ///
    /// Construction stays tied to the prepared request so an engine cannot
    /// under-report the number of legal roots or return an action that bypassed
    /// the sole Options adapter. Persisted/public interchange uses
    /// `AdviceResult`; this internal boundary is deliberately serialize-only.
    pub fn for_request(
        request: &PlannerRequest,
        payload: AdvicePayload,
    ) -> Result<Self, PlannerError> {
        if payload.lines.is_empty() {
            return Err(PlannerError::Unavailable(
                UnavailableReason::NoReliableActions,
            ));
        }

        let expected_roots = u64::try_from(request.legal_actions.actions.len()).unwrap_or(u64::MAX);
        if payload.search_completeness.total() != expected_roots {
            return Err(PlannerError::Failed {
                message: "search completeness total does not match prepared legal roots".into(),
            });
        }
        if payload
            .lines
            .iter()
            .any(|line| !request.legal_actions.actions.contains(&line.first_action))
        {
            return Err(PlannerError::Failed {
                message: "advice line first action is not a prepared legal root".into(),
            });
        }
        let represented_roots = payload
            .lines
            .iter()
            .map(|line| line.first_action)
            .collect::<BTreeSet<_>>()
            .len()
            .try_into()
            .unwrap_or(u64::MAX);
        if represented_roots > payload.search_completeness.reliably_compared() {
            return Err(PlannerError::Failed {
                message: "advice lines exceed reliably compared legal roots".into(),
            });
        }
        if payload.lines.iter().any(|line| {
            line.confidence
                .factors()
                .iter()
                .any(|factor| factor.reason == ConfidenceReason::SearchIncomplete)
        }) {
            return Err(PlannerError::Failed {
                message: "line confidence contains a result-level search factor".into(),
            });
        }

        let result_confidence = payload.confidence.confidence();
        let top_line_confidence = payload.lines[0].confidence.confidence();
        let completeness = payload.search_completeness.ratio();
        if result_confidence > top_line_confidence || result_confidence > completeness {
            return Err(PlannerError::Failed {
                message: "result confidence exceeds top-line or search-completeness evidence"
                    .into(),
            });
        }
        let search_factor = payload
            .confidence
            .factors()
            .iter()
            .find(|factor| factor.reason == ConfidenceReason::SearchIncomplete);
        if completeness < Confidence::ONE {
            if search_factor.is_none_or(|factor| factor.confidence != completeness) {
                return Err(PlannerError::Failed {
                    message: "incomplete search confidence reason does not match completeness"
                        .into(),
                });
            }
        } else if search_factor.is_some() {
            return Err(PlannerError::Failed {
                message: "complete search must not report SearchIncomplete".into(),
            });
        }
        Ok(Self {
            identity: request.identity.clone(),
            payload,
        })
    }

    pub fn identity(&self) -> &AdviceIdentity {
        &self.identity
    }

    pub fn payload(&self) -> &AdvicePayload {
        &self.payload
    }

    pub fn into_advice_result(self, generated_at: DateTime<Utc>) -> AdviceResult {
        AdviceResult::new(self.identity, self.payload, generated_at)
    }
}

/// Pure synchronous engine. Desktop scheduling owns debounce and background
/// execution; implementations remain safe to share between workers.
pub trait Engine: Send + Sync {
    fn plan(&self, request: PlannerRequest) -> Result<PlannerResult, PlannerError>;
}

/// Expected unavailability is data, while `Failed` is reserved for genuine
/// execution faults that should put the scheduler into its failed state.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum PlannerError {
    #[error("advice unavailable: {0:?}")]
    Unavailable(UnavailableReason),
    #[error("planner failed: {message}")]
    Failed { message: String },
}

impl From<UnavailableReason> for PlannerError {
    fn from(reason: UnavailableReason) -> Self {
        Self::Unavailable(reason)
    }
}

/// Prepare the only planner request for an observed snapshot.
///
/// All guards and identity fields consume this same owned snapshot. In
/// particular, server Options are projected exactly once and are never scanned
/// again to reconstruct root actions.
pub fn prepare_request(
    observed_state: ObservedGameState,
    resources: PlannerResources,
    search_config: SearchConfig,
) -> Result<PlannerRequest, PlannerError> {
    let build_id = observed_state
        .build_id
        .ok_or(UnavailableReason::MissingBuild)?;
    if resources.build_id != build_id {
        return Err(UnavailableReason::MissingBuildData.into());
    }

    let friendly = observed_state
        .friendly_player
        .ok_or(UnavailableReason::MissingFriendlyPlayer)?;
    if observed_state.current_player != Some(friendly) {
        return Err(UnavailableReason::NotFriendlyTurn.into());
    }
    if !observed_state.block_stack.is_empty() {
        return Err(UnavailableReason::BlocksOpen.into());
    }

    let (legal_actions, action_templates) =
        legal_actions_with_templates(&observed_state).map_err(map_legal_unavailable)?;
    debug_assert_eq!(action_templates.game_id(), legal_actions.game_id);
    debug_assert_eq!(action_templates.options_id(), legal_actions.options_id);
    debug_assert_eq!(
        action_templates.issued_revision(),
        legal_actions.issued_revision
    );
    if legal_actions.actions.is_empty() {
        return Err(UnavailableReason::NoReliableActions.into());
    }

    let identity = AdviceIdentity {
        game_id: legal_actions.game_id,
        state_revision: observed_state.revision,
        state_hash: state_hash(&observed_state),
        options_id: legal_actions.options_id,
        build_id,
    };
    let belief = belief_from_observed(&observed_state, friendly);

    Ok(PlannerRequest {
        identity,
        observed_state,
        legal_actions,
        action_templates,
        resources,
        search_config,
        belief,
    })
}

/// Project only publicly established opponent facts. This is deliberately the
/// sole observed-to-belief adapter so UI and engines cannot invent competing
/// interpretations of hidden entities.
fn belief_from_observed(state: &ObservedGameState, friendly: hs_types::PlayerId) -> BeliefState {
    let opponent_entities = state
        .entities
        .values()
        .filter(|entity| entity.controller.is_some_and(|owner| owner != friendly));

    let opponent_seen_cards = opponent_entities
        .clone()
        .filter(|entity| entity.revealed)
        .filter_map(|entity| entity.card_id.clone())
        .collect();
    let opponent_unknown_hand_count = opponent_entities
        .clone()
        .filter(|entity| {
            entity.zone == Zone::Hand && (!entity.revealed || entity.card_id.is_none())
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let known_generation_sources = opponent_entities
        .filter(|entity| entity.revealed)
        .filter_map(|entity| {
            let generated_card = entity.card_id.clone()?;
            let source = state.entity(entity.source_entity?)?;
            if !source.revealed {
                return None;
            }
            Some(KnownGenerationSource {
                generated_card,
                source_card: source.card_id.clone()?,
            })
        })
        .collect();

    BeliefState::from_public(PublicBeliefInput {
        opponent_seen_cards,
        opponent_unknown_hand_count,
        known_generation_sources,
    })
}

fn map_legal_unavailable(reason: LegalActionsUnavailable) -> PlannerError {
    let reason = match reason {
        LegalActionsUnavailable::MissingGame => UnavailableReason::MissingGame,
        LegalActionsUnavailable::MissingOptions => UnavailableReason::NoOptions,
        LegalActionsUnavailable::MissingFriendlyPlayer => UnavailableReason::MissingFriendlyPlayer,
        LegalActionsUnavailable::PendingChoices => UnavailableReason::PendingChoice,
    };
    PlannerError::Unavailable(reason)
}
