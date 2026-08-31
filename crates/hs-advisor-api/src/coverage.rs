use std::collections::BTreeMap;

use hs_types::{CardId, RuleStatus};
use serde::{Deserialize, Deserializer, Serialize};

use crate::Confidence;

/// Coverage evidence for one card rule relevant to this advice calculation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleCoverageItem {
    pub card_id: CardId,
    /// `None` means that this Build has no matching rule definition.
    pub status: Option<RuleStatus>,
    pub confidence: Confidence,
    pub runtime_unknown: bool,
}

/// Normalized coverage for only the rules relevant to this advice calculation.
///
/// `items` is the sole stored fact source. Counts and ratios are derived, and
/// deserialization performs the same sort/dedup merge as construction.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RuleCoverageReport {
    items: Vec<RuleCoverageItem>,
}

impl RuleCoverageReport {
    pub fn new(items: impl IntoIterator<Item = RuleCoverageItem>) -> Self {
        let mut normalized = BTreeMap::<CardId, RuleCoverageItem>::new();

        for item in items {
            normalized
                .entry(item.card_id.clone())
                .and_modify(|current| merge_item(current, &item))
                .or_insert(item);
        }

        Self {
            items: normalized.into_values().collect(),
        }
    }

    pub fn items(&self) -> &[RuleCoverageItem] {
        &self.items
    }

    pub fn total_count(&self) -> u64 {
        u64::try_from(self.items.len()).unwrap_or(u64::MAX)
    }

    pub fn covered_count(&self) -> u64 {
        count_matching(&self.items, |item| {
            matches!(
                item.status,
                Some(
                    RuleStatus::Verified
                        | RuleStatus::Inferred
                        | RuleStatus::Partial
                        | RuleStatus::Stale
                )
            )
        })
    }

    pub fn verified_count(&self) -> u64 {
        self.status_count(RuleStatus::Verified)
    }

    pub fn missing_count(&self) -> u64 {
        count_matching(&self.items, |item| item.status.is_none())
    }

    pub fn runtime_unknown_count(&self) -> u64 {
        count_matching(&self.items, |item| item.runtime_unknown)
    }

    pub fn status_count(&self, status: RuleStatus) -> u64 {
        count_matching(&self.items, |item| item.status == Some(status))
    }

    /// Ratio of present executable rule definitions to all relevant cards.
    /// Empty coverage has no supporting evidence and therefore returns zero.
    pub fn ratio(&self) -> Confidence {
        Confidence::ratio(self.covered_count(), self.total_count())
    }

    pub fn coverage_ratio(&self) -> Confidence {
        self.ratio()
    }
}

impl<'de> Deserialize<'de> for RuleCoverageReport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireReport {
            #[serde(default)]
            items: Vec<RuleCoverageItem>,
        }

        Ok(Self::new(WireReport::deserialize(deserializer)?.items))
    }
}

fn count_matching(
    items: &[RuleCoverageItem],
    predicate: impl Fn(&RuleCoverageItem) -> bool,
) -> u64 {
    items
        .iter()
        .filter(|item| predicate(item))
        .count()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn merge_item(current: &mut RuleCoverageItem, incoming: &RuleCoverageItem) {
    current.status = weakest_status(current.status, incoming.status);
    current.confidence = current.confidence.min(incoming.confidence);
    current.runtime_unknown |= incoming.runtime_unknown;
}

fn weakest_status(left: Option<RuleStatus>, right: Option<RuleStatus>) -> Option<RuleStatus> {
    if status_strength(left) <= status_strength(right) {
        left
    } else {
        right
    }
}

/// Explicit strength mapping. Never use `RuleStatus` declaration order for a
/// semantic coverage decision.
fn status_strength(status: Option<RuleStatus>) -> u8 {
    match status {
        None => 0,
        Some(RuleStatus::Unknown) => 1,
        Some(RuleStatus::Stale) => 2,
        Some(RuleStatus::Partial) => 3,
        Some(RuleStatus::Inferred) => 4,
        Some(RuleStatus::Verified) => 5,
    }
}
