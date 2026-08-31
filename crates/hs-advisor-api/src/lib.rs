//! Stable, serializable contracts shared by the advisor, UI, and replay tools.
//!
//! This crate contains data only. It does not decide whether an action is legal
//! and must not depend on observed state, simulation, storage, or UI crates.

use hs_types::EntityId;
use serde::{Deserialize, Serialize};

mod advice;
mod confidence;
mod coverage;
mod score;
mod search;

pub use advice::{
    ActionOrigin, AdviceCore, AdviceIdentity, AdviceLine, AdvicePayload, AdviceResult, AdviceRisk,
    AdviceRiskKind, AdviceWarning, AdviceWarningKind, ChanceSummary, InvalidChanceSummary,
    LethalAssessment, PlannedAction, UnavailableReason,
};
pub use confidence::{
    Confidence, ConfidenceBreakdown, ConfidenceFactor, ConfidencePolicy, ConfidenceReason,
    InvalidConfidence,
};
pub use coverage::{RuleCoverageItem, RuleCoverageReport};
pub use score::{InvalidScoreBreakdown, ScoreBreakdown, ScoreComponent, ScoreFeature};
pub use search::{InvalidSearchCompleteness, SearchCompleteness, SearchConfig};

/// The player-visible category of a server-issued action.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    PlayCard,
    Attack,
    UseHeroPower,
    ActivateLocation,
    EndTurn,
    /// The server option could not be classified from observed facts.
    Unknown,
}

/// Stable coordinates for one action in the current server Options packet.
///
/// The full identity also includes the game and the Options `issued_revision`
/// held by the caller. Entity ID alone is not an action identity: one entity can
/// expose multiple options, sub-options, targets, or board positions.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ServerActionRef {
    pub options_id: u64,
    pub option_index: u32,
    pub sub_option_index: Option<u32>,
    pub target: Option<EntityId>,
    pub position: Option<u8>,
}

/// A currently selectable server action, classified using observed facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct LegalAction {
    pub server_ref: ServerActionRef,
    pub kind: ActionKind,
    pub source: Option<EntityId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(position: Option<u8>, target: Option<i32>) -> LegalAction {
        LegalAction {
            server_ref: ServerActionRef {
                options_id: 17,
                option_index: 3,
                sub_option_index: Some(1),
                target: target.map(EntityId),
                position,
            },
            kind: ActionKind::PlayCard,
            source: Some(EntityId(42)),
        }
    }

    #[test]
    fn legal_action_serde_round_trip_is_exact() {
        let value = action(Some(2), Some(99));
        let json = serde_json::to_string(&value).expect("serialize action");
        let decoded: LegalAction = serde_json::from_str(&json).expect("deserialize action");
        assert_eq!(decoded, value);
    }

    #[test]
    fn server_coordinates_define_stable_order() {
        let mut actions = [
            action(Some(2), None),
            action(Some(1), None),
            action(None, None),
        ];
        actions.sort_by_key(|action| action.server_ref);
        assert_eq!(
            actions
                .iter()
                .map(|action| action.server_ref.position)
                .collect::<Vec<_>>(),
            vec![None, Some(1), Some(2)]
        );
    }
}
