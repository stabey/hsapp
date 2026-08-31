//! `hs-observed-state` — 由日志确定性归约的权威观察状态（三态之一，ADR-007）。
//!
//! 硬约束：
//! - 只包含日志明确提供或确定性归约可得的信息；
//! - `reduce` 是纯函数：同一事件序列 + 同一 Reducer 版本 = 同一状态（§7.6）；
//! - BeliefState / SimGameState 绝不写回本状态；
//! - 本 crate 禁止依赖 GPUI 与 SQLite。

mod hash;
mod state;

pub use hash::state_hash;
pub use hs_log_core::{OptionAvailability, OptionSummary, OptionTargetSummary};
pub use state::{
    BlockFrame, ChoicesState, EntityState, ObservedGameState, OptionsState, PlayerState,
    SentChoiceEcho, MAX_STORED_WARNINGS,
};

use hs_log_core::{game_tag, DomainEvent, TagKey};
use hs_types::{BuildId, EntityId, GameId, GamePhase, PlayerId, Zone};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("event not yet supported by reducer: {0}")]
    Unsupported(&'static str),
    #[error("inconsistent state: {0}")]
    Inconsistent(String),
}

/// 纯函数归约契约（设计文档 §7.5）。开发期 Clone 快照，CoW 优化后议。
pub fn reduce(
    previous: &ObservedGameState,
    event: &DomainEvent,
) -> Result<ObservedGameState, StateError> {
    let mut next = previous.clone();
    next.revision = previous.revision.saturating_add(1);
    match event {
        DomainEvent::GameCreated { game_id, build_id } => {
            apply_game_created(&mut next, *game_id, *build_id);
        }
        DomainEvent::EntityCreated {
            entity_id,
            card_id,
            zone,
        } => {
            next.entities.insert(
                *entity_id,
                EntityState::new(*entity_id, card_id.clone(), *zone),
            );
        }
        DomainEvent::TagChanged {
            entity_id,
            tag,
            value,
        } => {
            apply_tag_changed(&mut next, *entity_id, *tag, *value);
        }
        DomainEvent::EntityRevealed { entity_id, card_id } => {
            let entity = entity_mut(&mut next, *entity_id);
            entity.card_id = Some(card_id.clone());
            entity.revealed = true;
        }
        DomainEvent::EntityChanged { entity_id, card_id } => {
            // 变形只改 card_id；后续 TagChanged 覆盖 tags。有卡名则视为已揭示。
            let entity = entity_mut(&mut next, *entity_id);
            entity.card_id = card_id.clone();
            if card_id.is_some() {
                entity.revealed = true;
            }
        }
        DomainEvent::ZoneChanged { entity_id, to, .. } => {
            // M1 Normalizer 不发射本变体；若收到则仍更新 zone，与既有行为一致。
            let entity = entity_mut(&mut next, *entity_id);
            entity.zone = *to;
            entity
                .tags
                .insert(game_tag::ZONE, game_tag::zone_to_value(*to));
        }
        DomainEvent::BlockOpened {
            block_type,
            source,
            target,
            effect_card_id,
        } => {
            next.block_stack.push(BlockFrame {
                block_type: block_type.clone(),
                source: *source,
                target: *target,
                effect_card_id: effect_card_id.clone(),
            });
        }
        DomainEvent::BlockClosed => {
            if next.block_stack.pop().is_none() {
                push_warning(&mut next, "BlockClosed on empty block_stack");
            }
        }
        DomainEvent::OptionsUpdated { id, options } => {
            next.options = Some(OptionsState {
                id: *id,
                issued_revision: next.revision,
                options: options.clone(),
            });
        }
        DomainEvent::ChoicesUpdated {
            id,
            task_list,
            player,
            player_entity,
            player_raw,
            choice_type,
            min,
            max,
            source,
            source_raw,
            entities,
        } => {
            next.choices.push(ChoicesState {
                id: *id,
                task_list: *task_list,
                player: *player,
                player_entity: *player_entity,
                player_raw: player_raw.clone(),
                source: *source,
                source_raw: source_raw.clone(),
                issued_revision: next.revision,
                choice_type: choice_type.clone(),
                min: *min,
                max: *max,
                entities: entities.clone(),
            });
        }
        DomainEvent::ChoicesResolved {
            id,
            task_list,
            player_entity,
            player_raw,
            source,
            source_raw,
            entities,
        } => {
            consume_matching_choice(
                &mut next,
                ChoiceEvidence {
                    kind: ChoiceEvidenceKind::Resolved,
                    event_name: "ChoicesResolved",
                    id: *id,
                    task_list: *task_list,
                    player_entity: *player_entity,
                    player_raw,
                    source: *source,
                    source_raw,
                    choice_type: "",
                    entities,
                },
            );
        }
        DomainEvent::ChoicesSent {
            id,
            task_list,
            player_entity,
            player_raw,
            source,
            source_raw,
            choice_type,
            entities,
        } => {
            consume_matching_choice(
                &mut next,
                ChoiceEvidence {
                    kind: ChoiceEvidenceKind::Sent,
                    event_name: "ChoicesSent",
                    id: *id,
                    task_list: *task_list,
                    player_entity: *player_entity,
                    player_raw,
                    source: *source,
                    source_raw,
                    choice_type,
                    entities,
                },
            );
        }
        // SEND_OPTION 是已消费信号；有些真实客户端用 id=0，仍必须清掉旧快照。
        DomainEvent::OptionChosen { .. } => {
            next.options = None;
        }
        DomainEvent::GameEnded { winner } => {
            next.phase = GamePhase::Ended;
            next.winner = *winner;
            next.options = None;
            next.choices.clear();
            next.sent_choice_echoes.clear();
        }
        DomainEvent::FriendlyPlayerDetected { player } => {
            next.friendly_player = Some(*player);
        }
        DomainEvent::BuildIdentified { build_id } => {
            next.build_id = Some(*build_id);
        }
        DomainEvent::ProtocolWarning { message } => {
            push_warning(&mut next, message.clone());
        }
        _ => {
            push_warning(&mut next, "unhandled event");
        }
    }
    Ok(next)
}

/// 新对局：对局相关字段全部重置（含 warnings / warning_count）；`revision` 由调用方递增，不回零。
fn apply_game_created(state: &mut ObservedGameState, game_id: GameId, build_id: Option<BuildId>) {
    state.game_id = Some(game_id);
    state.build_id = build_id;
    state.phase = GamePhase::NotStarted;
    state.turn = 0;
    state.current_player = None;
    state.friendly_player = None;
    state.winner = None;
    state.entities.clear();
    state.players.clear();
    state.block_stack.clear();
    state.options = None;
    state.choices.clear();
    state.sent_choice_echoes.clear();
    state.public_counters.clear();
    state.warnings.clear();
    state.warning_count = 0;
}

/// Choices 回声/确认携带的可用身份线索。空值代表该事件没有提供证据，不能据此排除候选。
#[derive(Clone, Copy, Eq, PartialEq)]
enum ChoiceEvidenceKind {
    Sent,
    Resolved,
}

struct ChoiceEvidence<'a> {
    kind: ChoiceEvidenceKind,
    event_name: &'static str,
    id: u64,
    task_list: Option<u64>,
    player_entity: Option<EntityId>,
    player_raw: &'a str,
    source: Option<EntityId>,
    source_raw: &'a str,
    choice_type: &'a str,
    entities: &'a [EntityId],
}

fn consume_matching_choice(state: &mut ObservedGameState, evidence: ChoiceEvidence<'_>) {
    if evidence.kind == ChoiceEvidenceKind::Resolved && consume_matching_sent_echo(state, &evidence)
    {
        return;
    }

    let same_id: Vec<usize> = state
        .choices
        .iter()
        .enumerate()
        .filter_map(|(index, pending)| (pending.id == evidence.id).then_some(index))
        .collect();

    // SEND_CHOICES 后通常还会出现 CHOSEN_ENTITIES；前者已消费时，后者应静默。
    if same_id.is_empty() {
        return;
    }

    let strong_matching: Vec<usize> = same_id
        .iter()
        .copied()
        .filter(|&index| strong_choice_fields_match(&state.choices[index], &evidence))
        .collect();
    let matching =
        narrow_multiple_choices_by_raw_alias(&state.choices, &strong_matching, &evidence);

    match matching.as_slice() {
        [index] => {
            let consumed = state.choices.remove(*index);
            if evidence.kind == ChoiceEvidenceKind::Sent {
                state.sent_choice_echoes.push(SentChoiceEcho {
                    choice: consumed,
                    sent_revision: state.revision,
                });
            }
        }
        [] => {
            let identities = choice_identities(&state.choices, &same_id);
            push_warning(
                state,
                format!(
                    "{} did not uniquely match id={}: candidates=[{}]",
                    evidence.event_name, evidence.id, identities
                ),
            );
        }
        _ => {
            let identities = choice_identities(&state.choices, &matching);
            push_warning(
                state,
                format!(
                    "{} ambiguous id={}: candidates=[{}]",
                    evidence.event_name, evidence.id, identities
                ),
            );
        }
    }
}

/// `ChoicesSent` 后通常会有一条信息更少的 `ChoicesResolved`。先消费这里的
/// tombstone，避免它把同 id 的另一个 pending Choice 当成唯一候选清掉。
fn consume_matching_sent_echo(
    state: &mut ObservedGameState,
    evidence: &ChoiceEvidence<'_>,
) -> bool {
    let same_id: Vec<usize> = state
        .sent_choice_echoes
        .iter()
        .enumerate()
        .filter_map(|(index, sent)| (sent.choice.id == evidence.id).then_some(index))
        .collect();
    if same_id.is_empty() {
        return false;
    }

    let strong_matching: Vec<usize> = same_id
        .iter()
        .copied()
        .filter(|&index| {
            strong_choice_fields_match(&state.sent_choice_echoes[index].choice, evidence)
        })
        .collect();
    let matching = narrow_multiple_sent_echoes_by_raw_alias(
        &state.sent_choice_echoes,
        &strong_matching,
        evidence,
    );

    match matching.as_slice() {
        [index] => {
            let sent_revision = state.sent_choice_echoes[*index].sent_revision;
            let newer_pending: Vec<usize> = state
                .choices
                .iter()
                .enumerate()
                .filter_map(|(pending_index, pending)| {
                    (pending.id == evidence.id
                        && pending.issued_revision > sent_revision
                        && strong_choice_fields_match(pending, evidence))
                    .then_some(pending_index)
                })
                .collect();
            if !newer_pending.is_empty() {
                let pending_identities = choice_identities(&state.choices, &newer_pending);
                push_warning(
                    state,
                    format!(
                        "{} ambiguous id={} between sent echo revision={} and newer pending=[{}]",
                        evidence.event_name, evidence.id, sent_revision, pending_identities
                    ),
                );
                return true;
            }
            state.sent_choice_echoes.remove(*index);
            true
        }
        [] => false,
        _ => {
            let identities = sent_echo_identities(&state.sent_choice_echoes, &matching);
            push_warning(
                state,
                format!(
                    "{} ambiguous sent echo id={}: candidates=[{}]",
                    evidence.event_name, evidence.id, identities
                ),
            );
            true
        }
    }
}

fn narrow_multiple_sent_echoes_by_raw_alias(
    echoes: &[SentChoiceEcho],
    candidates: &[usize],
    evidence: &ChoiceEvidence<'_>,
) -> Vec<usize> {
    if candidates.len() <= 1 {
        return candidates.to_vec();
    }

    let scored: Vec<(usize, u8)> = candidates
        .iter()
        .copied()
        .map(|index| {
            let sent = &echoes[index].choice;
            let score =
                u8::from(!evidence.player_raw.is_empty() && sent.player_raw == evidence.player_raw)
                    + u8::from(
                        !evidence.source_raw.is_empty() && sent.source_raw == evidence.source_raw,
                    );
            (index, score)
        })
        .collect();
    let best_score = scored.iter().map(|(_, score)| *score).max().unwrap_or(0);
    if best_score == 0 {
        return candidates.to_vec();
    }

    scored
        .into_iter()
        .filter_map(|(index, score)| (score == best_score).then_some(index))
        .collect()
}

fn strong_choice_fields_match(pending: &ChoicesState, evidence: &ChoiceEvidence<'_>) -> bool {
    optional_field_matches(pending.task_list, evidence.task_list)
        && optional_field_matches(pending.player_entity, evidence.player_entity)
        && optional_field_matches(pending.source, evidence.source)
        && text_field_matches(&pending.choice_type, evidence.choice_type)
        && evidence
            .entities
            .iter()
            .all(|chosen| pending.entities.contains(chosen))
}

/// raw 名称会从占位符变成战网昵称，不能作为否定证据。仅在强字段仍有多个候选时，
/// 用相等的 raw 值做正向缩窄；没有任何相等项时保留原候选，绝不缩成 0。
fn narrow_multiple_choices_by_raw_alias(
    choices: &[ChoicesState],
    candidates: &[usize],
    evidence: &ChoiceEvidence<'_>,
) -> Vec<usize> {
    if candidates.len() <= 1 {
        return candidates.to_vec();
    }

    let scored: Vec<(usize, u8)> = candidates
        .iter()
        .copied()
        .map(|index| {
            let pending = &choices[index];
            let score = u8::from(
                !evidence.player_raw.is_empty() && pending.player_raw == evidence.player_raw,
            ) + u8::from(
                !evidence.source_raw.is_empty() && pending.source_raw == evidence.source_raw,
            );
            (index, score)
        })
        .collect();
    let best_score = scored.iter().map(|(_, score)| *score).max().unwrap_or(0);
    if best_score == 0 {
        return candidates.to_vec();
    }

    scored
        .into_iter()
        .filter_map(|(index, score)| (score == best_score).then_some(index))
        .collect()
}

fn optional_field_matches<T: Copy + Eq>(pending: Option<T>, evidence: Option<T>) -> bool {
    evidence.is_none() || pending.is_none() || pending == evidence
}

fn text_field_matches(pending: &str, evidence: &str) -> bool {
    evidence.is_empty() || pending.is_empty() || pending == evidence
}

fn choice_identities(choices: &[ChoicesState], indices: &[usize]) -> String {
    indices
        .iter()
        .map(|&index| {
            let pending = &choices[index];
            format!(
                "id={} task_list={:?} player={:?} player_entity={:?} player_raw={:?} source={:?} source_raw={:?} issued_revision={} type={:?}",
                pending.id,
                pending.task_list,
                pending.player,
                pending.player_entity,
                pending.player_raw,
                pending.source,
                pending.source_raw,
                pending.issued_revision,
                pending.choice_type,
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn sent_echo_identities(echoes: &[SentChoiceEcho], indices: &[usize]) -> String {
    indices
        .iter()
        .map(|&index| {
            let echo = &echoes[index];
            format!(
                "sent_revision={} choice=({})",
                echo.sent_revision,
                choice_identities(std::slice::from_ref(&echo.choice), &[0])
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn apply_tag_changed(state: &mut ObservedGameState, entity_id: EntityId, tag: TagKey, value: i32) {
    let player_hint = {
        let entity = entity_mut(state, entity_id);
        entity.tags.insert(tag.0, value);
        match tag.0 {
            game_tag::ZONE => {
                entity.zone = game_tag::zone_from_value(value);
            }
            game_tag::CONTROLLER => {
                entity.controller = Some(PlayerId(value));
            }
            game_tag::ZONE_POSITION => {
                entity.zone_position = u8::try_from(value).ok();
            }
            _ => {}
        }
        entity
            .tags
            .get(&game_tag::PLAYER_ID)
            .copied()
            .map(PlayerId)
            .or(entity.controller)
    };

    match tag.0 {
        game_tag::PLAYER_ID => {
            upsert_player(state, value, entity_id);
        }
        game_tag::PLAYSTATE => {
            if let Some(pid) = player_hint {
                upsert_player(state, pid.0, entity_id).playstate = Some(value);
            }
            if matches!(
                value,
                game_tag::play_state::WON | game_tag::play_state::LOST | game_tag::play_state::TIED
            ) {
                state.phase = GamePhase::Ended;
            }
            if value == game_tag::play_state::WON {
                state.winner = player_hint;
            }
        }
        game_tag::RESOURCES => {
            if let Some(pid) = player_hint {
                upsert_player(state, pid.0, entity_id).resources = Some(value);
            }
        }
        game_tag::RESOURCES_USED => {
            if let Some(pid) = player_hint {
                upsert_player(state, pid.0, entity_id).resources_used = Some(value);
            }
        }
        game_tag::HERO_ENTITY => {
            if let Some(pid) = player_hint {
                upsert_player(state, pid.0, entity_id).hero_entity = Some(EntityId(value));
            }
        }
        game_tag::TURN => {
            // 日志中 TURN 打在 GameEntity 上；任意实体携带该 tag 也同步到 state.turn。
            state.turn = u32::try_from(value).unwrap_or(0);
        }
        game_tag::STEP => {
            if let Some(phase) = phase_from_step(value) {
                state.phase = phase;
            }
        }
        game_tag::CURRENT_PLAYER => {
            if value == 1 {
                if player_hint.is_some() {
                    state.current_player = player_hint;
                }
            } else if value == 0 && player_hint.is_some() && state.current_player == player_hint {
                state.current_player = None;
            }
        }
        _ => {}
    }
}

fn upsert_player(
    state: &mut ObservedGameState,
    player_id: i32,
    entity_id: EntityId,
) -> &mut PlayerState {
    state
        .players
        .entry(player_id)
        .and_modify(|player| {
            player.player_id = PlayerId(player_id);
            player.entity_id = entity_id;
        })
        .or_insert_with(|| PlayerState {
            player_id: PlayerId(player_id),
            entity_id,
            playstate: None,
            resources: None,
            resources_used: None,
            hero_entity: None,
        })
}

fn entity_mut(state: &mut ObservedGameState, entity_id: EntityId) -> &mut EntityState {
    state
        .entities
        .entry(entity_id)
        .or_insert_with(|| EntityState::new(entity_id, None, Zone::Invalid))
}

fn push_warning(state: &mut ObservedGameState, message: impl Into<String>) {
    state.warning_count = state.warning_count.saturating_add(1);
    if state.warnings.len() < MAX_STORED_WARNINGS {
        state.warnings.push(message.into());
    }
}

fn phase_from_step(step: i32) -> Option<GamePhase> {
    match step {
        game_tag::step::BEGIN_MULLIGAN => Some(GamePhase::Mulligan),
        game_tag::step::FINAL_WRAPUP | game_tag::step::FINAL_GAMEOVER => Some(GamePhase::Ended),
        game_tag::step::MAIN_BEGIN
        | game_tag::step::MAIN_READY
        | game_tag::step::MAIN_RESOURCE
        | game_tag::step::MAIN_DRAW
        | game_tag::step::MAIN_START
        | game_tag::step::MAIN_ACTION
        | game_tag::step::MAIN_COMBAT
        | game_tag::step::MAIN_END
        | game_tag::step::MAIN_NEXT
        | game_tag::step::MAIN_CLEANUP
        | game_tag::step::MAIN_START_TRIGGERS => Some(GamePhase::Playing),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_log_core::{EventNormalizer, PowerEventNormalizer, PowerLogParser};
    use hs_log_tailer::{ParseStatus, RawLogRecord};
    use hs_types::{CardId, LogFileId, LogSessionId};
    use std::sync::Arc;

    fn reduce_all(events: &[DomainEvent]) -> ObservedGameState {
        events
            .iter()
            .fold(ObservedGameState::default(), |state, event| {
                reduce(&state, event).expect("reduce should not fail")
            })
    }

    fn created(entity_id: i32, card_id: Option<&str>, zone: Zone) -> DomainEvent {
        DomainEvent::EntityCreated {
            entity_id: EntityId(entity_id),
            card_id: card_id.map(CardId::new),
            zone,
        }
    }

    fn tag(entity_id: i32, tag: TagKey, value: i32) -> DomainEvent {
        DomainEvent::TagChanged {
            entity_id: EntityId(entity_id),
            tag,
            value,
        }
    }

    fn option_summary(index: u32, entity: i32) -> OptionSummary {
        // OptionSummary 在 hs-log-core 标记 non_exhaustive，跨 crate 只能先 Default 再填字段。
        let mut summary = OptionSummary::default();
        summary.index = index;
        summary.option_type = "PLAY".into();
        summary.entity = Some(EntityId(entity));
        summary
    }

    fn choices_updated(
        id: u64,
        task_list: u64,
        player_entity: Option<i32>,
        source: Option<i32>,
        choice_type: &str,
        entities: &[i32],
    ) -> DomainEvent {
        DomainEvent::ChoicesUpdated {
            id,
            task_list: Some(task_list),
            player: None,
            player_entity: player_entity.map(EntityId),
            player_raw: String::new(),
            choice_type: choice_type.into(),
            min: 0,
            max: entities.len() as u32,
            source: source.map(EntityId),
            source_raw: String::new(),
            entities: entities.iter().copied().map(EntityId).collect(),
        }
    }

    fn choices_sent(
        id: u64,
        task_list: Option<u64>,
        player_entity: Option<i32>,
        source: Option<i32>,
        choice_type: &str,
        entities: &[i32],
    ) -> DomainEvent {
        DomainEvent::ChoicesSent {
            id,
            task_list,
            player_entity: player_entity.map(EntityId),
            player_raw: String::new(),
            source: source.map(EntityId),
            source_raw: String::new(),
            choice_type: choice_type.into(),
            entities: entities.iter().copied().map(EntityId).collect(),
        }
    }

    fn choices_resolved(
        id: u64,
        task_list: Option<u64>,
        player_entity: Option<i32>,
        source: Option<i32>,
        entities: &[i32],
    ) -> DomainEvent {
        DomainEvent::ChoicesResolved {
            id,
            task_list,
            player_entity: player_entity.map(EntityId),
            player_raw: String::new(),
            source: source.map(EntityId),
            source_raw: String::new(),
            entities: entities.iter().copied().map(EntityId).collect(),
        }
    }

    fn mulligan_updated_with_player_raw(player_raw: &str) -> DomainEvent {
        DomainEvent::ChoicesUpdated {
            id: 1,
            task_list: None,
            player: None,
            player_entity: None,
            player_raw: player_raw.into(),
            choice_type: "MULLIGAN".into(),
            min: 0,
            max: 3,
            source: None,
            source_raw: String::new(),
            entities: Vec::new(),
        }
    }

    fn mulligan_resolved_with_player_raw(player_raw: &str) -> DomainEvent {
        DomainEvent::ChoicesResolved {
            id: 1,
            task_list: None,
            player_entity: None,
            player_raw: player_raw.into(),
            source: None,
            source_raw: String::new(),
            entities: Vec::new(),
        }
    }

    fn all_event_types_sequence() -> Vec<DomainEvent> {
        vec![
            DomainEvent::ProtocolWarning {
                message: "pre-game noise".into(),
            },
            DomainEvent::GameCreated {
                game_id: GameId(11),
                build_id: Some(BuildId(222)),
            },
            created(1, None, Zone::Play),
            tag(1, TagKey::TURN, 1),
            tag(1, TagKey::STEP, game_tag::step::BEGIN_MULLIGAN),
            created(2, None, Zone::Play),
            tag(2, TagKey::PLAYER_ID, 1),
            tag(2, TagKey::CONTROLLER, 1),
            tag(2, TagKey::PLAYSTATE, game_tag::play_state::PLAYING),
            tag(2, TagKey(game_tag::RESOURCES), 1),
            tag(2, TagKey(game_tag::RESOURCES_USED), 0),
            tag(2, TagKey(game_tag::HERO_ENTITY), 4),
            tag(2, TagKey::CURRENT_PLAYER, 1),
            created(3, None, Zone::Play),
            tag(3, TagKey::PLAYER_ID, 2),
            tag(3, TagKey::CONTROLLER, 2),
            tag(3, TagKey::PLAYSTATE, game_tag::play_state::PLAYING),
            created(4, Some("HERO_01"), Zone::Play),
            tag(4, TagKey::HEALTH, 30),
            tag(4, TagKey::DAMAGE, 5),
            tag(4, TagKey::ATK, 0),
            tag(4, TagKey::COST, 0),
            tag(4, TagKey::CONTROLLER, 1),
            tag(4, TagKey::ZONE_POSITION, 0),
            tag(4, TagKey(game_tag::SILENCED), 0),
            DomainEvent::EntityRevealed {
                entity_id: EntityId(64),
                card_id: CardId::new("EX1_116"),
            },
            created(7, Some("EX1_246"), Zone::Play),
            DomainEvent::EntityChanged {
                entity_id: EntityId(7),
                card_id: Some(CardId::new("NEW1_007")),
            },
            DomainEvent::BlockOpened {
                block_type: "POWER".into(),
                source: Some(EntityId(4)),
                target: Some(EntityId(7)),
                effect_card_id: Some(CardId::new("HERO_01")),
            },
            DomainEvent::BlockOpened {
                block_type: "TRIGGER".into(),
                source: Some(EntityId(1)),
                target: None,
                effect_card_id: None,
            },
            DomainEvent::BlockClosed,
            DomainEvent::BlockClosed,
            DomainEvent::BlockClosed,
            DomainEvent::OptionsUpdated {
                id: 3,
                options: vec![option_summary(0, 4), option_summary(1, 7)],
            },
            DomainEvent::OptionsUpdated {
                id: 4,
                options: vec![option_summary(0, 4)],
            },
            DomainEvent::OptionChosen {
                id: 4,
                option: 0,
                sub_option: -1,
                target: 0,
                position: 0,
            },
            DomainEvent::ChoicesUpdated {
                id: 8,
                task_list: Some(80),
                player: Some(PlayerId(1)),
                player_entity: Some(EntityId(2)),
                player_raw: "PlayerOne".into(),
                choice_type: "GENERAL".into(),
                min: 1,
                max: 1,
                source: Some(EntityId(4)),
                source_raw: "4".into(),
                entities: vec![EntityId(7), EntityId(64)],
            },
            DomainEvent::ChoicesSent {
                id: 8,
                task_list: None,
                player_entity: None,
                player_raw: String::new(),
                source: None,
                source_raw: String::new(),
                choice_type: "GENERAL".into(),
                entities: vec![EntityId(7)],
            },
            DomainEvent::ChoicesResolved {
                id: 8,
                task_list: None,
                player_entity: None,
                player_raw: "PlayerOne".into(),
                source: None,
                source_raw: String::new(),
                entities: vec![EntityId(7)],
            },
            DomainEvent::ChoicesUpdated {
                id: 9,
                task_list: Some(90),
                player: Some(PlayerId(2)),
                player_entity: Some(EntityId(3)),
                player_raw: "PlayerTwo".into(),
                choice_type: "MULLIGAN".into(),
                min: 0,
                max: 3,
                source: None,
                source_raw: String::new(),
                entities: vec![EntityId(10)],
            },
            DomainEvent::ChoicesResolved {
                id: 99,
                task_list: None,
                player_entity: None,
                player_raw: String::new(),
                source: None,
                source_raw: String::new(),
                entities: vec![],
            },
            DomainEvent::ZoneChanged {
                entity_id: EntityId(7),
                from: Zone::Play,
                to: Zone::Graveyard,
            },
            tag(2, TagKey::CURRENT_PLAYER, 0),
            tag(3, TagKey::CURRENT_PLAYER, 1),
            tag(1, TagKey::STEP, game_tag::step::MAIN_ACTION),
            tag(1, TagKey::TURN, 2),
            tag(2, TagKey(game_tag::RESOURCES), 2),
            tag(2, TagKey::PLAYSTATE, game_tag::play_state::WON),
            DomainEvent::GameEnded {
                winner: Some(PlayerId(1)),
            },
            DomainEvent::ProtocolWarning {
                message: "post-game noise".into(),
            },
        ]
    }

    #[test]
    fn game_created_initializes_and_clears_entities() {
        let seeded = reduce_all(&[
            created(9, Some("CS2_029"), Zone::Hand),
            DomainEvent::ProtocolWarning {
                message: "stale".into(),
            },
            DomainEvent::BlockOpened {
                block_type: "TRIGGER".into(),
                source: None,
                target: None,
                effect_card_id: None,
            },
            DomainEvent::OptionsUpdated {
                id: 1,
                options: Vec::new(),
            },
            choices_updated(1, 6, Some(2), Some(1), "MULLIGAN", &[10, 11]),
        ]);
        assert_eq!(seeded.entities.len(), 1);
        assert_eq!(seeded.warning_count, 1);
        assert_eq!(seeded.block_stack.len(), 1);
        assert!(seeded.options.is_some());
        assert_eq!(seeded.choices.len(), 1);

        let state = reduce(
            &seeded,
            &DomainEvent::GameCreated {
                game_id: GameId(7),
                build_id: Some(BuildId(123)),
            },
        )
        .unwrap();
        assert_eq!(state.game_id, Some(GameId(7)));
        assert_eq!(state.build_id, Some(BuildId(123)));
        assert_eq!(state.phase, GamePhase::NotStarted);
        assert!(state.entities.is_empty());
        assert!(state.players.is_empty());
        assert!(state.block_stack.is_empty());
        assert!(state.options.is_none());
        assert!(state.choices.is_empty());
        assert!(state.public_counters.is_empty());
        assert!(state.warnings.is_empty());
        assert_eq!(state.warning_count, 0);
        assert_eq!(state.winner, None);
        assert_eq!(state.revision, seeded.revision + 1);
    }

    #[test]
    fn entity_created_and_tag_changed_update_store() {
        let state = reduce_all(&[
            created(4, Some("HERO_01"), Zone::Play),
            tag(4, TagKey::HEALTH, 30),
            tag(4, TagKey::DAMAGE, 4),
            tag(4, TagKey::ATK, 2),
            tag(4, TagKey::COST, 0),
            tag(4, TagKey(game_tag::SILENCED), 1),
            tag(4, TagKey::CONTROLLER, 1),
            tag(4, TagKey::ZONE_POSITION, 0),
            tag(1, TagKey::TURN, 2),
            tag(1, TagKey::STEP, game_tag::step::BEGIN_MULLIGAN),
        ]);

        let hero = state.entities.get(&EntityId(4)).unwrap();
        assert_eq!(hero.card_id.as_ref().unwrap().as_str(), "HERO_01");
        assert!(hero.revealed);
        assert_eq!(hero.zone, Zone::Play);
        assert_eq!(hero.controller, Some(PlayerId(1)));
        assert_eq!(hero.zone_position, Some(0));
        assert_eq!(hero.tags.get(&game_tag::HEALTH), Some(&30));
        assert_eq!(hero.attack(), 2);
        assert_eq!(hero.health(), 30);
        assert_eq!(hero.damage(), 4);
        assert_eq!(hero.current_health(), 26);
        assert_eq!(hero.cost(), 0);
        assert!(hero.is_silenced());
        assert_eq!(state.turn, 2);
        assert_eq!(state.phase, GamePhase::Mulligan);
        assert_eq!(state.revision, 10);
    }

    #[test]
    fn player_tags_register_players_and_current_player() {
        let state = reduce_all(&[
            created(2, None, Zone::Play),
            tag(2, TagKey::PLAYER_ID, 1),
            tag(2, TagKey::PLAYSTATE, game_tag::play_state::PLAYING),
            tag(2, TagKey(game_tag::RESOURCES), 3),
            tag(2, TagKey(game_tag::RESOURCES_USED), 1),
            tag(2, TagKey::CURRENT_PLAYER, 1),
            created(3, None, Zone::Play),
            tag(3, TagKey::PLAYER_ID, 2),
            tag(3, TagKey::CURRENT_PLAYER, 1),
        ]);

        let p1 = state.player(PlayerId(1)).unwrap();
        assert_eq!(p1.entity_id, EntityId(2));
        assert_eq!(p1.playstate, Some(game_tag::play_state::PLAYING));
        assert_eq!(p1.resources, Some(3));
        assert_eq!(p1.resources_used, Some(1));
        assert_eq!(state.current_player, Some(PlayerId(2)));

        let cleared = reduce(&state, &tag(3, TagKey::CURRENT_PLAYER, 0)).unwrap();
        assert_eq!(cleared.current_player, None);
    }

    #[test]
    fn protocol_warnings_accumulate_and_cap() {
        let state = reduce_all(&[
            DomainEvent::ProtocolWarning {
                message: "raw line".into(),
            },
            DomainEvent::BlockClosed,
            DomainEvent::OptionsUpdated {
                id: 3,
                options: Vec::new(),
            },
        ]);
        assert_eq!(
            state.warnings,
            vec![
                "raw line".to_string(),
                "BlockClosed on empty block_stack".to_string(),
            ]
        );
        assert_eq!(state.warning_count, 2);
        assert_eq!(state.options.as_ref().map(|o| o.id), Some(3));
        assert_eq!(state.revision, 3);
        assert!(state.entities.is_empty());

        let mut capped = ObservedGameState::default();
        for i in 0..(MAX_STORED_WARNINGS + 5) {
            capped = reduce(
                &capped,
                &DomainEvent::ProtocolWarning {
                    message: format!("w{i}"),
                },
            )
            .unwrap();
        }
        assert_eq!(capped.warnings.len(), MAX_STORED_WARNINGS);
        assert_eq!(capped.warning_count, (MAX_STORED_WARNINGS + 5) as u64);
        assert_eq!(capped.warnings[0], "w0");
        assert_eq!(
            capped.warnings[MAX_STORED_WARNINGS - 1],
            format!("w{}", MAX_STORED_WARNINGS - 1)
        );
    }

    #[test]
    fn options_keep_issued_revision_and_any_send_option_consumes_them() {
        let before = reduce(
            &ObservedGameState::default(),
            &DomainEvent::ProtocolWarning {
                message: "before options".into(),
            },
        )
        .unwrap();
        let issued = reduce(
            &before,
            &DomainEvent::OptionsUpdated {
                id: 17,
                options: vec![option_summary(1, 42)],
            },
        )
        .unwrap();
        assert_eq!(issued.revision, 2);
        assert_eq!(issued.options.as_ref().unwrap().issued_revision, 2);

        let later = reduce(
            &issued,
            &DomainEvent::ProtocolWarning {
                message: "after options".into(),
            },
        )
        .unwrap();
        assert_eq!(later.revision, 3);
        assert_eq!(later.options.as_ref().unwrap().issued_revision, 2);

        let consumed = reduce(
            &later,
            &DomainEvent::OptionChosen {
                id: 0,
                option: 0,
                sub_option: -1,
                target: 0,
                position: 0,
            },
        )
        .unwrap();
        assert!(consumed.options.is_none());
    }

    #[test]
    fn game_ended_consumes_all_pending_decisions() {
        let pending = reduce_all(&[
            DomainEvent::OptionsUpdated {
                id: 1,
                options: vec![option_summary(1, 42)],
            },
            choices_updated(1, 6, Some(2), Some(1), "MULLIGAN", &[10]),
            choices_updated(2, 7, Some(3), Some(1), "MULLIGAN", &[20]),
        ]);
        assert!(pending.options.is_some());
        assert_eq!(pending.choices.len(), 2);

        let ended = reduce(&pending, &DomainEvent::GameEnded { winner: None }).unwrap();
        assert!(ended.options.is_none());
        assert!(ended.choices.is_empty());
    }

    #[test]
    fn choices_append_and_keep_their_issued_revisions() {
        let first = reduce(
            &ObservedGameState::default(),
            &choices_updated(1, 6, Some(2), Some(1), "MULLIGAN", &[10, 11]),
        )
        .unwrap();
        let second = reduce(
            &first,
            &choices_updated(2, 7, Some(3), Some(1), "MULLIGAN", &[20, 21, 22]),
        )
        .unwrap();

        assert_eq!(second.choices.len(), 2);
        assert_eq!(second.choices[0].issued_revision, 1);
        assert_eq!(second.choices[1].issued_revision, 2);
        assert_eq!(second.choices[0].task_list, Some(6));
        assert_eq!(second.choices[1].task_list, Some(7));
    }

    #[test]
    fn changed_player_alias_does_not_block_a_unique_choice_resolution() {
        let pending = reduce(
            &ObservedGameState::default(),
            &mulligan_updated_with_player_raw("UNKNOWN HUMAN PLAYER"),
        )
        .unwrap();
        let warning_count = pending.warning_count;

        let resolved =
            reduce(&pending, &mulligan_resolved_with_player_raw("PlayerA#0001")).unwrap();
        assert!(resolved.choices.is_empty());
        assert_eq!(resolved.warning_count, warning_count);
    }

    #[test]
    fn raw_alias_only_narrows_multiple_candidates_when_it_matches() {
        let pending = reduce_all(&[
            mulligan_updated_with_player_raw("UNKNOWN HUMAN PLAYER"),
            mulligan_updated_with_player_raw("PlayerA#0001"),
        ]);

        let unknown_alias = reduce(
            &pending,
            &mulligan_resolved_with_player_raw("A THIRD ALIAS"),
        )
        .unwrap();
        assert_eq!(unknown_alias.choices.len(), 2);
        assert_eq!(unknown_alias.warning_count, pending.warning_count + 1);

        let positive_match = reduce(
            &unknown_alias,
            &mulligan_resolved_with_player_raw("PlayerA#0001"),
        )
        .unwrap();
        assert_eq!(positive_match.choices.len(), 1);
        assert_eq!(positive_match.choices[0].player_raw, "UNKNOWN HUMAN PLAYER");
    }

    #[test]
    fn same_choice_id_is_only_consumed_when_evidence_is_unique() {
        let pending = reduce_all(&[
            choices_updated(5, 827, Some(2), Some(40), "GENERAL", &[101, 102]),
            choices_updated(5, 859, Some(2), Some(35), "GENERAL", &[201, 202]),
        ]);
        let warning_count = pending.warning_count;

        let ambiguous = reduce(&pending, &choices_sent(5, None, None, None, "GENERAL", &[]))
            .expect("ambiguous send should not fail");
        assert_eq!(ambiguous.choices.len(), 2);
        assert_eq!(ambiguous.warning_count, warning_count + 1);
        let warning = ambiguous.warnings.last().unwrap();
        assert!(warning.contains("ambiguous id=5"));
        assert!(warning.contains("task_list=Some(827)"));
        assert!(warning.contains("task_list=Some(859)"));

        let unique = reduce(
            &ambiguous,
            &choices_resolved(5, Some(827), None, None, &[101]),
        )
        .expect("task list and chosen entity identify one pending choice");
        assert_eq!(unique.choices.len(), 1);
        assert_eq!(unique.choices[0].task_list, Some(859));
    }

    #[test]
    fn sent_then_weak_resolved_echo_preserves_other_same_id_choice() {
        let pending = reduce_all(&[
            choices_updated(5, 827, Some(2), Some(40), "GENERAL", &[101, 102]),
            choices_updated(5, 859, Some(2), Some(35), "GENERAL", &[201, 202]),
        ]);

        let sent = reduce(
            &pending,
            &choices_sent(5, Some(827), None, None, "GENERAL", &[101]),
        )
        .expect("task list identifies the sent choice");
        assert_eq!(sent.choices.len(), 1);
        assert_eq!(sent.choices[0].task_list, Some(859));
        assert_eq!(sent.sent_choice_echoes.len(), 1);
        assert_eq!(sent.sent_choice_echoes[0].choice.task_list, Some(827));
        assert_eq!(sent.sent_choice_echoes[0].sent_revision, sent.revision);

        let resolved = reduce(&sent, &choices_resolved(5, None, None, None, &[]))
            .expect("weak resolved event should match the sent tombstone first");
        assert_eq!(resolved.choices.len(), 1);
        assert_eq!(resolved.choices[0].task_list, Some(859));
        assert!(resolved.sent_choice_echoes.is_empty());
        assert_eq!(resolved.warning_count, sent.warning_count);
    }

    #[test]
    fn stale_sent_tombstone_does_not_swallow_a_reused_id_send() {
        let first = reduce(
            &ObservedGameState::default(),
            &choices_updated(5, 827, Some(2), Some(40), "GENERAL", &[101]),
        )
        .unwrap();
        let first_sent = reduce(
            &first,
            &choices_sent(5, Some(827), None, None, "GENERAL", &[101]),
        )
        .unwrap();
        assert!(first_sent.choices.is_empty());
        assert_eq!(first_sent.sent_choice_echoes.len(), 1);

        let reused = reduce(
            &first_sent,
            &choices_updated(5, 859, Some(2), Some(35), "GENERAL", &[201]),
        )
        .unwrap();
        let reused_sent =
            reduce(&reused, &choices_sent(5, None, None, None, "GENERAL", &[])).unwrap();
        assert!(reused_sent.choices.is_empty());
        assert_eq!(reused_sent.sent_choice_echoes.len(), 2);
        assert_eq!(
            reused_sent.sent_choice_echoes[1].choice.task_list,
            Some(859)
        );
    }

    #[test]
    fn weak_resolution_between_stale_echo_and_newer_reused_id_preserves_both() {
        let first = reduce(
            &ObservedGameState::default(),
            &choices_updated(5, 827, Some(2), Some(40), "GENERAL", &[101]),
        )
        .unwrap();
        let first_sent = reduce(
            &first,
            &choices_sent(5, Some(827), None, None, "GENERAL", &[101]),
        )
        .unwrap();
        let reused = reduce(
            &first_sent,
            &choices_updated(5, 859, Some(2), Some(35), "GENERAL", &[201]),
        )
        .unwrap();

        let ambiguous = reduce(&reused, &choices_resolved(5, None, None, None, &[])).unwrap();
        assert_eq!(ambiguous.choices.len(), 1);
        assert_eq!(ambiguous.choices[0].task_list, Some(859));
        assert_eq!(ambiguous.sent_choice_echoes.len(), 1);
        assert_eq!(ambiguous.warning_count, reused.warning_count + 1);
        assert!(ambiguous.warnings.last().unwrap().contains("newer pending"));

        let identified = reduce(
            &ambiguous,
            &choices_resolved(5, Some(859), None, None, &[201]),
        )
        .unwrap();
        assert!(identified.choices.is_empty());
        assert_eq!(identified.sent_choice_echoes.len(), 1);
    }

    #[test]
    fn choice_mismatch_never_clears_and_late_resolution_is_quiet() {
        let pending = reduce_all(&[choices_updated(
            5,
            827,
            Some(2),
            Some(40),
            "GENERAL",
            &[101, 102],
        )]);
        let mismatched = reduce(
            &pending,
            &choices_resolved(5, Some(827), None, None, &[999]),
        )
        .unwrap();
        assert_eq!(mismatched.choices, pending.choices);
        assert_eq!(mismatched.warning_count, pending.warning_count + 1);
        assert!(mismatched
            .warnings
            .last()
            .unwrap()
            .contains("did not uniquely match id=5"));

        let no_same_id = reduce(&mismatched, &choices_resolved(99, None, None, None, &[])).unwrap();
        assert_eq!(no_same_id.choices, mismatched.choices);
        assert_eq!(no_same_id.warning_count, mismatched.warning_count);

        let sent = reduce(
            &no_same_id,
            &choices_sent(5, None, None, None, "GENERAL", &[101]),
        )
        .unwrap();
        assert!(sent.choices.is_empty());
        assert_eq!(sent.sent_choice_echoes.len(), 1);
        let late_resolution =
            reduce(&sent, &choices_resolved(5, None, None, None, &[101])).unwrap();
        assert!(late_resolution.choices.is_empty());
        assert!(late_resolution.sent_choice_echoes.is_empty());
        assert_eq!(late_resolution.warning_count, sent.warning_count);
    }

    #[test]
    fn legacy_optional_choice_json_migrates_to_the_pending_vector() {
        let mut legacy = serde_json::to_value(ObservedGameState::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("sent_choice_echoes");
        legacy["choices"] = serde_json::json!({
            "id": 5,
            "player": 1,
            "choice_type": "MULLIGAN",
            "min": 0,
            "max": 3,
            "source": null,
            "entities": [10, 11, 12]
        });

        let migrated: ObservedGameState = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(migrated.choices.len(), 1);
        assert_eq!(migrated.choices[0].id, 5);
        assert_eq!(migrated.choices[0].task_list, None);
        assert_eq!(migrated.choices[0].issued_revision, 0);
        assert!(migrated.sent_choice_echoes.is_empty());

        legacy["choices"] = serde_json::Value::Null;
        let migrated_null: ObservedGameState = serde_json::from_value(legacy).unwrap();
        assert!(migrated_null.choices.is_empty());
    }

    #[test]
    fn state_hash_covers_decision_availability_identity_and_revision() {
        let mut target = OptionTargetSummary::default();
        target.index = 3;
        target.entity = Some(EntityId(30));
        target.availability = OptionAvailability::Allowed;

        let mut sub_option = option_summary(2, 20);
        sub_option.availability = OptionAvailability::Allowed;
        sub_option.target_entries = vec![target];

        let mut option = option_summary(1, 10);
        option.availability = OptionAvailability::Allowed;
        option.sub_options = vec![sub_option];

        let mut base = ObservedGameState {
            options: Some(OptionsState {
                id: 7,
                issued_revision: 40,
                options: vec![option],
            }),
            revision: 41,
            ..ObservedGameState::default()
        };
        base.choices.push(ChoicesState {
            id: 5,
            task_list: Some(827),
            player: Some(PlayerId(2)),
            player_entity: Some(EntityId(3)),
            player_raw: "PlayerTwo".into(),
            source: Some(EntityId(40)),
            source_raw: "40".into(),
            issued_revision: 39,
            choice_type: "GENERAL".into(),
            min: 1,
            max: 1,
            entities: vec![EntityId(101)],
        });
        let base_hash = state_hash(&base);

        let mut availability = base.clone();
        availability.options.as_mut().unwrap().options[0].availability =
            OptionAvailability::Blocked;
        assert_ne!(base_hash, state_hash(&availability));

        let mut subtarget = base.clone();
        subtarget.options.as_mut().unwrap().options[0].sub_options[0].target_entries[0].entity =
            Some(EntityId(31));
        assert_ne!(base_hash, state_hash(&subtarget));

        let mut task_list = base.clone();
        task_list.choices[0].task_list = Some(859);
        assert_ne!(base_hash, state_hash(&task_list));

        let mut issued_revision = base.clone();
        issued_revision.options.as_mut().unwrap().issued_revision += 1;
        assert_ne!(base_hash, state_hash(&issued_revision));

        let mut sent_echo = base.clone();
        let echoed_choice = sent_echo.choices[0].clone();
        sent_echo.sent_choice_echoes.push(SentChoiceEcho {
            choice: echoed_choice,
            sent_revision: 42,
        });
        assert_ne!(base_hash, state_hash(&sent_echo));
    }

    #[test]
    fn same_sequence_is_deterministic() {
        let events = [
            DomainEvent::GameCreated {
                game_id: GameId(1),
                build_id: None,
            },
            created(1, None, Zone::Play),
            tag(1, TagKey::TURN, 1),
            created(4, Some("HERO_01"), Zone::Play),
            tag(4, TagKey::HEALTH, 30),
            tag(4, TagKey::CONTROLLER, 1),
            DomainEvent::ProtocolWarning {
                message: "BLOCK_START".into(),
            },
        ];
        assert_eq!(reduce_all(&events), reduce_all(&events));
    }

    #[test]
    fn replay_of_all_event_types_is_deterministic() {
        let events = all_event_types_sequence();
        assert!(events.len() >= 40);

        let runs: Vec<ObservedGameState> = (0..3).map(|_| reduce_all(&events)).collect();
        let hashes: Vec<String> = runs.iter().map(state_hash).collect();
        let debugs: Vec<String> = runs.iter().map(|s| format!("{s:?}")).collect();
        assert_eq!(hashes[0], hashes[1]);
        assert_eq!(hashes[1], hashes[2]);
        assert_eq!(hashes[0].len(), 64);
        assert_eq!(debugs[0], debugs[1]);
        assert_eq!(debugs[1], debugs[2]);
        assert_eq!(runs[0], runs[1]);
        assert_eq!(runs[1], runs[2]);

        let mut prev_revision = 0;
        let mut state = ObservedGameState::default();
        for event in &events {
            state = reduce(&state, event).unwrap();
            assert_eq!(state.revision, prev_revision + 1);
            prev_revision = state.revision;
        }
        assert_eq!(state.revision, events.len() as u64);

        assert_eq!(state.game_id, Some(GameId(11)));
        assert_eq!(state.phase, GamePhase::Ended);
        assert_eq!(state.winner, Some(PlayerId(1)));
        assert_eq!(state.turn, 2);
        assert_eq!(state.current_player, Some(PlayerId(2)));
        assert!(state.block_stack.is_empty());
        assert!(state.options.is_none());
        assert!(state.choices.is_empty());
        assert_eq!(state.players.len(), 2);
        assert_eq!(
            state.player(PlayerId(1)).unwrap().playstate,
            Some(game_tag::play_state::WON)
        );
        let hero = state.entity(EntityId(4)).unwrap();
        assert_eq!(hero.current_health(), 25);
        assert!(!hero.is_silenced());
        assert_eq!(
            state
                .entity(EntityId(7))
                .unwrap()
                .card_id
                .as_ref()
                .unwrap()
                .as_str(),
            "NEW1_007"
        );
        assert_eq!(state.entity(EntityId(7)).unwrap().zone, Zone::Graveyard);
        assert!(state
            .warnings
            .iter()
            .any(|w| w.contains("empty block_stack")));
        assert!(!state.warnings.iter().any(|w| w.contains("ChoicesResolved")));

        let mut with_extra_warning = state.clone();
        with_extra_warning.warnings.push("wording only".into());
        with_extra_warning.warning_count += 1;
        assert_eq!(state_hash(&state), state_hash(&with_extra_warning));
        with_extra_warning.revision += 1;
        assert_ne!(state_hash(&state), state_hash(&with_extra_warning));
    }

    fn fixture_records() -> Vec<RawLogRecord> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/logs/999999-game-open.powerlog.txt");
        let text = std::fs::read_to_string(path).unwrap();
        text.lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .enumerate()
            .map(|(idx, line)| RawLogRecord {
                session_id: LogSessionId(1),
                file_id: LogFileId(1),
                byte_offset: idx as u64,
                timestamp: None,
                raw_line: Arc::from(line),
                parse_status: ParseStatus::Pending,
            })
            .collect()
    }

    #[test]
    fn fixture_parser_normalizer_reduce_end_to_end() {
        let packets = PowerLogParser::parse_records(fixture_records());
        let mut normalizer = PowerEventNormalizer::new();
        let mut events = Vec::new();
        for packet in &packets {
            events.extend(normalizer.normalize(packet).unwrap());
        }
        assert!(events
            .iter()
            .any(|event| matches!(event, DomainEvent::GameCreated { .. })));

        let first = reduce_all(&events);
        let second = reduce_all(&events);
        assert_eq!(first, second);
        assert_eq!(state_hash(&first), state_hash(&second));

        assert_eq!(first.entities.len(), 4);
        // GameId 规则（M1）：session_id * 1_000_000 + CREATE_GAME 序号。
        assert_eq!(first.game_id, Some(GameId(1_000_001)));
        assert_eq!(first.turn, 1);
        assert_eq!(first.phase, GamePhase::Mulligan);
        assert_eq!(first.players.len(), 2);
        assert_eq!(
            first.player(PlayerId(1)).unwrap().playstate,
            Some(game_tag::play_state::PLAYING)
        );

        let game = first.entities.get(&EntityId(1)).unwrap();
        assert_eq!(game.zone, Zone::Play);
        assert_eq!(game.tags.get(&game_tag::STEP), Some(&4));
        assert_eq!(game.tags.get(&game_tag::TURN), Some(&1));

        let hero = first.entities.get(&EntityId(4)).unwrap();
        assert_eq!(hero.card_id.as_ref().unwrap().as_str(), "HERO_01");
        assert_eq!(hero.zone, Zone::Play);
        assert_eq!(hero.controller, Some(PlayerId(1)));
        assert_eq!(hero.tags.get(&game_tag::HEALTH), Some(&30));
        assert_eq!(hero.tags.get(&game_tag::DAMAGE), Some(&0));
        assert_eq!(hero.tags.get(&game_tag::CARDTYPE), Some(&3));
        assert_eq!(hero.health(), 30);
        assert_eq!(hero.current_health(), 30);

        assert_eq!(first.warnings.len(), 2);
        assert!(first.warnings[0].contains("BLOCK_START"));
        assert!(first.warnings[1].contains("LoadingScreen.OnSceneLoaded()"));
        assert_eq!(first.warning_count, 2);
        assert!(first.revision > 0);
    }
}
