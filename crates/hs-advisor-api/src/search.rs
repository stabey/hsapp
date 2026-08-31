use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::Confidence;

/// Deterministic, platform-independent search budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchConfig {
    pub seed: u64,
    /// Maximum simulator branches returned by one chance-producing action
    /// for one parent state. Global work remains bounded independently by
    /// `node_budget` and `candidate_budget`.
    pub sim_branch_budget: u32,
    pub max_depth: u16,
    pub beam_width: u32,
    pub node_budget: u64,
    pub candidate_budget: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            seed: 0,
            sim_branch_budget: 64,
            max_depth: 6,
            beam_width: 12,
            node_budget: 2_000,
            candidate_budget: 8_000,
        }
    }
}

/// Root-action comparison completeness. The ratio is always derived.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SearchCompleteness {
    total: u64,
    reliably_compared: u64,
    #[serde(skip_serializing_if = "confidence_is_one")]
    evidence_cap: Confidence,
}

impl SearchCompleteness {
    pub fn new(total: u64, reliably_compared: u64) -> Result<Self, InvalidSearchCompleteness> {
        Self::with_evidence_cap(total, reliably_compared, Confidence::ONE)
    }

    /// Preserve exact root coverage while independently capping evidence that
    /// was truncated inside a root (Beam/depth/node/probability boundaries).
    pub fn with_evidence_cap(
        total: u64,
        reliably_compared: u64,
        evidence_cap: Confidence,
    ) -> Result<Self, InvalidSearchCompleteness> {
        if reliably_compared > total {
            return Err(InvalidSearchCompleteness {
                total,
                reliably_compared,
            });
        }
        Ok(Self {
            total,
            reliably_compared,
            evidence_cap,
        })
    }

    pub const fn total(self) -> u64 {
        self.total
    }

    pub const fn reliably_compared(self) -> u64 {
        self.reliably_compared
    }

    pub const fn evidence_cap(self) -> Confidence {
        self.evidence_cap
    }

    /// An empty action set has no comparable recommendation and yields zero.
    pub fn ratio(self) -> Confidence {
        let root_ratio = Confidence::ratio(self.reliably_compared, self.total);
        if root_ratio < self.evidence_cap {
            root_ratio
        } else {
            self.evidence_cap
        }
    }
}

impl Default for SearchCompleteness {
    fn default() -> Self {
        Self {
            total: 0,
            reliably_compared: 0,
            evidence_cap: Confidence::ONE,
        }
    }
}

impl<'de> Deserialize<'de> for SearchCompleteness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireCompleteness {
            total: u64,
            reliably_compared: u64,
            #[serde(default = "default_evidence_cap")]
            evidence_cap: Confidence,
        }

        let wire = WireCompleteness::deserialize(deserializer)?;
        Self::with_evidence_cap(wire.total, wire.reliably_compared, wire.evidence_cap)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSearchCompleteness {
    total: u64,
    reliably_compared: u64,
}

impl InvalidSearchCompleteness {
    pub const fn total(self) -> u64 {
        self.total
    }

    pub const fn reliably_compared(self) -> u64 {
        self.reliably_compared
    }
}

impl fmt::Display for InvalidSearchCompleteness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "reliably compared actions ({}) exceed total actions ({})",
            self.reliably_compared, self.total
        )
    }
}

impl Error for InvalidSearchCompleteness {}

fn default_evidence_cap() -> Confidence {
    Confidence::ONE
}

fn confidence_is_one(value: &Confidence) -> bool {
    *value == Confidence::ONE
}
