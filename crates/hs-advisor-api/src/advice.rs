use chrono::{DateTime, Utc};
use hs_belief::BeliefSummary;
use hs_types::{BuildId, EntityId, GameId};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    ActionKind, Confidence, ConfidenceBreakdown, LegalAction, RuleCoverageReport, ScoreBreakdown,
    SearchCompleteness,
};

/// Whether an action is directly backed by current server Options or is only a
/// conservative simulator continuation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOrigin {
    Server,
    Simulated,
}

/// An action shown as part of a planned line.
///
/// Deliberately contains no `ServerActionRef`: simulated continuations must not
/// masquerade as currently executable server options.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PlannedAction {
    pub kind: ActionKind,
    pub source: Option<EntityId>,
    pub target: Option<EntityId>,
    pub sub_option_index: Option<u32>,
    pub position: Option<u8>,
    origin: ActionOrigin,
}

impl PlannedAction {
    /// Construct a conservative continuation that is not backed by current
    /// server Options. The current server action belongs in
    /// [`AdviceLine::first_action`], never in `follow_up`.
    pub const fn simulated(
        kind: ActionKind,
        source: Option<EntityId>,
        target: Option<EntityId>,
        sub_option_index: Option<u32>,
        position: Option<u8>,
    ) -> Self {
        Self {
            kind,
            source,
            target,
            sub_option_index,
            position,
            origin: ActionOrigin::Simulated,
        }
    }

    pub const fn origin(&self) -> ActionOrigin {
        self.origin
    }
}

impl<'de> Deserialize<'de> for PlannedAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireAction {
            kind: ActionKind,
            source: Option<EntityId>,
            target: Option<EntityId>,
            sub_option_index: Option<u32>,
            position: Option<u8>,
            origin: ActionOrigin,
        }

        let wire = WireAction::deserialize(deserializer)?;
        if wire.origin != ActionOrigin::Simulated {
            return Err(D::Error::custom(
                "planned follow-up action origin must be simulated",
            ));
        }

        Ok(Self::simulated(
            wire.kind,
            wire.source,
            wire.target,
            wire.sub_option_index,
            wire.position,
        ))
    }
}

/// One-turn lethal classification. Confidence is kept separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LethalAssessment {
    None,
    Guaranteed,
    Probabilistic {
        probability: Confidence,
    },
    Estimated {
        probability: Confidence,
        sample_count: u64,
    },
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdviceRiskKind {
    Randomness,
    RuleCoverage,
    RuntimeUnknown,
    DerivedLegality,
    HiddenInformation,
    SearchIncomplete,
    SearchTruncated,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdviceRisk {
    pub kind: AdviceRiskKind,
    /// Stable machine-readable identifier.
    pub code: String,
    /// Human-readable context; never used as identity.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdviceWarningKind {
    NoReliableActions,
    MissingRule,
    RuntimeUnknown,
    InsufficientBeliefData,
    SearchIncomplete,
    BudgetExhausted,
    Other,
}

/// Structured reason why advice calculation is not currently eligible.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    MissingGame,
    MissingFriendlyPlayer,
    NotFriendlyTurn,
    NoOptions,
    PendingChoice,
    BlocksOpen,
    MissingBuild,
    MissingBuildData,
    NoReliableActions,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdviceWarning {
    pub kind: AdviceWarningKind,
    /// Stable machine-readable identifier.
    pub code: String,
    /// Human-readable presentation text; never used as identity.
    pub message: String,
}

/// Fixed-point chance-group values that actually determined line ordering.
///
/// `ScoreBreakdown` remains the per-feature explanatory projection. This
/// summary exposes the single-rounding expected value and downside formula so
/// consumers never have to infer the ranking score from rounded components.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ChanceSummary {
    available: bool,
    expected_milli: i64,
    downside_milli: i64,
    rank_milli: i64,
    min_milli: i64,
    max_milli: i64,
    probability_mass_units: i64,
    probability_scale: i64,
    estimated: bool,
    sample_count: u64,
}

impl ChanceSummary {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        expected_milli: i64,
        downside_milli: i64,
        rank_milli: i64,
        min_milli: i64,
        max_milli: i64,
        probability_mass_units: i64,
        probability_scale: i64,
        estimated: bool,
        sample_count: u64,
    ) -> Result<Self, InvalidChanceSummary> {
        if probability_scale <= 0 || probability_mass_units != probability_scale {
            return Err(InvalidChanceSummary(
                "probability mass must equal a positive scale",
            ));
        }
        if min_milli > expected_milli || expected_milli > max_milli {
            return Err(InvalidChanceSummary(
                "expected score must lie between minimum and maximum",
            ));
        }
        if downside_milli < 0 || rank_milli > expected_milli {
            return Err(InvalidChanceSummary(
                "downside must be non-negative and rank cannot exceed expected",
            ));
        }
        if estimated != (sample_count > 0) {
            return Err(InvalidChanceSummary(
                "estimated chance summaries require a positive sample count",
            ));
        }
        Ok(Self {
            available: true,
            expected_milli,
            downside_milli,
            rank_milli,
            min_milli,
            max_milli,
            probability_mass_units,
            probability_scale,
            estimated,
            sample_count,
        })
    }

    pub const fn available(self) -> bool {
        self.available
    }

    pub const fn expected_milli(self) -> i64 {
        self.expected_milli
    }

    pub const fn downside_milli(self) -> i64 {
        self.downside_milli
    }

    pub const fn rank_milli(self) -> i64 {
        self.rank_milli
    }

    pub const fn min_milli(self) -> i64 {
        self.min_milli
    }

    pub const fn max_milli(self) -> i64 {
        self.max_milli
    }

    pub const fn probability_mass_units(self) -> i64 {
        self.probability_mass_units
    }

    pub const fn probability_scale(self) -> i64 {
        self.probability_scale
    }

    pub const fn estimated(self) -> bool {
        self.estimated
    }

    pub const fn sample_count(self) -> u64 {
        self.sample_count
    }
}

impl<'de> Deserialize<'de> for ChanceSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct WireSummary {
            available: bool,
            expected_milli: i64,
            downside_milli: i64,
            rank_milli: i64,
            min_milli: i64,
            max_milli: i64,
            probability_mass_units: i64,
            probability_scale: i64,
            estimated: bool,
            sample_count: u64,
        }

        let wire = WireSummary::deserialize(deserializer)?;
        if !wire.available {
            let empty = wire.expected_milli == 0
                && wire.downside_milli == 0
                && wire.rank_milli == 0
                && wire.min_milli == 0
                && wire.max_milli == 0
                && wire.probability_mass_units == 0
                && wire.probability_scale == 0
                && !wire.estimated
                && wire.sample_count == 0;
            return empty
                .then(Self::default)
                .ok_or_else(|| D::Error::custom("unavailable chance summary must be empty"));
        }
        Self::new(
            wire.expected_milli,
            wire.downside_milli,
            wire.rank_milli,
            wire.min_milli,
            wire.max_milli,
            wire.probability_mass_units,
            wire.probability_scale,
            wire.estimated,
            wire.sample_count,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidChanceSummary(&'static str);

impl std::fmt::Display for InvalidChanceSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for InvalidChanceSummary {}

/// A ranked advice line. Only the first action is directly executable now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdviceLine {
    pub first_action: LegalAction,
    pub follow_up: Vec<PlannedAction>,
    pub score: ScoreBreakdown,
    #[serde(default)]
    pub chance: ChanceSummary,
    pub lethal: LethalAssessment,
    pub confidence: ConfidenceBreakdown,
    pub complete: bool,
    pub win_point: Option<String>,
    pub explanation: String,
    pub risks: Vec<AdviceRisk>,
}

/// Exact observed identity for which an advice result was computed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdviceIdentity {
    pub game_id: GameId,
    pub state_revision: u64,
    pub state_hash: String,
    pub options_id: u64,
    pub build_id: BuildId,
}

/// Deterministic payload produced from a fixed request and search configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdvicePayload {
    pub confidence: ConfidenceBreakdown,
    pub rule_coverage: RuleCoverageReport,
    pub opponent_belief_summary: BeliefSummary,
    pub search_completeness: SearchCompleteness,
    pub lines: Vec<AdviceLine>,
    pub explanation: String,
    pub warnings: Vec<AdviceWarning>,
}

/// The deterministic portion of an advice result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdviceCore {
    pub identity: AdviceIdentity,
    pub payload: AdvicePayload,
}

impl AdviceCore {
    pub fn new(identity: AdviceIdentity, payload: AdvicePayload) -> Self {
        Self { identity, payload }
    }
}

/// A publishable advice result.
///
/// Wall-clock publication metadata is structurally outside [`AdviceCore`], so
/// deterministic comparison and golden serialization can operate on `core()`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdviceResult {
    #[serde(flatten)]
    core: AdviceCore,
    generated_at: DateTime<Utc>,
}

impl AdviceResult {
    pub fn new(
        identity: AdviceIdentity,
        payload: AdvicePayload,
        generated_at: DateTime<Utc>,
    ) -> Self {
        Self::from_core(AdviceCore::new(identity, payload), generated_at)
    }

    pub fn from_core(core: AdviceCore, generated_at: DateTime<Utc>) -> Self {
        Self { core, generated_at }
    }

    pub fn core(&self) -> &AdviceCore {
        &self.core
    }

    pub fn identity(&self) -> &AdviceIdentity {
        &self.core.identity
    }

    pub fn payload(&self) -> &AdvicePayload {
        &self.core.payload
    }

    pub const fn generated_at(&self) -> DateTime<Utc> {
        self.generated_at
    }

    pub fn into_core(self) -> AdviceCore {
        self.core
    }
}
