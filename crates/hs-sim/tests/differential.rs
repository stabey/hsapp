//! 真实日志差分:IR 执行器预测 vs 观测状态(M3 Track D)。
//!
//! 素材:`tests/logs/real/2026-08-21-ranked-standard-loss.power.log`
//! (PlayerB = PlayerId 2 = 友方,本局告负)。
//!
//! 管道与 `hs-observed-state` 的 real_logs / `from_observed` 相同:
//! parse → normalize → 逐事件 reduce,可在任意事件点取快照。

mod common;

use common::{apply_bound, apply_ops, expect_det};
use hs_effect_ir::{Operation, PlayerRef, Selector};
use hs_log_core::{
    game_tag, DomainEvent, EventNormalizer, Packet, PowerEventNormalizer, PowerLogParser, TagKey,
};
use hs_log_tailer::{ParseStatus, RawLogRecord};
use hs_observed_state::{reduce, ObservedGameState};
use hs_sim::{SimGameState, TargetBinding};
use hs_types::{EntityId, GamePhase, LogFileId, LogSessionId, PlayerId, Zone};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

const LOG_NAME: &str = "2026-08-21-ranked-standard-loss.power.log";

/// 友方第一次对局中抽牌(跳过起手/调度)。
///
/// `SHOW_ENTITY` id=48 `CORE_EX1_129`(刀扇),DECK→HAND。
/// 包行 1907–1923,其中 `tag=ZONE value=HAND` 在 1913。
const DRAW_LINE_START: usize = 1907;
const DRAW_LINE_END: usize = 1923;
const DRAW_ENTITY: i32 = 48;

/// 友方英雄第一次受伤:扛包收尸人(id=21)攻击貂蝉瓦莉拉(id=66),
/// `DAMAGE` 0→1。行 2388;攻击块从 2363 起。此时无护甲。
const DAMAGE_LINE: usize = 2388;
const DAMAGE_ENTITY: i32 = 66;
const DAMAGE_AMOUNT: i32 = 1;

/// 两个连续的真实攻击块：先由 12 攻随从把护甲 16→4，再由 6 攻随从
/// 把剩余 4 点护甲打光并让英雄实际掉 2 点生命。
const ARMOR_ATTACK_SAMPLES: &[(usize, usize, i32, i32)] =
    &[(58821, 58862, 16, 4), (58933, 58976, 4, 0)];

/// GameState 权威流中六组「先入墓、后以兄弟块触发亡语」的真实证据。
///
/// `(实体 id, ZONE→GRAVEYARD 行, DEATHRATTLE BLOCK_START 行)`。
const D3_DEATHRATTLE_SAMPLES: &[(i32, usize, usize)] = &[
    (9, 7265, 7295),
    (50, 11332, 11358),
    (43, 13786, 13789),
    (39, 16944, 16949),
    (150, 31140, 31146),
    (318, 39103, 39133),
];

struct LocatedEvent {
    event: DomainEvent,
    line_start: usize,
    line_end: usize,
}

struct AttackBlockSample {
    before: ObservedGameState,
    after: ObservedGameState,
    source: EntityId,
    target: EntityId,
    line_start: usize,
    line_end: usize,
}

/// 回放到第 `index` 个事件之前的快照,并扫描第一个满足谓词的事件。
fn replay_to(events: &[LocatedEvent], index: usize) -> ObservedGameState {
    events[..index]
        .iter()
        .fold(ObservedGameState::default(), |state, located| {
            reduce(&state, &located.event).expect("reduce")
        })
}

fn find_first(
    events: &[LocatedEvent],
    pred: impl Fn(&ObservedGameState, &LocatedEvent) -> bool,
) -> (usize, ObservedGameState) {
    let mut state = ObservedGameState::default();
    for (index, located) in events.iter().enumerate() {
        if pred(&state, located) {
            return (index, state);
        }
        state = reduce(&state, &located.event).expect("reduce");
    }
    panic!("日志中没有找到满足谓词的事件");
}

fn load_events() -> Vec<LocatedEvent> {
    let packets = PowerLogParser::parse_records(real_log_records());
    let mut normalizer = PowerEventNormalizer::new();
    let mut events = Vec::new();
    for packet in &packets {
        let (line_start, line_end) = packet_line_span(packet);
        for event in normalizer.normalize(packet).expect("normalize") {
            events.push(LocatedEvent {
                event,
                line_start,
                line_end,
            });
        }
    }
    events
}

/// 取一个完整 ATTACK 块两侧的生产观察快照。
///
/// 攻击块里可能嵌套触发器，所以用 reducer 的块栈深度找对应的外层
/// `BLOCK_END`，不能把第一个 `BlockClosed` 当作攻击结束。
fn attack_block_sample(events: &[LocatedEvent], expected_line: usize) -> AttackBlockSample {
    let (open_index, before) = find_first(events, |_, located| {
        located.line_start == expected_line
            && matches!(
                &located.event,
                DomainEvent::BlockOpened { block_type, .. } if block_type == "ATTACK"
            )
    });
    let (source, target) = match &events[open_index].event {
        DomainEvent::BlockOpened { source, target, .. } => (
            source.expect("攻击块应有 source"),
            target.expect("攻击块应有 target"),
        ),
        _ => unreachable!("查找谓词只接受 ATTACK BlockOpened"),
    };
    let parent_depth = before.block_stack.len();
    let mut after = before.clone();
    let mut line_end = expected_line;

    for located in &events[open_index..] {
        after = reduce(&after, &located.event).expect("reduce attack block");
        line_end = line_end.max(located.line_end);
        if matches!(located.event, DomainEvent::BlockClosed)
            && after.block_stack.len() == parent_depth
        {
            return AttackBlockSample {
                before,
                after,
                source,
                target,
                line_start: expected_line,
                line_end,
            };
        }
    }

    panic!("日志行 {expected_line} 的 ATTACK 块没有闭合");
}

fn real_log_records() -> Vec<RawLogRecord> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../tests/logs/real/{LOG_NAME}"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(idx, line)| RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            // 1-based 文件行号(含空行位置);本 fixture 无空行,与物理行号一致。
            byte_offset: (idx + 1) as u64,
            timestamp: None,
            raw_line: Arc::from(line),
            parse_status: ParseStatus::Pending,
        })
        .collect()
}

fn packet_line_span(packet: &Packet) -> (usize, usize) {
    let mut start = usize::MAX;
    let mut end = 0;
    for record in packet.source_records() {
        let line = record.byte_offset as usize;
        start = start.min(line);
        end = end.max(line);
    }
    if start == usize::MAX {
        (0, 0)
    } else {
        (start, end)
    }
}

fn zone_counts(state: &SimGameState, player: PlayerId) -> (usize, usize) {
    let player = state
        .player(player)
        .unwrap_or_else(|| panic!("玩家 {player:?} 不存在"));
    (player.hand_count, player.deck_count)
}

fn is_friendly_turn_draw(before: &ObservedGameState, located: &LocatedEvent) -> bool {
    if before.phase != GamePhase::Playing {
        return false;
    }
    let Some(friendly) = before.friendly_player else {
        return false;
    };
    let DomainEvent::TagChanged {
        entity_id,
        tag,
        value,
    } = &located.event
    else {
        return false;
    };
    if *tag != TagKey::ZONE || *value != game_tag::zone::HAND {
        return false;
    }
    let Some(entity) = before.entities.get(entity_id) else {
        return false;
    };
    entity.zone == Zone::Deck && entity.controller == Some(friendly)
}

fn damage_increase(before: &ObservedGameState, located: &LocatedEvent) -> Option<(EntityId, i32)> {
    let DomainEvent::TagChanged {
        entity_id,
        tag,
        value,
    } = &located.event
    else {
        return None;
    };
    if *tag != TagKey::DAMAGE {
        return None;
    }
    let old = before
        .entities
        .get(entity_id)
        .map(hs_observed_state::EntityState::damage)
        .unwrap_or(0);
    if *value <= old {
        return None;
    }
    Some((*entity_id, *value - old))
}

#[test]
fn draw_hand_and_deck_counts_match_observed() {
    let events = load_events();
    let (index, before_obs) = find_first(&events, is_friendly_turn_draw);
    let located = &events[index];
    assert_eq!(
        (located.line_start, located.line_end),
        (DRAW_LINE_START, DRAW_LINE_END),
        "抽牌事件行号漂移,请核日志后改注释与常量"
    );

    let after_obs = reduce(&before_obs, &located.event).expect("reduce");
    let friendly = before_obs
        .friendly_player
        .expect("对局中抽牌时友方应已判定");

    let before_sim = SimGameState::from_observed(&before_obs);
    let drawn = before_obs
        .entities
        .iter()
        .find_map(|(id, entity)| {
            if entity.zone == Zone::Deck && entity.controller == Some(friendly) {
                let after = after_obs.entities.get(id)?;
                (after.zone == Zone::Hand).then_some(*id)
            } else {
                None
            }
        })
        .expect("Deck→Hand 实体");
    assert_eq!(drawn, EntityId(DRAW_ENTITY));

    let outcome = apply_ops(
        &before_sim,
        vec![Operation::Draw {
            player: PlayerRef::SelfPlayer,
            count: 1,
        }],
    );
    let sim_after = expect_det(&outcome);
    let obs_after = SimGameState::from_observed(&after_obs);

    let (sim_hand, sim_deck) = zone_counts(sim_after, friendly);
    let (obs_hand, obs_deck) = zone_counts(&obs_after, friendly);
    assert_eq!(
        (sim_hand, sim_deck),
        (obs_hand, obs_deck),
        "抽牌差分不一致: 模拟 hand/deck={sim_hand}/{sim_deck} 观测 hand/deck={obs_hand}/{obs_deck} (日志行 {}-{})",
        located.line_start,
        located.line_end
    );
    assert!(
        outcome.trace.entries.iter().any(|entry| entry.op == "draw"),
        "RuleTrace 应记录 draw"
    );
}

#[test]
fn damage_on_friendly_hero_matches_observed() {
    let events = load_events();
    let (index, before_obs) = find_first(&events, |before, located| {
        damage_increase(before, located).is_some()
    });
    let located = &events[index];
    assert_eq!(
        located.line_start, DAMAGE_LINE,
        "伤害事件行号漂移,请核日志后改注释与常量"
    );
    assert_eq!(located.line_end, DAMAGE_LINE);

    let (entity_id, amount) = damage_increase(&before_obs, located).expect("伤害增量");
    assert_eq!(entity_id, EntityId(DAMAGE_ENTITY));
    assert_eq!(amount, DAMAGE_AMOUNT);

    let after_obs = reduce(&before_obs, &located.event).expect("reduce");
    let before_sim = SimGameState::from_observed(&before_obs);
    let outcome = apply_bound(
        &before_sim,
        vec![Operation::Damage {
            target: Selector::Target,
            amount,
        }],
        TargetBinding {
            source: None,
            target: Some(entity_id),
        },
    );
    let sim_after = expect_det(&outcome);
    let obs_after = SimGameState::from_observed(&after_obs);

    let sim_entity = sim_after
        .entity(entity_id)
        .unwrap_or_else(|| panic!("模拟缺少实体 {entity_id:?}"));
    let obs_entity = obs_after
        .entity(entity_id)
        .unwrap_or_else(|| panic!("观测缺少实体 {entity_id:?}"));
    assert_eq!(
        sim_entity.damage, obs_entity.damage,
        "伤害差分不一致: 模拟 damage={} 观测 damage={} (日志行 {})",
        sim_entity.damage, obs_entity.damage, located.line_start
    );

    if sim_entity.is_hero() {
        let pid = sim_entity.controller.expect("英雄应有控制器");
        let sim_armor = sim_after.player(pid).map(|p| p.armor).unwrap_or(0);
        let obs_armor = obs_after.player(pid).map(|p| p.armor).unwrap_or(0);
        assert_eq!(
            sim_armor, obs_armor,
            "伤害差分(护甲)不一致: 模拟 armor={sim_armor} 观测 armor={obs_armor} (日志行 {})",
            located.line_start
        );
    }
    assert!(
        outcome
            .trace
            .entries
            .iter()
            .any(|entry| entry.op == "damage"),
        "RuleTrace 应记录 damage"
    );
}

#[test]
fn hero_armor_attack_blocks_match_observed_effective_health() {
    let events = load_events();

    for &(line_start, line_end, armor_before, armor_after) in ARMOR_ATTACK_SAMPLES {
        let sample = attack_block_sample(&events, line_start);
        assert_eq!(sample.line_end, line_end, "攻击块物理行号漂移");

        let before_sim = SimGameState::from_observed(&sample.before);
        let observed_after = SimGameState::from_observed(&sample.after);
        let attacker = before_sim
            .entity(sample.source)
            .unwrap_or_else(|| panic!("攻击者 {:?} 不在块前快照", sample.source));
        let target = before_sim
            .entity(sample.target)
            .unwrap_or_else(|| panic!("目标 {:?} 不在块前快照", sample.target));
        let target_player = target.controller.expect("英雄目标应有控制器");
        assert!(target.is_hero(), "护甲样本目标必须是英雄");

        let before_player = before_sim.player(target_player).expect("块前目标玩家");
        let observed_player = observed_after.player(target_player).expect("块后目标玩家");
        assert_eq!(before_player.armor, armor_before, "块前护甲样本漂移");
        assert_eq!(observed_player.armor, armor_after, "块后护甲样本漂移");

        let outcome = apply_bound(
            &before_sim,
            vec![Operation::Damage {
                target: Selector::Target,
                amount: attacker.attack,
            }],
            TargetBinding {
                source: Some(sample.source),
                target: Some(sample.target),
            },
        );
        let simulated_after = expect_det(&outcome);
        let simulated_target = simulated_after
            .entity(sample.target)
            .expect("模拟后英雄目标");
        let observed_target = observed_after
            .entity(sample.target)
            .expect("观测后英雄目标");
        let simulated_player = simulated_after
            .player(target_player)
            .expect("模拟后目标玩家");

        assert_eq!(
            (simulated_target.current_health(), simulated_player.armor),
            (observed_target.current_health(), observed_player.armor),
            "护甲攻击块差分不一致 (日志行 {}-{})",
            sample.line_start,
            sample.line_end
        );
        assert_eq!(
            target.current_health() + before_player.armor
                - simulated_target.current_health()
                - simulated_player.armor,
            attacker.attack,
            "模拟有效生命应准确减少攻击力 (日志行 {}-{})",
            sample.line_start,
            sample.line_end
        );
        assert_eq!(
            target.current_health() + before_player.armor
                - observed_target.current_health()
                - observed_player.armor,
            attacker.attack,
            "真实日志有效生命应准确减少攻击力 (日志行 {}-{})",
            sample.line_start,
            sample.line_end
        );
    }
}

#[test]
fn replay_helpers_round_trip_same_snapshot() {
    // 辅助自检:replay_to(N) 应等于 find 返回的 before。
    let events = load_events();
    let (index, before) = find_first(&events, is_friendly_turn_draw);
    let replayed = replay_to(&events, index);
    assert_eq!(before.revision, replayed.revision);
    assert_eq!(before.friendly_player, replayed.friendly_player);
    assert_eq!(before.entities.len(), replayed.entities.len());
}

/// D3:真实日志证明死亡实体先离场，随后才在 DEATHS 的兄弟块触发亡语。
///
/// GameState 的 `BLOCK_START Entity=... zone=PLAY` 是旧文本快照，不能拿它判断
/// 当时区域；这里用同一生产 parser → normalizer → reducer 得到的状态作准。
#[test]
fn deathrattle_sibling_blocks_start_after_source_reaches_graveyard() {
    // PowerTaskList 是 GameState 的重复视图。只回放权威流，避免把同一亡语数两次。
    let records = real_log_records()
        .into_iter()
        .filter(|record| record.raw_line.contains("GameState.DebugPrintPower()"))
        .collect::<Vec<_>>();
    let packets = PowerLogParser::parse_records(records);
    let mut normalizer = PowerEventNormalizer::new();
    let mut state = ObservedGameState::default();
    let mut graveyard_evidence = BTreeMap::new();
    let mut found = BTreeMap::new();

    for packet in &packets {
        let (line_start, _) = packet_line_span(packet);

        if let Packet::BlockStart(block) = packet {
            if block.trigger_keyword.as_deref() == Some("DEATHRATTLE") {
                let source = block
                    .source
                    .unwrap_or_else(|| panic!("亡语块缺 source (日志行 {line_start})"));
                let (graveyard_line, parent_stack) = graveyard_evidence
                    .get(&source)
                    .unwrap_or_else(|| {
                        panic!(
                            "实体 {source:?} 触发亡语前没有 PLAY→GRAVEYARD 记录 (日志行 {line_start})"
                        )
                    });
                let entity = state.entities.get(&source).unwrap_or_else(|| {
                    panic!("亡语实体 {source:?} 不在观察状态 (日志行 {line_start})")
                });

                assert_eq!(
                    entity.zone,
                    Zone::Graveyard,
                    "实体 {source:?} 触发亡语时尚未入墓 (入墓行 {graveyard_line}, 触发行 {line_start})"
                );
                assert!(
                    *graveyard_line < line_start,
                    "实体 {source:?} 的入墓必须早于亡语 (入墓行 {graveyard_line}, 触发行 {line_start})"
                );
                assert_eq!(
                    &state.block_stack, parent_stack,
                    "实体 {source:?} 的亡语应回到 DEATHS 之前的父块，再以兄弟块触发 (日志行 {line_start})"
                );
                assert!(
                    found
                        .insert(source, (*graveyard_line, line_start))
                        .is_none(),
                    "GameState 权威流中实体 {source:?} 的亡语被重复计数"
                );
            }
        }

        for event in normalizer.normalize(packet).expect("normalize") {
            if let DomainEvent::TagChanged {
                entity_id,
                tag,
                value,
            } = &event
            {
                if *tag == TagKey::ZONE
                    && *value == game_tag::zone::GRAVEYARD
                    && D3_DEATHRATTLE_SAMPLES
                        .iter()
                        .any(|(id, _, _)| EntityId(*id) == *entity_id)
                    && state
                        .entities
                        .get(entity_id)
                        .is_some_and(|entity| entity.zone == Zone::Play)
                {
                    let deaths = state.block_stack.last().unwrap_or_else(|| {
                        panic!("实体 {entity_id:?} 入墓时块栈为空 (日志行 {line_start})")
                    });
                    assert_eq!(
                        deaths.block_type, "DEATHS",
                        "实体 {entity_id:?} 的 PLAY→GRAVEYARD 应发生在 DEATHS 块内 (日志行 {line_start})"
                    );
                    graveyard_evidence.insert(
                        *entity_id,
                        (
                            line_start,
                            state.block_stack[..state.block_stack.len() - 1].to_vec(),
                        ),
                    );
                }
            }
            state = reduce(&state, &event).expect("reduce");
        }
    }

    assert_eq!(
        found.len(),
        D3_DEATHRATTLE_SAMPLES.len(),
        "真实日志中的亡语样本数漂移: {found:?}"
    );
    for &(entity, graveyard_line, trigger_line) in D3_DEATHRATTLE_SAMPLES {
        assert_eq!(
            found.get(&EntityId(entity)),
            Some(&(graveyard_line, trigger_line)),
            "D3 样本漂移: entity={entity}"
        );
    }
}

/// 一个随从从 PLAY 进 GRAVEYARD 的事件(死亡)。
///
/// 只认「先前在 PLAY 且 CARDTYPE=MINION」的实体,排除手牌弃牌、法术入坟等。
fn minion_death(before: &ObservedGameState, located: &LocatedEvent) -> Option<EntityId> {
    let DomainEvent::TagChanged {
        entity_id,
        tag,
        value,
    } = &located.event
    else {
        return None;
    };
    if *tag != TagKey::ZONE || *value != game_tag::zone::GRAVEYARD {
        return None;
    }
    let entity = before.entities.get(entity_id)?;
    (entity.zone == Zone::Play
        && entity.card_type() == Some(hs_types::CardType::Minion)
        && entity.current_health() > 0)
        .then_some(*entity_id)
}

/// A8:真实日志里的随从死亡 —— 模拟的死亡结算 = 观测。
///
/// 差分口径:取观测到的**死亡前**局面,用执行器对该随从打出正好致死的伤害,
/// 比对「进坟」这一结果与观测一致。不比对致死原因(日志里多为攻击,而 Attack
/// 在 design §4 执行子集之外),比对的是本任务新增的死亡结算阶段。
#[test]
fn minion_death_matches_observed() {
    let events = load_events();
    let (index, before_obs) = find_first(&events, |before, located| {
        minion_death(before, located).is_some()
    });
    let located = &events[index];
    let dead_id = minion_death(&before_obs, located).expect("谓词已命中");

    let after_obs = reduce(&before_obs, &located.event).expect("reduce");
    assert_eq!(
        after_obs.entities[&dead_id].zone,
        Zone::Graveyard,
        "前置:观测里该实体确实进坟(日志行 {})",
        located.line_start
    );

    let before_sim = SimGameState::from_observed(&before_obs);
    let victim = before_sim.entity(dead_id).expect("死亡实体应在沙盘里");
    assert_eq!(victim.zone, Zone::Play, "死亡前应在场");
    let lethal = victim.current_health();
    assert!(lethal > 0, "死亡前生命应为正");

    let outcome = apply_bound(
        &before_sim,
        vec![Operation::Damage {
            target: hs_effect_ir::Selector::Target,
            amount: lethal,
        }],
        hs_sim::TargetBinding {
            source: None,
            target: Some(dead_id),
        },
    );
    let sim_after = expect_det(&outcome);

    assert_eq!(
        sim_after.entity(dead_id).unwrap().zone,
        Zone::Graveyard,
        "模拟的死亡结算应把随从移入坟场(日志行 {})",
        located.line_start
    );
    assert_eq!(
        sim_after.entity(dead_id).unwrap().zone,
        SimGameState::from_observed(&after_obs)
            .entity(dead_id)
            .unwrap()
            .zone,
        "模拟 = 观测,零差异(日志行 {})",
        located.line_start
    );
    assert!(
        outcome.trace.entries.iter().any(|e| e.op == "death"),
        "RuleTrace 应记录死亡结算"
    );
}
