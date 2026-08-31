//! 原子玩家动作入口。
//!
//! 这里拥有“一个动作”的校验和资源消耗；卡牌规则、随机分支与死亡结算
//! 继续委托给 executor。所有修改都发生在输入状态的副本上。

use crate::apply::{
    apply_program_with_lookups, SimCardDefinition, SimContext, SimOutcome, SimRequest,
    TargetBinding,
};
use crate::outcome::{
    OutcomeSet, ReproMeta, RuleTrace, SampleMethod, TraceEntry, UnsupportedReason,
};
use crate::state::{SimEntity, SimGameState};
use crate::SimError;
use hs_types::{CardType, EntityId, GamePhase, PlayerId, Zone};
use serde::{Deserialize, Serialize};

const MAX_BOARD_SLOTS: usize = 7;

/// 与服务器动作参数一一对应、但不反向依赖 advisor DTO 的沙盘动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimAction {
    PlayCard {
        entity: EntityId,
        target: Option<EntityId>,
        sub_option: Option<u32>,
        position: Option<u8>,
    },
    Attack {
        attacker: EntityId,
        defender: EntityId,
    },
    UseHeroPower {
        entity: EntityId,
        target: Option<EntityId>,
        sub_option: Option<u32>,
    },
    ActivateLocation {
        entity: EntityId,
        target: Option<EntityId>,
        sub_option: Option<u32>,
    },
    EndTurn,
}

/// 在副本上执行一个完整玩家动作。
pub fn apply_action(
    state: &SimGameState,
    action: &SimAction,
    context: &SimContext<'_>,
    request: &SimRequest,
) -> Result<SimOutcome, SimError> {
    let friendly = match validate_turn(state) {
        Ok(player) => player,
        Err(reason) => return Ok(unsupported_action(action, reason, request)),
    };
    if state.build_id != Some(context.build) {
        return Ok(unsupported_action(
            action,
            UnsupportedReason::InsufficientState {
                what: format!(
                    "动作 Build 不精确: state={:?}, context={}",
                    state.build_id, context.build.0
                ),
            },
            request,
        ));
    }
    if !matches!(action, SimAction::EndTurn) {
        if let Err(reason) = crate::executor::validate_strict_state(state) {
            return Ok(unsupported_action(action, reason, request));
        }
    }

    match *action {
        SimAction::PlayCard { .. } => play_card(state, friendly, action, context, request),
        SimAction::Attack { attacker, defender } => crate::executor::apply_combat_action(
            state, friendly, attacker, defender, context, request,
        ),
        SimAction::UseHeroPower {
            entity,
            target,
            sub_option,
        } => use_hero_power(
            state, friendly, entity, target, sub_option, context, request,
        ),
        SimAction::ActivateLocation {
            entity,
            target,
            sub_option,
        } => activate_location(
            state, friendly, entity, target, sub_option, context, request,
        ),
        SimAction::EndTurn => {
            let mut next = state.clone();
            next.turn_ended = true;
            Ok(deterministic_action(
                next,
                action,
                "current turn terminated",
                request,
            ))
        }
    }
}

fn play_card(
    state: &SimGameState,
    friendly: PlayerId,
    action: &SimAction,
    context: &SimContext<'_>,
    request: &SimRequest,
) -> Result<SimOutcome, SimError> {
    let SimAction::PlayCard {
        entity: source,
        target,
        sub_option,
        position,
    } = *action
    else {
        unreachable!("play_card only receives SimAction::PlayCard")
    };
    if sub_option.is_some() {
        return Ok(unsupported_action(
            action,
            UnsupportedReason::NotImplemented {
                what: "player action sub-option".into(),
            },
            request,
        ));
    }
    if let Err(reason) = validate_optional_target(state, target) {
        return Ok(unsupported_action(action, reason, request));
    }

    let mut next = state.clone();
    let definition =
        match prepare_source_definition(&mut next, source, friendly, Zone::Hand, context) {
            Ok(definition) => definition,
            Err(reason) => return Ok(unsupported_action(action, reason, request)),
        };

    let cost = next.entities[&source].cost.max(0);
    if let Err(reason) = spend_mana(&mut next, friendly, cost) {
        return Ok(unsupported_action(action, reason, request));
    }

    match definition.card_type {
        CardType::Minion | CardType::Location => {
            let Some(position) = position else {
                return Ok(unsupported_action(
                    action,
                    UnsupportedReason::InsufficientState {
                        what: "随从/Location 出牌缺少 1-based position".into(),
                    },
                    request,
                ));
            };
            if let Err(reason) = insert_into_play(&mut next, source, friendly, position) {
                return Ok(unsupported_action(action, reason, request));
            }
            if definition.card_type == CardType::Minion {
                let minion = next.entities.get_mut(&source).expect("validated source");
                minion.action.attacks_this_turn = 0;
                if minion.action.charge {
                    minion.action.exhausted = false;
                } else if minion.action.rush {
                    minion.action.exhausted = false;
                    // 本沙盘只展开当前回合；EndTurn 会终止后续展开，因此把
                    // Rush 的“本回合不能打脸”直接固化在当前分支即可。
                    minion.action.cannot_attack_heroes = true;
                } else {
                    minion.action.exhausted = true;
                }
            }
        }
        CardType::Spell => {
            if position.is_some() {
                return Ok(unsupported_action(
                    action,
                    UnsupportedReason::ActionUnavailable {
                        what: "法术不接受棋盘 position".into(),
                    },
                    request,
                ));
            }
            crate::executor::set_zone(&mut next, source, Zone::Graveyard);
        }
        CardType::Weapon => {
            if position.is_some() {
                return Ok(unsupported_action(
                    action,
                    UnsupportedReason::ActionUnavailable {
                        what: "武器不接受棋盘 position".into(),
                    },
                    request,
                ));
            }
            if next.entities.values().any(|entity| {
                entity.entity_id != source
                    && entity.controller == Some(friendly)
                    && entity.zone == Zone::Play
                    && entity.card_type == Some(CardType::Weapon)
            }) {
                return Ok(unsupported_action(
                    action,
                    UnsupportedReason::NotImplemented {
                        what: "weapon replacement".into(),
                    },
                    request,
                ));
            }
            if let Err(reason) = prepare_hero_attack_for_weapon(&mut next, friendly, context) {
                return Ok(unsupported_action(action, reason, request));
            }
            crate::executor::set_zone(&mut next, source, Zone::Play);
        }
        other => {
            return Ok(unsupported_action(
                action,
                UnsupportedReason::NotImplemented {
                    what: format!("play card type {other:?}"),
                },
                request,
            ))
        }
    }

    // Location 的 program.play 表示激活效果；初次入场绝不能提前触发。
    if definition.card_type == CardType::Location {
        return Ok(deterministic_action(
            next,
            action,
            "location entered play without activation",
            request,
        ));
    }

    apply_source_program(&next, source, target, context, request, "play_card")
}

fn prepare_hero_attack_for_weapon(
    state: &mut SimGameState,
    friendly: PlayerId,
    context: &SimContext<'_>,
) -> Result<(), UnsupportedReason> {
    let hero = state
        .hero_entity(friendly)
        .ok_or_else(|| UnsupportedReason::InsufficientState {
            what: format!("玩家 {} 缺少在场英雄实体", friendly.0),
        })?;
    if state.entities[&hero].action.attack_known {
        return Ok(());
    }
    let definition = prepare_source_definition(state, hero, friendly, Zone::Play, context)?;
    if definition.card_type != CardType::Hero {
        return Err(UnsupportedReason::InsufficientState {
            what: format!("entity {} 的英雄类型定义不一致", hero.0),
        });
    }
    Ok(())
}

fn use_hero_power(
    state: &SimGameState,
    friendly: PlayerId,
    source: EntityId,
    target: Option<EntityId>,
    sub_option: Option<u32>,
    context: &SimContext<'_>,
    request: &SimRequest,
) -> Result<SimOutcome, SimError> {
    if sub_option.is_some() {
        return Ok(unsupported_action(
            &SimAction::UseHeroPower {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::NotImplemented {
                what: "hero power sub-option".into(),
            },
            request,
        ));
    }
    if let Err(reason) = validate_optional_target(state, target) {
        return Ok(unsupported_action(
            &SimAction::UseHeroPower {
                entity: source,
                target,
                sub_option,
            },
            reason,
            request,
        ));
    }

    let mut next = state.clone();
    let definition =
        match prepare_source_definition(&mut next, source, friendly, Zone::Play, context) {
            Ok(definition) => definition,
            Err(reason) => {
                return Ok(unsupported_action(
                    &SimAction::UseHeroPower {
                        entity: source,
                        target,
                        sub_option,
                    },
                    reason,
                    request,
                ))
            }
        };
    if definition.card_type != CardType::HeroPower {
        return Ok(unsupported_action(
            &SimAction::UseHeroPower {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::ActionUnavailable {
                what: format!("entity {} 不是英雄技能", source.0),
            },
            request,
        ));
    }
    if next.entities[&source].action.exhausted {
        return Ok(unsupported_action(
            &SimAction::UseHeroPower {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::ActionUnavailable {
                what: "英雄技能本回合已使用".into(),
            },
            request,
        ));
    }
    let cost = next.entities[&source].cost.max(0);
    if let Err(reason) = spend_mana(&mut next, friendly, cost) {
        return Ok(unsupported_action(
            &SimAction::UseHeroPower {
                entity: source,
                target,
                sub_option,
            },
            reason,
            request,
        ));
    }
    next.entities
        .get_mut(&source)
        .expect("validated source")
        .action
        .exhausted = true;
    apply_source_program(&next, source, target, context, request, "use_hero_power")
}

fn activate_location(
    state: &SimGameState,
    friendly: PlayerId,
    source: EntityId,
    target: Option<EntityId>,
    sub_option: Option<u32>,
    context: &SimContext<'_>,
    request: &SimRequest,
) -> Result<SimOutcome, SimError> {
    if sub_option.is_some() {
        return Ok(unsupported_action(
            &SimAction::ActivateLocation {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::NotImplemented {
                what: "location sub-option".into(),
            },
            request,
        ));
    }
    if let Err(reason) = validate_optional_target(state, target) {
        return Ok(unsupported_action(
            &SimAction::ActivateLocation {
                entity: source,
                target,
                sub_option,
            },
            reason,
            request,
        ));
    }
    let mut next = state.clone();
    let definition =
        match prepare_source_definition(&mut next, source, friendly, Zone::Play, context) {
            Ok(definition) => definition,
            Err(reason) => {
                return Ok(unsupported_action(
                    &SimAction::ActivateLocation {
                        entity: source,
                        target,
                        sub_option,
                    },
                    reason,
                    request,
                ))
            }
        };
    if definition.card_type != CardType::Location {
        return Ok(unsupported_action(
            &SimAction::ActivateLocation {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::ActionUnavailable {
                what: format!("entity {} 不是 Location", source.0),
            },
            request,
        ));
    }
    let action_state = next.entities[&source].action;
    if action_state.location_cooldown > 0 || action_state.exhausted {
        return Ok(unsupported_action(
            &SimAction::ActivateLocation {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::ActionUnavailable {
                what: "Location 仍在冷却".into(),
            },
            request,
        ));
    }
    if !action_state.durability_known {
        return Ok(unsupported_action(
            &SimAction::ActivateLocation {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::InsufficientState {
                what: "Location 耐久未知".into(),
            },
            request,
        ));
    }
    if action_state.durability <= 0 {
        return Ok(unsupported_action(
            &SimAction::ActivateLocation {
                entity: source,
                target,
                sub_option,
            },
            UnsupportedReason::ActionUnavailable {
                what: "Location 已无可用次数".into(),
            },
            request,
        ));
    }
    {
        let location = next.entities.get_mut(&source).expect("validated source");
        location.action.durability -= 1;
        location.action.location_cooldown = 1;
        location.action.exhausted = true;
    }

    if next.entities[&source].action.durability == 0 {
        // 自 32.0 起，最后一次 Location 使用会先清掉自身，再结算召唤，
        // 从而在原本满场时也能为召唤物腾出一个格子。
        crate::executor::set_zone(&mut next, source, Zone::Graveyard);
    }
    crate::executor::apply_action_program(
        &next,
        source,
        target,
        context,
        request,
        "activate_location",
    )
}

fn validate_turn(state: &SimGameState) -> Result<PlayerId, UnsupportedReason> {
    if state.phase != GamePhase::Playing {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("当前阶段 {:?} 不能执行玩家动作", state.phase),
        });
    }
    if state.turn_ended {
        return Err(UnsupportedReason::ActionUnavailable {
            what: "当前沙盘回合已经结束".into(),
        });
    }
    let friendly = state
        .friendly_player
        .ok_or_else(|| UnsupportedReason::InsufficientState {
            what: "无法确定己方玩家".into(),
        })?;
    match state.current_player {
        Some(current) if current == friendly => Ok(friendly),
        Some(current) => Err(UnsupportedReason::ActionUnavailable {
            what: format!("当前是玩家 {} 的回合", current.0),
        }),
        None => Err(UnsupportedReason::InsufficientState {
            what: "无法确定当前回合玩家".into(),
        }),
    }
}

fn prepare_source_definition(
    state: &mut SimGameState,
    source: EntityId,
    friendly: PlayerId,
    expected_zone: Zone,
    context: &SimContext<'_>,
) -> Result<SimCardDefinition, UnsupportedReason> {
    let snapshot = state.entities.get(&source).cloned().ok_or_else(|| {
        UnsupportedReason::InsufficientState {
            what: format!("动作源实体 {} 不存在", source.0),
        }
    })?;
    if snapshot.controller != Some(friendly) {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("动作源实体 {} 不属于己方", source.0),
        });
    }
    if snapshot.zone != expected_zone {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!(
                "动作源实体 {} 区域为 {:?}，要求 {:?}",
                source.0, snapshot.zone, expected_zone
            ),
        });
    }
    let card_id = snapshot
        .card_id
        .ok_or_else(|| UnsupportedReason::InsufficientState {
            what: format!("动作源实体 {} 的 card_id 未观测", source.0),
        })?;
    let definition = context
        .cards
        .sim_card(context.build, &card_id)
        .ok_or_else(|| UnsupportedReason::InsufficientState {
            what: format!("Build {} 缺卡牌定义 {card_id}", context.build.0),
        })?;
    if definition.card_id != card_id {
        return Err(UnsupportedReason::InsufficientState {
            what: format!(
                "CardLookup 返回错卡: 请求 {card_id}，得到 {}",
                definition.card_id
            ),
        });
    }
    if snapshot
        .card_type
        .is_some_and(|card_type| card_type != definition.card_type)
    {
        return Err(UnsupportedReason::InsufficientState {
            what: format!("实体 {source:?} 的观测类型与 CardLookup 不一致"),
        });
    }
    hydrate_definition(
        state.entities.get_mut(&source).expect("validated source"),
        &definition,
    );
    Ok(definition)
}

fn hydrate_definition(entity: &mut SimEntity, definition: &SimCardDefinition) {
    entity.card_type = Some(definition.card_type);
    if !entity.action.cost_known {
        entity.cost = definition.cost;
        entity.action.cost_known = true;
    }
    if !entity.action.attack_known {
        entity.attack = definition.attack;
        entity.action.attack_known = true;
    }
    if !entity.action.health_known {
        entity.health = definition.health;
        entity.action.health_known = true;
    }
    if !entity.action.durability_known {
        entity.action.durability = definition.durability;
        entity.action.durability_known = true;
    }
}

fn validate_optional_target(
    state: &SimGameState,
    target: Option<EntityId>,
) -> Result<(), UnsupportedReason> {
    let Some(target) = target else {
        return Ok(());
    };
    match state.entity(target) {
        Some(entity) if entity.zone == Zone::Play => Ok(()),
        Some(entity) => Err(UnsupportedReason::ActionUnavailable {
            what: format!("目标实体 {} 不在场上（{:?}）", target.0, entity.zone),
        }),
        None => Err(UnsupportedReason::InsufficientState {
            what: format!("目标实体 {} 不存在", target.0),
        }),
    }
}

fn spend_mana(
    state: &mut SimGameState,
    player: PlayerId,
    amount: i32,
) -> Result<(), UnsupportedReason> {
    let available = state
        .players
        .get(&player)
        .ok_or_else(|| UnsupportedReason::InsufficientState {
            what: format!("玩家 {} 不存在", player.0),
        })?
        .mana;
    if available < amount {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("法力不足: 需要 {amount}，当前 {available}"),
        });
    }
    state
        .players
        .get_mut(&player)
        .expect("validated player")
        .mana -= amount;
    Ok(())
}

fn insert_into_play(
    state: &mut SimGameState,
    source: EntityId,
    controller: PlayerId,
    position: u8,
) -> Result<(), UnsupportedReason> {
    let board_entities = state
        .entities
        .values()
        .filter(|entity| {
            entity.entity_id != source
                && entity.controller == Some(controller)
                && entity.zone == Zone::Play
                && matches!(
                    entity.card_type,
                    Some(CardType::Minion | CardType::Location)
                )
        })
        .collect::<Vec<_>>();
    if board_entities.iter().any(|entity| {
        !entity
            .action
            .zone_position
            .is_some_and(|position| position > 0)
    }) {
        return Err(UnsupportedReason::InsufficientState {
            what: "己方在场随从/Location 缺少 zone_position".into(),
        });
    }
    let mut observed_positions = board_entities
        .iter()
        .filter_map(|entity| entity.action.zone_position)
        .collect::<Vec<_>>();
    observed_positions.sort_unstable();
    if observed_positions
        .iter()
        .enumerate()
        .any(|(index, observed)| usize::from(*observed) != index + 1)
    {
        return Err(UnsupportedReason::InsufficientState {
            what: format!("己方棋盘 zone_position 不连续: {observed_positions:?}"),
        });
    }
    let mut occupied = board_entities.len();
    if occupied >= MAX_BOARD_SLOTS {
        return Err(UnsupportedReason::ActionUnavailable {
            what: "己方棋盘已满（7 格）".into(),
        });
    }
    let expected = occupied + 1;
    if position == 0 || usize::from(position) > expected {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("position {position} 不在 1..={expected}"),
        });
    }
    for entity in state.entities.values_mut() {
        if entity.entity_id != source
            && entity.controller == Some(controller)
            && entity.zone == Zone::Play
            && entity
                .action
                .zone_position
                .is_some_and(|current| current >= position)
        {
            entity.action.zone_position = entity.action.zone_position.map(|value| value + 1);
        }
    }
    crate::executor::set_zone(state, source, Zone::Play);
    state
        .entities
        .get_mut(&source)
        .expect("validated source")
        .action
        .zone_position = Some(position);
    occupied += 1;
    debug_assert!(occupied <= MAX_BOARD_SLOTS);
    Ok(())
}

fn apply_source_program(
    state: &SimGameState,
    source: EntityId,
    target: Option<EntityId>,
    context: &SimContext<'_>,
    request: &SimRequest,
    action_name: &str,
) -> Result<SimOutcome, SimError> {
    let Some(card_id) = state
        .entity(source)
        .and_then(|entity| entity.card_id.as_ref())
    else {
        return Ok(unsupported_named(
            action_name,
            UnsupportedReason::InsufficientState {
                what: format!("动作源实体 {} 的 card_id 未观测", source.0),
            },
            request,
        ));
    };
    let Some(program) = context.rules.program_for(card_id) else {
        let mut outcome = unsupported_named(
            action_name,
            UnsupportedReason::InsufficientState {
                what: format!("缺少 {card_id} 的规则"),
            },
            request,
        );
        // Missing is structured coverage evidence, not merely a human-readable
        // failure reason. Planner continuations do not have a server-root
        // lookup to recover this CardId, so the simulator must preserve it.
        outcome.unknown_rule_cards.push(card_id.clone());
        return Ok(outcome);
    };
    let mut outcome = match apply_program_with_lookups(
        state,
        program,
        &TargetBinding {
            source: Some(source),
            target,
        },
        context,
        request,
    ) {
        Ok(outcome) => outcome,
        Err(SimError::InvalidBinding(message)) => {
            return Ok(unsupported_named(
                action_name,
                UnsupportedReason::InsufficientState {
                    what: format!("动作目标绑定不完整: {message}"),
                },
                request,
            ));
        }
        Err(error) => return Err(error),
    };
    outcome.trace.entries.insert(
        0,
        TraceEntry {
            op: "action".into(),
            detail: action_name.into(),
        },
    );
    Ok(outcome)
}

fn deterministic_action(
    state: SimGameState,
    action: &SimAction,
    detail: &str,
    request: &SimRequest,
) -> SimOutcome {
    SimOutcome {
        outcome: OutcomeSet::Deterministic(state),
        trace: RuleTrace {
            entries: vec![TraceEntry {
                op: "action".into(),
                detail: format!("{}: {detail}", action_name(action)),
            }],
        },
        meta: action_meta(action_name(action), request),
        unknown_rule_cards: Vec::new(),
        rule_uses: Vec::new(),
    }
}

fn unsupported_action(
    action: &SimAction,
    reason: UnsupportedReason,
    request: &SimRequest,
) -> SimOutcome {
    unsupported_named(action_name(action), reason, request)
}

fn unsupported_named(name: &str, reason: UnsupportedReason, request: &SimRequest) -> SimOutcome {
    SimOutcome {
        outcome: OutcomeSet::Unsupported(reason),
        trace: RuleTrace::default(),
        meta: action_meta(name, request),
        unknown_rule_cards: Vec::new(),
        rule_uses: Vec::new(),
    }
}

fn action_meta(name: &str, request: &SimRequest) -> ReproMeta {
    ReproMeta {
        seed: request.seed,
        branch_budget: request.branch_budget,
        method: SampleMethod::Exact,
        truncation: None,
        chance_kind: None,
        rule_version: format!("action:{name}"),
        pool_snapshot_hashes: Vec::new(),
    }
}

fn action_name(action: &SimAction) -> &'static str {
    match action {
        SimAction::PlayCard { .. } => "play_card",
        SimAction::Attack { .. } => "attack",
        SimAction::UseHeroPower { .. } => "use_hero_power",
        SimAction::ActivateLocation { .. } => "activate_location",
        SimAction::EndTurn => "end_turn",
    }
}
