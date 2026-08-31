use std::collections::BTreeMap;

use hs_advisor_api::{ScoreBreakdown, ScoreComponent, ScoreFeature};
use hs_planner::{explain_score, score_state, PlaybookCatalog, ScoreError, ScoreExplanation};
use hs_sim::{SimEntity, SimEntityActionState, SimGameState, SimPlayer};
use hs_types::{BuildId, CardType, EntityId, GamePhase, PlayerId, Zone};

const FRIENDLY: PlayerId = PlayerId(1);
const OPPONENT: PlayerId = PlayerId(2);

fn playbook_yaml(friendly_attack_weight: i32, friendly_attack_text: &str) -> String {
    format!(
        r#"
schema_version: 1
id: generic
priority: 0
applies_to:
  build_range:
    min: 0
  deck_signatures: []
weights:
  friendly_effective_health: 1000
  opponent_effective_health: -1000
  friendly_board_attack: {friendly_attack_weight}
  opponent_board_attack: -1000
  friendly_board_health: 1000
  opponent_board_health: -1000
  friendly_board_value: 1000
  opponent_board_value: -1000
  friendly_hand_size: 1000
  opponent_hand_size: -1000
  resource_advantage: 1000
  mana_efficiency: 1000
  playbook_preference: 1000
  risk_penalty: -1000
risk_weight_milli: 250
win_points:
  - id: a_pressure
    feature: opponent_effective_health
    comparison: at_most
    threshold: 27
    message: Pressure is online.
  - id: b_survival
    feature: friendly_effective_health
    comparison: at_least
    threshold: 20
    message: Survival is stable.
explanations:
  friendly_effective_health:
    positive: Friendly health helps.
    negative: Friendly health hurts.
  opponent_effective_health:
    positive: Opponent health helps.
    negative: Opponent health hurts.
  friendly_board_attack:
    positive: {friendly_attack_text}
    negative: {friendly_attack_text}
  opponent_board_attack:
    positive: Opponent attack helps.
    negative: Opponent attack hurts.
"#
    )
}

fn catalog(friendly_attack_weight: i32, friendly_attack_text: &str) -> PlaybookCatalog {
    PlaybookCatalog::from_sources([(
        "generic.yaml",
        playbook_yaml(friendly_attack_weight, friendly_attack_text),
    )])
    .expect("valid score test Playbook")
}

fn hero(id: i32, player: PlayerId, health: i32, damage: i32) -> SimEntity {
    SimEntity {
        entity_id: EntityId(id),
        card_id: None,
        card_type: Some(CardType::Hero),
        zone: Zone::Play,
        controller: Some(player),
        attack: 0,
        health,
        damage,
        cost: 0,
        action: SimEntityActionState {
            health_known: true,
            ..SimEntityActionState::default()
        },
        simulated: false,
    }
}

fn board_entity(
    id: i32,
    player: PlayerId,
    card_type: Option<CardType>,
    attack: i32,
    health: i32,
    damage: i32,
    durability: Option<i32>,
) -> SimEntity {
    SimEntity {
        entity_id: EntityId(id),
        card_id: None,
        card_type,
        zone: Zone::Play,
        controller: Some(player),
        attack,
        health,
        damage,
        cost: 0,
        action: SimEntityActionState {
            attack_known: true,
            health_known: true,
            durability: durability.unwrap_or_default(),
            durability_known: durability.is_some(),
            ..SimEntityActionState::default()
        },
        simulated: false,
    }
}

fn player(
    player_id: PlayerId,
    hero_health: i32,
    armor: i32,
    mana: i32,
    hand_count: usize,
) -> SimPlayer {
    SimPlayer {
        player_id,
        hero_health,
        armor,
        mana,
        max_mana: 10,
        overloaded: 0,
        deck_count: 20,
        hand_count,
        fatigue: 0,
    }
}

fn fixture() -> (SimGameState, SimGameState) {
    let mut state = SimGameState {
        entities: BTreeMap::new(),
        players: BTreeMap::from([
            (FRIENDLY, player(FRIENDLY, 20, 5, 5, 3)),
            (OPPONENT, player(OPPONENT, 25, 2, 3, 5)),
        ]),
        build_id: Some(BuildId(1)),
        friendly_player: Some(FRIENDLY),
        current_player: Some(FRIENDLY),
        phase: GamePhase::Playing,
        turn: 8,
        turn_ended: false,
        source_revision: 1,
        next_sim_entity: 0,
    };
    for entity in [
        hero(1, FRIENDLY, 30, 10),
        hero(2, OPPONENT, 30, 5),
        board_entity(10, FRIENDLY, Some(CardType::Minion), 3, 4, 1, None),
        board_entity(11, FRIENDLY, Some(CardType::Weapon), 2, 0, 0, Some(2)),
        board_entity(12, FRIENDLY, Some(CardType::Location), 0, 0, 0, Some(3)),
        board_entity(20, OPPONENT, Some(CardType::Minion), 4, 5, 2, None),
        board_entity(21, OPPONENT, Some(CardType::Weapon), 1, 0, 0, Some(3)),
        // Unknown and non-board card types must not leak their placeholder
        // stats into a score.
        board_entity(30, FRIENDLY, None, 999, 999, 0, Some(99)),
        board_entity(31, FRIENDLY, Some(CardType::Spell), 999, 999, 0, Some(99)),
    ] {
        state.entities.insert(entity.entity_id, entity);
    }

    let mut initial = state.clone();
    initial
        .players
        .get_mut(&FRIENDLY)
        .expect("friendly fixture player")
        .mana = 10;
    (initial, state)
}

fn component(breakdown: &ScoreBreakdown, feature: ScoreFeature) -> ScoreComponent {
    *breakdown
        .components()
        .iter()
        .find(|component| component.feature == feature)
        .expect("feature is present")
}

#[test]
fn all_features_are_exact_stable_and_recomputable() {
    let catalog = catalog(1_000, "Friendly attack helps.");
    let selected = catalog.select(BuildId(1), None).expect("generic selection");
    let (initial, state) = fixture();

    let breakdown = score_state(&initial, &state, &selected, 4, 7).expect("score");

    assert_eq!(
        breakdown
            .components()
            .iter()
            .map(|component| component.feature)
            .collect::<Vec<_>>(),
        ScoreFeature::ALL
    );
    let expected = [
        (ScoreFeature::FriendlyEffectiveHealth, 25, 1_000),
        (ScoreFeature::OpponentEffectiveHealth, 27, -1_000),
        (ScoreFeature::FriendlyBoardAttack, 5, 1_000),
        (ScoreFeature::OpponentBoardAttack, 5, -1_000),
        (ScoreFeature::FriendlyBoardHealth, 3, 1_000),
        (ScoreFeature::OpponentBoardHealth, 3, -1_000),
        (ScoreFeature::FriendlyBoardValue, 13, 1_000),
        (ScoreFeature::OpponentBoardValue, 11, -1_000),
        (ScoreFeature::FriendlyHandSize, 3, 1_000),
        (ScoreFeature::OpponentHandSize, 5, -1_000),
        (ScoreFeature::ResourceAdvantage, 2, 1_000),
        (ScoreFeature::ManaEfficiency, 5, 1_000),
        (ScoreFeature::PlaybookPreference, 7, 1_000),
        (ScoreFeature::RiskPenalty, 4, -1_000),
    ];
    for (feature, raw, weight_milli) in expected {
        assert_eq!(
            component(&breakdown, feature),
            ScoreComponent {
                feature,
                raw,
                weight_milli,
            }
        );
    }

    for feature in [
        ScoreFeature::FriendlyEffectiveHealth,
        ScoreFeature::FriendlyBoardAttack,
        ScoreFeature::FriendlyBoardHealth,
        ScoreFeature::FriendlyBoardValue,
        ScoreFeature::FriendlyHandSize,
        ScoreFeature::ResourceAdvantage,
        ScoreFeature::ManaEfficiency,
        ScoreFeature::PlaybookPreference,
    ] {
        assert!(component(&breakdown, feature).weighted_milli() > 0);
    }
    for feature in [
        ScoreFeature::OpponentEffectiveHealth,
        ScoreFeature::OpponentBoardAttack,
        ScoreFeature::OpponentBoardHealth,
        ScoreFeature::OpponentBoardValue,
        ScoreFeature::OpponentHandSize,
        ScoreFeature::RiskPenalty,
    ] {
        assert!(component(&breakdown, feature).weighted_milli() < 0);
    }
    assert_eq!(
        breakdown.total_milli(),
        breakdown
            .components()
            .iter()
            .map(|component| i128::from(component.weighted_milli()))
            .sum::<i128>()
    );
    assert_eq!(breakdown.total_milli(), 8_000);
}

#[test]
fn signed_deltas_and_non_negative_risk_input_are_preserved() {
    let catalog = catalog(1_000, "Friendly attack helps.");
    let selected = catalog.select(BuildId(1), None).expect("selection");
    let (_, mut state) = fixture();
    state.players.get_mut(&FRIENDLY).expect("friendly").mana = 5;
    state.players.get_mut(&OPPONENT).expect("opponent").mana = 10;
    let mut initial = state.clone();
    initial.players.get_mut(&FRIENDLY).expect("friendly").mana = 3;

    let breakdown = score_state(&initial, &state, &selected, -9, -7).expect("score");

    assert_eq!(
        component(&breakdown, ScoreFeature::ResourceAdvantage).raw,
        -5
    );
    assert_eq!(component(&breakdown, ScoreFeature::ManaEfficiency).raw, -2);
    assert_eq!(
        component(&breakdown, ScoreFeature::PlaybookPreference).raw,
        -7
    );
    assert_eq!(component(&breakdown, ScoreFeature::RiskPenalty).raw, 0);
}

#[test]
fn missing_players_heroes_and_health_are_explicit_errors() {
    let catalog = catalog(1_000, "Friendly attack helps.");
    let selected = catalog.select(BuildId(1), None).expect("selection");
    let (initial, state) = fixture();

    let mut missing_player = state.clone();
    missing_player.players.remove(&FRIENDLY);
    assert_eq!(
        score_state(&initial, &missing_player, &selected, 0, 0),
        Err(ScoreError::MissingFriendlyPlayerState(FRIENDLY))
    );

    let mut missing_opponent = state.clone();
    missing_opponent.players.remove(&OPPONENT);
    assert_eq!(
        score_state(&initial, &missing_opponent, &selected, 0, 0),
        Err(ScoreError::MissingOpponentPlayer)
    );

    let mut missing_hero = state.clone();
    missing_hero.entities.remove(&EntityId(1));
    assert_eq!(
        score_state(&initial, &missing_hero, &selected, 0, 0),
        Err(ScoreError::MissingFriendlyHero)
    );

    let mut missing_opponent_hero = state.clone();
    missing_opponent_hero.entities.remove(&EntityId(2));
    assert_eq!(
        score_state(&initial, &missing_opponent_hero, &selected, 0, 0),
        Err(ScoreError::MissingOpponentHero)
    );

    let mut unknown_friendly_health = state.clone();
    unknown_friendly_health
        .entities
        .get_mut(&EntityId(1))
        .expect("friendly hero")
        .action
        .health_known = false;
    assert_eq!(
        score_state(&initial, &unknown_friendly_health, &selected, 0, 0),
        Err(ScoreError::MissingFriendlyHeroHealth)
    );

    let mut unknown_opponent_health = state;
    unknown_opponent_health
        .entities
        .get_mut(&EntityId(2))
        .expect("opponent hero")
        .action
        .health_known = false;
    assert_eq!(
        score_state(&initial, &unknown_opponent_health, &selected, 0, 0),
        Err(ScoreError::MissingOpponentHeroHealth)
    );
}

#[test]
fn every_raw_input_is_clamped_before_weighting() {
    let catalog = catalog(1_000, "Friendly attack helps.");
    let selected = catalog.select(BuildId(1), None).expect("selection");
    let (_, mut state) = fixture();
    {
        let friendly = state.players.get_mut(&FRIENDLY).expect("friendly");
        friendly.hero_health = i32::MAX;
        friendly.armor = i32::MAX;
        friendly.hand_count = usize::MAX;
        friendly.mana = i32::MIN;
    }
    state.players.get_mut(&OPPONENT).expect("opponent").mana = i32::MAX;
    let minion = state.entities.get_mut(&EntityId(10)).expect("minion");
    minion.attack = i32::MAX;
    minion.health = i32::MAX;
    minion.damage = 0;
    let mut initial = state.clone();
    initial.players.get_mut(&FRIENDLY).expect("friendly").mana = i32::MAX;

    let high = score_state(&initial, &state, &selected, i32::MAX, i32::MAX).expect("score");
    assert_eq!(
        component(&high, ScoreFeature::FriendlyEffectiveHealth).raw,
        1_000
    );
    assert_eq!(
        component(&high, ScoreFeature::FriendlyBoardAttack).raw,
        10_000
    );
    assert_eq!(
        component(&high, ScoreFeature::FriendlyBoardHealth).raw,
        10_000
    );
    assert_eq!(
        component(&high, ScoreFeature::FriendlyBoardValue).raw,
        30_000
    );
    assert_eq!(component(&high, ScoreFeature::FriendlyHandSize).raw, 100);
    assert_eq!(component(&high, ScoreFeature::ResourceAdvantage).raw, -100);
    assert_eq!(component(&high, ScoreFeature::ManaEfficiency).raw, 100);
    assert_eq!(
        component(&high, ScoreFeature::PlaybookPreference).raw,
        10_000
    );
    assert_eq!(component(&high, ScoreFeature::RiskPenalty).raw, 10_000);

    state.players.get_mut(&FRIENDLY).expect("friendly").mana = i32::MAX;
    state.players.get_mut(&OPPONENT).expect("opponent").mana = i32::MIN;
    initial.players.get_mut(&FRIENDLY).expect("friendly").mana = i32::MIN;
    let low = score_state(&initial, &state, &selected, i32::MIN, i32::MIN).expect("score");
    assert_eq!(component(&low, ScoreFeature::ResourceAdvantage).raw, 100);
    assert_eq!(component(&low, ScoreFeature::ManaEfficiency).raw, -100);
    assert_eq!(
        component(&low, ScoreFeature::PlaybookPreference).raw,
        -10_000
    );
    assert_eq!(component(&low, ScoreFeature::RiskPenalty).raw, 0);
}

#[test]
fn explanation_uses_stable_contribution_order_and_win_point() {
    let catalog = catalog(1_000, "Friendly attack helps.");
    let selected = catalog.select(BuildId(1), None).expect("selection");
    let breakdown = ScoreBreakdown::new([
        ScoreComponent {
            feature: ScoreFeature::OpponentBoardAttack,
            raw: 1,
            weight_milli: -1_000,
        },
        ScoreComponent {
            feature: ScoreFeature::FriendlyBoardAttack,
            raw: 1,
            weight_milli: -1_000,
        },
        ScoreComponent {
            feature: ScoreFeature::OpponentEffectiveHealth,
            raw: 1,
            weight_milli: 1_000,
        },
        ScoreComponent {
            feature: ScoreFeature::FriendlyEffectiveHealth,
            raw: 20,
            weight_milli: 50,
        },
    ])
    .expect("unique components");

    let ScoreExplanation {
        win_point,
        explanation,
    } = explain_score(&breakdown, &selected);

    assert_eq!(win_point.as_deref(), Some("Pressure is online."));
    let friendly_health = explanation
        .find("Friendly health helps.")
        .expect("friendly health");
    let opponent_health = explanation
        .find("Opponent health helps.")
        .expect("opponent health");
    let friendly_attack = explanation
        .find("Friendly attack helps.")
        .expect("friendly attack");
    let opponent_attack = explanation
        .find("Opponent attack hurts.")
        .expect("opponent attack");
    assert!(friendly_health < opponent_health);
    assert!(opponent_health < friendly_attack);
    assert!(friendly_attack < opponent_attack);
}

#[test]
fn explanation_without_a_template_uses_stable_generic_text() {
    let catalog = catalog(1_000, "Friendly attack helps.");
    let selected = catalog.select(BuildId(1), None).expect("selection");
    let breakdown = ScoreBreakdown::new([ScoreComponent {
        feature: ScoreFeature::ResourceAdvantage,
        raw: 3,
        weight_milli: 1_000,
    }])
    .expect("unique component");

    assert_eq!(
        explain_score(&breakdown, &selected).explanation,
        "Resource advantage helps (raw 3, +3000 milli)"
    );
}

#[test]
fn changing_playbook_changes_only_weights_and_explanation() {
    let aggressive_catalog = catalog(10_000, "Aggressive pressure.");
    let cautious_catalog = catalog(-10_000, "Cautious restraint.");
    let aggressive = aggressive_catalog
        .select(BuildId(1), None)
        .expect("aggressive selection");
    let cautious = cautious_catalog
        .select(BuildId(1), None)
        .expect("cautious selection");
    let (initial, state) = fixture();
    let original_initial = initial.clone();
    let original_state = state.clone();

    let aggressive_score = score_state(&initial, &state, &aggressive, 4, 7).expect("score");
    let cautious_score = score_state(&initial, &state, &cautious, 4, 7).expect("score");

    assert_eq!(initial, original_initial);
    assert_eq!(state, original_state);
    assert_eq!(
        aggressive_score
            .components()
            .iter()
            .map(|component| (component.feature, component.raw))
            .collect::<Vec<_>>(),
        cautious_score
            .components()
            .iter()
            .map(|component| (component.feature, component.raw))
            .collect::<Vec<_>>()
    );
    assert_ne!(aggressive_score, cautious_score);
    assert_ne!(
        explain_score(&aggressive_score, &aggressive),
        explain_score(&cautious_score, &cautious)
    );
    assert!(explain_score(&aggressive_score, &aggressive)
        .explanation
        .contains("Aggressive pressure."));
    assert!(explain_score(&cautious_score, &cautious)
        .explanation
        .contains("Cautious restraint."));
}
