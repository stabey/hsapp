//! Deterministic planning primitives for the Hearthstone advisor.
//!
//! Server Options have one adapter, while request preparation, simulation
//! evidence, and advice confidence stay deterministic and platform-free.

mod beam;
mod confidence;
mod coverage;
mod engine;
mod legal_actions;
mod lethal;
mod node;
mod playbook;
mod score;
mod search;
mod sim_action;

pub use beam::{
    search_beam, BeamCandidate, BeamEngine, BeamSearchResult, BeamSearchStats, BeamStopReason,
    ChanceScore, SEARCH_COMPLETENESS_POLICY_VERSION,
};
pub use confidence::{line_confidence, result_confidence, LineEvidence, SimulationEvidence};
pub use coverage::{CoverageCollector, CoverageError};
pub use engine::{
    prepare_request, Engine, PlannerError, PlannerRequest, PlannerResources, PlannerResult,
};

pub use legal_actions::{
    is_current_action, legal_actions, LegalActionDiagnostic, LegalActionDiagnosticReason,
    LegalActionSet, LegalActionsUnavailable,
};
pub use lethal::{
    search_lethal, LethalEvidence, LethalRoute, LethalSearchResult, LethalSearchStats,
    LethalStopReason,
};
pub use node::{
    seed_for_path, CanonicalActionKey, CanonicalActionPath, CANONICAL_PATH_SEED_VERSION,
};
pub use playbook::{
    DeckCardCount, DeckSignature, DeckSignatureError, ExplanationTemplate, GenericFallbackReason,
    Playbook, PlaybookApplicability, PlaybookCatalog, PlaybookError, PlaybookSelectionReason,
    SelectedPlaybook, SideboardCardCount, WinPoint, WinPointComparison, MAX_ABS_WEIGHT_MILLI,
    MAX_RISK_WEIGHT_MILLI, PLAYBOOK_SCHEMA_VERSION,
};
pub use score::{explain_score, score_state, ScoreError, ScoreExplanation};
pub use search::PathEvidence;
pub use sim_action::{legal_action_to_sim, SimActionMappingError};
