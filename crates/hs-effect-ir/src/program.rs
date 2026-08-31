//! EffectProgram 与 Operation 全集(§11.2–11.5、§11.7)。
//!
//! 冻结原则:Operation 定义 §11.4 **全集**(含 M3 执行器不实现的变体),
//! 执行器对未实现变体统一返回 `Unsupported(NotImplemented)`——
//! 「未支持显式报告」本身是 M3 验收项。各 Track 对本文件只增不改。

use crate::selector::{CardRef, PlayerRef, Predicate, Selector};
use hs_types::{BuildRange, CardId, HandlerId, PoolId, RuleStatus, Zone};
use serde::{Deserialize, Serialize};

/// 当前支持的规则文件 schema 版本(§11.7 `schema: 1`)。
pub const IR_SCHEMA_VERSION: u32 = 1;

/// 证据类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// 真实对局日志片段。
    Log,
    /// 回归/机制测试。
    Test,
    /// 人工核对记录。
    Manual,
}

/// 单条规则证据(status 升级为 Verified 的依据链)。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleEvidence {
    pub kind: EvidenceKind,
    /// 指向证据的引用:fixture 路径、测试名或核对记录。
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// 触发事件全集(§11.5,冻结)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEvent {
    OnPlay,
    Battlecry,
    AfterPlay,
    BeforeAttack,
    AfterAttack,
    OnDamage,
    AfterDamage,
    OnHeal,
    OnDeath,
    Deathrattle,
    AfterSummon,
    AfterDraw,
    TurnStart,
    TurnEnd,
    AuraEnter,
    AuraLeave,
    AuraRecalculate,
    ZoneChanged,
}

/// 触发式效果:事件 + 条件(AND)+ 效果序列。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerProgram {
    pub event: TriggerEvent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<Predicate>,
    pub effects: Vec<Operation>,
}

/// 基础操作原语全集(§11.4,冻结)。
///
/// M3 执行子集与 Unsupported 子集的划分见 design §4;
/// 划分只约束 `hs-sim` 执行器,不影响本类型与解析。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Draw {
        player: PlayerRef,
        count: u8,
    },
    Damage {
        target: Selector,
        amount: i32,
    },
    Heal {
        target: Selector,
        amount: i32,
    },
    GainArmor {
        player: PlayerRef,
        amount: i32,
    },
    Summon {
        player: PlayerRef,
        card: CardRef,
        #[serde(default = "one")]
        count: u8,
    },
    Destroy {
        target: Selector,
    },
    Transform {
        target: Selector,
        into: CardRef,
    },
    Silence {
        target: Selector,
    },
    MoveZone {
        target: Selector,
        to: Zone,
    },
    CreateCard {
        player: PlayerRef,
        card: CardRef,
        zone: Zone,
        #[serde(default = "one")]
        count: u8,
    },
    AddEnchantment {
        target: Selector,
        enchantment: CardRef,
    },
    RemoveEnchantment {
        target: Selector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enchantment: Option<CardRef>,
    },
    /// tag 用 GameTag 名称字符串(如 "ATK"),避免依赖 hs-log-core 常量层。
    ModifyTag {
        target: Selector,
        tag: String,
        value: i32,
    },
    /// 从池中发现:展示 `options` 个候选,选 `count` 张。
    Discover {
        pool: PoolId,
        #[serde(default = "three")]
        options: u8,
        #[serde(default = "one")]
        count: u8,
    },
    /// 从池中随机取 `count` 张(不展示候选)。
    RandomChoice {
        pool: PoolId,
        #[serde(default = "one")]
        count: u8,
    },
    Attack {
        attacker: Selector,
        defender: Selector,
    },
    Cast {
        card: CardRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Selector>,
    },
    Repeat {
        times: u8,
        effects: Vec<Operation>,
    },
    Conditional {
        when: Vec<Predicate>,
        then: Vec<Operation>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        otherwise: Vec<Operation>,
    },
    ForEach {
        over: Selector,
        effects: Vec<Operation>,
    },
    ChooseOne {
        options: Vec<Vec<Operation>>,
    },
    Copy {
        source: Selector,
        to_zone: Zone,
        #[serde(default = "one")]
        count: u8,
    },
    SetAside {
        target: Selector,
    },
    ReturnToZone {
        target: Selector,
        to: Zone,
    },
    /// `amount: None` = 恢复全部法力水晶。
    RefreshMana {
        player: PlayerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount: Option<i32>,
    },
    Overload {
        player: PlayerRef,
        amount: i32,
    },
}

fn one() -> u8 {
    1
}

fn three() -> u8 {
    3
}

/// 一张卡在一个 build 区间内的可执行语义(§11.2)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectProgram {
    /// 规则文件 schema 版本,当前必须为 [`IR_SCHEMA_VERSION`]。
    pub schema: u32,
    pub card_id: CardId,
    pub build_range: BuildRange,
    pub status: RuleStatus,
    /// 置信度,校验时要求 ∈ [0,1]。
    #[serde(default = "full_confidence")]
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<TriggerProgram>,
    /// 打出时的直接效果(法术效果/战吼走这里或 triggers 的 battlecry,二选一)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub play: Vec<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_handler: Option<HandlerId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<RuleEvidence>,
}

fn full_confidence() -> f32 {
    1.0
}
