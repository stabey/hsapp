//! Packet → DomainEvent（设计文档 §7.4，M0 TD-01 + M1 S2）。

use crate::game_tag;
use crate::packet::{
    BlockStartPacket, ChangeEntityPacket, ChoicesPacket, ChosenEntitiesPacket, CreateGamePacket,
    CreateGamePlayer, FullEntityPacket, HideEntityPacket, OptionEntry, OptionsPacket, Packet,
    RawTag, SendChoicesPacket, SendOptionPacket, ShowEntityPacket, TagChangePacket,
};
use crate::{
    DomainEvent, EventNormalizer, NormalizeError, OptionSummary, OptionTargetSummary, TagKey,
};
use hs_types::{CardId, EntityId, GameId, PlayerId, Zone};
use std::collections::BTreeMap;

/// GameId = session_id * 1_000_000 + create_game_seq（session 内从 1 起）。
const GAME_ID_SESSION_SCALE: u64 = 1_000_000;

/// Power.log Packet 归一化器。
///
/// `GameId` 由 CREATE_GAME 首条 source record 的 `session_id` 与该 session
/// 内 CREATE_GAME 序号确定性派生；同一日志重放任意次结果相同。
#[derive(Clone, Debug, Default)]
pub struct PowerEventNormalizer {
    /// session_id → 已发出的 CREATE_GAME 次数（下一个序号 = 已计数 + 1）。
    create_game_seq: BTreeMap<u64, u64>,
    /// 本局 player entity_id → PlayerId（CREATE_GAME 与 PLAYER_ID tag）。
    player_by_entity: BTreeMap<EntityId, PlayerId>,
    /// 本局 entity → 控制者（CONTROLLER tag），friendly 推断用。
    controller_by_entity: BTreeMap<EntityId, PlayerId>,
    /// 本局玩家名 → PlayerId（DebugPrintGame 与括号实体引用中学习；
    /// 结算行 `TAG_CHANGE Entity=<BattleTag>` 靠它解析）。
    player_by_name: BTreeMap<String, PlayerId>,
    /// 本局 PlayerId → player entity（名字引用回填 entity 用）。
    entity_by_player: BTreeMap<PlayerId, EntityId>,
    /// 本局是否已发射 FriendlyPlayerDetected（每局至多一次）。
    friendly_detected: bool,
}

impl PowerEventNormalizer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventNormalizer for PowerEventNormalizer {
    fn normalize(&mut self, packet: &Packet) -> Result<Vec<DomainEvent>, NormalizeError> {
        match packet {
            Packet::CreateGame(packet) => Ok(normalize_create_game(self, packet)),
            Packet::FullEntity(packet) => Ok(normalize_full_entity(self, packet)),
            Packet::ShowEntity(packet) => Ok(normalize_show_entity(self, packet)),
            Packet::ChangeEntity(packet) => Ok(normalize_change_entity(self, packet)),
            Packet::HideEntity(packet) => Ok(normalize_hide_entity(packet)),
            Packet::TagChange(packet) => Ok(normalize_tag_change(self, packet)),
            Packet::BlockStart(packet) => Ok(normalize_block_start(packet)),
            Packet::BlockEnd(_) => Ok(vec![DomainEvent::BlockClosed]),
            Packet::Options(packet) => Ok(normalize_options(packet)),
            Packet::Choices(packet) => Ok(normalize_choices(self, packet)),
            Packet::ChosenEntities(packet) => Ok(normalize_chosen(packet)),
            Packet::SendOption(packet) => Ok(normalize_send_option(packet)),
            Packet::SendChoices(packet) => Ok(normalize_send_choices(packet)),
            Packet::GameMetadata(packet) => Ok(normalize_game_metadata(self, packet)),
            Packet::Unknown(packet) => Ok(vec![DomainEvent::ProtocolWarning {
                message: packet.record.raw_line.to_string(),
            }]),
        }
    }
}

fn normalize_create_game(
    normalizer: &mut PowerEventNormalizer,
    packet: &CreateGamePacket,
) -> Vec<DomainEvent> {
    let game_id = next_game_id(normalizer, packet);
    normalizer.player_by_entity.clear();
    normalizer.controller_by_entity.clear();
    normalizer.player_by_name.clear();
    normalizer.entity_by_player.clear();
    normalizer.friendly_detected = false;

    let mut events = vec![DomainEvent::GameCreated {
        game_id,
        build_id: None,
    }];

    if let Some(game) = &packet.game_entity {
        emit_entity(
            normalizer,
            &mut events,
            game.entity_id,
            None,
            &game.tags,
            &[],
        );
    }
    for player in &packet.players {
        remember_player(normalizer, player.entity_id, player.player_id);
        emit_player(normalizer, &mut events, player);
    }

    // 单人模式特例（hslog FriendlyPlayerExporter）：存在 AI 玩家
    // （GameAccountId lo=0）且恰有一个非 AI 玩家时，后者即友方。
    let ai_present = packet
        .players
        .iter()
        .any(|player| player.account_lo == Some(0));
    let non_ai: Vec<PlayerId> = packet
        .players
        .iter()
        .filter(|player| matches!(player.account_lo, Some(lo) if lo != 0))
        .map(|player| player.player_id)
        .collect();
    if ai_present && non_ai.len() == 1 && non_ai[0].0 != 0 {
        normalizer.friendly_detected = true;
        events.push(DomainEvent::FriendlyPlayerDetected { player: non_ai[0] });
    }
    events
}

fn next_game_id(normalizer: &mut PowerEventNormalizer, packet: &CreateGamePacket) -> GameId {
    let session_id = packet
        .records
        .first()
        .map(|record| record.session_id.0)
        .unwrap_or(0);
    let seq = normalizer.create_game_seq.entry(session_id).or_insert(0);
    *seq = seq.saturating_add(1);
    GameId(
        session_id
            .saturating_mul(GAME_ID_SESSION_SCALE)
            .saturating_add(*seq),
    )
}

fn normalize_full_entity(
    normalizer: &mut PowerEventNormalizer,
    packet: &FullEntityPacket,
) -> Vec<DomainEvent> {
    let mut events = Vec::new();
    emit_entity(
        normalizer,
        &mut events,
        packet.entity_id,
        packet.card_id.clone(),
        &packet.tags,
        &[],
    );
    events
}

fn normalize_show_entity(
    normalizer: &mut PowerEventNormalizer,
    packet: &ShowEntityPacket,
) -> Vec<DomainEvent> {
    let Some(entity_id) = packet.entity_id else {
        return vec![DomainEvent::ProtocolWarning {
            message: format!("SHOW_ENTITY unparsed entity: {}", packet.entity_raw),
        }];
    };
    let Some(card_id) = packet.card_id.clone() else {
        return vec![DomainEvent::ProtocolWarning {
            message: format!("SHOW_ENTITY missing CardID for entity {}", entity_id.0),
        }];
    };
    let mut events = vec![DomainEvent::EntityRevealed { entity_id, card_id }];
    emit_tags(normalizer, &mut events, entity_id, &packet.tags);

    // hslog 规则：第一个 ZONE=HAND 的 SHOW_ENTITY 归属友方玩家
    // （对手手牌不会在手中被揭示；观战模式两侧全揭示时可能误判，接受）。
    if !normalizer.friendly_detected && zone_from_tags(&packet.tags) == Zone::Hand {
        if let Some(player) = normalizer.controller_by_entity.get(&entity_id) {
            normalizer.friendly_detected = true;
            events.push(DomainEvent::FriendlyPlayerDetected { player: *player });
        }
    }
    events
}

fn normalize_change_entity(
    normalizer: &mut PowerEventNormalizer,
    packet: &ChangeEntityPacket,
) -> Vec<DomainEvent> {
    let Some(entity_id) = packet.entity_id else {
        return vec![DomainEvent::ProtocolWarning {
            message: format!("CHANGE_ENTITY unparsed entity: {}", packet.entity_raw),
        }];
    };
    let mut events = vec![DomainEvent::EntityChanged {
        entity_id,
        card_id: packet.card_id.clone(),
    }];
    emit_tags(normalizer, &mut events, entity_id, &packet.tags);
    events
}

fn normalize_hide_entity(packet: &HideEntityPacket) -> Vec<DomainEvent> {
    let Some(entity_id) = packet.entity_id else {
        return vec![DomainEvent::ProtocolWarning {
            message: format!("HIDE_ENTITY unparsed entity: {}", packet.entity_raw),
        }];
    };
    match packet.zone {
        Some(zone) => vec![DomainEvent::TagChanged {
            entity_id,
            tag: TagKey::ZONE,
            value: zone,
        }],
        None => vec![DomainEvent::ProtocolWarning {
            message: format!("HIDE_ENTITY missing ZONE for entity {}", entity_id.0),
        }],
    }
}

fn normalize_tag_change(
    normalizer: &mut PowerEventNormalizer,
    packet: &TagChangePacket,
) -> Vec<DomainEvent> {
    // 括号引用里带真名（如 `[entityName=Foo#123 id=2 ...]`，玩家实体）：
    // 学习 名字 → PlayerId，供后续纯名字引用（结算行）解析。
    if packet.entity.0 != 0 {
        if let Some(player_id) = normalizer.player_by_entity.get(&packet.entity).copied() {
            if let Some(name) = entity_name_from_bracket(&packet.entity_raw) {
                remember_player_name(normalizer, name, player_id);
            }
        }
    }

    // Entity 是纯玩家名（`TAG_CHANGE Entity=Foo#123 ...`）时回填实体 id。
    let entity = if packet.entity.0 != 0 {
        packet.entity
    } else {
        resolve_named_player(normalizer, packet.entity_raw.trim())
            .and_then(|player| normalizer.entity_by_player.get(&player))
            .copied()
            .unwrap_or(packet.entity)
    };

    let mut events = Vec::new();
    push_tag_changed(normalizer, &mut events, entity, packet.tag, packet.value);
    if packet.tag == game_tag::PLAYSTATE {
        if packet.value == game_tag::play_state::WON {
            let winner = normalizer
                .player_by_entity
                .get(&entity)
                .copied()
                .or_else(|| resolve_named_player(normalizer, packet.entity_raw.trim()));
            events.push(DomainEvent::GameEnded { winner });
        } else if packet.value == game_tag::play_state::TIED {
            events.push(DomainEvent::GameEnded { winner: None });
        }
    }
    events
}

fn normalize_game_metadata(
    normalizer: &mut PowerEventNormalizer,
    packet: &crate::packet::GameMetadataPacket,
) -> Vec<DomainEvent> {
    if let (Some(player_id), Some(name)) = (packet.player_id, packet.player_name.as_deref()) {
        remember_player_name(normalizer, name, PlayerId(player_id));
    }
    match packet.build_number {
        Some(build) => vec![DomainEvent::BuildIdentified {
            build_id: hs_types::BuildId(build),
        }],
        None => Vec::new(),
    }
}

fn normalize_block_start(packet: &BlockStartPacket) -> Vec<DomainEvent> {
    vec![DomainEvent::BlockOpened {
        block_type: packet.block_type.clone(),
        source: packet.source,
        target: packet.target,
        effect_card_id: packet.effect_card_id.clone(),
    }]
}

fn normalize_options(packet: &OptionsPacket) -> Vec<DomainEvent> {
    vec![DomainEvent::OptionsUpdated {
        id: packet.id,
        options: packet.options.iter().map(summarize_option).collect(),
    }]
}

fn summarize_option(entry: &OptionEntry) -> OptionSummary {
    let target_entries: Vec<OptionTargetSummary> = if entry.target_entries.is_empty() {
        // 旧序列化 Packet 只有 targets；先迁移成无损节点，之后所有兼容投影
        // 都从 target_entries 生成，避免两套来源漂移。
        entry
            .targets
            .iter()
            .enumerate()
            .map(|(index, entity)| OptionTargetSummary {
                index: index as u32,
                entity: Some(*entity),
                ..OptionTargetSummary::default()
            })
            .collect()
    } else {
        entry
            .target_entries
            .iter()
            .map(|target| OptionTargetSummary {
                index: target.index,
                entity: target.entity,
                entity_raw: target.entity_raw.clone(),
                error: target.error.clone(),
                availability: target.availability,
                raw_error: target.raw_error.clone(),
                error_param: target.error_param.clone(),
            })
            .collect()
    };
    let targets = target_entries
        .iter()
        .filter_map(|target| target.entity)
        .collect();
    OptionSummary {
        index: entry.index,
        option_type: entry.option_type.clone(),
        entity: entry.entity,
        entity_raw: entry.entity_raw.clone(),
        error: entry.error.clone(),
        availability: entry.availability,
        raw_error: entry.raw_error.clone(),
        error_param: entry.error_param.clone(),
        targets,
        target_entries,
        sub_options: entry.sub_options.iter().map(summarize_option).collect(),
    }
}

fn normalize_choices(
    normalizer: &PowerEventNormalizer,
    packet: &ChoicesPacket,
) -> Vec<DomainEvent> {
    let player = packet
        .player_entity
        .and_then(|entity| normalizer.player_by_entity.get(&entity).copied());
    vec![DomainEvent::ChoicesUpdated {
        id: packet.id,
        task_list: packet.task_list,
        player,
        player_entity: packet.player_entity,
        player_raw: packet.player_raw.clone(),
        choice_type: packet.choice_type.clone(),
        min: packet.min,
        max: packet.max,
        source: packet.source,
        source_raw: packet.source_raw.clone(),
        entities: packet.entities.clone(),
    }]
}

fn normalize_chosen(packet: &ChosenEntitiesPacket) -> Vec<DomainEvent> {
    vec![DomainEvent::ChoicesResolved {
        id: packet.id,
        task_list: packet.task_list,
        player_entity: packet.player_entity,
        player_raw: packet.player_raw.clone(),
        source: packet.source,
        source_raw: packet.source_raw.clone(),
        entities: packet.entities.clone(),
    }]
}

fn normalize_send_option(packet: &SendOptionPacket) -> Vec<DomainEvent> {
    vec![DomainEvent::OptionChosen {
        id: packet.id,
        option: packet.option,
        sub_option: packet.sub_option,
        target: packet.target,
        position: packet.position,
    }]
}

fn normalize_send_choices(packet: &SendChoicesPacket) -> Vec<DomainEvent> {
    vec![DomainEvent::ChoicesSent {
        id: packet.id,
        task_list: packet.task_list,
        player_entity: packet.player_entity,
        player_raw: packet.player_raw.clone(),
        choice_type: packet.choice_type.clone(),
        source: packet.source,
        source_raw: packet.source_raw.clone(),
        entities: packet.entities.clone(),
    }]
}

fn emit_player(
    normalizer: &mut PowerEventNormalizer,
    events: &mut Vec<DomainEvent>,
    player: &CreateGamePlayer,
) {
    let extra = if player.player_id.0 != 0 {
        vec![(game_tag::PLAYER_ID, player.player_id.0)]
    } else {
        Vec::new()
    };
    emit_entity(
        normalizer,
        events,
        player.entity_id,
        None,
        &player.tags,
        extra.as_slice(),
    );
}

fn emit_entity(
    normalizer: &mut PowerEventNormalizer,
    events: &mut Vec<DomainEvent>,
    entity_id: EntityId,
    card_id: Option<CardId>,
    tags: &[RawTag],
    extra: &[(i32, i32)],
) {
    if entity_id.0 == 0 {
        return;
    }
    events.push(DomainEvent::EntityCreated {
        entity_id,
        card_id,
        zone: zone_from_tags(tags),
    });
    for &(tag, value) in extra {
        push_tag_changed(normalizer, events, entity_id, tag, value);
    }
    emit_tags(normalizer, events, entity_id, tags);
}

fn emit_tags(
    normalizer: &mut PowerEventNormalizer,
    events: &mut Vec<DomainEvent>,
    entity_id: EntityId,
    tags: &[RawTag],
) {
    for tag in tags {
        push_tag_changed(
            normalizer,
            events,
            entity_id,
            game_tag::id_by_name(&tag.name),
            game_tag::value_by_name(&tag.name, &tag.value),
        );
    }
}

fn push_tag_changed(
    normalizer: &mut PowerEventNormalizer,
    events: &mut Vec<DomainEvent>,
    entity_id: EntityId,
    tag: i32,
    value: i32,
) {
    if tag == game_tag::PLAYER_ID {
        remember_player(normalizer, entity_id, PlayerId(value));
    }
    if tag == game_tag::CONTROLLER && entity_id.0 != 0 && value != 0 {
        normalizer
            .controller_by_entity
            .insert(entity_id, PlayerId(value));
    }
    events.push(DomainEvent::TagChanged {
        entity_id,
        tag: TagKey(tag),
        value,
    });
}

fn remember_player(
    normalizer: &mut PowerEventNormalizer,
    entity_id: EntityId,
    player_id: PlayerId,
) {
    if entity_id.0 != 0 && player_id.0 != 0 {
        normalizer.player_by_entity.insert(entity_id, player_id);
        normalizer.entity_by_player.insert(player_id, entity_id);
    }
}

/// 记住玩家名（`UNKNOWN HUMAN PLAYER` 是占位名，不入映射）。
fn remember_player_name(normalizer: &mut PowerEventNormalizer, name: &str, player_id: PlayerId) {
    let name = name.trim();
    if !name.is_empty() && name != "UNKNOWN HUMAN PLAYER" && player_id.0 != 0 {
        normalizer
            .player_by_name
            .insert(name.to_string(), player_id);
    }
}

/// 解析纯玩家名引用。未知名字时用排除法（hslog PlayerManager 惯例）：
/// 恰有一个玩家还没有名字，则该名字归属它，并顺带学习。
fn resolve_named_player(normalizer: &mut PowerEventNormalizer, name: &str) -> Option<PlayerId> {
    if name.is_empty() || name == "UNKNOWN HUMAN PLAYER" || name == "GameEntity" {
        return None;
    }
    if let Some(player) = normalizer.player_by_name.get(name) {
        return Some(*player);
    }
    let unnamed: Vec<PlayerId> = normalizer
        .entity_by_player
        .keys()
        .filter(|player| !normalizer.player_by_name.values().any(|p| p == *player))
        .copied()
        .collect();
    if let [player] = unnamed.as_slice() {
        let player = *player;
        remember_player_name(normalizer, name, player);
        return Some(player);
    }
    None
}

/// 从括号实体引用 `[entityName=... id=N ...]` 中提取 entityName。
fn entity_name_from_bracket(raw: &str) -> Option<&str> {
    let inner = raw.strip_prefix('[')?;
    let rest = inner.strip_prefix("entityName=")?;
    // entityName 值到下一个 ` key=` 为止（名字可含空格）。
    let mut end = rest.len();
    for (idx, _) in rest.match_indices(' ') {
        let after = &rest[idx + 1..];
        if after
            .split_once('=')
            .is_some_and(|(key, _)| !key.is_empty() && !key.contains(' '))
        {
            end = idx;
            break;
        }
    }
    let name = rest[..end].trim();
    (!name.is_empty()).then_some(name)
}

fn zone_from_tags(tags: &[RawTag]) -> Zone {
    tags.iter()
        .find(|tag| tag.name == "ZONE")
        .map(|tag| game_tag::zone_from_value(game_tag::value_by_name("ZONE", &tag.value)))
        .unwrap_or(Zone::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{
        BlockEndPacket, CreateGameEntity, HideEntityPacket, OptionAvailability, OptionTarget,
        ShowEntityPacket, UnknownPacket,
    };
    use crate::parser::PowerLogParser;
    use hs_log_record::{ParseStatus, RawLogRecord};
    use hs_types::{EntityId, LogFileId, LogSessionId, PlayerId};
    use std::sync::Arc;

    fn rec(line: &str) -> RawLogRecord {
        rec_with_session(line, 1)
    }

    fn rec_with_session(line: &str, session: u64) -> RawLogRecord {
        RawLogRecord {
            session_id: LogSessionId(session),
            file_id: LogFileId(1),
            byte_offset: 0,
            timestamp: None,
            raw_line: Arc::from(line),
            parse_status: ParseStatus::Unrecognized,
        }
    }

    fn raw(name: &str, value: &str) -> RawTag {
        RawTag {
            name: name.into(),
            value: value.into(),
        }
    }

    fn normalize(packet: Packet) -> Vec<DomainEvent> {
        PowerEventNormalizer::new().normalize(&packet).unwrap()
    }

    #[test]
    fn create_game_emits_game_and_entities() {
        let packet = Packet::CreateGame(CreateGamePacket {
            game_entity: Some(CreateGameEntity {
                entity_id: EntityId(1),
                tags: vec![raw("ZONE", "PLAY"), raw("TURN", "1")],
            }),
            players: vec![
                CreateGamePlayer {
                    entity_id: EntityId(2),
                    player_id: PlayerId(1),
                    tags: vec![raw("PLAYSTATE", "PLAYING")],
                    ..CreateGamePlayer::default()
                },
                CreateGamePlayer {
                    entity_id: EntityId(3),
                    player_id: PlayerId(2),
                    tags: vec![raw("ZONE", "PLAY")],
                    ..CreateGamePlayer::default()
                },
            ],
            records: Vec::new(),
        });

        let events = normalize(packet);
        assert!(matches!(
            &events[0],
            DomainEvent::GameCreated {
                game_id,
                build_id: None
            } if *game_id == GameId(1)
        ));
        assert!(matches!(
            &events[1],
            DomainEvent::EntityCreated {
                entity_id,
                card_id: None,
                zone: Zone::Play
            } if *entity_id == EntityId(1)
        ));
        assert!(matches!(
            &events[2],
            DomainEvent::TagChanged {
                entity_id,
                tag,
                value: 1
            } if *entity_id == EntityId(1) && *tag == TagKey::ZONE
        ));
        assert!(matches!(
            &events[3],
            DomainEvent::TagChanged {
                entity_id,
                tag,
                value: 1
            } if *entity_id == EntityId(1) && *tag == TagKey::TURN
        ));

        let player_ids: Vec<EntityId> = events
            .iter()
            .filter_map(|event| match event {
                DomainEvent::EntityCreated { entity_id, .. } => Some(*entity_id),
                _ => None,
            })
            .collect();
        assert_eq!(player_ids, vec![EntityId(1), EntityId(2), EntityId(3)]);

        assert!(events.iter().any(|event| matches!(
            event,
            DomainEvent::TagChanged {
                entity_id,
                tag,
                value: 1
            } if *entity_id == EntityId(2) && *tag == TagKey::PLAYER_ID
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            DomainEvent::TagChanged {
                entity_id,
                tag,
                value: 2
            } if *entity_id == EntityId(3) && *tag == TagKey::PLAYER_ID
        )));
    }

    #[test]
    fn full_entity_emits_create_and_tags() {
        let packet = Packet::FullEntity(FullEntityPacket {
            entity_id: EntityId(9),
            card_id: Some(CardId::new("CS2_029")),
            tags: vec![raw("COST", "4"), raw("ZONE", "HAND")],
            records: Vec::new(),
        });
        let events = normalize(packet);
        assert_eq!(events.len(), 3);
        assert!(matches!(
            &events[0],
            DomainEvent::EntityCreated {
                entity_id,
                card_id: Some(card),
                zone: Zone::Hand
            } if *entity_id == EntityId(9) && card.as_str() == "CS2_029"
        ));
        assert!(matches!(
            &events[1],
            DomainEvent::TagChanged { tag, value: 4, .. } if *tag == TagKey::COST
        ));
        assert!(matches!(
            &events[2],
            DomainEvent::TagChanged { tag, value: 3, .. } if *tag == TagKey::ZONE
        ));
    }

    #[test]
    fn tag_change_maps_numeric_fields() {
        let packet = Packet::TagChange(TagChangePacket {
            entity: EntityId(1),
            tag: 19,
            value: 4,
            tag_name: "STEP".into(),
            value_raw: "BEGIN_MULLIGAN".into(),
            entity_raw: "GameEntity".into(),
            record: None,
        });
        let events = normalize(packet);
        assert_eq!(
            events,
            vec![DomainEvent::TagChanged {
                entity_id: EntityId(1),
                tag: TagKey::STEP,
                value: 4,
            }]
        );
    }

    #[test]
    fn unknown_keeps_raw_line() {
        let line = "I 21:35:47.0 LoadingScreen.OnSceneLoaded() - nextMode=GAMEPLAY";
        let packet = Packet::Unknown(UnknownPacket { record: rec(line) });
        let events = normalize(packet);
        assert_eq!(
            events,
            vec![DomainEvent::ProtocolWarning {
                message: line.into(),
            }]
        );
    }

    #[test]
    fn show_entity_without_entity_id_is_warning() {
        let events = normalize(Packet::ShowEntity(ShowEntityPacket {
            entity_raw: "Anduin".into(),
            card_id: Some(CardId::new("CS2_029")),
            ..ShowEntityPacket::default()
        }));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            DomainEvent::ProtocolWarning { message }
                if message.contains("SHOW_ENTITY unparsed entity") && message.contains("Anduin")
        ));
    }

    #[test]
    fn show_entity_without_card_id_is_warning() {
        let events = normalize(Packet::ShowEntity(ShowEntityPacket {
            entity_id: Some(EntityId(12)),
            entity_raw: "12".into(),
            ..ShowEntityPacket::default()
        }));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            DomainEvent::ProtocolWarning { message } if message.contains("SHOW_ENTITY missing CardID")
        ));
    }

    #[test]
    fn show_entity_emits_revealed_and_tags() {
        let events = normalize(Packet::ShowEntity(ShowEntityPacket {
            entity_id: Some(EntityId(12)),
            entity_raw: "12".into(),
            card_id: Some(CardId::new("CS2_029")),
            tags: vec![raw("COST", "4"), raw("ZONE", "HAND")],
            records: Vec::new(),
        }));
        assert!(matches!(
            &events[0],
            DomainEvent::EntityRevealed { entity_id, card_id }
                if *entity_id == EntityId(12) && card_id.as_str() == "CS2_029"
        ));
        assert_eq!(events.len(), 3);
        assert!(matches!(
            &events[1],
            DomainEvent::TagChanged { tag, value: 4, .. } if *tag == TagKey::COST
        ));
    }

    #[test]
    fn change_entity_emits_changed_and_tags() {
        let events = normalize(Packet::ChangeEntity(ChangeEntityPacket {
            entity_id: Some(EntityId(9)),
            entity_raw: "9".into(),
            card_id: Some(CardId::new("NEW1_026")),
            tags: vec![raw("ATK", "3")],
            records: Vec::new(),
        }));
        assert!(matches!(
            &events[0],
            DomainEvent::EntityChanged { entity_id, card_id: Some(card) }
                if *entity_id == EntityId(9) && card.as_str() == "NEW1_026"
        ));
        assert!(matches!(
            &events[1],
            DomainEvent::TagChanged { tag, value: 3, .. } if *tag == TagKey::ATK
        ));
    }

    #[test]
    fn hide_entity_emits_zone_tag() {
        let events = normalize(Packet::HideEntity(HideEntityPacket {
            entity_id: Some(EntityId(12)),
            entity_raw: "12".into(),
            zone: Some(game_tag::zone::DECK),
            records: Vec::new(),
        }));
        assert_eq!(
            events,
            vec![DomainEvent::TagChanged {
                entity_id: EntityId(12),
                tag: TagKey::ZONE,
                value: game_tag::zone::DECK,
            }]
        );
    }

    #[test]
    fn hide_entity_without_entity_id_is_warning() {
        let events = normalize(Packet::HideEntity(HideEntityPacket {
            entity_raw: "Anduin".into(),
            zone: Some(game_tag::zone::DECK),
            ..HideEntityPacket::default()
        }));
        assert!(matches!(
            &events[0],
            DomainEvent::ProtocolWarning { message }
                if message.contains("HIDE_ENTITY unparsed entity") && message.contains("Anduin")
        ));
    }

    #[test]
    fn hide_entity_without_zone_is_warning() {
        let events = normalize(Packet::HideEntity(HideEntityPacket {
            entity_id: Some(EntityId(12)),
            entity_raw: "12".into(),
            ..HideEntityPacket::default()
        }));
        assert!(matches!(
            &events[0],
            DomainEvent::ProtocolWarning { message } if message.contains("HIDE_ENTITY missing ZONE")
        ));
    }

    #[test]
    fn block_start_and_end_map_directly() {
        let opened = normalize(Packet::BlockStart(BlockStartPacket {
            block_type: "ATTACK".into(),
            source: Some(EntityId(9)),
            target: Some(EntityId(6)),
            effect_card_id: Some(CardId::new("CS2_029")),
            ..BlockStartPacket::default()
        }));
        assert_eq!(
            opened,
            vec![DomainEvent::BlockOpened {
                block_type: "ATTACK".into(),
                source: Some(EntityId(9)),
                target: Some(EntityId(6)),
                effect_card_id: Some(CardId::new("CS2_029")),
            }]
        );
        assert_eq!(
            normalize(Packet::BlockEnd(BlockEndPacket::default())),
            vec![DomainEvent::BlockClosed]
        );
    }

    #[test]
    fn options_project_recursively_without_loss() {
        let events = normalize(Packet::Options(OptionsPacket {
            id: 2,
            options: vec![OptionEntry {
                index: 1,
                option_type: "POWER".into(),
                entity: Some(EntityId(4)),
                entity_raw: "[name=Power id=4]".into(),
                availability: OptionAvailability::Allowed,
                raw_error: Some("NONE".into()),
                error_param: Some(String::new()),
                // 故意与无损节点冲突；领域兼容投影必须只认 target_entries。
                targets: vec![EntityId(99)],
                target_entries: vec![OptionTarget {
                    index: 7,
                    entity: Some(EntityId(6)),
                    entity_raw: "[name=Target id=6]".into(),
                    error: Some("REQ_FRIENDLY_TARGET".into()),
                    availability: OptionAvailability::Blocked,
                    raw_error: Some("REQ_FRIENDLY_TARGET".into()),
                    error_param: Some("42".into()),
                }],
                sub_options: vec![OptionEntry {
                    index: 0,
                    option_type: "subOption".into(),
                    entity: Some(EntityId(8)),
                    entity_raw: "8".into(),
                    availability: OptionAvailability::Allowed,
                    sub_options: vec![OptionEntry {
                        index: 2,
                        entity: Some(EntityId(9)),
                        entity_raw: "9".into(),
                        availability: OptionAvailability::Blocked,
                        raw_error: Some("REQ_ENOUGH_MANA".into()),
                        ..OptionEntry::default()
                    }],
                    ..OptionEntry::default()
                }],
                ..OptionEntry::default()
            }],
            records: Vec::new(),
        }));
        let DomainEvent::OptionsUpdated { id, options } = &events[0] else {
            panic!("expected OptionsUpdated");
        };
        assert_eq!(*id, 2);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].index, 1);
        assert_eq!(options[0].entity, Some(EntityId(4)));
        assert_eq!(options[0].targets, vec![EntityId(6)]);
        assert_eq!(options[0].entity_raw, "[name=Power id=4]");
        assert_eq!(options[0].availability, OptionAvailability::Allowed);
        assert_eq!(options[0].raw_error.as_deref(), Some("NONE"));
        assert_eq!(options[0].error_param.as_deref(), Some(""));
        assert_eq!(options[0].target_entries[0].index, 7);
        assert_eq!(
            options[0].target_entries[0].availability,
            OptionAvailability::Blocked
        );
        assert_eq!(
            options[0].target_entries[0].error_param.as_deref(),
            Some("42")
        );
        assert_eq!(options[0].sub_options[0].entity_raw, "8");
        assert_eq!(options[0].sub_options[0].sub_options[0].index, 2);
        assert_eq!(
            options[0].sub_options[0].sub_options[0].availability,
            OptionAvailability::Blocked
        );
    }

    #[test]
    fn choices_lookup_player_id_from_create_game() {
        let mut normalizer = PowerEventNormalizer::new();
        normalizer
            .normalize(&Packet::CreateGame(CreateGamePacket {
                players: vec![CreateGamePlayer {
                    entity_id: EntityId(2),
                    player_id: PlayerId(1),
                    ..CreateGamePlayer::default()
                }],
                ..CreateGamePacket::default()
            }))
            .unwrap();
        let events = normalizer
            .normalize(&Packet::Choices(ChoicesPacket {
                id: 1,
                task_list: Some(4),
                player_entity: Some(EntityId(2)),
                player_raw: "2".into(),
                choice_type: "MULLIGAN".into(),
                min: 0,
                max: 3,
                source: Some(EntityId(1)),
                source_raw: "GameEntity".into(),
                entities: vec![EntityId(12)],
                ..ChoicesPacket::default()
            }))
            .unwrap();
        assert!(matches!(
            &events[0],
            DomainEvent::ChoicesUpdated {
                id: 1,
                task_list: Some(4),
                player: Some(player),
                player_entity: Some(player_entity),
                player_raw,
                choice_type,
                min: 0,
                max: 3,
                source: Some(src),
                source_raw,
                entities,
                ..
            } if *player == PlayerId(1)
                && *player_entity == EntityId(2)
                && player_raw == "2"
                && choice_type == "MULLIGAN"
                && *src == EntityId(1)
                && source_raw == "GameEntity"
                && entities == &vec![EntityId(12)]
        ));
    }

    #[test]
    fn choices_unknown_player_entity_is_none() {
        let events = normalize(Packet::Choices(ChoicesPacket {
            id: 1,
            player_entity: Some(EntityId(99)),
            choice_type: "GENERAL".into(),
            ..ChoicesPacket::default()
        }));
        assert!(matches!(
            &events[0],
            DomainEvent::ChoicesUpdated { player: None, .. }
        ));
    }

    #[test]
    fn chosen_send_choices_and_send_option_map() {
        assert_eq!(
            normalize(Packet::ChosenEntities(ChosenEntitiesPacket {
                id: 4,
                task_list: Some(8),
                player_entity: Some(EntityId(2)),
                player_raw: "2".into(),
                source: Some(EntityId(1)),
                source_raw: "GameEntity".into(),
                entities: vec![EntityId(12)],
                ..ChosenEntitiesPacket::default()
            })),
            vec![DomainEvent::ChoicesResolved {
                id: 4,
                task_list: Some(8),
                player_entity: Some(EntityId(2)),
                player_raw: "2".into(),
                source: Some(EntityId(1)),
                source_raw: "GameEntity".into(),
                entities: vec![EntityId(12)],
            }]
        );
        assert_eq!(
            normalize(Packet::SendChoices(SendChoicesPacket {
                id: 4,
                task_list: Some(8),
                player_entity: Some(EntityId(2)),
                player_raw: "2".into(),
                choice_type: "MULLIGAN".into(),
                source: Some(EntityId(1)),
                source_raw: "GameEntity".into(),
                entities: vec![EntityId(12)],
                ..SendChoicesPacket::default()
            })),
            vec![DomainEvent::ChoicesSent {
                id: 4,
                task_list: Some(8),
                player_entity: Some(EntityId(2)),
                player_raw: "2".into(),
                choice_type: "MULLIGAN".into(),
                source: Some(EntityId(1)),
                source_raw: "GameEntity".into(),
                entities: vec![EntityId(12)],
            }]
        );
        assert_eq!(
            normalize(Packet::SendOption(SendOptionPacket {
                id: 3,
                option: 1,
                sub_option: -1,
                target: 6,
                position: 0,
                records: Vec::new(),
            })),
            vec![DomainEvent::OptionChosen {
                id: 3,
                option: 1,
                sub_option: -1,
                target: 6,
                position: 0,
            }]
        );
    }

    #[test]
    fn game_id_uses_session_and_sequence() {
        let mut normalizer = PowerEventNormalizer::new();
        let first = CreateGamePacket {
            records: vec![rec_with_session("CREATE_GAME", 1)],
            ..CreateGamePacket::default()
        };
        let second = CreateGamePacket {
            records: vec![rec_with_session("CREATE_GAME", 1)],
            ..CreateGamePacket::default()
        };
        let other_session = CreateGamePacket {
            records: vec![rec_with_session("CREATE_GAME", 2)],
            ..CreateGamePacket::default()
        };
        let a = normalizer.normalize(&Packet::CreateGame(first)).unwrap();
        let b = normalizer.normalize(&Packet::CreateGame(second)).unwrap();
        let c = normalizer
            .normalize(&Packet::CreateGame(other_session))
            .unwrap();
        assert!(matches!(
            &a[0],
            DomainEvent::GameCreated { game_id, .. } if *game_id == GameId(1_000_001)
        ));
        assert!(matches!(
            &b[0],
            DomainEvent::GameCreated { game_id, .. } if *game_id == GameId(1_000_002)
        ));
        assert!(matches!(
            &c[0],
            DomainEvent::GameCreated { game_id, .. } if *game_id == GameId(2_000_001)
        ));
    }

    #[test]
    fn successive_create_game_without_records_starts_at_one() {
        let mut normalizer = PowerEventNormalizer::new();
        let first = normalizer
            .normalize(&Packet::CreateGame(CreateGamePacket::default()))
            .unwrap();
        let second = normalizer
            .normalize(&Packet::CreateGame(CreateGamePacket::default()))
            .unwrap();
        assert!(matches!(
            &first[0],
            DomainEvent::GameCreated { game_id, .. } if *game_id == GameId(1)
        ));
        assert!(matches!(
            &second[0],
            DomainEvent::GameCreated { game_id, .. } if *game_id == GameId(2)
        ));
    }

    fn create_two_players() -> Packet {
        Packet::CreateGame(CreateGamePacket {
            players: vec![
                CreateGamePlayer {
                    entity_id: EntityId(2),
                    player_id: PlayerId(1),
                    ..CreateGamePlayer::default()
                },
                CreateGamePlayer {
                    entity_id: EntityId(3),
                    player_id: PlayerId(2),
                    ..CreateGamePlayer::default()
                },
            ],
            ..CreateGamePacket::default()
        })
    }

    #[test]
    fn playstate_won_appends_game_ended() {
        let mut normalizer = PowerEventNormalizer::new();
        normalizer.normalize(&create_two_players()).unwrap();
        let events = normalizer
            .normalize(&Packet::TagChange(TagChangePacket {
                entity: EntityId(2),
                tag: game_tag::PLAYSTATE,
                value: game_tag::play_state::WON,
                tag_name: "PLAYSTATE".into(),
                value_raw: "WON".into(),
                entity_raw: "2".into(),
                record: None,
            }))
            .unwrap();
        assert_eq!(
            events,
            vec![
                DomainEvent::TagChanged {
                    entity_id: EntityId(2),
                    tag: TagKey::PLAYSTATE,
                    value: game_tag::play_state::WON,
                },
                DomainEvent::GameEnded {
                    winner: Some(PlayerId(1)),
                },
            ]
        );
    }

    #[test]
    fn playstate_tied_emits_game_ended_without_winner() {
        let events = normalize(Packet::TagChange(TagChangePacket {
            entity: EntityId(1),
            tag: game_tag::PLAYSTATE,
            value: game_tag::play_state::TIED,
            tag_name: "PLAYSTATE".into(),
            value_raw: "TIED".into(),
            ..TagChangePacket::default()
        }));
        assert!(matches!(
            events.last(),
            Some(DomainEvent::GameEnded { winner: None })
        ));
    }

    #[test]
    fn playstate_won_unmapped_entity_has_no_winner() {
        let events = normalize(Packet::TagChange(TagChangePacket {
            entity: EntityId(0),
            tag: game_tag::PLAYSTATE,
            value: game_tag::play_state::WON,
            tag_name: "PLAYSTATE".into(),
            value_raw: "WON".into(),
            entity_raw: "Anduin".into(),
            record: None,
        }));
        assert_eq!(
            events.last(),
            Some(&DomainEvent::GameEnded { winner: None })
        );
    }

    #[test]
    fn player_id_tag_change_updates_mapping() {
        let mut normalizer = PowerEventNormalizer::new();
        normalizer
            .normalize(&Packet::TagChange(TagChangePacket {
                entity: EntityId(8),
                tag: game_tag::PLAYER_ID,
                value: 2,
                tag_name: "PLAYER_ID".into(),
                value_raw: "2".into(),
                ..TagChangePacket::default()
            }))
            .unwrap();
        let events = normalizer
            .normalize(&Packet::TagChange(TagChangePacket {
                entity: EntityId(8),
                tag: game_tag::PLAYSTATE,
                value: game_tag::play_state::WON,
                tag_name: "PLAYSTATE".into(),
                value_raw: "WON".into(),
                ..TagChangePacket::default()
            }))
            .unwrap();
        assert!(matches!(
            events.last(),
            Some(DomainEvent::GameEnded {
                winner: Some(player)
            }) if *player == PlayerId(2)
        ));
    }

    fn normalize_records(records: Vec<RawLogRecord>) -> Vec<DomainEvent> {
        let packets = PowerLogParser::parse_records(records);
        let mut normalizer = PowerEventNormalizer::new();
        let mut events = Vec::new();
        for packet in &packets {
            events.extend(normalizer.normalize(packet).unwrap());
        }
        events
    }

    #[test]
    fn fixture_packets_normalize_to_expected_kinds() {
        let events = normalize_records(crate::golden::load_fixture_records());

        assert!(
            matches!(events[0], DomainEvent::GameCreated { game_id, .. } if game_id == GameId(1_000_001))
        );
        let created: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                DomainEvent::EntityCreated { entity_id, .. } => Some(*entity_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            created,
            vec![EntityId(1), EntityId(2), EntityId(3), EntityId(4)]
        );

        assert!(events.iter().any(|event| matches!(
            event,
            DomainEvent::BlockOpened {
                block_type,
                source: Some(src),
                ..
            } if block_type == "TRIGGER" && *src == EntityId(1)
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, DomainEvent::BlockClosed)));
        assert!(events.iter().any(|event| matches!(
            event,
            DomainEvent::EntityRevealed { entity_id, card_id }
                if *entity_id == EntityId(4) && card_id.as_str() == "HERO_01"
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, DomainEvent::OptionsUpdated { id: 1, .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            DomainEvent::ChoicesUpdated {
                id: 1,
                player: Some(player),
                ..
            } if *player == PlayerId(1)
        )));

        let warnings: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                DomainEvent::ProtocolWarning { message } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("LoadingScreen.OnSceneLoaded()")),
            "LoadingScreen must remain a ProtocolWarning: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("PowerTaskList")),
            "PowerTaskList must remain a ProtocolWarning: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .all(|message| !message.contains("GameState.DebugPrintPower() - BLOCK_START")),
            "BLOCK_START from GameState must be parsed, not warned: {warnings:?}"
        );
    }

    #[test]
    fn fixture_parse_normalize_is_deterministic() {
        let first = normalize_records(crate::golden::load_fixture_records());
        let second = normalize_records(crate::golden::load_fixture_records());
        assert_eq!(first, second);
    }
}
