//! 观察状态类型（设计文档 §8.2 / §8.3）。
//!
//! 容器一律用 `BTreeMap` / `Vec`，避免 `HashMap` 迭代序破坏重放确定性。

use hs_log_core::{game_tag, OptionSummary};
use hs_types::{BuildId, CardId, CardType, EntityId, GameId, GamePhase, PlayerId, Zone};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// 警告文本最多保留条数；超出只累加 [`ObservedGameState::warning_count`]。
pub const MAX_STORED_WARNINGS: usize = 100;

/// 单实体状态（设计文档 §8.2）。派生访问器只读 tags，不覆盖原始 Tag。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EntityState {
    pub entity_id: EntityId,
    pub card_id: Option<CardId>,
    pub controller: Option<PlayerId>,
    pub zone: Zone,
    pub zone_position: Option<u8>,
    /// 原始 GameTag（键为 tag 数值）。`BTreeMap` 保证序列化与 Debug 顺序稳定。
    pub tags: BTreeMap<i32, i32>,
    pub revealed: bool,
    /// 创建来源实体（M1 Reducer 不填充，保留给后续 BLOCK/附魔链路）。
    pub source_entity: Option<EntityId>,
    /// 附属于本实体的附魔实体 ID 列表（M1 Reducer 不填充）。
    pub enchantments: Vec<EntityId>,
}

impl EntityState {
    pub fn new(entity_id: EntityId, card_id: Option<CardId>, zone: Zone) -> Self {
        Self {
            entity_id,
            revealed: card_id.is_some(),
            card_id,
            controller: None,
            zone,
            zone_position: None,
            tags: BTreeMap::new(),
            source_entity: None,
            enchantments: Vec::new(),
        }
    }

    fn tag_or_zero(&self, tag: i32) -> i32 {
        self.tags.get(&tag).copied().unwrap_or(0)
    }

    fn named_tag(&self, name: &str) -> Option<i32> {
        self.tags.get(&game_tag::id_by_name(name)).copied()
    }

    fn named_tag_is_set(&self, name: &str) -> bool {
        self.named_tag(name).is_some_and(|value| value != 0)
    }

    /// 已观测到的 `ATK` 原值；`None` 表示 tag 缺失，和真实的 0 区分。
    pub fn observed_attack(&self) -> Option<i32> {
        self.tags.get(&game_tag::ATK).copied()
    }

    /// `ATK` tag 原值；缺失视为 0。
    pub fn attack(&self) -> i32 {
        self.observed_attack().unwrap_or(0)
    }

    /// 已观测到的 `HEALTH` 原值；`None` 表示 tag 缺失，和真实的 0 区分。
    pub fn observed_health(&self) -> Option<i32> {
        self.tags.get(&game_tag::HEALTH).copied()
    }

    /// `HEALTH` tag 原值（最大生命，不含伤害扣减）；缺失视为 0。
    ///
    /// 当前生命请用 [`current_health`](Self::current_health)（`health - damage`）。
    pub fn health(&self) -> i32 {
        self.observed_health().unwrap_or(0)
    }

    /// `DAMAGE` tag 原值；缺失视为 0。
    pub fn damage(&self) -> i32 {
        self.tag_or_zero(game_tag::DAMAGE)
    }

    /// `COST` tag 原值；缺失视为 0。
    pub fn cost(&self) -> i32 {
        self.observed_cost().unwrap_or(0)
    }

    /// 已观测到的 `COST` 原值；`None` 表示 tag 缺失，和真实的 0 区分。
    pub fn observed_cost(&self) -> Option<i32> {
        self.tags.get(&game_tag::COST).copied()
    }

    /// `SILENCED` tag 非 0 即为沉默。
    pub fn is_silenced(&self) -> bool {
        self.tag_or_zero(game_tag::SILENCED) != 0
    }

    /// 已观测到的武器/Location 耐久；缺失时不把它猜成 0。
    pub fn observed_durability(&self) -> Option<i32> {
        self.named_tag("DURABILITY")
    }

    /// 英雄护甲；tag 缺失时按既有投影口径返回 0。
    pub fn armor(&self) -> i32 {
        self.named_tag("ARMOR").unwrap_or(0)
    }

    /// 已观测到的待偿还过载；玩家实体和英雄实体的优先级由调用方决定。
    pub fn observed_overload_owed(&self) -> Option<i32> {
        self.named_tag("OVERLOAD_OWED")
    }

    /// 本回合已攻击次数；tag 缺失表示尚未攻击，返回 0。
    pub fn attacks_this_turn(&self) -> i32 {
        self.named_tag("NUM_ATTACKS_THIS_TURN").unwrap_or(0)
    }

    pub fn is_exhausted(&self) -> bool {
        self.named_tag_is_set("EXHAUSTED")
    }

    pub fn is_frozen(&self) -> bool {
        self.named_tag_is_set("FROZEN")
    }

    pub fn has_taunt(&self) -> bool {
        self.named_tag_is_set("TAUNT")
    }

    pub fn has_stealth(&self) -> bool {
        self.named_tag_is_set("STEALTH")
    }

    pub fn location_action_cooldown(&self) -> i32 {
        self.named_tag("LOCATION_ACTION_COOLDOWN").unwrap_or(0)
    }

    pub fn location_action_cost(&self) -> i32 {
        self.named_tag("LOCATION_ACTION_COST").unwrap_or(0)
    }

    pub fn has_windfury(&self) -> bool {
        self.named_tag_is_set("WINDFURY")
    }

    pub fn has_mega_windfury(&self) -> bool {
        self.named_tag_is_set("MEGA_WINDFURY")
    }

    pub fn has_divine_shield(&self) -> bool {
        self.named_tag_is_set("DIVINE_SHIELD")
    }

    pub fn has_poisonous(&self) -> bool {
        self.named_tag_is_set("POISONOUS")
    }

    pub fn has_lifesteal(&self) -> bool {
        self.named_tag_is_set("LIFESTEAL")
    }

    pub fn is_immune(&self) -> bool {
        self.named_tag_is_set("IMMUNE")
    }

    pub fn is_immune_while_attacking(&self) -> bool {
        self.named_tag_is_set("IMMUNE_WHILE_ATTACKING")
    }

    pub fn has_charge(&self) -> bool {
        self.named_tag_is_set("CHARGE")
    }

    pub fn has_rush(&self) -> bool {
        self.named_tag_is_set("RUSH")
    }

    pub fn cannot_attack(&self) -> bool {
        self.named_tag_is_set("CANT_ATTACK")
    }

    pub fn cannot_be_attacked(&self) -> bool {
        self.named_tag_is_set("CANT_BE_ATTACKED")
    }

    pub fn cannot_attack_heroes(&self) -> bool {
        self.named_tag_is_set("CANNOT_ATTACK_HEROES")
    }

    /// `CARDTYPE` tag 的领域映射；tag 缺失返回 `None`（未观测，不猜测）。
    ///
    /// 数值取 `game_tag::card_type::*` —— **必须与 `value_by_name` 写入
    /// tags 时用的是同一套常量**，否则两侧值空间会漂移。未识别的值走
    /// `CardType::Unknown(raw)` 显式保留，不塌缩。
    pub fn card_type(&self) -> Option<CardType> {
        let raw = self.tags.get(&game_tag::CARDTYPE).copied()?;
        Some(match raw {
            game_tag::card_type::GAME => CardType::Game,
            game_tag::card_type::PLAYER => CardType::Player,
            game_tag::card_type::HERO => CardType::Hero,
            game_tag::card_type::MINION => CardType::Minion,
            game_tag::card_type::SPELL => CardType::Spell,
            game_tag::card_type::ENCHANTMENT => CardType::Enchantment,
            game_tag::card_type::WEAPON => CardType::Weapon,
            game_tag::card_type::HERO_POWER => CardType::HeroPower,
            game_tag::card_type::LOCATION => CardType::Location,
            game_tag::card_type::INVALID => CardType::Invalid,
            other => CardType::Unknown(other),
        })
    }

    /// 当前生命 = `HEALTH` 原值 − `DAMAGE` 原值。
    pub fn current_health(&self) -> i32 {
        self.health() - self.damage()
    }
}

/// 玩家投影。`ObservedGameState.players` 以 `PlayerId.0` 为键
///（`PlayerId` 尚未派生 `Ord`，故不用 `BTreeMap<PlayerId, _>`）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PlayerState {
    pub player_id: PlayerId,
    pub entity_id: EntityId,
    pub playstate: Option<i32>,
    pub resources: Option<i32>,
    pub resources_used: Option<i32>,
    /// `HERO_ENTITY` tag，便于 Inspector 关联英雄。
    pub hero_entity: Option<EntityId>,
}

/// 一层 BLOCK 调用帧。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BlockFrame {
    pub block_type: String,
    pub source: Option<EntityId>,
    pub target: Option<EntityId>,
    pub effect_card_id: Option<CardId>,
}

/// 当前服务器下发的 Options 全量快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OptionsState {
    pub id: u64,
    /// 这份 Options 进入观察状态时的 revision；后续状态变化不会改写它。
    #[serde(default)]
    pub issued_revision: u64,
    pub options: Vec<OptionSummary>,
}

/// 一份尚未被客户端提交或服务器确认消费的 Choices 全量快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChoicesState {
    pub id: u64,
    #[serde(default)]
    pub task_list: Option<u64>,
    pub player: Option<PlayerId>,
    #[serde(default)]
    pub player_entity: Option<EntityId>,
    #[serde(default)]
    pub player_raw: String,
    pub source: Option<EntityId>,
    #[serde(default)]
    pub source_raw: String,
    /// 这份 Choices 进入观察状态时的 revision；后续状态变化不会改写它。
    #[serde(default)]
    pub issued_revision: u64,
    pub choice_type: String,
    pub min: u32,
    pub max: u32,
    pub entities: Vec<EntityId>,
}

/// 已提交 Choice 的短期回声身份；`sent_revision` 区分提交时已经并存的
/// sibling 与提交后才复用同 id 的新决策。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SentChoiceEcho {
    pub choice: ChoicesState,
    pub sent_revision: u64,
}

/// 观察状态快照（设计文档 §8.3）。`Clone` 即快照。
///
/// 相对 §8.3 草稿：`build_id`/`game_id` 保持 `Option`（对局开始前未知）；
/// `entities`/`players`/`public_counters` 用 `BTreeMap` 替代 `HashMap`；
/// `warnings` 仍为 `Vec<String>`（未引入 `StateWarning` 新类型）；
/// 增补 `winner` 与 `warning_count`。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ObservedGameState {
    pub build_id: Option<BuildId>,
    pub game_id: Option<GameId>,
    pub phase: GamePhase,
    pub turn: u32,
    pub current_player: Option<PlayerId>,
    pub friendly_player: Option<PlayerId>,
    /// 对局胜者；`GameEnded` 或玩家 `PLAYSTATE=WON` 时写入。
    pub winner: Option<PlayerId>,
    pub entities: BTreeMap<EntityId, EntityState>,
    /// 键为 `PlayerId.0`。建议 hs-types 为 `PlayerId` 派生 `Ord` 后改回强类型键。
    pub players: BTreeMap<i32, PlayerState>,
    pub block_stack: Vec<BlockFrame>,
    pub options: Option<OptionsState>,
    /// 所有尚未消费的 Choices。双方 mulligan 等并行抉择不能互相覆盖。
    #[serde(default, deserialize_with = "deserialize_choices")]
    pub choices: Vec<ChoicesState>,
    /// 已由 `ChoicesSent` 消费、等待随后 `ChoicesResolved` 回声的快照。
    ///
    /// 回声必须先在这里匹配，避免同 id 的另一个 pending Choice 被二次消费。
    #[serde(default)]
    pub sent_choice_echoes: Vec<SentChoiceEcho>,
    /// M1 占位计数器，Reducer 暂不写入；键序稳定以便 hash。
    pub public_counters: BTreeMap<String, i64>,
    pub warnings: Vec<String>,
    /// 累计警告条数（含因封顶未入库的文本）。
    pub warning_count: u64,
    /// 每次成功 reduce 递增；UI 与建议系统以此判断状态新鲜度。
    pub revision: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ChoicesWireFormat {
    Many(Vec<ChoicesState>),
    One(ChoicesState),
}

fn deserialize_choices<'de, D>(deserializer: D) -> Result<Vec<ChoicesState>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        match Option::<ChoicesWireFormat>::deserialize(deserializer)? {
            None => Vec::new(),
            Some(ChoicesWireFormat::Many(choices)) => choices,
            Some(ChoicesWireFormat::One(choice)) => vec![choice],
        },
    )
}

impl ObservedGameState {
    pub fn entity(&self, entity_id: EntityId) -> Option<&EntityState> {
        self.entities.get(&entity_id)
    }

    pub fn player(&self, player_id: PlayerId) -> Option<&PlayerState> {
        self.players.get(&player_id.0)
    }
}
