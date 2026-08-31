//! 测试夹具:手造 SimGameState + 字面量 EffectProgram(不走 YAML)。

#![allow(dead_code)]

use hs_effect_ir::{EffectProgram, Operation, IR_SCHEMA_VERSION};
use hs_pools::{PoolSnapshot, WeightedCard};
use hs_sim::{
    apply_program, CardLookup, OutcomeSet, PoolResolver, SimCardDefinition, SimEntity,
    SimEntityActionState, SimError, SimGameState, SimOutcome, SimPlayer, SimRequest, TargetBinding,
};
use hs_types::{
    BuildId, BuildRange, CardId, CardType, EntityId, GamePhase, PlayerId, PoolId, RuleStatus, Zone,
};

pub struct EmptyPool;

impl PoolResolver for EmptyPool {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        panic!("测试不应请求池 {pool_id}");
    }
}

pub struct FixedPool {
    pub snapshot: PoolSnapshot,
}

impl PoolResolver for FixedPool {
    fn snapshot(&self, _pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        Ok(self.snapshot.clone())
    }
}

pub fn pool_snapshot(pool: &str, cards: &[&str]) -> PoolSnapshot {
    let mut candidates: Vec<WeightedCard> = cards
        .iter()
        .map(|id| WeightedCard {
            card_id: CardId::new(*id),
            dbf_id: None,
            weight: 1.0,
        })
        .collect();
    candidates.sort_by(|a, b| a.card_id.cmp(&b.card_id));
    PoolSnapshot {
        pool_id: PoolId::new(pool),
        build_id: BuildId(1),
        context_hash: format!("hash-{pool}"),
        candidates,
        trace: Vec::new(),
        exact: true,
    }
}

pub fn program(card: &str, play: Vec<Operation>) -> EffectProgram {
    EffectProgram {
        schema: IR_SCHEMA_VERSION,
        card_id: CardId::new(card),
        build_range: BuildRange {
            min: BuildId(1),
            max: None,
        },
        status: RuleStatus::Verified,
        confidence: 1.0,
        triggers: Vec::new(),
        play,
        custom_handler: None,
        evidence: Vec::new(),
    }
}

pub fn hero(id: i32, card: &str, controller: i32, health: i32) -> SimEntity {
    SimEntity {
        entity_id: EntityId(id),
        card_id: Some(CardId::new(card)),
        card_type: Some(CardType::Hero),
        zone: Zone::Play,
        controller: Some(PlayerId(controller)),
        attack: 0,
        health,
        damage: 0,
        cost: 0,
        action: SimEntityActionState {
            cost_known: true,
            attack_known: true,
            health_known: true,
            ..SimEntityActionState::default()
        },
        simulated: false,
    }
}

pub fn minion(id: i32, card: &str, controller: i32, attack: i32, health: i32) -> SimEntity {
    SimEntity {
        entity_id: EntityId(id),
        card_id: Some(CardId::new(card)),
        card_type: Some(CardType::Minion),
        zone: Zone::Play,
        controller: Some(PlayerId(controller)),
        attack,
        health,
        damage: 0,
        cost: 2,
        action: SimEntityActionState {
            cost_known: true,
            attack_known: true,
            health_known: true,
            ..SimEntityActionState::default()
        },
        simulated: false,
    }
}

pub fn player(
    id: i32,
    health: i32,
    armor: i32,
    mana: i32,
    max_mana: i32,
    deck: usize,
    hand: usize,
) -> SimPlayer {
    SimPlayer {
        player_id: PlayerId(id),
        hero_health: health,
        armor,
        mana,
        max_mana,
        overloaded: 0,
        deck_count: deck,
        hand_count: hand,
        fatigue: 0,
    }
}

pub fn board(entities: Vec<SimEntity>, players: Vec<SimPlayer>) -> SimGameState {
    SimGameState {
        entities: entities.into_iter().map(|e| (e.entity_id, e)).collect(),
        players: players.into_iter().map(|p| (p.player_id, p)).collect(),
        build_id: Some(BuildId(1)),
        friendly_player: Some(PlayerId(1)),
        current_player: Some(PlayerId(1)),
        phase: GamePhase::Playing,
        turn: 1,
        turn_ended: false,
        source_revision: 1,
        next_sim_entity: 0,
    }
}

pub fn default_state() -> SimGameState {
    board(
        vec![
            hero(4, "HERO_01", 1, 30),
            hero(5, "HERO_02", 2, 30),
            minion(10, "CS2_182", 1, 4, 5),
        ],
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    )
}

pub fn apply_ops(state: &SimGameState, play: Vec<Operation>) -> SimOutcome {
    apply_program(
        state,
        &program("TEST_CARD", play),
        &TargetBinding::default(),
        &EmptyPool,
        &SimRequest::default(),
    )
    .expect("apply_program")
}

pub fn apply_bound(
    state: &SimGameState,
    play: Vec<Operation>,
    binding: TargetBinding,
) -> SimOutcome {
    apply_program(
        state,
        &program("TEST_CARD", play),
        &binding,
        &EmptyPool,
        &SimRequest::default(),
    )
    .expect("apply_program")
}

pub fn expect_det(outcome: &SimOutcome) -> &SimGameState {
    match &outcome.outcome {
        OutcomeSet::Deterministic(state) => state,
        other => panic!("expected Deterministic, got {other:?}"),
    }
}

pub fn trace_ops(outcome: &SimOutcome) -> Vec<&str> {
    outcome
        .trace
        .entries
        .iter()
        .map(|e| e.op.as_str())
        .collect()
}

pub fn two_player_with_hand_counts(hand: usize) -> SimGameState {
    let mut state = default_state();
    state.players.get_mut(&PlayerId(1)).unwrap().hand_count = hand;
    state
}

/// 测试用规则表:`CardId` → `EffectProgram`。
///
/// 生产侧的适配器由 M4 接线(desktop 已有 `RulesStore` 可直接包一层)。
pub struct MapRules {
    programs: std::collections::BTreeMap<CardId, EffectProgram>,
}

impl MapRules {
    pub fn new(entries: Vec<EffectProgram>) -> Self {
        Self {
            programs: entries
                .into_iter()
                .map(|p| (p.card_id.clone(), p))
                .collect(),
        }
    }
}

impl hs_sim::RuleLookup for MapRules {
    fn program_for(&self, card_id: &CardId) -> Option<&EffectProgram> {
        self.programs.get(card_id)
    }
}

/// 测试用精确卡牌定义表；键同时包含 Build，避免测试绕过版本合同。
pub struct MapCards {
    definitions: std::collections::BTreeMap<(BuildId, CardId), SimCardDefinition>,
}

impl MapCards {
    pub fn new(build: BuildId, definitions: Vec<SimCardDefinition>) -> Self {
        Self {
            definitions: definitions
                .into_iter()
                .map(|definition| ((build, definition.card_id.clone()), definition))
                .collect(),
        }
    }
}

impl CardLookup for MapCards {
    fn sim_card(&self, build: BuildId, card_id: &CardId) -> Option<SimCardDefinition> {
        self.definitions.get(&(build, card_id.clone())).cloned()
    }
}

pub fn card_definition(
    card: &str,
    card_type: CardType,
    cost: i32,
    attack: i32,
    health: i32,
    durability: i32,
) -> SimCardDefinition {
    SimCardDefinition {
        card_id: CardId::new(card),
        card_type,
        cost,
        attack,
        health,
        durability,
    }
}

/// 造一张只有亡语的卡:`card` 死亡时执行 `effects`。
pub fn deathrattle_card(card: &str, effects: Vec<Operation>) -> EffectProgram {
    let mut p = program(card, Vec::new());
    p.triggers = vec![hs_effect_ir::TriggerProgram {
        event: hs_effect_ir::TriggerEvent::Deathrattle,
        when: Vec::new(),
        effects,
    }];
    p
}

/// 带规则表执行(死亡结算能查到别的实体的亡语)。
pub fn apply_with_rules(
    state: &SimGameState,
    play: Vec<Operation>,
    binding: TargetBinding,
    rules: &dyn hs_sim::RuleLookup,
) -> SimOutcome {
    hs_sim::apply_program_with_rules(
        state,
        &program("TEST_CARD", play),
        &binding,
        &EmptyPool,
        rules,
        &SimRequest::default(),
    )
    .expect("apply_program_with_rules")
}
