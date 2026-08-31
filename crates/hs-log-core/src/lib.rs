//! `hs-log-core` — 协议层（Packet）与领域事件层（DomainEvent）。
//!
//! 分层约束（设计文档 §7.3/§7.4）：
//! - Parser 只输出 Packet，不修改业务状态；
//! - 日志文本格式变化由 Normalizer 吸收，Reducer 只见 DomainEvent；
//! - 未识别行必须保留为 `Packet::Unknown`，不得丢弃。
//!
//! G0 状态：类型骨架已冻结为契约；字段随 Track C/D 实装扩充
//! （所有 payload 结构标记 `#[non_exhaustive]`，新增字段不属于破坏性变更）。

pub mod decks_log;
pub mod game_tag;
mod game_tag_table;
mod game_tag_value_table;
pub mod normalizer;
pub mod packet;
pub mod parser;

pub use normalizer::PowerEventNormalizer;
pub use packet::{
    BlockEndPacket, BlockStartPacket, ChangeEntityPacket, ChoicesPacket, ChosenEntitiesPacket,
    CreateGameEntity, CreateGamePacket, CreateGamePlayer, FullEntityPacket, GameMetadataPacket,
    HideEntityPacket, OptionAvailability, OptionEntry, OptionTarget, OptionsPacket, Packet, RawTag,
    SendChoicesPacket, SendOptionPacket, ShowEntityPacket, TagChangePacket, UnknownPacket,
};
pub use parser::PowerLogParser;

use hs_types::{BuildId, EntityId, GameId, PlayerId, Zone};
use serde::{Deserialize, Serialize};

/// 领域事件：Reducer 的唯一输入（设计文档 §7.4）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DomainEvent {
    GameCreated {
        game_id: GameId,
        build_id: Option<BuildId>,
    },
    EntityCreated {
        entity_id: EntityId,
        card_id: Option<hs_types::CardId>,
        zone: Zone,
    },
    EntityRevealed {
        entity_id: EntityId,
        card_id: hs_types::CardId,
    },
    TagChanged {
        entity_id: EntityId,
        tag: TagKey,
        value: i32,
    },
    /// 实体整体变换（CHANGE_ENTITY，如变形术后的新卡）。
    EntityChanged {
        entity_id: EntityId,
        card_id: Option<hs_types::CardId>,
    },
    /// M1 约定：Normalizer 无状态、不派生 from/to，区域变化经
    /// `TagChanged(ZONE)` 传递，由 Reducer 更新 zone。本变体保留给
    /// 未来带位置语义的归一化（不在 M1 发射）。
    ZoneChanged {
        entity_id: EntityId,
        from: Zone,
        to: Zone,
    },
    BlockOpened {
        block_type: String,
        source: Option<EntityId>,
        target: Option<EntityId>,
        effect_card_id: Option<hs_types::CardId>,
    },
    BlockClosed,
    /// 服务器下发的合法可选项全量更新。
    OptionsUpdated {
        id: u64,
        options: Vec<OptionSummary>,
    },
    /// 服务器下发的待抉择（调度/发现等）。
    ChoicesUpdated {
        id: u64,
        #[serde(default)]
        task_list: Option<u64>,
        player: Option<PlayerId>,
        #[serde(default)]
        player_entity: Option<EntityId>,
        #[serde(default)]
        player_raw: String,
        choice_type: String,
        min: u32,
        max: u32,
        source: Option<EntityId>,
        #[serde(default)]
        source_raw: String,
        entities: Vec<EntityId>,
    },
    /// 服务器确认的抉择结果（CHOSEN_ENTITIES）。
    ChoicesResolved {
        id: u64,
        #[serde(default)]
        task_list: Option<u64>,
        #[serde(default)]
        player_entity: Option<EntityId>,
        #[serde(default)]
        player_raw: String,
        #[serde(default)]
        source: Option<EntityId>,
        #[serde(default)]
        source_raw: String,
        entities: Vec<EntityId>,
    },
    /// 客户端提交的抉择（SEND_CHOICES）。
    ChoicesSent {
        id: u64,
        #[serde(default)]
        task_list: Option<u64>,
        #[serde(default)]
        player_entity: Option<EntityId>,
        #[serde(default)]
        player_raw: String,
        choice_type: String,
        #[serde(default)]
        source: Option<EntityId>,
        #[serde(default)]
        source_raw: String,
        entities: Vec<EntityId>,
    },
    /// 客户端提交的操作（SEND_OPTION）。
    OptionChosen {
        id: u64,
        option: i32,
        sub_option: i32,
        target: i32,
        position: i32,
    },
    GameEnded {
        winner: Option<PlayerId>,
    },
    /// 客户端 build 号确定（来自 GameState.DebugPrintGame 的 BuildNumber）。
    BuildIdentified {
        build_id: BuildId,
    },
    /// 友方玩家判定（hslog FriendlyPlayerExporter 规则）：
    /// 第一个 ZONE=HAND 的 SHOW_ENTITY 归属友方；单人模式下唯一非 AI
    /// 玩家即友方。由 Normalizer 发射,每局至多一次。
    FriendlyPlayerDetected {
        player: PlayerId,
    },
    /// 协议层警告（未识别/异常行），必须向上传递而非静默吞掉。
    ProtocolWarning {
        message: String,
    },
}

/// Options 中单个可选项的无损领域投影。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OptionSummary {
    pub index: u32,
    pub option_type: String,
    pub entity: Option<EntityId>,
    #[serde(default)]
    pub entity_raw: String,
    pub error: Option<String>,
    #[serde(default)]
    pub availability: OptionAvailability,
    #[serde(default)]
    pub raw_error: Option<String>,
    #[serde(default)]
    pub error_param: Option<String>,
    /// 兼容旧调用方；只从 [`Self::target_entries`] 投影。
    #[serde(default)]
    pub targets: Vec<EntityId>,
    #[serde(default)]
    pub target_entries: Vec<OptionTargetSummary>,
    #[serde(default)]
    pub sub_options: Vec<OptionSummary>,
}

/// Options target 子行的无损领域投影。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OptionTargetSummary {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub entity: Option<EntityId>,
    #[serde(default)]
    pub entity_raw: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub availability: OptionAvailability,
    #[serde(default)]
    pub raw_error: Option<String>,
    #[serde(default)]
    pub error_param: Option<String>,
}

/// GameTag 键。数值与 [`game_tag`] 命名常量对齐。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TagKey(pub i32);

impl TagKey {
    pub const PLAYSTATE: Self = Self(game_tag::PLAYSTATE);
    pub const STEP: Self = Self(game_tag::STEP);
    pub const TURN: Self = Self(game_tag::TURN);
    pub const CURRENT_PLAYER: Self = Self(game_tag::CURRENT_PLAYER);
    pub const PLAYER_ID: Self = Self(game_tag::PLAYER_ID);
    pub const DAMAGE: Self = Self(game_tag::DAMAGE);
    pub const HEALTH: Self = Self(game_tag::HEALTH);
    pub const ATK: Self = Self(game_tag::ATK);
    pub const COST: Self = Self(game_tag::COST);
    pub const ZONE: Self = Self(game_tag::ZONE);
    pub const CONTROLLER: Self = Self(game_tag::CONTROLLER);
    pub const ENTITY_ID: Self = Self(game_tag::ENTITY_ID);
    pub const CARDTYPE: Self = Self(game_tag::CARDTYPE);
    pub const ZONE_POSITION: Self = Self(game_tag::ZONE_POSITION);
}

#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("packet not yet supported by normalizer: {0}")]
    Unsupported(&'static str),
}

/// Packet → 领域事件归一化契约（一个 Packet 可产出 0..n 个事件）。
pub trait EventNormalizer {
    fn normalize(&mut self, packet: &Packet) -> Result<Vec<DomainEvent>, NormalizeError>;
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_domain_decision_fields_default_when_reading_legacy_json() {
        let option: OptionSummary = serde_json::from_value(json!({
            "index": 1,
            "option_type": "POWER",
            "entity": 4,
            "error": null,
            "targets": [6]
        }))
        .unwrap();
        assert_eq!(option.availability, OptionAvailability::LegacyUnspecified);
        assert!(option.target_entries.is_empty());
        assert!(option.sub_options.is_empty());

        let event: DomainEvent = serde_json::from_value(json!({
            "ChoicesUpdated": {
                "id": 1,
                "player": 1,
                "choice_type": "MULLIGAN",
                "min": 0,
                "max": 3,
                "source": 1,
                "entities": [12]
            }
        }))
        .unwrap();
        assert!(matches!(
            event,
            DomainEvent::ChoicesUpdated {
                task_list: None,
                player_entity: None,
                ref player_raw,
                ref source_raw,
                ..
            } if player_raw.is_empty() && source_raw.is_empty()
        ));
    }
}

#[cfg(test)]
mod golden;
