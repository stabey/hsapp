//! Strict, versioned scoring preferences selected by Build and exact deck signature.
//!
//! Playbooks are deliberately data-only. They cannot see legal actions or the
//! simulator, and this module performs no directory IO: callers pass named YAML
//! sources explicitly so startup and tests consume the same deterministic input.

use std::collections::{BTreeMap, BTreeSet};

use hs_advisor_api::ScoreFeature;
use hs_types::{BuildId, BuildRange, CardId};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub const PLAYBOOK_SCHEMA_VERSION: u32 = 1;
pub const MAX_ABS_WEIGHT_MILLI: i32 = 1_000_000;
pub const MAX_RISK_WEIGHT_MILLI: i32 = 1_000;

/// One normalized main-deck card/count entry.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckCardCount {
    pub card_id: CardId,
    pub count: u16,
}

/// One normalized sideboard entry. The owner is part of signature identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SideboardCardCount {
    pub owner_card_id: CardId,
    pub card_id: CardId,
    pub count: u16,
}

/// Canonical exact-deck identity used by Playbook selection.
///
/// Entries are sorted and duplicate keys are merged with checked addition.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DeckSignature {
    main: Vec<DeckCardCount>,
    sideboard: Vec<SideboardCardCount>,
}

impl DeckSignature {
    pub fn new(
        main: impl IntoIterator<Item = DeckCardCount>,
        sideboard: impl IntoIterator<Item = SideboardCardCount>,
    ) -> Result<Self, DeckSignatureError> {
        let mut normalized_main: BTreeMap<CardId, u16> = BTreeMap::new();
        for entry in main {
            validate_card_count(&entry.card_id, entry.count)?;
            checked_add_count(
                normalized_main.entry(entry.card_id.clone()).or_default(),
                entry.count,
                &entry.card_id,
            )?;
        }

        let mut normalized_sideboard: BTreeMap<(CardId, CardId), u16> = BTreeMap::new();
        for entry in sideboard {
            validate_card_id(&entry.owner_card_id)?;
            validate_card_count(&entry.card_id, entry.count)?;
            checked_add_count(
                normalized_sideboard
                    .entry((entry.owner_card_id.clone(), entry.card_id.clone()))
                    .or_default(),
                entry.count,
                &entry.card_id,
            )?;
        }

        Ok(Self {
            main: normalized_main
                .into_iter()
                .map(|(card_id, count)| DeckCardCount { card_id, count })
                .collect(),
            sideboard: normalized_sideboard
                .into_iter()
                .map(|((owner_card_id, card_id), count)| SideboardCardCount {
                    owner_card_id,
                    card_id,
                    count,
                })
                .collect(),
        })
    }

    pub fn main(&self) -> &[DeckCardCount] {
        &self.main
    }

    pub fn sideboard(&self) -> &[SideboardCardCount] {
        &self.sideboard
    }
}

impl<'de> Deserialize<'de> for DeckSignature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireSignature {
            main: Vec<DeckCardCount>,
            #[serde(default)]
            sideboard: Vec<SideboardCardCount>,
        }

        let wire = WireSignature::deserialize(deserializer)?;
        Self::new(wire.main, wire.sideboard).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum DeckSignatureError {
    #[error("card ID is not a stable Hearthstone ID: {card_id}")]
    InvalidCardId { card_id: CardId },
    #[error("card count must be greater than zero: {card_id}")]
    ZeroCount { card_id: CardId },
    #[error("merged card count exceeds u16: {card_id}")]
    CountOverflow { card_id: CardId },
}

/// A declarative win-point check. It is explanatory only and cannot affect legality.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WinPoint {
    pub id: String,
    pub feature: ScoreFeature,
    pub comparison: WinPointComparison,
    pub threshold: i32,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WinPointComparison {
    AtLeast,
    AtMost,
}

/// Stable positive/negative explanation templates for one public score feature.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationTemplate {
    pub positive: String,
    pub negative: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybookApplicability {
    pub build_range: BuildRange,
    #[serde(default)]
    pub deck_signatures: Vec<DeckSignature>,
}

/// One validated Playbook document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Playbook {
    pub schema_version: u32,
    pub id: String,
    #[serde(default)]
    pub priority: u32,
    pub applies_to: PlaybookApplicability,
    pub weights: BTreeMap<ScoreFeature, i32>,
    pub risk_weight_milli: i32,
    #[serde(default)]
    pub win_points: Vec<WinPoint>,
    #[serde(default)]
    pub explanations: BTreeMap<ScoreFeature, ExplanationTemplate>,
}

impl Playbook {
    pub fn from_yaml(yaml: &str) -> Result<Self, PlaybookError> {
        let mut playbook: Self =
            serde_yaml::from_str(yaml).map_err(|source| PlaybookError::Yaml {
                source_name: "<memory>".to_owned(),
                source,
            })?;
        validate_and_normalize(&mut playbook)?;
        Ok(playbook)
    }
}

/// Validated immutable catalog. Construction is independent of input order.
#[derive(Clone, Debug)]
pub struct PlaybookCatalog {
    playbooks: Vec<Playbook>,
    generic_index: usize,
}

impl PlaybookCatalog {
    pub fn new(playbooks: impl IntoIterator<Item = Playbook>) -> Result<Self, PlaybookError> {
        let mut playbooks: Vec<_> = playbooks.into_iter().collect();
        playbooks.sort_by(|left, right| left.id.cmp(&right.id));
        for playbook in &mut playbooks {
            validate_and_normalize(playbook)?;
        }
        validate_catalog(&playbooks)?;
        let generic_index = playbooks
            .iter()
            .position(|playbook| playbook.id == "generic")
            .ok_or(PlaybookError::MissingGeneric)?;
        Ok(Self {
            playbooks,
            generic_index,
        })
    }

    /// Parse a deterministic catalog from caller-provided `(source name, YAML)` pairs.
    pub fn from_sources<I, N, Y>(sources: I) -> Result<Self, PlaybookError>
    where
        I: IntoIterator<Item = (N, Y)>,
        N: Into<String>,
        Y: AsRef<str>,
    {
        let mut sources: Vec<(String, String)> = sources
            .into_iter()
            .map(|(name, yaml)| (name.into(), yaml.as_ref().to_owned()))
            .collect();
        sources.sort_by(|left, right| left.0.cmp(&right.0));

        let mut seen_sources = BTreeSet::new();
        let mut playbooks = Vec::with_capacity(sources.len());
        for (source_name, yaml) in sources {
            if !seen_sources.insert(source_name.clone()) {
                return Err(PlaybookError::DuplicateSource { source_name });
            }
            let playbook = serde_yaml::from_str(&yaml).map_err(|source| PlaybookError::Yaml {
                source_name,
                source,
            })?;
            playbooks.push(playbook);
        }
        Self::new(playbooks)
    }

    pub fn select(
        &self,
        build: BuildId,
        signature: Option<&DeckSignature>,
    ) -> Result<SelectedPlaybook<'_>, PlaybookError> {
        let mut exact_matches: Vec<_> = signature
            .into_iter()
            .flat_map(|signature| {
                self.playbooks.iter().filter(move |playbook| {
                    playbook.id != "generic"
                        && playbook.applies_to.build_range.contains(build)
                        && playbook
                            .applies_to
                            .deck_signatures
                            .binary_search(signature)
                            .is_ok()
                })
            })
            .collect();

        if !exact_matches.is_empty() {
            exact_matches.sort_by(|left, right| {
                right
                    .priority
                    .cmp(&left.priority)
                    .then_with(|| left.id.cmp(&right.id))
            });
            if exact_matches
                .get(1)
                .is_some_and(|other| other.priority == exact_matches[0].priority)
            {
                return Err(PlaybookError::AmbiguousExactMatch {
                    left: exact_matches[0].id.clone(),
                    right: exact_matches[1].id.clone(),
                });
            }
            return Ok(SelectedPlaybook {
                playbook: exact_matches[0],
                reason: PlaybookSelectionReason::ExactDeckMatch { build },
            });
        }

        let generic = &self.playbooks[self.generic_index];
        if !generic.applies_to.build_range.contains(build) {
            return Err(PlaybookError::GenericDoesNotCoverBuild { build });
        }
        let reason = if signature.is_some() {
            GenericFallbackReason::NoExactDeckMatch
        } else {
            GenericFallbackReason::DeckSignatureUnavailable
        };
        Ok(SelectedPlaybook {
            playbook: generic,
            reason: PlaybookSelectionReason::GenericFallback { build, reason },
        })
    }

    pub fn playbooks(&self) -> &[Playbook] {
        &self.playbooks
    }
}

#[derive(Clone, Debug)]
pub struct SelectedPlaybook<'a> {
    playbook: &'a Playbook,
    reason: PlaybookSelectionReason,
}

impl SelectedPlaybook<'_> {
    pub fn id(&self) -> &str {
        &self.playbook.id
    }

    pub fn weight(&self, feature: ScoreFeature) -> i32 {
        self.playbook.weights[&feature]
    }

    pub fn risk_weight_milli(&self) -> i32 {
        self.playbook.risk_weight_milli
    }

    pub fn win_points(&self) -> &[WinPoint] {
        &self.playbook.win_points
    }

    pub fn explanations(&self) -> &BTreeMap<ScoreFeature, ExplanationTemplate> {
        &self.playbook.explanations
    }

    pub fn selection_reason(&self) -> &PlaybookSelectionReason {
        &self.reason
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenericFallbackReason {
    DeckSignatureUnavailable,
    NoExactDeckMatch,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum PlaybookSelectionReason {
    ExactDeckMatch {
        build: BuildId,
    },
    GenericFallback {
        build: BuildId,
        reason: GenericFallbackReason,
    },
}

#[derive(Debug, Error)]
pub enum PlaybookError {
    #[error("failed to parse Playbook source {source_name}: {source}")]
    Yaml {
        source_name: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("duplicate Playbook source name: {source_name}")]
    DuplicateSource { source_name: String },
    #[error("unsupported Playbook schema {found}; expected {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("invalid stable Playbook ID: {id}")]
    InvalidId { id: String },
    #[error("Playbook {id} has invalid Build range {min}..={max}")]
    InvalidBuildRange { id: String, min: u32, max: u32 },
    #[error("Playbook {id} is missing required score weight {feature:?}")]
    MissingWeight { id: String, feature: ScoreFeature },
    #[error("Playbook {id} has out-of-range weight {value} for {feature:?}")]
    InvalidWeight {
        id: String,
        feature: ScoreFeature,
        value: i32,
    },
    #[error(
        "Playbook {id} has invalid risk_weight_milli {value}; expected 0..={MAX_RISK_WEIGHT_MILLI}"
    )]
    InvalidRiskWeight { id: String, value: i32 },
    #[error("Playbook {id} contains duplicate win-point ID {win_point_id}")]
    DuplicateWinPointId { id: String, win_point_id: String },
    #[error("Playbook {id} has invalid win-point ID {win_point_id}")]
    InvalidWinPointId { id: String, win_point_id: String },
    #[error("Playbook {id} has an empty message/template at {field}")]
    EmptyText { id: String, field: String },
    #[error("duplicate Playbook ID: {id}")]
    DuplicateId { id: String },
    #[error("Playbook catalog is missing the generic fallback")]
    MissingGeneric,
    #[error("generic Playbook must cover every Build and have no deck signatures")]
    InvalidGeneric,
    #[error("non-generic Playbook {id} must contain at least one exact deck signature")]
    MissingExactSignature { id: String },
    #[error("equal-priority exact Playbooks are ambiguous: {left} and {right}")]
    AmbiguousExactMatch { left: String, right: String },
    #[error("generic Playbook does not cover requested Build {build:?}")]
    GenericDoesNotCoverBuild { build: BuildId },
}

fn validate_and_normalize(playbook: &mut Playbook) -> Result<(), PlaybookError> {
    if playbook.schema_version != PLAYBOOK_SCHEMA_VERSION {
        return Err(PlaybookError::UnsupportedSchema {
            found: playbook.schema_version,
            supported: PLAYBOOK_SCHEMA_VERSION,
        });
    }
    if !valid_stable_id(&playbook.id) {
        return Err(PlaybookError::InvalidId {
            id: playbook.id.clone(),
        });
    }
    if let Some(max) = playbook.applies_to.build_range.max {
        if playbook.applies_to.build_range.min > max {
            return Err(PlaybookError::InvalidBuildRange {
                id: playbook.id.clone(),
                min: playbook.applies_to.build_range.min.0,
                max: max.0,
            });
        }
    }

    for feature in ScoreFeature::ALL {
        let Some(weight) = playbook.weights.get(&feature).copied() else {
            return Err(PlaybookError::MissingWeight {
                id: playbook.id.clone(),
                feature,
            });
        };
        if !(-MAX_ABS_WEIGHT_MILLI..=MAX_ABS_WEIGHT_MILLI).contains(&weight) {
            return Err(PlaybookError::InvalidWeight {
                id: playbook.id.clone(),
                feature,
                value: weight,
            });
        }
    }
    if !(0..=MAX_RISK_WEIGHT_MILLI).contains(&playbook.risk_weight_milli) {
        return Err(PlaybookError::InvalidRiskWeight {
            id: playbook.id.clone(),
            value: playbook.risk_weight_milli,
        });
    }

    let mut win_point_ids = BTreeSet::new();
    for win_point in &playbook.win_points {
        if !valid_stable_id(&win_point.id) {
            return Err(PlaybookError::InvalidWinPointId {
                id: playbook.id.clone(),
                win_point_id: win_point.id.clone(),
            });
        }
        if !win_point_ids.insert(win_point.id.clone()) {
            return Err(PlaybookError::DuplicateWinPointId {
                id: playbook.id.clone(),
                win_point_id: win_point.id.clone(),
            });
        }
        if win_point.message.trim().is_empty() {
            return Err(PlaybookError::EmptyText {
                id: playbook.id.clone(),
                field: format!("win_points.{}.message", win_point.id),
            });
        }
    }
    for (feature, template) in &playbook.explanations {
        if template.positive.trim().is_empty() {
            return Err(PlaybookError::EmptyText {
                id: playbook.id.clone(),
                field: format!("explanations.{feature:?}.positive"),
            });
        }
        if template.negative.trim().is_empty() {
            return Err(PlaybookError::EmptyText {
                id: playbook.id.clone(),
                field: format!("explanations.{feature:?}.negative"),
            });
        }
    }

    playbook.applies_to.deck_signatures.sort();
    playbook.applies_to.deck_signatures.dedup();
    playbook
        .win_points
        .sort_by(|left, right| left.id.cmp(&right.id));
    Ok(())
}

fn validate_catalog(playbooks: &[Playbook]) -> Result<(), PlaybookError> {
    for pair in playbooks.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(PlaybookError::DuplicateId {
                id: pair[0].id.clone(),
            });
        }
    }

    let Some(generic) = playbooks.iter().find(|playbook| playbook.id == "generic") else {
        return Err(PlaybookError::MissingGeneric);
    };
    if generic.applies_to.build_range.min != BuildId(0)
        || generic.applies_to.build_range.max.is_some()
        || !generic.applies_to.deck_signatures.is_empty()
    {
        return Err(PlaybookError::InvalidGeneric);
    }

    for playbook in playbooks.iter().filter(|playbook| playbook.id != "generic") {
        if playbook.applies_to.deck_signatures.is_empty() {
            return Err(PlaybookError::MissingExactSignature {
                id: playbook.id.clone(),
            });
        }
    }

    for (index, left) in playbooks.iter().enumerate() {
        if left.id == "generic" {
            continue;
        }
        for right in playbooks.iter().skip(index + 1) {
            if right.id == "generic"
                || left.priority != right.priority
                || !ranges_overlap(&left.applies_to.build_range, &right.applies_to.build_range)
            {
                continue;
            }
            if signatures_intersect(
                &left.applies_to.deck_signatures,
                &right.applies_to.deck_signatures,
            ) {
                return Err(PlaybookError::AmbiguousExactMatch {
                    left: left.id.clone(),
                    right: right.id.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_card_count(card_id: &CardId, count: u16) -> Result<(), DeckSignatureError> {
    validate_card_id(card_id)?;
    if count == 0 {
        return Err(DeckSignatureError::ZeroCount {
            card_id: card_id.clone(),
        });
    }
    Ok(())
}

fn validate_card_id(card_id: &CardId) -> Result<(), DeckSignatureError> {
    let value = card_id.as_str();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(DeckSignatureError::InvalidCardId {
            card_id: card_id.clone(),
        });
    }
    Ok(())
}

fn checked_add_count(
    total: &mut u16,
    count: u16,
    card_id: &CardId,
) -> Result<(), DeckSignatureError> {
    *total = total
        .checked_add(count)
        .ok_or_else(|| DeckSignatureError::CountOverflow {
            card_id: card_id.clone(),
        })?;
    Ok(())
}

fn valid_stable_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let mut previous_separator = false;
    for (index, byte) in id.bytes().enumerate() {
        let alphanumeric = byte.is_ascii_lowercase() || byte.is_ascii_digit();
        let separator = matches!(byte, b'.' | b'_' | b'-');
        if !alphanumeric && !separator {
            return false;
        }
        if separator && (index == 0 || previous_separator) {
            return false;
        }
        previous_separator = separator;
    }
    !previous_separator
}

fn ranges_overlap(left: &BuildRange, right: &BuildRange) -> bool {
    left.contains(right.min) || right.contains(left.min)
}

fn signatures_intersect(left: &[DeckSignature], right: &[DeckSignature]) -> bool {
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}
