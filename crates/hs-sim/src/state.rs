//! SimGameState:可变的局面沙盘副本。
//!
//! 从 [`ObservedGameState`] 按「已观测事实」构造:未知区域(对手手牌/牌库
//! 具体牌)显式保留 Unknown 计数,不猜测(风险条款 design §9)。

use hs_observed_state::{EntityState, ObservedGameState, PlayerState};
use hs_types::{BuildId, CardId, CardType, EntityId, GamePhase, PlayerId, Zone};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 玩家动作所需、且必须随沙盘副本一起变化的实体事实。
///
/// 整体 `serde(default)` 保证旧快照缺少任意新增字段时仍可读取；四个
/// `*_known` 区分「日志里真实为 0」和「从未观测到，需 CardLookup 补齐」。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default)]
pub struct SimEntityActionState {
    pub zone_position: Option<u8>,
    pub cost_known: bool,
    pub attack_known: bool,
    pub health_known: bool,
    pub durability: i32,
    pub durability_known: bool,
    pub attacks_this_turn: i32,
    pub exhausted: bool,
    pub frozen: bool,
    pub taunt: bool,
    pub stealth: bool,
    pub location_cooldown: i32,
    pub location_action_cost: i32,
    pub windfury: bool,
    pub mega_windfury: bool,
    pub divine_shield: bool,
    pub poisonous: bool,
    pub lifesteal: bool,
    pub immune: bool,
    pub immune_while_attacking: bool,
    pub charge: bool,
    pub rush: bool,
    pub cant_attack: bool,
    pub cant_be_attacked: bool,
    pub cannot_attack_heroes: bool,
}

/// 沙盘内实体(观测实体的可变投影)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimEntity {
    pub entity_id: EntityId,
    /// None = 该实体牌面未观测(如对手手牌)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_id: Option<CardId>,
    /// 卡牌类型;None = `CARDTYPE` tag 未观测(不猜测)。
    ///
    /// 由 `EntityState::card_type()` 投影而来 —— 数值映射只此一处,
    /// 本 crate 不重写游戏枚举表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_type: Option<CardType>,
    pub zone: Zone,
    pub controller: Option<PlayerId>,
    pub attack: i32,
    pub health: i32,
    pub damage: i32,
    pub cost: i32,
    #[serde(default)]
    pub action: SimEntityActionState,
    /// 模拟中新生成的实体(负 entity_id 段,与日志实体区分)。
    #[serde(default)]
    pub simulated: bool,
}

impl SimEntity {
    /// 当前生命 = `health − damage`(与观测态 `current_health` 同口径)。
    pub fn current_health(&self) -> i32 {
        self.health - self.damage
    }

    /// 是否为英雄实体。
    ///
    /// 优先用观测到的 `card_type`;仅在 `CARDTYPE` tag 未观测时才退回
    /// 旧的 card_id 启发式(`HERO_*` 且不含 hero power 后缀 `bp`),
    /// 保证不比 M3 的行为差。
    pub fn is_hero(&self) -> bool {
        match self.card_type {
            Some(CardType::Hero) => true,
            Some(_) => false,
            None => self
                .card_id
                .as_ref()
                .is_some_and(|id| is_hero_card_id(id.as_str())),
        }
    }

    /// 生命归零时是否走死亡结算(随从/武器/地标;英雄归零是游戏结束)。
    ///
    /// `card_type` 未观测时退回:在场且牌面已知即当作随从
    /// (与 M3 `infer_card_type` 的旧启发式同口径)。
    pub fn dies_on_zero_health(&self) -> bool {
        match self.card_type {
            Some(ct) => ct.dies_on_zero_health(),
            None => self.zone == Zone::Play && self.card_id.is_some() && !self.is_hero(),
        }
    }
}

/// 沙盘内玩家。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimPlayer {
    pub player_id: PlayerId,
    pub hero_health: i32,
    pub armor: i32,
    pub mana: i32,
    pub max_mana: i32,
    pub overloaded: i32,
    /// 牌库剩余张数(具体牌未知时只有计数)。
    pub deck_count: usize,
    pub hand_count: usize,
    pub fatigue: i32,
}

/// 可变局面沙盘(§13;BTreeMap 保证遍历序确定,复现性要求)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SimGameState {
    pub entities: BTreeMap<EntityId, SimEntity>,
    pub players: BTreeMap<PlayerId, SimPlayer>,
    /// CardLookup / RuleLookup 的精确版本键；旧快照缺失时保持未知。
    #[serde(default)]
    pub build_id: Option<BuildId>,
    pub friendly_player: Option<PlayerId>,
    /// 当前服务端回合玩家；不能由 friendly_player 猜测。
    #[serde(default)]
    pub current_player: Option<PlayerId>,
    pub phase: GamePhase,
    pub turn: i32,
    /// 仅由沙盘内 EndTurn 动作设置；从 Observed 新建时总为 false。
    #[serde(default)]
    pub turn_ended: bool,
    /// 源自的观测状态修订号(建议绑定 Revision,§21 M4 验收前置)。
    pub source_revision: u64,
    /// 模拟内自增序号(新实体分配负 ID 用)。
    pub next_sim_entity: i32,
}

impl SimGameState {
    /// 从观测状态构造沙盘副本(冻结签名)。
    ///
    /// 未知信息处理:对手手牌/牌库只有计数;未观测 card_id 保持 None。
    /// 不从「应该有多少牌」反推未出现的实体。
    pub fn from_observed(observed: &ObservedGameState) -> Self {
        let entities: BTreeMap<EntityId, SimEntity> = observed
            .entities
            .iter()
            .map(|(id, entity)| (*id, project_entity(entity)))
            .collect();

        let mut players = BTreeMap::new();
        for (raw_id, player) in &observed.players {
            let player_id = PlayerId(*raw_id);
            players.insert(
                player_id,
                project_player(player, player_id, observed, &entities),
            );
        }

        Self {
            entities,
            players,
            build_id: observed.build_id,
            friendly_player: observed.friendly_player,
            current_player: observed.current_player,
            phase: observed.phase,
            turn: i32::try_from(observed.turn).unwrap_or(i32::MAX),
            turn_ended: false,
            source_revision: observed.revision,
            next_sim_entity: 0,
        }
    }

    /// 分配模拟实体 ID(负数段,与日志实体永不冲突)。
    pub fn alloc_sim_entity(&mut self) -> EntityId {
        self.next_sim_entity -= 1;
        EntityId(self.next_sim_entity)
    }

    /// 按 ID 取实体。
    pub fn entity(&self, entity_id: EntityId) -> Option<&SimEntity> {
        self.entities.get(&entity_id)
    }

    /// 按 ID 取玩家。
    pub fn player(&self, player_id: PlayerId) -> Option<&SimPlayer> {
        self.players.get(&player_id)
    }

    /// 某玩家在场英雄实体(entity_id 升序取第一个 `is_hero`)。
    pub fn hero_entity(&self, player: PlayerId) -> Option<EntityId> {
        self.entities.values().find_map(|entity| {
            if entity.controller == Some(player) && entity.zone == Zone::Play && entity.is_hero() {
                Some(entity.entity_id)
            } else {
                None
            }
        })
    }
}

/// card_id 是否像英雄牌(排除 `HERO_*bp*` 英雄技能)。
pub(crate) fn is_hero_card_id(id: &str) -> bool {
    id.starts_with("HERO_") && !id.contains("bp")
}

fn project_entity(entity: &EntityState) -> SimEntity {
    let attack = entity.observed_attack();
    let health = entity.observed_health();
    let cost = entity.observed_cost();
    let durability = entity.observed_durability();

    SimEntity {
        entity_id: entity.entity_id,
        card_id: entity.card_id.clone(),
        card_type: entity.card_type(),
        zone: entity.zone,
        controller: entity.controller,
        attack: attack.unwrap_or(0),
        health: health.unwrap_or(0),
        damage: entity.damage(),
        cost: cost.unwrap_or(0),
        action: SimEntityActionState {
            zone_position: entity.zone_position,
            cost_known: cost.is_some(),
            attack_known: attack.is_some(),
            health_known: health.is_some(),
            durability: durability.unwrap_or(0),
            durability_known: durability.is_some(),
            attacks_this_turn: entity.attacks_this_turn(),
            exhausted: entity.is_exhausted(),
            frozen: entity.is_frozen(),
            taunt: entity.has_taunt(),
            stealth: entity.has_stealth(),
            location_cooldown: entity.location_action_cooldown(),
            location_action_cost: entity.location_action_cost(),
            windfury: entity.has_windfury(),
            mega_windfury: entity.has_mega_windfury(),
            divine_shield: entity.has_divine_shield(),
            poisonous: entity.has_poisonous(),
            lifesteal: entity.has_lifesteal(),
            immune: entity.is_immune(),
            immune_while_attacking: entity.is_immune_while_attacking(),
            charge: entity.has_charge(),
            rush: entity.has_rush(),
            cant_attack: entity.cannot_attack(),
            cant_be_attacked: entity.cannot_be_attacked(),
            cannot_attack_heroes: entity.cannot_attack_heroes(),
        },
        simulated: false,
    }
}

fn project_player(
    player: &PlayerState,
    player_id: PlayerId,
    observed: &ObservedGameState,
    entities: &BTreeMap<EntityId, SimEntity>,
) -> SimPlayer {
    let deck_count = count_zone(entities, player_id, Zone::Deck);
    let hand_count = count_zone(entities, player_id, Zone::Hand);

    // 英雄生命/护甲:优先 PlayerState.hero_entity,其次 CARDTYPE=HERO 的在场实体。
    // 都读不到时填 0——不猜测「标准 30 血」。
    let hero_obs = player
        .hero_entity
        .and_then(|id| observed.entities.get(&id))
        .or_else(|| find_hero_entity(observed, player_id));
    let hero_health = hero_obs.map(EntityState::current_health).unwrap_or(0);
    let armor = hero_obs.map(EntityState::armor).unwrap_or(0);

    // 法力:RESOURCES − RESOURCES_USED;缺 tag 当 0,不按回合数反推。
    let max_mana = player.resources.unwrap_or(0);
    let used = player.resources_used.unwrap_or(0);
    let mana = max_mana.saturating_sub(used);

    // 过载:玩家实体或英雄实体上的 OVERLOAD_OWED;缺省 0。
    let overloaded = overload_owed(observed, player);

    SimPlayer {
        player_id,
        hero_health,
        armor,
        mana,
        max_mana,
        overloaded,
        deck_count,
        hand_count,
        // 观测态不跟踪疲劳计数,默认 0。
        fatigue: 0,
    }
}

fn count_zone(entities: &BTreeMap<EntityId, SimEntity>, player: PlayerId, zone: Zone) -> usize {
    entities
        .values()
        .filter(|entity| entity.controller == Some(player) && entity.zone == zone)
        .count()
}

fn find_hero_entity(observed: &ObservedGameState, player: PlayerId) -> Option<&EntityState> {
    observed.entities.values().find(|entity| {
        entity.controller == Some(player)
            && entity.zone == Zone::Play
            && entity.card_type() == Some(CardType::Hero)
    })
}

fn overload_owed(observed: &ObservedGameState, player: &PlayerState) -> i32 {
    let from_player_entity = observed
        .entities
        .get(&player.entity_id)
        .and_then(EntityState::observed_overload_owed);
    let from_hero = player
        .hero_entity
        .and_then(|id| observed.entities.get(&id))
        .and_then(EntityState::observed_overload_owed);
    from_player_entity.or(from_hero).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_types::CardId;

    #[test]
    fn from_observed_keeps_unrevealed_card_id_none() {
        let mut observed = ObservedGameState::default();
        observed.revision = 7;
        observed.phase = GamePhase::Playing;
        observed.turn = 3;
        observed.friendly_player = Some(PlayerId(1));

        let mut hidden = EntityState::new(EntityId(13), None, Zone::Hand);
        hidden.controller = Some(PlayerId(2));
        observed.entities.insert(EntityId(13), hidden);

        let mut hero = EntityState::new(EntityId(4), Some(CardId::new("HERO_01")), Zone::Play);
        hero.controller = Some(PlayerId(1));
        hero.tags.insert(45, 30);
        hero.tags.insert(44, 4);
        observed.entities.insert(EntityId(4), hero);

        let sim = SimGameState::from_observed(&observed);
        let hidden = sim.entity(EntityId(13)).expect("hidden entity");
        assert_eq!(hidden.card_id, None);
        assert_eq!(hidden.zone, Zone::Hand);
        let hero = sim.entity(EntityId(4)).expect("hero");
        assert_eq!(hero.card_id.as_ref().map(CardId::as_str), Some("HERO_01"));
        assert_eq!(hero.health, 30);
        assert_eq!(hero.damage, 4);
        assert_eq!(hero.current_health(), 26);
        assert!(hero.is_hero());
        assert!(sim.players.is_empty(), "无 PlayerState 时不捏造玩家");
        assert_eq!(sim.friendly_player, Some(PlayerId(1)));
        assert_eq!(sim.phase, GamePhase::Playing);
        assert_eq!(sim.turn, 3);
        assert_eq!(sim.source_revision, 7);
    }

    #[test]
    fn alloc_sim_entity_uses_negative_ids() {
        let mut state = SimGameState::default();
        assert_eq!(state.alloc_sim_entity(), EntityId(-1));
        assert_eq!(state.alloc_sim_entity(), EntityId(-2));
    }
}
