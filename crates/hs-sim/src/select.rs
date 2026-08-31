//! Selector / Predicate 求值(只筛选不改状态,§11.6)。

use crate::apply::TargetBinding;
use crate::state::SimGameState;
use hs_effect_ir::{EntityCondition, EntityRole, PlayerRef, Predicate, Selector};
use hs_types::{CardType, EntityId, PlayerId, Zone};

/// 选择失败:绑定错误走 `SimError`,状态不足走 Unsupported。
#[derive(Debug)]
pub(crate) enum SelectError {
    Binding(String),
    Insufficient(String),
}

/// 按冻结语义筛选实体,结果按 `entity_id` 升序去重。
pub(crate) fn resolve_selector(
    state: &SimGameState,
    binding: &TargetBinding,
    selector: &Selector,
) -> Result<Vec<EntityId>, SelectError> {
    let mut ids = resolve_selector_raw(state, binding, selector)?;
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn resolve_selector_raw(
    state: &SimGameState,
    binding: &TargetBinding,
    selector: &Selector,
) -> Result<Vec<EntityId>, SelectError> {
    match selector {
        Selector::SelfEntity | Selector::Source => bound(binding.source, "source"),
        Selector::Target => bound(binding.target, "target"),
        Selector::FriendlyHero => {
            let player = self_player(state, binding)?;
            Ok(state.hero_entity(player).into_iter().collect())
        }
        Selector::EnemyHero => {
            let player = opponent_player(state, binding)?;
            Ok(state.hero_entity(player).into_iter().collect())
        }
        Selector::FriendlyMinions => {
            let player = self_player(state, binding)?;
            Ok(minions_in_play(state, player))
        }
        Selector::EnemyMinions => {
            let player = opponent_player(state, binding)?;
            Ok(minions_in_play(state, player))
        }
        Selector::EntitiesInZone(zone) => Ok(state
            .entities
            .values()
            .filter(|entity| entity.zone == *zone)
            .map(|entity| entity.entity_id)
            .collect()),
        Selector::Filter(inner, condition) => {
            let inner_ids = resolve_selector_raw(state, binding, inner)?;
            let mut kept = Vec::new();
            for id in inner_ids {
                let entity = state.entities.get(&id).ok_or_else(|| {
                    SelectError::Binding(format!("selector 引用了缺失实体 {id:?}"))
                })?;
                if entity_matches(state, binding, entity, condition)? {
                    kept.push(id);
                }
            }
            Ok(kept)
        }
        Selector::Union(parts) => {
            let mut all = Vec::new();
            for part in parts {
                all.extend(resolve_selector_raw(state, binding, part)?);
            }
            Ok(all)
        }
    }
}

/// 判定 Predicate:角色实体不存在 → 假(不报错)。
pub(crate) fn predicate_holds(
    state: &SimGameState,
    binding: &TargetBinding,
    predicate: &Predicate,
) -> Result<bool, SelectError> {
    let Some(entity_id) = role_entity(binding, predicate.subject) else {
        return Ok(false);
    };
    let Some(entity) = state.entities.get(&entity_id) else {
        return Ok(false);
    };
    entity_matches(state, binding, entity, &predicate.condition)
}

pub(crate) fn all_predicates(
    state: &SimGameState,
    binding: &TargetBinding,
    predicates: &[Predicate],
) -> Result<bool, SelectError> {
    for predicate in predicates {
        if !predicate_holds(state, binding, predicate)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn resolve_player_ref(
    state: &SimGameState,
    binding: &TargetBinding,
    player: PlayerRef,
) -> Result<Vec<PlayerId>, SelectError> {
    match player {
        PlayerRef::SelfPlayer => Ok(vec![self_player(state, binding)?]),
        PlayerRef::Opponent => Ok(vec![opponent_player(state, binding)?]),
        PlayerRef::Both => {
            let mut ids: Vec<PlayerId> = state.players.keys().copied().collect();
            if ids.is_empty() {
                if let Ok(self_id) = self_player(state, binding) {
                    ids.push(self_id);
                }
            }
            ids.sort();
            ids.dedup();
            if ids.is_empty() {
                return Err(SelectError::Binding("Both: 局面没有玩家".into()));
            }
            Ok(ids)
        }
    }
}

fn bound(id: Option<EntityId>, name: &str) -> Result<Vec<EntityId>, SelectError> {
    match id {
        Some(id) => Ok(vec![id]),
        None => Err(SelectError::Binding(format!(
            "Selector 引用了 {name},但 TargetBinding.{name} 为空"
        ))),
    }
}

fn self_player(state: &SimGameState, binding: &TargetBinding) -> Result<PlayerId, SelectError> {
    if let Some(source) = binding.source {
        if let Some(controller) = state.entities.get(&source).and_then(|e| e.controller) {
            return Ok(controller);
        }
    }
    state.friendly_player.ok_or_else(|| {
        SelectError::Insufficient("无法确定己方玩家(无 source.controller / friendly_player)".into())
    })
}

fn opponent_player(state: &SimGameState, binding: &TargetBinding) -> Result<PlayerId, SelectError> {
    let self_id = self_player(state, binding)?;
    state
        .players
        .keys()
        .copied()
        .find(|id| *id != self_id)
        .ok_or_else(|| SelectError::Insufficient("无法确定对手玩家".into()))
}

fn role_entity(binding: &TargetBinding, role: EntityRole) -> Option<EntityId> {
    match role {
        EntityRole::SelfEntity | EntityRole::Source | EntityRole::Attacker => binding.source,
        EntityRole::Target | EntityRole::Defender => binding.target,
    }
}

fn minions_in_play(state: &SimGameState, player: PlayerId) -> Vec<EntityId> {
    state
        .entities
        .values()
        .filter(|entity| {
            entity.controller == Some(player)
                && entity.zone == Zone::Play
                && entity.card_type == Some(CardType::Minion)
        })
        .map(|entity| entity.entity_id)
        .collect()
}

fn entity_matches(
    state: &SimGameState,
    binding: &TargetBinding,
    entity: &crate::state::SimEntity,
    condition: &EntityCondition,
) -> Result<bool, SelectError> {
    if let Some(controller) = condition.controller {
        let allowed = resolve_player_ref(state, binding, controller)?;
        match entity.controller {
            Some(id) if allowed.contains(&id) => {}
            Some(_) => return Ok(false),
            None => return Ok(false),
        }
    }
    if let Some(zone) = condition.zone {
        if entity.zone != zone {
            return Ok(false);
        }
    }
    if let Some(ref want) = condition.card_id {
        match entity.card_id.as_ref() {
            None => {
                return Err(SelectError::Insufficient(
                    "unrevealed card_id: Selector 需要具体牌面".into(),
                ));
            }
            Some(got) if got != want => return Ok(false),
            Some(_) => {}
        }
    }
    if let Some(max) = condition.cost_at_most {
        if entity.cost > max {
            return Ok(false);
        }
    }
    if let Some(min) = condition.cost_at_least {
        if entity.cost < min {
            return Ok(false);
        }
    }
    if let Some(ref card_type) = condition.card_type {
        match infer_card_type(entity) {
            None if entity.card_id.is_none() => {
                return Err(SelectError::Insufficient(
                    "unrevealed card_type: Selector 需要对手手牌具体牌".into(),
                ));
            }
            None => return Ok(false),
            Some(got) if !got.eq_ignore_ascii_case(card_type) => return Ok(false),
            Some(_) => {}
        }
    }
    if condition.card_class.is_some() {
        // SimEntity 无职业字段,本 crate 无卡牌库:要职业必须先有牌面,然后仍无法判定。
        if entity.card_id.is_none() {
            return Err(SelectError::Insufficient(
                "unrevealed card_class: Selector 需要对手手牌具体牌".into(),
            ));
        }
        return Ok(false);
    }
    Ok(true)
}

/// 实体的卡牌类型码(对齐卡牌库 `card_type` 列,即 `EntityCondition.card_type`
/// 的取值口径)。
///
/// 优先用观测到的 `card_type`;仅在 `CARDTYPE` tag 未观测时才退回 M3 的旧
/// 启发式(在场且牌面已知 ⇒ 当作随从),保证不比 M3 的行为差。
fn infer_card_type(entity: &crate::state::SimEntity) -> Option<&'static str> {
    if let Some(ct) = entity.card_type {
        return match ct {
            CardType::Hero => Some("HERO"),
            CardType::Minion => Some("MINION"),
            CardType::Spell => Some("SPELL"),
            CardType::Weapon => Some("WEAPON"),
            CardType::HeroPower => Some("HERO_POWER"),
            CardType::Location => Some("LOCATION"),
            CardType::Enchantment => Some("ENCHANTMENT"),
            // GAME/PLAYER/Invalid/Unknown 不是可被 Selector 匹配的牌类型。
            CardType::Game | CardType::Player | CardType::Invalid | CardType::Unknown(_) => None,
        };
    }
    if entity.is_hero() {
        return Some("HERO");
    }
    if entity.zone == Zone::Play && entity.card_id.is_some() {
        return Some("MINION");
    }
    None
}
