//! `hs-tracker` — Tracker 派生读模型(设计文档 §10.2,M2 PRD 决策 3)。
//!
//! 纯函数、无 IO:`ObservedGameState`(+可选活动牌组)→ [`TrackerView`]。
//! 确定性与 M1 同标准(容器 `BTreeMap`/`Vec`,禁 `HashMap`;
//! 同一输入 N 次派生结果相同)。Overlay 与 Live Game 页共用本视图,
//! UI 不得自行复制游戏状态(设计文档 §6.1 禁令)。
//!
//! `my_deck` 口径:活动牌组行按 `dbf_id` 原样透传;日志观察行按 `card_id`
//! 聚合。派生层不做 dbf↔card 映射(交给 UI + card-db)。

use hs_deckstrings::DeckList;
use hs_log_core::game_tag;
use hs_observed_state::{EntityState, ObservedGameState};
use hs_types::{CardId, DbfId, EntityId, GameId, GamePhase, PlayerId, Zone};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 我方牌库卡牌来源(§10.1:原始牌组/生成/洗入/变形)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CardOrigin {
    OriginalDeck,
    Created,
    Shuffled,
    Transformed,
}

/// 我方牌库单行(按 card/dbf 聚合)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DeckCardRow {
    /// 活动牌组给出的 dbf_id;仅从日志观察到时为 None。
    pub dbf_id: Option<DbfId>,
    /// 日志观察到的 card_id;牌组中尚未见到实体时为 None。
    pub card_id: Option<CardId>,
    pub origin: CardOrigin,
    /// 初始张数(生成/洗入的行初始为 0)。
    pub count_initial: u8,
    pub count_remaining: u8,
    pub count_drawn: u8,
    pub count_played: u8,
}

/// 对手已出牌历史单行(按打出顺序)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PlayedCardRow {
    pub turn: u32,
    pub entity_id: EntityId,
    pub card_id: Option<CardId>,
    /// 生成牌(非对手原始牌组来源)标识。
    pub created: bool,
}

/// 对手手牌单张信息(数量 = Vec 长度)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HandCardInfo {
    pub entity_id: EntityId,
    /// 进入手牌的回合(持有回合 = 当前回合 − 该值)。
    pub since_turn: u32,
    /// 已知牌(如被展示/生成时可见)。
    pub known_card: Option<CardId>,
}

/// 墓地/场上引用。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EntityRef {
    pub entity_id: EntityId,
    pub card_id: Option<CardId>,
}

/// 单方公共计数(§10.2:武器/地点/任务/秘密/疲劳)。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SideCounters {
    pub weapons: u8,
    pub locations: u8,
    pub quests: u8,
    pub secrets: u8,
    /// 已受疲劳次数(下一次疲劳伤害 = fatigue + 1)。
    pub fatigue: u8,
}

/// 双方公共计数;`friendly`/`opposing` 以 `GameMeta.friendly_player` 划分。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PublicCounters {
    pub friendly: SideCounters,
    pub opposing: SideCounters,
}

/// 对局元信息(直接来自 `ObservedGameState`)。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GameMeta {
    pub game_id: Option<GameId>,
    pub phase: GamePhase,
    pub turn: u32,
    pub friendly_player: Option<PlayerId>,
    pub winner: Option<PlayerId>,
    /// 派生来源状态的 revision(UI 判新鲜度)。
    pub revision: u64,
}

/// Tracker 派生视图(Overlay 两面板 + Live Game 页的唯一数据源)。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrackerView {
    pub my_deck: Vec<DeckCardRow>,
    pub opponent_played: Vec<PlayedCardRow>,
    pub opponent_hand: Vec<HandCardInfo>,
    /// `[friendly, opposing]`。
    pub graveyards: [Vec<EntityRef>; 2],
    pub counters: PublicCounters,
    pub game: GameMeta,
}

/// 从观察状态派生 Tracker 视图。`active_deck` 为当前激活牌组
/// (None 时 `my_deck` 仅含日志观察到的行)。
pub fn derive(state: &ObservedGameState, active_deck: Option<&DeckList>) -> TrackerView {
    let friendly = state.friendly_player;
    let opposing = friendly.and_then(|pid| opposing_player(state, pid));

    TrackerView {
        my_deck: derive_my_deck(state, active_deck, friendly),
        opponent_played: derive_opponent_played(state, opposing),
        opponent_hand: derive_opponent_hand(state, opposing),
        graveyards: derive_graveyards(state, friendly, opposing),
        counters: derive_counters(state, friendly, opposing),
        game: GameMeta {
            game_id: state.game_id,
            phase: state.phase,
            turn: state.turn,
            friendly_player: state.friendly_player,
            winner: state.winner,
            revision: state.revision,
        },
    }
}

/// 生成表核对(见 `game_tag_table.rs` / `game_tag_value_table.rs`):
/// CREATOR=313, CREATOR_DBID=1284, DISPLAYED_CREATOR=385,
/// TRANSFORMED_FROM_CARD=435, TIMES_BEEN_TRANSFORMED=2321,
/// FATIGUE=22, SECRET=219, QUEST=462, QUESTLINE=1725, SIDE_QUEST=1192,
/// NUM_TURNS_IN_HAND=273, NUM_TURNS_IN_PLAY=271。
fn tag_id(name: &str) -> i32 {
    game_tag::id_by_name(name)
}

const CARDTYPE_GAME: i32 = 1;
const CARDTYPE_PLAYER: i32 = 2;
const CARDTYPE_HERO: i32 = 3;
const CARDTYPE_MINION: i32 = 4;
const CARDTYPE_SPELL: i32 = 5;
const CARDTYPE_ENCHANTMENT: i32 = 6;
const CARDTYPE_WEAPON: i32 = 7;
const CARDTYPE_HERO_POWER: i32 = 10;

fn opposing_player(state: &ObservedGameState, friendly: PlayerId) -> Option<PlayerId> {
    state
        .players
        .keys()
        .copied()
        .map(PlayerId)
        .find(|pid| *pid != friendly)
        .or_else(|| {
            state
                .entities
                .values()
                .filter_map(|entity| entity.controller)
                .find(|pid| *pid != friendly)
        })
}

fn derive_my_deck(
    state: &ObservedGameState,
    active_deck: Option<&DeckList>,
    friendly: Option<PlayerId>,
) -> Vec<DeckCardRow> {
    let mut rows = Vec::new();
    if let Some(deck) = active_deck {
        for &(dbf_id, count) in &deck.cards {
            rows.push(DeckCardRow {
                dbf_id: Some(dbf_id),
                card_id: None,
                origin: CardOrigin::OriginalDeck,
                count_initial: count,
                count_remaining: count,
                count_drawn: 0,
                count_played: 0,
            });
        }
    }

    let mut observed: BTreeMap<(CardId, u8), ObservedCounts> = BTreeMap::new();
    if let Some(friendly) = friendly {
        for entity in state.entities.values() {
            if entity.controller != Some(friendly) || !is_trackable_card(entity) {
                continue;
            }
            let Some(card_id) = entity.card_id.clone() else {
                continue;
            };
            let origin = card_origin(entity);
            let slot = observed
                .entry((card_id, origin_rank(origin)))
                .or_insert_with(|| ObservedCounts {
                    origin,
                    remaining: 0,
                    drawn: 0,
                    played: 0,
                });
            match zone_bucket(entity.zone) {
                ZoneBucket::Remaining => slot.remaining = slot.remaining.saturating_add(1),
                ZoneBucket::Drawn => slot.drawn = slot.drawn.saturating_add(1),
                ZoneBucket::Played => {
                    slot.drawn = slot.drawn.saturating_add(1);
                    slot.played = slot.played.saturating_add(1);
                }
                ZoneBucket::Ignore => {}
            }
        }
    }

    for ((card_id, _), counts) in observed {
        let count_initial = match counts.origin {
            CardOrigin::OriginalDeck => counts.remaining.saturating_add(counts.drawn),
            CardOrigin::Created | CardOrigin::Shuffled | CardOrigin::Transformed => 0,
        };
        rows.push(DeckCardRow {
            dbf_id: None,
            card_id: Some(card_id),
            origin: counts.origin,
            count_initial,
            count_remaining: counts.remaining,
            count_drawn: counts.drawn,
            count_played: counts.played,
        });
    }
    rows
}

struct ObservedCounts {
    origin: CardOrigin,
    remaining: u8,
    drawn: u8,
    played: u8,
}

fn derive_opponent_played(
    state: &ObservedGameState,
    opposing: Option<PlayerId>,
) -> Vec<PlayedCardRow> {
    let Some(opposing) = opposing else {
        return Vec::new();
    };
    let mut rows: Vec<PlayedCardRow> = state
        .entities
        .values()
        .filter(|entity| {
            entity.controller == Some(opposing)
                && is_trackable_card(entity)
                && zone_bucket(entity.zone) == ZoneBucket::Played
        })
        .map(|entity| PlayedCardRow {
            turn: infer_play_turn(state.turn, entity),
            entity_id: entity.entity_id,
            card_id: entity.card_id.clone(),
            created: is_created_like(entity),
        })
        .collect();
    rows.sort_by_key(|row| (row.turn, row.entity_id));
    rows
}

fn derive_opponent_hand(
    state: &ObservedGameState,
    opposing: Option<PlayerId>,
) -> Vec<HandCardInfo> {
    let Some(opposing) = opposing else {
        return Vec::new();
    };
    let turns_in_hand = tag_id("NUM_TURNS_IN_HAND");
    let mut rows: Vec<(u8, HandCardInfo)> = state
        .entities
        .values()
        .filter(|entity| entity.controller == Some(opposing) && entity.zone == Zone::Hand)
        .map(|entity| {
            let held = tag_or_zero(entity, turns_in_hand) as u32;
            (
                entity.zone_position.unwrap_or(u8::MAX),
                HandCardInfo {
                    entity_id: entity.entity_id,
                    since_turn: state.turn.saturating_sub(held),
                    known_card: entity.card_id.clone(),
                },
            )
        })
        .collect();
    rows.sort_by_key(|(pos, info)| (*pos, info.entity_id));
    rows.into_iter().map(|(_, info)| info).collect()
}

fn derive_graveyards(
    state: &ObservedGameState,
    friendly: Option<PlayerId>,
    opposing: Option<PlayerId>,
) -> [Vec<EntityRef>; 2] {
    let mut friendly_gy = Vec::new();
    let mut opposing_gy = Vec::new();
    for entity in state.entities.values() {
        if entity.zone != Zone::Graveyard || !is_trackable_card(entity) {
            continue;
        }
        let row = EntityRef {
            entity_id: entity.entity_id,
            card_id: entity.card_id.clone(),
        };
        if Some(entity.controller.unwrap_or(PlayerId(0))) == friendly {
            friendly_gy.push(row);
        } else if Some(entity.controller.unwrap_or(PlayerId(0))) == opposing {
            opposing_gy.push(row);
        }
    }
    [friendly_gy, opposing_gy]
}

fn derive_counters(
    state: &ObservedGameState,
    friendly: Option<PlayerId>,
    opposing: Option<PlayerId>,
) -> PublicCounters {
    PublicCounters {
        friendly: side_counters(state, friendly),
        opposing: side_counters(state, opposing),
    }
}

fn side_counters(state: &ObservedGameState, player: Option<PlayerId>) -> SideCounters {
    let Some(player) = player else {
        return SideCounters::default();
    };
    let secret_tag = tag_id("SECRET");
    let quest_tag = tag_id("QUEST");
    let questline_tag = tag_id("QUESTLINE");
    let side_quest_tag = tag_id("SIDE_QUEST");
    let fatigue_tag = tag_id("FATIGUE");

    let mut counters = SideCounters::default();
    for entity in state.entities.values() {
        if entity.controller != Some(player) {
            continue;
        }
        let cardtype = cardtype_of(entity);
        if entity.zone == Zone::Play {
            if cardtype == CARDTYPE_WEAPON {
                counters.weapons = counters.weapons.saturating_add(1);
            }
            if cardtype == game_tag::card_type::LOCATION {
                counters.locations = counters.locations.saturating_add(1);
            }
        }
        let is_quest = tag_or_zero(entity, quest_tag) != 0
            || tag_or_zero(entity, questline_tag) != 0
            || tag_or_zero(entity, side_quest_tag) != 0;
        if is_quest && matches!(entity.zone, Zone::Secret | Zone::Play) {
            counters.quests = counters.quests.saturating_add(1);
        } else if entity.zone == Zone::Secret && tag_or_zero(entity, secret_tag) != 0 {
            counters.secrets = counters.secrets.saturating_add(1);
        } else if entity.zone == Zone::Secret && !is_quest {
            // ZONE=SECRET 但 SECRET tag 未置位:仍计为秘密(任务已在上面分流)。
            counters.secrets = counters.secrets.saturating_add(1);
        }
    }
    counters.fatigue = player_fatigue(state, player, fatigue_tag);
    counters
}

fn player_fatigue(state: &ObservedGameState, player: PlayerId, fatigue_tag: i32) -> u8 {
    if let Some(player_state) = state.player(player) {
        if let Some(entity) = state.entity(player_state.entity_id) {
            let value = tag_or_zero(entity, fatigue_tag);
            if value > 0 {
                return u8::try_from(value).unwrap_or(u8::MAX);
            }
        }
    }
    state
        .entities
        .values()
        .filter(|entity| {
            entity.controller == Some(player) && cardtype_of(entity) == CARDTYPE_PLAYER
        })
        .map(|entity| tag_or_zero(entity, fatigue_tag))
        .max()
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(0)
}

fn card_origin(entity: &EntityState) -> CardOrigin {
    let transformed = tag_or_zero(entity, tag_id("TRANSFORMED_FROM_CARD")) != 0
        || tag_or_zero(entity, tag_id("TIMES_BEEN_TRANSFORMED")) != 0;
    if transformed {
        return CardOrigin::Transformed;
    }
    if is_created_like(entity) {
        if entity.zone == Zone::Deck {
            return CardOrigin::Shuffled;
        }
        return CardOrigin::Created;
    }
    CardOrigin::OriginalDeck
}

fn is_created_like(entity: &EntityState) -> bool {
    tag_or_zero(entity, tag_id("CREATOR")) != 0
        || tag_or_zero(entity, tag_id("CREATOR_DBID")) != 0
        || tag_or_zero(entity, tag_id("DISPLAYED_CREATOR")) != 0
}

fn is_trackable_card(entity: &EntityState) -> bool {
    match cardtype_of(entity) {
        CARDTYPE_GAME | CARDTYPE_PLAYER | CARDTYPE_HERO_POWER | CARDTYPE_ENCHANTMENT => false,
        CARDTYPE_HERO => !matches!(entity.zone, Zone::Play | Zone::Invalid),
        CARDTYPE_MINION | CARDTYPE_SPELL | CARDTYPE_WEAPON | game_tag::card_type::LOCATION | 0 => {
            true
        }
        _ => true,
    }
}

fn cardtype_of(entity: &EntityState) -> i32 {
    entity.tags.get(&game_tag::CARDTYPE).copied().unwrap_or(0)
}

fn tag_or_zero(entity: &EntityState, tag: i32) -> i32 {
    entity.tags.get(&tag).copied().unwrap_or(0)
}

fn origin_rank(origin: CardOrigin) -> u8 {
    match origin {
        CardOrigin::OriginalDeck => 0,
        CardOrigin::Created => 1,
        CardOrigin::Shuffled => 2,
        CardOrigin::Transformed => 3,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ZoneBucket {
    Remaining,
    Drawn,
    Played,
    Ignore,
}

fn zone_bucket(zone: Zone) -> ZoneBucket {
    match zone {
        Zone::Deck => ZoneBucket::Remaining,
        Zone::Hand | Zone::SetAside => ZoneBucket::Drawn,
        Zone::Play | Zone::Graveyard | Zone::Secret | Zone::RemovedFromGame => ZoneBucket::Played,
        Zone::Invalid | Zone::Unknown(_) => ZoneBucket::Ignore,
    }
}

fn infer_play_turn(current_turn: u32, entity: &EntityState) -> u32 {
    let turns_in_play = tag_or_zero(entity, tag_id("NUM_TURNS_IN_PLAY"));
    if turns_in_play > 0 {
        return current_turn.saturating_sub((turns_in_play as u32).saturating_sub(1));
    }
    if tag_or_zero(entity, tag_id("JUST_PLAYED")) != 0 {
        return current_turn;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_log_core::{EventNormalizer, PowerEventNormalizer, PowerLogParser};
    use hs_log_tailer::{ParseStatus, RawLogRecord};
    use hs_observed_state::reduce;
    use hs_types::{CardType, FormatType, LogFileId, LogSessionId};

    fn entity(
        id: i32,
        card: Option<&str>,
        controller: i32,
        zone: Zone,
        cardtype: i32,
        extra: &[(i32, i32)],
    ) -> EntityState {
        let mut state = EntityState::new(EntityId(id), card.map(CardId::new), zone);
        state.controller = Some(PlayerId(controller));
        state.tags.insert(game_tag::CARDTYPE, cardtype);
        state.tags.insert(game_tag::CONTROLLER, controller);
        for &(tag, value) in extra {
            state.tags.insert(tag, value);
        }
        state
    }

    fn with_entities(entities: Vec<EntityState>) -> ObservedGameState {
        let mut state = ObservedGameState::default();
        state.friendly_player = Some(PlayerId(1));
        state.turn = 3;
        state.entities.insert(
            EntityId(2),
            entity(
                2,
                None,
                1,
                Zone::Play,
                CARDTYPE_PLAYER,
                &[(tag_id("FATIGUE"), 2)],
            ),
        );
        state.entities.insert(
            EntityId(3),
            entity(3, None, 2, Zone::Play, CARDTYPE_PLAYER, &[]),
        );
        for item in entities {
            state.entities.insert(item.entity_id, item);
        }
        state
    }

    #[test]
    fn generated_tag_ids_match_table() {
        assert_eq!(tag_id("CREATOR"), 313);
        assert_eq!(tag_id("CREATOR_DBID"), 1284);
        assert_eq!(tag_id("DISPLAYED_CREATOR"), 385);
        assert_eq!(tag_id("TRANSFORMED_FROM_CARD"), 435);
        assert_eq!(tag_id("TIMES_BEEN_TRANSFORMED"), 2321);
        assert_eq!(tag_id("FATIGUE"), 22);
        assert_eq!(tag_id("SECRET"), 219);
        assert_eq!(tag_id("QUEST"), 462);
        assert_eq!(tag_id("QUESTLINE"), 1725);
        assert_eq!(tag_id("SIDE_QUEST"), 1192);
        assert_eq!(tag_id("NUM_TURNS_IN_HAND"), 273);
        assert_eq!(tag_id("NUM_TURNS_IN_PLAY"), 271);
        assert_eq!(tag_id("JUST_PLAYED"), 261);
        assert_eq!(game_tag::card_type::LOCATION, 39);
        assert_eq!(game_tag::card_type::WEAPON, CARDTYPE_WEAPON);
        assert_eq!(game_tag::card_type::MINION, CARDTYPE_MINION);
    }

    fn power_record(offset: u64, payload: &str) -> RawLogRecord {
        RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            byte_offset: offset,
            timestamp: None,
            raw_line: format!("D 00:00:00.0000000 GameState.DebugPrintPower() - {payload}").into(),
            parse_status: ParseStatus::Pending,
        }
    }

    #[test]
    fn symbolic_location_flows_from_power_log_to_tracker() {
        let payloads = [
            "FULL_ENTITY - Creating ID=20 CardID=",
            "    tag=ZONE value=HAND",
            "    tag=CONTROLLER value=1",
            "SHOW_ENTITY - Updating Entity=[entityName=UNKNOWN ENTITY id=20 zone=HAND zonePos=1 cardId= player=1] CardID=CS2_029",
            "    tag=ZONE value=HAND",
            "    tag=CONTROLLER value=1",
            "FULL_ENTITY - Creating ID=25 CardID=VAC_LOC",
            "    tag=ZONE value=PLAY",
            "    tag=CONTROLLER value=2",
            "    tag=CARDTYPE value=LOCATION",
        ];
        let packets = PowerLogParser::parse_records(
            payloads
                .iter()
                .enumerate()
                .map(|(index, payload)| power_record(index as u64, payload)),
        );
        let mut normalizer = PowerEventNormalizer::new();
        let events: Vec<_> = packets
            .iter()
            .flat_map(|packet| normalizer.normalize(packet).expect("normalize"))
            .collect();
        let state = events
            .iter()
            .fold(ObservedGameState::default(), |state, event| {
                reduce(&state, event).expect("reduce")
            });

        let location = state.entities.get(&EntityId(25)).expect("location");
        assert_eq!(location.tags.get(&game_tag::CARDTYPE).copied(), Some(39));
        assert_eq!(location.card_type(), Some(CardType::Location));
        assert_eq!(derive(&state, None).counters.opposing.locations, 1);
    }

    #[test]
    fn active_deck_rows_passthrough_and_observed_rows_aggregate() {
        let deck = DeckList {
            format: FormatType::Standard,
            heroes: vec![DbfId(7)],
            cards: vec![(DbfId(100), 2), (DbfId(200), 1)],
            sideboards: Vec::new(),
        };
        let state = with_entities(vec![
            entity(10, Some("CS2_029"), 1, Zone::Hand, CARDTYPE_SPELL, &[]),
            entity(11, Some("CS2_029"), 1, Zone::Play, CARDTYPE_SPELL, &[]),
            entity(
                12,
                Some("EX1_tk"),
                1,
                Zone::Hand,
                CARDTYPE_MINION,
                &[(tag_id("CREATOR"), 99)],
            ),
            entity(
                13,
                Some("SHUF_01"),
                1,
                Zone::Deck,
                CARDTYPE_SPELL,
                &[(tag_id("CREATOR_DBID"), 55)],
            ),
            entity(
                14,
                Some("NEW1_007"),
                1,
                Zone::Play,
                CARDTYPE_MINION,
                &[(tag_id("TRANSFORMED_FROM_CARD"), 8)],
            ),
        ]);

        let view = derive(&state, Some(&deck));
        assert_eq!(view.my_deck.len(), 6);
        assert_eq!(view.my_deck[0].dbf_id, Some(DbfId(100)));
        assert_eq!(view.my_deck[0].card_id, None);
        assert_eq!(view.my_deck[0].count_initial, 2);
        assert_eq!(view.my_deck[0].count_remaining, 2);
        assert_eq!(view.my_deck[1].dbf_id, Some(DbfId(200)));

        let fireball = view
            .my_deck
            .iter()
            .find(|row| row.card_id.as_ref().map(CardId::as_str) == Some("CS2_029"))
            .unwrap();
        assert_eq!(fireball.dbf_id, None);
        assert_eq!(fireball.origin, CardOrigin::OriginalDeck);
        assert_eq!(fireball.count_initial, 2);
        assert_eq!(fireball.count_remaining, 0);
        assert_eq!(fireball.count_drawn, 2);
        assert_eq!(fireball.count_played, 1);

        let created = view
            .my_deck
            .iter()
            .find(|row| row.card_id.as_ref().map(CardId::as_str) == Some("EX1_tk"))
            .unwrap();
        assert_eq!(created.origin, CardOrigin::Created);
        assert_eq!(created.count_initial, 0);
        assert_eq!(created.count_drawn, 1);

        let shuffled = view
            .my_deck
            .iter()
            .find(|row| row.card_id.as_ref().map(CardId::as_str) == Some("SHUF_01"))
            .unwrap();
        assert_eq!(shuffled.origin, CardOrigin::Shuffled);
        assert_eq!(shuffled.count_remaining, 1);

        let transformed = view
            .my_deck
            .iter()
            .find(|row| row.card_id.as_ref().map(CardId::as_str) == Some("NEW1_007"))
            .unwrap();
        assert_eq!(transformed.origin, CardOrigin::Transformed);
    }

    #[test]
    fn opponent_hand_played_graveyard_and_counters() {
        let state = with_entities(vec![
            entity(
                20,
                None,
                2,
                Zone::Hand,
                CARDTYPE_MINION,
                &[(tag_id("NUM_TURNS_IN_HAND"), 1)],
            ),
            entity(21, Some("CS2_200"), 2, Zone::Play, CARDTYPE_MINION, &[]),
            entity(
                22,
                Some("EX1_tk"),
                2,
                Zone::Graveyard,
                CARDTYPE_MINION,
                &[(tag_id("CREATOR"), 21)],
            ),
            entity(23, Some("CS2_037"), 1, Zone::Graveyard, CARDTYPE_SPELL, &[]),
            entity(24, Some("CS2_091"), 1, Zone::Play, CARDTYPE_WEAPON, &[]),
            entity(
                25,
                Some("VAC_LOC"),
                2,
                Zone::Play,
                game_tag::card_type::LOCATION,
                &[],
            ),
            entity(
                26,
                Some("SECRET_01"),
                2,
                Zone::Secret,
                CARDTYPE_SPELL,
                &[(tag_id("SECRET"), 1)],
            ),
            entity(
                27,
                Some("QUEST_01"),
                1,
                Zone::Secret,
                CARDTYPE_SPELL,
                &[(tag_id("QUEST"), 1)],
            ),
        ]);
        let view = derive(&state, None);
        assert_eq!(view.opponent_hand.len(), 1);
        assert_eq!(view.opponent_hand[0].entity_id, EntityId(20));
        assert_eq!(view.opponent_hand[0].since_turn, 2);
        assert_eq!(view.opponent_hand[0].known_card, None);

        assert_eq!(view.opponent_played.len(), 4);
        assert!(view.opponent_played.iter().any(|row| {
            row.card_id.as_ref().map(CardId::as_str) == Some("EX1_tk") && row.created
        }));

        assert_eq!(view.graveyards[0].len(), 1);
        assert_eq!(view.graveyards[1].len(), 1);
        assert_eq!(view.counters.friendly.weapons, 1);
        assert_eq!(view.counters.friendly.quests, 1);
        assert_eq!(view.counters.friendly.fatigue, 2);
        assert_eq!(view.counters.opposing.locations, 1);
        assert_eq!(view.counters.opposing.secrets, 1);
    }

    #[test]
    fn derive_is_deterministic_on_synthetic_state() {
        let state = with_entities(vec![entity(
            10,
            Some("CS2_029"),
            1,
            Zone::Hand,
            CARDTYPE_SPELL,
            &[],
        )]);
        let a = derive(&state, None);
        let b = derive(&state, None);
        let c = derive(&state, None);
        assert_eq!(a, b);
        assert_eq!(b, c);
    }
}
