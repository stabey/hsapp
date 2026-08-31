//! Deterministic fixed-point state scoring and offline explanations.
//!
//! The scorer consumes only facts already present in [`SimGameState`]. It does
//! not inspect confidence or simulation evidence: those remain independent
//! advice-quality signals.

use hs_advisor_api::{InvalidScoreBreakdown, ScoreBreakdown, ScoreComponent, ScoreFeature};
use hs_sim::{SimGameState, SimPlayer};
use hs_types::{CardType, PlayerId, Zone};
use thiserror::Error;

use crate::playbook::{SelectedPlaybook, WinPointComparison};

const EFFECTIVE_HEALTH_MAX: i64 = 1_000;
const BOARD_STAT_MAX: i64 = 10_000;
const BOARD_VALUE_MAX: i64 = 30_000;
const HAND_SIZE_MAX: i64 = 100;
const RESOURCE_DELTA_MAX: i64 = 100;
const MANA_EFFICIENCY_MAX: i64 = 100;
const PREFERENCE_ABS_MAX: i64 = 10_000;
const RISK_PENALTY_MAX: i64 = 10_000;
const EXPLANATION_COMPONENTS_PER_SIGN: usize = 2;

/// A stable, presentation-ready explanation derived only from score facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoreExplanation {
    /// The first configured win point whose threshold is satisfied.
    pub win_point: Option<String>,
    /// The largest positive and negative fixed-point contributions.
    pub explanation: String,
}

/// Scoring cannot continue when player identity or hero-health facts are
/// absent. Unknown facts must never be converted to a real zero score.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum ScoreError {
    #[error("friendly player identity is missing")]
    MissingFriendlyPlayer,
    #[error("friendly player {0:?} is missing from the current state")]
    MissingFriendlyPlayerState(PlayerId),
    #[error("opponent player is missing from the current state")]
    MissingOpponentPlayer,
    #[error("more than one opponent player is present in the current state")]
    AmbiguousOpponentPlayer,
    #[error("friendly hero is missing from the current state")]
    MissingFriendlyHero,
    #[error("opponent hero is missing from the current state")]
    MissingOpponentHero,
    #[error("friendly hero health is unknown")]
    MissingFriendlyHeroHealth,
    #[error("opponent hero health is unknown")]
    MissingOpponentHeroHealth,
    #[error("initial state does not contain friendly player {0:?}")]
    MissingInitialFriendlyPlayer(PlayerId),
    #[error("initial and scored states disagree about the friendly player")]
    FriendlyPlayerChanged,
    #[error("integer accumulation overflowed while extracting {0:?}")]
    ArithmeticOverflow(ScoreFeature),
    #[error(transparent)]
    InvalidBreakdown(#[from] InvalidScoreBreakdown),
}

/// Extract all fourteen stable score features and apply one selected
/// Playbook's fixed-point weights.
///
/// `risk_penalty` is a non-negative magnitude whose Playbook weight determines
/// the negative contribution. Negative values cannot become a reward and are
/// clamped to zero. Every other caller-provided or accumulated raw value is
/// clamped to the feature's documented range before weighting.
pub fn score_state(
    initial: &SimGameState,
    state: &SimGameState,
    playbook: &SelectedPlaybook<'_>,
    risk_penalty: i32,
    playbook_preference: i32,
) -> Result<ScoreBreakdown, ScoreError> {
    let participants = participants(state)?;
    let initial_friendly = initial
        .friendly_player
        .ok_or(ScoreError::MissingFriendlyPlayer)?;
    if initial_friendly != participants.friendly_id {
        return Err(ScoreError::FriendlyPlayerChanged);
    }
    let initial_friendly_player = initial.players.get(&participants.friendly_id).ok_or(
        ScoreError::MissingInitialFriendlyPlayer(participants.friendly_id),
    )?;

    let friendly_board = board_metrics(
        state,
        participants.friendly_id,
        BoardFeatureSet {
            attack: ScoreFeature::FriendlyBoardAttack,
            health: ScoreFeature::FriendlyBoardHealth,
            value: ScoreFeature::FriendlyBoardValue,
        },
    )?;
    let opponent_board = board_metrics(
        state,
        participants.opponent_id,
        BoardFeatureSet {
            attack: ScoreFeature::OpponentBoardAttack,
            health: ScoreFeature::OpponentBoardHealth,
            value: ScoreFeature::OpponentBoardValue,
        },
    )?;

    let friendly_health =
        effective_health(participants.friendly, ScoreFeature::FriendlyEffectiveHealth)?;
    let opponent_health =
        effective_health(participants.opponent, ScoreFeature::OpponentEffectiveHealth)?;
    let friendly_hand = clamp_usize(participants.friendly.hand_count, 0, HAND_SIZE_MAX);
    let opponent_hand = clamp_usize(participants.opponent.hand_count, 0, HAND_SIZE_MAX);

    let friendly_resource = available_resource(participants.friendly);
    let opponent_resource = available_resource(participants.opponent);
    let resource_advantage =
        clamp_i64(
            friendly_resource.checked_sub(opponent_resource).ok_or(
                ScoreError::ArithmeticOverflow(ScoreFeature::ResourceAdvantage),
            )?,
            -RESOURCE_DELTA_MAX,
            RESOURCE_DELTA_MAX,
        );
    let mana_efficiency = clamp_i64(
        i64::from(initial_friendly_player.mana)
            .checked_sub(i64::from(participants.friendly.mana))
            .ok_or(ScoreError::ArithmeticOverflow(ScoreFeature::ManaEfficiency))?,
        -MANA_EFFICIENCY_MAX,
        MANA_EFFICIENCY_MAX,
    );
    let preference = clamp_i64(
        i64::from(playbook_preference),
        -PREFERENCE_ABS_MAX,
        PREFERENCE_ABS_MAX,
    );
    let risk = clamp_i64(i64::from(risk_penalty).max(0), 0, RISK_PENALTY_MAX);

    let raw = [
        friendly_health,
        opponent_health,
        friendly_board.attack,
        opponent_board.attack,
        friendly_board.health,
        opponent_board.health,
        friendly_board.value,
        opponent_board.value,
        friendly_hand,
        opponent_hand,
        resource_advantage,
        mana_efficiency,
        preference,
        risk,
    ];

    let components = ScoreFeature::ALL
        .into_iter()
        .zip(raw)
        .map(|(feature, raw)| ScoreComponent {
            feature,
            raw,
            weight_milli: playbook.weight(feature),
        });
    Ok(ScoreBreakdown::new(components)?)
}

/// Render a stable explanation without changing or re-scoring the breakdown.
pub fn explain_score(
    breakdown: &ScoreBreakdown,
    playbook: &SelectedPlaybook<'_>,
) -> ScoreExplanation {
    let win_point = playbook.win_points().iter().find_map(|point| {
        breakdown
            .components()
            .iter()
            .find(|component| component.feature == point.feature)
            .filter(|component| match point.comparison {
                WinPointComparison::AtLeast => component.raw >= point.threshold,
                WinPointComparison::AtMost => component.raw <= point.threshold,
            })
            .map(|_| {
                if point.message.trim().is_empty() {
                    point.id.clone()
                } else {
                    point.message.clone()
                }
            })
    });

    let mut positive: Vec<_> = breakdown
        .components()
        .iter()
        .copied()
        .filter(|component| component.weighted_milli() > 0)
        .collect();
    let mut negative: Vec<_> = breakdown
        .components()
        .iter()
        .copied()
        .filter(|component| component.weighted_milli() < 0)
        .collect();
    sort_explanation_components(&mut positive);
    sort_explanation_components(&mut negative);

    let mut selected: Vec<_> = positive
        .into_iter()
        .take(EXPLANATION_COMPONENTS_PER_SIGN)
        .chain(negative.into_iter().take(EXPLANATION_COMPONENTS_PER_SIGN))
        .collect();
    sort_explanation_components(&mut selected);

    let explanation = if selected.is_empty() {
        "No scored feature changes this line.".to_owned()
    } else {
        selected
            .into_iter()
            .map(|component| render_component(component, playbook))
            .collect::<Vec<_>>()
            .join("; ")
    };

    ScoreExplanation {
        win_point,
        explanation,
    }
}

struct Participants<'a> {
    friendly_id: PlayerId,
    opponent_id: PlayerId,
    friendly: &'a SimPlayer,
    opponent: &'a SimPlayer,
}

fn participants(state: &SimGameState) -> Result<Participants<'_>, ScoreError> {
    let friendly_id = state
        .friendly_player
        .ok_or(ScoreError::MissingFriendlyPlayer)?;
    let friendly = state
        .players
        .get(&friendly_id)
        .ok_or(ScoreError::MissingFriendlyPlayerState(friendly_id))?;
    let mut opponents = state
        .players
        .iter()
        .filter(|(player_id, _)| **player_id != friendly_id);
    let (opponent_id, opponent) = opponents.next().ok_or(ScoreError::MissingOpponentPlayer)?;
    if opponents.next().is_some() {
        return Err(ScoreError::AmbiguousOpponentPlayer);
    }

    require_known_hero(state, friendly_id, true)?;
    require_known_hero(state, *opponent_id, false)?;

    Ok(Participants {
        friendly_id,
        opponent_id: *opponent_id,
        friendly,
        opponent,
    })
}

fn require_known_hero(
    state: &SimGameState,
    player: PlayerId,
    friendly: bool,
) -> Result<(), ScoreError> {
    let hero = state.entities.values().find(|entity| {
        entity.controller == Some(player)
            && entity.zone == Zone::Play
            && entity.card_type == Some(CardType::Hero)
    });
    let Some(hero) = hero else {
        return Err(if friendly {
            ScoreError::MissingFriendlyHero
        } else {
            ScoreError::MissingOpponentHero
        });
    };
    if !hero.action.health_known {
        return Err(if friendly {
            ScoreError::MissingFriendlyHeroHealth
        } else {
            ScoreError::MissingOpponentHeroHealth
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct BoardMetrics {
    attack: i32,
    health: i32,
    value: i32,
}

#[derive(Clone, Copy, Debug)]
struct BoardFeatureSet {
    attack: ScoreFeature,
    health: ScoreFeature,
    value: ScoreFeature,
}

fn board_metrics(
    state: &SimGameState,
    player: PlayerId,
    features: BoardFeatureSet,
) -> Result<BoardMetrics, ScoreError> {
    let mut attack = 0_i64;
    let mut health = 0_i64;
    let mut value = 0_i64;

    for entity in state.entities.values().filter(|entity| {
        entity.zone == Zone::Play
            && entity.controller == Some(player)
            && matches!(
                entity.card_type,
                Some(CardType::Minion | CardType::Weapon | CardType::Location)
            )
    }) {
        match entity.card_type {
            Some(CardType::Minion) => {
                if entity.action.attack_known {
                    add_non_negative(&mut attack, entity.attack, features.attack)?;
                    add_non_negative(&mut value, entity.attack, features.value)?;
                }
                if entity.action.health_known {
                    add_non_negative(&mut health, entity.current_health(), features.health)?;
                    add_non_negative(&mut value, entity.current_health(), features.value)?;
                }
            }
            Some(CardType::Weapon) => {
                if entity.action.attack_known {
                    add_non_negative(&mut attack, entity.attack, features.attack)?;
                    add_non_negative(&mut value, entity.attack, features.value)?;
                }
                if entity.action.durability_known {
                    add_non_negative(&mut value, entity.action.durability, features.value)?;
                }
            }
            Some(CardType::Location) => {
                if entity.action.durability_known {
                    add_non_negative(&mut value, entity.action.durability, features.value)?;
                }
            }
            _ => unreachable!("board filter admits only known scored card types"),
        }
    }

    Ok(BoardMetrics {
        attack: clamp_i64(attack, 0, BOARD_STAT_MAX),
        health: clamp_i64(health, 0, BOARD_STAT_MAX),
        value: clamp_i64(value, 0, BOARD_VALUE_MAX),
    })
}

fn add_non_negative(target: &mut i64, value: i32, feature: ScoreFeature) -> Result<(), ScoreError> {
    *target = target
        .checked_add(i64::from(value).max(0))
        .ok_or(ScoreError::ArithmeticOverflow(feature))?;
    Ok(())
}

fn effective_health(player: &SimPlayer, feature: ScoreFeature) -> Result<i32, ScoreError> {
    let value = i64::from(player.hero_health)
        .max(0)
        .checked_add(i64::from(player.armor).max(0))
        .ok_or(ScoreError::ArithmeticOverflow(feature))?;
    Ok(clamp_i64(value, 0, EFFECTIVE_HEALTH_MAX))
}

fn available_resource(player: &SimPlayer) -> i64 {
    i64::from(player.mana).max(0) - i64::from(player.overloaded).max(0)
}

fn clamp_usize(value: usize, min: i64, max: i64) -> i32 {
    let value = i64::try_from(value).unwrap_or(i64::MAX);
    clamp_i64(value, min, max)
}

fn clamp_i64(value: i64, min: i64, max: i64) -> i32 {
    debug_assert!(min >= i64::from(i32::MIN));
    debug_assert!(max <= i64::from(i32::MAX));
    i32::try_from(value.clamp(min, max)).expect("feature bounds fit i32")
}

fn sort_explanation_components(components: &mut [ScoreComponent]) {
    components.sort_by(|left, right| {
        let left_abs = i128::from(left.weighted_milli()).abs();
        let right_abs = i128::from(right.weighted_milli()).abs();
        right_abs.cmp(&left_abs).then_with(|| {
            left.feature
                .stable_index()
                .cmp(&right.feature.stable_index())
        })
    });
}

fn render_component(component: ScoreComponent, playbook: &SelectedPlaybook<'_>) -> String {
    let contribution = component.weighted_milli();
    let configured = playbook
        .explanations()
        .get(&component.feature)
        .and_then(|template| {
            let text = if contribution > 0 {
                template.positive.trim()
            } else {
                template.negative.trim()
            };
            (!text.is_empty()).then_some(text)
        });
    let text =
        configured.unwrap_or_else(|| generic_explanation(component.feature, contribution > 0));
    format!("{text} (raw {}, {contribution:+} milli)", component.raw)
}

fn generic_explanation(feature: ScoreFeature, positive: bool) -> &'static str {
    match (feature, positive) {
        (ScoreFeature::FriendlyEffectiveHealth, true) => "Friendly effective health helps",
        (ScoreFeature::FriendlyEffectiveHealth, false) => "Friendly effective health hurts",
        (ScoreFeature::OpponentEffectiveHealth, true) => "Opponent effective health helps",
        (ScoreFeature::OpponentEffectiveHealth, false) => "Opponent effective health hurts",
        (ScoreFeature::FriendlyBoardAttack, true) => "Friendly board attack helps",
        (ScoreFeature::FriendlyBoardAttack, false) => "Friendly board attack hurts",
        (ScoreFeature::OpponentBoardAttack, true) => "Opponent board attack helps",
        (ScoreFeature::OpponentBoardAttack, false) => "Opponent board attack hurts",
        (ScoreFeature::FriendlyBoardHealth, true) => "Friendly board health helps",
        (ScoreFeature::FriendlyBoardHealth, false) => "Friendly board health hurts",
        (ScoreFeature::OpponentBoardHealth, true) => "Opponent board health helps",
        (ScoreFeature::OpponentBoardHealth, false) => "Opponent board health hurts",
        (ScoreFeature::FriendlyBoardValue, true) => "Friendly board value helps",
        (ScoreFeature::FriendlyBoardValue, false) => "Friendly board value hurts",
        (ScoreFeature::OpponentBoardValue, true) => "Opponent board value helps",
        (ScoreFeature::OpponentBoardValue, false) => "Opponent board value hurts",
        (ScoreFeature::FriendlyHandSize, true) => "Friendly hand size helps",
        (ScoreFeature::FriendlyHandSize, false) => "Friendly hand size hurts",
        (ScoreFeature::OpponentHandSize, true) => "Opponent hand size helps",
        (ScoreFeature::OpponentHandSize, false) => "Opponent hand size hurts",
        (ScoreFeature::ResourceAdvantage, true) => "Resource advantage helps",
        (ScoreFeature::ResourceAdvantage, false) => "Resource advantage hurts",
        (ScoreFeature::ManaEfficiency, true) => "Mana efficiency helps",
        (ScoreFeature::ManaEfficiency, false) => "Mana efficiency hurts",
        (ScoreFeature::PlaybookPreference, true) => "Playbook preference helps",
        (ScoreFeature::PlaybookPreference, false) => "Playbook preference hurts",
        (ScoreFeature::RiskPenalty, true) => "Risk adjustment helps",
        (ScoreFeature::RiskPenalty, false) => "Risk penalty hurts",
    }
}
