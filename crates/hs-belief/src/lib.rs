//! Hidden-information state kept separate from the authoritative game mirror.
//!
//! The first M4 slice intentionally models public evidence only. It never reads
//! logs, mutates `ObservedGameState`, or invents archetype probabilities when no
//! corpus is available.

use std::error::Error;
use std::fmt;

use hs_types::CardId;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One publicly established generated-card relationship.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct KnownGenerationSource {
    pub generated_card: CardId,
    pub source_card: CardId,
}

/// Public facts from which the minimal belief state can be constructed.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublicBeliefInput {
    /// Opponent cards whose identities have become public during this game.
    #[serde(default)]
    pub opponent_seen_cards: Vec<CardId>,
    /// Opponent hand entities that remain unidentified.
    #[serde(default)]
    pub opponent_unknown_hand_count: u32,
    /// Generated-card relationships proven by public events.
    #[serde(default)]
    pub known_generation_sources: Vec<KnownGenerationSource>,
}

/// A future archetype candidate. The first M4 slice always leaves this list
/// empty because no versioned archetype corpus is available yet.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArchetypeProbability {
    pub archetype: String,
    pub probability: BeliefProbability,
}

/// A finite archetype probability in the inclusive range `[0, 1]`.
///
/// This lives in `hs-belief` rather than depending backwards on the advisor
/// API's confidence type. Probability and evidence confidence are distinct
/// concepts even though both share the same numeric bounds.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct BeliefProbability(f32);

impl BeliefProbability {
    pub fn new(value: f32) -> Result<Self, InvalidBeliefProbability> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(InvalidBeliefProbability { value });
        }

        Ok(if value == 0.0 { Self(0.0) } else { Self(value) })
    }

    pub const fn value(self) -> f32 {
        self.0
    }
}

impl Eq for BeliefProbability {}

impl Serialize for BeliefProbability {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f32(self.0)
    }
}

impl<'de> Deserialize<'de> for BeliefProbability {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(f32::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InvalidBeliefProbability {
    value: f32,
}

impl InvalidBeliefProbability {
    pub const fn value(self) -> f32 {
        self.value
    }
}

impl fmt::Display for InvalidBeliefProbability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "belief probability must be finite and between 0 and 1, got {}",
            self.value
        )
    }
}

impl Error for InvalidBeliefProbability {}

/// Hidden-information state derived from public facts.
///
/// Fields are private so callers cannot bypass normalization or insert invented
/// probabilities. Future corpus-backed constructors can extend this crate
/// without coupling it to the observed-state reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeliefState {
    opponent_seen_cards: Vec<CardId>,
    opponent_unknown_hand_count: u32,
    known_generation_sources: Vec<KnownGenerationSource>,
}

impl BeliefState {
    /// Build the conservative M4 belief: sort and deduplicate public evidence,
    /// and do not infer any probability distribution.
    pub fn from_public(mut input: PublicBeliefInput) -> Self {
        input.opponent_seen_cards.sort();
        input.opponent_seen_cards.dedup();
        input.known_generation_sources.sort();
        input.known_generation_sources.dedup();

        Self {
            opponent_seen_cards: input.opponent_seen_cards,
            opponent_unknown_hand_count: input.opponent_unknown_hand_count,
            known_generation_sources: input.known_generation_sources,
        }
    }

    pub fn opponent_seen_cards(&self) -> &[CardId] {
        &self.opponent_seen_cards
    }

    pub fn opponent_unknown_hand_count(&self) -> u32 {
        self.opponent_unknown_hand_count
    }

    pub fn known_generation_sources(&self) -> &[KnownGenerationSource] {
        &self.known_generation_sources
    }

    /// Produce the UI/API-safe summary. Until a real corpus is introduced, the
    /// answer is always explicitly insufficient rather than a fabricated guess.
    pub fn summary(&self) -> BeliefSummary {
        BeliefSummary {
            opponent_seen_cards: self.opponent_seen_cards.clone(),
            opponent_unknown_hand_count: self.opponent_unknown_hand_count,
            known_generation_sources: self.known_generation_sources.clone(),
            archetype_probabilities: Vec::new(),
            insufficient_data: true,
        }
    }
}

/// Serializable public summary consumed by Advice DTOs and presentation code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BeliefSummary {
    pub opponent_seen_cards: Vec<CardId>,
    pub opponent_unknown_hand_count: u32,
    pub known_generation_sources: Vec<KnownGenerationSource>,
    pub archetype_probabilities: Vec<ArchetypeProbability>,
    pub insufficient_data: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str) -> CardId {
        CardId::new(id)
    }

    #[test]
    fn public_evidence_is_normalized_without_mutating_the_input() {
        let input = PublicBeliefInput {
            opponent_seen_cards: vec![card("B"), card("A"), card("A")],
            opponent_unknown_hand_count: 4,
            known_generation_sources: vec![
                KnownGenerationSource {
                    generated_card: card("TOKEN_B"),
                    source_card: card("SOURCE"),
                },
                KnownGenerationSource {
                    generated_card: card("TOKEN_A"),
                    source_card: card("SOURCE"),
                },
                KnownGenerationSource {
                    generated_card: card("TOKEN_A"),
                    source_card: card("SOURCE"),
                },
            ],
        };
        let untouched = input.clone();

        let belief = BeliefState::from_public(input);

        assert_eq!(belief.opponent_seen_cards(), &[card("A"), card("B")]);
        assert_eq!(belief.known_generation_sources().len(), 2);
        assert_eq!(untouched.opponent_seen_cards[0], card("B"));
    }

    #[test]
    fn summary_reports_insufficient_data_instead_of_fake_probabilities() {
        let belief = BeliefState::from_public(PublicBeliefInput {
            opponent_seen_cards: vec![card("KNOWN")],
            opponent_unknown_hand_count: 3,
            known_generation_sources: Vec::new(),
        });

        let summary = belief.summary();

        assert!(summary.insufficient_data);
        assert!(summary.archetype_probabilities.is_empty());
        assert_eq!(summary.opponent_unknown_hand_count, 3);
        assert_eq!(summary.opponent_seen_cards, vec![card("KNOWN")]);
    }

    #[test]
    fn summary_serde_round_trip_is_exact() {
        let summary = BeliefState::from_public(PublicBeliefInput::default()).summary();
        let json = serde_json::to_string(&summary).expect("serialize belief summary");
        let decoded: BeliefSummary =
            serde_json::from_str(&json).expect("deserialize belief summary");
        assert_eq!(decoded, summary);
    }

    #[test]
    fn archetype_probability_rejects_non_finite_and_out_of_range_values() {
        for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert!(BeliefProbability::new(invalid).is_err());
        }
        assert!(serde_json::from_str::<BeliefProbability>("1.1").is_err());

        let probability = BeliefProbability::new(0.25).expect("valid probability");
        let json = serde_json::to_string(&probability).expect("serialize probability");
        let decoded =
            serde_json::from_str::<BeliefProbability>(&json).expect("deserialize probability");
        assert_eq!(decoded, probability);
        assert_eq!(decoded.value(), 0.25);
    }
}
