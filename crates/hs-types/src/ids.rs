//! 强类型 ID：禁止业务层裸用 i32/String（设计文档 §8.1）。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Hearthstone 客户端 Build 号（卡牌数据、规则、池定义的版本键）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BuildId(pub u32);

/// Power.log 中的实体 ID（单局内唯一）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityId(pub i32);

/// 玩家 ID（单局内 1/2）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlayerId(pub i32);

/// 卡牌字符串 ID（如 "CS2_029"）。共享不可变，克隆廉价。
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CardId(pub Arc<str>);

impl CardId {
    pub fn new(id: impl AsRef<str>) -> Self {
        Self(Arc::from(id.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 卡牌数值 ID（HearthstoneJSON `dbfId`；Deckstring 用它编码卡牌）。
/// 与 `card_definitions.dbf_id` 列(i64)对齐。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DbfId(pub i64);

/// 本地牌组 ID（SQLite rowid，M2 牌组域主键）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeckId(pub i64);

/// 单局对局的稳定标识（由日志会话与 CREATE_GAME 序号派生，M1 定稿派生规则）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GameId(pub u64);

/// 卡牌池标识（如 "discover.mage_spells"；规则文件与 PoolSpec 的主键，M3 池域）。
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PoolId(pub Arc<str>);

impl PoolId {
    pub fn new(id: impl AsRef<str>) -> Self {
        Self(Arc::from(id.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PoolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 自定义规则 Handler 标识（如 "cards.current_set.example_weird_card"，§11.8）。
/// M3 只冻结标识，不实现 Handler 注册与执行。
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HandlerId(pub Arc<str>);

impl HandlerId {
    pub fn new(id: impl AsRef<str>) -> Self {
        Self(Arc::from(id.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HandlerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 一次游戏进程产生的日志会话（目录级）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct LogSessionId(pub u64);

/// 会话内单个日志文件。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct LogFileId(pub u32);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn card_id_eq_hash_display() {
        let a = CardId::new("CS2_029");
        let b = CardId::new(String::from("CS2_029"));
        assert_eq!(a, b);
        let mut set = HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&b));
        assert_eq!(a.to_string(), "CS2_029");
    }

    #[test]
    fn ids_serde_round_trip() {
        let card = CardId::new("EX1_001");
        let json = serde_json::to_string(&card).unwrap();
        let back: CardId = serde_json::from_str(&json).unwrap();
        assert_eq!(card, back);

        let build = BuildId(123456);
        let json = serde_json::to_string(&build).unwrap();
        let back: BuildId = serde_json::from_str(&json).unwrap();
        assert_eq!(build, back);

        let entity = EntityId(-1);
        let json = serde_json::to_string(&entity).unwrap();
        let back: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(entity, back);
    }

    #[test]
    fn build_id_ordering() {
        assert!(BuildId(100) < BuildId(200));
    }
}
