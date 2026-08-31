use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

/// Stable feature IDs available to the first fixed-point advisor scorer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreFeature {
    FriendlyEffectiveHealth,
    OpponentEffectiveHealth,
    FriendlyBoardAttack,
    OpponentBoardAttack,
    FriendlyBoardHealth,
    OpponentBoardHealth,
    FriendlyBoardValue,
    OpponentBoardValue,
    FriendlyHandSize,
    OpponentHandSize,
    ResourceAdvantage,
    ManaEfficiency,
    PlaybookPreference,
    RiskPenalty,
}

impl ScoreFeature {
    pub const ALL: [Self; 14] = [
        Self::FriendlyEffectiveHealth,
        Self::OpponentEffectiveHealth,
        Self::FriendlyBoardAttack,
        Self::OpponentBoardAttack,
        Self::FriendlyBoardHealth,
        Self::OpponentBoardHealth,
        Self::FriendlyBoardValue,
        Self::OpponentBoardValue,
        Self::FriendlyHandSize,
        Self::OpponentHandSize,
        Self::ResourceAdvantage,
        Self::ManaEfficiency,
        Self::PlaybookPreference,
        Self::RiskPenalty,
    ];

    pub const fn stable_index(self) -> u8 {
        match self {
            Self::FriendlyEffectiveHealth => 0,
            Self::OpponentEffectiveHealth => 1,
            Self::FriendlyBoardAttack => 2,
            Self::OpponentBoardAttack => 3,
            Self::FriendlyBoardHealth => 4,
            Self::OpponentBoardHealth => 5,
            Self::FriendlyBoardValue => 6,
            Self::OpponentBoardValue => 7,
            Self::FriendlyHandSize => 8,
            Self::OpponentHandSize => 9,
            Self::ResourceAdvantage => 10,
            Self::ManaEfficiency => 11,
            Self::PlaybookPreference => 12,
            Self::RiskPenalty => 13,
        }
    }
}

/// One integer feature and its fixed-point (`milli`) weight.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScoreComponent {
    pub feature: ScoreFeature,
    pub raw: i32,
    pub weight_milli: i32,
}

impl ScoreComponent {
    /// Exact weighted contribution. The product of two i32 values fits in i64.
    pub fn weighted_milli(self) -> i64 {
        i64::from(self.raw) * i64::from(self.weight_milli)
    }
}

/// Stable score components. The total is derived and is never serialized.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ScoreBreakdown {
    components: Vec<ScoreComponent>,
}

impl ScoreBreakdown {
    pub fn new(
        components: impl IntoIterator<Item = ScoreComponent>,
    ) -> Result<Self, InvalidScoreBreakdown> {
        let mut components: Vec<_> = components.into_iter().collect();
        components.sort_by_key(|component| component.feature.stable_index());

        let mut seen = BTreeSet::new();
        for component in &components {
            let index = component.feature.stable_index();
            if !seen.insert(index) {
                return Err(InvalidScoreBreakdown {
                    duplicate: component.feature,
                });
            }
        }

        Ok(Self { components })
    }

    pub fn components(&self) -> &[ScoreComponent] {
        &self.components
    }

    /// Exact sum in a wider integer, avoiding both floating-point tie drift and
    /// a duplicated stored total that could disagree with its components.
    pub fn total_milli(&self) -> i128 {
        self.components
            .iter()
            .map(|component| i128::from(component.weighted_milli()))
            .sum()
    }

    pub fn total(&self) -> i128 {
        self.total_milli()
    }
}

impl<'de> Deserialize<'de> for ScoreBreakdown {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireBreakdown {
            #[serde(default)]
            components: Vec<ScoreComponent>,
        }

        Self::new(WireBreakdown::deserialize(deserializer)?.components).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidScoreBreakdown {
    duplicate: ScoreFeature,
}

impl InvalidScoreBreakdown {
    pub const fn duplicate(self) -> ScoreFeature {
        self.duplicate
    }
}

impl fmt::Display for InvalidScoreBreakdown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "duplicate score feature: {:?}", self.duplicate)
    }
}

impl Error for InvalidScoreBreakdown {}
