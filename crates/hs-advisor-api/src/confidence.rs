use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use hs_types::RuleStatus;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::SearchCompleteness;

/// A finite confidence value in the inclusive range `[0, 1]`.
///
/// Construction and deserialization both enforce the range. This is an
/// evidence-quality value, not a probability of winning.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Confidence(f32);

impl Confidence {
    pub const ZERO: Self = Self(0.0);
    pub const ONE: Self = Self(1.0);

    pub fn new(value: f32) -> Result<Self, InvalidConfidence> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(InvalidConfidence { value });
        }

        // Keep the only zero representation canonical so `Eq` is sound.
        Ok(if value == 0.0 {
            Self::ZERO
        } else {
            Self(value)
        })
    }

    /// Construct a bounded ratio. An empty denominator has no evidence and
    /// therefore yields zero; a numerator above the denominator is clamped.
    pub fn ratio(numerator: u64, denominator: u64) -> Self {
        if denominator == 0 {
            return Self::ZERO;
        }

        let bounded = numerator.min(denominator);
        // u64 -> f64 -> f32 is deterministic and remains finite in [0, 1].
        Self((bounded as f64 / denominator as f64) as f32)
    }

    pub const fn value(self) -> f32 {
        self.0
    }

    pub fn min(self, other: Self) -> Self {
        if self.0 <= other.0 {
            self
        } else {
            other
        }
    }
}

impl Eq for Confidence {}

impl TryFrom<f32> for Confidence {
    type Error = InvalidConfidence;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Confidence> for f32 {
    fn from(value: Confidence) -> Self {
        value.value()
    }
}

impl Serialize for Confidence {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f32(self.0)
    }
}

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(f32::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Error returned when a confidence is NaN, infinite, or outside `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InvalidConfidence {
    value: f32,
}

impl InvalidConfidence {
    pub const fn value(self) -> f32 {
        self.value
    }
}

impl fmt::Display for InvalidConfidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "confidence must be finite and between 0 and 1, got {}",
            self.value
        )
    }
}

impl Error for InvalidConfidence {}

/// Stable explanations for a confidence cap.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceReason {
    RuleVerified,
    RuleInferred,
    RulePartial,
    RuleStale,
    RuleUnknown,
    RuleMissing,
    RuntimeUnknown,
    ServerLegalAction,
    SimulatedLegalAction,
    DeterministicSimulation,
    ExactSimulation,
    SampledSimulation,
    ProbabilityMassIncomplete,
    SearchIncomplete,
    BudgetTruncated,
}

/// One independently explainable confidence cap.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceFactor {
    pub reason: ConfidenceReason,
    pub confidence: Confidence,
}

/// A normalized set of confidence caps.
///
/// Duplicate reasons retain the smallest confidence, and the reported value is
/// always derived as the minimum factor. An empty set is fully unconstrained.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ConfidenceBreakdown {
    factors: Vec<ConfidenceFactor>,
}

impl ConfidenceBreakdown {
    pub fn new(factors: impl IntoIterator<Item = ConfidenceFactor>) -> Self {
        let mut normalized = BTreeMap::<ConfidenceReason, Confidence>::new();
        for factor in factors {
            normalized
                .entry(factor.reason)
                .and_modify(|current| *current = current.min(factor.confidence))
                .or_insert(factor.confidence);
        }

        Self {
            factors: normalized
                .into_iter()
                .map(|(reason, confidence)| ConfidenceFactor { reason, confidence })
                .collect(),
        }
    }

    pub fn from_factor(reason: ConfidenceReason, confidence: Confidence) -> Self {
        Self::new([ConfidenceFactor { reason, confidence }])
    }

    pub fn factors(&self) -> &[ConfidenceFactor] {
        &self.factors
    }

    pub fn confidence(&self) -> Confidence {
        self.factors
            .iter()
            .fold(Confidence::ONE, |current, factor| {
                current.min(factor.confidence)
            })
    }

    pub fn with_factor(&self, reason: ConfidenceReason, confidence: Confidence) -> Self {
        Self::new(
            self.factors
                .iter()
                .cloned()
                .chain([ConfidenceFactor { reason, confidence }]),
        )
    }
}

impl<'de> Deserialize<'de> for ConfidenceBreakdown {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireBreakdown {
            #[serde(default)]
            factors: Vec<ConfidenceFactor>,
        }

        Ok(Self::new(WireBreakdown::deserialize(deserializer)?.factors))
    }
}

/// Central confidence caps used by M4 advice producers.
///
/// The default rule sequence is intentionally strictly decreasing. Keeping the
/// constants here prevents individual search paths from inventing confidence
/// scales or turning score into confidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfidencePolicy {
    pub verified_rule: Confidence,
    pub inferred_rule: Confidence,
    pub partial_rule: Confidence,
    pub stale_rule: Confidence,
    pub unknown_rule: Confidence,
    pub missing_rule: Confidence,
    pub runtime_unknown: Confidence,
    pub server_legal_action: Confidence,
    pub simulated_legal_action: Confidence,
    pub deterministic_simulation: Confidence,
    pub exact_simulation: Confidence,
    pub sampled_simulation: Confidence,
}

impl Default for ConfidencePolicy {
    fn default() -> Self {
        Self {
            verified_rule: Confidence::ONE,
            inferred_rule: Confidence::new(0.85).expect("valid policy constant"),
            partial_rule: Confidence::new(0.65).expect("valid policy constant"),
            stale_rule: Confidence::new(0.45).expect("valid policy constant"),
            unknown_rule: Confidence::new(0.2).expect("valid policy constant"),
            missing_rule: Confidence::new(0.15).expect("valid policy constant"),
            runtime_unknown: Confidence::new(0.05).expect("valid policy constant"),
            server_legal_action: Confidence::ONE,
            simulated_legal_action: Confidence::new(0.85).expect("valid policy constant"),
            deterministic_simulation: Confidence::ONE,
            exact_simulation: Confidence::new(0.95).expect("valid policy constant"),
            sampled_simulation: Confidence::new(0.7).expect("valid policy constant"),
        }
    }
}

impl ConfidencePolicy {
    pub fn rule_factor(
        &self,
        status: Option<RuleStatus>,
        runtime_unknown: bool,
    ) -> ConfidenceFactor {
        if runtime_unknown {
            return ConfidenceFactor {
                reason: ConfidenceReason::RuntimeUnknown,
                confidence: self.runtime_unknown,
            };
        }

        let (reason, confidence) = match status {
            Some(RuleStatus::Verified) => (ConfidenceReason::RuleVerified, self.verified_rule),
            Some(RuleStatus::Inferred) => (ConfidenceReason::RuleInferred, self.inferred_rule),
            Some(RuleStatus::Partial) => (ConfidenceReason::RulePartial, self.partial_rule),
            Some(RuleStatus::Stale) => (ConfidenceReason::RuleStale, self.stale_rule),
            Some(RuleStatus::Unknown) => (ConfidenceReason::RuleUnknown, self.unknown_rule),
            None => (ConfidenceReason::RuleMissing, self.missing_rule),
        };

        ConfidenceFactor { reason, confidence }
    }

    /// Apply the independently measured root-action completeness cap to a top
    /// line. This is the result-level confidence contract.
    pub fn result_breakdown(
        &self,
        top_line: &ConfidenceBreakdown,
        search: &SearchCompleteness,
    ) -> ConfidenceBreakdown {
        let completeness = search.ratio();
        if completeness < Confidence::ONE {
            top_line.with_factor(ConfidenceReason::SearchIncomplete, completeness)
        } else {
            top_line.clone()
        }
    }
}
