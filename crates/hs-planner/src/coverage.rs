//! Relevant-rule coverage aggregation for evaluated planner paths.

use hs_advisor_api::{ActionKind, Confidence, LegalAction, RuleCoverageItem, RuleCoverageReport};
use hs_observed_state::ObservedGameState;
use hs_sim::{RuleLookup, RuleUse, SimOutcome};
use hs_types::{BuildId, CardId};
use thiserror::Error;

use crate::search::PathEvidence;
use crate::PlannerRequest;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum CoverageError {
    #[error("coverage root action was not produced by the prepared request")]
    ForeignRoot,
}

/// Incremental evidence collector for one result or line.
///
/// Callers may add several evaluated roots and outcomes as search expands. The
/// collector never enumerates a ruleset; it queries only cards named by the
/// current root action or structured simulation evidence.
pub struct CoverageCollector<'a> {
    build_id: BuildId,
    rules: &'a dyn RuleLookup,
    observed_state: &'a ObservedGameState,
    legal_roots: &'a [LegalAction],
    items: Vec<RuleCoverageItem>,
}

impl<'a> CoverageCollector<'a> {
    /// Bind coverage to the same snapshot, exact-Build resources, and sole
    /// Options projection as the planner job.
    pub fn for_request(request: &'a PlannerRequest) -> Self {
        Self {
            build_id: request.resources().build_id(),
            rules: request.resources().rules(),
            observed_state: request.observed_state(),
            legal_roots: &request.legal_actions().actions,
            items: Vec::new(),
        }
    }

    /// Add the card named by the current server root action, when that source
    /// has an observed CardId. Attack and EndTurn intentionally contribute no
    /// root card rule because they execute shared simulator mechanics.
    pub fn record_root_action(&mut self, action: &LegalAction) -> Result<(), CoverageError> {
        if !self.legal_roots.contains(action) {
            return Err(CoverageError::ForeignRoot);
        }

        // Attacks execute the shared combat path, not the attacker's card
        // program. Any aura or triggered rules actually used by combat arrive
        // later through structured `RuleUse` evidence.
        if !matches!(
            action.kind,
            ActionKind::PlayCard | ActionKind::UseHeroPower | ActionKind::ActivateLocation
        ) {
            return Ok(());
        }

        let Some(card_id) = action
            .source
            .and_then(|entity| self.observed_state.entity(entity))
            .and_then(|entity| entity.card_id.as_ref())
        else {
            return Ok(());
        };
        self.record_lookup(card_id, false);
        Ok(())
    }

    /// Add every structured rule use and runtime-unknown card from one
    /// simulation. Human-readable trace entries are deliberately ignored.
    pub fn record_outcome(&mut self, outcome: &SimOutcome) {
        for rule_use in &outcome.rule_uses {
            self.record_rule_use(rule_use);
        }
        for card_id in &outcome.unknown_rule_cards {
            self.record_runtime_unknown(card_id);
        }
    }

    /// Add the normalized evidence accumulated by the planner's own search
    /// pipeline. This remains crate-private so external callers still cannot
    /// manufacture individual RuleUse entries or bypass complete outcomes.
    pub(crate) fn record_path_evidence(&mut self, evidence: &PathEvidence) {
        for rule_use in evidence.rule_uses() {
            self.record_rule_use(rule_use);
        }
        for card_id in evidence.unknown_rule_cards() {
            self.record_runtime_unknown(card_id);
        }
    }

    fn record_rule_use(&mut self, rule_use: &RuleUse) {
        // The current lookup is a separate piece of evidence. If it is missing
        // or bound to another Build, API normalization keeps that weaker fact.
        self.record_lookup(&rule_use.card_id, false);
        self.items.push(RuleCoverageItem {
            card_id: rule_use.card_id.clone(),
            status: Some(rule_use.status),
            confidence: bounded_confidence(rule_use.confidence),
            runtime_unknown: false,
        });
    }

    fn record_runtime_unknown(&mut self, card_id: &CardId) {
        self.record_lookup(card_id, true);
    }

    /// Finish through the API type, which is the sole owner of CardId sorting,
    /// deduplication, weakest-status merging, and derived counts.
    pub fn finish(self) -> RuleCoverageReport {
        RuleCoverageReport::new(self.items)
    }

    fn record_lookup(&mut self, card_id: &CardId, runtime_unknown: bool) {
        let matching = self
            .rules
            .program_for(card_id)
            .filter(|program| program.build_range.contains(self.build_id));
        self.items.push(match matching {
            Some(program) => RuleCoverageItem {
                card_id: card_id.clone(),
                status: Some(program.status),
                confidence: bounded_confidence(program.confidence),
                runtime_unknown,
            },
            None => RuleCoverageItem {
                card_id: card_id.clone(),
                status: None,
                confidence: Confidence::ZERO,
                runtime_unknown,
            },
        });
    }
}

fn bounded_confidence(value: f32) -> Confidence {
    Confidence::new(value).unwrap_or(Confidence::ZERO)
}
