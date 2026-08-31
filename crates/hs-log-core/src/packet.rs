//! 协议 Packet 骨架（设计文档 §7.3 首版覆盖清单）。
//! Track C 实装 Parser 时填充字段；`#[non_exhaustive]` 允许无痛扩列。

use hs_log_record::RawLogRecord;
use hs_types::{CardId, EntityId, PlayerId};
use serde::{Deserialize, Serialize};

/// 服务器对 Options 节点给出的可用性结论。
///
/// 旧版日志没有 `error` 字段，此时只能保守地保留为
/// [`LegacyUnspecified`](Self::LegacyUnspecified)，不能误判为受阻。
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum OptionAvailability {
    Allowed,
    #[default]
    LegacyUnspecified,
    Blocked,
}

impl OptionAvailability {
    /// 当前节点是否可以进入候选动作集合。
    pub const fn is_selectable(self) -> bool {
        matches!(self, Self::Allowed | Self::LegacyUnspecified)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Packet {
    CreateGame(CreateGamePacket),
    FullEntity(FullEntityPacket),
    ShowEntity(ShowEntityPacket),
    ChangeEntity(ChangeEntityPacket),
    HideEntity(HideEntityPacket),
    TagChange(TagChangePacket),
    BlockStart(BlockStartPacket),
    BlockEnd(BlockEndPacket),
    Options(OptionsPacket),
    Choices(ChoicesPacket),
    ChosenEntities(ChosenEntitiesPacket),
    SendOption(SendOptionPacket),
    SendChoices(SendChoicesPacket),
    /// `GameState.DebugPrintGame()`：build 号 / 对局模式 / 玩家名（逐行一包）。
    GameMetadata(GameMetadataPacket),
    /// 未识别行：保留完整原始记录（禁止丢弃）。
    Unknown(UnknownPacket),
}

impl Packet {
    /// 组成该 Packet 的原始日志行（多行 Packet 含全部 continuation）。
    pub fn source_records(&self) -> Vec<&RawLogRecord> {
        match self {
            Packet::CreateGame(packet) => packet.records.iter().collect(),
            Packet::FullEntity(packet) => packet.records.iter().collect(),
            Packet::ShowEntity(packet) => packet.records.iter().collect(),
            Packet::ChangeEntity(packet) => packet.records.iter().collect(),
            Packet::HideEntity(packet) => packet.records.iter().collect(),
            Packet::TagChange(packet) => packet.record.as_ref().into_iter().collect(),
            Packet::BlockStart(packet) => packet.records.iter().collect(),
            Packet::BlockEnd(packet) => packet.records.iter().collect(),
            Packet::Options(packet) => packet.records.iter().collect(),
            Packet::Choices(packet) => packet.records.iter().collect(),
            Packet::ChosenEntities(packet) => packet.records.iter().collect(),
            Packet::SendOption(packet) => packet.records.iter().collect(),
            Packet::SendChoices(packet) => packet.records.iter().collect(),
            Packet::GameMetadata(packet) => packet.records.iter().collect(),
            Packet::Unknown(packet) => vec![&packet.record],
        }
    }
}

/// `SHOW_ENTITY`：隐藏实体揭示为具体卡（M1 契约冻结，S2 实装解析）。
///
/// `entity_id` 解析失败时为 `None`，原文留在 `entity_raw`（与 [`BlockStartPacket`] 一致）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ShowEntityPacket {
    pub entity_id: Option<EntityId>,
    #[serde(default)]
    pub entity_raw: String,
    pub card_id: Option<CardId>,
    pub tags: Vec<RawTag>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

/// `CHANGE_ENTITY`：实体整体变换（如变形）。
///
/// `entity_id` 解析失败时为 `None`，原文留在 `entity_raw`（与 [`BlockStartPacket`] 一致）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChangeEntityPacket {
    pub entity_id: Option<EntityId>,
    #[serde(default)]
    pub entity_raw: String,
    pub card_id: Option<CardId>,
    pub tags: Vec<RawTag>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

/// `HIDE_ENTITY`：实体回到隐藏区（zone 为原始 tag 数值）。
///
/// `entity_id` 解析失败时为 `None`，原文留在 `entity_raw`（与 [`BlockStartPacket`] 一致）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HideEntityPacket {
    pub entity_id: Option<EntityId>,
    #[serde(default)]
    pub entity_raw: String,
    pub zone: Option<i32>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

/// `BLOCK_START`。source/target 解析失败时保留原文于 `*_raw`。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BlockStartPacket {
    pub block_type: String,
    pub source: Option<EntityId>,
    #[serde(default)]
    pub source_raw: String,
    pub target: Option<EntityId>,
    #[serde(default)]
    pub target_raw: String,
    pub effect_card_id: Option<CardId>,
    pub effect_index: Option<i32>,
    pub sub_option: Option<i32>,
    pub trigger_keyword: Option<String>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BlockEndPacket {
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

/// 单个可选项（`option N type=... entityName=...`；target/sub 为子行）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OptionEntry {
    pub index: u32,
    pub option_type: String,
    pub entity: Option<EntityId>,
    #[serde(default)]
    pub entity_raw: String,
    pub error: Option<String>,
    /// 标准化后的可用性；旧序列化数据默认按未声明处理。
    #[serde(default)]
    pub availability: OptionAvailability,
    /// 日志中 `error=` 的原始值。`Some("")` 与字段缺失不同。
    #[serde(default)]
    pub raw_error: Option<String>,
    /// 日志中 `errorParam=` 的原始值，显式空值保留为 `Some("")`。
    #[serde(default)]
    pub error_param: Option<String>,
    #[serde(default)]
    pub targets: Vec<EntityId>,
    /// target 的无损节点；[`Self::targets`] 只能从这里投影。
    #[serde(default)]
    pub target_entries: Vec<OptionTarget>,
    #[serde(default)]
    pub sub_options: Vec<OptionEntry>,
}

/// Options 中单个 target 子行的无损协议节点。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OptionTarget {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub entity: Option<EntityId>,
    #[serde(default)]
    pub entity_raw: String,
    /// 兼容旧调用方的标准化错误；`NONE`、空值和缺失均为 `None`。
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub availability: OptionAvailability,
    #[serde(default)]
    pub raw_error: Option<String>,
    #[serde(default)]
    pub error_param: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OptionsPacket {
    pub id: u64,
    pub options: Vec<OptionEntry>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChoicesPacket {
    pub id: u64,
    #[serde(default)]
    pub task_list: Option<u64>,
    pub player_entity: Option<EntityId>,
    #[serde(default)]
    pub player_raw: String,
    pub choice_type: String,
    pub min: u32,
    pub max: u32,
    pub source: Option<EntityId>,
    #[serde(default)]
    pub source_raw: String,
    #[serde(default)]
    pub entities: Vec<EntityId>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChosenEntitiesPacket {
    pub id: u64,
    #[serde(default)]
    pub task_list: Option<u64>,
    #[serde(default)]
    pub player_entity: Option<EntityId>,
    #[serde(default)]
    pub player_raw: String,
    #[serde(default)]
    pub source: Option<EntityId>,
    #[serde(default)]
    pub source_raw: String,
    #[serde(default)]
    pub entities: Vec<EntityId>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SendOptionPacket {
    pub id: u64,
    pub option: i32,
    pub sub_option: i32,
    pub target: i32,
    pub position: i32,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SendChoicesPacket {
    pub id: u64,
    #[serde(default)]
    pub task_list: Option<u64>,
    #[serde(default)]
    pub player_entity: Option<EntityId>,
    #[serde(default)]
    pub player_raw: String,
    #[serde(default)]
    pub choice_type: String,
    #[serde(default)]
    pub source: Option<EntityId>,
    #[serde(default)]
    pub source_raw: String,
    #[serde(default)]
    pub entities: Vec<EntityId>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

/// Power.log 中的 `tag=NAME value=VALUE` 原始对。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RawTag {
    pub name: String,
    pub value: String,
}

/// `CREATE_GAME` 下的 `GameEntity` 子块。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CreateGameEntity {
    pub entity_id: EntityId,
    pub tags: Vec<RawTag>,
}

impl Default for CreateGameEntity {
    fn default() -> Self {
        Self {
            entity_id: EntityId(0),
            tags: Vec::new(),
        }
    }
}

/// `CREATE_GAME` 下的 `Player` 子块。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CreateGamePlayer {
    pub entity_id: EntityId,
    pub player_id: PlayerId,
    pub account_hi: Option<u64>,
    pub account_lo: Option<u64>,
    pub tags: Vec<RawTag>,
}

impl Default for CreateGamePlayer {
    fn default() -> Self {
        Self {
            entity_id: EntityId(0),
            player_id: PlayerId(0),
            account_hi: None,
            account_lo: None,
            tags: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CreateGamePacket {
    pub game_entity: Option<CreateGameEntity>,
    pub players: Vec<CreateGamePlayer>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FullEntityPacket {
    pub entity_id: EntityId,
    pub card_id: Option<CardId>,
    pub tags: Vec<RawTag>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

impl Default for FullEntityPacket {
    fn default() -> Self {
        Self {
            entity_id: EntityId(0),
            card_id: None,
            tags: Vec::new(),
            records: Vec::new(),
        }
    }
}

/// TAG_CHANGE 是首条纵向链路的最小载荷，G0 即定型字段。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TagChangePacket {
    pub entity: EntityId,
    pub tag: i32,
    pub value: i32,
    #[serde(default)]
    pub tag_name: String,
    #[serde(default)]
    pub value_raw: String,
    #[serde(default)]
    pub entity_raw: String,
    #[serde(default)]
    pub record: Option<RawLogRecord>,
}

impl Default for TagChangePacket {
    fn default() -> Self {
        Self {
            entity: EntityId(0),
            tag: 0,
            value: 0,
            tag_name: String::new(),
            value_raw: String::new(),
            entity_raw: String::new(),
            record: None,
        }
    }
}

/// `GameState.DebugPrintGame()` 单行元数据。字段按行内容择一填充：
/// `BuildNumber=` / `GameType=` / `FormatType=` / `ScenarioID=` /
/// `PlayerID=N, PlayerName=X`。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GameMetadataPacket {
    pub build_number: Option<u32>,
    pub game_type: Option<String>,
    pub format_type: Option<String>,
    pub scenario_id: Option<i64>,
    pub player_id: Option<i32>,
    pub player_name: Option<String>,
    #[serde(default)]
    pub records: Vec<RawLogRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UnknownPacket {
    pub record: RawLogRecord,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn availability_default_and_selectability_are_stable() {
        assert_eq!(
            OptionAvailability::default(),
            OptionAvailability::LegacyUnspecified
        );
        assert!(OptionAvailability::Allowed.is_selectable());
        assert!(OptionAvailability::LegacyUnspecified.is_selectable());
        assert!(!OptionAvailability::Blocked.is_selectable());
    }

    #[test]
    fn new_decision_packet_fields_default_when_reading_legacy_json() {
        let option: OptionEntry = serde_json::from_value(json!({
            "index": 1,
            "option_type": "POWER",
            "entity": 4,
            "error": null,
            "targets": [6],
            "sub_options": []
        }))
        .unwrap();
        assert_eq!(option.availability, OptionAvailability::LegacyUnspecified);
        assert_eq!(option.raw_error, None);
        assert_eq!(option.error_param, None);
        assert!(option.target_entries.is_empty());

        let choices: ChoicesPacket = serde_json::from_value(json!({
            "id": 1,
            "player_entity": 2,
            "player_raw": "2",
            "choice_type": "MULLIGAN",
            "min": 0,
            "max": 3,
            "source": 1,
            "entities": [12],
            "records": []
        }))
        .unwrap();
        assert_eq!(choices.task_list, None);
        assert_eq!(choices.source_raw, "");

        let chosen: ChosenEntitiesPacket = serde_json::from_value(json!({
            "id": 1,
            "player_raw": "Anduin",
            "entities": [12],
            "records": []
        }))
        .unwrap();
        assert_eq!(chosen.task_list, None);
        assert_eq!(chosen.player_entity, None);
        assert_eq!(chosen.source, None);

        let sent: SendChoicesPacket = serde_json::from_value(json!({
            "id": 1,
            "choice_type": "MULLIGAN",
            "entities": [12],
            "records": []
        }))
        .unwrap();
        assert_eq!(sent.task_list, None);
        assert_eq!(sent.player_raw, "");
        assert_eq!(sent.source_raw, "");
    }
}
